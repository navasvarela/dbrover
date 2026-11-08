//! Where lines break and how far they are indented.
//!
//! Queries and data changes (statements starting with `SELECT`, `WITH`, `INSERT`, `UPDATE`,
//! `DELETE`, `VALUES`) are laid out clause by clause:
//! - each clause (`FROM`, `WHERE`, `JOIN …`, `ORDER BY`, …) starts a line;
//! - a clause listing several items (`SELECT a, b`) puts each on a line of its own, one level
//!   in; with a single item it stays on the clause's line;
//! - `WHERE` and `HAVING` start a line for each top-level `AND` and `OR`;
//! - a subquery is indented one level inside its parentheses, which close on a line of their
//!   own; other parentheses (function calls, `IN (…)` lists) stay on one line;
//! - a `CASE` with several branches puts each `WHEN` and `ELSE` on a line of its own.
//!
//! Other statements (DDL, PL/pgSQL blocks, `GRANT`, …) keep their line breaks as typed, with
//! runs of spaces collapsed, except that a `CREATE TABLE`'s columns go one per line and a
//! query after `AS` (`CREATE VIEW v AS SELECT …`) is laid out as above.

use std::ops::Range;

use super::{Gap, Item, Sep, Writer, removable};
use crate::lexer::TokenKind;

/// First words of statements laid out clause by clause. `replace` is SQLite's `REPLACE INTO`.
const QUERY_STARTS: &[&str] = &[
    "select", "with", "values", "insert", "update", "delete", "replace",
];
/// Words that may follow `INSERT` before the table: `INSERT OR REPLACE INTO` (SQLite).
const INSERT_WORDS: &[&str] = &[
    "or", "replace", "ignore", "abort", "fail", "rollback", "into",
];
/// Words that may come before `JOIN` in a join's phrase.
/// The last line are DuckDB's and ClickHouse's: `ASOF JOIN`, `POSITIONAL JOIN`, `ARRAY JOIN`,
/// `GLOBAL ANY LEFT JOIN`, `SEMI` and `ANTI` joins.
const JOIN_MODIFIERS: &[&str] = &[
    "left",
    "right",
    "full",
    "inner",
    "cross",
    "natural",
    "outer",
    "asof",
    "positional",
    "array",
    "global",
    "any",
    "all",
    "semi",
    "anti",
];
/// Words after `FOR` in a locking clause (`FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`).
const LOCKING_WORDS: &[&str] = &["update", "share", "no", "key"];
/// Words after which a query in another statement starts: `CREATE VIEW v AS SELECT`,
/// `EXPLAIN ANALYZE SELECT`, SQLite's `EXPLAIN QUERY PLAN SELECT`.
const QUERY_INTRODUCERS: &[&str] = &["as", "explain", "analyze", "verbose", "plan"];

/// A token, or a parenthesised group of them.
enum Node {
    Leaf(usize),
    Group {
        open: usize,
        children: Vec<Node>,
        /// `None` if the text ends before the group closes.
        close: Option<usize>,
    },
}

/// How a clause lays out what follows its keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClauseKind {
    /// Comma-separated items, one per line when there are several.
    List,
    /// Conditions, one line per top-level `AND` or `OR`.
    Condition,
    /// Everything on the clause's line.
    Inline,
}

/// A clause of a query: its keywords (`ORDER BY`) and what follows them, as ranges of the
/// nodes of its level.
struct Clause {
    kind: ClauseKind,
    phrase: Range<usize>,
    body: Range<usize>,
}

/// What clause detection needs to know about the words before.
#[derive(Default)]
struct ClauseState<'a> {
    /// Nothing but comments came before at this level.
    at_start: bool,
    /// The first word of the clause being read.
    current: Option<&'a str>,
    previous_word: Option<&'a str>,
    /// `UPDATE` came before at this level, so `SET` starts its assignments.
    seen_update: bool,
}

/// One item of a comma-separated list.
struct ListPart<'n> {
    element: &'n [Node],
    comma: Option<&'n Node>,
    /// Comments on the comma's line, which stay after it.
    trailing: &'n [Node],
}

pub(super) fn render(items: &[Item], out: &mut Writer) {
    let nodes = tree(items);
    let mut layout = Layout {
        items,
        out,
        cases: Vec::new(),
    };
    if layout.is_query(&nodes) {
        layout.block(&nodes, 0, Sep::Glue);
    } else {
        layout.plain(&nodes, Sep::Glue);
    }
}

/// Nests the tokens by parentheses and square brackets (`ARRAY[1, 2]`, `a[1:2]`), so commas
/// inside them don't split a list. A closing token without its opening one is an ordinary
/// token; an opening one without its closing one runs to the end.
fn tree(items: &[Item]) -> Vec<Node> {
    let mut parents: Vec<(usize, Vec<Node>)> = Vec::new();
    let mut current = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let closes_innermost = parents.last().is_some_and(|&(open, _)| {
            let closing = if items[open].is_punctuation("(") {
                ")"
            } else {
                "]"
            };
            item.is_punctuation(closing)
        });
        if item.is_punctuation("(") || item.is_punctuation("[") {
            parents.push((index, std::mem::take(&mut current)));
        } else if closes_innermost && let Some((open, parent)) = parents.pop() {
            let children = std::mem::replace(&mut current, parent);
            current.push(Node::Group {
                open,
                children,
                close: Some(index),
            });
        } else {
            current.push(Node::Leaf(index));
        }
    }
    while let Some((open, parent)) = parents.pop() {
        let children = std::mem::replace(&mut current, parent);
        current.push(Node::Group {
            open,
            children,
            close: None,
        });
    }
    current
}

struct Layout<'a> {
    items: &'a [Item],
    out: &'a mut Writer,
    /// For each `CASE` being written, the level of its `WHEN`s, or `None` for one that stays
    /// on one line.
    cases: Vec<Option<usize>>,
}

impl<'a> Layout<'a> {
    fn leaf(&self, node: &Node) -> Option<&'a Item> {
        match node {
            Node::Leaf(index) => self.items.get(*index),
            Node::Group { .. } => None,
        }
    }

    fn word(&self, node: &Node) -> Option<&'a str> {
        self.leaf(node).and_then(|item| item.word.as_deref())
    }

    fn first_item(&self, node: &Node) -> &'a Item {
        match node {
            Node::Leaf(index) | Node::Group { open: index, .. } => &self.items[*index],
        }
    }

    fn is_comment(&self, node: &Node) -> bool {
        self.leaf(node).is_some_and(Item::is_comment)
    }

    /// Whether the node before `index` is a `*`, as in `SELECT t.* EXCEPT (a)`.
    fn follows_star(&self, nodes: &[Node], index: usize) -> bool {
        index
            .checked_sub(1)
            .and_then(|before| nodes.get(before))
            .and_then(|node| self.leaf(node))
            .is_some_and(|item| item.is_punctuation("*"))
    }

    fn is_comma(&self, node: &Node) -> bool {
        self.leaf(node).is_some_and(|item| item.is_punctuation(","))
    }

    /// Whether the nodes are a query, possibly in parentheses.
    fn is_query(&self, nodes: &[Node]) -> bool {
        match nodes.iter().find(|node| !self.is_comment(node)) {
            Some(Node::Group { open, children, .. }) => {
                self.items[*open].is_punctuation("(") && self.is_query(children)
            }
            Some(node) => self
                .word(node)
                .is_some_and(|word| QUERY_STARTS.contains(&word)),
            None => false,
        }
    }

    fn node(&mut self, node: &Node, sep: Sep) {
        match node {
            Node::Leaf(index) => self.out.emit(&self.items[*index], sep),
            Node::Group {
                open,
                children,
                close,
            } => self.group(*open, children, *close, sep),
        }
    }

    /// The separator a token gets where the layout has nothing to say: none if it was typed
    /// against the token before, or may lose its space (`(`, `,`, `)`), else one space. A
    /// comma is always followed by a space.
    fn natural_sep(&self, node: &Node) -> Sep {
        let item = self.first_item(node);
        let previous = self.out.previous.as_ref();
        if previous.is_some_and(|previous| removable(previous, item)) {
            return Sep::Glue;
        }
        if item.spaced || previous.is_some_and(|previous| previous.is_punctuation(",")) {
            return Sep::Space;
        }
        match item.gap {
            Gap::None => Sep::Glue,
            Gap::Newline if item.is_comment() => Sep::Newline(self.out.line_level),
            Gap::Space | Gap::Newline => Sep::Space,
        }
    }

    // ---- Queries ----------------------------------------------------------------------

    /// Lays out a query at `level`, its first token separated by `first`.
    fn block(&mut self, nodes: &[Node], level: usize, first: Sep) {
        for (index, clause) in self.clauses(nodes).iter().enumerate() {
            let sep = if index == 0 {
                first
            } else {
                Sep::Newline(level)
            };
            self.clause(nodes, clause, level, sep);
        }
    }

    fn clauses(&self, nodes: &[Node]) -> Vec<Clause> {
        let mut starts: Vec<(usize, usize, ClauseKind)> = Vec::new();
        let mut state = ClauseState {
            at_start: true,
            ..ClauseState::default()
        };
        let mut index = 0;
        while index < nodes.len() {
            let mut length = 1;
            if let Some((phrase_length, kind)) = self.clause_at(nodes, index, &state) {
                starts.push((index, phrase_length, kind));
                state.current = self.word(&nodes[index]);
                length = phrase_length;
            }
            for node in &nodes[index..index + length] {
                if let Some(word) = self.word(node) {
                    state.seen_update |= word == "update";
                    state.previous_word = Some(word);
                }
                state.at_start &= self.is_comment(node);
            }
            index += length;
        }
        if starts.first().is_none_or(|&(start, _, _)| start > 0) {
            starts.insert(0, (0, 0, ClauseKind::Inline));
        }
        let ends = starts
            .iter()
            .skip(1)
            .map(|&(start, _, _)| start)
            .chain([nodes.len()]);
        starts
            .iter()
            .zip(ends)
            .map(|(&(start, length, kind), end)| Clause {
                kind,
                phrase: start..start + length,
                body: start + length..end,
            })
            .collect()
    }

    /// Whether a clause starts at `index`, and if so how many words its phrase has.
    fn clause_at(
        &self,
        nodes: &[Node],
        index: usize,
        state: &ClauseState<'_>,
    ) -> Option<(usize, ClauseKind)> {
        let word = self.word(&nodes[index])?;
        let next = |offset: usize| nodes.get(index + offset).and_then(|node| self.word(node));
        let with_following = |continuations: &[&str]| {
            let mut length = 1;
            while next(length).is_some_and(|word| continuations.contains(&word)) {
                length += 1;
            }
            length
        };
        let opens_statement = state.at_start || state.current == Some("with");
        let clause = match word {
            "select" => (with_following(&["distinct", "all"]), ClauseKind::List),
            "from" if state.previous_word != Some("distinct") => (1, ClauseKind::List),
            "where" | "having" => (1, ClauseKind::Condition),
            "window" | "returning" => (1, ClauseKind::List),
            "limit" | "offset" | "fetch" => (1, ClauseKind::Inline),
            "group" | "order" if next(1) == Some("by") => (2, ClauseKind::List),
            // GoogleSQL's `SELECT * EXCEPT (a)` leaves columns out, rather than rows.
            "except" if self.follows_star(nodes, index) => return None,
            "union" | "intersect" | "except" => {
                (with_following(&["all", "distinct"]), ClauseKind::Inline)
            }
            "values" if state.previous_word != Some("default") => (1, ClauseKind::List),
            "set" if state.seen_update => (1, ClauseKind::List),
            "with" if state.at_start => (with_following(&["recursive"]), ClauseKind::List),
            "insert" if opens_statement => (with_following(INSERT_WORDS), ClauseKind::Inline),
            "replace" if opens_statement => (with_following(&["into"]), ClauseKind::Inline),
            "update" if opens_statement => (1, ClauseKind::Inline),
            "delete" if opens_statement => (with_following(&["from"]), ClauseKind::Inline),
            "on" if next(1) == Some("conflict") => (2, ClauseKind::Inline),
            "for" if next(1).is_some_and(|word| LOCKING_WORDS.contains(&word)) => {
                (with_following(LOCKING_WORDS), ClauseKind::Inline)
            }
            "join" => (1, ClauseKind::Inline),
            _ if JOIN_MODIFIERS.contains(&word) => {
                let modifiers = (0..)
                    .take_while(|&offset| next(offset).is_some_and(|w| JOIN_MODIFIERS.contains(&w)))
                    .count();
                if next(modifiers) != Some("join") {
                    return None;
                }
                (modifiers + 1, ClauseKind::Inline)
            }
            _ => return None,
        };
        Some(clause)
    }

    fn clause(&mut self, nodes: &[Node], clause: &Clause, level: usize, sep: Sep) {
        let mut body = &nodes[clause.body.clone()];
        let mut lead = sep;
        for (index, node) in nodes[clause.phrase.clone()].iter().enumerate() {
            self.node(node, if index == 0 { sep } else { Sep::Space });
            lead = Sep::Space;
        }
        // `SELECT DISTINCT ON (a)` keeps its `ON (…)` on the clause's line.
        let distinct = clause
            .phrase
            .end
            .checked_sub(1)
            .filter(|&last| clause.phrase.contains(&last))
            .and_then(|last| self.word(&nodes[last]))
            == Some("distinct");
        if distinct
            && let [on, group @ Node::Group { .. }, rest @ ..] = body
            && self.word(on) == Some("on")
        {
            self.node(on, Sep::Space);
            let sep = self.natural_sep(group);
            self.node(group, sep);
            body = rest;
        }
        if self.word(&nodes[clause.phrase.start]) == Some("select") {
            body = self.select_top(body);
        }
        match clause.kind {
            ClauseKind::List => self.list(body, level, lead),
            ClauseKind::Condition => self.conditions(body, level, lead),
            ClauseKind::Inline => self.inline(body, lead),
        }
    }

    /// Transact-SQL's `SELECT TOP (n) [PERCENT] [WITH TIES]` keeps its row limit on the
    /// clause's line, ahead of the columns. Returns the rest of the clause.
    fn select_top<'n>(&mut self, body: &'n [Node]) -> &'n [Node] {
        let [top, count, rest @ ..] = body else {
            return body;
        };
        if self.word(top) != Some("top") {
            return body;
        }
        self.node(top, Sep::Space);
        let sep = self.natural_sep(count);
        self.node(count, sep);
        let mut rest = rest;
        for words in [&["percent"][..], &["with", "ties"]] {
            let matches = rest.len() >= words.len()
                && rest
                    .iter()
                    .zip(words)
                    .all(|(node, word)| self.word(node) == Some(*word));
            if matches {
                for node in &rest[..words.len()] {
                    self.node(node, Sep::Space);
                }
                rest = &rest[words.len()..];
            }
        }
        rest
    }

    fn list(&mut self, body: &[Node], level: usize, lead: Sep) {
        let parts = self.split_list(body);
        if parts.len() <= 1 {
            self.inline(body, lead);
            return;
        }
        self.list_parts(&parts, level + 1, |layout, element, sep| {
            layout.inline(element, sep);
        });
    }

    /// Writes each part on a line of its own at `level`, with `element` laying out each item.
    fn list_parts(
        &mut self,
        parts: &[ListPart<'_>],
        level: usize,
        mut element: impl FnMut(&mut Self, &[Node], Sep),
    ) {
        for part in parts {
            if !part.element.is_empty() {
                element(self, part.element, Sep::Newline(level));
            }
            if let Some(comma) = part.comma {
                let sep = if part.element.is_empty() {
                    Sep::Newline(level)
                } else {
                    Sep::Glue
                };
                self.node(comma, sep);
            }
            for comment in part.trailing {
                self.node(comment, Sep::Space);
            }
        }
    }

    fn split_list<'n>(&self, body: &'n [Node]) -> Vec<ListPart<'n>> {
        let mut parts = Vec::new();
        let mut start = 0;
        let mut index = 0;
        while index < body.len() {
            if !self.is_comma(&body[index]) {
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while body
                .get(end)
                .and_then(|node| self.leaf(node))
                .is_some_and(|item| item.is_comment() && item.gap != Gap::Newline)
            {
                end += 1;
            }
            parts.push(ListPart {
                element: &body[start..index],
                comma: Some(&body[index]),
                trailing: &body[index + 1..end],
            });
            start = end;
            index = end;
        }
        if start < body.len() || parts.is_empty() {
            parts.push(ListPart {
                element: &body[start..],
                comma: None,
                trailing: &[],
            });
        }
        parts
    }

    /// `WHERE a AND b OR c`: the first condition on the clause's line, each `AND` and `OR`
    /// starting a line one level in. The `AND` of a `BETWEEN`, and those inside a `CASE`,
    /// don't count.
    fn conditions(&mut self, body: &[Node], level: usize, lead: Sep) {
        let mut starts = vec![0];
        let mut between = false;
        let mut case_depth = 0usize;
        for (index, node) in body.iter().enumerate() {
            match self.word(node) {
                Some("between") => between = true,
                Some("case") => case_depth += 1,
                Some("end") => case_depth = case_depth.saturating_sub(1),
                Some("and") if between => between = false,
                Some("and" | "or") if case_depth == 0 && index > 0 => starts.push(index),
                _ => {}
            }
        }
        let ends = starts.iter().skip(1).copied().chain([body.len()]);
        for (segment, (start, end)) in starts.iter().copied().zip(ends).enumerate() {
            let sep = if segment == 0 {
                lead
            } else {
                Sep::Newline(level + 1)
            };
            self.inline(&body[start..end], sep);
        }
    }

    /// Writes the nodes on the current line, but for `CASE` branches and comments.
    fn inline(&mut self, nodes: &[Node], lead: Sep) {
        for (index, node) in nodes.iter().enumerate() {
            let natural = if index == 0 {
                lead
            } else {
                self.natural_sep(node)
            };
            let sep = self.case_sep(nodes, index, natural);
            self.node(node, sep);
        }
    }

    fn case_sep(&mut self, nodes: &[Node], index: usize, natural: Sep) -> Sep {
        match self.word(&nodes[index]) {
            Some("case") => {
                let level = match natural {
                    Sep::Newline(level) => level,
                    _ => self.out.line_level,
                } + 1;
                let branches = self.has_several_branches(&nodes[index + 1..]);
                self.cases.push(branches.then_some(level));
                natural
            }
            Some("when" | "else") => match self.cases.last() {
                Some(Some(level)) => Sep::Newline(*level),
                _ => natural,
            },
            Some("end") => match self.cases.pop() {
                Some(Some(level)) => Sep::Newline(level.saturating_sub(1)),
                _ => natural,
            },
            _ => natural,
        }
    }

    /// Whether the `CASE` whose remaining tokens are `rest` has more than one `WHEN` or
    /// `ELSE`.
    fn has_several_branches(&self, rest: &[Node]) -> bool {
        let mut depth = 0usize;
        let mut branches = 0;
        for node in rest {
            match self.word(node) {
                Some("case") => depth += 1,
                Some("end") if depth == 0 => break,
                Some("end") => depth -= 1,
                Some("when" | "else") if depth == 0 => branches += 1,
                _ => {}
            }
        }
        branches > 1
    }

    fn group(&mut self, open: usize, children: &[Node], close: Option<usize>, sep: Sep) {
        self.out.emit(&self.items[open], sep);
        let level = self.out.line_level;
        if self.items[open].is_punctuation("(") && self.is_query(children) {
            self.block(children, level + 1, Sep::Newline(level + 1));
            if let Some(close) = close {
                self.out.emit(&self.items[close], Sep::Newline(level));
            }
        } else {
            self.inline(children, Sep::Glue);
            if let Some(close) = close {
                let sep = self.natural_sep(&Node::Leaf(close));
                self.out.emit(&self.items[close], sep);
            }
        }
    }

    // ---- Other statements -------------------------------------------------------------

    /// Keeps line breaks as typed, but lays out a query that follows `AS` or `EXPLAIN`, and
    /// a `CREATE TABLE`'s definitions.
    fn plain(&mut self, nodes: &[Node], lead: Sep) {
        for (index, node) in nodes.iter().enumerate() {
            if index > 0 && self.starts_nested_query(nodes, index) {
                let level = self.out.line_level;
                let sep = if self.previous_word(nodes, index) == Some("as") {
                    Sep::Newline(level)
                } else {
                    Sep::Space
                };
                self.block(&nodes[index..], level, sep);
                return;
            }
            let sep = if index == 0 {
                lead
            } else {
                self.plain_sep(node)
            };
            match node {
                Node::Leaf(item) => self.out.emit(&self.items[*item], sep),
                Node::Group {
                    open,
                    children,
                    close,
                } if self.is_definition_list(nodes, index) => {
                    self.definition_list(*open, children, *close, sep);
                }
                Node::Group {
                    open,
                    children,
                    close,
                } if self.items[*open].is_punctuation("(") && self.is_query(children) => {
                    self.group(*open, children, *close, sep);
                }
                Node::Group {
                    open,
                    children,
                    close,
                } => {
                    self.out.emit(&self.items[*open], sep);
                    self.plain(children, Sep::Glue);
                    if let Some(close) = close {
                        let sep = self.plain_sep(&Node::Leaf(*close));
                        self.out.emit(&self.items[*close], sep);
                    }
                }
            }
        }
    }

    fn plain_sep(&self, node: &Node) -> Sep {
        let item = self.first_item(node);
        match item.gap {
            Gap::Newline => Sep::AsTyped,
            Gap::Space | Gap::None => self.natural_sep(node),
        }
    }

    fn previous_word(&self, nodes: &[Node], index: usize) -> Option<&'a str> {
        nodes[..index]
            .iter()
            .rev()
            .find(|node| !self.is_comment(node))
            .and_then(|node| self.word(node))
    }

    fn starts_nested_query(&self, nodes: &[Node], index: usize) -> bool {
        let starts_query = self
            .word(&nodes[index])
            .is_some_and(|word| QUERY_STARTS.contains(&word));
        let introduced = match nodes[..index]
            .iter()
            .rev()
            .find(|node| !self.is_comment(node))
        {
            Some(Node::Group { .. }) => self.word(&nodes[0]) == Some("explain"),
            Some(node) => self
                .word(node)
                .is_some_and(|word| QUERY_INTRODUCERS.contains(&word)),
            None => false,
        };
        starts_query && introduced
    }

    /// The parentheses after the table's name in `CREATE TABLE name (…)`.
    fn is_definition_list(&self, nodes: &[Node], index: usize) -> bool {
        let before = &nodes[..index];
        let creates_table = self.word(&nodes[0]) == Some("create")
            && before.iter().any(|node| self.word(node) == Some("table"));
        let first_group = !before.iter().any(|node| matches!(node, Node::Group { .. }));
        let after_name = before
            .last()
            .and_then(|node| self.leaf(node))
            .is_some_and(|item| matches!(item.kind, TokenKind::Word | TokenKind::QuotedIdentifier));
        creates_table && first_group && after_name
    }

    fn definition_list(&mut self, open: usize, children: &[Node], close: Option<usize>, sep: Sep) {
        self.out.emit(&self.items[open], sep);
        let level = self.out.line_level;
        let parts = self.split_list(children);
        self.list_parts(&parts, level + 1, |layout, element, sep| {
            layout.plain(element, sep);
        });
        if let Some(close) = close {
            self.out.emit(&self.items[close], Sep::Newline(level));
        }
    }
}
