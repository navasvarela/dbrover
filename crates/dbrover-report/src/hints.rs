//! The kinds of hint a report's runs give, by the ids a report's `quiet_hints` names them by
//! (`docs/reporting.md`, "Finishing RP4"). The ids are those of the rules in
//! `docs/reports-at-scale.md`'s "Hints", which add to this list.

use std::fmt;
use std::str::FromStr;

/// A kind of hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HintId {
    /// A source reads far more rows than the outputs write.
    SourceReadsMore,
    /// The combine step spilled to disk.
    CombineSpilled,
    /// A source spent most of its time before its first row (RS1e).
    SlowFirstRow,
    /// A source spent most of its time loading into DuckDB (RS1e).
    LoadBound,
    /// A lookup sent many batches (RS1e).
    ManyBatches,
    /// A source waited long for sources it doesn't read (RS1e).
    RoundWait,
    /// An output's writer took most of the run (RS1e).
    WriterBound,
    /// A peak came close to its limit without passing it (RS1e).
    NearLimit,
}

impl HintId {
    /// Every id, in the order the hints are listed.
    pub const ALL: [Self; 8] = [
        Self::SourceReadsMore,
        Self::CombineSpilled,
        Self::SlowFirstRow,
        Self::LoadBound,
        Self::ManyBatches,
        Self::RoundWait,
        Self::WriterBound,
        Self::NearLimit,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceReadsMore => "source-reads-more",
            Self::CombineSpilled => "combine-spilled",
            Self::SlowFirstRow => "slow-first-row",
            Self::LoadBound => "load-bound",
            Self::ManyBatches => "many-batches",
            Self::RoundWait => "round-wait",
            Self::WriterBound => "writer-bound",
            Self::NearLimit => "near-limit",
        }
    }
}

impl fmt::Display for HintId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `quiet_hints` entry that names no hint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a hint's id; the ids are {ids}", ids = known_ids())]
pub struct UnknownHint(pub String);

/// `a, b and c`.
fn known_ids() -> String {
    let ids: Vec<&str> = HintId::ALL.iter().map(|id| id.as_str()).collect();
    match ids.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

impl FromStr for HintId {
    type Err = UnknownHint;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|id| id.as_str() == text)
            .ok_or_else(|| UnknownHint(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_every_id_and_refuses_others() {
        for id in HintId::ALL {
            assert_eq!(id.as_str().parse::<HintId>(), Ok(id));
        }
        assert_eq!(
            "source-reads-more-rows"
                .parse::<HintId>()
                .unwrap_err()
                .to_string(),
            "\"source-reads-more-rows\" isn't a hint's id; the ids are source-reads-more, \
             combine-spilled, slow-first-row, load-bound, many-batches, round-wait, writer-bound \
             and near-limit"
        );
    }
}
