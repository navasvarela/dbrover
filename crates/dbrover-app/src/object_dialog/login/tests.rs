use dbrover_core::{LoginServer, SqlLoginState};
use dbrover_sql::ddl_statements;

use super::*;

fn management(may_change: Permit, encrypted: Option<bool>) -> UserManagement {
    UserManagement {
        current: "sa".to_owned(),
        encrypted,
        may_list: Permit::Allowed,
        may_change,
        server: ManagedUsers::Logins(LoginServer {
            database: "shop".to_owned(),
            contained: false,
            may_change_users: Permit::Allowed,
        }),
    }
}

fn statements(form: &dyn ObjectForm) -> Result<Vec<String>, DdlError> {
    form.operations()
        .and_then(|operations| ddl_statements(&operations, Dialect::SqlServer))
}

fn shared(encrypted_by_settings: bool, management: &UserManagement) -> Shared {
    let mut shared = Shared::new(encrypted_by_settings);
    shared.loaded(&Ok(management.clone()));
    shared
}

fn typed(entry: &mut PasswordEntry, password: &str) {
    entry.password = password.to_owned();
    entry.confirmation = password.to_owned();
    entry.hash = Some((password.to_owned(), SqlServerHash::hash(password)));
}

#[test]
fn finds_each_principal_by_its_path() {
    let login = logins_folder().child(NodeKind::Login, "app");
    let user = database_folder("shop", FolderKind::Users).child(NodeKind::User, "app");
    let role = server_roles_folder().child(NodeKind::Role, "sysadmin");
    assert_eq!(
        Principal::at(&login),
        Some(Principal::Login("app".to_owned()))
    );
    assert_eq!(
        Principal::at(&user),
        Some(Principal::User {
            database: "shop".to_owned(),
            name: "app".to_owned()
        })
    );
    assert_eq!(
        Principal::at(&role).map(|role| role.noun()),
        Some("server role")
    );
    assert!(form(SqlServerHelper::NewLogin(CatalogPath::root()), true).is_some());
    assert!(form(SqlServerHelper::NewUser(logins_folder()), true).is_none());
    assert!(form(SqlServerHelper::SetEnabled(user), true).is_none());
}

#[test]
fn hashes_a_new_logins_password_unless_the_policy_must_check_it() {
    let management = management(Permit::Allowed, None);
    let mut new = NewLogin::new(shared(true, &management));
    new.name = "app".to_owned();
    typed(&mut new.password, "Pass-2026!");
    // The policy is on by default, so the password goes as typed.
    assert_eq!(
        statements(&new).unwrap(),
        ["CREATE LOGIN [app] WITH PASSWORD = N'Pass-2026!'"]
    );
    new.policy.check_policy = false;
    let hashed = statements(&new).unwrap();
    assert!(
        hashed[0].starts_with("CREATE LOGIN [app] WITH PASSWORD = 0x0200")
            && hashed[0].ends_with(" HASHED, CHECK_POLICY = OFF"),
        "{hashed:?}"
    );
    // A typed password needs an encrypted connection: the server didn't say, and the data
    // source's settings turn TLS off.
    let mut unencrypted = NewLogin::new(shared(false, &management));
    unencrypted.name = "app".to_owned();
    typed(&mut unencrypted.password, "Pass-2026!");
    assert_eq!(statements(&unencrypted), Err(DdlError::UnencryptedPassword));
}

#[test]
fn new_login_maps_users_and_flags_administration() {
    let management = management(Permit::Allowed, Some(true));
    let mut new = NewLogin::new(shared(true, &management));
    new.kind = NewLoginKind::Windows;
    new.name = "CORP\\ops".to_owned();
    new.server_roles.insert("sysadmin".to_owned());
    new.mappings
        .push(("shop".to_owned(), ["db_datareader".to_owned()].into()));
    assert_eq!(
        statements(&new).unwrap(),
        [
            "CREATE LOGIN [CORP\\ops] FROM WINDOWS",
            "ALTER SERVER ROLE [sysadmin] ADD MEMBER [CORP\\ops]",
            "EXEC [shop].sys.sp_executesql N'CREATE USER [CORP\\ops] FOR LOGIN [CORP\\ops] WITH DEFAULT_SCHEMA = [dbo]'",
            "EXEC [shop].sys.sp_executesql N'ALTER ROLE [db_datareader] ADD MEMBER [CORP\\ops]'",
        ]
    );
    assert_eq!(
        new.administration().as_deref(),
        Some("This gives CORP\\ops administration: sysadmin.")
    );
}

#[test]
fn says_what_a_login_without_alter_any_login_lacks() {
    let management = management(Permit::Needs("ALTER ANY LOGIN".to_owned()), Some(true));
    let mut new = NewLogin::new(shared(true, &management));
    new.name = "app".to_owned();
    assert_eq!(
        statements(&new),
        Err(DdlError::NotAllowed("ALTER ANY LOGIN".to_owned()))
    );
}

fn described_login() -> LoginDescription {
    LoginDescription {
        name: "app".to_owned(),
        kind: LoginKind::Sql,
        disabled: false,
        default_database: "master".to_owned(),
        default_language: None,
        sql: Some(SqlLoginState::default()),
        server_roles: BTreeSet::new(),
        permissions: [PermissionGrant {
            permission: "VIEW SERVER STATE".to_owned(),
            securable: Securable::Server,
            state: PermissionState::Grant,
        }]
        .into(),
        users: [DatabaseUserName {
            database: "shop".to_owned(),
            user: "app".to_owned(),
        }]
        .into(),
        owned_databases: Vec::new(),
    }
}

#[test]
fn edit_login_unlocks_and_revokes() {
    let management = management(Permit::Allowed, Some(true));
    let path = logins_folder().child(NodeKind::Login, "app");
    let mut edit = EditLogin::new(path, "app".to_owned(), shared(true, &management));
    edit.principal_described(&Ok(PrincipalDescription::Login(described_login())));
    assert_eq!(statements(&edit), Err(DdlError::NothingChanged));
    edit.unlock = true;
    assert_eq!(statements(&edit), Err(DdlError::UnlockNeedsPassword));
    typed(&mut edit.password, "New-pass-2026!");
    edit.revoked.extend(described_login().permissions);
    // Its policy is off, so the new password is hashed.
    let statements = statements(&edit).unwrap();
    assert!(
        statements[0].starts_with("ALTER LOGIN [app] WITH PASSWORD = 0x0200")
            && statements[0].ends_with(" HASHED UNLOCK"),
        "{statements:?}"
    );
    assert_eq!(
        statements[1],
        "EXEC [master].sys.sp_executesql N'REVOKE VIEW SERVER STATE FROM [app]'"
    );
}

#[test]
fn drop_login_drops_its_users_unless_told_not_to() {
    let management = management(Permit::Allowed, Some(true));
    let path = logins_folder().child(NodeKind::Login, "app");
    let mut drop = DropPrincipal::new(
        path.clone(),
        Principal::at(&path).unwrap(),
        shared(true, &management),
    );
    drop.principal_described(&Ok(PrincipalDescription::Login(described_login())));
    assert_eq!(
        statements(&drop).unwrap(),
        [
            "EXEC [shop].sys.sp_executesql N'DROP USER [app]'",
            "DROP LOGIN [app]"
        ]
    );
    assert_eq!(
        drop.destruction(Dialect::SqlServer).unwrap().consequences,
        ["Its users are dropped first: app in shop."]
    );
    drop.drop_users = false;
    assert_eq!(statements(&drop).unwrap(), ["DROP LOGIN [app]"]);
    assert_eq!(
        drop.destruction(Dialect::SqlServer).unwrap().consequences,
        ["Its users are left orphaned, with no login: app in shop."]
    );
}

#[test]
fn a_fixed_role_is_not_dropped() {
    let management = management(Permit::Allowed, Some(true));
    let path = server_roles_folder().child(NodeKind::Role, "sysadmin");
    let mut drop = DropPrincipal::new(
        path.clone(),
        Principal::at(&path).unwrap(),
        shared(true, &management),
    );
    drop.principal_described(&Ok(PrincipalDescription::SqlServerRole(
        SqlServerRoleDescription {
            scope: PrincipalScope::Server,
            name: "sysadmin".to_owned(),
            fixed: true,
            owner: None,
            members: BTreeSet::new(),
        },
    )));
    assert_eq!(statements(&drop), Err(DdlError::FixedRole));
}
