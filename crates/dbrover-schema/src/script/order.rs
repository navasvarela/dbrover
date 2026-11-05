//! The order views are created and dropped in, from what each one reads.

use std::collections::BTreeSet;

/// `names` ordered so that each comes after the names it reads that are also in `names`:
/// the order to create them in. Reversed, it is the order to drop them in. Ties go by name, so
/// the order is the same every time; a cycle, which a database can't hold, is broken where it
/// is found rather than looped on.
pub(super) fn dependencies_first<'a>(
    names: &BTreeSet<&'a str>,
    reads: impl Fn(&str) -> Vec<&'a str>,
) -> Vec<&'a str> {
    let mut ordered = Vec::new();
    let mut visited = BTreeSet::new();
    for name in names {
        visit(name, names, &reads, &mut visited, &mut ordered);
    }
    ordered
}

fn visit<'a>(
    name: &'a str,
    names: &BTreeSet<&'a str>,
    reads: &impl Fn(&str) -> Vec<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    ordered: &mut Vec<&'a str>,
) {
    if !visited.insert(name) {
        return;
    }
    for read in reads(name) {
        if names.contains(read) {
            visit(read, names, reads, visited, ordered);
        }
    }
    ordered.push(name);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn a_view_comes_after_what_it_reads() {
        let reads: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
            ("a_report", vec!["m_totals", "orders"]),
            ("m_totals", vec!["z_orders"]),
            ("z_orders", vec!["orders"]),
            ("lonely", vec![]),
        ]);
        let names: BTreeSet<&str> = reads.keys().copied().collect();

        let ordered = dependencies_first(&names, |name| reads[name].clone());

        assert_eq!(ordered, ["z_orders", "m_totals", "a_report", "lonely"]);
    }

    #[test]
    fn a_cycle_ends() {
        let names = BTreeSet::from(["a", "b"]);

        let ordered = dependencies_first(&names, |name| match name {
            "a" => vec!["b"],
            _ => vec!["a"],
        });

        assert_eq!(ordered, ["b", "a"]);
    }
}
