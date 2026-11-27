use dbrover_core::{AccountServer, DefinedKind, DefinedObject};
use dbrover_sql::ddl_statements;

use super::*;

fn management(flavor: MysqlFlavor, password_checks: bool, encrypted: bool) -> UserManagement {
    UserManagement {
        current: "admin@%".to_owned(),
        encrypted: Some(encrypted),
        may_list: Permit::Allowed,
        may_change: Permit::Allowed,
        server: ManagedUsers::Accounts(AccountServer {
            flavor,
            version: "8.4.13".to_owned(),
            plugins: vec![
                "caching_sha2_password".to_owned(),
                "mysql_native_password".to_owned(),
                "sha256_password".to_owned(),
            ],
            password_checks,
            failed_login_lock: flavor == MysqlFlavor::Mysql,
        }),
    }
}

fn statements(form: &dyn ObjectForm) -> Result<Vec<String>, DdlError> {
    form.operations()
        .and_then(|operations| ddl_statements(&operations, Dialect::Mysql))
}

/// A new user named `app`, its password typed twice and made as the dialog makes it.
fn new_user(management: &UserManagement) -> NewAccount {
    let mut form = NewAccount::new(&users_folder(), AccountKind::User).unwrap();
    form.user_management_loaded(&Ok(management.clone()));
    form.user = "app".to_owned();
    form.password.password = "it's secret".to_owned();
    form.password.confirmation = "it's secret".to_owned();
    let Loaded::Ready(server) = &form.server else {
        panic!("no server");
    };
    let plugin = *form.fields.plugin.get_or_insert(server.plugins[0]);
    let as_typed = server.as_typed(plugin);
    form.password.refresh(plugin, as_typed);
    form
}

#[test]
fn opens_on_the_folders_and_their_accounts() {
    let user = users_folder().child(NodeKind::User, "app@%");
    let role = roles_folder().child(NodeKind::Role, "app_read");
    assert!(NewAccount::new(&CatalogPath::root(), AccountKind::User).is_some());
    assert!(NewAccount::new(&roles_folder(), AccountKind::Role).is_some());
    assert!(NewAccount::new(&user, AccountKind::User).is_none());
    assert_eq!(
        account_at(&user),
        Some((AccountName::new("app", "%"), AccountKind::User))
    );
    assert_eq!(
        account_at(&role),
        Some((AccountName::new("app_read", ""), AccountKind::Role))
    );
    assert!(
        LockAccount::new(role.clone()).is_none(),
        "a role doesn't log in"
    );
    assert!(DropAccount::new(role).is_some());
}

#[test]
fn hashes_the_password_unless_the_server_checks_it() {
    let hashed = statements(&new_user(&management(MysqlFlavor::Mysql, false, false))).unwrap();
    assert!(
        hashed[0]
            .starts_with("CREATE USER `app`@`%` IDENTIFIED WITH caching_sha2_password AS '$A$005$"),
        "{hashed:?}"
    );
    assert!(!hashed[0].contains("secret"));
    let typed = statements(&new_user(&management(MysqlFlavor::Mysql, true, true))).unwrap();
    assert_eq!(
        typed,
        ["CREATE USER `app`@`%` IDENTIFIED WITH caching_sha2_password BY 'it''s secret'"]
    );
    assert_eq!(
        statements(&new_user(&management(MysqlFlavor::Mysql, true, false))),
        Err(DdlError::UnencryptedPassword)
    );
}

#[test]
fn offers_mariadb_only_the_plugins_it_hashes_or_takes() {
    let form = new_user(&management(MysqlFlavor::MariaDb, false, false));
    let Loaded::Ready(server) = &form.server else {
        panic!("no server");
    };
    assert_eq!(server.plugins, [PasswordPlugin::MysqlNative]);
    assert_eq!(
        statements(&form).unwrap(),
        [
            "CREATE USER `app`@`%` IDENTIFIED BY PASSWORD '*5882E74C3D73B7EDFF86538730AEBC6D6FE6A5AF'"
        ]
    );
}

#[test]
fn says_what_a_login_without_create_user_lacks() {
    let mut refused = management(MysqlFlavor::Mysql, false, true);
    refused.may_change = Permit::Needs("CREATE USER".to_owned());
    assert_eq!(
        statements(&new_user(&refused)),
        Err(DdlError::NotAllowed("CREATE USER".to_owned()))
    );
}

fn described(locked: bool) -> AccountDescription {
    AccountDescription {
        name: AccountName::new("app", "%"),
        kind: AccountKind::User,
        plugin: "auth_socket".to_owned(),
        has_password: false,
        locked,
        password_expired: false,
        lifetime: PasswordLifetime::ServerDefault,
        failed_login: None,
        limits: AccountLimits::default(),
        roles: BTreeMap::new(),
        default_roles: BTreeSet::new(),
        global: PrivilegeSet::default(),
        databases: BTreeMap::new(),
        objects: BTreeSet::new(),
        members: BTreeSet::new(),
        defines: vec![DefinedObject {
            kind: DefinedKind::View,
            database: "shop".to_owned(),
            name: "totals".to_owned(),
        }],
    }
}

#[test]
fn edit_writes_what_changed_and_flags_administration() {
    let path = users_folder().child(NodeKind::User, "app@%");
    let mut form = EditAccount::new(path).unwrap();
    form.user_management_loaded(&Ok(management(MysqlFlavor::Mysql, false, true)));
    assert_eq!(statements(&form), Err(DdlError::NothingChanged));
    form.principal_described(&Ok(PrincipalDescription::Account(described(false))));
    assert_eq!(form.plugin, "auth_socket");
    assert_eq!(
        form.fields.plugin, None,
        "a plugin DbRover doesn't write is kept"
    );
    assert_eq!(statements(&form), Err(DdlError::NothingChanged));
    form.fields
        .global
        .privileges
        .insert(Privilege::new("SUPER"));
    form.fields.locked = true;
    assert_eq!(
        statements(&form).unwrap(),
        [
            "ALTER USER `app`@`%` ACCOUNT LOCK",
            "GRANT SUPER ON *.* TO `app`@`%`"
        ]
    );
    assert_eq!(
        form.administration().as_deref(),
        Some("This gives app@% administration of the server: SUPER.")
    );
}

#[test]
fn lock_turns_into_unlock_for_a_locked_account() {
    let path = users_folder().child(NodeKind::User, "app@%");
    let mut form = LockAccount::new(path).unwrap();
    form.user_management_loaded(&Ok(management(MysqlFlavor::Mysql, false, true)));
    form.principal_described(&Ok(PrincipalDescription::Account(described(true))));
    assert_eq!(form.title(), "Unlock user app@%");
    assert_eq!(
        statements(&form).unwrap(),
        ["ALTER USER `app`@`%` ACCOUNT UNLOCK"]
    );
}

#[test]
fn drop_names_what_runs_as_the_account_and_the_own_login() {
    let path = users_folder().child(NodeKind::User, "admin@%");
    let mut form = DropAccount::new(path).unwrap();
    form.user_management_loaded(&Ok(management(MysqlFlavor::Mysql, false, true)));
    form.principal_described(&Ok(PrincipalDescription::Account(described(false))));
    let destruction = form.destruction(Dialect::Mysql).unwrap();
    assert_eq!(destruction.summary, "Drops the user admin@%.");
    assert_eq!(
        destruction.consequences,
        [
            "These run as it, and fail once it is dropped: shop.totals (view).",
            "It is the account DbRover is connected as: this data source can't connect as it \
             again.",
        ]
    );
    assert_eq!(statements(&form).unwrap(), ["DROP USER `admin`@`%`"]);
}
