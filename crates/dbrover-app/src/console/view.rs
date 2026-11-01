//! Draws a console: toolbar, editor and results, and turns clicks and shortcuts into
//! [`ConsoleAction`]s for the app.

use std::time::Instant;

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Id, Margin, ScrollArea, TextEdit, Ui, vec2};

use super::highlight::Marks;
use super::{
    Candidate, ClipboardCopy, Disk, FormatScope, Found, LintOptions, QueryConsole,
    RecentCompletions, RowsToOpen, RunDestination, RunScope, RunState, SubmittedRun, TreeCatalog,
    cell_viewer, completion_popup, find_bar, highlight, results,
};
use crate::icons::{self, Role};
use crate::keymap::{Action, Keymap};
use crate::picture::PictureFormat;
use crate::theme::Palette;
use crate::widgets::{self, IconButton};
use dbrover_core::{CatalogPath, RelationName};
use dbrover_core::{SafetyLevel, TransactionStatus};
use dbrover_engine::{RunId, RunOrigin, RunOutcome, SessionState, TransactionEnd, TransactionMode};
use dbrover_language::ExplainDepth;
use dbrover_language::redis::KeyView;
use dbrover_sql::{Dialect, ProblemSource, Severity};
use dbrover_store::{ActionId, CustomAction, Snippet};

use crate::custom_actions::GridCells;

/// What the console's keys can do while its editor has focus.
const EDITOR_ACTIONS: [Action; 18] = [
    Action::Complete,
    Action::RunCurrent,
    Action::RunToFile,
    Action::RunAll,
    Action::Explain,
    Action::ExplainAnalyse,
    Action::Cancel,
    Action::ShowHistory,
    Action::Commit,
    Action::Rollback,
    Action::Format,
    Action::FormatAll,
    Action::NextProblem,
    Action::PreviousProblem,
    Action::Find,
    Action::Replace,
    Action::FindNext,
    Action::FindPrevious,
];

/// Height the results start at; the user can drag the line above them.
const RESULTS_HEIGHT: f32 = 280.0;
/// The editor and the results each keep at least this share of the console's height, so
/// neither can be dragged (or squeezed by a small window) out of reach.
const MIN_SHARE: f32 = 0.2;
const SPLITTER_HEIGHT: f32 = 6.0;

/// What the user asked a console to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleAction {
    Run(SubmittedRun),
    FetchMore(RunId),
    Cancel(RunId),
    EndTransaction(TransactionEnd),
    /// Saves the rows the grid has loaded, or every row; the app asks where.
    /// Opens the export dialog for the selected statement's rows.
    OpenExportDialog,
    /// Puts the selected cells on the clipboard; the app has the export options.
    Copy(ClipboardCopy),
    /// Opens the export dialog for the statement at the cursor, to run it into a file.
    RunToFile,
    /// Opens the data source's query history, to insert a past statement here.
    ShowHistory,
    /// Loads these catalog levels into the explorer's cache, for completion.
    LoadCatalog(Vec<CatalogPath>),
    /// Asks which columns and key a result's table has, to decide whether rows can be edited.
    DescribeTable(RelationName),
    /// Writes the grid's pending edits.
    SubmitEdits,
    /// Formats SQL in the editor, with the formatting settings the app holds.
    Format(FormatScope),
    /// Asks the database whether it would accept these statements, without running them.
    Validate(Vec<String>),
    /// A completion was accepted; it ranks higher next time, in every console.
    CompletionAccepted(Candidate),
    /// Connects again after the connection was lost, as a new session.
    Reconnect,
    /// Opens rows a foreign key leads to, in a new console on the same data source.
    OpenRows(RowsToOpen),
    /// Runs a custom action from `actions.toml` on the grid's selected cells.
    RunAction(ActionId, GridCells),
    /// Writes the shown chart as a picture; the app asks where.
    ExportChart(PictureFormat),
    /// Changes the shown Redis key from its key bar with `command`, then reads it again with
    /// `then`'s command: the same key, or the key it was renamed to.
    KeyCommand {
        command: String,
        then: KeyView,
    },
}

/// What a console draws with besides its own state.
pub struct ConsoleContext<'a> {
    pub keymap: &'a Keymap,
    /// The data source's cached catalog, for completion. `None` if the data source is gone.
    pub catalog: Option<TreeCatalog<'a>>,
    /// Which problems to look for while typing.
    pub lint: &'a LintOptions,
    /// Completions accepted lately, which rank first.
    pub recent: &'a RecentCompletions,
    /// The custom actions the grid's cell menu offers, for this console's driver.
    pub cell_actions: &'a [&'a CustomAction],
    /// The snippets completion offers, for this console's driver.
    pub snippets: &'a [&'a Snippet],
}

pub fn show(
    ui: &mut Ui,
    console: &mut QueryConsole,
    context: &ConsoleContext<'_>,
    actions: &mut Vec<ConsoleAction>,
) {
    let keymap = context.keymap;
    let editor_id = Id::new(("console_editor", console.id.0));
    shortcuts(ui, console, keymap, editor_id, actions);
    toolbar(ui, console, keymap, actions);
    file_bar(ui, console);
    if console.dialect().is_some_and(Dialect::has_transactions) {
        transaction_bar(ui, console, keymap, actions);
    }
    let found = find_bar::show(ui, console, keymap);
    ui.separator();
    let (editor_height, results_height) = split_heights(ui, console);
    let width = ui.available_width();
    ui.allocate_ui(vec2(width, editor_height), |ui| {
        editor(ui, console, editor_id, &found, context, actions);
    });
    splitter(ui, console);
    ui.allocate_ui(vec2(width, results_height), |ui| {
        results::show(ui, console, keymap, context.cell_actions, actions);
    });
    cell_viewer::show(ui.ctx(), console);
    actions.extend(
        console
            .take_accepted_completion()
            .map(ConsoleAction::CompletionAccepted),
    );
}

/// The editor's and the results' heights, from the results height the user last dragged to.
///
/// Laid out by hand rather than with a resizable `egui::Panel`: a panel lays its contents out
/// at full size even when the console is too short for it, and the editor below then covers
/// (and takes the clicks of) the results' summary line.
fn split_heights(ui: &Ui, console: &QueryConsole) -> (f32, f32) {
    let total = (ui.available_height() - SPLITTER_HEIGHT).max(0.0);
    let wanted = ui.data(|data| {
        data.get_temp(results_height_id(console))
            .unwrap_or(RESULTS_HEIGHT)
    });
    let results = wanted.clamp(total * MIN_SHARE, total * (1.0 - MIN_SHARE));
    (total - results, results)
}

fn results_height_id(console: &QueryConsole) -> Id {
    Id::new(("console_results_height", console.id.0))
}

/// The line between editor and results, dragged to resize them.
fn splitter(ui: &mut Ui, console: &QueryConsole) {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), SPLITTER_HEIGHT),
        egui::Sense::drag(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        let id = results_height_id(console);
        let height = ui.data(|data| data.get_temp(id).unwrap_or(RESULTS_HEIGHT));
        ui.data_mut(|data| data.insert_temp(id, height - response.drag_delta().y));
    }
    let stroke = if response.hovered() || response.dragged() {
        ui.visuals().widgets.hovered.fg_stroke
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    ui.painter().hline(rect.x_range(), rect.center().y, stroke);
}

/// Shortcuts act only while this console's editor has focus, and are consumed before the
/// editor sees them, so ⌘↵ runs instead of inserting a newline.
fn shortcuts(
    ui: &mut Ui,
    console: &mut QueryConsole,
    keymap: &Keymap,
    editor_id: Id,
    actions: &mut Vec<ConsoleAction>,
) {
    if !ui.memory(|memory| memory.has_focus(editor_id)) {
        return;
    }
    completion_popup::handle_keys(ui, console);
    snippet_keys(ui, console);
    for action in ui.input_mut(|input| keymap.consume(input, &EDITOR_ACTIONS)) {
        match action {
            Action::Complete => console.open_completion(true),
            Action::NextProblem => console.jump_to_problem(true),
            Action::PreviousProblem => console.jump_to_problem(false),
            Action::Find => console.open_find(false),
            Action::Replace => console.open_find(true),
            Action::FindNext => console.find_next(true),
            Action::FindPrevious => console.find_next(false),
            action => perform(console, action, actions),
        }
    }
}

/// Tab and ⇧Tab move through the fields of the snippet just inserted, and Esc leaves them.
/// The completion list, when it has something to pick, takes Tab first.
fn snippet_keys(ui: &mut Ui, console: &mut QueryConsole) {
    if !console.in_snippet() {
        return;
    }
    let pressed = |modifiers, key| ui.input_mut(|input| input.consume_key(modifiers, key));
    if pressed(egui::Modifiers::SHIFT, egui::Key::Tab) {
        console.next_snippet_field(false);
    } else if pressed(egui::Modifiers::NONE, egui::Key::Tab) {
        console.next_snippet_field(true);
    } else if pressed(egui::Modifiers::NONE, egui::Key::Escape) {
        console.leave_snippet();
    }
}

/// Does what a toolbar button or a shortcut asks, if it can be done now.
fn perform(console: &QueryConsole, action: Action, actions: &mut Vec<ConsoleAction>) {
    if !is_enabled(console, action) {
        return;
    }
    let scope = match action {
        Action::RunCurrent => RunScope::Current,
        Action::RunAll => RunScope::All,
        Action::Explain => RunScope::Explain(ExplainDepth::Estimated),
        Action::ExplainAnalyse => RunScope::Explain(ExplainDepth::Analysed),
        Action::Cancel => {
            actions.extend(console.cancellable_run().map(ConsoleAction::Cancel));
            return;
        }
        Action::ShowHistory => {
            actions.push(ConsoleAction::ShowHistory);
            return;
        }
        Action::Commit => {
            actions.push(ConsoleAction::EndTransaction(TransactionEnd::Commit));
            return;
        }
        Action::Rollback => {
            actions.push(ConsoleAction::EndTransaction(TransactionEnd::Rollback));
            return;
        }
        Action::Format => {
            actions.push(ConsoleAction::Format(FormatScope::Current));
            return;
        }
        Action::FormatAll => {
            actions.push(ConsoleAction::Format(FormatScope::All));
            return;
        }
        Action::RunToFile => {
            actions.push(ConsoleAction::RunToFile);
            return;
        }
        // The grid and the editor handle these themselves, and the app the files.
        Action::ViewCell
        | Action::EditCell
        | Action::CopyWithHeaders
        | Action::CopyAs(_)
        | Action::Complete
        | Action::NextProblem
        | Action::PreviousProblem
        | Action::OpenFile
        | Action::Settings
        | Action::Save
        | Action::SaveAs
        | Action::Find
        | Action::Replace
        | Action::FindNext
        | Action::FindPrevious => return,
    };
    let statements = console.statements_to_run(scope);
    let purpose = scope.purpose();
    if !statements.is_empty() {
        actions.push(ConsoleAction::Run(SubmittedRun {
            statements,
            purpose,
            parameters: dbrover_sql::ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
        }));
    }
}

/// Actions with no meaning outside SQL: hidden and disabled in a console whose language has no
/// SQL dialect.
fn needs_sql(action: Action) -> bool {
    matches!(action, Action::Format | Action::FormatAll)
}

/// Whether the console's language has `action` at all: SQL's formatting, and a plan to explain
/// in SQL and MongoDB. Hidden and disabled where it doesn't.
fn offered(console: &QueryConsole, action: Action) -> bool {
    match action {
        Action::Explain => console
            .language()
            .can_explain(ExplainDepth::Estimated, console.server()),
        Action::ExplainAnalyse => console
            .language()
            .can_explain(ExplainDepth::Analysed, console.server()),
        action => !needs_sql(action) || console.dialect().is_some(),
    }
}

fn is_enabled(console: &QueryConsole, action: Action) -> bool {
    if !offered(console, action) {
        return false;
    }
    match action {
        Action::RunCurrent
        | Action::RunAll
        | Action::Explain
        | Action::ExplainAnalyse
        | Action::RunToFile => !console.is_running(),
        Action::Cancel => console.cancellable_run().is_some(),
        Action::Commit | Action::Rollback => console.has_open_transaction(),
        Action::ShowHistory
        | Action::ViewCell
        | Action::EditCell
        | Action::CopyWithHeaders
        | Action::CopyAs(_)
        | Action::Complete
        | Action::Format
        | Action::FormatAll
        | Action::NextProblem
        | Action::PreviousProblem
        | Action::OpenFile
        | Action::Settings
        | Action::Save
        | Action::SaveAs
        | Action::Find
        | Action::Replace
        | Action::FindNext
        | Action::FindPrevious => true,
    }
}

/// A toolbar button for `action`, described with its shortcut, if it has one.
fn action_button(
    ui: &mut Ui,
    console: &QueryConsole,
    keymap: &Keymap,
    (label, action): (&str, Action),
    actions: &mut Vec<ConsoleAction>,
) {
    let hint = match keymap.hint(ui.ctx(), action) {
        Some(shortcut) => format!("{} ({shortcut})", action.description()),
        None => action.description().to_owned(),
    };
    let enabled = is_enabled(console, action);
    let response = match toolbar_icon(action) {
        Some((glyph, role)) => IconButton::new(glyph, label)
            .color(role.color(&Palette::for_visuals(ui.visuals())))
            .enabled(enabled)
            .tooltip(hint)
            .show(ui),
        None => ui
            .add_enabled(enabled, egui::Button::new(label))
            .on_hover_text(hint),
    };
    if response.clicked() {
        perform(console, action, actions);
    }
}

/// The icon a toolbar action is shown with. Commit and Rollback stay words: they end a
/// transaction, and an icon alone would be too easy to misread.
fn toolbar_icon(action: Action) -> Option<(&'static str, Role)> {
    match action {
        Action::RunCurrent => Some((icons::PLAY, Role::Success)),
        Action::RunAll => Some((icons::FAST_FORWARD, Role::Success)),
        Action::Explain => Some((icons::TREE_VIEW, Role::Secondary)),
        Action::Cancel => Some((icons::STOP, Role::Error)),
        Action::ShowHistory => Some((icons::CLOCK_COUNTER_CLOCKWISE, Role::Weak)),
        Action::Format => Some((icons::TEXT_INDENT, Role::Primary)),
        Action::Commit
        | Action::Rollback
        | Action::ExplainAnalyse
        | Action::RunToFile
        | Action::FormatAll
        | Action::NextProblem
        | Action::PreviousProblem
        | Action::ViewCell
        | Action::EditCell
        | Action::CopyWithHeaders
        | Action::CopyAs(_)
        | Action::Complete
        | Action::OpenFile
        | Action::Settings
        | Action::Save
        | Action::SaveAs
        | Action::Find
        | Action::Replace
        | Action::FindNext
        | Action::FindPrevious => None,
    }
}

fn toolbar(
    ui: &mut Ui,
    console: &mut QueryConsole,
    keymap: &Keymap,
    actions: &mut Vec<ConsoleAction>,
) {
    let palette = &Palette::for_visuals(ui.visuals());
    widgets::toolbar(ui, |ui| {
        for button in [
            ("Run", Action::RunCurrent),
            ("Run all", Action::RunAll),
            ("Explain", Action::Explain),
            ("Cancel", Action::Cancel),
            ("History", Action::ShowHistory),
            ("Format", Action::Format),
        ] {
            if !offered(console, button.1) {
                continue;
            }
            action_button(ui, console, keymap, button, actions);
        }
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        safety_badge(ui, console, palette);
        lost_connection(ui, console, palette, actions);
        run_status(ui, console, palette);
        problem_count(ui, console, keymap, palette);
        if let Some(error) = console.save_error() {
            ui.colored_label(palette.error, "Not saved")
                .on_hover_text(error);
        }
        if let Some(error) = &console.format_error {
            ui.colored_label(palette.error, "Not formatted")
                .on_hover_text(error);
        }
    });
}

/// What another program did to the console's `.sql` file, while that matters: changed under
/// unsaved edits (which to keep?), or deleted (Save writes it again).
fn file_bar(ui: &mut Ui, console: &mut QueryConsole) {
    let Some(disk) = console.disk() else {
        return;
    };
    let palette = &Palette::for_visuals(ui.visuals());
    let name = console.title().to_owned();
    match disk {
        Disk::Changed(_) => {
            ui.horizontal(|ui| {
                let text = format!("{name} changed on disk, and you have unsaved changes");
                ui.label(widgets::icon_text(
                    ui,
                    icons::WARNING,
                    palette.warning,
                    &text,
                ));
                if ui
                    .button("Reload")
                    .on_hover_text("Show what the file holds now, dropping your changes")
                    .clicked()
                {
                    console.reload_file();
                }
                if ui
                    .button("Keep mine")
                    .on_hover_text("Keep your text; saving it replaces the file")
                    .clicked()
                {
                    console.keep_mine();
                }
            });
        }
        Disk::Deleted => {
            let text = format!("{name} was deleted from disk; Save writes it again");
            ui.label(widgets::icon_text(
                ui,
                icons::WARNING,
                palette.warning,
                &text,
            ));
        }
        Disk::Unchanged => {}
    }
}

/// While the connection is lost: that it is, what went with it, and Reconnect. The console
/// never reconnects by itself, so nothing runs in a new session unawares.
fn lost_connection(
    ui: &mut Ui,
    console: &QueryConsole,
    palette: &Palette,
    actions: &mut Vec<ConsoleAction>,
) {
    let SessionState::Lost(reason) = &console.session else {
        return;
    };
    let job = widgets::icon_text(
        ui,
        icons::PLUGS,
        palette.warning,
        "Disconnected: its transaction, temporary tables and session settings are gone",
    );
    ui.label(job).on_hover_text(reason);
    let reconnect = widgets::icon_text(ui, icons::ARROWS_CLOCKWISE, palette.primary, "Reconnect");
    let response = widgets::name(
        ui.button(reconnect),
        egui::WidgetType::Button,
        "Reconnect",
        true,
    )
    .on_hover_text("Connect again, as a new session");
    if response.clicked() {
        actions.push(ConsoleAction::Reconnect);
    }
    ui.add_space(6.0);
}

/// The data source's safety level, unless it is normal, with what it means on hover.
fn safety_badge(ui: &mut Ui, console: &QueryConsole, palette: &Palette) {
    let safety = console.safety();
    let (glyph, color) = match safety {
        SafetyLevel::Normal => return,
        SafetyLevel::Careful => (icons::WARNING, palette.warning),
        SafetyLevel::ReadOnly => (icons::LOCK_SIMPLE, palette.primary),
    };
    let job = widgets::icon_text(ui, glyph, color, safety.display_name());
    ui.label(job).on_hover_text(format!(
        "\u{201c}{}\u{201d} is {}. {}",
        console.data_source_name(),
        safety.display_name().to_lowercase(),
        safety.description()
    ));
    ui.add_space(6.0);
}

/// How many errors and warnings the console has; a click goes to the next one.
fn problem_count(ui: &mut Ui, console: &mut QueryConsole, keymap: &Keymap, palette: &Palette) {
    let errors = console.problems.count(Severity::Error);
    let warnings = console.problems.count(Severity::Warning);
    let (glyph, color) = match (errors, warnings) {
        (0, 0) => return,
        (0, _) => (icons::WARNING, palette.warning),
        _ => (icons::X_CIRCLE, palette.error),
    };
    let text = problem_summary(errors, warnings);
    let hint = match keymap.hint(ui.ctx(), Action::NextProblem) {
        Some(shortcut) => format!("{} ({shortcut})", Action::NextProblem.description()),
        None => Action::NextProblem.description().to_owned(),
    };
    let job = widgets::icon_text(ui, glyph, color, &text);
    let response = ui
        .add(egui::Button::new(job).frame(false))
        .on_hover_text(hint);
    let response = widgets::name(response, egui::WidgetType::Button, &text, true);
    if response.clicked() {
        console.jump_to_problem(true);
    }
}

/// `1 error, 2 warnings`, leaving out a count of zero.
fn problem_summary(errors: usize, warnings: usize) -> String {
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    [
        count(errors, "error", "errors"),
        count(warnings, "warning", "warnings"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

/// The transaction mode, Commit and Rollback, and whether a transaction is pending. The
/// buttons show in manual mode, or whenever a transaction is open (one the user began with
/// their own `BEGIN`, say).
fn transaction_bar(
    ui: &mut Ui,
    console: &mut QueryConsole,
    keymap: &Keymap,
    actions: &mut Vec<ConsoleAction>,
) {
    ui.horizontal(|ui| {
        let open = console.has_open_transaction();
        ui.add_enabled_ui(!open, |ui| {
            egui::ComboBox::from_id_salt(("transaction_mode", console.id.0))
                .selected_text(mode_label(console.transactions))
                .show_ui(ui, |ui| {
                    for mode in [TransactionMode::Auto, TransactionMode::Manual] {
                        ui.selectable_value(&mut console.transactions, mode, mode_label(mode));
                    }
                })
        })
        .response
        .on_disabled_hover_text("Commit or roll back the open transaction first");
        if console.transactions == TransactionMode::Manual || open {
            for button in [("Commit", Action::Commit), ("Rollback", Action::Rollback)] {
                action_button(ui, console, keymap, button, actions);
            }
        }
        transaction_status(ui, console);
    });
}

fn mode_label(mode: TransactionMode) -> &'static str {
    match mode {
        TransactionMode::Auto => "Auto-commit",
        TransactionMode::Manual => "Manual commit",
    }
}

fn transaction_status(ui: &mut Ui, console: &QueryConsole) {
    let palette = &Palette::for_visuals(ui.visuals());
    match (console.transaction, &console.transaction_ended) {
        (TransactionStatus::InTransaction, _) => {
            ui.colored_label(palette.warning, "● Transaction open")
                .on_hover_text("Changes stay pending until you commit or roll back");
        }
        (TransactionStatus::Failed, _) => {
            ui.colored_label(palette.error, "● Transaction failed")
                .on_hover_text(
                    "A statement failed, so the database only accepts Rollback now \
                     (Commit rolls back too)",
                );
        }
        (TransactionStatus::Idle, Some(Ok(TransactionEnd::Commit))) => {
            ui.colored_label(palette.success, "Committed");
        }
        (TransactionStatus::Idle, Some(Ok(TransactionEnd::Rollback))) => {
            ui.weak("Rolled back");
        }
        (TransactionStatus::Idle, Some(Err(_)) | None) => {}
    }
    // A failed commit can leave the transaction open (SQLite's "database is locked", say),
    // so the error shows whatever the status.
    if let Some(Err(error)) = &console.transaction_ended {
        ui.colored_label(palette.error, error);
    }
}

fn run_status(ui: &mut Ui, console: &QueryConsole, palette: &Palette) {
    match (&console.session, console.run.as_ref().map(|run| run.state)) {
        (SessionState::Connecting, _) => {
            ui.spinner();
            ui.weak("Connecting…");
        }
        (_, Some(RunState::Running)) => {
            ui.spinner();
            ui.weak("Running…");
        }
        (_, Some(RunState::Finished { outcome, elapsed })) => {
            let millis = elapsed.as_millis();
            let (glyph, color, text) = match outcome {
                RunOutcome::Completed => (
                    icons::CHECK_CIRCLE,
                    palette.success,
                    format!("Done in {millis} ms"),
                ),
                RunOutcome::Failed => (icons::X_CIRCLE, palette.error, "Failed".to_owned()),
                RunOutcome::Cancelled => (icons::PROHIBIT, palette.warning, "Cancelled".to_owned()),
            };
            widgets::status(ui, glyph, color, &text);
        }
        (SessionState::Connected(server), None) => {
            ui.weak(format!("Connected to {server}"));
        }
        (_, None) => {}
    }
}

/// Applies a cursor move or selection requested from outside the editor. A cursor move also
/// focuses the editor; a selection (a match the find bar went to) leaves the focus where it
/// is, and is returned so the editor can scroll to it. After the text was replaced (a format,
/// a reload from its file, a replacement), also records the text before and after as undo
/// points, so one undo restores it; egui would otherwise only record a point once the text
/// had stayed the same for a second.
fn move_cursor(ui: &Ui, console: &mut QueryConsole, editor_id: Id) -> Option<CCursorRange> {
    let focus = console.pending_cursor.is_some();
    let range = match (
        console.pending_cursor.take(),
        console.pending_selection.take(),
    ) {
        (_, Some(range)) => range,
        (Some(offset), None) => offset..offset,
        (None, None) => return None,
    };
    let char_at = |offset: usize| {
        CCursor::new(
            console
                .text
                .get(..offset)
                .unwrap_or(&console.text)
                .chars()
                .count(),
        )
    };
    let cursor = CCursorRange::two(char_at(range.start), char_at(range.end));
    let mut state = TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
    if let Some(before) = console.replaced_text.take() {
        let mut undoer = state.undoer();
        let cursor_before = state.cursor.char_range().unwrap_or(cursor);
        undoer.add_undo(&(cursor_before, before));
        undoer.add_undo(&(cursor, console.text.clone()));
        state.set_undoer(undoer);
    }
    state.cursor.set_char_range(Some(cursor));
    state.store(ui.ctx(), editor_id);
    if focus {
        ui.memory_mut(|memory| memory.request_focus(editor_id));
        return None;
    }
    Some(cursor)
}

fn editor(
    ui: &mut Ui,
    console: &mut QueryConsole,
    editor_id: Id,
    found: &Found,
    context: &ConsoleContext<'_>,
    actions: &mut Vec<ConsoleAction>,
) {
    let catalog = context.catalog.as_ref();
    let reveal = move_cursor(ui, console, editor_id);
    let checked = console.check_problems(catalog, context.lint, Instant::now());
    if let Some(wait) = checked.repaint_after {
        crate::repaint::request_repaint_after_pause(ui.ctx(), wait);
    }
    if !checked.ask_database.is_empty() {
        actions.push(ConsoleAction::Validate(checked.ask_database));
    }
    let marks = Marks {
        error: console.error_marker.clone(),
        current_statement: console.current_statement(),
        current_match: found.current.map(|index| found.matches[index].clone()),
        matches: found.matches.clone(),
    };
    let mut layouter = highlight::layouter(console.language(), &marks);
    let gutter_width = gutter_width(ui, &console.text);
    ScrollArea::vertical()
        .id_salt(("console_editor_scroll", console.id.0))
        .auto_shrink(false)
        .show(ui, |ui| {
            let output = TextEdit::multiline(&mut console.text)
                .id(editor_id)
                .code_editor()
                .hint_text("SELECT …")
                // Fills the height given by `min_size`; one row keeps it from overflowing
                // that height when the console is short.
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .min_size(vec2(0.0, ui.available_height()))
                .margin(Margin {
                    left: gutter_width,
                    ..TEXT_MARGIN
                })
                .layouter(&mut layouter)
                .show(ui);
            if let Some(cursor) = reveal {
                let rect = output.galley.pos_from_cursor(cursor.primary);
                let rect = rect.translate(output.galley_pos.to_vec2());
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            line_numbers(ui, &output);
            squiggles(ui, console, &output);
            problem_tooltip(ui, console, &output);
            if output.response.changed() {
                console.edited(Instant::now());
            }
            if let Some(cursor) = output.cursor_range {
                console.selection = byte_range(&console.text, &cursor);
            }
            console.follow_snippet(output.response.changed(), output.response.clicked());
            completion_popup::follow_editor(ui, console, &output);
            let missing = console.refresh_completion(catalog, context.recent, context.snippets);
            if !missing.is_empty() {
                actions.push(ConsoleAction::LoadCatalog(missing));
            }
            completion_popup::show(ui, console, &output);
        });
}

/// How far a squiggle's wave rises and falls, and the length of one rise or fall.
const SQUIGGLE_AMPLITUDE: f32 = 1.5;
const SQUIGGLE_STEP: f32 = 2.5;

/// Underlines each problem with a wave, red for errors and amber for warnings, and remembers
/// where, so hovering can show the message.
fn squiggles(ui: &Ui, console: &mut QueryConsole, output: &egui::text_edit::TextEditOutput) {
    let palette = &Palette::for_visuals(ui.visuals());
    let origin = output.galley_pos;
    let mut rects = Vec::new();
    for (index, problem) in console.problems.shown().iter().enumerate() {
        let color = match problem.severity {
            Severity::Error => palette.error,
            Severity::Warning => palette.warning,
        };
        let start = char_index(&console.text, problem.range.start);
        let end = char_index(&console.text, problem.range.end).max(start + 1);
        let mut row_start = 0;
        for row in &output.galley.rows {
            let row_end = row_start + row.char_count_including_newline().0;
            let (from, to) = (start.max(row_start), end.min(row_end));
            if from < to {
                let column =
                    |char: usize| egui::text::CharIndex((char - row_start).min(row.glyphs.len()));
                let left = origin.x + row.pos.x + row.x_offset(column(from));
                let right = origin.x + row.pos.x + row.x_offset(column(to));
                let rect = egui::Rect::from_min_max(
                    egui::pos2(left, origin.y + row.min_y()),
                    egui::pos2(
                        right.max(left + 2.0 * SQUIGGLE_STEP),
                        origin.y + row.max_y(),
                    ),
                );
                ui.painter().add(wave(rect, color));
                rects.push((rect, index));
            }
            row_start = row_end;
        }
    }
    console.problem_rects = rects;
}

fn wave(rect: egui::Rect, color: egui::Color32) -> egui::Shape {
    let baseline = rect.bottom() - SQUIGGLE_AMPLITUDE;
    let mut points = Vec::new();
    let mut x = rect.left();
    let mut up = true;
    while x <= rect.right() {
        let offset = if up {
            -SQUIGGLE_AMPLITUDE
        } else {
            SQUIGGLE_AMPLITUDE
        };
        points.push(egui::pos2(x, baseline + offset));
        x += SQUIGGLE_STEP;
        up = !up;
    }
    egui::Shape::line(points, egui::Stroke::new(1.0, color))
}

/// Shows the message of the problem under the pointer, and how to switch off a lint rule.
fn problem_tooltip(ui: &Ui, console: &QueryConsole, output: &egui::text_edit::TextEditOutput) {
    if !output.response.hovered() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    let Some(problem) = console
        .problem_rects
        .iter()
        .find(|(rect, _)| rect.contains(pointer))
        .and_then(|(_, index)| console.problems.shown().get(*index))
    else {
        return;
    };
    output
        .response
        .response
        .clone()
        .on_hover_ui_at_pointer(|ui| {
            ui.label(&problem.message);
            if let ProblemSource::Lint(rule) = problem.source {
                ui.weak(format!(
                    "{} · can be switched off in View → Settings",
                    rule.id()
                ));
            }
        });
}

/// The character index of byte `offset`, as the editor's galley counts.
fn char_index(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).chars().count()
}

/// The editor's own margin around the text; the line numbers widen its left side.
const TEXT_MARGIN: Margin = Margin::symmetric(4, 2);
/// Space between the line numbers and the text.
const GUTTER_GAP: f32 = 12.0;

/// The editor's left margin: room for the largest line number, so the text doesn't shift
/// until a line count needs another digit.
fn gutter_width(ui: &Ui, text: &str) -> i8 {
    let lines = text.lines().count().max(1) + usize::from(text.ends_with('\n'));
    let digits = lines.to_string().len();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let digit_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
    let width = f32::from(TEXT_MARGIN.left) + digits as f32 * digit_width + GUTTER_GAP;
    // Margins are whole points in an `i8`; 127 points hold over a dozen digits.
    width.ceil().min(f32::from(i8::MAX)) as i8
}

/// Numbers each line of the editor's text in the gutter. A line that wraps is numbered on
/// its first row only.
fn line_numbers(ui: &Ui, output: &egui::text_edit::TextEditOutput) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().weak_text_color();
    let right_edge = output.galley_pos.x - GUTTER_GAP;
    let rows = &output.galley.rows;
    let ends: Vec<bool> = rows.iter().map(|row| row.ends_with_newline).collect();
    for (line, row_index) in line_starts(&ends).into_iter().enumerate() {
        let y = output.galley_pos.y + rows[row_index].rect().center().y;
        ui.painter().text(
            egui::pos2(right_edge, y),
            egui::Align2::RIGHT_CENTER,
            (line + 1).to_string(),
            font.clone(),
            color,
        );
    }
}

/// The indices of the rows that start a line, given whether each row ends with a line break
/// (rather than wrapping onto the next).
fn line_starts(ends_with_newline: &[bool]) -> Vec<usize> {
    (0..ends_with_newline.len())
        .filter(|&row| row == 0 || ends_with_newline[row - 1])
        .collect()
}

/// Converts the editor's character-index cursor into byte offsets into `text`.
fn byte_range(text: &str, cursor: &CCursorRange) -> std::ops::Range<usize> {
    let byte = |char_index: usize| {
        text.char_indices()
            .nth(char_index)
            .map_or(text.len(), |(byte, _)| byte)
    };
    byte(cursor.secondary.index.0)..byte(cursor.primary.index.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disables_what_only_sql_has_in_a_console_of_another_language() {
        let console_in = |language| {
            let key = dbrover_store::ConsoleKey {
                data_source: dbrover_core::DataSourceId::generate(),
                name: dbrover_store::ConsoleName::numbered(1),
            };
            QueryConsole::new(
                dbrover_engine::ConsoleId(1),
                key,
                "test".to_owned(),
                language,
                String::new(),
            )
        };
        let sql = console_in(dbrover_language::QueryLanguage::Sql(Dialect::Postgres));
        let test = console_in(dbrover_language::QueryLanguage::Test);
        for action in [Action::Explain, Action::Format, Action::FormatAll] {
            assert!(is_enabled(&sql, action), "{action:?}");
            assert!(!is_enabled(&test, action), "{action:?}");
        }
        assert!(is_enabled(&test, Action::RunCurrent));
    }

    #[test]
    fn numbers_only_the_first_row_of_a_wrapped_line() {
        // Line 1 wraps onto a second row; lines 2 and 3 fit on one row each.
        assert_eq!(line_starts(&[false, true, true, false]), [0, 2, 3]);
        assert_eq!(line_starts(&[false]), [0]);
        assert!(line_starts(&[]).is_empty());
    }
}
