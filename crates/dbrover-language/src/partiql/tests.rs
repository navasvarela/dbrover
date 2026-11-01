use super::*;

fn statements(script: &str) -> Vec<&str> {
    split(script)
        .into_iter()
        .map(|range| &script[range])
        .collect()
}

fn parsed(text: &str) -> Statement {
    parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn error(text: &str) -> ParseError {
    match parse(text) {
        Ok(statement) => panic!("{text} parsed as {statement:?}"),
        Err(error) => error,
    }
}

#[test]
fn splits_at_semicolons_outside_strings_names_and_comments() {
    let script =
        "SELECT * FROM \"a;b\" WHERE x = 'c;d';\n-- e;f\nDELETE FROM t WHERE k = 1 /* ; */;;\n  ";
    assert_eq!(
        statements(script),
        [
            "SELECT * FROM \"a;b\" WHERE x = 'c;d'",
            "DELETE FROM t WHERE k = 1",
        ]
    );
    assert_eq!(statements("select 1\nselect 2"), ["select 1\nselect 2"]);
    assert!(statements("-- only a comment;").is_empty());
}

#[test]
fn tokens_cover_the_text_and_tell_names_strings_numbers_and_operators_apart() {
    let text = "SELECT \"a\"\"b\" FROM t WHERE n <= -2.5e3 AND s <> 'it''s' AND x IN <<1>> -- c";
    let tokens = tokenize(text);
    let joined: String = tokens
        .iter()
        .map(|token| &text[token.range.clone()])
        .collect();
    assert_eq!(joined, text);
    let kinds: Vec<(TokenKind, &str)> = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace))
        .map(|token| (token.kind, &text[token.range.clone()]))
        .collect();
    assert_eq!(
        kinds,
        [
            (TokenKind::Word, "SELECT"),
            (TokenKind::QuotedIdentifier, "\"a\"\"b\""),
            (TokenKind::Word, "FROM"),
            (TokenKind::Word, "t"),
            (TokenKind::Word, "WHERE"),
            (TokenKind::Word, "n"),
            (TokenKind::Punctuation, "<="),
            (TokenKind::Punctuation, "-"),
            (TokenKind::Number, "2.5e3"),
            (TokenKind::Word, "AND"),
            (TokenKind::Word, "s"),
            (TokenKind::Punctuation, "<>"),
            (TokenKind::String, "'it''s'"),
            (TokenKind::Word, "AND"),
            (TokenKind::Word, "x"),
            (TokenKind::Word, "IN"),
            (TokenKind::Punctuation, "<<"),
            (TokenKind::Number, "1"),
            (TokenKind::Punctuation, ">>"),
            (TokenKind::LineComment, "-- c"),
        ]
    );
}

#[test]
fn reads_the_table_and_index_a_statement_names() {
    let select = parsed("SELECT * FROM \"Orders\".\"By-Customer\" WHERE c = 'x'");
    assert_eq!(select.verb, Verb::Select);
    assert_eq!(select.table, "Orders");
    assert_eq!(select.index.as_deref(), Some("By-Customer"));
    let unquoted = parsed("select a.b, \"c\" from Orders");
    assert_eq!((unquoted.table.as_str(), unquoted.index), ("Orders", None));
    assert_eq!(
        parsed("SELECT * FROM \"say \"\"hi\"\"\"").table,
        "say \"hi\""
    );
    let insert = parsed("INSERT INTO \"Music\" VALUE {'Artist': 'A'}");
    assert_eq!(
        (insert.verb, insert.table.as_str()),
        (Verb::Insert, "Music")
    );
    let update = parsed("UPDATE Music SET a = 1 WHERE Artist = 'A' AND SongTitle = 'B'");
    assert_eq!(
        (update.verb, update.table.as_str()),
        (Verb::Update, "Music")
    );
    assert_eq!(
        update.condition,
        Condition::All(vec![
            Term::Equals("Artist".into()),
            Term::Equals("SongTitle".into())
        ])
    );
    let delete = parsed("DELETE FROM \"Music\" WHERE Artist = 'A'");
    assert_eq!(
        (delete.verb, delete.table.as_str()),
        (Verb::Delete, "Music")
    );
}

#[test]
fn reads_the_conditions_joined_by_and_at_the_top_of_a_where() {
    let cases: &[(&str, Condition)] = &[
        ("SELECT * FROM t", Condition::Everything),
        (
            "SELECT * FROM t WHERE pk = 'a' AND sk BETWEEN 1 AND 5 AND 'b' = \"other\"",
            Condition::All(vec![
                Term::Equals("pk".into()),
                Term::Other,
                Term::Equals("other".into()),
            ]),
        ),
        (
            "SELECT * FROM t WHERE ((pk = ?) AND (n > -1 AND m = -1)) ORDER BY sk",
            Condition::All(vec![
                Term::Equals("pk".into()),
                Term::Other,
                Term::Equals("m".into()),
            ]),
        ),
        (
            "SELECT * FROM t WHERE pk IN ['a', 'b'] AND x IN ('c')",
            Condition::All(vec![Term::In("pk".into()), Term::In("x".into())]),
        ),
        ("SELECT * FROM t WHERE pk = 'a' OR pk = 'b'", Condition::Any),
        (
            "SELECT * FROM t WHERE (pk = 'a' OR x = 1) AND y = 2",
            Condition::All(vec![Term::Other, Term::Equals("y".into())]),
        ),
        (
            "SELECT * FROM t WHERE a.b = 1 AND c[0] = 2 AND d = e AND f = true AND size(g) = 3",
            Condition::All(vec![
                Term::Other,
                Term::Other,
                Term::Other,
                Term::Equals("f".into()),
                Term::Other,
            ]),
        ),
    ];
    for (text, condition) in cases {
        assert_eq!(&parsed(text).condition, condition, "{text}");
    }
}

/// The scan guard's question: does a `SELECT` read only the partitions it names?
#[test]
fn a_select_reads_by_key_only_with_an_equality_or_in_on_the_partition_key() {
    let cases = [
        ("SELECT * FROM t WHERE pk = 'a'", Access::Partitions),
        ("SELECT * FROM t WHERE \"pk\" = ?", Access::Partitions),
        (
            "SELECT * FROM t WHERE 'a' = pk AND sk > 3",
            Access::Partitions,
        ),
        ("SELECT * FROM t WHERE pk IN ['a', 'b']", Access::Partitions),
        (
            "SELECT * FROM t WHERE (pk = 1) ORDER BY sk DESC",
            Access::Partitions,
        ),
        ("SELECT * FROM t WHERE pk = -1", Access::Partitions),
        ("SELECT * FROM t", Access::Scan),
        ("SELECT * FROM t WHERE sk = 'a'", Access::Scan),
        ("SELECT * FROM t WHERE PK = 'a'", Access::Scan),
        ("SELECT * FROM t WHERE pk > 'a'", Access::Scan),
        ("SELECT * FROM t WHERE pk <> 'a'", Access::Scan),
        ("SELECT * FROM t WHERE begins_with(pk, 'a')", Access::Scan),
        ("SELECT * FROM t WHERE pk = 'a' OR pk = 'b'", Access::Scan),
        ("SELECT * FROM t WHERE pk = other", Access::Scan),
        ("SELECT * FROM t WHERE NOT pk = 'a'", Access::Scan),
        ("SELECT * FROM t WHERE pk.x = 'a'", Access::Scan),
        ("UPDATE t SET a = 1 WHERE pk = 'a'", Access::Partitions),
        ("DELETE FROM t WHERE x = 'a'", Access::Partitions),
    ];
    for (text, access) in cases {
        assert_eq!(parsed(text).access("pk"), access, "{text}");
    }
}

#[test]
fn says_what_it_cannot_read_and_where() {
    let exists = "EXISTS(SELECT * FROM t)";
    let refused = error(exists);
    assert!(
        refused
            .message
            .contains("SELECT, INSERT, UPDATE and DELETE"),
        "{refused}"
    );
    assert_eq!(&exists[refused.range], "EXISTS");
    let unclosed = "SELECT * FROM t WHERE a = 'b";
    assert_eq!(&unclosed[error(unclosed).range], "'b");
    let bracket = "INSERT INTO t VALUE {'a': [1, 2}";
    let refused = error(bracket);
    assert_eq!(refused.message, "Expected `]`");
    assert_eq!(&bracket[refused.range], "}");
    assert_eq!(error("SELECT a, b").message, "Expected FROM and a table");
    assert_eq!(
        error("DELETE t WHERE a = 1").message,
        "Expected DELETE FROM and a table"
    );
    assert_eq!(
        error("SELECT * FROM 'Orders'").message,
        "Expected a table's name"
    );
}

#[test]
fn refuses_to_guess_what_text_without_exactly_one_statement_does() {
    assert_eq!(classify("  -- nothing"), Err(Unclassified::Empty));
    assert_eq!(
        classify("SELECT * FROM a; SELECT * FROM b"),
        Err(Unclassified::Several(2))
    );
}

/// Each statement in `classify.txt` follows a comment naming what it must be classified as,
/// `-- expect: read` (or `write`, `unparseable`), as the other languages' corpora do.
#[test]
fn classifies_its_corpus() {
    const EXPECT: &str = "-- expect:";
    let corpus = include_str!("classify.txt");
    let mut previous_end = 0;
    let mut checked = 0;
    for range in split(corpus) {
        let before = &corpus[previous_end..range.start];
        previous_end = range.end;
        let statement = &corpus[range];
        let expected = before
            .lines()
            .filter_map(|line| line.trim().strip_prefix(EXPECT))
            .next_back()
            .unwrap_or_else(|| panic!("no `{EXPECT}` line before {statement}"))
            .trim();
        let actual = match classify(statement) {
            Ok(StatementKind::Read) => "read",
            Ok(StatementKind::Write) => "write",
            Ok(kind) => panic!("{statement}: {kind:?}"),
            Err(Unclassified::Unparseable(_)) => "unparseable",
            Err(error) => panic!("{statement}: {error}"),
        };
        assert_eq!(actual, expected, "{statement}");
        checked += 1;
    }
    assert!(checked > 20, "only {checked} statements checked");
}

#[test]
fn completes_keywords_and_functions_with_their_arguments() {
    let words = completion_words();
    assert!(words.windows(2).all(|pair| pair[0] < pair[1]), "{words:?}");
    assert!(words.contains(&"begins_with") && words.contains(&"select"));
    assert_eq!(syntax("begins_with"), Some("begins_with(path, 'prefix')"));
    assert_eq!(syntax("select"), None);
    assert!(is_keyword("SELECT") && is_keyword("Missing") && !is_keyword("Orders"));
}

#[test]
fn open_data_reads_a_table_or_one_of_its_indexes() {
    let table = CatalogPath::root().child(NodeKind::Table, "orders");
    let index = table
        .child(
            NodeKind::Folder(dbrover_core::FolderKind::Indexes),
            "indexes",
        )
        .child(NodeKind::Index, "by-status");
    assert_eq!(
        read_all(&table).as_deref(),
        Some("SELECT * FROM \"orders\"")
    );
    assert_eq!(
        read_all(&index).as_deref(),
        Some("SELECT * FROM \"orders\".\"by-status\"")
    );
    assert_eq!(read_all(&table.child(NodeKind::Column, "pk")), None);
}

#[test]
fn open_data_reads_every_item_of_a_table_or_index() {
    assert_eq!(select_all("Orders", None), "SELECT * FROM \"Orders\"");
    assert_eq!(
        select_all("my\"table", Some("by-date")),
        "SELECT * FROM \"my\"\"table\".\"by-date\""
    );
    let statement = parsed(&select_all("my\"table", Some("by-date")));
    assert_eq!(statement.table, "my\"table");
    assert_eq!(statement.index.as_deref(), Some("by-date"));
}

mod edits {
    use dbrover_document::{Path, Step, Value};

    use super::super::{TableView, literal, typed_value};

    fn document(json: &str) -> Value {
        Value::parse(json).unwrap()
    }

    fn view() -> TableView {
        TableView::new("orders".to_owned(), vec!["pk".to_owned(), "sk".to_owned()])
    }

    fn path(names: &[&str]) -> Path {
        names.iter().fold(Path::default(), |path, name| {
            path.child(Step::Field((*name).to_owned()))
        })
    }

    #[test]
    fn writes_each_type_as_a_partiql_literal() {
        let cases = [
            (r#""it's""#, "'it''s'"),
            ("12.50", "12.50"),
            ("true", "true"),
            ("null", "NULL"),
            (r#"[1, "a"]"#, "[1, 'a']"),
            (r#"{"city": "Leeds", "n": 2}"#, "{'city': 'Leeds', 'n': 2}"),
            (r#"{"$stringSet": ["a", "b"]}"#, "<<'a', 'b'>>"),
            (r#"{"$numberSet": [1, 2]}"#, "<<1, 2>>"),
        ];
        for (json, expected) in cases {
            assert_eq!(literal(&document(json)).as_deref(), Ok(expected), "{json}");
        }
        assert!(
            literal(&document(
                r#"{"$binary": {"base64": "AQ==", "subType": "00"}}"#
            ))
            .is_err()
        );
    }

    #[test]
    fn typed_text_keeps_the_old_values_type() {
        let set = document(r#"{"$numberSet": [1]}"#);
        assert_eq!(
            typed_value(Some(&set), Some("[2, 3]")),
            Ok(document(r#"{"$numberSet": [2, 3]}"#))
        );
        assert!(typed_value(Some(&set), Some(r#"["a"]"#)).is_err());
        assert!(typed_value(Some(&set), Some("[]")).is_err());
        assert!(typed_value(Some(&document("1")), Some("abc")).is_err());
        assert_eq!(
            typed_value(Some(&document(r#""x""#)), Some("12")),
            Ok(document(r#""12""#))
        );
        assert_eq!(typed_value(None, Some("12")), Ok(document("12")));
        assert_eq!(typed_value(None, None), Ok(Value::Null));
    }

    #[test]
    fn edits_become_statements_by_the_whole_key_with_their_undo() {
        let item =
            document(r#"{"pk": "c-1", "sk": 3, "total": 10, "tags": {"$stringSet": ["a"]}}"#);
        let delete = view().delete(&item).unwrap();
        assert_eq!(
            delete.command,
            r#"DELETE FROM "orders" WHERE "pk" = 'c-1' AND "sk" = 3"#
        );
        assert_eq!(
            delete.revert.as_deref(),
            Some(
                r#"INSERT INTO "orders" VALUE {'pk': 'c-1', 'sk': 3, 'total': 10, 'tags': <<'a'>>}"#
            )
        );
        let (update, name) = view()
            .update(
                &item,
                &path(&["total"]),
                Some(&document("10")),
                Some("11.5"),
            )
            .unwrap();
        assert_eq!(name, "total");
        assert_eq!(
            update.command,
            r#"UPDATE "orders" SET "total" = 11.5 WHERE "pk" = 'c-1' AND "sk" = 3"#
        );
        assert_eq!(
            update.revert.as_deref(),
            Some(r#"UPDATE "orders" SET "total" = 10 WHERE "pk" = 'c-1' AND "sk" = 3"#)
        );
        let (added, _) = view()
            .update(&item, &path(&["address", "city"]), None, Some("Leeds"))
            .unwrap();
        assert_eq!(
            added.revert.as_deref(),
            Some(r#"UPDATE "orders" REMOVE "address"."city" WHERE "pk" = 'c-1' AND "sk" = 3"#)
        );
        assert!(
            view()
                .update(&item, &path(&["pk"]), None, Some("x"))
                .is_err()
        );
        assert!(view().delete(&document(r#"{"pk": "c-1"}"#)).is_err());
        assert_eq!(
            view().insert(&[("pk", Some("c-2")), ("sk", Some("1")), ("note", None)]),
            Ok(r#"INSERT INTO "orders" VALUE {'pk': 'c-2', 'sk': 1}"#.to_owned())
        );
        assert!(view().insert(&[("pk", Some("c-2"))]).is_err());
    }

    #[test]
    fn only_a_select_of_one_table_is_editable() {
        assert_eq!(
            TableView::source(r#"SELECT * FROM "orders" WHERE pk = 'a'"#).as_deref(),
            Some("orders")
        );
        assert_eq!(
            TableView::source(r#"SELECT * FROM "orders"."by-status""#),
            None
        );
        assert_eq!(
            TableView::source(r#"DELETE FROM "orders" WHERE pk = 'a'"#),
            None
        );
    }
}
