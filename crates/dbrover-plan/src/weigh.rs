//! Which steps of a plan cost the most.
//!
//! Every engine reports a step's cost, and its time, with everything below it included, so the
//! root always looks the costliest. What a step costs on its own is what is left once its
//! children's share is taken away, and that is what points at the step worth looking at.

use crate::{Plan, Step};

/// A step whose own share of the plan is at least this is marked as costly, besides the
/// costliest step. A quarter was chosen so that a plan has at most three such steps besides the
/// costliest, which keeps the highlighting meaningful on a plan of dozens of steps.
const WARM_SHARE: f64 = 0.25;

/// What a plan's steps are weighed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    /// The database's estimated cost.
    Cost,
    /// The time each step took, once the statement has run with its steps timed.
    Time,
}

/// How much one step accounts for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weight {
    /// The step's own cost or time, without its children's: in the database's cost units, or
    /// in milliseconds over every time it ran.
    pub own: f64,
    /// `own` as a share of the whole plan's, from 0 to 1.
    pub share: f64,
    pub heat: Heat,
}

/// How strongly a step is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heat {
    Cool,
    /// At least a quarter of the plan's cost, but not the costliest step.
    Warm,
    /// The step with the largest own share. A plan whose steps all cost nothing has none.
    Costliest,
}

/// A step of the plan in reading order (each step, then its children), with its place in the
/// tree and its weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line<'a> {
    pub step: &'a Step,
    /// 0 for the root.
    pub depth: usize,
    /// How many steps are below this one. They are the lines right after it, so folding the
    /// step skips that many.
    pub descendants: usize,
    pub weight: Weight,
}

impl Plan {
    /// Time, when the statement ran and the database timed its steps; the estimate otherwise.
    pub fn measure(&self) -> Measure {
        let timed = |step: &Step| step.actual.is_some_and(|actual| actual.time.is_some());
        if self.root.any(&timed) {
            Measure::Time
        } else {
            Measure::Cost
        }
    }

    /// Every step in reading order, weighed by [`measure`](Self::measure).
    pub fn outline(&self) -> Vec<Line<'_>> {
        let measure = self.measure();
        let mut lines = Vec::new();
        add_lines(&self.root, 0, measure, &mut lines);
        let total: f64 = lines.iter().map(|line| line.weight.own).sum();
        let costliest = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.weight.own > 0.0)
            .fold(None::<(usize, f64)>, |best, (index, line)| match best {
                Some((_, own)) if own >= line.weight.own => best,
                _ => Some((index, line.weight.own)),
            })
            .map(|(index, _)| index);
        for (index, line) in lines.iter_mut().enumerate() {
            let share = if total > 0.0 {
                line.weight.own / total
            } else {
                0.0
            };
            line.weight.share = share;
            line.weight.heat = if Some(index) == costliest {
                Heat::Costliest
            } else if share >= WARM_SHARE {
                Heat::Warm
            } else {
                Heat::Cool
            };
        }
        lines
    }
}

/// Adds `step` and the steps below it, with their own weights; shares come once every step is
/// known. Returns how many lines were added.
fn add_lines<'a>(
    step: &'a Step,
    depth: usize,
    measure: Measure,
    lines: &mut Vec<Line<'a>>,
) -> usize {
    let at = lines.len();
    lines.push(Line {
        step,
        depth,
        descendants: 0,
        weight: Weight {
            own: own(step, measure),
            share: 0.0,
            heat: Heat::Cool,
        },
    });
    let descendants: usize = step
        .children
        .iter()
        .map(|child| add_lines(child, depth + 1, measure, lines))
        .sum();
    if let Some(line) = lines.get_mut(at) {
        line.descendants = descendants;
    }
    descendants + 1
}

/// What the step and everything below it cost, as the database reports it.
fn inclusive(step: &Step, measure: Measure) -> Option<f64> {
    match measure {
        Measure::Cost => step.estimate.total_cost,
        Measure::Time => step.actual.and_then(|actual| actual.total_ms()),
    }
}

/// The step's subtree's cost: its own figure where it has one, otherwise its children's, so a
/// step the database didn't cost (some of MySQL's) passes its children's cost up.
fn subtree(step: &Step, measure: Measure) -> f64 {
    inclusive(step, measure).unwrap_or_else(|| children(step, measure))
}

fn children(step: &Step, measure: Measure) -> f64 {
    step.children
        .iter()
        .map(|child| subtree(child, measure))
        .sum()
}

/// The step's cost without its children's. Never below 0: a database may cost a child that runs
/// once per row of its parent (PostgreSQL's subplans) above the parent's own total.
fn own(step: &Step, measure: Measure) -> f64 {
    inclusive(step, measure)
        .map(|value| (value - children(step, measure)).max(0.0))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Actual, Estimate, Timing};

    fn step(operation: &str, cost: Option<f64>, children: Vec<Step>) -> Step {
        Step {
            operation: operation.to_owned(),
            target: None,
            heading: None,
            estimate: Estimate {
                startup_cost: None,
                total_cost: cost,
                rows: None,
            },
            actual: None,
            details: Vec::new(),
            children,
        }
    }

    fn timed(mut step: Step, last_row_ms: f64, loops: f64) -> Step {
        step.actual = Some(Actual {
            rows: Some(1.0),
            loops,
            time: Some(Timing {
                first_row_ms: None,
                last_row_ms,
            }),
        });
        step
    }

    fn plan(root: Step) -> Plan {
        Plan {
            root,
            details: Vec::new(),
        }
    }

    fn summary(plan: &Plan) -> Vec<(&str, usize, usize, f64, Heat)> {
        plan.outline()
            .iter()
            .map(|line| {
                (
                    line.step.operation.as_str(),
                    line.depth,
                    line.descendants,
                    (line.weight.own * 100.0).round() / 100.0,
                    line.weight.heat,
                )
            })
            .collect()
    }

    #[test]
    fn weighs_each_step_by_its_cost_without_its_childrens() {
        let plan = plan(step(
            "Limit",
            Some(100.0),
            vec![step(
                "Hash Join",
                Some(95.0),
                vec![
                    step("Seq Scan", Some(60.0), vec![]),
                    step(
                        "Hash",
                        Some(20.0),
                        vec![step("Seq Scan", Some(20.0), vec![])],
                    ),
                ],
            )],
        ));
        assert_eq!(plan.measure(), Measure::Cost);
        assert_eq!(
            summary(&plan),
            [
                ("Limit", 0, 4, 5.0, Heat::Cool),
                ("Hash Join", 1, 3, 15.0, Heat::Cool),
                ("Seq Scan", 2, 0, 60.0, Heat::Costliest),
                ("Hash", 2, 1, 0.0, Heat::Cool),
                ("Seq Scan", 3, 0, 20.0, Heat::Cool),
            ]
        );
        let shares: f64 = plan.outline().iter().map(|line| line.weight.share).sum();
        assert!((shares - 1.0).abs() < 1e-9, "{shares}");
    }

    #[test]
    fn marks_steps_with_a_quarter_of_the_plan_as_warm() {
        let plan = plan(step(
            "Append",
            Some(100.0),
            vec![
                step("Seq Scan", Some(40.0), vec![]),
                step("Seq Scan", Some(30.0), vec![]),
                step("Seq Scan", Some(30.0), vec![]),
            ],
        ));
        let heats: Vec<Heat> = plan.outline().iter().map(|line| line.weight.heat).collect();
        assert_eq!(heats, [Heat::Cool, Heat::Costliest, Heat::Warm, Heat::Warm]);
    }

    #[test]
    fn weighs_by_time_over_every_loop_once_analysed() {
        // The inner index scan is cheap each time but runs 100 times.
        let plan = plan(timed(
            step(
                "Nested Loop",
                Some(10.0),
                vec![
                    timed(step("Seq Scan", Some(8.0), vec![]), 1.0, 1.0),
                    timed(step("Index Scan", Some(0.5), vec![]), 0.05, 100.0),
                ],
            ),
            7.0,
            1.0,
        ));
        assert_eq!(plan.measure(), Measure::Time);
        assert_eq!(
            summary(&plan),
            [
                ("Nested Loop", 0, 2, 1.0, Heat::Cool),
                ("Seq Scan", 1, 0, 1.0, Heat::Cool),
                ("Index Scan", 1, 0, 5.0, Heat::Costliest),
            ]
        );
    }

    #[test]
    fn passes_up_the_cost_of_steps_the_database_did_not_cost() {
        let plan = plan(step(
            "Sort",
            None,
            vec![step(
                "Aggregate",
                None,
                vec![step("Table scan", Some(50.0), vec![])],
            )],
        ));
        assert_eq!(
            summary(&plan),
            [
                ("Sort", 0, 2, 0.0, Heat::Cool),
                ("Aggregate", 1, 1, 0.0, Heat::Cool),
                ("Table scan", 2, 0, 50.0, Heat::Costliest),
            ]
        );
    }

    #[test]
    fn a_plan_that_costs_nothing_has_no_costliest_step() {
        let plan = plan(step("Result", Some(0.0), vec![]));
        assert_eq!(summary(&plan), [("Result", 0, 0, 0.0, Heat::Cool)]);
    }
}
