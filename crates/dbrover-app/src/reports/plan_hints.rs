//! The hints a combine step's plan gives (`docs/reports-at-scale.md`, "Hints", RS2e): what the
//! combine does to one source alone, which that source's own query could do where its rows
//! are, so fewer of them cross the network and load into DuckDB.
//!
//! - `unused-columns`: columns of a source the plan never reads, worth a share of its bytes;
//! - `filter-one-source`: filters the plan applies as it reads one source's table;
//! - `aggregate-one-source`: an aggregate over one source's table with nothing joined first,
//!   which is where moving it keeps the result the same.
//!
//! Pure functions of a plan and what is known of the sources: a run's profile, or **Explain**'s
//! plan with the last run's tables.

use dbrover_core::ByteCount;
use dbrover_plan::{Plan, TableRead};
use dbrover_report::{HintId, grouped};

use super::hints::{Hint, HintAt};

/// `unused-columns`: the share of a source's bytes, from 0 to 100, its unread columns must
/// hold: under a fifth, dropping them saves too little to mention.
const UNUSED_SHARE_PCT: u64 = 20;
/// A source must return at least this many rows for its hint to be worth giving, as for
/// `source-reads-more`: below it, moving work to the source saves too little.
const MIN_ROWS: u64 = 10_000;

/// What is known of a source for its plan hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFacts {
    pub id: String,
    /// The rows it returned; `None` when it hasn't run, as when explaining a report before
    /// its first run, which gives every hint the plan shows.
    pub rows: Option<u64>,
    /// Its table's columns, each with the bytes its values took when known.
    pub columns: Vec<(String, Option<u64>)>,
}

impl SourceFacts {
    fn worth_a_hint(&self) -> bool {
        self.rows.is_none_or(|rows| rows >= MIN_ROWS)
    }
}

/// Every plan hint of `plan`, a source at a time in `sources`' order.
pub fn plan_hints(plan: &Plan, sources: &[SourceFacts]) -> Vec<Hint> {
    let reads = plan.table_reads();
    let aggregated = plan.tables_aggregated_alone();
    let mut hints = Vec::new();
    for source in sources.iter().filter(|source| source.worth_a_hint()) {
        let reads: Vec<&TableRead> = reads
            .iter()
            .filter(|read| read.table.eq_ignore_ascii_case(&source.id))
            .collect();
        if reads.is_empty() {
            continue;
        }
        hints.extend(unused_columns(source, &reads));
        hints.extend(filter_one_source(source, &reads));
        if aggregated
            .iter()
            .any(|table| table.eq_ignore_ascii_case(&source.id))
        {
            hints.push(aggregate_one_source(source));
        }
    }
    hints
}

fn unused_columns(source: &SourceFacts, reads: &[&TableRead]) -> Option<Hint> {
    let used = |name: &str| {
        reads.iter().any(|read| {
            read.columns
                .iter()
                .any(|column| column.eq_ignore_ascii_case(name))
                || read.filters.iter().any(|filter| mentions(filter, name))
        })
    };
    let total: u64 = source.columns.iter().filter_map(|(_, bytes)| *bytes).sum();
    let unused: Vec<&(String, Option<u64>)> = source
        .columns
        .iter()
        .filter(|(name, _)| !used(name))
        .collect();
    let unused_bytes: u64 = unused.iter().filter_map(|(_, bytes)| *bytes).sum();
    let share = u128::from(unused_bytes) * 100 / u128::from(total.max(1));
    if unused.is_empty() || total == 0 || share < u128::from(UNUSED_SHARE_PCT) {
        return None;
    }
    let names: Vec<&str> = unused.iter().map(|(name, _)| name.as_str()).collect();
    let (columns, them) = if names.len() == 1 {
        ("column", "it")
    } else {
        ("columns", "them")
    };
    Some(Hint {
        id: HintId::UnusedColumns,
        at: HintAt::Source(source.id.clone()),
        text: format!(
            "The combine step never reads the {columns} {} of source {}: {share}% of the {} it \
             returned. Dropping {them} from the source's query saves sending and loading {them}.",
            names.join(", "),
            source.id,
            ByteCount(total)
        ),
    })
}

fn filter_one_source(source: &SourceFacts, reads: &[&TableRead]) -> Option<Hint> {
    let filters: Vec<&str> = reads
        .iter()
        .flat_map(|read| read.filters.iter())
        .map(String::as_str)
        .filter(|filter| !is_from_a_join(filter))
        .collect();
    if filters.is_empty() {
        return None;
    }
    let returned = source
        .rows
        .map(|rows| format!(" of the {} rows it returned", grouped(rows)))
        .unwrap_or_default();
    Some(Hint {
        id: HintId::FilterOneSource,
        at: HintAt::Source(source.id.clone()),
        text: format!(
            "The combine step filters the rows of source {}{returned} as it reads them: {}. In the \
             source's query, its database returns only the rows kept, and can use an index.",
            source.id,
            filters.join(" AND ")
        ),
    })
}

fn aggregate_one_source(source: &SourceFacts) -> Hint {
    Hint {
        id: HintId::AggregateOneSource,
        at: HintAt::Source(source.id.clone()),
        text: format!(
            "The combine step aggregates source {} with nothing joined to it first. Aggregating \
             in the source's query returns its groups instead of its rows, with the same result.",
            source.id
        ),
    }
}

/// Whether `filter` is one DuckDB derived from a join to skip rows that can't match (a range
/// of the other side's keys), rather than one the query wrote: it marks them `optional`.
fn is_from_a_join(filter: &str) -> bool {
    filter
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("optional")
}

/// Whether `filter` names the column `name`, as a whole word, without regard to case.
fn mentions(filter: &str, name: &str) -> bool {
    filter
        .split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .any(|word| word.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use dbrover_plan::PlanFormat;

    use super::*;

    /// `SELECT region, sum(amount) FROM orders WHERE region = 'EU' GROUP BY region`, as
    /// DuckDB explains it: the filter on the scan, the aggregate on it alone.
    const PLAN: &str = r##"[{"name": "HASH_GROUP_BY", "extra_info": {"Groups": "#0"},
        "children": [{"name": "SEQ_SCAN", "children": [], "extra_info": {"Table": "orders",
        "Projections": ["region", "amount"], "Filters": "region='EU'",
        "Estimated Cardinality": "1000"}}]}]"##;

    fn orders(rows: Option<u64>) -> SourceFacts {
        SourceFacts {
            id: "orders".to_owned(),
            rows,
            columns: vec![
                ("region".to_owned(), Some(20_000)),
                ("amount".to_owned(), Some(50_000)),
                ("memo".to_owned(), Some(30_000)),
            ],
        }
    }

    fn hints(sources: &[SourceFacts]) -> Vec<Hint> {
        let plan = PlanFormat::DuckDbJson.parse(PLAN).unwrap();
        plan_hints(&plan, sources)
    }

    #[test]
    fn names_the_unused_columns_the_filter_and_the_aggregate_of_one_source() {
        let hints = hints(&[orders(Some(10_000))]);
        let ids: Vec<HintId> = hints.iter().map(|hint| hint.id).collect();
        assert_eq!(
            ids,
            [
                HintId::UnusedColumns,
                HintId::FilterOneSource,
                HintId::AggregateOneSource
            ]
        );
        assert!(
            hints
                .iter()
                .all(|hint| hint.at == HintAt::Source("orders".to_owned()))
        );
        assert_eq!(
            hints[0].text,
            "The combine step never reads the column memo of source orders: 30% of the 100.0 KB \
             it returned. Dropping it from the source's query saves sending and loading it."
        );
        assert!(
            hints[1]
                .text
                .contains("10,000 rows it returned as it reads them: region='EU'."),
            "{}",
            hints[1].text
        );
    }

    #[test]
    fn says_nothing_of_a_small_source_or_one_the_plan_doesnt_read() {
        assert_eq!(hints(&[orders(Some(9_999))]), []);
        let mut other = orders(None);
        other.id = "customers".to_owned();
        assert_eq!(hints(&[other]), []);
    }

    #[test]
    fn gives_every_hint_before_a_first_run_and_weighs_columns_only_when_their_bytes_are_known() {
        let mut unknown_bytes = orders(None);
        for column in &mut unknown_bytes.columns {
            column.1 = None;
        }
        let ids: Vec<HintId> = hints(&[unknown_bytes]).iter().map(|hint| hint.id).collect();
        assert_eq!(ids, [HintId::FilterOneSource, HintId::AggregateOneSource]);
    }

    #[test]
    fn counts_a_column_a_filter_names_as_read_and_leaves_a_joins_filter_out() {
        assert!(mentions("(memo IS NOT NULL)", "MEMO"));
        assert!(!mentions("memorandum = 1", "memo"));
        assert!(is_from_a_join("optional: asset>='BTC' AND asset<='ETH'"));
        assert!(!is_from_a_join("region='EU'"));
    }
}
