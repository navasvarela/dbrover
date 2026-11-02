//! A project's `schemas` allow-list: which schemas the catalog tools show and statements may
//! name (see `docs/mcp-server.md`).
//!
//! Names are checked as statements spell them ([`dbrover_sql::analyze`]). Unqualified names
//! resolve to a schema the server picks: on PostgreSQL each call's `search_path` is set to the
//! allowed schemas, so they can only resolve there; elsewhere they resolve to the
//! connection's default schema, which must then be allowed. What a function does inside is
//! not seen, so only a dedicated login's grants make the limit airtight.

use dbrover_core::{CatalogPath, ConnectionConfig, DriverKind, NodeKind, Quoting};
use dbrover_sql::{NamePart, NamedSchemas};
use dbrover_store::SchemaScope;

/// The allow-list as one data source applies it.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    allowed: Vec<String>,
    /// Where unqualified names resolve; `None` where the server pins it to the allowed
    /// schemas instead.
    default_schema: Option<String>,
    search_path: Option<String>,
}

impl Scope {
    /// `None` when every schema is allowed.
    pub(crate) fn new(schemas: &SchemaScope, config: &ConnectionConfig) -> Option<Self> {
        let SchemaScope::Only(allowed) = schemas else {
            return None;
        };
        let pins_search_path = config.driver() == DriverKind::Postgres;
        let search_path = pins_search_path.then(|| {
            let quoted: Vec<String> = allowed
                .iter()
                .map(|schema| Quoting::STANDARD.identifier(schema))
                .collect();
            format!("SET LOCAL search_path TO {}", quoted.join(", "))
        });
        Some(Self {
            allowed: allowed.clone(),
            default_schema: (!pins_search_path).then(|| config.default_schema().to_owned()),
            search_path,
        })
    }

    /// The statement that limits unqualified names to the allowed schemas, run at the start
    /// of each call's transaction.
    pub(crate) fn search_path(&self) -> Option<&str> {
        self.search_path.as_deref()
    }

    /// Whether `schema`, a catalog name as the database stores it, is allowed.
    pub(crate) fn allows(&self, schema: &str) -> bool {
        self.allowed.iter().any(|allowed| allowed == schema)
    }

    /// Unquoted names fold to the database's case, which differs between engines and, on
    /// MySQL, between servers; matching them without regard to case accepts each engine's
    /// folding of an allowed name, and errs towards allowing only names that differ in case
    /// from one that is allowed.
    fn allows_name(&self, part: &NamePart) -> bool {
        self.allowed.iter().any(|allowed| {
            if part.quoted {
                *allowed == part.value
            } else {
                allowed.eq_ignore_ascii_case(&part.value)
            }
        })
    }

    /// Refuses a statement that names a schema outside the list, or whose names can't all be
    /// read. The reason says which.
    pub(crate) fn check(&self, schemas: &NamedSchemas) -> Result<(), String> {
        let NamedSchemas::Known {
            qualified,
            unqualified,
        } = schemas
        else {
            return Err(format!(
                "this project only allows the schemas {}, and DbRover can't tell which schemas \
                 this statement uses",
                self.listed()
            ));
        };
        if let Some(outside) = qualified.iter().find(|part| !self.allows_name(part)) {
            return Err(format!(
                "this project only allows the schemas {}, and the statement names {:?}",
                self.listed(),
                outside.value
            ));
        }
        if let Some(default) = &self.default_schema
            && *unqualified
            && !self.allows(default)
        {
            return Err(format!(
                "this project only allows the schemas {}; unqualified names resolve to {default:?}, \
                 so qualify them with an allowed schema",
                self.listed()
            ));
        }
        Ok(())
    }

    /// Refuses a catalog path that goes through a schema outside the list.
    pub(crate) fn check_path(&self, path: &CatalogPath) -> Result<(), String> {
        let outside = path
            .segments()
            .iter()
            .find(|segment| segment.kind == NodeKind::Schema && !self.allows(&segment.name));
        match outside {
            Some(segment) => Err(format!(
                "this project only allows the schemas {}, not {:?}",
                self.listed(),
                segment.name
            )),
            None => Ok(()),
        }
    }

    fn listed(&self) -> String {
        self.allowed.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{MysqlConfig, PostgresConfig, ServerConfig, TlsMode};
    use dbrover_sql::{Dialect, analyze};

    use super::*;

    fn server(database: &str) -> ServerConfig {
        ServerConfig {
            host: "localhost".to_owned(),
            port: 1.into(),
            database: database.to_owned(),
            user: "u".to_owned(),
            tls: TlsMode::Prefer,
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        }
    }

    fn only(schemas: &[&str]) -> SchemaScope {
        SchemaScope::Only(schemas.iter().map(|schema| (*schema).to_owned()).collect())
    }

    fn check(scope: &Scope, sql: &str, dialect: Dialect) -> Result<(), String> {
        scope.check(&analyze(sql, dialect).unwrap().schemas)
    }

    #[test]
    fn allows_everything_without_a_list() {
        let config = ConnectionConfig::Postgres(PostgresConfig {
            server: server("app"),
        });
        assert!(Scope::new(&SchemaScope::All, &config).is_none());
    }

    #[test]
    fn pins_postgres_unqualified_names_to_the_allowed_schemas() {
        let config = ConnectionConfig::Postgres(PostgresConfig {
            server: server("app"),
        });
        let scope = Scope::new(&only(&["analytics", "Billing"]), &config).unwrap();

        assert_eq!(
            scope.search_path(),
            Some("SET LOCAL search_path TO \"analytics\", \"Billing\"")
        );
        let pg = Dialect::Postgres;
        assert_eq!(check(&scope, "SELECT * FROM events", pg), Ok(()));
        assert_eq!(check(&scope, "SELECT * FROM ANALYTICS.events", pg), Ok(()));
        assert_eq!(
            check(&scope, "SELECT * FROM \"Billing\".invoices", pg),
            Ok(())
        );
        assert!(check(&scope, "SELECT * FROM \"billing\".invoices", pg).is_err());
        assert!(check(&scope, "SELECT * FROM public.users", pg).is_err());
        assert!(check(&scope, "SELECT public.f()", pg).is_err());
        assert!(check(&scope, "DELETE FROM public.users", pg).is_err());
        assert!(check(&scope, "GRANT SELECT ON events TO x", pg).is_err());
        assert!(scope.allows("analytics"));
        assert!(!scope.allows("public"));
    }

    #[test]
    fn needs_the_default_schema_for_unqualified_names_elsewhere() {
        let config = ConnectionConfig::Mysql(MysqlConfig {
            server: server("shop"),
        });
        let my = Dialect::Mysql;

        let without_default = Scope::new(&only(&["reports"]), &config).unwrap();
        assert_eq!(without_default.search_path(), None);
        assert_eq!(
            check(&without_default, "SELECT * FROM reports.t", my),
            Ok(())
        );
        let refused = check(&without_default, "SELECT * FROM t", my).unwrap_err();
        assert!(refused.contains("\"shop\""), "{refused}");
        assert_eq!(
            check(&without_default, "SELECT count(*) FROM reports.t", my),
            Ok(()),
            "built-in functions aren't names to check"
        );

        let with_default = Scope::new(&only(&["shop"]), &config).unwrap();
        assert_eq!(check(&with_default, "SELECT * FROM t", my), Ok(()));
    }

    #[test]
    fn refuses_paths_through_other_schemas() {
        let config = ConnectionConfig::Postgres(PostgresConfig {
            server: server("app"),
        });
        let scope = Scope::new(&only(&["analytics"]), &config).unwrap();
        let database = CatalogPath::root().child(NodeKind::Database, "app");

        assert_eq!(scope.check_path(&database), Ok(()));
        assert_eq!(
            scope.check_path(&database.child(NodeKind::Schema, "analytics")),
            Ok(())
        );
        assert!(
            scope
                .check_path(&database.child(NodeKind::Schema, "public"))
                .is_err()
        );
    }
}
