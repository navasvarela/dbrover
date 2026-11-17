//! What a report's kept runs say (`docs/reports-at-scale.md`, "Performance tooling"): the
//! hints' rules, a run's timeline and bottleneck, two runs compared, and what explaining a
//! report needs from its history.
//!
//! Every function here is pure, over a [`dbrover_store::ReportRun`] and, where a plan is
//! involved, a [`dbrover_plan::Plan`]. Nothing reads a file, a database or the clock, so the
//! rules are tested on runs built in memory and give the same answer wherever they are called
//! from: the report's tab, `dbrover report runs`, `dbrover report compare`,
//! `dbrover report explain`, and `dbrover mcp`'s read-only report tools.
//!
//! The rules are in a crate of their own because a kept run is `dbrover-store`'s type and a
//! plan is `dbrover-plan`'s, while the hint ids are `dbrover-report`'s: no existing crate may
//! depend on all three. `dbrover-report` must not depend on `dbrover-store` or the reverse,
//! and `dbrover-report` pulls in DuckDB, so that edge would reach every crate that reads the
//! settings folder (`docs/reports-at-scale.md`, "RS8 against today's code").
//!
//! The drawing stays in the app: `reports/profile_view.rs` draws [`profile`]'s timeline and
//! [`funnel`]'s lines, `reports/compare_view.rs` draws [`compare`]'s comparison, and
//! `reports/runs_table.rs` draws [`insights`]' and [`hints`]' summaries. What those modules
//! draw, what the command line prints, and what an assistant reads are all computed here, and
//! [`json`] is the one shape the last two answer in.

pub mod compare;
pub mod explanation;
pub mod funnel;
pub mod hints;
pub mod insights;
pub mod json;
pub mod plan_hints;
pub mod profile;

pub use hints::{Hint, HintAt};
