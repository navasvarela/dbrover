//! View → Actions…: every custom action from `actions.toml`, what the file got wrong, and
//! buttons to create and reload it. See `docs/hackability.md`.

use egui::{Grid, RichText, ScrollArea, Window};

use crate::command_runs::{CommandRuns, command_text, timeout_text};
use crate::custom_files::{self, CustomFiles};
use crate::keymap::Keymap;
use dbrover_store::{ActionBody, CommandAction, CustomAction, CustomFileKind};

const DEFAULT_SIZE: [f32; 2] = [760.0, 420.0];

pub enum ActionsWindowAction {
    Reload,
    /// Write a starting `actions.toml`, if there is none.
    Create,
    Close,
}

pub fn show(
    ctx: &egui::Context,
    files: &CustomFiles,
    keymap: &Keymap,
    runs: &CommandRuns,
) -> Option<ActionsWindowAction> {
    let mut open = true;
    let action = Window::new("Actions")
        .id(egui::Id::new("actions_window"))
        .open(&mut open)
        .default_size(DEFAULT_SIZE)
        .show(ctx, |ui| contents(ui, files, keymap, runs))
        .and_then(|response| response.inner.flatten());
    if open {
        action
    } else {
        Some(ActionsWindowAction::Close)
    }
}

fn contents(
    ui: &mut egui::Ui,
    files: &CustomFiles,
    keymap: &Keymap,
    runs: &CommandRuns,
) -> Option<ActionsWindowAction> {
    let kind = CustomFileKind::Actions;
    let mut action = custom_files::header(ui, files.path(kind), &files.summary(kind))
        .then_some(ActionsWindowAction::Reload);
    if ui
        .button("Create actions.toml")
        .on_hover_text("Write a file with commented examples, to edit. An existing file is kept.")
        .clicked()
    {
        action = Some(ActionsWindowAction::Create);
    }
    custom_files::problems(ui, files.problems(kind));
    ui.separator();
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        if files.actions().is_empty() {
            ui.weak("No actions yet. Add [[action]] tables to actions.toml, then Reload.");
            return;
        }
        Grid::new("custom_actions")
            .striped(true)
            .num_columns(6)
            .show(ui, |ui| {
                for heading in ["Action", "Id", "On", "Drivers", "Keys", "Runs"] {
                    ui.strong(heading);
                }
                ui.end_row();
                for custom in files.actions() {
                    row(ui, custom, keymap, runs);
                }
            });
    });
    action
}

fn row(ui: &mut egui::Ui, action: &CustomAction, keymap: &Keymap, runs: &CommandRuns) {
    ui.label(&action.name);
    ui.label(RichText::new(action.id.keymap_id()).monospace());
    let targets: Vec<&str> = action
        .applies_to
        .iter()
        .map(|target| target.name())
        .collect();
    ui.label(targets.join(", "));
    match &action.drivers {
        Some(drivers) => {
            let names: Vec<&str> = drivers.iter().map(|driver| driver.display_name()).collect();
            ui.label(names.join(", "));
        }
        None => {
            ui.weak("all");
        }
    }
    let keys: Vec<String> = keymap
        .custom_bindings(&action.id)
        .iter()
        .map(|shortcut| ui.ctx().format_shortcut(shortcut))
        .collect();
    if keys.is_empty() {
        ui.weak("unbound");
    } else {
        ui.label(keys.join(", "));
    }
    match &action.body {
        ActionBody::Sql { sql, run } => {
            let sql = ui.label(RichText::new(sql.source()).monospace());
            if !run {
                sql.on_hover_text("Opens in a console to edit before running");
            }
        }
        ActionBody::Command(command) => {
            let trusted = runs.is_trusted(&action.id, command);
            ui.vertical(|ui| command_cell(ui, command, trusted));
        }
    }
    ui.end_row();
}

/// The command in full, what goes in and out of it, and whether it runs without asking.
fn command_cell(ui: &mut egui::Ui, command: &CommandAction, trusted: bool) {
    ui.label(RichText::new(command_text(command.command.source())).monospace());
    ui.weak(format!(
        "stdin {} · output {} · timeout {}",
        command.stdin.name(),
        command.output.name(),
        timeout_text(command.timeout)
    ));
    if trusted {
        ui.weak("Trusted: runs without asking");
    } else {
        ui.weak("Asks before it next runs");
    }
}

/// A starting `actions.toml`: the plan's examples, commented out, so the file documents the
/// format and adds nothing until a block is uncommented.
pub const TEMPLATE: &str = r#"# DbRover custom actions. Uncomment and change an action, then View → Actions… → Reload.
#
# applies_to: node kinds (database, schema, table, view, materialized_view, function, procedure,
#   sequence, column, index, constraint, trigger, role, …), data_source, grid.selection (once for
#   the selected cells) or grid.row (once per selected row).
# drivers: postgres, sqlite, mysql, sqlserver, duckdb, clickhouse, redis, mongodb, elasticsearch;
#   every driver when left out.
# run = false opens the SQL in a console to edit first.
#
# Placeholders, never pasted in as text: names are quoted, values become literals.
#   {qualified_name} {database} {schema} {table} {column} {column:default}
#   {data_source} (a string)  {value} {values} {row.<column>} (from the grid)
# Write {{ for a { that would otherwise start a placeholder.
#
# An action can run a program instead of SQL, with command = [program, arguments…]. There is no
# shell: each placeholder fills one argument, as raw text. Commands also take the data source's
# {host} {port} {user} {file}, never a password. With a command:
#   stdin = "json", "csv" or "tsv" sends the grid's selected cells (default "none");
#   output = "viewer", "console", "log" or "none" says where stdout goes (default "viewer");
#   timeout = "30s" stops it after that long (default 60s).
# DbRover shows each command in full and asks before its first run.
#
# Bind an action to a key in keymap.toml as "custom.<id>" = "cmd+shift+y".

# [[action]]
# id = "row-count"
# name = "Row count"
# applies_to = ["table", "view"]
# sql = "SELECT count(*) FROM {qualified_name}"

# [[action]]
# id = "latest"
# name = "Last 100 rows"
# applies_to = ["table"]
# sql = "SELECT * FROM {qualified_name} ORDER BY {column:created_at} DESC LIMIT 100"
# run = false

# [[action]]
# id = "rows-to-jq"
# name = "Send rows to jq"
# applies_to = ["grid.selection"]
# command = ["jq", "-r", ".[] | .email"]
# stdin = "json"
# output = "viewer"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_adds_nothing_until_uncommented_and_then_reads_cleanly() {
        let as_written = dbrover_store::parse_actions(TEMPLATE);
        assert!(as_written.value.is_empty() && as_written.problems.is_empty());

        let uncommented: String = TEMPLATE
            .lines()
            .skip_while(|line| *line != "# [[action]]")
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        let checked = dbrover_store::parse_actions(&uncommented);
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        assert_eq!(checked.value.len(), 3);
    }
}
