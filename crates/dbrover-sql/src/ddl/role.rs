//! `CREATE ROLE`, `ALTER ROLE` with `GRANT` and `REVOKE` for memberships, and `DROP ROLE`
//! with what the role owns, on the dialects whose users are roles.

use std::collections::BTreeSet;

use dbrover_core::RoleAttribute;

use super::{DdlError, OwnedObjects, PasswordChange, RoleSpec, quote_name, require_name};
use crate::dialect::UserModel;
use crate::{Dialect, ScramVerifier};

/// What `CONNECTION LIMIT` takes for no limit.
const NO_CONNECTION_LIMIT: i64 = -1;
/// What `VALID UNTIL` takes for a password that never expires.
const NEVER: &str = "infinity";

pub(super) fn create(
    name: &str,
    spec: &RoleSpec,
    password: Option<&ScramVerifier>,
    dialect: Dialect,
) -> Result<String, DdlError> {
    supported(dialect)?;
    require_name(name, "role")?;
    let mut options: Vec<String> = RoleAttribute::ALL
        .into_iter()
        .filter(|attribute| spec.attributes.contains(attribute) != attribute.is_default())
        .map(|attribute| attribute_option(attribute, spec.attributes.contains(&attribute)))
        .collect();
    if let Some(limit) = spec.connection_limit {
        options.push(format!("CONNECTION LIMIT {limit}"));
    }
    if let Some(until) = valid_until(spec) {
        options.push(valid_until_option(until, dialect));
    }
    if !spec.member_of.is_empty() {
        options.push(format!("IN ROLE {}", role_list(&spec.member_of, dialect)));
    }
    // Last: see `password_option`.
    if let Some(password) = password {
        options.push(password_option(password, dialect));
    }
    let role = quote_name(dialect, name);
    Ok(if options.is_empty() {
        format!("CREATE ROLE {role}")
    } else {
        format!("CREATE ROLE {role} WITH {}", options.join(" "))
    })
}

/// One `ALTER ROLE` for the attributes, password, limit and expiry that differ between
/// `before` and `after`, then a `REVOKE` for the memberships left and a `GRANT` for the ones
/// joined.
pub(super) fn alter(
    name: &str,
    before: &RoleSpec,
    after: &RoleSpec,
    password: &PasswordChange,
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    supported(dialect)?;
    require_name(name, "role")?;
    let mut options: Vec<String> = RoleAttribute::ALL
        .into_iter()
        .filter(|attribute| {
            before.attributes.contains(attribute) != after.attributes.contains(attribute)
        })
        .map(|attribute| attribute_option(attribute, after.attributes.contains(&attribute)))
        .collect();
    if before.connection_limit != after.connection_limit {
        let limit = after
            .connection_limit
            .map_or(NO_CONNECTION_LIMIT, i64::from);
        options.push(format!("CONNECTION LIMIT {limit}"));
    }
    if valid_until(before) != valid_until(after) {
        options.push(valid_until_option(
            valid_until(after).unwrap_or(NEVER),
            dialect,
        ));
    }
    // Last: see `password_option`.
    match password {
        PasswordChange::Keep => {}
        PasswordChange::Set(verifier) => options.push(password_option(verifier, dialect)),
        PasswordChange::Remove => options.push("PASSWORD NULL".to_owned()),
    }
    let role = quote_name(dialect, name);
    let mut statements = Vec::new();
    if !options.is_empty() {
        statements.push(format!("ALTER ROLE {role} WITH {}", options.join(" ")));
    }
    let left: BTreeSet<String> = before
        .member_of
        .difference(&after.member_of)
        .cloned()
        .collect();
    if !left.is_empty() {
        statements.push(format!("REVOKE {} FROM {role}", role_list(&left, dialect)));
    }
    let joined: BTreeSet<String> = after
        .member_of
        .difference(&before.member_of)
        .cloned()
        .collect();
    if !joined.is_empty() {
        statements.push(format!("GRANT {} TO {role}", role_list(&joined, dialect)));
    }
    if statements.is_empty() {
        return Err(DdlError::NothingChanged);
    }
    Ok(statements)
}

pub(super) fn rename(name: &str, new_name: &str, dialect: Dialect) -> Result<String, DdlError> {
    supported(dialect)?;
    require_name(name, "role")?;
    require_name(new_name, "role")?;
    Ok(format!(
        "ALTER ROLE {} RENAME TO {}",
        quote_name(dialect, name),
        quote_name(dialect, new_name)
    ))
}

/// `DROP ROLE`, after giving away or dropping what the role owns in the connected database
/// where `owned` says to. `DROP OWNED` also revokes the privileges the role was granted,
/// which would otherwise stop `DROP ROLE` too, so it follows `REASSIGN OWNED`.
pub(super) fn drop(
    name: &str,
    owned: &OwnedObjects,
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    supported(dialect)?;
    require_name(name, "role")?;
    let role = quote_name(dialect, name);
    let mut statements = Vec::new();
    match owned {
        OwnedObjects::Keep => {}
        OwnedObjects::ReassignTo(owner) => {
            let owner = owner.trim();
            if owner.is_empty() {
                return Err(DdlError::NoNewOwner);
            }
            if owner == name {
                return Err(DdlError::ReassignToItself);
            }
            statements.push(format!(
                "REASSIGN OWNED BY {role} TO {}",
                quote_name(dialect, owner)
            ));
            statements.push(format!("DROP OWNED BY {role}"));
        }
        OwnedObjects::Drop => statements.push(format!("DROP OWNED BY {role}")),
    }
    statements.push(format!("DROP ROLE {role}"));
    Ok(statements)
}

fn supported(dialect: Dialect) -> Result<(), DdlError> {
    match dialect.capabilities().users {
        UserModel::Roles => Ok(()),
        UserModel::Accounts | UserModel::LoginsAndUsers | UserModel::Unsupported => {
            Err(DdlError::Unsupported {
                what: "roles DbRover can create or change",
                dialect,
            })
        }
    }
}

fn attribute_option(attribute: RoleAttribute, on: bool) -> String {
    if on {
        attribute.keyword().to_owned()
    } else {
        format!("NO{}", attribute.keyword())
    }
}

/// Written after every other option. The history's redaction (`dbrover_store::redact_secrets`),
/// which the review panel shows too, hides every string from `PASSWORD` to the end of its
/// clause; an expiry written after the password would read `VALID UNTIL '<redacted>'`.
fn password_option(verifier: &ScramVerifier, dialect: Dialect) -> String {
    format!(
        "PASSWORD {}",
        dialect.quoting().string_literal(verifier.as_str())
    )
}

fn valid_until(spec: &RoleSpec) -> Option<&str> {
    spec.valid_until
        .as_deref()
        .map(str::trim)
        .filter(|until| !until.is_empty())
}

fn valid_until_option(until: &str, dialect: Dialect) -> String {
    format!("VALID UNTIL {}", dialect.quoting().string_literal(until))
}

fn role_list(roles: &BTreeSet<String>, dialect: Dialect) -> String {
    roles
        .iter()
        .map(|role| quote_name(dialect, role))
        .collect::<Vec<_>>()
        .join(", ")
}
