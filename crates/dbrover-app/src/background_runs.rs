//! Exports of a whole table, CSV imports, dumps and restores (W17), once their dialog has
//! closed: the status bar shows each one running, with **Cancel** where it can be cancelled,
//! and how the last one ended, as it does for programs. A long export no longer holds the app
//! behind its dialog.

use std::path::Path;

use dbrover_core::{ByteSize, DataSourceId};
use dbrover_engine::{
    Command, DumpDone, DumpFailure, DumpProgress, DumpRunId, Event, Exporter, ImportFailure,
    RestoreDone, RunId,
};
use egui::RichText;

use crate::theme::Palette;

#[derive(Default)]
pub struct BackgroundRuns {
    running: Vec<Running>,
    /// How the last run ended, until dismissed.
    notice: Option<Notice>,
}

/// A run under way, and the rows it has written so far, or for a dump or restore its tables
/// and bytes.
struct Running {
    work: Work,
    rows: u64,
    dumped: DumpProgress,
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
    Dump {
        run: DumpRunId,
        /// *shop from production*, for the status bar.
        what: String,
    },
    Restore {
        run: DumpRunId,
        /// *shop.dump into production*.
        what: String,
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

    pub fn dump(&mut self, run: DumpRunId, what: String) {
        self.start(Work::Dump { run, what });
    }

    pub fn restore(&mut self, run: DumpRunId, what: String) {
        self.start(Work::Restore { run, what });
    }

    fn start(&mut self, work: Work) {
        self.running.push(Running {
            work,
            rows: 0,
            dumped: DumpProgress::default(),
        });
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
            Event::SavedDumpStarted { run, name } => self.dump(*run, name.clone()),
            Event::DumpProgress { run, progress } => {
                if let Some(running) = self.running.iter_mut().find(|r| r.work.is_run(*run)) {
                    running.dumped = progress.clone();
                }
            }
            Event::DumpEnded { run, result } => {
                if let Some(work) = self.finish(|work| work.is_run(*run)) {
                    self.notice = Some(work.dumped(result));
                }
            }
            Event::RestoreEnded { run, result } => {
                if let Some(work) = self.finish(|work| work.is_run(*run)) {
                    self.notice = Some(work.restored(result));
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
                RichText::new(running.work.describe(running.rows, &running.dumped))
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

    fn is_run(&self, id: DumpRunId) -> bool {
        matches!(self, Self::Dump { run, .. } | Self::Restore { run, .. } if *run == id)
    }

    fn table(&self) -> &str {
        match self {
            Self::TableExport { table, .. } | Self::CsvImport { table, .. } => table,
            Self::Dump { what, .. } | Self::Restore { what, .. } => what,
        }
    }

    fn describe(&self, rows: u64, dumped: &DumpProgress) -> String {
        let table = self.table();
        match self {
            Self::TableExport { .. } => format!("Exporting {table}… {rows} rows"),
            Self::CsvImport { .. } => format!("Importing into {table}… {rows} rows"),
            Self::Dump { .. } => format!("Dumping {table}… {}", dump_progress(dumped)),
            Self::Restore { .. } => format!("Restoring {table}… {}", dump_progress(dumped)),
        }
    }

    fn dumped(&self, result: &Result<DumpDone, DumpFailure>) -> Notice {
        match result {
            Ok(done) => Notice {
                text: format!(
                    "Dumped {} to {}, {}",
                    self.table(),
                    done.file.display(),
                    ByteSize(done.bytes)
                ),
                failed: false,
            },
            Err(DumpFailure::Cancelled) => Notice {
                text: format!(
                    "Cancelled the dump of {}; nothing was written",
                    self.table()
                ),
                failed: false,
            },
            Err(failure) => Notice {
                text: format!("Dumping {} failed: {failure}", self.table()),
                failed: true,
            },
        }
    }

    fn restored(&self, result: &Result<RestoreDone, DumpFailure>) -> Notice {
        match result {
            Ok(done) => Notice {
                text: format!("Restored {} ({})", self.table(), done.database),
                failed: false,
            },
            Err(DumpFailure::Cancelled) => Notice {
                text: format!("Cancelled the restore of {}", self.table()),
                failed: true,
            },
            Err(failure) => Notice {
                text: format!("Restoring {} failed: {failure}", self.table()),
                failed: true,
            },
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
            Self::Dump { run, .. } | Self::Restore { run, .. } => Some(Command::CancelDump(*run)),
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

/// *12 tables, 142 MB*: what the program has got through.
fn dump_progress(progress: &DumpProgress) -> String {
    let tables = match progress.tables {
        1 => "1 table".to_owned(),
        count => format!("{count} tables"),
    };
    format!("{tables}, {}", ByteSize(progress.bytes))
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

    #[test]
    fn shows_a_dumps_tables_and_bytes_and_cancels_it() {
        let runs = Rc::new(RefCell::new(BackgroundRuns::default()));
        runs.borrow_mut()
            .dump(DumpRunId(3), "shop from production".to_owned());
        let cancels = Rc::new(RefCell::new(Vec::new()));
        let mut harness = crate::test_support::ui_harness({
            let (runs, cancels) = (Rc::clone(&runs), Rc::clone(&cancels));
            move |ui| {
                let palette = Palette::for_visuals(ui.visuals());
                let asked = runs.borrow_mut().status(ui, &palette);
                cancels.borrow_mut().extend(asked);
            }
        });
        runs.borrow_mut().apply(&Event::DumpProgress {
            run: DumpRunId(3),
            progress: DumpProgress {
                tables: 12,
                table: Some("public.orders".to_owned()),
                bytes: 142_000_000,
            },
        });
        harness.run();
        harness.get_by_label_contains("Dumping shop from production… 12 tables");
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(matches!(
            cancels.borrow().as_slice(),
            [Command::CancelDump(DumpRunId(3))]
        ));
        runs.borrow_mut().apply(&Event::DumpEnded {
            run: DumpRunId(3),
            result: Err(DumpFailure::Cancelled),
        });
        harness.run();
        harness.get_by_label("Cancelled the dump of shop from production; nothing was written");
    }
}
