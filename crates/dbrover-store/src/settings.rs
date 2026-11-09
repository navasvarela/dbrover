//! `settings.toml`: preferences that aren't key bindings, such as how SQL is formatted. The
//! app's Settings window writes it; it can also be edited by hand.

use std::io;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dbrover_core::ByteSize;
use serde::{Deserialize, Serialize};

use crate::file_version::{
    CheckedWrite, FileVersion, Snapshot, read_versioned, write_if_unchanged,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub format: FormatSettings,
    pub lint: LintSettings,
    pub updates: UpdateSettings,
    pub history: HistorySettings,
    pub reports: ReportSettings,
    pub explorer: ExplorerSettings,
}

/// How the SQL formatter writes statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormatSettings {
    pub keyword_case: KeywordCase,
    /// Spaces per indentation level.
    pub indent_width: u8,
}

impl Default for FormatSettings {
    fn default() -> Self {
        Self {
            keyword_case: KeywordCase::Upper,
            indent_width: 4,
        }
    }
}

/// What the console checks while typing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintSettings {
    /// Ids of the lint rules switched off. Kept as written; the app reports ids it doesn't
    /// know.
    pub disabled: Vec<String>,
    /// Whether a connected console asks its database to prepare (never run) its statements.
    pub ask_database: bool,
}

impl Default for LintSettings {
    fn default() -> Self {
        Self {
            disabled: Vec::new(),
            ask_database: true,
        }
    }
}

/// Whether DbRover looks for newer releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
    /// Ask GitHub for the newest release once each time DbRover starts. Help › Check for
    /// updates works either way.
    pub check_on_start: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_on_start: true,
        }
    }
}

/// How long the query history keeps statements. Both limits apply when both are set; with
/// neither, as by default, nothing is ever deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistorySettings {
    /// Statements older than this many days go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<NonZeroU32>,
    /// Only the newest this many statements of each data source stay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_data_source: Option<NonZeroU32>,
}

/// What the explorer lists besides data sources.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExplorerSettings {
    /// Folders of `.sql` files listed under **Query folders**, in the order added. Folders
    /// under the home folder are written as `~/…` ([`crate::home_relative`]), so a synced
    /// settings folder brings the list to another machine; one missing there is still listed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub query_folders: Vec<PathBuf>,
}

/// The defaults of every report's limits, which a report's file overrides key by key, and how
/// many runs of each report the history keeps (`docs/reporting.md`, "Settings › Reports").
/// Each is `None` until changed, for DbRover's own default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReportSettings {
    /// The rows a source may return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<NonZeroU64>,
    /// The bytes of values a source may return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<ByteSize>,
    /// How long a source's statements may run.
    #[serde(skip_serializing_if = "Option::is_none", with = "written_duration")]
    pub source_timeout: Option<Duration>,
    /// The memory the run's DuckDB holds before it spills.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<ByteSize>,
    /// What the run's DuckDB may spill to disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_temp_size: Option<ByteSize>,
    /// The threads the run's DuckDB runs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<NonZeroU32>,
    /// How long the combine query may run.
    #[serde(skip_serializing_if = "Option::is_none", with = "written_duration")]
    pub combine_timeout: Option<Duration>,
    /// The bytes of values an output may write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size: Option<ByteSize>,
    /// How long a whole run may take.
    #[serde(skip_serializing_if = "Option::is_none", with = "written_duration")]
    pub run_timeout: Option<Duration>,
    /// How many runs of each report the history keeps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs_kept: Option<NonZeroU32>,
}

/// A duration as a report file writes one, `"10m"` or `"1h 30m"`, so `[reports]` reads like the
/// keys it sets defaults for. Zero is refused: no time limit means anything at zero.
mod written_duration {
    use std::time::Duration;

    use jiff::SignedDuration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        duration: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match duration.map(SignedDuration::try_from) {
            Some(Ok(duration)) => serializer.collect_str(&format_args!("{duration:#}")),
            Some(Err(error)) => Err(serde::ser::Error::custom(error)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse::<SignedDuration>()
            .ok()
            .and_then(|duration| Duration::try_from(duration).ok())
            .filter(|duration| !duration.is_zero())
            .map(Some)
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "{text:?} isn't a duration such as \"30s\" or \"10m\""
                ))
            })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeywordCase {
    #[default]
    Upper,
    Lower,
    /// As typed.
    Preserve,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a valid settings file", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write the settings as TOML")]
    Serialize(#[source] toml::ser::Error),
    /// Another program wrote the file between DbRover reading it and writing it back.
    #[error("{} changed on disk while it was being saved", path.display())]
    ChangedWhileSaving { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The settings in the file; a missing file, or a setting it leaves out, means the
    /// default.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Read`] or [`SettingsError::Parse`] if the file exists but can't be
    /// used.
    pub fn load(&self) -> Result<Settings, SettingsError> {
        self.read().map(|snapshot| snapshot.value)
    }

    /// The settings with the file's version, to [`write_over`](Self::write_over) later.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load).
    pub fn read(&self) -> Result<Snapshot<Settings>, SettingsError> {
        let read = read_versioned(&self.path).map_err(|source| SettingsError::Read {
            path: self.path.clone(),
            source,
        })?;
        let settings = match &read.text {
            Some(text) => toml::from_str(text).map_err(|source| SettingsError::Parse {
                path: self.path.clone(),
                source,
            })?,
            None => Settings::default(),
        };
        Ok(Snapshot::new(read, settings))
    }

    /// Writes `settings` over the file `read` came from, and returns its new version.
    /// Comments in a hand-edited file are lost.
    ///
    /// # Errors
    ///
    /// [`SettingsError::ChangedWhileSaving`] if the file is no longer what `read` holds, and
    /// [`SettingsError::Write`] if it can't be written.
    pub fn write_over(
        &self,
        read: &Snapshot<Settings>,
        settings: &Settings,
    ) -> Result<FileVersion, SettingsError> {
        let text = toml::to_string(settings).map_err(SettingsError::Serialize)?;
        let written = write_if_unchanged(&self.path, read.version, &text).map_err(|source| {
            SettingsError::Write {
                path: self.path.clone(),
                source,
            }
        })?;
        match written {
            CheckedWrite::Written(version) => Ok(version),
            CheckedWrite::ChangedOnDisk(_) => Err(SettingsError::ChangedWhileSaving {
                path: self.path.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_or_setting_means_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().join("settings.toml"));
        assert_eq!(file.load().unwrap(), Settings::default());

        std::fs::write(file.path(), "[format]\nkeyword_case = \"lower\"\n").unwrap();

        let settings = file.load().unwrap();
        assert_eq!(settings.format.keyword_case, KeywordCase::Lower);
        assert_eq!(settings.format.indent_width, 4);
        assert!(settings.updates.check_on_start);
        assert_eq!(settings.history, HistorySettings::default());

        std::fs::write(file.path(), "[history]\nmax_per_data_source = 0\n").unwrap();
        assert!(
            matches!(file.load(), Err(SettingsError::Parse { .. })),
            "keeping none of the history is a mistake, not a setting"
        );
    }

    #[test]
    fn reads_the_reports_defaults_as_a_report_file_writes_them() {
        let cases = [
            ("", Some(ReportSettings::default())),
            (
                "[reports]\nmax_bytes = \"500 MB\"\nrun_timeout = \"1h 30m\"\nthreads = 4\n",
                Some(ReportSettings {
                    max_bytes: Some(ByteSize(500_000_000)),
                    run_timeout: Some(Duration::from_secs(90 * 60)),
                    threads: NonZeroU32::new(4),
                    ..ReportSettings::default()
                }),
            ),
            (
                "[reports]\nmax_size = 1048576\n",
                Some(ReportSettings {
                    max_size: Some(ByteSize(1 << 20)),
                    ..ReportSettings::default()
                }),
            ),
            ("[reports]\nmax_rows = 0\n", None),
            ("[reports]\nmemory_limit = \"0 GiB\"\n", None),
            ("[reports]\ncombine_timeout = \"0s\"\n", None),
            ("[reports]\nsource_timeout = \"soon\"\n", None),
            ("[reports]\ntimeout = \"10m\"\n", None),
        ];
        for (text, reports) in cases {
            let read = toml::from_str::<Settings>(text)
                .ok()
                .map(|settings| settings.reports);
            assert_eq!(read, reports, "{text:?}");
        }
    }

    #[test]
    fn saves_and_reads_back_and_rejects_unknown_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().join("settings.toml"));
        let settings = Settings {
            format: FormatSettings {
                keyword_case: KeywordCase::Preserve,
                indent_width: 2,
            },
            lint: LintSettings {
                disabled: vec!["null-comparison".to_owned()],
                ask_database: false,
            },
            updates: UpdateSettings {
                check_on_start: false,
            },
            history: HistorySettings {
                max_age_days: NonZeroU32::new(90),
                max_per_data_source: None,
            },
            reports: ReportSettings {
                max_rows: NonZeroU64::new(5_000_000),
                memory_limit: Some(ByteSize(4 << 30)),
                source_timeout: Some(Duration::from_secs(45 * 60)),
                runs_kept: NonZeroU32::new(50),
                ..ReportSettings::default()
            },
            explorer: ExplorerSettings {
                query_folders: vec!["~/src/shop/queries".into(), "/srv/queries".into()],
            },
        };

        let read = file.read().unwrap();
        file.write_over(&read, &settings).unwrap();
        assert_eq!(file.load().unwrap(), settings);
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(
            text.contains("[reports]\nmax_rows = 5000000\nsource_timeout = \"45m\"\n"),
            "{text}"
        );
        assert!(text.contains("memory_limit = \"4 GiB\"\n"), "{text}");
        assert!(
            text.contains(
                "[explorer]\nquery_folders = [\"~/src/shop/queries\", \"/srv/queries\"]\n"
            ),
            "{text}"
        );

        std::fs::write(file.path(), "[format]\nindent = 2\n").unwrap();
        assert!(matches!(file.load(), Err(SettingsError::Parse { .. })));
    }
}
