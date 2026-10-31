//! The text plan: a line per step, indented under its parent with its facts below it, as
//! PostgreSQL's `EXPLAIN` writes its plans, for every engine's plan alike.

use std::fmt::Write as _;

use crate::{Actual, Estimate, Plan, Step};

/// How far a step's facts, and its children's arrows, are indented past its own arrow. Six, as
/// PostgreSQL does: the width of the `->  ` before a step's title, and two more.
const LEVEL_INDENT: usize = 6;
/// How far a subtree's heading pushes the subtree in, as PostgreSQL indents an `InitPlan` or a
/// `SubPlan` under its heading.
const HEADING_INDENT: usize = 2;

impl Plan {
    /// The plan as text, ending with a line break.
    pub fn text(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "{}{}", self.root.title(), figures(&self.root));
        write_body(&mut text, &self.root, 2);
        for detail in &self.details {
            let _ = writeln!(text, "{}: {}", detail.label, detail.value);
        }
        text
    }
}

/// Writes the step's facts, then its children, at `indent`.
fn write_body(text: &mut String, step: &Step, indent: usize) {
    for detail in &step.details {
        let _ = writeln!(text, "{:indent$}{}: {}", "", detail.label, detail.value);
    }
    for child in &step.children {
        write_child(text, child, indent);
    }
}

fn write_child(text: &mut String, step: &Step, mut indent: usize) {
    if let Some(heading) = &step.heading {
        let _ = writeln!(text, "{:indent$}{heading}", "");
        indent += HEADING_INDENT;
    }
    let _ = writeln!(text, "{:indent$}->  {}{}", "", step.title(), figures(step));
    write_body(text, step, indent + LEVEL_INDENT);
}

/// The estimate and, once the statement ran, what happened, in parentheses after the title.
fn figures(step: &Step) -> String {
    let mut figures = estimate(step.estimate);
    if let Some(actual) = step.actual {
        figures.push_str(&actual_figures(actual));
    }
    figures
}

fn estimate(estimate: Estimate) -> String {
    let cost = match (estimate.startup_cost, estimate.total_cost) {
        (Some(startup), Some(total)) => Some(format!("cost={startup:.2}..{total:.2}")),
        (None, Some(total)) => Some(format!("cost={total:.2}")),
        (_, None) => None,
    };
    let rows = estimate
        .rows
        .map(|rows| format!("rows={}", format_rows(rows)));
    let parts: Vec<String> = cost.into_iter().chain(rows).collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join(" "))
    }
}

fn actual_figures(actual: Actual) -> String {
    if actual.loops == 0.0 {
        return " (never executed)".to_owned();
    }
    let time = actual.time.map(|time| match time.first_row_ms {
        Some(first) => format!("time={first:.3}..{:.3}", time.last_row_ms),
        None => format!("time={:.3}", time.last_row_ms),
    });
    let rows = actual
        .rows
        .map(|rows| format!("rows={}", format_rows(rows)));
    let loops = format!("loops={}", format_rows(actual.loops));
    let parts: Vec<String> = time.into_iter().chain(rows).chain([loops]).collect();
    format!(" (actual {})", parts.join(" "))
}

/// A row count as plans show it: whole, unless it is an average or an estimate that isn't.
pub fn format_rows(value: f64) -> String {
    if (value - value.round()).abs() < 0.005 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Detail, Timing};

    fn step(operation: &str, target: Option<&str>, children: Vec<Step>) -> Step {
        Step {
            operation: operation.to_owned(),
            target: target.map(str::to_owned),
            heading: None,
            estimate: Estimate {
                startup_cost: Some(0.0),
                total_cost: Some(12.5),
                rows: Some(100.0),
            },
            actual: None,
            details: Vec::new(),
            children,
        }
    }

    #[test]
    fn writes_steps_indented_under_their_parents_with_their_facts() {
        let mut scan = step("Seq Scan", Some("on orders o"), vec![]);
        scan.details = vec![Detail::new("Filter", "(total > 10)")];
        let mut cte = step("Seq Scan", Some("on customers"), vec![]);
        cte.heading = Some("CTE recent".to_owned());
        cte.estimate = Estimate {
            startup_cost: None,
            total_cost: None,
            rows: Some(3.333),
        };
        let mut join = step("Hash Join", None, vec![scan, cte]);
        join.details = vec![Detail::new("Hash Cond", "(o.customer_id = c.id)")];
        let plan = Plan {
            root: step("Limit", None, vec![join]),
            details: vec![Detail::new("Planning Time", "0.1 ms")],
        };
        assert_eq!(
            plan.text(),
            "Limit  (cost=0.00..12.50 rows=100)\n\
             \x20 ->  Hash Join  (cost=0.00..12.50 rows=100)\n\
             \x20       Hash Cond: (o.customer_id = c.id)\n\
             \x20       ->  Seq Scan on orders o  (cost=0.00..12.50 rows=100)\n\
             \x20             Filter: (total > 10)\n\
             \x20       CTE recent\n\
             \x20         ->  Seq Scan on customers  (rows=3.33)\n\
             Planning Time: 0.1 ms\n"
        );
    }

    #[test]
    fn writes_what_happened_after_the_estimate() {
        let mut ran = step("Index Scan", Some("using pk on t"), vec![]);
        ran.actual = Some(Actual {
            rows: Some(1.0),
            loops: 20.0,
            time: Some(Timing {
                first_row_ms: Some(0.01),
                last_row_ms: 0.0125,
            }),
        });
        let mut untimed = step("Sort", None, vec![]);
        untimed.actual = Some(Actual {
            rows: Some(0.5),
            loops: 2.0,
            time: None,
        });
        let mut uncounted = step("Query block", None, vec![]);
        uncounted.actual = Some(Actual {
            rows: None,
            loops: 1.0,
            time: Some(Timing {
                first_row_ms: None,
                last_row_ms: 2.5,
            }),
        });
        let mut skipped = step("Seq Scan", None, vec![]);
        skipped.actual = Some(Actual {
            rows: Some(0.0),
            loops: 0.0,
            time: None,
        });
        assert_eq!(
            figures(&ran),
            "  (cost=0.00..12.50 rows=100) (actual time=0.010..0.013 rows=1 loops=20)"
        );
        assert_eq!(
            figures(&untimed),
            "  (cost=0.00..12.50 rows=100) (actual rows=0.50 loops=2)"
        );
        assert_eq!(
            figures(&uncounted),
            "  (cost=0.00..12.50 rows=100) (actual time=2.500 loops=1)"
        );
        assert_eq!(
            figures(&skipped),
            "  (cost=0.00..12.50 rows=100) (never executed)"
        );
    }
}
