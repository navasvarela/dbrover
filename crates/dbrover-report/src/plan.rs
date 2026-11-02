//! A report against what it names: its data source and `exports.toml`'s presets, found and
//! checked, so that a run needs nothing but its parameters' values.

use std::collections::BTreeMap;
use std::time::Duration;

use dbrover_core::DataSource;
use dbrover_export::{
    CellTarget, ColumnType, CsvOptions, CustomFormat, CustomFormatKind, ExportFormat, ExportSpec,
    SchemaColumn, XlsxOptions,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, ParameterValues};

use crate::{
    FileName, Output, OutputColumns, OutputLimits, OutputWriter, Param, Report, Source,
    SourceLimits,
};

/// A report ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    pub description: Option<String>,
    /// In the order the query first uses them, then those only file names use, as the file
    /// lists them: the order the tab asks for them in.
    pub params: Vec<Param>,
    pub timeout: Duration,
    pub source: PlannedSource,
    pub outputs: Vec<PlannedOutput>,
}

/// The source, with its data source found.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedSource {
    pub id: String,
    pub data_source: DataSource,
    /// The data source's SQL dialect, which its query and parameters are read in.
    pub dialect: Dialect,
    pub query: String,
    pub limits: SourceLimits,
}

/// An output, with its preset found and its columns known.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedOutput {
    pub format: OutputFormat,
    pub file: FileName,
    /// `None` writes every column of the result as it comes.
    pub columns: Option<Vec<SchemaColumn>>,
    pub limits: OutputLimits,
    pub continue_on_new_sheet: bool,
}

/// The formats a report writes in RP1, with their options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Csv(CsvOptions),
    Xlsx(XlsxOptions),
}

impl OutputFormat {
    pub fn spec(&self) -> ExportSpec {
        match self {
            Self::Csv(options) => ExportSpec::Csv(options.clone()),
            Self::Xlsx(options) => ExportSpec::Xlsx(options.clone()),
        }
    }

    /// Where its converted values go, which decides how finely times are written.
    pub fn target(&self) -> CellTarget {
        match self {
            Self::Csv(_) => CellTarget::Text,
            Self::Xlsx(_) => CellTarget::Excel,
        }
    }

    /// Whether the file is a workbook, which holds so many rows a sheet.
    pub fn is_excel(&self) -> bool {
        matches!(self, Self::Xlsx(_))
    }
}

/// The values of a run's parameters as typed, by name.
pub type TypedValues = BTreeMap<String, String>;

/// A run's parameters, checked, and the files they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunValues {
    /// Each value in its type's plain form (`2026-10-01`), as the query is sent them.
    pub parameters: ParameterValues,
    /// Each output's file name, in the outputs' order.
    pub files: Vec<String>,
}

impl Plan {
    /// Finds what `report` names among `data_sources` and `presets`.
    ///
    /// # Errors
    ///
    /// Every problem found, each a sentence naming where it is.
    pub fn new(
        report: Report,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
    ) -> Result<Self, Vec<String>> {
        let mut problems = Vec::new();
        let source = plan_source(&report.source, data_sources).map_err(|problem| {
            problems.push(format!("source {:?}: {problem}", report.source.id));
        });
        let outputs = plan_outputs(&report.outputs, presets, &mut problems);
        let params = match &source {
            Ok(source) => ordered_params(&report.params, source, &mut problems),
            Err(()) => report.params.clone(),
        };
        match source {
            Ok(source) if problems.is_empty() => Ok(Self {
                name: report.name,
                description: report.description,
                params,
                timeout: report.timeout,
                source,
                outputs,
            }),
            _ => Err(problems),
        }
    }

    /// Checks `typed` against the parameters' types, and names each output's file.
    ///
    /// # Errors
    ///
    /// One sentence per parameter whose value is missing or isn't of its type, or per file
    /// name the values make unusable.
    pub fn values(&self, typed: &TypedValues) -> Result<RunValues, Vec<String>> {
        let mut problems = Vec::new();
        let mut checked: BTreeMap<&str, (ColumnType, String)> = BTreeMap::new();
        for param in &self.params {
            let value = typed
                .get(&param.name)
                .map(String::as_str)
                .unwrap_or_default();
            if value.trim().is_empty() && param.param_type != ColumnType::Text {
                problems.push(format!("{} needs a value", param.name));
                continue;
            }
            match param.param_type.plain_form(value.trim()) {
                Ok(plain) => {
                    checked.insert(&param.name, (param.param_type, plain));
                }
                Err(reason) => problems.push(format!("{}: {value:?} {reason}", param.name)),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let by_name: BTreeMap<&str, (ColumnType, &str)> = checked
            .iter()
            .map(|(name, (param_type, plain))| (*name, (*param_type, plain.as_str())))
            .collect();
        let mut files: Vec<String> = Vec::new();
        for (index, output) in self.outputs.iter().enumerate() {
            match output.file.render(&by_name) {
                Ok(file) if files.contains(&file) => problems.push(format!(
                    "output {}: another output already writes {file:?} with these values",
                    index + 1
                )),
                Ok(file) => files.push(file),
                Err(problem) => problems.push(format!("output {}: {problem}", index + 1)),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let parameters = checked
            .into_iter()
            .map(|(name, (_, plain))| (name, Some(plain)))
            .collect();
        Ok(RunValues { parameters, files })
    }
}

fn plan_source(source: &Source, data_sources: &[DataSource]) -> Result<PlannedSource, String> {
    let data_source = find_data_source(&source.data_source, data_sources)?;
    let driver = data_source.config.driver();
    let dialect = QueryLanguage::from(driver).dialect().ok_or_else(|| {
        format!(
            "{:?} is a {} data source, which a report reads once RP3 declares the columns \
             its results have; until then a source is on a SQL data source",
            data_source.name,
            driver.display_name()
        )
    })?;
    Ok(PlannedSource {
        id: source.id.clone(),
        data_source: data_source.clone(),
        dialect,
        query: source.query.clone(),
        limits: source.limits,
    })
}

/// The data source `wanted` names: by name if one has it, else by id.
fn find_data_source<'a>(
    wanted: &str,
    data_sources: &'a [DataSource],
) -> Result<&'a DataSource, String> {
    let named: Vec<&DataSource> = data_sources
        .iter()
        .filter(|data_source| data_source.name == wanted)
        .collect();
    match named.as_slice() {
        [one] => return Ok(one),
        [] => {}
        several => {
            let ids: Vec<String> = several
                .iter()
                .map(|data_source| data_source.id.to_string())
                .collect();
            return Err(format!(
                "{} data sources are named {wanted:?}; name one by its id instead: {}",
                several.len(),
                ids.join(", ")
            ));
        }
    }
    data_sources
        .iter()
        .find(|data_source| data_source.id.to_string() == wanted)
        .ok_or_else(|| {
            let mut names: Vec<&str> = data_sources
                .iter()
                .map(|data_source| data_source.name.as_str())
                .collect();
            names.sort_unstable();
            if names.is_empty() {
                format!("there is no data source {wanted:?}, nor any other yet")
            } else {
                format!(
                    "there is no data source {wanted:?}; there are {}",
                    names.join(", ")
                )
            }
        })
}

/// The parameters in the order the query first uses them, then the rest, with a problem for
/// each `:name` the report doesn't declare.
fn ordered_params(
    params: &[Param],
    source: &PlannedSource,
    problems: &mut Vec<String>,
) -> Vec<Param> {
    let used = dbrover_sql::parameter_names(&source.query, source.dialect);
    let mut ordered: Vec<Param> = Vec::new();
    for name in &used {
        match params.iter().find(|param| &param.name == name) {
            Some(param) => ordered.push(param.clone()),
            None => problems.push(format!(
                "source {:?}: the query uses :{name}, which [report] params doesn't declare; \
                 add {name} = \"text\" (or its type) to params",
                source.id
            )),
        }
    }
    ordered.extend(
        params
            .iter()
            .filter(|param| !used.contains(&param.name))
            .cloned(),
    );
    ordered
}

fn plan_outputs(
    outputs: &[Output],
    presets: &[CustomFormat],
    problems: &mut Vec<String>,
) -> Vec<PlannedOutput> {
    let first_columns = outputs.first().and_then(|first| match &first.columns {
        OutputColumns::Declared(columns) => Some(columns.clone()),
        OutputColumns::All | OutputColumns::Same => None,
    });
    let mut planned = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let label = format!("output {}", index + 1);
        let format = match &output.writer {
            OutputWriter::Format(preset) => built_in(preset.format, &preset.options),
            OutputWriter::Preset(id) => find_preset(id.as_str(), presets),
        };
        let format = match format {
            Ok(format) => format,
            Err(problem) => {
                problems.push(format!("{label}: {problem}"));
                continue;
            }
        };
        if output.continue_on_new_sheet && !format.is_excel() {
            problems.push(format!(
                "{label}: continue_on_new_sheet is for Excel outputs"
            ));
        }
        let columns = match &output.columns {
            OutputColumns::All => None,
            OutputColumns::Declared(columns) => Some(columns.clone()),
            OutputColumns::Same => first_columns.clone(),
        };
        planned.push(PlannedOutput {
            format,
            file: output.file.clone(),
            columns,
            limits: output.limits,
            continue_on_new_sheet: output.continue_on_new_sheet,
        });
    }
    planned
}

fn built_in(
    format: ExportFormat,
    options: &dbrover_export::ExportOptions,
) -> Result<OutputFormat, String> {
    match format {
        ExportFormat::Csv => Ok(OutputFormat::Csv(options.csv.clone())),
        ExportFormat::Xlsx => Ok(OutputFormat::Xlsx(options.xlsx.clone())),
        other => Err(format!(
            "a report writes CSV or Excel files in RP1, not {}",
            other.name()
        )),
    }
}

fn find_preset(id: &str, presets: &[CustomFormat]) -> Result<OutputFormat, String> {
    let Some(found) = presets.iter().find(|format| format.id.as_str() == id) else {
        return Err(format!(
            "exports.toml has no export with the id {id:?}; give the output a format, or add \
             the preset to exports.toml"
        ));
    };
    match &found.kind {
        CustomFormatKind::Preset(preset) => built_in(preset.format, &preset.options)
            .map_err(|problem| format!("the preset {id:?}: {problem}")),
        CustomFormatKind::Template(_) | CustomFormatKind::Program(_) => Err(format!(
            "{id:?} in exports.toml is a template or a program; a report's output is a CSV or \
             Excel preset in RP1"
        )),
    }
}
