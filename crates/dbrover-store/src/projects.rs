//! `projects.toml`: projects, the named groups the explorer shows data sources in, and what an
//! MCP client may do in each. Which project a data source belongs to is written on the data
//! source, in `data_sources.toml`, so data sources stay one file.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dbrover_core::ProjectId;
use serde::{Deserialize, Serialize};

use crate::atomic::write_atomically;
use crate::file_version::{
    CheckedWrite, FileVersion, Snapshot, read_versioned, write_if_unchanged,
};

const HEADER: &str = "\
# DbRover projects. DbRover picks up changes when its window comes back to the front.
# Which project a data source is in is written in data_sources.toml.
# Comments are not preserved when DbRover saves this file.

";

/// Everything `projects.toml` holds.
///
/// Unknown keys are refused here and in every table below, rather than ignored, because a
/// later version adds permissions: an older DbRover that skipped a newer restriction (say, a
/// list of allowed schemas) would serve more than the file allows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projects {
    /// What a project allows when it doesn't say otherwise.
    #[serde(default)]
    pub mcp_defaults: McpPermissions,
    #[serde(default, rename = "project")]
    pub projects: Vec<Project>,
}

/// A named group of data sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// What `dbrover mcp --project` takes. Chosen from the first name and kept through
    /// renames, so a client's configuration keeps working.
    pub slug: ProjectSlug,
    /// Whether `dbrover mcp` serves this project at all. Off until switched on, so creating a
    /// project never exposes its data sources by accident.
    #[serde(default)]
    pub mcp_enabled: bool,
    /// What this project sets differently from [`Projects::mcp_defaults`].
    #[serde(default, skip_serializing_if = "McpOverrides::is_empty")]
    pub mcp: McpOverrides,
}

impl Project {
    /// A new project, invisible to MCP.
    pub fn new(id: ProjectId, name: String, slug: ProjectSlug) -> Self {
        Self {
            id,
            name,
            slug,
            mcp_enabled: false,
            mcp: McpOverrides::default(),
        }
    }
}

/// What an MCP client may do in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpPermissions {
    /// Browsing the catalog, `SELECT`, `EXPLAIN`, describing tables.
    pub read: bool,
    /// `INSERT`, `UPDATE`, `DELETE` and the like.
    pub write: bool,
    /// `CREATE`, `ALTER`, `DROP` and other DDL.
    pub schema: bool,
    /// Grants, roles, server settings and other administration.
    pub admin: bool,
    /// Running the reports whose data sources are all in the project (`docs/reporting.md`,
    /// "Finishing RP4"). Off by default: a run reads as much as its report says, under the
    /// report's own limits, not the project's.
    pub reports: bool,
    /// The most rows one call returns; the reply says when there were more.
    pub max_rows: NonZeroU32,
    /// How long one statement may run before it is cancelled.
    pub statement_timeout: StatementTimeout,
    /// Which schemas the catalog tools show and statements may name.
    pub schemas: SchemaScope,
}

/// 500 rows is enough to see what a table holds without flooding a model's context.
const DEFAULT_MAX_ROWS: NonZeroU32 = match NonZeroU32::new(500) {
    Some(rows) => rows,
    None => panic!("the default row limit must not be zero"),
};

/// Stops a runaway query long before anyone would wait for it.
const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// The plan's defaults (`docs/mcp-server.md`): an assistant can look, not change.
impl Default for McpPermissions {
    fn default() -> Self {
        Self {
            read: true,
            write: false,
            schema: false,
            admin: false,
            reports: false,
            max_rows: DEFAULT_MAX_ROWS,
            statement_timeout: StatementTimeout(DEFAULT_STATEMENT_TIMEOUT),
            schemas: SchemaScope::All,
        }
    }
}

/// A project's own settings; each one left out falls back to the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reports: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<NonZeroU32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_timeout: Option<StatementTimeout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<SchemaScope>,
}

impl McpOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These overrides on top of `defaults`.
    pub fn apply_to(&self, defaults: &McpPermissions) -> McpPermissions {
        McpPermissions {
            read: self.read.unwrap_or(defaults.read),
            write: self.write.unwrap_or(defaults.write),
            schema: self.schema.unwrap_or(defaults.schema),
            admin: self.admin.unwrap_or(defaults.admin),
            reports: self.reports.unwrap_or(defaults.reports),
            max_rows: self.max_rows.unwrap_or(defaults.max_rows),
            statement_timeout: self.statement_timeout.unwrap_or(defaults.statement_timeout),
            schemas: self
                .schemas
                .clone()
                .unwrap_or_else(|| defaults.schemas.clone()),
        }
    }
}

/// The schemas an MCP client may use: `"all"`, or a list such as `["public", "analytics"]`.
///
/// Written as `"all"` rather than left out or as an empty list, so a project can say "all"
/// when the defaults name a few, and an empty list (which would allow nothing) can't be
/// written by mistake.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SchemaScopeText", into = "SchemaScopeText")]
pub enum SchemaScope {
    #[default]
    All,
    /// Schema names as the database stores them; never empty.
    Only(Vec<String>),
}

impl SchemaScope {
    /// Whether `schema`, a name as the database stores it, is in scope.
    pub fn allows(&self, schema: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(schemas) => schemas.iter().any(|allowed| allowed == schema),
        }
    }
}

/// How [`SchemaScope`] is spelt in the file.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SchemaScopeText {
    Keyword(String),
    List(Vec<String>),
}

const ALL_SCHEMAS: &str = "all";

#[derive(Debug, thiserror::Error)]
pub enum InvalidSchemaScope {
    #[error("schemas must be \"{ALL_SCHEMAS}\" or a list of schema names, not {0:?}")]
    Keyword(String),
    #[error("schemas must name at least one schema, or be \"{ALL_SCHEMAS}\"")]
    Empty,
    #[error("schemas lists an empty name")]
    EmptyName,
}

impl TryFrom<SchemaScopeText> for SchemaScope {
    type Error = InvalidSchemaScope;

    fn try_from(text: SchemaScopeText) -> Result<Self, Self::Error> {
        match text {
            SchemaScopeText::Keyword(keyword) if keyword == ALL_SCHEMAS => Ok(Self::All),
            SchemaScopeText::Keyword(keyword) => Err(InvalidSchemaScope::Keyword(keyword)),
            SchemaScopeText::List(schemas) if schemas.is_empty() => Err(InvalidSchemaScope::Empty),
            SchemaScopeText::List(schemas) if schemas.iter().any(String::is_empty) => {
                Err(InvalidSchemaScope::EmptyName)
            }
            SchemaScopeText::List(schemas) => Ok(Self::Only(schemas)),
        }
    }
}

impl From<SchemaScope> for SchemaScopeText {
    fn from(scope: SchemaScope) -> Self {
        match scope {
            SchemaScope::All => Self::Keyword(ALL_SCHEMAS.to_owned()),
            SchemaScope::Only(schemas) => Self::List(schemas),
        }
    }
}

/// A positive duration, written the way people say it: `"30s"`, `"2m"`, `"1m 30s"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StatementTimeout(Duration);

impl StatementTimeout {
    pub fn from_secs(seconds: std::num::NonZeroU64) -> Self {
        Self(Duration::from_secs(seconds.get()))
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0:?} is not a duration such as \"30s\" or \"2m\" greater than zero")]
pub struct InvalidTimeout(String);

impl TryFrom<String> for StatementTimeout {
    type Error = InvalidTimeout;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let parsed: Option<Duration> = text
            .parse::<jiff::SignedDuration>()
            .ok()
            .and_then(|duration| Duration::try_from(duration).ok())
            .filter(|duration| !duration.is_zero());
        parsed.map(Self).ok_or(InvalidTimeout(text))
    }
}

impl From<StatementTimeout> for String {
    fn from(timeout: StatementTimeout) -> Self {
        timeout.to_string()
    }
}

impl fmt::Display for StatementTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match jiff::SignedDuration::try_from(self.0) {
            // `#` selects jiff's friendly format ("30s") over ISO 8601 ("PT30S").
            Ok(duration) => write!(f, "{duration:#}"),
            Err(_) => write!(f, "{}s", self.0.as_secs()),
        }
    }
}

/// A project's name as MCP clients spell it: lower-case ASCII letters, digits and single
/// hyphens, such as `analytics` or `shop-eu`. Fits in a shell argument and in an MCP
/// client's server name without quoting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectSlug(String);

/// Generous for a name typed on a command line, and short enough to stay readable in a
/// client's list of servers.
const MAX_SLUG_LENGTH: usize = 63;

#[derive(Debug, thiserror::Error)]
#[error(
    "{0:?} is not a valid project slug: use lower-case letters, digits and single hyphens, \
     up to {MAX_SLUG_LENGTH} characters"
)]
pub struct InvalidSlug(String);

impl ProjectSlug {
    /// A slug made from `name`: lower-cased, with anything but ASCII letters and digits turned
    /// into hyphens. `project` when nothing usable is left.
    pub fn from_name(name: &str) -> Self {
        let mut slug = String::new();
        for c in name.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                slug.push(c);
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        slug.truncate(MAX_SLUG_LENGTH);
        let slug = slug.trim_end_matches('-');
        if slug.is_empty() {
            Self("project".to_owned())
        } else {
            Self(slug.to_owned())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This slug with `-2`, `-3`… added, shortened if needed to stay valid.
    fn numbered(&self, number: usize) -> Self {
        let suffix = format!("-{number}");
        let keep = MAX_SLUG_LENGTH.saturating_sub(suffix.len());
        let stem = self.0.get(..keep).unwrap_or(&self.0).trim_end_matches('-');
        Self(format!("{stem}{suffix}"))
    }
}

impl TryFrom<String> for ProjectSlug {
    type Error = InvalidSlug;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let valid = !text.is_empty()
            && text.len() <= MAX_SLUG_LENGTH
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !text.starts_with('-')
            && !text.ends_with('-')
            && !text.contains("--");
        if valid {
            Ok(Self(text))
        } else {
            Err(InvalidSlug(text))
        }
    }
}

impl From<ProjectSlug> for String {
    fn from(slug: ProjectSlug) -> Self {
        slug.0
    }
}

impl fmt::Display for ProjectSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Projects {
    pub fn get(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub fn by_slug(&self, slug: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.slug.as_str() == slug)
    }

    /// What MCP clients may do in `project`: its overrides on top of the defaults.
    pub fn mcp_permissions(&self, project: &Project) -> McpPermissions {
        project.mcp.apply_to(&self.mcp_defaults)
    }

    /// A slug for a new project called `name` that no project has yet.
    pub fn unused_slug(&self, name: &str) -> ProjectSlug {
        let base = ProjectSlug::from_name(name);
        let taken = |slug: &ProjectSlug| self.projects.iter().any(|p| &p.slug == slug);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|number| base.numbered(number))
            .find(|slug| !taken(slug))
            .unwrap_or(base)
    }

    /// Ids and slugs must each be unique: data sources point at the id, clients at the slug.
    pub fn check_unique(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        let mut slugs = HashSet::new();
        for project in &self.projects {
            if !ids.insert(project.id) {
                return Err(format!("two projects have the id {}", project.id));
            }
            if !slugs.insert(&project.slug) {
                return Err(format!("two projects have the slug {:?}", project.slug.0));
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectStoreError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a valid projects file", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{} is not a valid projects file: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
    #[error("could not serialize the projects")]
    Serialize(#[source] toml::ser::Error),
    /// Another program wrote the file between DbRover reading it and writing it back.
    #[error("{} changed on disk while it was being saved; try again", path.display())]
    ChangedWhileSaving { path: PathBuf },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The `projects.toml` file.
#[derive(Debug, Clone)]
pub struct ProjectFile {
    path: PathBuf,
}

impl ProjectFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the projects. A missing file means no projects and the default permissions.
    ///
    /// # Errors
    ///
    /// [`ProjectStoreError::Read`], [`ProjectStoreError::Parse`] or
    /// [`ProjectStoreError::Invalid`] if the file exists but can't be used. Callers must then
    /// not [`save`](Self::save) over it, or the user's projects are lost.
    pub fn load(&self) -> Result<Projects, ProjectStoreError> {
        self.read().map(|snapshot| snapshot.value)
    }

    /// Reads the projects with the file's version, to [`write_over`](Self::write_over) later.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load).
    pub fn read(&self) -> Result<Snapshot<Projects>, ProjectStoreError> {
        let read = read_versioned(&self.path).map_err(|source| ProjectStoreError::Read {
            path: self.path.clone(),
            source,
        })?;
        let Some(text) = &read.text else {
            return Ok(Snapshot::new(read, Projects::default()));
        };
        let projects: Projects =
            toml::from_str(text).map_err(|source| ProjectStoreError::Parse {
                path: self.path.clone(),
                source,
            })?;
        projects
            .check_unique()
            .map_err(|reason| ProjectStoreError::Invalid {
                path: self.path.clone(),
                reason,
            })?;
        Ok(Snapshot::new(read, projects))
    }

    /// Writes `projects` over the file `read` came from, and returns its new version.
    ///
    /// # Errors
    ///
    /// [`ProjectStoreError::ChangedWhileSaving`] if the file is no longer what `read` holds,
    /// and [`ProjectStoreError::Write`] if it can't be written.
    pub fn write_over(
        &self,
        read: &Snapshot<Projects>,
        projects: &Projects,
    ) -> Result<FileVersion, ProjectStoreError> {
        let text = text_of(projects)?;
        let written = write_if_unchanged(&self.path, read.version, &text).map_err(|source| {
            ProjectStoreError::Write {
                path: self.path.clone(),
                source,
            }
        })?;
        match written {
            CheckedWrite::Written(version) => Ok(version),
            CheckedWrite::ChangedOnDisk(_) => Err(ProjectStoreError::ChangedWhileSaving {
                path: self.path.clone(),
            }),
        }
    }

    /// Replaces the file's contents. The defaults are always written out, so someone editing
    /// the file by hand sees what they can change.
    ///
    /// Written atomically: a crash mid-write leaves either the old or the new file.
    ///
    /// # Errors
    ///
    /// [`ProjectStoreError::Write`] if the directory or file can't be written.
    pub fn save(&self, projects: &Projects) -> Result<(), ProjectStoreError> {
        write_atomically(&self.path, &text_of(projects)?).map_err(|source| {
            ProjectStoreError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }
}

fn text_of(projects: &Projects) -> Result<String, ProjectStoreError> {
    let body = toml::to_string(projects).map_err(ProjectStoreError::Serialize)?;
    Ok(format!("{HEADER}{body}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn slug(text: &str) -> ProjectSlug {
        ProjectSlug::try_from(text.to_owned()).unwrap()
    }

    fn project(name: &str) -> Project {
        Project::new(
            ProjectId::generate(),
            name.to_owned(),
            ProjectSlug::from_name(name),
        )
    }

    #[test]
    fn a_missing_file_has_no_projects_and_the_default_permissions() {
        let dir = tempfile::tempdir().unwrap();

        let loaded = ProjectFile::new(dir.path().join("projects.toml"))
            .load()
            .unwrap();

        assert_eq!(loaded, Projects::default());
        let defaults = loaded.mcp_defaults;
        assert!(defaults.read && !defaults.write && !defaults.schema && !defaults.admin);
        assert_eq!(defaults.max_rows.get(), 500);
        assert_eq!(
            defaults.statement_timeout.duration(),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn reads_the_format_the_plan_shows() {
        let dir = tempfile::tempdir().unwrap();
        let file = ProjectFile::new(dir.path().join("projects.toml"));
        fs::write(
            file.path(),
            r#"
            [mcp_defaults]
            read = true
            write = false
            schema = false
            admin = false
            max_rows = 200
            statement_timeout = "1m 30s"

            [[project]]
            id = "5b0c7d1e-2f3a-4b5c-8d6e-7f8091a2b3c4"
            name = "Analytics"
            slug = "analytics"

            [project.mcp]
            write = true
            schemas = ["public", "Reporting"]

            [[project]]
            id = "6c1d8e2f-3a4b-4c5d-9e7f-8091a2b3c4d5"
            name = "Shop"
            slug = "shop"
            mcp_enabled = true
            "#,
        )
        .unwrap();

        let projects = file.load().unwrap();

        let [analytics, shop] = projects.projects.as_slice() else {
            panic!("expected two projects, got {projects:?}");
        };
        assert!(!analytics.mcp_enabled, "MCP is off unless switched on");
        assert!(shop.mcp_enabled);
        let permissions = projects.mcp_permissions(analytics);
        assert!(permissions.read && permissions.write && !permissions.schema);
        assert_eq!(permissions.max_rows.get(), 200);
        assert_eq!(
            permissions.statement_timeout.duration(),
            Duration::from_secs(90)
        );
        assert!(permissions.schemas.allows("Reporting"));
        assert!(!permissions.schemas.allows("reporting"));
        assert_eq!(projects.mcp_permissions(shop).schemas, SchemaScope::All);
        assert_eq!(projects.by_slug("shop"), Some(shop));
    }

    #[test]
    fn saves_and_loads_writing_the_defaults_and_only_the_overrides_set() {
        let dir = tempfile::tempdir().unwrap();
        let file = ProjectFile::new(dir.path().join("nested/projects.toml"));
        let mut analytics = project("Analytics");
        analytics.mcp.max_rows = NonZeroU32::new(50);
        analytics.mcp.schemas = Some(SchemaScope::All);
        let projects = Projects {
            mcp_defaults: McpPermissions {
                schemas: SchemaScope::Only(vec!["public".to_owned()]),
                ..McpPermissions::default()
            },
            projects: vec![analytics, project("Shop")],
        };

        file.save(&projects).unwrap();

        assert_eq!(file.load().unwrap(), projects);
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.starts_with("# DbRover projects."), "{text}");
        assert!(text.contains("[mcp_defaults]"), "{text}");
        assert!(text.contains("statement_timeout = \"30s\""), "{text}");
        assert_eq!(text.matches("[project.mcp]").count(), 1, "{text}");
        assert!(text.contains("max_rows = 50"), "{text}");
        assert!(text.contains("schemas = [\"public\"]"), "{text}");
        assert!(text.contains("schemas = \"all\""), "{text}");
        assert!(!file.path().with_extension("toml.tmp").exists());
    }

    #[test]
    fn refuses_files_it_cant_use_whole() {
        let id = "5b0c7d1e-2f3a-4b5c-8d6e-7f8091a2b3c4";
        let cases = [
            (
                "unknown permission",
                "[mcp_defaults]\ndatabases = [\"public\"]\n",
            ),
            ("no schemas", "[mcp_defaults]\nschemas = []\n"),
            ("schemas keyword", "[mcp_defaults]\nschemas = \"none\"\n"),
            ("zero rows", "[mcp_defaults]\nmax_rows = 0\n"),
            (
                "bad timeout",
                "[mcp_defaults]\nstatement_timeout = \"soon\"\n",
            ),
            (
                "zero timeout",
                "[mcp_defaults]\nstatement_timeout = \"0s\"\n",
            ),
            (
                "upper-case slug",
                &format!("[[project]]\nid = \"{id}\"\nname = \"A\"\nslug = \"A\"\n"),
            ),
            (
                "unknown override",
                &format!(
                    "[[project]]\nid = \"{id}\"\nname = \"A\"\nslug = \"a\"\n[project.mcp]\nexecute = true\n"
                ),
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let file = ProjectFile::new(dir.path().join("projects.toml"));
        for (case, text) in cases {
            fs::write(file.path(), text).unwrap();
            assert!(
                matches!(file.load(), Err(ProjectStoreError::Parse { .. })),
                "{case}"
            );
        }

        let twice = format!(
            "[[project]]\nid = \"{id}\"\nname = \"A\"\nslug = \"a\"\n\
             [[project]]\nid = \"6c1d8e2f-3a4b-4c5d-9e7f-8091a2b3c4d5\"\nname = \"B\"\nslug = \"a\"\n"
        );
        fs::write(file.path(), twice).unwrap();
        assert!(matches!(
            file.load(),
            Err(ProjectStoreError::Invalid { reason, .. }) if reason.contains("slug")
        ));
    }

    #[test]
    fn makes_slugs_from_names() {
        let cases = [
            ("Analytics", "analytics"),
            ("Shop EU (prod)", "shop-eu-prod"),
            ("  --Data__Lake--  ", "data-lake"),
            ("Análisis 2026", "an-lisis-2026"),
            ("???", "project"),
        ];
        for (name, expected) in cases {
            assert_eq!(ProjectSlug::from_name(name).as_str(), expected, "{name}");
        }
        assert_eq!(
            ProjectSlug::from_name(&"x".repeat(100)).as_str().len(),
            MAX_SLUG_LENGTH
        );
        for invalid in ["", "-a", "a-", "a--b", "a b", "A"] {
            assert!(
                ProjectSlug::try_from(invalid.to_owned()).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn numbers_a_slug_that_is_taken() {
        let mut projects = Projects::default();
        projects.projects.push(project("Shop"));
        projects.projects.push(Project::new(
            ProjectId::generate(),
            "Shop 2".to_owned(),
            slug("shop-2"),
        ));

        assert_eq!(projects.unused_slug("Analytics"), slug("analytics"));
        assert_eq!(projects.unused_slug("shop!"), slug("shop-3"));
        let long = "y".repeat(MAX_SLUG_LENGTH);
        projects.projects.push(project(&long));
        assert_eq!(
            projects.unused_slug(&long).as_str(),
            format!("{}-2", "y".repeat(MAX_SLUG_LENGTH - 2))
        );
    }
}
