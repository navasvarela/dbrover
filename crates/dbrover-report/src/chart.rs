//! A report's charts in a run (`docs/charts-as-code.md`, CH3d): each reads the rows beside the
//! outputs, a page at a time, keeping no more than it can draw, and once the last row is in is
//! drawn as SVG, or PNG from it, into the staging folder, to be put in place with the outputs'
//! files.
//!
//! A chart is unattended, so where the tab draws the first rows and says so, a chart past its
//! kind's rows or its colours fails the run, unless `limit` says to draw the first ones.

use std::time::{Duration, Instant};

use dbrover_chart::{
    ChartData, ChartKind, ChartSpec, Choice, Column, ColumnProblem, FILE_TEXT_SIZE, LayoutOptions,
    Resolved, Role, STARTING_SERIES, Size, arrange, layout_file,
};
use dbrover_core::Row;
use dbrover_export::{CompletedFile, ExportColumn};
use dbrover_picture::{FontMeasure, Fonts};

use crate::events::{CHART_ROWS_KEY, CHART_SERIES_KEY};
use crate::run::{Completed, RunFolders, describe};
use crate::{Chart, ChartPicture, LimitHit, RunFailure, Step, grouped};

/// What a chart's warning says of a result with no rows.
const NO_ROWS: &str = "the result has no rows, so the chart says \"No rows to chart\"";

/// A chart being read and drawn.
pub(crate) struct ChartRun<'a> {
    number: usize,
    chart: &'a Chart,
    /// Its file's name, with the run's values in it.
    file: String,
    columns: Vec<Column>,
    /// The spec resolved against the columns, and their roles, once the first rows came.
    resolved: Option<(Vec<Role>, Resolved)>,
    /// The rows it may draw: the first ones, or with `sort` and `limit` the top ones so far.
    rows: Vec<Row>,
    /// Every row it was given.
    seen: u64,
    /// The time spent keeping rows and drawing.
    busy: Duration,
}

impl<'a> ChartRun<'a> {
    pub(crate) fn new(
        number: usize,
        chart: &'a Chart,
        file: &str,
        columns: &[ExportColumn],
    ) -> Self {
        Self {
            number,
            chart,
            file: file.to_owned(),
            columns: columns
                .iter()
                .map(|column| Column {
                    name: column.name.clone(),
                    kind: column.kind,
                })
                .collect(),
            resolved: None,
            rows: Vec::new(),
            seen: 0,
            busy: Duration::ZERO,
        }
    }

    /// Its place among the report's charts, counted from 1.
    pub(crate) fn number(&self) -> usize {
        self.number
    }

    pub(crate) fn step(&self) -> Step {
        Step::Chart {
            number: self.number,
            file: self.file.clone(),
        }
    }

    fn spec(&self) -> &ChartSpec {
        &self.chart.spec
    }

    /// Takes a page of the rows: the first page resolves the spec, whose columns' roles a
    /// column without a declared type takes from it.
    ///
    /// # Errors
    ///
    /// A [`RunFailure`] in the chart's step when the spec names a column the rows don't have,
    /// or one that can't be where it puts it.
    pub(crate) fn add(&mut self, page: &[Row]) -> Result<(), RunFailure> {
        let started = Instant::now();
        let added = self.keep(page);
        self.busy += started.elapsed();
        added
    }

    fn keep(&mut self, page: &[Row]) -> Result<(), RunFailure> {
        if page.is_empty() {
            return Ok(());
        }
        self.seen += page.len() as u64;
        if self.resolved.is_none() {
            let roles: Vec<Role> = self
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    Role::of(column.kind, page.iter().filter_map(|row| row.get(index)))
                })
                .collect();
            let resolved = self
                .spec()
                .resolve(&self.columns, &roles)
                .map_err(|problems| self.column_failure(&problems))?;
            self.resolved = Some((roles, resolved));
        }
        let Some((roles, resolved)) = &self.resolved else {
            return Ok(());
        };
        // Never more than the kind draws: past it the run fails, `limit` or not.
        let most = resolved.choice.kind.max_rows();
        let kept = resolved.limit.map_or(most, |limit| limit.min(most));
        match resolved.sort {
            Some(sort) if resolved.limit.is_some() => {
                self.rows.extend(page.iter().cloned());
                if self.rows.len() >= kept.saturating_mul(2) {
                    let order: Vec<usize> = (0..self.rows.len()).collect();
                    let top = arrange(&self.rows, &order, roles, Some(sort), Some(kept));
                    self.rows = pick(std::mem::take(&mut self.rows), &top);
                }
            }
            _ => {
                let room = kept.saturating_sub(self.rows.len());
                self.rows.extend(page.iter().take(room).cloned());
            }
        }
        Ok(())
    }

    /// Draws the chart into the staging folder, under a partial name.
    ///
    /// # Errors
    ///
    /// A [`RunFailure`] in the chart's step: past its kind's rows or its colours, at the limit
    /// it hit; a column the spec names that the rows don't have; or a file it can't write.
    pub(crate) fn draw(mut self, folders: RunFolders<'_>) -> Result<Completed, RunFailure> {
        let started = Instant::now();
        let drawn = self.drawn()?;
        let bytes = self.picture(&drawn)?;
        let file = CompletedFile::staged(&folders.folder.join(&self.file), folders.staging, &bytes)
            .map_err(|error| RunFailure::new(self.step(), describe(&error)))?;
        self.busy += started.elapsed();
        Ok(Completed {
            step: self.step(),
            rows: drawn.rows,
            warnings: drawn.warnings,
            busy: self.busy,
            file,
        })
    }

    /// What the chart draws of the rows it kept, and the line under it.
    fn drawn(&self) -> Result<Drawn, RunFailure> {
        let Some((roles, resolved)) = &self.resolved else {
            return self.nothing_to_draw();
        };
        let kind = resolved.choice.kind;
        let to_draw = match resolved.limit {
            Some(limit) => self.seen.min(limit as u64),
            None => self.seen,
        };
        if to_draw > kind.max_rows() as u64 {
            return Err(self.past_rows(kind, resolved.limit));
        }
        let order: Vec<usize> = (0..self.rows.len()).collect();
        let order = arrange(&self.rows, &order, roles, resolved.sort, resolved.limit);
        let rows: Vec<&Row> = order
            .iter()
            .filter_map(|&index| self.rows.get(index))
            .collect();
        let data = ChartData::read(&self.columns, roles, &rows, &resolved.choice);
        if !data.summary.series_left_out.is_empty() {
            return Err(self.past_colours(&data));
        }
        let note = match resolved.arrangement(&self.columns) {
            Some(arranged) => format!("{} · {arranged}", data.note()),
            None => data.note(),
        };
        Ok(Drawn {
            rows: rows.len() as u64,
            warnings: data.left_out(),
            options: resolved.options.clone(),
            note,
            data,
        })
    }

    /// The chart of a result with no rows: "No rows to chart" under its title. Its columns'
    /// names are still checked, but not their roles, which need values.
    fn nothing_to_draw(&self) -> Result<Drawn, RunFailure> {
        self.spec()
            .check_names(&self.columns)
            .map_err(|problems| self.column_failure(&problems))?;
        let kind = self.spec().kind.unwrap_or(ChartKind::Bars);
        let roles = vec![Role::Text; self.columns.len()];
        let data = ChartData::read(&self.columns, &roles, &[], &Choice::of(kind, 0, Vec::new()));
        Ok(Drawn {
            rows: 0,
            warnings: vec![NO_ROWS.to_owned()],
            options: self.spec().layout_options(),
            note: data.note(),
            data,
        })
    }

    fn picture(&self, drawn: &Drawn) -> Result<Vec<u8>, RunFailure> {
        let fail = |message: String| RunFailure::new(self.step(), message);
        let measure =
            FontMeasure::inter(FILE_TEXT_SIZE).map_err(|error| fail(error.to_string()))?;
        let size = Size {
            width: self.chart.width as f32,
            height: self.chart.height as f32,
        };
        let drawing = layout_file(&drawn.data, &drawn.options, size, &measure, &drawn.note);
        let svg = dbrover_chart::svg(&drawing, FILE_TEXT_SIZE);
        match self.chart.picture {
            ChartPicture::Svg => Ok(svg.into_bytes()),
            ChartPicture::Png { scale } => {
                dbrover_picture::png(&svg, f32::from(scale), &Fonts::bundled())
                    .map_err(|error| fail(error.to_string()))
            }
        }
    }

    fn column_failure(&self, problems: &[ColumnProblem]) -> RunFailure {
        let details: Vec<&str> = problems
            .iter()
            .map(|problem| problem.detail.as_str())
            .collect();
        RunFailure::new(self.step(), details.join("; "))
    }

    /// More rows than the chart's kind draws, after its `limit` if it has one.
    fn past_rows(&self, kind: ChartKind, limit: Option<usize>) -> RunFailure {
        let most = kind.max_rows();
        let draws = match kind {
            ChartKind::Bars => "bars",
            ChartKind::Lines => "lines",
            ChartKind::Scatter => "a scatter",
        };
        let seen = grouped(self.seen);
        let shown_most = grouped(most as u64);
        let message = match limit {
            Some(limit) => format!(
                "it draws {draws}, at most {shown_most} rows, and its limit is {} of the \
                 result's {seen}; lower limit to {shown_most} or less",
                grouped(limit as u64)
            ),
            None => format!(
                "it draws {draws}, at most {shown_most} rows, and the result has {seen}; group \
                 the rows in the query, or set limit to draw the first ones"
            ),
        };
        RunFailure::at_limit(
            self.step(),
            message,
            LimitHit {
                key: CHART_ROWS_KEY.to_owned(),
                value: format!("{shown_most} rows of {}", kind.spec_name()),
                raise_in: format!("chart {}", self.number),
                set_in: None,
            },
        )
    }

    /// More values of the series column than colours.
    fn past_colours(&self, data: &ChartData) -> RunFailure {
        let split_by = data.split_by.as_deref().unwrap_or_default();
        let left_out = data.summary.series_left_out.join(", ");
        RunFailure::at_limit(
            self.step(),
            format!(
                "{split_by} has more values than the chart's {STARTING_SERIES} colours; past the \
                 eighth: {left_out}"
            ),
            LimitHit {
                key: CHART_SERIES_KEY.to_owned(),
                value: format!("{STARTING_SERIES} series"),
                raise_in: format!("chart {}", self.number),
                set_in: None,
            },
        )
    }
}

/// A chart's points, how it is titled, the line under it, and what it couldn't draw.
struct Drawn {
    data: ChartData,
    options: LayoutOptions,
    note: String,
    rows: u64,
    warnings: Vec<String>,
}

/// The rows at `indexes`, in their order.
fn pick(rows: Vec<Row>, indexes: &[usize]) -> Vec<Row> {
    let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    indexes
        .iter()
        .filter_map(|&index| slots.get_mut(index).and_then(Option::take))
        .collect()
}
