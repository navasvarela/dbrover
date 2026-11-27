//! Where a statement in the history came from, as typed provenance and as the columns that
//! store it.

use std::path::PathBuf;

use super::change::RowChange;

/// Who sent a statement, and from where in DbRover.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HistorySource {
    /// A query console's run.
    Console,
    /// The grid's Submit: one `UPDATE`, `INSERT` or `DELETE` per row, with what it did to the
    /// row. `None` when the change isn't known: a row whose stored change this DbRover can't
    /// read.
    GridEdit(Option<RowChange>),
    /// One of the explorer's object helpers: creating, changing, renaming, dropping or copying
    /// a table, view or schema.
    ObjectHelper,
    /// A CSV import into a table, recorded once for the whole file.
    Import { file: PathBuf },
    /// Export › All rows: the statement the rows were read with.
    Export,
    /// An MCP client, through `dbrover mcp`, with the name it gave when it connected.
    Mcp { client: Option<String> },
    /// A custom action from `actions.toml`, by its id.
    Action { id: String },
    /// Cancel query or Kill in a Server activity tab (W15), with what was stopped: the
    /// session, whose it was, and its statement, redacted.
    Activity { stopped: String },
    /// A data search (W16), recorded once with the statements of the tables that matched, and
    /// what was searched for, where, and what was found.
    Search { searched: String },
    /// A dump (W17), recorded once with the command that ran, and what was dumped, where to,
    /// and how large it was.
    Dump { dumped: String },
    /// A restore (W17), recorded once with the command that ran, and what was restored into
    /// which database.
    Restore { restored: String },
    /// DbRover, before it recorded where a statement came from: rows written before the history
    /// had provenance, or since then by an older DbRover sharing the file.
    DbRover,
}

/// What [`HistorySource`] is, without its details: what the history is filtered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistorySourceKind {
    Console,
    GridEdit,
    ObjectHelper,
    Import,
    Export,
    Mcp,
    Action,
    Activity,
    Search,
    Dump,
    Restore,
    DbRover,
}

impl HistorySourceKind {
    pub const ALL: [Self; 12] = [
        Self::Console,
        Self::GridEdit,
        Self::ObjectHelper,
        Self::Import,
        Self::Export,
        Self::Mcp,
        Self::Action,
        Self::Activity,
        Self::Search,
        Self::Dump,
        Self::Restore,
        Self::DbRover,
    ];

    /// The kind as `coalesce(origin, source)` reads in the database. `app` and `mcp` are the
    /// values of `source` from before `origin` existed, so a row an older DbRover wrote reads as
    /// [`Self::DbRover`] or [`Self::Mcp`].
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::GridEdit => "grid",
            Self::ObjectHelper => "object",
            Self::Import => "import",
            Self::Export => "export",
            Self::Mcp => "mcp",
            Self::Action => "action",
            Self::Activity => "activity",
            Self::Search => "search",
            Self::Dump => "dump",
            Self::Restore => "restore",
            Self::DbRover => "app",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

impl HistorySource {
    pub fn kind(&self) -> HistorySourceKind {
        match self {
            Self::Console => HistorySourceKind::Console,
            Self::GridEdit(_) => HistorySourceKind::GridEdit,
            Self::ObjectHelper => HistorySourceKind::ObjectHelper,
            Self::Import { .. } => HistorySourceKind::Import,
            Self::Export => HistorySourceKind::Export,
            Self::Mcp { .. } => HistorySourceKind::Mcp,
            Self::Action { .. } => HistorySourceKind::Action,
            Self::Activity { .. } => HistorySourceKind::Activity,
            Self::Search { .. } => HistorySourceKind::Search,
            Self::Dump { .. } => HistorySourceKind::Dump,
            Self::Restore { .. } => HistorySourceKind::Restore,
            Self::DbRover => HistorySourceKind::DbRover,
        }
    }

    /// The `source` column, the only one a DbRover from before provenance reads: it parses
    /// `app` and `mcp`, and skips a row with anything else.
    pub(super) fn legacy_source(&self) -> &'static str {
        match self {
            Self::Mcp { .. } => HistorySourceKind::Mcp.as_str(),
            _ => HistorySourceKind::DbRover.as_str(),
        }
    }

    /// The `origin` column; `NULL` for [`Self::DbRover`], as an older DbRover leaves it.
    pub(super) fn origin(&self) -> Option<&'static str> {
        match self {
            Self::DbRover => None,
            other => Some(other.kind().as_str()),
        }
    }

    /// The `detail` column.
    pub(super) fn detail(&self) -> Option<String> {
        match self {
            Self::Import { file } => Some(file.to_string_lossy().into_owned()),
            Self::Mcp { client } => client.clone(),
            Self::Action { id } => Some(id.clone()),
            Self::Activity { stopped } => Some(stopped.clone()),
            Self::Search { searched } => Some(searched.clone()),
            Self::Dump { dumped } => Some(dumped.clone()),
            Self::Restore { restored } => Some(restored.clone()),
            _ => None,
        }
    }

    /// The `changes` column, as JSON.
    pub(super) fn changes(&self) -> Option<String> {
        match self {
            Self::GridEdit(Some(change)) => Some(change.to_json()),
            _ => None,
        }
    }

    /// The source a row stores, from `coalesce(origin, source)`, `detail` and `changes`.
    pub(super) fn from_stored(
        kind: &str,
        detail: Option<String>,
        changes: Option<&str>,
    ) -> Option<Self> {
        Some(match HistorySourceKind::parse(kind)? {
            HistorySourceKind::Console => Self::Console,
            HistorySourceKind::GridEdit => Self::GridEdit(changes.and_then(RowChange::from_json)),
            HistorySourceKind::ObjectHelper => Self::ObjectHelper,
            HistorySourceKind::Import => Self::Import {
                file: PathBuf::from(detail.unwrap_or_default()),
            },
            HistorySourceKind::Export => Self::Export,
            HistorySourceKind::Mcp => Self::Mcp { client: detail },
            HistorySourceKind::Action => Self::Action {
                id: detail.unwrap_or_default(),
            },
            HistorySourceKind::Activity => Self::Activity {
                stopped: detail.unwrap_or_default(),
            },
            HistorySourceKind::Search => Self::Search {
                searched: detail.unwrap_or_default(),
            },
            HistorySourceKind::Dump => Self::Dump {
                dumped: detail.unwrap_or_default(),
            },
            HistorySourceKind::Restore => Self::Restore {
                restored: detail.unwrap_or_default(),
            },
            HistorySourceKind::DbRover => Self::DbRover,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::change::RowChangeKind;
    use super::*;

    #[test]
    fn every_source_reads_back_as_stored() {
        let sources = [
            HistorySource::Console,
            HistorySource::GridEdit(None),
            HistorySource::GridEdit(Some(RowChange {
                table: "main.people".to_owned(),
                kind: RowChangeKind::Inserted { values: vec![] },
                revert: None,
            })),
            HistorySource::ObjectHelper,
            HistorySource::Import {
                file: PathBuf::from("/data/orders.csv"),
            },
            HistorySource::Export,
            HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            },
            HistorySource::Mcp { client: None },
            HistorySource::Activity {
                stopped: "Killed 4211: app on orders-api".to_owned(),
            },
            HistorySource::Action {
                id: "row-count".to_owned(),
            },
            HistorySource::Search {
                searched: "Searched 3 tables of shop.public for \"o'brien\"".to_owned(),
            },
            HistorySource::Dump {
                dumped: "Dumped shop to /dumps/shop.dump (custom, 142 MB)".to_owned(),
            },
            HistorySource::Restore {
                restored: "Restored /dumps/shop.dump into a new database shop_restored".to_owned(),
            },
            HistorySource::DbRover,
        ];
        for source in sources {
            let kind = source.origin().unwrap_or(source.legacy_source());
            assert_eq!(
                HistorySource::from_stored(kind, source.detail(), source.changes().as_deref()),
                Some(source.clone())
            );
        }
    }

    #[test]
    fn older_readers_see_only_app_and_mcp() {
        for kind in HistorySourceKind::ALL {
            let source = HistorySource::from_stored(kind.as_str(), None, None).unwrap();
            assert!(["app", "mcp"].contains(&source.legacy_source()), "{kind:?}");
        }
    }
}
