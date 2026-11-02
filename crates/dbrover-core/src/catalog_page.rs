//! Part of a catalog level at a time, filtered by the server: what a level of thousands of
//! tables, or millions of Redis keys, needs, since no tree can load it whole
//! (`docs/nosql.md`, N0a).

use crate::CatalogNode;

/// How many nodes of one level the explorer asks for at a time: every node of an ordinary
/// schema in one go, while a schema of 20,000 tables, or a Redis database of millions of keys,
/// never loads whole. Chosen, not measured; a page of this size draws without a stutter.
pub const CATALOG_PAGE_SIZE: usize = 1_000;

/// Where a partial level continues: the driver's own position (a name to list after, a
/// `SCAN` cursor), which nothing but the driver reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageToken(String);

impl PageToken {
    pub fn new(position: impl Into<String>) -> Self {
        Self(position.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which part of a catalog level to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildrenQuery {
    /// Only the nodes whose label contains this, ignoring case. `None` lists them all.
    pub filter: Option<String>,
    /// Where to continue, from the [`ChildrenPage::next`] of the page before; `None` starts
    /// at the beginning.
    pub after: Option<PageToken>,
    /// At most this many nodes; `None` for the whole level.
    pub limit: Option<usize>,
}

impl ChildrenQuery {
    /// The first page the explorer asks for, filtered by `filter` when there is one.
    pub fn first_page(filter: Option<String>) -> Self {
        Self {
            filter,
            after: None,
            limit: Some(CATALOG_PAGE_SIZE),
        }
    }

    /// The page after one that ended at `next`, with the same filter and size.
    pub fn after(&self, next: PageToken) -> Self {
        Self {
            after: Some(next),
            ..self.clone()
        }
    }

    /// Whether `label` passes the filter: contains it, ignoring case.
    pub fn matches(&self, label: &str) -> bool {
        self.filter
            .as_deref()
            .is_none_or(|filter| label.to_lowercase().contains(&filter.to_lowercase()))
    }
}

impl Default for ChildrenQuery {
    fn default() -> Self {
        Self::first_page(None)
    }
}

/// Part of a catalog level, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildrenPage {
    pub nodes: Vec<CatalogNode>,
    /// Where the next part starts, when there is more: the level is partial.
    pub next: Option<PageToken>,
}

impl ChildrenPage {
    /// The whole of a level a driver can't page, filtered here: what
    /// [`Connection::children_page`](crate::Connection::children_page) answers by default.
    pub fn whole(nodes: Vec<CatalogNode>, query: &ChildrenQuery) -> Self {
        Self {
            nodes: nodes
                .into_iter()
                .filter(|node| query.matches(&node.label))
                .collect(),
            next: None,
        }
    }
}

/// How many matches a search of a whole catalog returns at most: enough to find what was
/// meant, few enough to list at once. Typing more narrows a search that found more.
pub const CATALOG_SEARCH_LIMIT: usize = 100;

/// A search of a whole catalog by name, on the server, without loading its levels (C0g,
/// `docs/cloud-data.md`): every object whose name contains `text`, ignoring case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSearch {
    pub text: String,
    pub limit: usize,
}

impl CatalogSearch {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: CATALOG_SEARCH_LIMIT,
        }
    }
}

/// What a search found, each a node with its whole path, as [`Connection::children`]
/// (crate::Connection::children) would have listed it under its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMatches {
    pub nodes: Vec<CatalogNode>,
    /// There were more than the search's limit.
    pub more: bool,
}

impl CatalogMatches {
    /// `nodes`, read with one more than `limit` asked for, cut at the limit.
    pub fn cut(mut nodes: Vec<CatalogNode>, limit: usize) -> Self {
        let more = nodes.len() > limit;
        nodes.truncate(limit);
        Self { nodes, more }
    }
}

/// The SQL `LIKE` pattern for "contains `filter`", with `\` as its escape character, so that
/// `%` and `_` typed in the filter match themselves.
pub fn like_contains(filter: &str) -> String {
    let mut pattern = String::with_capacity(filter.len() + 2);
    pattern.push('%');
    for c in filter.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

#[cfg(test)]
mod tests {
    use crate::{CatalogPath, NodeKind};

    use super::*;

    fn table(name: &str) -> CatalogNode {
        CatalogNode {
            path: CatalogPath::root().child(NodeKind::Table, name),
            label: name.to_owned(),
            detail: None,
            has_children: true,
            has_definition: false,
            system: false,
        }
    }

    #[test]
    fn a_whole_level_is_filtered_here_and_complete() {
        let nodes = ["Orders", "order_items", "customers"].map(table).to_vec();
        let query = ChildrenQuery::first_page(Some("ORDER".to_owned()));
        let page = ChildrenPage::whole(nodes, &query);
        let labels: Vec<&str> = page.nodes.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(labels, ["Orders", "order_items"]);
        assert_eq!(page.next, None);
    }

    #[test]
    fn the_next_page_keeps_the_filter_and_size() {
        let first = ChildrenQuery::first_page(Some("a".to_owned()));
        let next = first.after(PageToken::new("m"));
        assert_eq!(next.filter, first.filter);
        assert_eq!(next.limit, Some(CATALOG_PAGE_SIZE));
        assert_eq!(next.after.as_ref().map(PageToken::as_str), Some("m"));
    }

    #[test]
    fn like_patterns_match_wildcards_as_themselves() {
        assert_eq!(like_contains("order"), "%order%");
        assert_eq!(like_contains("50%_off\\"), "%50\\%\\_off\\\\%");
    }
}
