//! What Azure's warehouses keep of SQL Server's catalog (C1e, `docs/cloud-data.md`): Synapse's
//! dedicated and serverless SQL pools and Microsoft Fabric speak TDS and T-SQL, but have no
//! sequences, synonyms or triggers, only some of the constraints, and, in a dedicated pool, no
//! reading another database's catalog. The explorer lists only what the server has, so no
//! level is a query that fails.
//!
//! The facts are Microsoft's documentation's (each warehouse's "T-SQL surface area"), not
//! tried against the services; anything they wrongly allow fails with the server's message.

use dbrover_core::{DriverKind, ManagedService};

/// `SERVERPROPERTY('EngineEdition')` of a Synapse dedicated SQL pool.
const SYNAPSE_DEDICATED_EDITION: i32 = 6;
/// `SERVERPROPERTY('EngineEdition')` of a Synapse serverless SQL pool, and of a Fabric
/// warehouse or SQL analytics endpoint: only the host tells them apart.
const SERVERLESS_EDITION: i32 = 11;

/// The constraint kinds a warehouse keeps in its catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstraintKind {
    /// Primary keys and unique constraints (`sys.key_constraints`).
    Key,
    Foreign,
    Check,
    Default,
}

/// What a server's catalog has, beyond tables, views, functions and procedures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogSupport {
    /// Whether another database's catalog views can be read by name (`[sales].sys.objects`).
    pub other_databases: bool,
    pub sequences: bool,
    pub synonyms: bool,
    pub triggers: bool,
    pub indexes: bool,
    /// Indexes on a view, which SQL Server's indexed views have and the warehouses don't.
    pub indexed_views: bool,
    pub constraints: &'static [ConstraintKind],
}

const ALL_CONSTRAINTS: &[ConstraintKind] = &[
    ConstraintKind::Key,
    ConstraintKind::Foreign,
    ConstraintKind::Check,
    ConstraintKind::Default,
];

/// SQL Server, Azure SQL and Azure SQL Managed Instance.
pub(crate) const SQL_SERVER: CatalogSupport = CatalogSupport {
    other_databases: true,
    sequences: true,
    synonyms: true,
    triggers: true,
    indexes: true,
    indexed_views: true,
    constraints: ALL_CONSTRAINTS,
};

/// One database per pool, read through its own `sys` views; clustered columnstore, clustered
/// and nonclustered indexes; primary keys and unique constraints only `NOT ENFORCED`, defaults,
/// and no foreign keys or checks.
const SYNAPSE_DEDICATED: CatalogSupport = CatalogSupport {
    other_databases: false,
    sequences: false,
    synonyms: false,
    triggers: false,
    indexes: true,
    indexed_views: false,
    constraints: &[ConstraintKind::Key, ConstraintKind::Default],
};

/// External tables and views over files, read across the workspace's databases; no
/// indexes or constraints.
const SYNAPSE_SERVERLESS: CatalogSupport = CatalogSupport {
    other_databases: true,
    sequences: false,
    synonyms: false,
    triggers: false,
    indexes: false,
    indexed_views: false,
    constraints: &[],
};

/// The workspace's warehouses and endpoints read across by name; no indexes; primary keys,
/// unique and foreign keys only `NOT ENFORCED`, and no checks or defaults.
const FABRIC: CatalogSupport = CatalogSupport {
    other_databases: true,
    sequences: false,
    synonyms: false,
    triggers: false,
    indexes: false,
    indexed_views: false,
    constraints: &[ConstraintKind::Key, ConstraintKind::Foreign],
};

/// The warehouse a server is, from its `EngineEdition` and the host name connected to.
pub(crate) fn service_of(engine_edition: Option<i32>, host: &str) -> Option<ManagedService> {
    match engine_edition? {
        SYNAPSE_DEDICATED_EDITION => Some(ManagedService::SynapseDedicated),
        SERVERLESS_EDITION => Some(
            match ManagedService::recognise(DriverKind::SqlServer, host) {
                Some(ManagedService::Fabric) => ManagedService::Fabric,
                _ => ManagedService::SynapseServerless,
            },
        ),
        _ => None,
    }
}

/// What the catalog of a server that is `service` has.
pub(crate) fn support(service: Option<ManagedService>) -> &'static CatalogSupport {
    match service {
        Some(ManagedService::SynapseDedicated) => &SYNAPSE_DEDICATED,
        Some(ManagedService::SynapseServerless) => &SYNAPSE_SERVERLESS,
        Some(ManagedService::Fabric) => &FABRIC,
        _ => &SQL_SERVER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_each_warehouse_from_its_edition_and_host() {
        for (edition, host, expected) in [
            (
                Some(6),
                "myworkspace.sql.azuresynapse.net",
                Some(ManagedService::SynapseDedicated),
            ),
            // A dedicated pool reached through a name of its own is one all the same.
            (Some(6), "localhost", Some(ManagedService::SynapseDedicated)),
            (
                Some(11),
                "myworkspace-ondemand.sql.azuresynapse.net",
                Some(ManagedService::SynapseServerless),
            ),
            (
                Some(11),
                "abc123.datawarehouse.fabric.microsoft.com",
                Some(ManagedService::Fabric),
            ),
            (
                Some(11),
                "localhost",
                Some(ManagedService::SynapseServerless),
            ),
            // Azure SQL Database, SQL Server Enterprise and Developer, Managed Instance, and
            // a Fabric SQL database, which is Azure SQL's engine.
            (Some(5), "myserver.database.windows.net", None),
            (Some(3), "localhost", None),
            (Some(8), "mi.abc123.database.windows.net", None),
            (Some(12), "abc123.database.fabric.microsoft.com", None),
            (None, "localhost", None),
        ] {
            assert_eq!(service_of(edition, host), expected, "{edition:?} {host}");
        }
    }

    #[test]
    fn warehouses_keep_less_of_the_catalog_than_sql_server() {
        assert_eq!(support(None), &SQL_SERVER);
        assert_eq!(support(Some(ManagedService::AzureSql)), &SQL_SERVER);
        for service in [
            ManagedService::SynapseDedicated,
            ManagedService::SynapseServerless,
            ManagedService::Fabric,
        ] {
            let support = support(Some(service));
            assert!(!support.sequences && !support.synonyms && !support.triggers);
            assert!(!support.constraints.contains(&ConstraintKind::Check));
        }
        assert!(!support(Some(ManagedService::SynapseDedicated)).other_databases);
        assert!(!support(Some(ManagedService::Fabric)).indexes);
    }
}
