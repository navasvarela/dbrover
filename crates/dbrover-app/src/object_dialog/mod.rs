//! The object helpers' dialogs: a form saying what to create or change, then the SQL it will
//! run, shown before anything runs (the review panel). Nothing reaches the database until
//! Execute is clicked; Open in console and Copy hand the SQL over instead.
//!
//! Each helper is an [`ObjectForm`]; this module is the frame they share: the window, the
//! confirmation a destructive change needs, the review panel, and waiting for the engine.

mod confirm;
mod fields;
mod index;
mod review;
mod role;
mod schema;
mod table;
mod table_form;
mod view;

use dbrover_core::{
    CatalogPath, DataSource, DataSourceId, NodeKind, RoleDescription, SafetyLevel, SourceColor,
};
use dbrover_engine::{Event, SessionState};
use dbrover_language::QueryLanguage;
use dbrover_sql::{
    DdlError, DdlOperation, Dialect, ExistingTable, ObjectName, ViewKind, ddl_statements,
};
use egui::{Id, Ui, Window};

pub use confirm::Destruction;
use index::{DropIndex, NewIndex};
use role::{DropRole, EditRole, NewRole, RenameRole, RoleKind};
use schema::{DropSchema, NewSchema};
use table::{DropTable, RenameTable, TableAt, TruncateTable};
use table_form::TableForm;
use view::{DropView, EditView, NewView, RefreshView, RenameView, ViewAt};

use crate::catalog_tree::CatalogTree;
use crate::explorer::Helper;
use review::{Review, ReviewAction};

/// The window's width when it opens; its height follows what the form shows.
const DEFAULT_WIDTH: f32 = 620.0;

/// The data source a dialog changes, and what about it decides how careful the dialog is.
#[derive(Debug, Clone)]
pub struct Target {
    pub data_source: DataSourceId,
    pub name: String,
    pub dialect: Dialect,
    pub safety: SafetyLevel,
    /// Coloured red, the colour that marks production: a destructive change asks for the
    /// object's name to be typed.
    pub red: bool,
}

impl Target {
    /// The data source the helpers change, or `None` when it doesn't speak SQL, which is the
    /// only language they write, or speaks a dialect whose DDL they don't write (CQL).
    pub fn new(data_source: &DataSource) -> Option<Self> {
        Some(Self {
            data_source: data_source.id,
            name: data_source.name.clone(),
            dialect: QueryLanguage::from(data_source.config.driver())
                .dialect()
                .filter(|dialect| dialect.writes_ddl())?,
            safety: data_source.safety,
            red: data_source.color == Some(SourceColor::Red),
        })
    }
}

/// One helper's fields: what the user asks for, turned into operations for the builder.
pub trait ObjectForm {
    /// The window's title, e.g. "New table" or "Drop table orders".
    fn title(&self) -> String;

    /// Draws the fields.
    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>);

    /// Catalog levels the form wants loaded, such as a foreign key target's columns: asked
    /// for after every frame, and loaded once.
    fn catalog_requests(&self) -> Vec<CatalogPath> {
        Vec::new()
    }

    /// The catalog node whose definition the form starts from, such as the view Edit view
    /// changes: loaded once, when the dialog opens.
    fn definition_request(&self) -> Option<CatalogPath> {
        None
    }

    /// The answer to [`ObjectForm::definition_request`].
    fn definition_loaded(&mut self, _result: &Result<String, String>, _dialect: Dialect) {}

    /// The role the form starts from, such as the one Edit changes: described once, when
    /// the dialog opens.
    fn role_request(&self) -> Option<CatalogPath> {
        None
    }

    /// The answer to [`ObjectForm::role_request`].
    fn role_described(&mut self, _result: &Result<RoleDescription, String>) {}

    /// The table the form starts from, such as the one Modify table changes: loaded once,
    /// when the dialog opens.
    fn table_request(&self) -> Option<CatalogPath> {
        None
    }

    /// The answer to [`ObjectForm::table_request`].
    fn table_loaded(&mut self, _result: &Result<ExistingTable, String>) {}

    /// What the form asks for, or why it can't be written as SQL yet.
    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError>;

    /// The catalog node whose children change once the operations are applied, refreshed
    /// afterwards.
    fn changed(&self) -> CatalogPath;

    /// For a change that destroys something, what it destroys, spelt out before Execute.
    fn destruction(&self, _dialect: Dialect) -> Option<Destruction> {
        None
    }
}

/// What a form may read while it draws.
pub struct FormContext<'a> {
    pub target: &'a Target,
    /// The data source's catalog as loaded so far, the same one the explorer and completion
    /// use. A form only reads it, and asks for more with
    /// [`ObjectForm::catalog_requests`].
    pub catalog: &'a CatalogTree,
}

/// What the app does for an open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectDialogAction {
    /// Runs the statements with `Command::ApplyDdl`; the dialog waits for the answer.
    Execute {
        data_source: DataSourceId,
        statements: Vec<String>,
    },
    /// Opens a new console on the data source holding `text`, and closes the dialog.
    OpenInConsole {
        data_source: DataSourceId,
        text: String,
    },
    Close,
}

/// The form for a helper the explorer asked for. `None` if its node isn't the kind the
/// helper works on, which the explorer's menus never offer.
pub fn form(helper: Helper, dialect: Dialect) -> Option<Box<dyn ObjectForm>> {
    let first_type = dialect.capabilities().common_types.first().copied();
    Some(match helper {
        Helper::NewTable(path) => {
            Box::new(TableForm::new_table(path, first_type.unwrap_or_default())?)
        }
        Helper::ModifyTable(path) => Box::new(TableForm::modify(path)?),
        Helper::RenameTable(path) => Box::new(RenameTable::new(TableAt::new(path)?)),
        Helper::TruncateTable(path) => Box::new(TruncateTable::new(TableAt::new(path)?)),
        Helper::DropTable(path) => Box::new(DropTable::new(TableAt::new(path)?)),
        Helper::NewIndex(path) => Box::new(NewIndex::new(&path)?),
        Helper::DropIndex(path) => Box::new(DropIndex::new(&path)?),
        Helper::NewView(path) => Box::new(NewView::new(path, ViewKind::View)?),
        Helper::NewMaterializedView(path) => Box::new(NewView::new(path, ViewKind::Materialized)?),
        Helper::EditView(path) => Box::new(EditView::new(ViewAt::new(path)?)),
        Helper::RenameView(path) => Box::new(RenameView::new(ViewAt::new(path)?)),
        Helper::RefreshView(path) => Box::new(RefreshView::new(ViewAt::new(path)?)?),
        Helper::DropView(path) => Box::new(DropView::new(ViewAt::new(path)?)),
        Helper::NewSchema(path) => Box::new(NewSchema::new(path)?),
        Helper::DropSchema(path) => Box::new(DropSchema::new(path)?),
        Helper::NewUser(path) => Box::new(NewRole::new(&path, RoleKind::User)?),
        Helper::NewRole(path) => Box::new(NewRole::new(&path, RoleKind::Role)?),
        Helper::EditRole(path) => Box::new(EditRole::new(path)?),
        Helper::RenameRole(path) => Box::new(RenameRole::new(&path)?),
        Helper::DropRole(path) => Box::new(DropRole::new(path)?),
    })
}

pub struct ObjectDialog {
    target: Target,
    form: Box<dyn ObjectForm>,
    review: Review,
    confirmation: confirm::Confirmation,
}

impl ObjectDialog {
    pub fn new(target: Target, form: Box<dyn ObjectForm>) -> Self {
        Self {
            target,
            form,
            review: Review::default(),
            confirmation: confirm::Confirmation::default(),
        }
    }

    pub fn data_source(&self) -> DataSourceId {
        self.target.data_source
    }

    /// See [`ObjectForm::catalog_requests`].
    pub fn catalog_requests(&self) -> Vec<CatalogPath> {
        self.form.catalog_requests()
    }

    /// See [`ObjectForm::definition_request`].
    pub fn definition_request(&self) -> Option<CatalogPath> {
        self.form.definition_request()
    }

    /// See [`ObjectForm::role_request`].
    pub fn role_request(&self) -> Option<CatalogPath> {
        self.form.role_request()
    }

    /// See [`ObjectForm::table_request`].
    pub fn table_request(&self) -> Option<CatalogPath> {
        self.form.table_request()
    }

    /// Follows the engine's answer to Execute. Returns the node to refresh once the change
    /// has been applied, which means the dialog is done.
    pub fn apply(&mut self, event: &Event) -> Option<CatalogPath> {
        let data_source = self.target.data_source;
        match event {
            Event::DdlApplied {
                data_source: answered,
                result,
            } if *answered == data_source => match result {
                Ok(()) => return Some(self.form.changed()),
                Err(failure) => self.review.failed(failure),
            },
            Event::DefinitionLoaded { id, path, result }
                if *id == data_source && Some(path) == self.form.definition_request().as_ref() =>
            {
                self.form.definition_loaded(result, self.target.dialect);
            }
            Event::RoleDescribed {
                data_source: answered,
                path,
                result,
            } if *answered == data_source && Some(path) == self.form.role_request().as_ref() => {
                self.form.role_described(result);
            }
            Event::TableLoaded {
                data_source: answered,
                path,
                result,
            } if *answered == data_source && Some(path) == self.form.table_request().as_ref() => {
                self.form.table_loaded(result);
            }
            // A request sent while connecting is never answered if the connection fails.
            Event::SessionChanged { id, state } if *id == data_source => match state {
                SessionState::Failed(error) | SessionState::Lost(error) => {
                    self.review.not_sent(&format!("Could not connect: {error}"));
                }
                SessionState::Disconnected => self.review.not_sent("Disconnected."),
                SessionState::Connecting | SessionState::Connected(_) => {}
            },
            _ => {}
        }
        None
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        catalog: &CatalogTree,
    ) -> Option<ObjectDialogAction> {
        let mut open = true;
        let mut action = None;
        Window::new(self.form.title())
            .id(Id::new("object_dialog"))
            .open(&mut open)
            .collapsible(false)
            .default_width(DEFAULT_WIDTH)
            .show(ctx, |ui| action = self.contents(ui, catalog));
        if !open && !self.review.is_running() {
            return Some(ObjectDialogAction::Close);
        }
        action
    }

    fn contents(&mut self, ui: &mut Ui, catalog: &CatalogTree) -> Option<ObjectDialogAction> {
        let context = FormContext {
            target: &self.target,
            catalog,
        };
        self.form.show(ui, &context);
        let destruction = self.form.destruction(self.target.dialect);
        let confirmed = match &destruction {
            Some(destruction) => self.confirmation.show(ui, destruction, &self.target),
            None => true,
        };
        ui.separator();
        let statements = self
            .form
            .operations()
            .and_then(|operations| ddl_statements(&operations, self.target.dialect));
        let action = self.review.show(ui, &self.target, &statements, confirmed)?;
        let statements = statements.ok()?;
        Some(match action {
            ReviewAction::Execute => ObjectDialogAction::Execute {
                data_source: self.target.data_source,
                statements,
            },
            ReviewAction::OpenInConsole => ObjectDialogAction::OpenInConsole {
                data_source: self.target.data_source,
                text: review::script(&statements),
            },
            ReviewAction::Cancel => ObjectDialogAction::Close,
        })
    }
}

/// The schema object at `path`, named as the builder names it: in the path's schema, which
/// every engine's tree has above its tables (MySQL's and ClickHouse's databases are schemas
/// there).
fn object_name(path: &CatalogPath) -> Option<ObjectName> {
    let object = path.last()?;
    if matches!(
        object.kind,
        NodeKind::Database | NodeKind::Schema | NodeKind::Folder(_)
    ) {
        return None;
    }
    Some(ObjectName::new(
        path.name_of(NodeKind::Schema),
        &object.name,
    ))
}

#[cfg(test)]
mod tests;
