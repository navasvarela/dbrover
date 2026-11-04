//! `dbrover report list`: the reports in the settings folder's `reports/`, each with the name
//! `report run` takes, its title, its parameters and what keeps it from running
//! (`docs/reporting.md`, "Finishing RP6").

use std::io::{self, Write};
use std::path::PathBuf;

use dbrover_report::{LimitDefaults, Plan, Report, read_report};
use dbrover_store::{AppPaths, ReportName};
use serde::Serialize;

use super::args::OutputStyle;
use crate::report_worker::Named;

/// A report as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedReport {
    /// What `dbrover report run` takes.
    pub name: ReportName,
    /// `[report] name`, or the file's name when the file can't be read.
    pub title: String,
    pub description: Option<String>,
    pub path: PathBuf,
    /// The data sources its sources name, by name or id as the file writes them; `None` when
    /// the file can't be read.
    pub data_sources: Option<Vec<String>>,
    /// In the order the report asks for them.
    pub params: Vec<ListedParam>,
    /// Empty when it can run.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedParam {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
}

/// Every report in `paths`' reports folder, each read and planned against what the folder
/// names, under `defaults`.
///
/// # Errors
///
/// Why the reports folder can't be read.
pub fn reports(paths: &AppPaths, defaults: &LimitDefaults) -> io::Result<Vec<ListedReport>> {
    let files = paths.report_files();
    let named = Named::load(paths);
    let listed = files
        .list()?
        .into_iter()
        .map(|name| {
            let path = files.path(&name);
            let text = files.read(&name);
            describe(name, path, text, &named, defaults)
        })
        .collect();
    Ok(listed)
}

fn describe(
    name: ReportName,
    path: PathBuf,
    text: io::Result<String>,
    named: &Result<Named, Vec<String>>,
    defaults: &LimitDefaults,
) -> ListedReport {
    let listed = |title: String, report: Option<&Report>, params, problems| ListedReport {
        name: name.clone(),
        title,
        description: report.and_then(|report| report.description.clone()),
        path: path.clone(),
        data_sources: report.map(|report| {
            report
                .sources
                .iter()
                .map(|source| source.data_source.clone())
                .collect()
        }),
        params,
        problems,
    };
    let report = match text {
        Ok(text) => read_report(&text),
        Err(error) => Err(vec![format!("could not read the file: {error}")]),
    };
    let report = match report {
        Ok(report) => report,
        Err(problems) => return listed(name.to_string(), None, Vec::new(), problems),
    };
    let declared = params(&report.params);
    let named = match named {
        Ok(named) => named,
        Err(problems) => {
            return listed(
                report.name.clone(),
                Some(&report),
                declared,
                problems.clone(),
            );
        }
    };
    match Plan::new(
        report.clone(),
        &named.data_sources,
        &named.presets,
        defaults,
    ) {
        Ok(plan) => listed(plan.name, Some(&report), params(&plan.params), Vec::new()),
        Err(problems) => listed(report.name.clone(), Some(&report), declared, problems),
    }
}

fn params(params: &[dbrover_report::Param]) -> Vec<ListedParam> {
    params
        .iter()
        .map(|param| ListedParam {
            name: param.name.clone(),
            param_type: param.param_type.to_string(),
        })
        .collect()
}

/// Writes `reports` to `out` in `style`: a line or more a report for a person, or a JSON object
/// a line.
///
/// # Errors
///
/// If `out` can't be written.
pub fn print(out: &mut impl Write, reports: &[ListedReport], style: OutputStyle) -> io::Result<()> {
    match style {
        OutputStyle::JsonLines => {
            for report in reports {
                let line = serde_json::to_string(report).map_err(io::Error::other)?;
                writeln!(out, "{line}")?;
            }
        }
        OutputStyle::Human => {
            let width = reports
                .iter()
                .map(|report| report.name.as_str().chars().count())
                .max()
                .unwrap_or(0);
            for report in reports {
                writeln!(out, "{}", line(report, width))?;
                for problem in &report.problems {
                    for (index, text) in problem.lines().enumerate() {
                        let lead = if index == 0 { "  ! " } else { "    " };
                        writeln!(out, "{lead}{text}")?;
                    }
                }
            }
        }
    }
    out.flush()
}

/// `sales-by-category  Sales by category (since: date, until: date)`, or, for a report with
/// problems, how many keep it from running.
fn line(report: &ListedReport, width: usize) -> String {
    let name = format!("{:width$}", report.name.as_str());
    let detail = match report.problems.len() {
        0 if report.params.is_empty() => String::new(),
        0 => {
            let params: Vec<String> = report
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, param.param_type))
                .collect();
            format!(" ({})", params.join(", "))
        }
        1 => " — can't run, 1 problem:".to_owned(),
        problems => format!(" — can't run, {problems} problems:"),
    };
    format!("{name}  {}{detail}", report.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, params: &[(&str, &str)], problems: &[&str]) -> ListedReport {
        ListedReport {
            name: ReportName::new(name).unwrap(),
            title: format!("The {name}"),
            description: None,
            path: PathBuf::from(format!("/reports/{name}.toml")),
            data_sources: Some(vec!["shop".to_owned()]),
            params: params
                .iter()
                .map(|(name, param_type)| ListedParam {
                    name: (*name).to_owned(),
                    param_type: (*param_type).to_owned(),
                })
                .collect(),
            problems: problems
                .iter()
                .map(|problem| (*problem).to_owned())
                .collect(),
        }
    }

    fn printed(style: OutputStyle) -> String {
        let reports = [
            listed("sales", &[("since", "date"), ("until", "date")], &[]),
            listed("stock", &[], &[]),
            listed(
                "broken",
                &[],
                &["[combine]: prql, line 1, column 9: Unknown name\n    from tx | x\n         ^"],
            ),
        ];
        let mut out = Vec::new();
        print(&mut out, &reports, style).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn lists_a_report_a_line_with_its_problems_under_it() {
        let expected = [
            "sales   The sales (since: date, until: date)",
            "stock   The stock",
            "broken  The broken — can't run, 1 problem:",
            "  ! [combine]: prql, line 1, column 9: Unknown name",
            "        from tx | x",
            "             ^",
        ];
        assert_eq!(
            printed(OutputStyle::Human),
            expected.map(|line| format!("{line}\n")).concat()
        );
    }

    #[test]
    fn lists_a_json_object_a_report() {
        let printed = printed(OutputStyle::JsonLines);
        let lines: Vec<serde_json::Value> = printed
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["name"], "sales");
        assert_eq!(lines[0]["params"][1]["type"], "date");
        assert_eq!(lines[2]["problems"].as_array().unwrap().len(), 1);
    }
}
