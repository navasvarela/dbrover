//! Exports of a whole table and CSV imports, once their dialog has closed: the status bar shows
//! each one running, with **Cancel** where it can be cancelled, and how the last one ended, as
//! it does for programs. A long export no longer holds the app behind its dialog.

use std::path::Path;

use dbrover_core::DataSourceId;
use dbrover_engine::{Command, Event, Exporter, ImportFailure, RunId};
use egui::RichText;

use crate::theme::Palette;

#[derive(Default)]
pub struct BackgroundRuns {
    running: Vec<Running>,
    /// How the last run ended, until dismissed.
    notice: Option<Notice>,
}

/// A run under way, and the rows it has written so far.
struct Running {
    work: Work,
    rows: u64,
}

/// What a run does. The engine reports progress by data source, so each data source runs one
/// of each at a time, which the dialogs already ensure.
enum Work {
    TableExport {
        data_source: DataSourceId,
        run: RunId,
        /// The table's name, for the status bar.
        table: String,
    },
    CsvImport {
        data_source: DataSourceId,
        /// `main.orders`, as the dialog named it.
        table: String,
    },
}

struct Notice {
    text: String,
    failed: bool,
}

impl BackgroundRuns {
    pub fn table_export(&mut self, data_source: DataSourceId, run: RunId, table: String) {
        self.start(Work::TableExport {
            data_source,
            run,
            table,
        });
    }

    pub fn csv_import(&mut self, data_source: DataSourceId, table: String) {
        self.start(Work::CsvImport { data_source, table });
    }

    fn start(&mut self, work: Work) {
        self.running.push(Running { work, rows: 0 });
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::ExportProgress {
                exporter: Exporter::DataSource(id),
                rows,
            } => self.progress(|work| work.exports_from(*id), *rows as u64),
            Event::ImportProgress { data_source, rows } => {
                self.progress(|work| work.imports_into(*data_source), *rows);
            }
            Event::ExportFinished {
                exporter: Exporter::DataSource(id),
                path,
                result,
            } => {
                if let Some(work) = self.finish(|work| work.exports_from(*id)) {
                    self.notice = Some(work.exported(path, result));
                }
            }
            Event::RowsImported {
                data_source,
                result,
            } => {
                if let Some(work) = self.finish(|work| work.imports_into(*data_source)) {
                    self.notice = Some(work.imported(result));
                }
            }
            _ => {}
        }
    }

    fn progress(&mut self, ours: impl Fn(&Work) -> bool, rows: u64) {
        if let Some(running) = self.running.iter_mut().find(|running| ours(&running.work)) {
            running.rows = rows;
        }
    }

    /// Takes the run `ours` picks out of the running ones.
    fn finish(&mut self, ours: impl Fn(&Work) -> bool) -> Option<Work> {
        let index = self
            .running
            .iter()
            .position(|running| ours(&running.work))?;
        Some(self.running.remove(index).work)
    }

    /// The status bar's part: each run with how far it has got, and **Cancel** for an
    /// export, then how the last one ended, which a click dismisses. Returns the cancels
    /// asked for.
    pub fn status(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Vec<Command> {
        let mut cancels = Vec::new();
        for running in &self.running {
            ui.label(
                RichText::new(running.work.describe(running.rows))
                    .small()
                    .color(palette.text_weak),
            );
            if let Some(cancel) = running.work.cancel()
                && ui.link(RichText::new("Cancel").small()).clicked()
            {
                cancels.push(cancel);
            }
        }
        if let Some(notice) = &self.notice {
            let color = if notice.failed {
                palette.error
            } else {
                palette.text_weak
            };
            if ui
                .link(RichText::new(&notice.text).small().color(color))
                .on_hover_text("Dismiss")
                .clicked()
            {
                self.notice = None;
            }
        }
        cancels
    }
}

impl Work {
    fn exports_from(&self, id: DataSourceId) -> bool {
        matches!(self, Self::TableExport { data_source, .. } if *data_source == id)
    }

    fn imports_into(&self, id: DataSourceId) -> bool {
        matches!(self, Self::CsvImport { data_source, .. } if *data_source == id)
    }

    fn table(&self) -> &str {
        match self {
            Self::TableExport { table, .. } | Self::CsvImport { table, .. } => table,
        }
    }

    fn describe(&self, rows: u64) -> String {
        let table = self.table();
        match self {
            Self::TableExport { .. } => format!("Exporting {table}… {rows} rows"),
            Self::CsvImport { .. } => format!("Importing into {table}… {rows} rows"),
        }
    }

    /// What stops the run. An import runs in one transaction, which the database rolls back
    /// if the run fails, and the engine has no way to stop it part way.
    fn cancel(&self) -> Option<Command> {
        match self {
            Self::TableExport {
                data_source, run, ..
            } => Some(Command::CancelTableExport {
                data_source: *data_source,
                run: *run,
            }),
            Self::CsvImport { .. } => None,
        }
    }

    fn exported(&self, path: &Path, result: &Result<usize, String>) -> Notice {
        match result {
            Ok(rows) => Notice {
                text: format!("Exported {rows} rows to {}", path.display()),
                failed: false,
            },
            Err(error) => Notice {
                text: format!("Exporting {} failed: {error}", self.table()),
                failed: true,
            },
        }
    }

    fn imported(&self, result: &Result<u64, ImportFailure>) -> Notice {
        let table = self.table();
        match result {
            Ok(rows) => Notice {
                text: format!("Imported {rows} rows into {table}"),
                failed: false,
            },
            Err(failure) => {
                let place = match failure.lines {
                    Some((first, last)) if first == last => format!("Line {first}: "),
                    Some((first, last)) => format!("Lines {first}\u{2013}{last}: "),
                    None => String::new(),
                };
                Notice {
                    text: format!(
                        "{place}{}. Nothing was imported into {table}.",
                        failure.message
                    ),
                    failed: true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn shows_an_export_s_progress_cancels_it_and_says_how_it_ended() {
        let data_source = DataSourceId::generate();
        let run = RunId(7);
        let runs = Rc::new(RefCell::new(BackgroundRuns::default()));
        runs.borrow_mut()
            .table_export(data_source, run, "orders".to_owned());
        let cancels = Rc::new(RefCell::new(Vec::new()));
        let mut harness = crate::test_support::ui_harness({
            let (runs, cancels) = (Rc::clone(&runs), Rc::clone(&cancels));
            move |ui| {
                let palette = Palette::for_visuals(ui.visuals());
                let asked = runs.borrow_mut().status(ui, &palette);
                cancels.borrow_mut().extend(asked);
            }
        });

        runs.borrow_mut().apply(&Event::ExportProgress {
            exporter: Exporter::DataSource(data_source),
            rows: 1200,
        });
        harness.run();
        harness.get_by_label("Exporting orders… 1200 rows");
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(matches!(
            cancels.borrow().as_slice(),
            [Command::CancelTableExport { run: cancelled, .. }] if *cancelled == run
        ));

        runs.borrow_mut().apply(&Event::ExportFinished {
            exporter: Exporter::DataSource(data_source),
            path: "orders.csv".into(),
            result: Ok(1200),
        });
        harness.run();
        assert!(
            harness
                .query_by_label_contains("Exporting orders")
                .is_none()
        );
        harness
            .get_by_label("Exported 1200 rows to orders.csv")
            .click();
        harness.run();
        assert!(harness.query_by_label_contains("Exported").is_none());
    }
}
