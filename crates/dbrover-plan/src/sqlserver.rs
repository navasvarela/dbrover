//! SQL Server's XML showplan, from `SET SHOWPLAN_XML` (estimated) or `SET STATISTICS XML`
//! (actual): a batch of statements, each with a `QueryPlan` whose `RelOp` elements are the
//! steps. A `RelOp` holds one element named for its operator (`IndexScan`, `Hash`, `Sort`, …),
//! which holds what the operator reads (an `Object`), its predicates, and the `RelOp`s it reads
//! from, sometimes deep inside its expressions (a subquery in a predicate).
//!
//! Actual figures are per thread, under `RunTimeInformation`: rows and executions are summed
//! over threads, and the time is the slowest thread's, as SQL Server Management Studio shows
//! them.

use roxmltree::{Document, Node};

use crate::{Actual, Detail, Estimate, Plan, PlanError, Step, Timing};

/// The elements of a `RelOp` beside its operator's, which hold no steps.
const NOT_OPERATORS: &[&str] = &[
    "OutputList",
    "MemoryFractions",
    "RunTimeInformation",
    "RunTimePartitionSummary",
    "Warnings",
    "InternalInfo",
];

/// The `RelOp` attributes worth showing among a step's facts, with their labels.
const FACTS: &[(&str, &str)] = &[
    ("EstimateCPU", "Estimated CPU cost"),
    ("EstimateIO", "Estimated I/O cost"),
    ("EstimatedExecutionMode", "Execution mode"),
    (
        "EstimateRowsWithoutRowGoal",
        "Rows estimated without a row goal",
    ),
];

/// The predicates an operator may hold, with their labels.
const PREDICATES: &[(&str, &str)] = &[
    ("Predicate", "Predicate"),
    ("SeekPredicates", "Seek predicate"),
    ("ProbeResidual", "Probe residual"),
    ("Residual", "Residual"),
    ("PassThru", "Pass-through"),
];

pub(crate) fn parse(text: &str) -> Result<Plan, PlanError> {
    let document = Document::parse(text)?;
    let statement = document
        .descendants()
        .filter(|node| node.has_tag_name("StmtSimple"))
        .find(|statement| query_plan(*statement).is_some())
        .ok_or_else(|| {
            PlanError::Shape("the showplan holds no statement with a plan".to_owned())
        })?;
    let root = query_plan(statement)
        .and_then(|plan| plan.children().find(|node| node.has_tag_name("RelOp")))
        .ok_or_else(|| PlanError::Shape("the statement's plan has no operator".to_owned()))?;
    Ok(Plan {
        root: step(root),
        details: plan_details(statement),
    })
}

fn query_plan<'a>(statement: Node<'a, 'a>) -> Option<Node<'a, 'a>> {
    statement
        .children()
        .find(|node| node.has_tag_name("QueryPlan"))
}

fn step(relop: Node<'_, '_>) -> Step {
    let operator = relop
        .children()
        .find(|node| node.is_element() && !NOT_OPERATORS.contains(&node.tag_name().name()));
    let mut details: Vec<Detail> = FACTS
        .iter()
        .filter_map(|(attribute, label)| Some(Detail::new(*label, relop.attribute(*attribute)?)))
        .collect();
    if let Some(operator) = operator {
        details.extend(predicates(operator));
    }
    details.extend(warnings(relop));
    Step {
        operation: operation(relop),
        target: operator.and_then(target),
        heading: None,
        estimate: Estimate {
            startup_cost: None,
            total_cost: number(relop, "EstimatedTotalSubtreeCost"),
            rows: number(relop, "EstimateRows"),
        },
        actual: actual(relop),
        details,
        children: operator.map(child_steps).unwrap_or_default(),
    }
}

/// The physical operator, and the logical one when it says more: `Hash Match (Inner Join)`.
fn operation(relop: Node<'_, '_>) -> String {
    let physical = relop.attribute("PhysicalOp").unwrap_or("Operator");
    match relop.attribute("LogicalOp") {
        Some(logical) if logical != physical => format!("{physical} ({logical})"),
        _ => physical.to_owned(),
    }
}

/// What the operator reads, as `on [dbo].[orders].[PK_orders] [o]`.
fn target(operator: Node<'_, '_>) -> Option<String> {
    let object = operator
        .children()
        .find(|node| node.has_tag_name("Object"))?;
    let name: Vec<&str> = ["Schema", "Table", "Index"]
        .into_iter()
        .filter_map(|attribute| object.attribute(attribute))
        .collect();
    if name.is_empty() {
        return None;
    }
    let alias = object
        .attribute("Alias")
        .map(|alias| format!(" {alias}"))
        .unwrap_or_default();
    Some(format!("on {}{alias}", name.join(".")))
}

/// The `RelOp`s the operator reads from: the nearest under it, wherever they are nested.
fn child_steps(operator: Node<'_, '_>) -> Vec<Step> {
    let mut steps = Vec::new();
    for child in operator.children().filter(Node::is_element) {
        if child.has_tag_name("RelOp") {
            steps.push(step(child));
        } else {
            steps.extend(child_steps(child));
        }
    }
    steps
}

fn predicates<'a>(operator: Node<'a, 'a>) -> impl Iterator<Item = Detail> + 'a {
    PREDICATES.iter().filter_map(move |(element, label)| {
        let predicate = operator
            .children()
            .find(|node| node.has_tag_name(*element))?;
        let expressions: Vec<&str> = predicate
            .descendants()
            .filter(|node| node.has_tag_name("ScalarOperator"))
            .filter_map(|node| node.attribute("ScalarString"))
            .collect();
        // A seek's ranges are a scalar per key column; elsewhere the outermost expression
        // already holds the ones inside it.
        let shown = match *element {
            "SeekPredicates" => expressions.join(", "),
            _ => expressions.first().copied().unwrap_or_default().to_owned(),
        };
        (!shown.is_empty()).then(|| Detail::new(*label, shown))
    })
}

/// The warnings SQL Server attached to the step, by name: `NoJoinPredicate`, `SpillToTempDb`.
fn warnings(relop: Node<'_, '_>) -> Option<Detail> {
    let warnings = relop
        .children()
        .find(|node| node.has_tag_name("Warnings"))?;
    let names: Vec<&str> = warnings
        .attributes()
        .filter(|attribute| attribute.value() == "true" || attribute.value() == "1")
        .map(|attribute| attribute.name())
        .chain(
            warnings
                .children()
                .filter(Node::is_element)
                .map(|node| node.tag_name().name()),
        )
        .collect();
    (!names.is_empty()).then(|| Detail::new("Warnings", names.join(", ")))
}

/// What the step did, summed over its threads: rows and executions added up, time the slowest
/// thread's. Rows are then spread over executions, to compare with the estimate, which SQL
/// Server makes per execution.
fn actual(relop: Node<'_, '_>) -> Option<Actual> {
    let threads: Vec<Node<'_, '_>> = relop
        .children()
        .find(|node| node.has_tag_name("RunTimeInformation"))?
        .children()
        .filter(|node| node.has_tag_name("RunTimeCountersPerThread"))
        .collect();
    let sum = |attribute: &str| -> f64 {
        threads
            .iter()
            .filter_map(|thread| number(*thread, attribute))
            .sum()
    };
    let loops = sum("ActualExecutions");
    let elapsed = threads
        .iter()
        .filter_map(|thread| number(*thread, "ActualElapsedms"))
        .reduce(f64::max);
    let ran = loops > 0.0;
    Some(Actual {
        rows: Some(if ran { sum("ActualRows") / loops } else { 0.0 }),
        loops,
        time: elapsed.filter(|_| ran).map(|elapsed| Timing {
            first_row_ms: None,
            last_row_ms: elapsed / loops,
        }),
    })
}

/// The statement's text, and, when it ran, the time it took.
fn plan_details(statement: Node<'_, '_>) -> Vec<Detail> {
    let mut details: Vec<Detail> = statement
        .attribute("StatementText")
        .map(|text| Detail::new("Statement", text.trim()))
        .into_iter()
        .collect();
    let times = query_plan(statement).and_then(|plan| {
        plan.children()
            .find(|node| node.has_tag_name("QueryTimeStats"))
    });
    if let Some(times) = times {
        let labelled = [("ElapsedTime", "Elapsed time"), ("CpuTime", "CPU time")];
        details.extend(labelled.into_iter().filter_map(|(attribute, label)| {
            Some(Detail::new(
                label,
                format!("{} ms", times.attribute(attribute)?),
            ))
        }));
    }
    details
}

fn number(node: Node<'_, '_>, attribute: &str) -> Option<f64> {
    node.attribute(attribute)?.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Heat, Measure, PlanFormat};

    fn fixture(name: &str) -> Plan {
        let path = format!(
            "{}/fixtures/sqlserver/{name}.xml",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        PlanFormat::SqlServerXml.parse(&text).unwrap()
    }

    fn titles(plan: &Plan) -> Vec<(usize, String)> {
        plan.outline()
            .iter()
            .map(|line| (line.depth, line.step.title()))
            .collect()
    }

    #[test]
    fn reads_an_estimated_join_as_its_operators() {
        let plan = fixture("join");
        assert!(!plan.is_analysed());
        let titles = titles(&plan);
        assert_eq!(titles[0], (0, "Sort (TopN Sort)".to_owned()));
        assert!(
            titles.iter().any(|(_, title)| title
                .starts_with("Clustered Index Scan on [dbo].[w10_customers].[PK__w10_cust")
                && title.ends_with(" [c]")),
            "{titles:?}"
        );
        assert!(plan.details[0].value.starts_with("SELECT TOP (10)"));
        let outline = plan.outline();
        let costliest = outline
            .iter()
            .find(|line| line.weight.heat == Heat::Costliest)
            .unwrap();
        // SQL Server sums each customer's orders before the join, over all 20000 of them.
        assert_eq!(costliest.step.operation, "Hash Match (Aggregate)");
    }

    #[test]
    fn reads_what_each_operator_did_in_an_actual_plan() {
        let plan = fixture("join_actual");
        assert!(plan.is_analysed());
        assert_eq!(plan.measure(), Measure::Time);
        let outline = plan.outline();
        let customers = outline
            .iter()
            .find(|line| {
                line.step
                    .target
                    .as_deref()
                    .is_some_and(|target| target.contains("[w10_customers]"))
            })
            .unwrap();
        let actual = customers.step.actual.unwrap();
        assert_eq!((actual.rows, actual.loops), (Some(333.0), 1.0));
        assert!(
            customers
                .step
                .details
                .iter()
                .any(|detail| detail.label == "Predicate" && detail.value.contains("'ES'")),
            "{:?}",
            customers.step.details
        );
        assert!(
            plan.details
                .iter()
                .any(|detail| detail.label == "Elapsed time")
        );
    }

    #[test]
    fn finds_operators_nested_in_subqueries() {
        let plan = fixture("shapes_actual");
        let outline = plan.outline();
        // Every operator in the document is a step, wherever it is nested.
        let text = std::fs::read_to_string(format!(
            "{}/fixtures/sqlserver/shapes_actual.xml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert_eq!(outline.len(), text.matches("<RelOp ").count());
        // SQL Server turned the select list's subquery into a join.
        assert!(
            outline
                .iter()
                .any(|line| line.step.operation == "Merge Join (Right Outer Join)")
        );
    }

    #[test]
    fn refuses_xml_without_a_plan() {
        let cases = [
            (
                "<ShowPlanXML><BatchSequence/></ShowPlanXML>",
                "no statement with a plan",
            ),
            ("SELECT 1", "isn't XML"),
        ];
        for (text, expected) in cases {
            let error = PlanFormat::SqlServerXml.parse(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}: {error}");
        }
    }
}
