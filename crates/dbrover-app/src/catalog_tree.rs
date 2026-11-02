//! What the explorer knows about one data source's catalog: which nodes are expanded, which
//! children have been loaded, and, for a level the driver pages, where it continues and the
//! filter the server applied. Pure state; the explorer draws it and the app turns its "needs
//! loading" answers into engine commands.

use std::collections::{HashMap, HashSet};

use dbrover_core::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage, ChildrenQuery, PageToken,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CatalogTree {
    expanded: HashSet<CatalogPath>,
    children: HashMap<CatalogPath, Children>,
    /// The levels whose first page wasn't all of them, or that the server filtered.
    paged: HashMap<CatalogPath, Paging>,
    /// The search of the whole catalog for the explorer's filter, on the server (C0g).
    search: Search,
}

/// Where the search of the whole catalog for the explorer's filter is.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Search {
    /// No filter, or nothing asked yet.
    #[default]
    Idle,
    /// The driver can't search: the explorer filters what it has loaded, and asks no more.
    Unsupported,
    Searching(String),
    Found {
        text: String,
        matches: CatalogMatches,
    },
    Failed {
        text: String,
        error: String,
    },
}

/// A level the server pages and filters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paging {
    /// The filter its nodes were asked for with.
    filter: Option<String>,
    rest: Rest,
}

/// What is left of a paged level to load.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rest {
    Nothing,
    /// The pages after this token, not asked for yet; with why the last try failed, if it did.
    After(PageToken, Option<String>),
    /// The page after this token, on its way.
    Loading(PageToken),
}

/// What the explorer shows at the end of a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelEnd<'a> {
    /// The level is whole.
    Complete,
    /// There is more: **Load more…**, after why the last try failed, if it did.
    More(Option<&'a str>),
    LoadingMore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Children {
    Loading,
    Loaded(Vec<CatalogNode>),
    Failed(String),
}

impl CatalogTree {
    pub fn is_expanded(&self, path: &CatalogPath) -> bool {
        self.expanded.contains(path)
    }

    pub fn children(&self, path: &CatalogPath) -> Option<&Children> {
        self.children.get(path)
    }

    /// Expands `path`. Returns `true` if its children must be requested: they were never
    /// loaded, or the last attempt failed.
    #[must_use]
    pub fn expand(&mut self, path: &CatalogPath) -> bool {
        self.expanded.insert(path.clone());
        match self.children.get(path) {
            Some(Children::Loading | Children::Loaded(_)) => false,
            Some(Children::Failed(_)) | None => {
                self.children.insert(path.clone(), Children::Loading);
                true
            }
        }
    }

    /// Asks for `path`'s children without expanding it, as completion does to fill the cache.
    /// Returns `true` if they must be requested: nobody has asked for them before. A failed
    /// load isn't retried here; expanding the node in the explorer retries it.
    #[must_use]
    pub fn request(&mut self, path: &CatalogPath) -> bool {
        if self.children.contains_key(path) {
            return false;
        }
        self.children.insert(path.clone(), Children::Loading);
        true
    }

    /// The children of `path`, if they have been loaded.
    pub fn loaded_children(&self, path: &CatalogPath) -> Option<&[CatalogNode]> {
        match self.children.get(path)? {
            Children::Loaded(nodes) => Some(nodes),
            Children::Loading | Children::Failed(_) => None,
        }
    }

    pub fn collapse(&mut self, path: &CatalogPath) {
        self.expanded.remove(path);
    }

    /// Forgets everything loaded at and below `path` and collapses its descendants. Returns
    /// `true` if `path` itself is expanded, so its children must be requested again.
    #[must_use]
    pub fn refresh(&mut self, path: &CatalogPath) -> bool {
        self.children.retain(|loaded, _| !loaded.starts_with(path));
        self.paged.retain(|paged, _| !paged.starts_with(path));
        self.expanded
            .retain(|expanded| expanded == path || !expanded.starts_with(path));
        if self.is_expanded(path) {
            self.children.insert(path.clone(), Children::Loading);
            true
        } else {
            false
        }
    }

    /// Records a page of children that arrived from the engine, for `query`. Ignored unless it
    /// was being waited for, with the same filter, so a late answer can't resurrect a subtree
    /// that was reset in the meantime, nor one for a filter since replaced show.
    pub fn loaded(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
        result: &Result<ChildrenPage, String>,
    ) {
        let filter = self
            .paged
            .get(path)
            .and_then(|paging| paging.filter.as_ref());
        if filter != query.filter.as_ref() {
            return;
        }
        match &query.after {
            None => self.first_page_loaded(path, query, result),
            Some(after) => self.more_loaded(path, after, result),
        }
    }

    fn first_page_loaded(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
        result: &Result<ChildrenPage, String>,
    ) {
        let Some(children @ Children::Loading) = self.children.get_mut(path) else {
            return;
        };
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                *children = Children::Failed(error.clone());
                return;
            }
        };
        *children = Children::Loaded(page.nodes.clone());
        if page.next.is_none() && query.filter.is_none() {
            self.paged.remove(path);
            return;
        }
        let rest = page
            .next
            .clone()
            .map_or(Rest::Nothing, |next| Rest::After(next, None));
        self.paged.insert(
            path.clone(),
            Paging {
                filter: query.filter.clone(),
                rest,
            },
        );
    }

    fn more_loaded(
        &mut self,
        path: &CatalogPath,
        after: &PageToken,
        result: &Result<ChildrenPage, String>,
    ) {
        let Some(paging) = self.paged.get_mut(path) else {
            return;
        };
        if paging.rest != Rest::Loading(after.clone()) {
            return;
        }
        paging.rest = match result {
            Ok(page) => {
                if let Some(Children::Loaded(nodes)) = self.children.get_mut(path) {
                    nodes.extend(page.nodes.iter().cloned());
                }
                page.next
                    .clone()
                    .map_or(Rest::Nothing, |next| Rest::After(next, None))
            }
            Err(error) => Rest::After(after.clone(), Some(error.clone())),
        };
    }

    /// What follows the loaded children of `path`.
    pub fn level_end(&self, path: &CatalogPath) -> LevelEnd<'_> {
        match self.paged.get(path).map(|paging| &paging.rest) {
            None | Some(Rest::Nothing) => LevelEnd::Complete,
            Some(Rest::After(_, error)) => LevelEnd::More(error.as_deref()),
            Some(Rest::Loading(_)) => LevelEnd::LoadingMore,
        }
    }

    /// The query for the next page of `path`, marking it as loading, or `None` when there is
    /// no more or it is already on its way.
    #[must_use]
    pub fn load_more(&mut self, path: &CatalogPath) -> Option<ChildrenQuery> {
        let paging = self.paged.get_mut(path)?;
        let Rest::After(after, _) = &paging.rest else {
            return None;
        };
        let query = ChildrenQuery::first_page(paging.filter.clone()).after(after.clone());
        paging.rest = Rest::Loading(after.clone());
        Some(query)
    }

    /// Asks again, filtered by `filter` on the server, for every level the server pages and
    /// last filtered differently; a level loaded whole is filtered by the explorer alone.
    /// Returns the first pages to request.
    #[must_use]
    pub fn filter_paged_levels(
        &mut self,
        filter: Option<&str>,
    ) -> Vec<(CatalogPath, ChildrenQuery)> {
        let mut requests = Vec::new();
        for (path, paging) in &mut self.paged {
            if paging.filter.as_deref() == filter {
                continue;
            }
            paging.filter = filter.map(str::to_owned);
            paging.rest = Rest::Nothing;
            self.children.insert(path.clone(), Children::Loading);
            requests.push((
                path.clone(),
                ChildrenQuery::first_page(paging.filter.clone()),
            ));
        }
        requests
    }

    /// Records a whole level, as a driver that doesn't page answers, for tests.
    #[cfg(test)]
    pub fn loaded_whole(&mut self, path: &CatalogPath, result: Result<Vec<CatalogNode>, String>) {
        let page = result.map(|nodes| ChildrenPage { nodes, next: None });
        self.loaded(path, &ChildrenQuery::default(), &page);
    }

    /// Forgets everything, e.g. after the connection closed.
    pub fn reset(&mut self) {
        self.expanded.clear();
        self.children.clear();
        self.paged.clear();
        self.search = Search::default();
    }

    pub fn search(&self) -> &Search {
        &self.search
    }

    /// Starts searching for `filter`, unless that search is already on its way or done, or the
    /// driver can't search. Returns the search to send.
    #[must_use]
    pub fn search_for(&mut self, filter: Option<&str>) -> Option<CatalogSearch> {
        let Some(text) = filter else {
            if self.search != Search::Unsupported {
                self.search = Search::Idle;
            }
            return None;
        };
        let current = match &self.search {
            Search::Unsupported => return None,
            Search::Idle => None,
            Search::Searching(current)
            | Search::Found { text: current, .. }
            | Search::Failed { text: current, .. } => Some(current.as_str()),
        };
        if current == Some(text) {
            return None;
        }
        self.search = Search::Searching(text.to_owned());
        Some(CatalogSearch::new(text))
    }

    /// Records what a search found, unless the filter has changed since it was asked.
    pub fn searched(
        &mut self,
        search: &CatalogSearch,
        result: &Result<Option<CatalogMatches>, String>,
    ) {
        if self.search != Search::Searching(search.text.clone()) {
            return;
        }
        let text = search.text.clone();
        self.search = match result {
            Ok(Some(matches)) => Search::Found {
                text,
                matches: matches.clone(),
            },
            Ok(None) => Search::Unsupported,
            Err(error) => Search::Failed {
                text,
                error: error.clone(),
            },
        };
    }

    /// The matches of the last search that the tree doesn't already show: those whose level
    /// hasn't been loaded with them in it.
    pub fn found_elsewhere(&self) -> Vec<&CatalogNode> {
        let Search::Found { matches, .. } = &self.search else {
            return Vec::new();
        };
        matches
            .nodes
            .iter()
            .filter(|found| {
                let shown = found
                    .path
                    .parent()
                    .and_then(|parent| self.loaded_children(&parent))
                    .is_some_and(|level| level.iter().any(|node| node.path == found.path));
                !shown
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::NodeKind;

    use super::*;

    fn schema(name: &str) -> CatalogPath {
        CatalogPath::root().child(NodeKind::Schema, name)
    }

    fn schema_node(name: &str) -> CatalogNode {
        CatalogNode {
            path: schema(name),
            label: name.to_owned(),
            detail: None,
            has_children: true,
            has_definition: false,
            system: false,
        }
    }

    #[test]
    fn requests_children_once_and_again_after_a_failure() {
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();

        assert!(tree.expand(&root));
        assert!(!tree.expand(&root));
        tree.loaded_whole(&root, Err("timed out".to_owned()));
        tree.collapse(&root);

        assert!(tree.expand(&root));
    }

    #[test]
    fn refresh_forgets_the_subtree_and_reloads_only_if_expanded() {
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();
        assert!(tree.expand(&root));
        tree.loaded_whole(&root, Ok(vec![schema_node("main")]));
        assert!(tree.expand(&schema("main")));
        tree.loaded_whole(&schema("main"), Ok(vec![]));

        assert!(tree.refresh(&root));

        assert_eq!(tree.children(&root), Some(&Children::Loading));
        assert_eq!(tree.children(&schema("main")), None);
        assert!(!tree.is_expanded(&schema("main")));
        assert!(!tree.refresh(&schema("other")));
    }

    #[test]
    fn ignores_answers_nobody_is_waiting_for() {
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();
        assert!(tree.expand(&root));
        tree.reset();

        tree.loaded_whole(&root, Ok(vec![schema_node("main")]));

        assert_eq!(tree.children(&root), None);
    }

    #[test]
    fn requests_children_for_completion_once_without_expanding() {
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();

        assert!(tree.request(&root));
        assert!(!tree.request(&root));
        assert!(!tree.is_expanded(&root));
        assert_eq!(tree.loaded_children(&root), None);
        tree.loaded_whole(&root, Ok(vec![schema_node("main")]));

        assert_eq!(tree.loaded_children(&root).map(<[_]>::len), Some(1));
        assert!(!tree.expand(&root), "already loaded");
    }

    fn tables() -> CatalogPath {
        schema("main").child(NodeKind::Folder(dbrover_core::FolderKind::Tables), "tables")
    }

    fn table(name: &str) -> CatalogNode {
        CatalogNode {
            path: tables().child(NodeKind::Table, name),
            label: name.to_owned(),
            ..schema_node(name)
        }
    }

    fn labels(tree: &CatalogTree) -> Vec<String> {
        tree.loaded_children(&tables())
            .unwrap_or_default()
            .iter()
            .map(|node| node.label.clone())
            .collect()
    }

    fn page(names: &[&str], next: Option<&str>) -> Result<ChildrenPage, String> {
        Ok(ChildrenPage {
            nodes: names.iter().map(|name| table(name)).collect(),
            next: next.map(PageToken::new),
        })
    }

    #[test]
    fn loads_more_of_a_partial_level_once_at_a_time() {
        let mut tree = CatalogTree::default();
        let first = ChildrenQuery::default();
        assert!(tree.expand(&tables()));
        tree.loaded(&tables(), &first, &page(&["a", "b"], Some("b")));
        assert_eq!(tree.level_end(&tables()), LevelEnd::More(None));

        let more = tree.load_more(&tables()).unwrap();
        assert_eq!(more.after, Some(PageToken::new("b")));
        assert_eq!(tree.load_more(&tables()), None, "already on its way");
        assert_eq!(tree.level_end(&tables()), LevelEnd::LoadingMore);

        tree.loaded(&tables(), &more, &Err("lost".to_owned()));
        assert_eq!(tree.level_end(&tables()), LevelEnd::More(Some("lost")));
        let again = tree.load_more(&tables()).unwrap();
        assert_eq!(again, more);
        tree.loaded(&tables(), &again, &page(&["c"], None));

        assert_eq!(labels(&tree), ["a", "b", "c"]);
        assert_eq!(tree.level_end(&tables()), LevelEnd::Complete);
    }

    #[test]
    fn asks_the_server_to_filter_only_partial_levels_and_ignores_stale_answers() {
        let mut tree = CatalogTree::default();
        assert!(tree.expand(&schema("main")));
        tree.loaded_whole(&schema("main"), Ok(vec![]));
        assert!(tree.expand(&tables()));
        tree.loaded(
            &tables(),
            &ChildrenQuery::default(),
            &page(&["a", "b"], Some("b")),
        );
        let more = tree.load_more(&tables()).unwrap();

        let requests = tree.filter_paged_levels(Some("ord"));
        let filtered = ChildrenQuery::first_page(Some("ord".to_owned()));
        assert_eq!(
            requests,
            [(tables(), filtered.clone())],
            "a whole level isn't asked again"
        );
        assert!(tree.filter_paged_levels(Some("ord")).is_empty());

        tree.loaded(&tables(), &more, &page(&["c"], None));
        tree.loaded(&tables(), &ChildrenQuery::default(), &page(&["x"], None));
        assert_eq!(
            tree.children(&tables()),
            Some(&Children::Loading),
            "both were stale"
        );

        tree.loaded(&tables(), &filtered, &page(&["orders"], None));
        assert_eq!(labels(&tree), ["orders"]);
        assert_eq!(tree.level_end(&tables()), LevelEnd::Complete);

        let cleared = tree.filter_paged_levels(None);
        assert_eq!(cleared, [(tables(), ChildrenQuery::default())]);
        tree.loaded(&tables(), &ChildrenQuery::default(), &page(&["a"], None));
        assert!(
            tree.filter_paged_levels(Some("x")).is_empty(),
            "whole once unfiltered, so the explorer filters it"
        );
    }

    fn found_table(schema: &str, name: &str) -> CatalogNode {
        let parent = CatalogPath::root()
            .child(NodeKind::Schema, schema)
            .child(NodeKind::Folder(dbrover_core::FolderKind::Tables), "tables");
        CatalogNode {
            path: parent.child(NodeKind::Table, name),
            label: name.to_owned(),
            detail: Some(schema.to_owned()),
            has_children: true,
            has_definition: true,
            system: false,
        }
    }

    #[test]
    fn searches_once_per_filter_and_keeps_only_the_latest_answer() {
        let mut tree = CatalogTree::default();
        assert_eq!(tree.search_for(None), None);
        let first = tree.search_for(Some("ord")).unwrap();
        assert_eq!(tree.search_for(Some("ord")), None, "already on its way");
        let second = tree.search_for(Some("orde")).unwrap();
        let found = CatalogMatches {
            nodes: vec![found_table("sales", "orders")],
            more: false,
        };
        tree.searched(&first, &Ok(Some(found.clone())));
        assert_eq!(tree.search(), &Search::Searching("orde".to_owned()));
        tree.searched(&second, &Ok(Some(found)));
        assert_eq!(tree.found_elsewhere().len(), 1);
        assert_eq!(tree.search_for(Some("orde")), None, "already found");

        assert_eq!(tree.search_for(None), None);
        assert_eq!(tree.search(), &Search::Idle);
    }

    #[test]
    fn a_driver_that_cant_search_is_asked_no_more() {
        let mut tree = CatalogTree::default();
        let search = tree.search_for(Some("ord")).unwrap();
        tree.searched(&search, &Ok(None));
        assert_eq!(tree.search_for(Some("orders")), None);
        assert_eq!(tree.search_for(None), None);
        assert_eq!(tree.search(), &Search::Unsupported);
    }

    #[test]
    fn a_match_the_tree_already_shows_isnt_shown_again() {
        let mut tree = CatalogTree::default();
        let search = tree.search_for(Some("ord")).unwrap();
        let shown = found_table("main", "orders");
        let elsewhere = found_table("archive", "old_orders");
        tree.searched(
            &search,
            &Ok(Some(CatalogMatches {
                nodes: vec![shown.clone(), elsewhere.clone()],
                more: false,
            })),
        );
        let level = shown.path.parent().unwrap();
        assert!(tree.expand(&level));
        tree.loaded_whole(&level, Ok(vec![shown]));
        let found: Vec<&str> = tree
            .found_elsewhere()
            .iter()
            .map(|node| node.label.as_str())
            .collect();
        assert_eq!(found, ["old_orders"]);
    }
}
