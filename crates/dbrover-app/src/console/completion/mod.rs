//! What to suggest at the cursor: the names the clause calls for (see [`context`]), from
//! the cached catalog and from the statement itself (aliases, CTEs, subqueries), functions
//! and keywords, ranked by [`ranking`]. Pure; the catalog comes in through
//! [`CatalogSource`].

mod context;
mod derived;
mod ranking;

use std::ops::Range;

use dbrover_core::Quoting;
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, Token, TokenKind, is_keyword, keywords, tokenize};

use context::Context;
use derived::{DerivedKind, DerivedTable};

pub use ranking::RecentCompletions;

/// Keywords after which a table name follows.
const TABLE_INTRODUCERS: &[&str] = &["from", "join", "update", "into"];

/// What the catalog knows, as far as it has been loaded.
pub trait CatalogSource {
    /// Where unqualified names resolve: `public` on PostgreSQL, `main` on SQLite.
    fn default_schema(&self) -> &str;
    fn schemas(&self) -> Vec<String>;
    fn tables(&self) -> Vec<TableEntry>;
    /// `None` if the table's columns haven't been loaded.
    fn columns(&self, table: &TableEntry) -> Option<Vec<ColumnEntry>>;
    /// Whether every table and view of `schema` has been loaded, or with `None`, of every
    /// schema (and the list of schemas itself): only then is a name missing from
    /// [`CatalogSource::tables`] really missing.
    fn tables_loaded(&self, schema: Option<&str>) -> bool;
    /// The functions of the schemas whose functions have been loaded.
    fn functions(&self) -> Vec<FunctionEntry>;
}

/// A data source with nothing cached: completion offers keywords only.
pub struct NoCatalog;

impl CatalogSource for NoCatalog {
    fn default_schema(&self) -> &str {
        ""
    }

    fn schemas(&self) -> Vec<String> {
        Vec::new()
    }

    fn tables(&self) -> Vec<TableEntry> {
        Vec::new()
    }

    fn columns(&self, _: &TableEntry) -> Option<Vec<ColumnEntry>> {
        None
    }

    fn tables_loaded(&self, _: Option<&str>) -> bool {
        false
    }

    fn functions(&self) -> Vec<FunctionEntry> {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntry {
    pub schema: String,
    pub name: String,
    pub is_view: bool,
}

/// A function the database has, as the explorer lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionEntry {
    pub schema: String,
    pub name: String,
    /// The argument list as the explorer shows it, `(integer, integer)`, when it does.
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnEntry {
    pub name: String,
    /// The type, as the explorer shows it.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Column,
    Table,
    View,
    /// A CTE, or a subquery's alias.
    Derived,
    /// A table's alias in the statement.
    Alias,
    Schema,
    Function,
    Keyword,
    /// From `snippets.toml`: its label is the trigger, its insert the body.
    Snippet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: CandidateKind,
    /// What the list shows.
    pub label: String,
    /// Shown dimmed after the label: a column's table and type, a table's schema.
    pub detail: Option<String>,
    /// What replaces the typed prefix.
    pub insert: String,
    /// How many bytes at the end of `insert` the cursor stays before: inside a function's
    /// parentheses.
    pub cursor_back: usize,
}

/// Everything completion looks at.
pub struct CompletionInput<'a> {
    pub point: &'a CompletionPoint,
    /// The statement the cursor is in.
    pub statement: &'a str,
    /// Where in `statement` the typed prefix starts.
    pub prefix_start: usize,
    pub dialect: Dialect,
    pub catalog: &'a dyn CatalogSource,
    pub recent: &'a RecentCompletions,
}

/// Where completion happens: the word being typed and what qualifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionPoint {
    /// The typed prefix's bytes, which an accepted candidate replaces.
    pub replace: Range<usize>,
    pub prefix: String,
    /// The name before a `.` directly before the prefix: a schema, table or alias.
    pub qualifier: Option<String>,
}

/// A table the statement names, as `[schema.]table [[AS] alias]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub table: String,
    pub alias: Option<String>,
    /// The bytes of `[schema.]table` in the statement.
    pub range: Range<usize>,
}

/// The completion point for a cursor at byte `cursor` of `text`, or `None` where nothing
/// should be suggested: inside a string, comment or quoted identifier, or after a number.
pub fn completion_point(
    text: &str,
    cursor: usize,
    language: QueryLanguage,
) -> Option<CompletionPoint> {
    let before = text.get(..cursor)?;
    let tokens = language.tokens(before);
    let last = tokens.last();
    let (replace, prefix_tokens) = match last {
        Some(token) if token.kind == TokenKind::Word => (token.range.clone(), tokens.len() - 1),
        Some(token)
            if matches!(
                token.kind,
                TokenKind::String
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
                    | TokenKind::QuotedIdentifier
                    | TokenKind::Number
            ) =>
        {
            return None;
        }
        _ => (cursor..cursor, tokens.len()),
    };
    let qualifier = match tokens[..prefix_tokens] {
        [.., ref name, ref dot] if is_dot(before, dot) => identifier(before, name),
        _ => None,
    };
    Some(CompletionPoint {
        prefix: before[replace.clone()].to_owned(),
        replace,
        qualifier,
    })
}

fn is_dot(text: &str, token: &Token) -> bool {
    token.kind == TokenKind::Punctuation && &text[token.range.clone()] == "."
}

/// An identifier's name: a word as written, a quoted identifier unquoted.
fn identifier(text: &str, token: &Token) -> Option<String> {
    let written = &text[token.range.clone()];
    match token.kind {
        TokenKind::Word => Some(written.to_owned()),
        TokenKind::QuotedIdentifier => {
            let inner = written.get(1..written.len().checked_sub(1)?)?;
            Some(inner.replace("\"\"", "\""))
        }
        _ => None,
    }
}

/// The tables `statement` names after `FROM` (including comma-separated lists), `JOIN`,
/// `UPDATE` and `INTO`, with their aliases.
pub fn table_references(statement: &str, dialect: Dialect) -> Vec<TableRef> {
    let tokens: Vec<Token> = tokenize(statement, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let word = |index: usize| {
        tokens
            .get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| statement[token.range.clone()].to_ascii_lowercase())
    };
    let mut references = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        // `DO UPDATE SET` (an upsert) and `FOR [NO KEY] UPDATE` (locking) name no table.
        let not_a_statement = index
            .checked_sub(1)
            .and_then(word)
            .is_some_and(|before| matches!(before.as_str(), "do" | "for" | "key"));
        let introduces = word(index).is_some_and(|w| TABLE_INTRODUCERS.contains(&w.as_str()))
            && !(not_a_statement && word(index).as_deref() == Some("update"));
        index += 1;
        if !introduces {
            continue;
        }
        while let Some((reference, next)) = read_reference(statement, &tokens, index) {
            references.push(reference);
            index = next;
            let comma = tokens
                .get(index)
                .is_some_and(|token| &statement[token.range.clone()] == ",");
            if !comma {
                break;
            }
            index += 1;
        }
    }
    references
}

/// Reads `[schema.]table [[AS] alias]` at `index`; returns it and the index after it.
fn read_reference(text: &str, tokens: &[Token], index: usize) -> Option<(TableRef, usize)> {
    let first = identifier(text, tokens.get(index)?)?;
    let (schema, table, mut next) = match tokens.get(index + 1) {
        Some(dot) if is_dot(text, dot) => {
            let table = identifier(text, tokens.get(index + 2)?)?;
            (Some(first), table, index + 3)
        }
        _ => (None, first, index + 1),
    };
    let lowercase_word = |at: usize| {
        tokens
            .get(at)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| text[token.range.clone()].to_ascii_lowercase())
    };
    if lowercase_word(next).as_deref() == Some("as") {
        next += 1;
    }
    let alias = tokens
        .get(next)
        .and_then(|token| identifier(text, token))
        .filter(|name| !is_keyword(name));
    if alias.is_some() {
        next += 1;
    }
    let name_end = tokens[if schema.is_some() { index + 2 } else { index }]
        .range
        .end;
    Some((
        TableRef {
            schema,
            table,
            alias,
            range: tokens[index].range.start..name_end,
        },
        next,
    ))
}

/// The candidates at the cursor, best first. With a qualifier (`x.`): the columns of the
/// table, alias, CTE or subquery it names, or the tables of the schema it names. Without one,
/// what the clause calls for (see [`Context`]).
pub fn candidates(input: &CompletionInput<'_>) -> Vec<Candidate> {
    let references = table_references(input.statement, input.dialect);
    let derived = derived::derived_tables(input.statement, input.dialect, input.catalog);
    let names = Names {
        references: &references,
        derived: &derived,
        catalog: input.catalog,
        quoting: input.dialect.quoting(),
    };
    let found = match &input.point.qualifier {
        Some(qualifier) => names.qualified(qualifier),
        None => {
            let before = input
                .statement
                .get(..input.prefix_start)
                .unwrap_or(input.statement);
            let context = context::context_at(before, input.dialect);
            names.unqualified(&context, input)
        }
    };
    ranking::rank(found, &input.point.prefix, input.recent)
}

/// The names a statement can use: its tables, its own derived tables, and the catalog.
struct Names<'a> {
    references: &'a [TableRef],
    derived: &'a [DerivedTable],
    catalog: &'a dyn CatalogSource,
    quoting: Quoting,
}

impl Names<'_> {
    fn qualified(&self, qualifier: &str) -> Vec<Candidate> {
        let referenced = self.references.iter().find(|reference| {
            reference
                .alias
                .as_deref()
                .unwrap_or(&reference.table)
                .eq_ignore_ascii_case(qualifier)
        });
        if let Some(columns) = match referenced {
            Some(reference) => self.columns_of(reference.schema.as_deref(), &reference.table),
            None => self.columns_of(None, qualifier),
        } {
            return columns;
        }
        let tables = self.catalog.tables();
        let schema_named = self
            .catalog
            .schemas()
            .into_iter()
            .find(|schema| schema.eq_ignore_ascii_case(qualifier));
        match schema_named {
            Some(schema) => tables
                .iter()
                .filter(|table| table.schema == schema)
                .map(|table| table_candidate(table, self.quoting.identifier_if_needed(&table.name)))
                .collect(),
            None => Vec::new(),
        }
    }

    /// The columns of a derived table (CTE or subquery alias) or catalog table of that name,
    /// `None` if there is neither.
    fn columns_of(&self, schema: Option<&str>, table: &str) -> Option<Vec<Candidate>> {
        let derived = schema.is_none().then(|| {
            self.derived
                .iter()
                .find(|derived| derived.name.eq_ignore_ascii_case(table))
        });
        if let Some(Some(derived)) = derived {
            return Some(derived_columns(derived, self.quoting));
        }
        let tables = self.catalog.tables();
        let entry = resolve(schema, table, &tables, self.catalog)?;
        Some(column_candidates(entry, self.catalog, self.quoting))
    }

    fn unqualified(&self, context: &Context, input: &CompletionInput<'_>) -> Vec<Candidate> {
        let prefix = &input.point.prefix;
        let mut found = Vec::new();
        match context {
            Context::InsertColumns { schema, table } => {
                found.extend(
                    self.columns_of(schema.as_deref(), table)
                        .unwrap_or_default(),
                );
            }
            Context::AfterTable => found.extend(keyword_candidates(prefix)),
            Context::Tables => {
                found.extend(self.cte_candidates());
                found.extend(self.table_candidates());
                found.extend(self.schema_candidates());
                found.extend(keyword_candidates(prefix));
            }
            Context::Columns => {
                found.extend(self.statement_columns());
                found.extend(self.alias_candidates());
                found.extend(function_candidates(input));
                found.extend(keyword_candidates(prefix));
            }
            Context::Any => {
                found.extend(self.statement_columns());
                found.extend(self.cte_candidates());
                found.extend(self.table_candidates());
                found.extend(self.schema_candidates());
                found.extend(function_candidates(input));
                found.extend(keyword_candidates(prefix));
            }
        }
        found
    }

    /// The columns of every table the statement names, and of its subqueries, each name once.
    fn statement_columns(&self) -> Vec<Candidate> {
        let mut found: Vec<Candidate> = Vec::new();
        let referenced = self
            .references
            .iter()
            .filter_map(|reference| self.columns_of(reference.schema.as_deref(), &reference.table));
        let subqueries = self
            .derived
            .iter()
            .filter(|derived| derived.kind == DerivedKind::Subquery)
            .map(|derived| derived_columns(derived, self.quoting));
        for column in referenced.chain(subqueries).flatten() {
            if !found.iter().any(|seen| seen.label == column.label) {
                found.push(column);
            }
        }
        found
    }

    fn alias_candidates(&self) -> Vec<Candidate> {
        let aliases = self.references.iter().filter_map(|reference| {
            let alias = reference.alias.as_ref()?;
            Some((alias.clone(), format!("alias · {}", reference.table)))
        });
        let subqueries = self
            .derived
            .iter()
            .filter(|derived| derived.kind == DerivedKind::Subquery)
            .map(|derived| (derived.name.clone(), "subquery".to_owned()));
        aliases
            .chain(subqueries)
            .map(|(alias, detail)| Candidate {
                kind: CandidateKind::Alias,
                insert: self.quoting.identifier_if_needed(&alias),
                label: alias,
                detail: Some(detail),
                cursor_back: 0,
            })
            .collect()
    }

    fn cte_candidates(&self) -> Vec<Candidate> {
        self.derived
            .iter()
            .filter(|derived| derived.kind == DerivedKind::Cte)
            .map(|cte| Candidate {
                kind: CandidateKind::Derived,
                insert: self.quoting.identifier_if_needed(&cte.name),
                label: cte.name.clone(),
                detail: Some("CTE".to_owned()),
                cursor_back: 0,
            })
            .collect()
    }

    /// Tables and views, schema-qualified outside the default schema.
    fn table_candidates(&self) -> Vec<Candidate> {
        let default_schema = self.catalog.default_schema();
        self.catalog
            .tables()
            .iter()
            .map(|table| {
                let insert = if table.schema == default_schema {
                    self.quoting.identifier_if_needed(&table.name)
                } else {
                    format!(
                        "{}.{}",
                        self.quoting.identifier_if_needed(&table.schema),
                        self.quoting.identifier_if_needed(&table.name)
                    )
                };
                table_candidate(table, insert)
            })
            .collect()
    }

    fn schema_candidates(&self) -> Vec<Candidate> {
        self.catalog
            .schemas()
            .into_iter()
            .map(|schema| Candidate {
                kind: CandidateKind::Schema,
                insert: self.quoting.identifier_if_needed(&schema),
                label: schema,
                detail: Some("schema".to_owned()),
                cursor_back: 0,
            })
            .collect()
    }
}

/// Whether completed words follow the case being typed: upper case if the prefix has any.
fn typed_in_upper_case(prefix: &str) -> bool {
    prefix.chars().any(|c| c.is_ascii_uppercase())
}

/// The candidates for a language without a SQL dialect: its own words, each with what the
/// language says of it (a Redis command's or a MongoDB method's arguments) or as a keyword,
/// and the catalog's `names` its commands take, such as an Elasticsearch cluster's indices;
/// best first.
pub fn word_candidates(
    prefix: &str,
    language: QueryLanguage,
    names: &[String],
    recent: &RecentCompletions,
) -> Vec<Candidate> {
    let words = language.completion_words();
    let mut candidates = if language.completion_keeps_case() {
        listed_words(prefix, words)
    } else {
        language_words(prefix, words)
    };
    for candidate in &mut candidates {
        if let Some(detail) = language.completion_detail(&candidate.insert) {
            candidate.detail = Some(detail.to_owned());
        }
    }
    candidates.extend(names.iter().map(|name| Candidate {
        kind: CandidateKind::Table,
        label: name.clone(),
        detail: None,
        insert: name.clone(),
        cursor_back: 0,
    }));
    ranking::rank(candidates, prefix, recent)
}

/// Keywords, in the case being typed. None for an empty prefix: every keyword matches it,
/// which would bury the names.
fn keyword_candidates(prefix: &str) -> Vec<Candidate> {
    language_words(prefix, keywords())
}

/// `words`, in the case being typed. None for an empty prefix, as for keywords.
fn language_words(prefix: &str, words: &[&str]) -> Vec<Candidate> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let upper = typed_in_upper_case(prefix);
    words
        .iter()
        .map(|keyword| {
            let insert = if upper {
                keyword.to_ascii_uppercase()
            } else {
                (*keyword).to_owned()
            };
            Candidate {
                kind: CandidateKind::Keyword,
                label: insert.clone(),
                detail: Some("keyword".to_owned()),
                insert,
                cursor_back: 0,
            }
        })
        .collect()
}

/// `words` as listed, for a language whose words are case-sensitive. None for an empty
/// prefix, as for keywords.
fn listed_words(prefix: &str, words: &[&str]) -> Vec<Candidate> {
    if prefix.is_empty() {
        return Vec::new();
    }
    words
        .iter()
        .map(|word| Candidate {
            kind: CandidateKind::Keyword,
            label: (*word).to_owned(),
            detail: Some("keyword".to_owned()),
            insert: (*word).to_owned(),
            cursor_back: 0,
        })
        .collect()
}

/// The database's functions the catalog has loaded, then the dialect's built-ins, each
/// inserted with its parentheses and the cursor between them. None for an empty prefix.
fn function_candidates(input: &CompletionInput<'_>) -> Vec<Candidate> {
    let prefix = &input.point.prefix;
    if prefix.is_empty() {
        return Vec::new();
    }
    let call = |name: &str| format!("{name}()");
    let mut found: Vec<Candidate> = input
        .catalog
        .functions()
        .into_iter()
        .map(|function| Candidate {
            kind: CandidateKind::Function,
            insert: call(&input.dialect.quoting().identifier_if_needed(&function.name)),
            detail: Some(format!(
                "function · {}{}",
                function.schema,
                function.arguments.as_deref().unwrap_or("()")
            )),
            label: function.name,
            cursor_back: 1,
        })
        .collect();
    let upper = typed_in_upper_case(prefix) && input.dialect.names_ignore_case();
    for builtin in input.dialect.builtin_functions() {
        if found
            .iter()
            .any(|seen| seen.label.eq_ignore_ascii_case(builtin))
        {
            continue;
        }
        let name = if upper {
            builtin.to_ascii_uppercase()
        } else {
            (*builtin).to_owned()
        };
        found.push(Candidate {
            kind: CandidateKind::Function,
            insert: call(&name),
            label: name,
            detail: Some("function".to_owned()),
            cursor_back: 1,
        });
    }
    found
}

fn derived_columns(derived: &DerivedTable, quoting: Quoting) -> Vec<Candidate> {
    let detail = match derived.kind {
        DerivedKind::Cte => format!("{} · CTE", derived.name),
        DerivedKind::Subquery => format!("{} · subquery", derived.name),
    };
    derived
        .columns
        .iter()
        .map(|column| Candidate {
            kind: CandidateKind::Column,
            insert: quoting.identifier_if_needed(column),
            label: column.clone(),
            detail: Some(detail.clone()),
            cursor_back: 0,
        })
        .collect()
}

/// The catalog's table for a reference: in its schema if it names one, else in the default
/// schema, else in whichever schema has it.
pub(super) fn resolve<'a>(
    schema: Option<&str>,
    name: &str,
    tables: &'a [TableEntry],
    catalog: &dyn CatalogSource,
) -> Option<&'a TableEntry> {
    let named = |table: &&TableEntry| table.name.eq_ignore_ascii_case(name);
    match schema {
        Some(schema) => tables
            .iter()
            .filter(named)
            .find(|table| table.schema.eq_ignore_ascii_case(schema)),
        None => tables
            .iter()
            .filter(named)
            .find(|table| table.schema == catalog.default_schema())
            .or_else(|| tables.iter().find(named)),
    }
}

fn column_candidates(
    table: &TableEntry,
    catalog: &dyn CatalogSource,
    quoting: Quoting,
) -> Vec<Candidate> {
    catalog
        .columns(table)
        .unwrap_or_default()
        .into_iter()
        .map(|column| Candidate {
            kind: CandidateKind::Column,
            insert: quoting.identifier_if_needed(&column.name),
            detail: Some(match column.detail {
                Some(detail) => format!("{detail} · {}", table.name),
                None => table.name.clone(),
            }),
            label: column.name,
            cursor_back: 0,
        })
        .collect()
}

fn table_candidate(table: &TableEntry, insert: String) -> Candidate {
    let (kind, word) = if table.is_view {
        (CandidateKind::View, "view")
    } else {
        (CandidateKind::Table, "table")
    };
    Candidate {
        kind,
        label: table.name.clone(),
        detail: Some(format!("{word} · {}", table.schema)),
        insert,
        cursor_back: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCatalog;

    impl CatalogSource for FakeCatalog {
        fn default_schema(&self) -> &str {
            "public"
        }

        fn schemas(&self) -> Vec<String> {
            vec!["public".to_owned(), "Sales".to_owned()]
        }

        fn tables(&self) -> Vec<TableEntry> {
            let table = |schema: &str, name: &str, is_view| TableEntry {
                schema: schema.to_owned(),
                name: name.to_owned(),
                is_view,
            };
            vec![
                table("public", "orders", false),
                table("public", "order_totals", true),
                table("Sales", "Order Items", false),
            ]
        }

        fn columns(&self, table: &TableEntry) -> Option<Vec<ColumnEntry>> {
            let column = |name: &str| ColumnEntry {
                name: name.to_owned(),
                detail: Some("integer".to_owned()),
            };
            match table.name.as_str() {
                "orders" => Some(vec![column("id"), column("customer_id")]),
                "Order Items" => Some(vec![column("order_id"), column("Quantity")]),
                _ => None,
            }
        }

        fn tables_loaded(&self, _: Option<&str>) -> bool {
            true
        }

        fn functions(&self) -> Vec<FunctionEntry> {
            vec![FunctionEntry {
                schema: "public".to_owned(),
                name: "order_total".to_owned(),
                arguments: Some("(integer)".to_owned()),
            }]
        }
    }

    fn point_at_end(text: &str) -> Option<CompletionPoint> {
        completion_point(text, text.len(), QueryLanguage::Sql(Dialect::Postgres))
    }

    /// The candidates for a cursor at `cursor` in `text`, which is one statement.
    fn suggest_at(text: &str, cursor: usize) -> Vec<Candidate> {
        let point = completion_point(text, cursor, QueryLanguage::Sql(Dialect::Postgres)).unwrap();
        candidates(&CompletionInput {
            point: &point,
            statement: text,
            prefix_start: point.replace.start,
            dialect: Dialect::Postgres,
            catalog: &FakeCatalog,
            recent: &RecentCompletions::default(),
        })
    }

    fn suggest(text: &str) -> Vec<(CandidateKind, String)> {
        suggest_at(text, text.len())
            .into_iter()
            .map(|candidate| (candidate.kind, candidate.insert))
            .collect()
    }

    #[test]
    fn finds_the_prefix_and_its_qualifier() {
        let cases = [
            ("select ord", "ord", None),
            ("select o.", "", Some("o")),
            ("select o.cu", "cu", Some("o")),
            ("select \"Sales\".Ord", "Ord", Some("Sales")),
            ("select ", "", None),
        ];
        for (text, prefix, qualifier) in cases {
            let point = point_at_end(text).unwrap();
            assert_eq!(point.prefix, prefix, "{text}");
            assert_eq!(point.qualifier.as_deref(), qualifier, "{text}");
            assert_eq!(&text[point.replace], prefix, "{text}");
        }
        for text in ["select 'ord", "-- ord", "select 12", "select \"ord\""] {
            assert_eq!(point_at_end(text), None, "{text}");
        }
    }

    #[test]
    fn reads_tables_and_aliases_from_the_statement() {
        let references = table_references(
            "SELECT * FROM orders o, \"Sales\".\"Order Items\" AS items \
             JOIN customers ON true WHERE x",
            Dialect::Postgres,
        );
        let read: Vec<(Option<&str>, &str, Option<&str>)> = references
            .iter()
            .map(|r| (r.schema.as_deref(), r.table.as_str(), r.alias.as_deref()))
            .collect();
        assert_eq!(
            read,
            [
                (None, "orders", Some("o")),
                (Some("Sales"), "Order Items", Some("items")),
                (None, "customers", None),
            ]
        );
    }

    #[test]
    fn suggests_columns_of_an_alias_or_table_after_a_dot() {
        // The cursor sits after `o.c`; the alias is defined later in the statement.
        let found: Vec<String> = suggest_at("select o.c from orders o", 10)
            .into_iter()
            .map(|candidate| candidate.insert)
            .collect();
        assert_eq!(found, ["customer_id"]);
        assert_eq!(
            suggest("select * from \"Sales\".\"Order Items\" i where i."),
            [
                (CandidateKind::Column, "order_id".to_owned()),
                (CandidateKind::Column, "\"Quantity\"".to_owned())
            ]
        );
    }

    #[test]
    fn suggests_a_schemas_tables_after_its_name() {
        assert_eq!(
            suggest("select * from sales."),
            [(CandidateKind::Table, "\"Order Items\"".to_owned())]
        );
    }

    #[test]
    fn suggests_columns_then_tables_schemas_and_keywords_matching_the_prefix() {
        assert_eq!(
            suggest("select * from orders where cus")[0],
            (CandidateKind::Column, "customer_id".to_owned())
        );
        assert_eq!(
            suggest("select * from Ord"),
            [
                (CandidateKind::Table, "orders".to_owned()),
                (CandidateKind::View, "order_totals".to_owned()),
                (CandidateKind::Table, "\"Sales\".\"Order Items\"".to_owned()),
                (CandidateKind::Keyword, "ORDER".to_owned()),
            ]
        );
        assert!(
            suggest("select * from ")
                .iter()
                .all(|(kind, _)| *kind != CandidateKind::Keyword)
        );
    }

    #[test]
    fn offers_what_the_clause_calls_for() {
        use CandidateKind::*;
        let kinds = |text: &str| -> Vec<CandidateKind> {
            let mut kinds: Vec<CandidateKind> =
                suggest(text).into_iter().map(|(kind, _)| kind).collect();
            kinds.dedup();
            kinds
        };
        assert_eq!(
            kinds("select * from orders o join "),
            [Table, View, Table, Schema]
        );
        assert_eq!(
            kinds("select * from orders o "),
            Vec::<CandidateKind>::new()
        );
        assert_eq!(kinds("select * from orders o wh"), [Keyword]);
        assert_eq!(
            suggest("insert into orders (cu"),
            [(Column, "customer_id".to_owned())]
        );
        assert_eq!(
            suggest("select * from orders where cid"),
            [(Column, "customer_id".to_owned())],
            "fuzzy"
        );
        assert_eq!(
            suggest("select * from orders oo where o")[..3],
            [
                (Alias, "oo".to_owned()),
                (Function, "order_total()".to_owned()),
                (Keyword, "offset".to_owned()),
            ]
        );
    }

    #[test]
    fn completes_cte_and_subquery_columns_and_functions() {
        let cte = "with big as (select id as big_id, total from orders) select * from big b where ";
        assert_eq!(
            suggest(&format!("{cte}big_")),
            [(CandidateKind::Column, "big_id".to_owned())]
        );
        assert_eq!(
            suggest(&format!("{cte}b.")),
            [
                (CandidateKind::Column, "big_id".to_owned()),
                (CandidateKind::Column, "total".to_owned()),
            ]
        );
        assert_eq!(
            suggest("select * from (select customer_id as who from orders) s where s.w"),
            [(CandidateKind::Column, "who".to_owned())]
        );
        assert_eq!(
            suggest("with big as (select 1) select * from bi")[0],
            (CandidateKind::Derived, "big".to_owned())
        );
        let coalesce = suggest_at("select COAL", 11);
        assert_eq!(coalesce[0].insert, "COALESCE()");
        assert_eq!(coalesce[0].cursor_back, 1);
    }
}
