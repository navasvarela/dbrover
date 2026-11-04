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
}

impl HintId {
    pub const ALL: [Self; 2] = [Self::SourceReadsMore, Self::CombineSpilled];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceReadsMore => "source-reads-more",
            Self::CombineSpilled => "combine-spilled",
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

fn known_ids() -> String {
    HintId::ALL
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join(" and ")
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
            "\"source-reads-more-rows\" isn't a hint's id; the ids are source-reads-more and \
             combine-spilled"
        );
    }
}
