//! The modal dialog for creating or editing a data source, with "Test connection".

use std::path::Path;

use dbrover_core::{
    DataSource, DataSourceId, DriverKind, ElasticsearchAuth, MongoConfig, SafetyLevel, ServerInfo,
    SourceColor, expand, expand_path, has_placeholder,
};
use dbrover_engine::read_only::{self, Guard};
use dbrover_engine::{DatabaseFile, Event, PasswordUpdate, ProcessEnvironment};
use egui::{
    Align, Button, Color32, Grid, Id, Layout, Modal, RichText, Sense, Stroke, TextEdit, Ui,
    WidgetInfo, WidgetType, vec2,
};

use crate::data_source_form::{
    DataSourceForm, DatabaseField, DriverFields, DuckDbFields, DuckDbLocation, ElasticsearchFields,
    FormError, McpLoginFields, McpPasswordChange, MongoFields, MongoTarget, ServerFields,
    SshFields, SshLoginChoice, TlsChoice,
};
use crate::file_picker::{self, FileEngine};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

const DIALOG_WIDTH: f32 = 420.0;
const FIELD_WIDTH: f32 = 280.0;
const SWATCH_RADIUS: f32 = 8.0;

pub enum DialogAction {
    Test {
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
    },
    Save {
        data_source: DataSource,
        password: PasswordUpdate,
        ssh_secret: PasswordUpdate,
        mcp_password: McpPasswordChange,
    },
    /// Create an empty database file at the path chosen in the Save dialog.
    CreateDatabaseFile(DatabaseFile),
    Close,
}

pub struct DataSourceDialog {
    id: DataSourceId,
    is_new: bool,
    form: DataSourceForm,
    status: Status,
}

enum Status {
    Editing,
    Invalid(FormError),
    Testing,
    Tested(Result<ServerInfo, String>),
    Saving,
    SaveFailed(String),
    Creating,
    Created(Result<(), String>),
}

impl DataSourceDialog {
    pub fn create(driver: DriverKind) -> Self {
        Self {
            id: DataSourceId::generate(),
            is_new: true,
            form: DataSourceForm::new(driver),
            status: Status::Editing,
        }
    }

    pub fn edit(data_source: &DataSource) -> Self {
        Self {
            id: data_source.id,
            is_new: false,
            form: DataSourceForm::from_data_source(data_source),
            status: Status::Editing,
        }
    }

    /// Updates the dialog from an engine event. Returns `true` once its data source has been
    /// saved, meaning the dialog is done.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::ConnectionTested { id, result } if *id == self.id => {
                self.status = Status::Tested(result.clone());
                false
            }
            Event::DataSourceSaveFailed { id, error } if *id == self.id => {
                self.status = Status::SaveFailed(error.clone());
                false
            }
            Event::DataSourceSaved(data_source) => data_source.id == self.id,
            Event::DatabaseFileCreated { file, result } if self.is_creating(file) => {
                self.status = Status::Created(result.clone());
                false
            }
            _ => false,
        }
    }

    /// Whether `file` is the database this dialog's File field names.
    fn is_creating(&self, file: &DatabaseFile) -> bool {
        let path = match (&self.form.fields, file) {
            (DriverFields::Sqlite(fields), DatabaseFile::Sqlite(_)) => &fields.path,
            (
                DriverFields::DuckDb(DuckDbFields {
                    location: DuckDbLocation::File,
                    path,
                    ..
                }),
                DatabaseFile::DuckDb(_),
            ) => path,
            _ => return false,
        };
        Path::new(path.trim()) == file.path()
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<DialogAction> {
        let modal = Modal::new(Id::new("data_source_dialog")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(DialogAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<DialogAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.horizontal(|ui| {
            let logo_size = ui.text_style_height(&egui::TextStyle::Heading);
            ui.add(icons::driver_logo_sized(
                self.form.driver(),
                palette,
                logo_size,
            ));
            ui.heading(self.title());
        });
        ui.add_space(ui.spacing().item_spacing.y);
        let new_database = self.fields(ui);
        ui.add_space(ui.spacing().item_spacing.y);
        self.status_line(ui);
        ui.separator();
        let action = self.buttons(ui);
        match new_database {
            Some(file) => {
                self.status = Status::Creating;
                Some(DialogAction::CreateDatabaseFile(file))
            }
            None => action,
        }
    }

    fn title(&self) -> String {
        let driver = self.form.driver().display_name();
        if self.is_new {
            format!("New {driver} data source")
        } else {
            format!("Edit {driver} data source")
        }
    }

    /// Draws the form. Returns a database file if the user chose to create one.
    fn fields(&mut self, ui: &mut Ui) -> Option<DatabaseFile> {
        let password_hint = if self.is_new { "none" } else { "unchanged" };
        Grid::new("data_source_fields")
            .num_columns(2)
            .show(ui, |ui| {
                text_row(ui, "Name", &mut self.form.name, "optional");
                ui.label("Colour");
                color_picker(ui, &mut self.form.color);
                ui.end_row();
                safety_rows(ui, &mut self.form);
                let server = match &mut self.form.fields {
                    DriverFields::Postgres(fields) => Some(&mut fields.server),
                    DriverFields::Mysql(fields) => Some(&mut fields.server),
                    DriverFields::SqlServer(fields) => Some(&mut fields.server),
                    DriverFields::ClickHouse(fields) => Some(&mut fields.server),
                    DriverFields::Redis(fields) => Some(&mut fields.server),
                    DriverFields::Mongo(fields) => {
                        mongo_rows(ui, fields, password_hint);
                        None
                    }
                    DriverFields::Elasticsearch(fields) => {
                        elasticsearch_rows(ui, fields, password_hint);
                        None
                    }
                    DriverFields::Sqlite(fields) => {
                        return file_rows(
                            ui,
                            FileEngine::Sqlite,
                            &mut fields.path,
                            &mut fields.read_only,
                        );
                    }
                    DriverFields::DuckDb(fields) => return duckdb_rows(ui, fields),
                };
                if let Some(server) = server {
                    server_rows(ui, server, password_hint);
                }
                if let DriverFields::Redis(fields) = &mut self.form.fields {
                    text_row(ui, "Key separator", &mut fields.separator, "none");
                }
                // Only where `dbrover mcp` can serve the engine; elsewhere it would never be used.
                match read_only::guard(self.form.driver(), self.form.storage()) {
                    Guard::Session(_) => mcp_login_rows(ui, &mut self.form.mcp_login, false),
                    Guard::Login => mcp_login_rows(ui, &mut self.form.mcp_login, true),
                    Guard::CheckOnly => {}
                }
                None
            })
            .inner
    }

    fn status_line(&self, ui: &mut Ui) {
        let palette = &Palette::for_visuals(ui.visuals());
        let error_color = palette.error;
        match &self.status {
            Status::Editing => {}
            Status::Invalid(error) => {
                ui.colored_label(error_color, error.to_string());
            }
            Status::Testing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Testing connection…");
                });
            }
            Status::Tested(Ok(server)) => {
                widgets::status(
                    ui,
                    icons::CHECK_CIRCLE,
                    palette.success,
                    &format!("Connected to {server}."),
                );
            }
            Status::Tested(Err(error)) => {
                ui.colored_label(error_color, error);
            }
            Status::Saving => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving…");
                });
            }
            Status::SaveFailed(error) => {
                ui.colored_label(error_color, format!("Could not save: {error}"));
            }
            Status::Creating => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Creating the database file…");
                });
            }
            Status::Created(Ok(())) => {
                widgets::status(
                    ui,
                    icons::CHECK_CIRCLE,
                    palette.success,
                    "Created an empty database. Save to keep this data source.",
                );
            }
            Status::Created(Err(error)) => {
                ui.colored_label(error_color, format!("Could not create the file: {error}"));
            }
        }
    }

    fn buttons(&mut self, ui: &mut Ui) -> Option<DialogAction> {
        let testing = matches!(self.status, Status::Testing);
        let saving = matches!(self.status, Status::Saving);
        let (test, save, cancel) = ui
            .horizontal(|ui| {
                let test = ui
                    .add_enabled(!testing && !saving, Button::new("Test connection"))
                    .clicked();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let palette = &Palette::for_visuals(ui.visuals());
                    let save_button = Button::new(RichText::new("Save").color(palette.on_primary))
                        .fill(palette.primary);
                    let save = ui.add_enabled(!saving, save_button).clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (test, save, cancel)
                })
                .inner
            })
            .inner;

        if cancel {
            Some(DialogAction::Close)
        } else if test {
            let ssh_secret = self.form.ssh_secret_update();
            self.submit(Status::Testing)
                .map(|(data_source, password)| DialogAction::Test {
                    data_source,
                    password,
                    ssh_secret,
                })
        } else if save {
            let mcp_password = self.form.mcp_password_change();
            let ssh_secret = self.form.ssh_secret_update();
            self.submit(Status::Saving)
                .map(|(data_source, password)| DialogAction::Save {
                    data_source,
                    password,
                    ssh_secret,
                    mcp_password,
                })
        } else {
            None
        }
    }

    /// Validates the form and, if it is valid, moves to `next` and returns what to send.
    fn submit(&mut self, next: Status) -> Option<(DataSource, PasswordUpdate)> {
        match self.form.to_data_source(self.id) {
            Ok(data_source) => {
                self.status = next;
                Some((data_source, self.form.password_update()))
            }
            Err(error) => {
                self.status = Status::Invalid(error);
                None
            }
        }
    }
}

/// The rows every network engine shares: where the server is, whom to connect as, and how to
/// encrypt. An engine's own rows go before or after them.
fn server_rows(ui: &mut Ui, fields: &mut ServerFields, password_hint: &str) {
    let defaults = fields.defaults;
    let default_port = fields.default_port().to_string();
    setting_row(ui, "Host", &mut fields.host, defaults.host, Resolve::Text);
    setting_row(ui, "Port", &mut fields.port, &default_port, Resolve::Text);
    if defaults.database_field != DatabaseField::Absent {
        setting_row(
            ui,
            defaults.database_field.label(),
            &mut fields.database,
            defaults.database,
            Resolve::Text,
        );
    }
    setting_row(ui, "User", &mut fields.user, defaults.user, Resolve::Text);
    password_rows(ui, fields, password_hint);
    tls_rows(ui, fields);
    ssh_rows(ui, &mut fields.ssh, password_hint);
}

/// Elasticsearch's rows: the shared server rows, or, signing in with an API key, the same rows
/// with the key in the password's place and no user.
fn elasticsearch_rows(ui: &mut Ui, fields: &mut ElasticsearchFields, password_hint: &str) {
    ui.label("Sign in with");
    ui.horizontal(|ui| {
        ui.radio_value(
            &mut fields.auth,
            ElasticsearchAuth::Password,
            "User and password",
        );
        ui.radio_value(&mut fields.auth, ElasticsearchAuth::ApiKey, "API key");
    });
    ui.end_row();
    let server = &mut fields.server;
    match fields.auth {
        ElasticsearchAuth::Password => server_rows(ui, server, password_hint),
        ElasticsearchAuth::ApiKey => {
            let defaults = server.defaults;
            let default_port = server.default_port().to_string();
            setting_row(ui, "Host", &mut server.host, defaults.host, Resolve::Text);
            setting_row(ui, "Port", &mut server.port, &default_port, Resolve::Text);
            secret_rows(ui, server, "API key", password_hint);
            ui.label("");
            ui.weak("The encoded key Elasticsearch gives, kept in the keychain as a password is.");
            ui.end_row();
            tls_rows(ui, server);
            ssh_rows(ui, &mut server.ssh, password_hint);
        }
    }
}

/// The password to save in the keychain, or the command that prints one on every connection.
fn password_rows(ui: &mut Ui, fields: &mut ServerFields, password_hint: &str) {
    secret_rows(ui, fields, "Password", password_hint);
}

/// [`password_rows`] for a secret with another name, such as an API key.
fn secret_rows(ui: &mut Ui, fields: &mut ServerFields, name: &str, password_hint: &str) {
    if fields.password_from_command {
        password_command_row(ui, &mut fields.password_command);
    } else {
        let label = ui.label(name);
        ui.add(
            TextEdit::singleline(&mut fields.password)
                .password(true)
                .hint_text(password_hint)
                .desired_width(FIELD_WIDTH),
        )
        .labelled_by(label.id);
        ui.end_row();
    }
    ui.label("");
    ui.checkbox(&mut fields.password_from_command, "Password from a command")
        .on_hover_text(
            "For short-lived tokens and secret stores (aws, gcloud, az, op, vault): DbRover runs \
             the command on every connection, and what it prints is the password",
        );
    ui.end_row();
}

fn password_command_row(ui: &mut Ui, command: &mut String) {
    let label = ui.label("Password command");
    ui.vertical(|ui| {
        ui.add(
            TextEdit::multiline(command)
                .font(egui::TextStyle::Monospace)
                .hint_text("op\nread\nop://Work/{host}/password")
                .desired_rows(3)
                .desired_width(FIELD_WIDTH),
        )
        .labelled_by(label.id);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.label(
            RichText::new(
                "The program, then each argument, one per line, with no shell. {host}, {port}, \
                 {user} and {database} are filled in.",
            )
            .small()
            .color(palette.text_weak),
        );
    });
    ui.end_row();
}

/// Whether to connect through an SSH server and, if so, where it is and how to sign in.
fn ssh_rows(ui: &mut Ui, fields: &mut SshFields, secret_hint: &str) {
    let label = ui.label("SSH");
    ui.checkbox(&mut fields.enabled, "Connect through an SSH server")
        .labelled_by(label.id)
        .on_hover_text(
            "For a database only a jump host can reach: DbRover opens an SSH tunnel and \
             connects through it",
        );
    ui.end_row();
    if !fields.enabled {
        return;
    }
    let default_port = dbrover_core::SshConfig::DEFAULT_PORT.to_string();
    setting_row(
        ui,
        "SSH host",
        &mut fields.host,
        "bastion.example.com",
        Resolve::Text,
    );
    setting_row(
        ui,
        "SSH port",
        &mut fields.port,
        &default_port,
        Resolve::Text,
    );
    setting_row(ui, "SSH user", &mut fields.user, "required", Resolve::Text);
    let label = ui.label("SSH login");
    egui::ComboBox::from_id_salt("data_source_ssh_login")
        .selected_text(fields.login.label())
        .show_ui(ui, |ui| {
            for choice in SshLoginChoice::ALL {
                ui.selectable_value(&mut fields.login, choice, choice.label())
                    .on_hover_text(choice.description());
            }
        })
        .response
        .labelled_by(label.id)
        .on_hover_text(fields.login.description());
    ui.end_row();
    if fields.login == SshLoginChoice::KeyFile {
        setting_row(
            ui,
            "Key file",
            &mut fields.key_file,
            "~/.ssh/id_ed25519",
            Resolve::Path,
        );
        ui.label("");
        let palette = &Palette::for_visuals(ui.visuals());
        if file_button(ui, icons::FOLDER_OPEN, palette.warning, "Browse…")
            .on_hover_text("Choose a private key in OpenSSH's format")
            .clicked()
            && let Some(path) = file_picker::pick_ssh_key_file(&fields.key_file)
        {
            fields.key_file = path.display().to_string();
        }
        ui.end_row();
    }
    let Some(secret_label) = fields.login.secret_label() else {
        return;
    };
    let hint = match fields.login {
        SshLoginChoice::KeyFile => "only for an encrypted key",
        SshLoginChoice::Agent | SshLoginChoice::Password => secret_hint,
    };
    let label = ui.label(secret_label);
    ui.add(
        TextEdit::singleline(&mut fields.secret)
            .password(true)
            .hint_text(hint)
            .desired_width(FIELD_WIDTH),
    )
    .labelled_by(label.id);
    ui.end_row();
}

/// The login `dbrover mcp` signs in as, so the database's grants limit what an assistant can
/// do; the project settings warn when a project allows changes without one.
/// The MCP login's user and password. `required` where the engine has no read-only mode, so
/// MCP clients are served only through such a login.
fn mcp_login_rows(ui: &mut Ui, fields: &mut McpLoginFields, required: bool) {
    let label = ui.label("MCP login").on_hover_text(if required {
        "Another user for MCP clients to sign in as, used only by dbrover mcp. This engine has \
         no read-only mode, so MCP clients are served only through such a login: give it \
         nothing but reading."
    } else {
        "Another user for MCP clients to sign in as, used only by dbrover mcp. Give it only the \
         grants an assistant should have: beyond reading, only the database's grants are \
         airtight."
    });
    let hint = if required {
        "none: not served to MCP clients"
    } else {
        "none: the user above"
    };
    ui.add(
        TextEdit::singleline(&mut fields.user)
            .hint_text(hint)
            .desired_width(FIELD_WIDTH),
    )
    .labelled_by(label.id);
    ui.end_row();
    if fields.user.trim().is_empty() {
        return;
    }
    let label = ui.label("MCP password");
    ui.add(
        TextEdit::singleline(&mut fields.password)
            .password(true)
            .hint_text("unchanged")
            .desired_width(FIELD_WIDTH),
    )
    .labelled_by(label.id);
    ui.end_row();
}

/// The TLS mode and, for `verify-full`, the file of authorities to trust.
fn tls_rows(ui: &mut Ui, fields: &mut ServerFields) {
    let label = ui.label("TLS");
    let mut chosen = fields.tls;
    egui::ComboBox::from_id_salt("data_source_tls")
        .selected_text(fields.tls.label())
        .show_ui(ui, |ui| {
            for &choice in fields.tls_choices() {
                ui.selectable_value(&mut chosen, choice, choice.label())
                    .on_hover_text(choice.description());
            }
        })
        .response
        .labelled_by(label.id)
        .on_hover_text(fields.tls.description());
    ui.end_row();
    if chosen != fields.tls {
        fields.choose_tls(chosen);
    }
    if fields.tls != TlsChoice::VerifyFull {
        return;
    }
    setting_row(
        ui,
        "CA file",
        &mut fields.ca_file,
        "public authorities",
        Resolve::Path,
    );
    ui.label("");
    let palette = &Palette::for_visuals(ui.visuals());
    if file_button(ui, icons::FOLDER_OPEN, palette.warning, "Browse…")
        .on_hover_text("Choose a PEM file of authorities to trust")
        .clicked()
        && let Some(path) = file_picker::pick_ca_file(&fields.ca_file)
    {
        fields.ca_file = path.display().to_string();
    }
    ui.end_row();
}

/// The file field, then Browse… (open an existing database) and New… (create one). Returns the
/// database to create, if New… was used; the engine creates it.
fn file_rows(
    ui: &mut Ui,
    engine: FileEngine,
    path: &mut String,
    read_only: &mut bool,
) -> Option<DatabaseFile> {
    let hint = match engine {
        FileEngine::Sqlite => "/path/to/database.db",
        FileEngine::DuckDb => "/path/to/database.duckdb",
    };
    setting_row(ui, "File", path, hint, Resolve::Path);
    ui.label("");
    let new_database = ui
        .horizontal(|ui| {
            let palette = &Palette::for_visuals(ui.visuals());
            if file_button(ui, icons::FOLDER_OPEN, palette.warning, "Browse…")
                .on_hover_text("Open an existing database file")
                .clicked()
                && let Some(chosen) = file_picker::pick_database_file(engine, path)
            {
                *path = chosen.display().to_string();
            }
            let new_path = file_button(ui, icons::FILE_PLUS, palette.success, "New…")
                .on_hover_text("Create a new, empty database file")
                .clicked()
                .then(|| file_picker::pick_new_database_file(engine, path))
                .flatten()?;
            *path = new_path.display().to_string();
            // A database you just created is one you mean to write to.
            *read_only = false;
            Some(engine.file(new_path))
        })
        .inner;
    ui.end_row();
    ui.label("");
    ui.checkbox(read_only, "Read-only");
    ui.end_row();
    new_database
}

/// A file, as for SQLite, or a database in memory, for querying CSV, Parquet and JSON files.
/// MongoDB's rows: the shared server rows, or a connection string with the database, user and
/// password beside it, and the database the user signs in against.
fn mongo_rows(ui: &mut Ui, fields: &mut MongoFields, password_hint: &str) {
    ui.label("Connect with");
    ui.horizontal(|ui| {
        ui.radio_value(&mut fields.target, MongoTarget::Server, "Server");
        ui.radio_value(
            &mut fields.target,
            MongoTarget::ConnectionString,
            "Connection string",
        );
    });
    ui.end_row();
    match fields.target {
        MongoTarget::Server => server_rows(ui, &mut fields.server, password_hint),
        MongoTarget::ConnectionString => {
            setting_row(
                ui,
                "Connection string",
                &mut fields.connection_string,
                "mongodb+srv://cluster0.example.mongodb.net/",
                Resolve::Text,
            );
            fields.take_credentials();
            ui.label("");
            ui.weak("Its user and password move to their own fields; the password is kept in the keychain.");
            ui.end_row();
            let server = &mut fields.server;
            let database_hint = server.defaults.database;
            setting_row(
                ui,
                "Database",
                &mut server.database,
                database_hint,
                Resolve::Text,
            );
            setting_row(ui, "User", &mut server.user, "none", Resolve::Text);
            password_rows(ui, server, password_hint);
        }
    }
    setting_row(
        ui,
        "Auth database",
        &mut fields.auth_database,
        MongoConfig::DEFAULT_AUTH_DATABASE,
        Resolve::Text,
    );
}

fn duckdb_rows(ui: &mut Ui, fields: &mut DuckDbFields) -> Option<DatabaseFile> {
    ui.label("Database");
    ui.horizontal(|ui| {
        ui.radio_value(&mut fields.location, DuckDbLocation::File, "File");
        ui.radio_value(&mut fields.location, DuckDbLocation::InMemory, "In memory");
    });
    ui.end_row();
    match fields.location {
        DuckDbLocation::File => file_rows(
            ui,
            FileEngine::DuckDb,
            &mut fields.path,
            &mut fields.read_only,
        ),
        DuckDbLocation::InMemory => {
            ui.label("");
            ui.weak(
                "Starts empty each time it connects. Query files with read_csv(), \
                 read_parquet() and read_json(), or by naming them: FROM 'orders.csv'.",
            );
            ui.end_row();
            None
        }
    }
}

/// A button with a coloured icon before its text, named by the text alone.
fn file_button(ui: &mut Ui, glyph: &str, icon_color: Color32, text: &str) -> egui::Response {
    let label = widgets::icon_text(ui, glyph, icon_color, text);
    widgets::name(ui.button(label), WidgetType::Button, text, true)
}

/// The safety level, like radio buttons, then a suggestion when the colour makes one, and a
/// note when the engine can only be made read-only in part.
fn safety_rows(ui: &mut Ui, form: &mut DataSourceForm) {
    let label = ui.label("Safety");
    ui.horizontal(|ui| {
        for level in SafetyLevel::ALL {
            let selected = form.safety == level;
            let response = ui.selectable_value(&mut form.safety, level, level.display_name());
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::RadioButton,
                    true,
                    selected,
                    level.display_name(),
                )
            });
            response.on_hover_text(level.description());
        }
    })
    .response
    .labelled_by(label.id);
    ui.end_row();
    let palette = &Palette::for_visuals(ui.visuals());
    if let Some(suggested) = SafetyLevel::suggested_by(form.color)
        .filter(|suggested| form.safety == SafetyLevel::Normal && *suggested != form.safety)
    {
        ui.label("");
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Red usually marks production.")
                    .small()
                    .color(palette.text_weak),
            );
            if ui
                .small_button(format!(
                    "Make it {}",
                    suggested.display_name().to_lowercase()
                ))
                .clicked()
            {
                form.safety = suggested;
            }
        });
        ui.end_row();
    }
    let unchecked = "has no read-only mode DbRover can use: statements are checked, but a write \
                     inside a procedure or function isn't stopped.";
    let partly = match read_only::guard(form.driver(), form.storage()) {
        Guard::Session(_) => None,
        Guard::Login => Some(format!(
            "{unchecked} Sign in as {} to have the database refuse it.",
            read_only::read_only_login(form.driver())
        )),
        Guard::CheckOnly => {
            Some("can't open this database read-only: only the statements are checked.".to_owned())
        }
    };
    if let Some(partly) = partly.filter(|_| form.safety == SafetyLevel::ReadOnly) {
        ui.label("");
        ui.label(
            RichText::new(format!("{} {partly}", form.driver().display_name()))
                .small()
                .color(palette.warning),
        );
        ui.end_row();
    }
}

/// "None" and one swatch per [`SourceColor`], like radio buttons.
fn color_picker(ui: &mut Ui, selected: &mut Option<SourceColor>) {
    let palette = &Palette::for_visuals(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let choices = std::iter::once(None).chain(SourceColor::ALL.map(Some));
        for choice in choices {
            if swatch(ui, choice, *selected == choice, palette).clicked() {
                *selected = choice;
            }
        }
    });
}

/// A filled circle in `color`, or a struck-out empty one for "no colour". The selected one
/// gets a ring.
fn swatch(
    ui: &mut Ui,
    color: Option<SourceColor>,
    selected: bool,
    palette: &Palette,
) -> egui::Response {
    let size = vec2(SWATCH_RADIUS * 2.0 + 6.0, SWATCH_RADIUS * 2.0 + 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let name = color.map_or("No colour", SourceColor::display_name);
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, name));
    let center = rect.center();
    let painter = ui.painter();
    match color {
        Some(color) => {
            painter.circle_filled(center, SWATCH_RADIUS, palette.source(color));
        }
        None => {
            painter.circle_stroke(
                center,
                SWATCH_RADIUS - 0.5,
                Stroke::new(1.0, palette.text_weak),
            );
            let offset = vec2(SWATCH_RADIUS, -SWATCH_RADIUS) * std::f32::consts::FRAC_1_SQRT_2;
            painter.line_segment(
                [center - offset, center + offset],
                Stroke::new(1.0, palette.text_weak),
            );
        }
    }
    let ring = if selected {
        Stroke::new(2.0, palette.text)
    } else if response.hovered() {
        Stroke::new(1.0, palette.text_weak)
    } else {
        Stroke::new(0.0, Color32::TRANSPARENT)
    };
    painter.circle_stroke(center, SWATCH_RADIUS + 2.5, ring);
    response.on_hover_text(name)
}

/// How a setting's placeholders are resolved when connecting.
#[derive(Clone, Copy)]
enum Resolve {
    Text,
    /// Also turns a leading `~` into the home folder.
    Path,
}

/// A setting's row, then, when it holds a `${NAME}` placeholder, a row saying what that
/// currently resolves to, or why it doesn't: the same rules connecting uses.
fn setting_row(ui: &mut Ui, label: &str, value: &mut String, hint: &str, resolve: Resolve) {
    text_row(ui, label, value, hint);
    let text = value.trim();
    if !has_placeholder(text) {
        return;
    }
    let resolved = match resolve {
        Resolve::Text => expand(text, &ProcessEnvironment),
        Resolve::Path => {
            expand_path(Path::new(text), &ProcessEnvironment).map(|path| path.display().to_string())
        }
    };
    let palette = &Palette::for_visuals(ui.visuals());
    ui.label("");
    match resolved {
        Ok(resolved) => ui.label(
            RichText::new(format!("= {resolved}"))
                .small()
                .color(palette.text_weak),
        ),
        Err(error) => ui.label(
            RichText::new(error.to_string())
                .small()
                .color(palette.warning),
        ),
    }
    .on_hover_text("Placeholders are resolved from the environment DbRover was started in");
    ui.end_row();
}

/// A label and a text field, linked so screen readers (and UI tests) find the field by label.
fn text_row(ui: &mut Ui, label: &str, value: &mut String, hint: &str) {
    let label = ui.label(label);
    ui.add(
        TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(FIELD_WIDTH),
    )
    .labelled_by(label.id);
    ui.end_row();
}
