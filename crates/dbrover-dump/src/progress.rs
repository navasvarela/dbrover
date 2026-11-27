//! Progress read from the programs' `--verbose` lines on stderr. The lines change between
//! versions, so a line this doesn't know is only logged: progress then falls back to bytes and
//! time, and never fails a run.

use dbrover_core::DumpEngine;

/// The table a dump program has started on, if `line` says it has:
///
/// - `pg_dump: dumping contents of table "public.orders"` (quoted since PostgreSQL 12, bare
///   before);
/// - `-- Retrieving table structure for table orders...` from `mysqldump` and `mariadb-dump`.
pub fn dumped_table(engine: DumpEngine, line: &str) -> Option<String> {
    let line = line.trim();
    let rest = match engine {
        DumpEngine::Postgres => line.strip_prefix("pg_dump: dumping contents of table ")?,
        DumpEngine::Mysql | DumpEngine::MariaDb => line
            .strip_prefix("-- Retrieving table structure for table ")?
            .trim_end_matches('.'),
    };
    let name = rest.trim().trim_matches('"').trim_matches('`');
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_programs_table_lines() {
        let cases = [
            (
                DumpEngine::Postgres,
                "pg_dump: dumping contents of table \"public.orders\"",
                Some("public.orders"),
            ),
            (
                DumpEngine::Postgres,
                "pg_dump: dumping contents of table public.orders",
                Some("public.orders"),
            ),
            (DumpEngine::Postgres, "pg_dump: reading schemas", None),
            (
                DumpEngine::Mysql,
                "-- Retrieving table structure for table orders...",
                Some("orders"),
            ),
            (
                DumpEngine::MariaDb,
                "-- Retrieving table structure for table `order items`...",
                Some("order items"),
            ),
            (DumpEngine::Mysql, "-- Connecting to localhost...", None),
        ];
        for (engine, line, table) in cases {
            assert_eq!(dumped_table(engine, line).as_deref(), table, "{line}");
        }
    }
}
