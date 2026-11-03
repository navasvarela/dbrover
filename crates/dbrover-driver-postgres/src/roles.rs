//! The server's roles: the roles folder beside the databases, and what one role is, for the
//! users and roles helpers.

use std::collections::BTreeSet;

use dbrover_core::{
    CatalogNode, CatalogPath, DriverError, NodeKind, RoleAttribute, RoleDescription,
};
use tokio_postgres::types::Type;
use tokio_postgres::{Client, Row};

use crate::connection_error;

/// The roles PostgreSQL predefines (`pg_read_all_data`, `pg_monitor`, …) are named `pg_…`,
/// a prefix `CREATE ROLE` refuses.
const SYSTEM_PREFIX: &str = "pg_";

/// The `pg_roles` column that holds each attribute. `describe` selects them first, in this
/// order, and reads them back by position.
const ATTRIBUTE_COLUMNS: [(RoleAttribute, &str); 7] = [
    (RoleAttribute::Login, "rolcanlogin"),
    (RoleAttribute::Superuser, "rolsuper"),
    (RoleAttribute::CreateDb, "rolcreatedb"),
    (RoleAttribute::CreateRole, "rolcreaterole"),
    (RoleAttribute::Inherit, "rolinherit"),
    (RoleAttribute::Replication, "rolreplication"),
    (RoleAttribute::BypassRls, "rolbypassrls"),
];

/// The roles each role is a member of, as an array of names in alphabetical order. Spliced
/// into queries over `pg_roles r`.
const MEMBER_OF: &str = "ARRAY(SELECT g.rolname::text FROM pg_auth_members m \
     JOIN pg_roles g ON g.oid = m.roleid WHERE m.member = r.oid ORDER BY 1)";

pub(crate) async fn list(
    client: &Client,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = format!(
        "SELECT r.rolname::text, r.rolcanlogin, r.rolsuper, {MEMBER_OF} \
         FROM pg_roles r ORDER BY r.rolname"
    );
    client
        .query_typed(&sql, &[])
        .await
        .map_err(connection_error)?
        .iter()
        .map(|row| {
            let name: String = get(row, 0)?;
            let login: bool = get(row, 1)?;
            let superuser: bool = get(row, 2)?;
            let member_of: Vec<String> = get(row, 3)?;
            Ok(CatalogNode {
                path: path.child(NodeKind::Role, name.clone()),
                detail: detail(login, superuser, &member_of),
                system: name.starts_with(SYSTEM_PREFIX),
                label: name,
                has_children: false,
                has_definition: false,
            })
        })
        .collect()
}

/// "login, superuser, member of readers, writers", or whichever parts apply.
fn detail(login: bool, superuser: bool, member_of: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if login {
        parts.push("login".to_owned());
    }
    if superuser {
        parts.push("superuser".to_owned());
    }
    if !member_of.is_empty() {
        parts.push(format!("member of {}", member_of.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

pub(crate) async fn describe(
    client: &Client,
    path: &CatalogPath,
) -> Result<RoleDescription, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let role = match path.segments() {
        [_folder, role] if role.kind == NodeKind::Role => role.name.as_str(),
        _ => return Err(invalid()),
    };
    // Objects in the connected database the role owns ('o') or holds privileges on ('a'),
    // as pg_shdepend records them for DROP ROLE's check.
    let dependencies = |deptype: &str| {
        format!(
            "(SELECT count(*) FROM pg_shdepend d \
              WHERE d.refclassid = 'pg_authid'::regclass AND d.refobjid = r.oid \
                AND d.deptype = '{deptype}' \
                AND d.dbid = (SELECT oid FROM pg_database WHERE datname = current_database()))"
        )
    };
    let attribute_columns: Vec<String> = ATTRIBUTE_COLUMNS
        .iter()
        .map(|(_, column)| format!("r.{column}"))
        .collect();
    let sql = format!(
        "SELECT {attributes}, r.rolconnlimit, \
                NULLIF(r.rolvaliduntil, 'infinity')::text, {MEMBER_OF}, \
                ARRAY(SELECT u.rolname::text FROM pg_auth_members m \
                      JOIN pg_roles u ON u.oid = m.member WHERE m.roleid = r.oid ORDER BY 1), \
                {owned}, {privileges}, \
                ARRAY(SELECT datname::text FROM pg_database WHERE datdba = r.oid ORDER BY 1) \
         FROM pg_roles r WHERE r.rolname = $1",
        attributes = attribute_columns.join(", "),
        owned = dependencies("o"),
        privileges = dependencies("a"),
    );
    let row = client
        .query_typed(&sql, &[(&role, Type::TEXT)])
        .await
        .map_err(connection_error)?
        .into_iter()
        .next()
        .ok_or_else(invalid)?;
    let mut attributes = BTreeSet::new();
    for (column, (attribute, _)) in ATTRIBUTE_COLUMNS.into_iter().enumerate() {
        if get::<bool>(&row, column)? {
            attributes.insert(attribute);
        }
    }
    let connection_limit: i32 = get(&row, 7)?;
    let member_of: Vec<String> = get(&row, 9)?;
    let members: Vec<String> = get(&row, 10)?;
    let owned_objects: i64 = get(&row, 11)?;
    let privileges: i64 = get(&row, 12)?;
    Ok(RoleDescription {
        name: role.to_owned(),
        attributes,
        // Negative is PostgreSQL's "no limit".
        connection_limit: u32::try_from(connection_limit).ok(),
        valid_until: get(&row, 8)?,
        member_of: member_of.into_iter().collect(),
        members: members.into_iter().collect(),
        owned_objects: u64::try_from(owned_objects).unwrap_or_default(),
        privileges: u64::try_from(privileges).unwrap_or_default(),
        owned_databases: get(&row, 13)?,
    })
}

fn get<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a Row,
    column: usize,
) -> Result<T, DriverError> {
    row.try_get(column).map_err(connection_error)
}
