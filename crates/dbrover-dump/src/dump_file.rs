//! What a dump file is, read before anything runs: an archive of PostgreSQL's, or plain SQL,
//! gzipped or not, and whose engine wrote it. And what `pg_restore --list` says an archive
//! holds.

use dbrover_core::{DumpEngine, TableName};

use crate::version::Version;

/// The kinds of dump file a restore reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpFileKind {
    /// PostgreSQL's custom archive, restored by `pg_restore`.
    Custom,
    /// PostgreSQL's directory archive (a folder with `toc.dat`), restored by `pg_restore`.
    Directory,
    /// Plain SQL, run by `psql` or `mysql`. `engine` is the one its header names, `None` when
    /// it names none.
    Sql {
        engine: Option<DumpEngine>,
        gzip: bool,
    },
    /// A SQLite database file, copied as it is.
    SqliteFile,
    /// DuckDB's `EXPORT DATABASE` folder (with `schema.sql` and `load.sql`), read back by
    /// `IMPORT DATABASE`.
    DuckDbExport,
}

impl DumpFileKind {
    /// The engine the file is for, where it says.
    pub fn engine(self) -> Option<DumpEngine> {
        match self {
            Self::Custom | Self::Directory => Some(DumpEngine::Postgres),
            Self::Sql { engine, .. } => engine,
            Self::SqliteFile => Some(DumpEngine::Sqlite),
            Self::DuckDbExport => Some(DumpEngine::DuckDb),
        }
    }

    pub fn is_archive(self) -> bool {
        matches!(self, Self::Custom | Self::Directory)
    }

    /// Whether a file of this kind restores into a server of `engine`: MySQL's and MariaDB's
    /// plain dumps into either, and a dump that names no engine into any.
    pub fn restores_into(self, engine: DumpEngine) -> bool {
        if matches!(engine, DumpEngine::Sqlite | DumpEngine::DuckDb) {
            return self.engine() == Some(engine);
        }
        matches!(
            (self.engine(), engine),
            (None, _)
                | (Some(DumpEngine::Postgres), DumpEngine::Postgres)
                | (
                    Some(DumpEngine::Mysql | DumpEngine::MariaDb),
                    DumpEngine::Mysql | DumpEngine::MariaDb
                )
        )
    }
}

/// The start of a custom archive.
const CUSTOM_MAGIC: &[u8] = b"PGDMP";

/// The start of every SQLite database file.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// The start of a gzip stream.
pub const GZIP_MAGIC: &[u8] = &[0x1f, 0x8b];

/// What a file is, from its first bytes (gunzipped already when [`GZIP_MAGIC`] started it,
/// with `gzip` saying so). `None` for something that isn't a dump DbRover reads.
pub fn sniff(start: &[u8], gzip: bool) -> Option<DumpFileKind> {
    if !gzip && start.starts_with(CUSTOM_MAGIC) {
        return Some(DumpFileKind::Custom);
    }
    if !gzip && start.starts_with(SQLITE_MAGIC) {
        return Some(DumpFileKind::SqliteFile);
    }
    let text = String::from_utf8_lossy(start);
    if text.contains('\0') {
        return None;
    }
    let engine = sql_engine(&text);
    let looks_like_sql = engine.is_some()
        || text
            .lines()
            .map(str::trim_start)
            .any(|line| line.starts_with("--") || line.starts_with("/*") || line.contains(';'));
    looks_like_sql.then_some(DumpFileKind::Sql { engine, gzip })
}

/// The engine a plain dump's header names: `-- PostgreSQL database dump`, `-- MariaDB dump …`,
/// or `-- MySQL dump …`, which an older MariaDB's `mysqldump` writes too, with `-MariaDB` in
/// its version.
fn sql_engine(text: &str) -> Option<DumpEngine> {
    text.lines().take(10).find_map(|line| {
        let line = line.trim();
        if line.starts_with("-- PostgreSQL database dump") {
            Some(DumpEngine::Postgres)
        } else if line.starts_with("-- MariaDB dump") || line.contains("-MariaDB") {
            Some(DumpEngine::MariaDb)
        } else if line.starts_with("-- MySQL dump") {
            Some(DumpEngine::Mysql)
        } else {
            None
        }
    })
}

/// What an archive holds and where it came from, as `pg_restore --list` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveContents {
    /// The database it was dumped from.
    pub database: Option<String>,
    /// The server it was dumped from.
    pub server: Option<Version>,
    /// The `pg_dump` that wrote it, which `pg_restore` must be at least as new as.
    pub made_by: Option<Version>,
    pub tables: Vec<TableName>,
}

impl ArchiveContents {
    /// Reads `pg_restore --list`'s output: a header of `;`-comments, then one line per entry,
    /// `218; 1259 16420 TABLE public customers ada`. A table's entry is `TABLE`, then its
    /// schema, its name and its owner; a schema or owner with a space in it is read as far as
    /// the first space, which only shifts the name the list shows.
    pub fn parse(listing: &str) -> Self {
        let mut contents = Self::default();
        for line in listing.lines() {
            let line = line.trim();
            if let Some(comment) = line.strip_prefix(';') {
                contents.read_header(comment.trim());
            } else if let Some(table) = table_entry(line) {
                contents.tables.push(table);
            }
        }
        contents
    }

    fn read_header(&mut self, comment: &str) {
        if let Some(name) = comment.strip_prefix("dbname:") {
            self.database = Some(name.trim().to_owned());
        } else if let Some(version) = comment.strip_prefix("Dumped from database version:") {
            self.server = Version::parse_leading(version);
        } else if let Some(version) = comment.strip_prefix("Dumped by pg_dump version:") {
            self.made_by = Version::parse_leading(version);
        }
    }
}

fn table_entry(line: &str) -> Option<TableName> {
    let (_, entry) = line.split_once(';')?;
    let mut words = entry.split_whitespace();
    let _table_oid = words.next()?;
    let _oid = words.next()?;
    if words.next()? != "TABLE" {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    // `TABLE DATA` is the rows of a table already listed by its `TABLE` entry.
    if rest.first() == Some(&"DATA") || rest.len() < 3 {
        return None;
    }
    let schema = rest[0].to_owned();
    let name = rest[1..rest.len() - 1].join(" ");
    Some(TableName { schema, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_dumps_apart_by_their_first_bytes() {
        let cases: [(&[u8], bool, Option<DumpFileKind>); 8] = [
            (b"PGDMP\x01\x10\x00", false, Some(DumpFileKind::Custom)),
            (
                b"--\n-- PostgreSQL database dump\n--\n",
                false,
                Some(DumpFileKind::Sql {
                    engine: Some(DumpEngine::Postgres),
                    gzip: false,
                }),
            ),
            (
                b"-- MySQL dump 10.13  Distrib 8.4.3, for Linux (x86_64)\n",
                true,
                Some(DumpFileKind::Sql {
                    engine: Some(DumpEngine::Mysql),
                    gzip: true,
                }),
            ),
            (
                b"-- MariaDB dump 10.19-11.4.4-MariaDB, for debian-linux-gnu (aarch64)\n",
                false,
                Some(DumpFileKind::Sql {
                    engine: Some(DumpEngine::MariaDb),
                    gzip: false,
                }),
            ),
            (
                b"-- MySQL dump 10.19  Distrib 10.11.6-MariaDB, for debian-linux-gnu\n",
                false,
                Some(DumpFileKind::Sql {
                    engine: Some(DumpEngine::MariaDb),
                    gzip: false,
                }),
            ),
            (
                b"CREATE TABLE t (id int);\n",
                false,
                Some(DumpFileKind::Sql {
                    engine: None,
                    gzip: false,
                }),
            ),
            (b"\x89PNG\r\n\x1a\n\0\0", false, None),
            (
                b"SQLite format 3\0\x10\0",
                false,
                Some(DumpFileKind::SqliteFile),
            ),
        ];
        for (start, gzip, expected) in cases {
            assert_eq!(
                sniff(start, gzip),
                expected,
                "{}",
                String::from_utf8_lossy(start)
            );
        }
    }

    const LISTING: &str = "\
;
; Archive created at 2026-11-27 12:52:11 GMT
;     dbname: shop
;     TOC Entries: 10
;     Compression: gzip
;     Format: CUSTOM
;     Dumped from database version: 17.11 (Debian 17.11-1.pgdg13+2)
;     Dumped by pg_dump version: 18.6 (Homebrew)
;
;
; Selected TOC Entries:
;
5; 2615 2200 SCHEMA - public pg_database_owner
218; 1259 16420 TABLE public Order Items ada
217; 1259 16397 TABLE public customers ada
3427; 0 16420 TABLE DATA public Order Items ada
3280; 2606 16433 CONSTRAINT public Order Items Order Items_pkey ada
";

    #[test]
    fn reads_what_an_archive_holds() {
        let contents = ArchiveContents::parse(LISTING);
        assert_eq!(contents.database.as_deref(), Some("shop"));
        assert_eq!(contents.server, Some(Version::new(17, 11, 0)));
        assert_eq!(contents.made_by, Some(Version::new(18, 6, 0)));
        assert_eq!(
            contents.tables,
            [
                TableName {
                    schema: "public".to_owned(),
                    name: "Order Items".to_owned()
                },
                TableName {
                    schema: "public".to_owned(),
                    name: "customers".to_owned()
                },
            ]
        );
    }
}
