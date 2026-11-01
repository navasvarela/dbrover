use super::*;

fn console(text: &str) -> QueryConsole {
    console_with(text, Dialect::Postgres)
}

fn console_with(text: &str, dialect: Dialect) -> QueryConsole {
    console_in(text, QueryLanguage::Sql(dialect))
}

fn console_in(text: &str, language: QueryLanguage) -> QueryConsole {
    QueryConsole::new(
        ConsoleId(1),
        ConsoleKey {
            data_source: DataSourceId::generate(),
            name: dbrover_store::ConsoleName::numbered(1),
        },
        "test".to_owned(),
        language,
        text.to_owned(),
    )
}

fn sqls(statements: &[SubmittedStatement]) -> Vec<&str> {
    statements.iter().map(|s| s.sql.as_str()).collect()
}

#[test]
fn runs_the_selection_the_statement_under_the_cursor_or_everything() {
    let text = "select 1;\nselect 2;\nselect 3";
    let second = text.find("select 2").unwrap();
    let cases: [(Range<usize>, RunScope, &[&str]); 5] = [
        (second..second, RunScope::Current, &["select 2"]),
        // Just after the `;`, the statement before it is meant.
        (second - 1..second - 1, RunScope::Current, &["select 1"]),
        // A selection runs exactly what is selected, split into statements, in either
        // selection direction.
        (
            text.len()..second,
            RunScope::Current,
            &["select 2", "select 3"],
        ),
        (second..second + 6, RunScope::Current, &["select"]),
        (0..0, RunScope::All, &["select 1", "select 2", "select 3"]),
    ];

    for (selection, scope, expected) in cases {
        let mut console = console(text);
        console.selection = selection.clone();
        let statements = console.statements_to_run(scope);
        assert_eq!(sqls(&statements), expected, "{selection:?} {scope:?}");
        for statement in &statements {
            assert_eq!(&text[statement.range.clone()], statement.sql);
        }
    }
}

#[test]
fn follows_a_run_through_rows_failure_and_completion() {
    let text = "select n from t;\nselect broken";
    let mut console = console(text);
    let statements = console.statements_to_run(RunScope::All);
    let run = RunId(7);
    console.begin_run(run, &statements);
    assert!(console.is_running());

    console.apply(&Event::StatementStarted {
        run,
        statement: 0,
        columns: vec![ColumnInfo {
            name: "n".to_owned(),
            type_name: None,
        }],
        shape: ResultShape::Rows,
    });
    console.apply(&Event::RowsFetched {
        run,
        statement: 0,
        rows: vec![vec![Some("1".to_owned())]],
        more: true,
    });
    assert_eq!(console.request_more(), Some(run));
    assert_eq!(console.request_more(), None, "a fetch is already in flight");
    console.apply(&Event::StatementFinished {
        run,
        statement: 1,
        result: Err(StatementFailure {
            message: "syntax error".to_owned(),
            position: Some("select ".len()),
        }),
    });
    console.apply(&Event::RunFinished {
        run,
        outcome: RunOutcome::Failed,
        elapsed: Duration::from_millis(3),
    });

    assert!(!console.is_running());
    let marker = console.error_marker.clone().unwrap();
    assert_eq!(&text[marker], "broken");
    assert_eq!(console.run.as_ref().unwrap().selected, 1);
}

#[test]
fn ignores_events_from_older_runs() {
    let mut console = console("select 1");
    let statements = console.statements_to_run(RunScope::All);
    console.begin_run(RunId(1), &statements);
    console.begin_run(RunId(2), &statements);

    console.apply(&Event::RunFinished {
        run: RunId(1),
        outcome: RunOutcome::Completed,
        elapsed: Duration::ZERO,
    });

    assert!(console.is_running());
}

#[test]
fn a_lost_connection_fails_the_running_statement() {
    let mut console = console("select 1; select 2");
    let statements = console.statements_to_run(RunScope::All);
    console.begin_run(RunId(1), &statements);

    console.apply(&Event::ConsoleSessionChanged {
        console: ConsoleId(1),
        state: SessionState::Failed("could not connect: refused".to_owned()),
    });

    assert!(!console.is_running());
    let first = &console.run.as_ref().unwrap().statements[0];
    assert!(matches!(&first.result, Some(Err(failure)) if failure.message.contains("refused")));
}

#[test]
fn inserts_a_statement_on_a_line_of_its_own_at_the_cursor() {
    let cases: [(&str, Range<usize>, &str, &str); 3] = [
        ("", 0..0, "select 1", "select 1;\n"),
        ("select 1;", 9..9, "select 2;", "select 1;\nselect 2;\n"),
        // A selection is replaced.
        ("select 1;\nold;\n", 10..14, "new  ", "select 1;\nnew;\n\n"),
    ];
    for (text, selection, sql, expected) in cases {
        let mut console = console(text);
        console.selection = selection;

        console.insert_statement(sql, Instant::now());

        assert_eq!(console.text, expected, "{text:?}");
        assert_eq!(console.pending_cursor, Some(console.selection.end));
        assert!(
            matches!(&console.storage, file::Storage::Config(file) if file.unsaved_since.is_some())
        );
    }
}

#[test]
fn explains_the_statement_at_the_cursor_and_marks_errors_in_it() {
    let text = "select 1;\nselect broken from;";
    let second = text.find("select broken").unwrap();
    let mut console = console(text);
    console.selection = second..second;

    let statements = console.statements_to_run(RunScope::Explain(ExplainDepth::Estimated));

    assert_eq!(
        sqls(&statements),
        ["EXPLAIN (FORMAT JSON) select broken from"]
    );
    console.begin_run(RunId(1), &statements);
    console.apply(&Event::StatementFinished {
        run: RunId(1),
        statement: 0,
        result: Err(StatementFailure {
            message: "syntax error".to_owned(),
            position: Some("EXPLAIN (FORMAT JSON) select broken ".len()),
        }),
    });
    assert_eq!(&text[console.error_marker.clone().unwrap()], "from");
}

/// PostgreSQL's plan comes back as one JSON value, which the console reads into a plan to draw
/// once the statement finishes; a value that isn't a plan stays rows, with the reason.
#[test]
fn reads_the_plan_an_explain_returns() {
    let mut console = console("select * from orders");
    let statements = console.statements_to_run(RunScope::Explain(ExplainDepth::Estimated));
    assert_eq!(
        statements[0].plan,
        Some(dbrover_plan::PlanFormat::PostgresJson)
    );
    let explain = |console: &mut QueryConsole, run: u64, value: &str| {
        console.begin_run(RunId(run), &statements);
        for event in [
            Event::StatementStarted {
                run: RunId(run),
                statement: 0,
                columns: vec![ColumnInfo {
                    name: "QUERY PLAN".to_owned(),
                    type_name: Some("json".to_owned()),
                }],
                shape: ResultShape::Rows,
            },
            Event::RowsFetched {
                run: RunId(run),
                statement: 0,
                rows: vec![vec![Some(value.to_owned())]],
                more: false,
            },
            Event::StatementFinished {
                run: RunId(run),
                statement: 0,
                result: Ok(StatementSummary {
                    rows_changed: None,
                    elapsed: Duration::from_millis(3),
                    truncated: false,
                }),
            },
        ] {
            console.apply(&event);
        }
        console.run.as_ref().unwrap().statements[0]
            .plan
            .as_ref()
            .map(|plan| (plan.is_read(), plan.failure().map(str::to_owned)))
    };

    let read = explain(
        &mut console,
        1,
        r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "orders", "Total Cost": 1.5}}]"#,
    );
    assert_eq!(read, Some((true, None)));
    let plan = console.run.as_ref().unwrap().statements[0].plan.as_ref();
    assert_eq!(
        plan.and_then(|plan| plan.plan())
            .map(|plan| plan.root.title()),
        Some("Seq Scan on orders".to_owned())
    );

    let unread = explain(&mut console, 2, "Seq Scan on orders");
    assert!(matches!(unread, Some((false, Some(reason))) if reason.contains("isn't JSON")));
}

#[test]
fn shades_the_statement_at_the_cursor_unless_text_is_selected() {
    let text = "select 1;\nselect 2;";
    let second = text.find("select 2").unwrap();
    let mut console = console(text);

    console.selection = second + 3..second + 3;
    assert_eq!(console.current_statement(), Some(second..text.len() - 1));

    console.selection = 0..4;
    assert_eq!(console.current_statement(), None);
}

#[test]
fn completion_replaces_the_typed_prefix_and_closes() {
    let mut console = console("select * from t where SEL");
    console.selection = console.text.len()..console.text.len();

    let recent = RecentCompletions::default();
    console.open_completion(false);
    let _ = console.refresh_completion(None, &recent, &[]);
    assert!(
        console.completion.is_some(),
        "keywords match without a catalog"
    );
    console.accept_completion(Instant::now());

    assert_eq!(console.text, "select * from t where SELECT");
    assert!(console.completion.is_none());
    assert_eq!(console.pending_cursor, Some(console.text.len()));

    console.open_completion(false);
    let _ = console.refresh_completion(None, &recent, &[]);
    assert!(
        console.completion.is_none(),
        "nothing starts with SELECT but SELECT itself"
    );
}

#[test]
fn formats_the_statement_at_the_cursor_and_keeps_the_cursor_on_its_token() {
    let text = "select 1;\nselect a,b from t where x=1;";
    let mut console = console(text);
    let cursor = text.find("from").unwrap() + 2;
    console.selection = cursor..cursor;

    console.format(
        FormatScope::Current,
        &FormatOptions::default(),
        Instant::now(),
    );

    let expected = "select 1;\nSELECT\n    a,\n    b\nFROM t\nWHERE x = 1;";
    assert_eq!(console.text, expected);
    let moved = expected.find("FROM").unwrap() + 2;
    assert_eq!(console.selection, moved..moved);
    assert_eq!(console.pending_cursor, Some(moved));
    assert_eq!(console.replaced_text.as_deref(), Some(text));
    assert!(
        matches!(&console.storage, file::Storage::Config(file) if file.unsaved_since.is_some())
    );
}

#[test]
fn formats_everything_or_only_whole_tokens_of_the_selection() {
    let text = "select 'a,  b', c from t; select d,e from u";
    let mut console = console(text);
    console.format(FormatScope::All, &FormatOptions::default(), Instant::now());
    assert_eq!(
        console.text,
        "SELECT\n    'a,  b',\n    c\nFROM t; SELECT\n    d,\n    e\nFROM u"
    );

    // A selection starting inside the string formats from the token after it; a fragment
    // that isn't a whole query only has its keywords' case changed.
    let mut console = console_with(text, Dialect::Postgres);
    let start = text.find("  b").unwrap();
    console.selection = start..text.find(';').unwrap();
    console.format(
        FormatScope::Current,
        &FormatOptions::default(),
        Instant::now(),
    );
    assert_eq!(console.text, "select 'a,  b', c FROM t; select d,e from u");
}

#[test]
fn a_format_that_changes_nothing_leaves_no_undo_point() {
    let text = "SELECT 1";
    let mut console = console(text);
    console.format(FormatScope::All, &FormatOptions::default(), Instant::now());
    assert_eq!(console.text, text);
    assert!(console.replaced_text.is_none());
    assert!(console.pending_cursor.is_none());
}

#[test]
fn accepting_a_function_puts_the_cursor_inside_its_parentheses_and_reports_it() {
    let mut console = console("select coal");
    console.selection = console.text.len()..console.text.len();
    let recent = RecentCompletions::default();

    console.open_completion(false);
    let _ = console.refresh_completion(None, &recent, &[]);
    console.accept_completion(Instant::now());

    assert_eq!(console.text, "select coalesce()");
    assert_eq!(console.pending_cursor, Some(console.text.len() - 1));
    let accepted = console.take_accepted_completion().unwrap();
    assert_eq!(accepted.label, "coalesce");
    assert!(console.take_accepted_completion().is_none());
}

#[test]
fn keeps_clickhouse_results_read_only_without_describing_the_table() {
    let cases = [(Dialect::Postgres, true), (Dialect::ClickHouse, false)];
    for (dialect, describes) in cases {
        let mut console = console_with("select n from t", dialect);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: vec![ColumnInfo {
                name: "n".to_owned(),
                type_name: None,
            }],
            shape: ResultShape::Rows,
        });

        let tables = console.tables_to_describe();

        assert_eq!(!tables.is_empty(), describes, "{dialect:?}");
        let editability = &console.run.as_ref().unwrap().statements[0]
            .edits
            .editability;
        assert_eq!(
            matches!(editability, Editability::ReadOnly(_)),
            !describes,
            "{dialect:?}: {editability:?}"
        );
    }
}

/// A read-only data source's results are never editable, but their table is still described,
/// so its foreign keys can be followed.
#[test]
fn describes_a_read_only_data_sources_tables_for_their_keys() {
    let mut console = console_with("select n from t", Dialect::Postgres);
    console.safety = SafetyLevel::ReadOnly;
    console.data_source_name = "production".to_owned();
    let statements = console.statements_to_run(RunScope::All);
    let run = RunId(1);
    console.begin_run(run, &statements);
    console.apply(&Event::StatementStarted {
        run,
        statement: 0,
        columns: vec![ColumnInfo {
            name: "n".to_owned(),
            type_name: None,
        }],
        shape: ResultShape::Rows,
    });

    let tables = console.tables_to_describe();

    assert_eq!(tables.len(), 1);
    let statement = &console.run.as_ref().unwrap().statements[0];
    assert_eq!(
        statement.edits.editability,
        Editability::ReadOnly("\u{201c}production\u{201d} is read-only".to_owned())
    );
    assert_eq!(statement.describing.as_ref(), tables.first());
}

/// Sorting in the database runs the shown statement again, wrapped; its new result keeps the
/// statement as typed (which decides what can be edited) and the sort, and switching back
/// runs it as typed.
#[test]
fn runs_a_statement_again_wrapped_and_keeps_it_as_typed() {
    let mut console = console_with("select id, note from t;", Dialect::Postgres);
    let statements = console.statements_to_run(RunScope::All);
    console.begin_run(RunId(1), &statements);
    console.apply(&Event::StatementStarted {
        run: RunId(1),
        statement: 0,
        columns: ["id", "note"]
            .into_iter()
            .map(|name| ColumnInfo {
                name: name.to_owned(),
                type_name: None,
            })
            .collect(),
        shape: ResultShape::Rows,
    });
    let grid = &mut console.run.as_mut().unwrap().statements[0].grid;
    grid.set_in_database(true);
    grid.toggle_sort(1);

    let wrapped = console.rearrange().unwrap();
    let statement = &wrapped.statements[0];
    assert_eq!(
        statement.sql,
        "SELECT * FROM (\nselect id, note from t\n) AS dbrover_rows\nORDER BY 2 ASC"
    );
    assert_eq!(&statement.sql[statement.prefix_len..][..6], "select");
    console.begin_run(RunId(2), &wrapped.statements);
    let view = &console.run.as_ref().unwrap().statements[0];
    assert_eq!(view.sql, "select id, note from t");
    assert!(view.wrapped && view.grid.is_in_database());
    assert_eq!(view.grid.sort().map(|sort| sort.column), Some(1));

    console.run.as_mut().unwrap().statements[0]
        .grid
        .set_in_database(false);
    let as_typed = console.rearrange().unwrap();
    assert_eq!(as_typed.statements[0].sql, "select id, note from t");
    assert_eq!(as_typed.statements[0].in_database, None);
}

#[test]
fn finds_from_the_selection_and_replaces_one_match_at_a_time() {
    let mut console = console("select id from orders; select id from order_items;");
    console.selection = 7..9;
    console.open_find(false);
    assert_eq!(
        console.find_bar().query,
        "id",
        "the selection is searched for"
    );

    console.find_next(true);
    assert_eq!(console.selection, 30..32);
    assert_eq!(console.found().current, Some(1));
    console.find_next(true);
    assert_eq!(console.selection, 7..9, "wraps around");

    let bar = console.find_bar_mut();
    bar.replacement = "order_id".to_owned();
    bar.options.whole_word = true;
    console.replace_current(Instant::now());
    assert_eq!(
        console.text(),
        "select order_id from orders; select id from order_items;"
    );
    assert_eq!(console.selection, 36..38, "the next match is selected");
    assert_eq!(
        console.replaced_text.as_deref(),
        Some("select id from orders; select id from order_items;"),
        "one undo step"
    );
}

#[test]
fn replaces_every_match_with_regular_expression_groups() {
    let mut console = console("select a1, b2 from t");
    console.open_find(true);
    let bar = console.find_bar_mut();
    bar.query = r"([a-z])(\d)".to_owned();
    bar.replacement = "$2$1".to_owned();
    bar.options.regex = true;

    assert_eq!(console.replace_all(Instant::now()), 2);
    assert_eq!(console.text(), "select 1a, 2b from t");

    console.find_bar_mut().query = "(".to_owned();
    assert!(console.found().error.is_some());
    assert_eq!(console.replace_all(Instant::now()), 0);
    console.close_find();
    assert_eq!(console.found(), Found::default());
}

/// A console in the test language, which has no SQL dialect: what a Redis or MongoDB console
/// will go through.
/// A CQL console completes CQL's own words in the case being typed, keeps a batch as one
/// statement, and has no explain.
#[test]
fn a_cql_console_completes_its_words_and_has_no_explain() {
    let text = "CREATE KEYSP";
    let mut console = console_with(text, Dialect::Cassandra);
    console.selection = text.len()..text.len();
    let recent = RecentCompletions::default();
    console.open_completion(false);
    let _ = console.refresh_completion(None, &recent, &[]);
    console.accept_completion(Instant::now());
    assert_eq!(console.text, "CREATE KEYSPACE");
    assert!(
        console
            .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
            .is_empty()
    );

    let batch =
        "BEGIN BATCH\nINSERT INTO a (id) VALUES (1);\nDELETE FROM b WHERE id = 1;\nAPPLY BATCH";
    let console = console_with(&format!("{batch};\nSELECT * FROM a"), Dialect::Cassandra);
    assert_eq!(
        sqls(&console.statements_to_run(RunScope::All)),
        [batch, "SELECT * FROM a"]
    );
}

/// A CQL table's rows are edited by the whole primary key: a loaded row's key cells stay as they
/// are, an added row's take values, and each value is written as its column's type reads it.
#[test]
fn edits_cql_rows_by_their_key_with_typed_literals() {
    use dbrover_core::ColumnDescription;

    let mut console = console_with("SELECT p, n, tags FROM shop.t", Dialect::Cassandra);
    let statements = console.statements_to_run(RunScope::All);
    let run = RunId(1);
    console.begin_run(run, &statements);
    let typed = |name: &str, type_name: &str| ColumnInfo {
        name: name.to_owned(),
        type_name: Some(type_name.to_owned()),
    };
    let columns = [
        typed("p", "text"),
        typed("n", "int"),
        typed("tags", "map<text, int>"),
    ];
    console.apply(&Event::StatementStarted {
        run,
        statement: 0,
        columns: columns.to_vec(),
        shape: ResultShape::Rows,
    });
    console.apply(&Event::RowsFetched {
        run,
        statement: 0,
        rows: vec![vec![
            Some("x".to_owned()),
            Some("1".to_owned()),
            Some(r#"{"a": 1}"#.to_owned()),
        ]],
        more: false,
    });
    let tables = console.tables_to_describe();
    let [table] = tables.as_slice() else {
        panic!("expected the table to be described: {tables:?}");
    };
    let description = TableDescription {
        schema: "shop".to_owned(),
        name: "t".to_owned(),
        columns: columns
            .iter()
            .map(|column| ColumnDescription {
                name: column.name.clone(),
                data_type: column.type_name.clone().unwrap_or_default(),
                nullable: column.name == "tags",
                default: None,
                identity: None,
            })
            .collect(),
        primary_key: vec!["p".to_owned(), "n".to_owned()],
        foreign_keys: vec![],
        referenced_by: vec![],
    };
    console.apply(&Event::TableDescribed {
        data_source: console.data_source(),
        table: table.clone(),
        result: Ok(Some(description)),
    });

    let shown = &mut console.run.as_mut().unwrap().statements[0];
    assert!(
        !shown.edits.is_editable(edit::RowRef::Loaded(0), 1),
        "a key cell"
    );
    assert!(shown.edits.is_editable(edit::RowRef::Loaded(0), 2));
    let rows = shown.rows.clone();
    shown.edits.pending.set(
        &rows,
        edit::RowRef::Loaded(0),
        2,
        Some(r#"{"b": 2}"#.to_owned()),
    );
    shown.edits.add_row();
    assert!(shown.edits.is_editable(edit::RowRef::New(0), 1));
    for (column, value) in [(0, "y"), (1, "2")] {
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::New(0), column, Some(value.to_owned()));
    }

    let Some(Command::ApplyEdits { statements, .. }) = console.submit_edits() else {
        panic!("nothing submitted");
    };
    let sql: Vec<&str> = statements.iter().map(|edit| edit.sql.as_str()).collect();
    assert_eq!(
        sql,
        [
            r#"UPDATE "shop"."t" SET "tags" = fromJson('{"b": 2}') WHERE "p" = 'x' AND "n" = 1"#,
            r#"INSERT INTO "shop"."t" ("p", "n") VALUES ('y', 2)"#,
        ]
    );
}

mod test_language {
    use super::*;

    #[test]
    fn runs_one_command_per_line() {
        let text = "get a\n# a note\nset b \"x y\"\n\nkeys *";
        let mut console = console_in(text, QueryLanguage::Test);
        assert_eq!(
            sqls(&console.statements_to_run(RunScope::All)),
            ["get a", "set b \"x y\"", "keys *"]
        );
        let second = text.find("set").unwrap();
        console.selection = second + 2..second + 2;
        assert_eq!(
            sqls(&console.statements_to_run(RunScope::Current)),
            ["set b \"x y\""]
        );
        assert_eq!(
            console.current_statement(),
            Some(second..second + "set b \"x y\"".len())
        );
    }

    #[test]
    fn completes_the_languages_own_words() {
        let mut console = console_in("ge", QueryLanguage::Test);
        console.selection = 2..2;
        let recent = RecentCompletions::default();
        console.open_completion(false);
        let _ = console.refresh_completion(None, &recent, &[]);
        console.accept_completion(Instant::now());
        assert_eq!(console.text, "get");
    }

    #[test]
    fn completes_redis_commands_with_their_arguments() {
        let mut console = console_in("HSE", QueryLanguage::Redis);
        console.selection = 3..3;
        let recent = RecentCompletions::default();
        console.open_completion(false);
        let _ = console.refresh_completion(None, &recent, &[]);
        let hset = console
            .completion
            .as_ref()
            .and_then(|completion| {
                completion
                    .candidates
                    .iter()
                    .find(|candidate| candidate.label == "HSET")
            })
            .map(|candidate| candidate.detail.clone());
        assert_eq!(
            hset,
            Some(Some("key field value [field value ...]".to_owned()))
        );
    }

    /// ⌥⌘E on a MongoDB `find` runs it with `.explain("executionStats")`; a command with no
    /// plan to show, such as a count, is left alone.
    #[test]
    fn explains_a_mongodb_find_at_the_cursor() {
        let mut console = console_in(
            "db.orders.countDocuments()\ndb.orders.find({a: 1})",
            QueryLanguage::Mongo,
        );
        console.selection = 40..40;
        let explained: Vec<String> = console
            .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
            .into_iter()
            .map(|statement| statement.sql)
            .collect();
        assert_eq!(
            explained,
            ["db.orders.find({a: 1}).explain(\"executionStats\")"]
        );
        console.selection = 3..3;
        assert!(
            console
                .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
                .is_empty()
        );
    }

    /// ⌥⌘E on a search runs it with `"profile": true` at the end of its body; a request with
    /// nothing to profile, such as a count, is left alone.
    #[test]
    fn explains_a_search_at_the_cursor() {
        let mut console = console_in(
            "GET /orders/_count\nGET /orders/_search\n{ \"size\": 5 }",
            QueryLanguage::Search,
        );
        console.selection = 30..30;
        let explained: Vec<String> = console
            .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
            .into_iter()
            .map(|statement| statement.sql)
            .collect();
        assert_eq!(
            explained,
            ["GET /orders/_search\n{ \"size\": 5, \"profile\": true }"]
        );
        console.selection = 3..3;
        assert!(
            console
                .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
                .is_empty()
        );
    }

    #[test]
    fn completes_mongodb_methods_as_written_whatever_the_case_typed() {
        let mut console = console_in("db.orders.insertO", QueryLanguage::Mongo);
        console.selection = 17..17;
        let recent = RecentCompletions::default();
        console.open_completion(false);
        let _ = console.refresh_completion(None, &recent, &[]);
        let insert_one = console
            .completion
            .as_ref()
            .and_then(|completion| {
                completion
                    .candidates
                    .iter()
                    .find(|candidate| candidate.insert == "insertOne")
            })
            .map(|candidate| candidate.detail.clone());
        assert_eq!(insert_one, Some(Some("document, options".to_owned())));
    }

    /// In an Elasticsearch console, a path's endpoint completes in a request line and a query
    /// word inside the string being typed in a body, as written.
    #[test]
    fn completes_search_endpoints_and_query_words_as_written() {
        let recent = RecentCompletions::default();
        let cases = [
            ("GET /orders/_se", "_search", Some("GET /<index>/_search")),
            (
                "POST /orders/_search\n{ \"query\": { \"match_a",
                "match_all",
                Some("query"),
            ),
        ];
        for (text, word, detail) in cases {
            let mut console = console_in(text, QueryLanguage::Search);
            console.selection = text.len()..text.len();
            console.open_completion(false);
            let _ = console.refresh_completion(None, &recent, &[]);
            let found = console
                .completion
                .as_ref()
                .and_then(|completion| {
                    completion
                        .candidates
                        .iter()
                        .find(|candidate| candidate.insert == word)
                })
                .map(|candidate| candidate.detail.clone());
            assert_eq!(found, Some(detail.map(str::to_owned)), "{text}");
        }
    }

    #[test]
    fn leaves_out_what_only_sql_has() {
        let mut console = console_in("get a", QueryLanguage::Test);
        assert_eq!(console.dialect(), None);
        assert!(
            console
                .statements_to_run(RunScope::Explain(ExplainDepth::Estimated))
                .is_empty()
        );

        console.format(FormatScope::All, &FormatOptions::default(), Instant::now());
        assert_eq!(console.text, "get a");
        assert!(console.format_error.is_some());

        let outcome = console.check_problems(None, &LintOptions::default(), Instant::now());
        assert_eq!(outcome, problems::CheckOutcome::default());
    }

    /// Rows from a language without a SQL dialect copy and export in every format but the SQL
    /// ones, which N0a left hidden.
    #[test]
    fn copies_and_exports_rows_without_a_sql_dialect() {
        let mut console = console_in("hgetall h", QueryLanguage::Test);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        let column = |name: &str| ColumnInfo {
            name: name.to_owned(),
            type_name: None,
        };
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: vec![column("field"), column("value")],
            shape: ResultShape::Rows,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows: vec![
                vec![Some("a".to_owned()), Some("1".to_owned())],
                vec![Some("b".to_owned()), None],
            ],
            more: false,
        });

        let preview = console.export_preview().unwrap();
        assert_eq!(preview.loaded, 2);
        assert_eq!(preview.source.dialect, None);
        let offered: Vec<&str> = dbrover_export::export_choices(&preview.source)
            .into_iter()
            .filter(|choice| choice.spec.is_ok())
            .map(|choice| choice.name)
            .collect();
        assert_eq!(
            offered,
            [
                "CSV",
                "CSV for Excel",
                "Excel",
                "TSV",
                "Markdown",
                "JSON",
                "JSON Lines",
                "Parquet",
                "HTML"
            ]
        );

        let statement = &mut console.run.as_mut().unwrap().statements[0];
        statement.grid.refresh(&statement.rows);
        statement.grid.select(grid::CellPos { row: 0, column: 0 });
        statement
            .grid
            .extend_selection(grid::CellPos { row: 1, column: 1 });
        assert_eq!(
            console
                .copy(
                    clipboard::ClipboardCopy::As(clipboard::ClipboardFormat::Markdown),
                    &ExportOptions::default()
                )
                .as_deref(),
            Some("| field | value |\n| --- | --- |\n| a | 1 |\n| b | NULL |\n")
        );

        let spec = dbrover_export::ExportFormat::Csv
            .default_spec(&preview.source)
            .unwrap();
        let Some(Command::ExportRows { export, .. }) = console.export(spec, "rows.csv".into())
        else {
            panic!("no export");
        };
        assert_eq!(export.rows.len(), 2);
        assert!(
            export
                .columns
                .iter()
                .all(|column| column.kind == dbrover_core::ValueKind::Undeclared)
        );
    }

    /// A result of documents, as the engine sends a documents cursor's: one document a row,
    /// a page at a time (ADR 0006).
    mod document_results {
        use egui_kittest::kittest::Queryable;

        use super::*;
        use dbrover_export::ExportFormat;

        use crate::console::clipboard::{ClipboardCopy, ClipboardFormat};
        use crate::console::documents::{self, ColumnFold, ShownAs};
        use crate::test_support::ui_harness;

        const FIRST_PAGE: [&str; 2] = [
            r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}, "name": "Ada", "address": {"city": "Leeds", "zip": "LS1"}, "note": null}"#,
            r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c4"}, "name": "Bo", "total": {"$numberLong": "7"}}"#,
        ];
        const SECOND_PAGE: [&str; 1] =
            [r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c5"}, "tags": ["a", "b"]}"#];

        fn page(documents: &[&str]) -> Vec<Row> {
            documents
                .iter()
                .map(|document| vec![Some((*document).to_owned())])
                .collect()
        }

        /// A test-language console with a result of documents: the first page loaded, and the
        /// second too when `both`.
        fn console_with_documents(both: bool) -> QueryConsole {
            let mut console = console_in("find", QueryLanguage::Test);
            let statements = console.statements_to_run(RunScope::All);
            let run = RunId(1);
            console.begin_run(run, &statements);
            console.apply(&Event::StatementStarted {
                run,
                statement: 0,
                columns: vec![ColumnInfo {
                    name: "document".to_owned(),
                    type_name: None,
                }],
                shape: ResultShape::Documents,
            });
            console.apply(&Event::RowsFetched {
                run,
                statement: 0,
                rows: page(&FIRST_PAGE),
                more: true,
            });
            if both {
                console.apply(&Event::RowsFetched {
                    run,
                    statement: 0,
                    rows: page(&SECOND_PAGE),
                    more: false,
                });
            }
            console
        }

        fn statement(console: &QueryConsole) -> &StatementView {
            &console.run.as_ref().unwrap().statements[0]
        }

        fn names(statement: &StatementView) -> Vec<&str> {
            statement
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect()
        }

        #[test]
        fn flattens_documents_into_columns_a_page_at_a_time() {
            let mut console = console_with_documents(false);
            assert_eq!(
                names(statement(&console)),
                ["_id", "name", "address", "note", "total"]
            );

            let run = RunId(1);
            console.apply(&Event::RowsFetched {
                run,
                statement: 0,
                rows: page(&SECOND_PAGE),
                more: false,
            });
            let statement = statement(&console);
            assert_eq!(
                names(statement),
                ["_id", "name", "address", "note", "total", "tags"],
                "a field first seen on a later page adds a column at the right"
            );
            let cell = |row: usize, column: usize| statement.rows[row][column].as_deref();
            assert_eq!(cell(0, 0), Some(r#"ObjectId("5f1d7f0e2b5c3a0001a1b2c3")"#));
            assert_eq!(cell(0, 5), None, "rows loaded earlier get the new column");
            assert_eq!(cell(1, 4), Some("NumberLong(7)"));
            assert_eq!(cell(2, 5), Some(r#"["a","b"]"#));

            let flattened = &statement.documents.as_ref().unwrap();
            assert!(matches!(
                documents::cell_look(flattened, 0, 3),
                documents::CellLook::Text
            ));
            assert!(matches!(
                documents::cell_look(flattened, 1, 3),
                documents::CellLook::Missing
            ));
            assert!(matches!(
                documents::cell_look(flattened, 0, 2),
                documents::CellLook::Container(ref size) if size == "{2 fields}"
            ));
        }

        #[test]
        fn expands_a_column_in_place_and_starts_the_grid_over() {
            let mut console = console_with_documents(true);
            let run = console.run.as_mut().unwrap();
            let statement = &mut run.statements[0];
            statement.grid.toggle_sort(4);
            statement.fold_column(&ColumnFold::Expand(2));
            assert_eq!(
                names(statement),
                [
                    "_id",
                    "name",
                    "address.city",
                    "address.zip",
                    "note",
                    "total",
                    "tags"
                ]
            );
            assert_eq!(statement.rows[0][2].as_deref(), Some("Leeds"));
            assert_eq!(statement.grid.sort(), None, "the sorted column moved");
        }

        #[test]
        fn is_read_only_and_not_arranged_in_the_database() {
            let mut console = console_with_documents(true);
            assert!(console.tables_to_describe().is_empty());
            let statement = statement(&console);
            assert!(matches!(
                statement.edits.editability,
                Editability::ReadOnly(_)
            ));
            assert!(statement.can_arrange(Dialect::Postgres).is_err());
        }

        /// The export command's file written, as the engine writes it.
        fn exported(console: &mut QueryConsole, format: ExportFormat) -> String {
            let source = statement(console).row_source(None);
            let spec = format.default_spec(&source).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("export");
            let Some(Command::ExportRows { export, .. }) = console.export(spec, path) else {
                panic!("no export");
            };
            dbrover_export::export_rows(&export.path, &export.spec, &export.columns, &export.rows)
                .unwrap();
            std::fs::read_to_string(&export.path).unwrap()
        }

        #[test]
        fn exports_documents_to_json_lines_that_read_back_the_same() {
            let mut console = console_with_documents(true);
            let written = exported(&mut console, ExportFormat::JsonLines);
            let read_back: Vec<dbrover_document::Value> = written
                .lines()
                .map(|line| dbrover_document::Value::parse(line).unwrap())
                .collect();
            let sent: Vec<dbrover_document::Value> = FIRST_PAGE
                .iter()
                .chain(&SECOND_PAGE)
                .map(|document| dbrover_document::Value::parse(document).unwrap())
                .collect();
            assert_eq!(read_back, sent);
        }

        #[test]
        fn exports_and_copies_the_flattened_columns_in_the_other_formats() {
            let mut console = console_with_documents(true);
            assert_eq!(
                exported(&mut console, ExportFormat::Csv),
                "_id,name,address,note,total,tags\r\n\
                 \"ObjectId(\"\"5f1d7f0e2b5c3a0001a1b2c3\"\")\",Ada,\"{\"\"city\"\":\"\"Leeds\"\",\"\"zip\"\":\"\"LS1\"\"}\",,,\r\n\
                 \"ObjectId(\"\"5f1d7f0e2b5c3a0001a1b2c4\"\")\",Bo,,,NumberLong(7),\r\n\
                 \"ObjectId(\"\"5f1d7f0e2b5c3a0001a1b2c5\"\")\",,,,,\"[\"\"a\"\",\"\"b\"\"]\"\r\n"
            );

            let statement = &mut console.run.as_mut().unwrap().statements[0];
            statement.grid.refresh(&statement.rows);
            statement.grid.select(grid::CellPos { row: 0, column: 1 });
            statement
                .grid
                .extend_selection(grid::CellPos { row: 1, column: 1 });
            let options = ExportOptions::default();
            let copy_as = |console: &mut QueryConsole, format| {
                console.copy(ClipboardCopy::As(format), &options)
            };
            assert_eq!(
                copy_as(&mut console, ClipboardFormat::Csv).as_deref(),
                Some("name\r\nAda\r\nBo\r\n")
            );
            let lines = copy_as(&mut console, ClipboardFormat::JsonLines).unwrap();
            assert_eq!(
                lines.lines().next(),
                Some(
                    r#"{"_id":{"$oid":"5f1d7f0e2b5c3a0001a1b2c3"},"name":"Ada","address":{"city":"Leeds","zip":"LS1"},"note":null}"#
                ),
                "JSON Lines copies the selected rows' documents whole"
            );
            assert_eq!(lines.lines().count(), 2);
            assert!(copy_as(&mut console, ClipboardFormat::SqlInsert).is_none());
            assert_eq!(
                console.export_status,
                Some(Err("Not for documents, which have no table".to_owned()))
            );
        }

        #[test]
        fn shows_the_grid_then_the_selected_document_as_a_tree() {
            let mut console = console_with_documents(true);
            let keymap = crate::keymap::Keymap::default();
            let mut harness = ui_harness(|ui| {
                let mut actions = Vec::new();
                results::show(ui, &mut console, &keymap, &[], &mut actions);
            });
            harness.get_by_label("3 documents");
            harness.get_by_label("{2 fields}");
            harness.get_by_label("Ada");

            harness.get_by_label("address").click_secondary();
            harness.run();
            harness.get_by_label("Expand column").click();
            harness.run();
            harness.get_by_label("address.zip");
            harness.get_by_label("Leeds");

            harness.get_by_label("Bo").click();
            harness.run();
            harness.get_by_label("Tree").click();
            harness.run();
            harness.get_by_label_contains("Document 2 of 3 loaded");
            harness.get_by_label("NumberLong(7)");
            harness.get_by_label("Long");
            harness.get_by_label("JSON").click();
            harness.run();
            harness.get_by_value(
                "{\n  \"_id\": {\n    \"$oid\": \"5f1d7f0e2b5c3a0001a1b2c4\"\n  },\n  \"name\": \"Bo\",\n  \
                 \"total\": {\n    \"$numberLong\": \"7\"\n  }\n}",
            );
            drop(harness);
            assert_eq!(
                statement(&console).documents.as_ref().unwrap().shown_as,
                ShownAs::Json
            );
        }
    }
}

/// A Redis key's value, shown whole, is edited a command per change, and its key bar acts on it
/// (`docs/nosql.md`, N1).
mod redis_keys {
    use dbrover_core::ColumnInfo;
    use dbrover_engine::RowEdit;

    use super::*;

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            type_name: None,
        }
    }

    fn cells(values: &[&str]) -> Vec<Option<String>> {
        values
            .iter()
            .map(|value| Some((*value).to_owned()))
            .collect()
    }

    /// A console that ran `command`, whose result was `columns` and `rows`.
    fn shown(command: &str, columns: &[&str], rows: Vec<Row>) -> QueryConsole {
        let mut console = console_in(command, QueryLanguage::Redis);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: columns.iter().map(|name| column(name)).collect(),
            shape: ResultShape::Rows,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows,
            more: false,
        });
        assert!(console.tables_to_describe().is_empty());
        console
    }

    fn statement(console: &mut QueryConsole) -> &mut StatementView {
        &mut console.run.as_mut().unwrap().statements[0]
    }

    #[test]
    fn edits_a_hash_a_command_per_change() {
        let mut console = shown(
            "HSCAN user:42 0 COUNT 1000",
            &["field", "value"],
            vec![cells(&["name", "Ada"]), cells(&["city", "Paris"])],
        );
        let shown = statement(&mut console);
        assert!(matches!(shown.edits.editability, Editability::Key(_)));
        assert!(
            !shown.edits.is_editable(edit::RowRef::Loaded(0), 0),
            "a field names its row"
        );
        assert!(shown.edits.is_editable(edit::RowRef::Loaded(0), 1));
        let rows = shown.rows.clone();
        shown.edits.pending.set(
            &rows,
            edit::RowRef::Loaded(0),
            1,
            Some("Ada Lovelace".to_owned()),
        );
        shown.edits.delete_rows(&[edit::RowRef::Loaded(1)]);
        shown.edits.add_row();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::New(0), 0, Some("born".to_owned()));
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::New(0), 1, Some("1815".to_owned()));

        let Some(Command::ApplyEdits { statements, .. }) = console.submit_edits() else {
            panic!("nothing submitted");
        };
        let commands: Vec<&str> = statements.iter().map(|edit| edit.sql.as_str()).collect();
        assert_eq!(
            commands,
            [
                "HDEL user:42 city",
                "HSET user:42 name \"Ada Lovelace\"",
                "HSET user:42 born 1815",
            ]
        );
        let RowEdit { change, .. } = &statements[1];
        let revert = change.as_ref().and_then(|change| change.revert.as_deref());
        assert_eq!(revert, Some("HSET user:42 name Ada"));

        console.apply(&Event::EditsApplied {
            console: ConsoleId(1),
            result: Ok(()),
        });
        // A row was added, so the key is read again to show it as the server has it.
        assert!(console.take_reload().is_some());
    }

    #[test]
    fn an_added_field_needs_its_name() {
        let mut console = shown("HGETALL h", &["field", "value"], vec![cells(&["a", "1"])]);
        statement(&mut console).edits.add_row();
        assert!(console.submit_edits().is_none());
        assert_eq!(
            statement(&mut console).edits.error.as_deref(),
            Some("New row 1: type field and value first")
        );
    }

    #[test]
    fn partial_reads_are_read_only() {
        let mut console = shown("LRANGE q 0 10", &["value"], vec![cells(&["a"])]);
        assert!(matches!(
            statement(&mut console).edits.editability,
            Editability::ReadOnly(_)
        ));
        let mut console = shown("HGET h f", &["value"], vec![cells(&["a"])]);
        assert!(matches!(
            statement(&mut console).edits.editability,
            Editability::ReadOnly(_)
        ));
    }

    #[test]
    fn the_key_bar_renames_the_key_and_reads_it_again() {
        let mut console = shown("GET greeting", &["value"], vec![cells(&["hello"])]);
        let view = dbrover_language::redis::KeyView::of("GET greeting").unwrap();
        let renamed = view.renamed(b"welcome".to_vec());
        let run = console
            .key_command_run(view.rename(b"welcome"), &renamed)
            .unwrap();
        assert_eq!(
            sqls(&run.statements),
            ["RENAMENX greeting welcome", "GET welcome"]
        );
        assert_eq!(console.text, "GET welcome");
    }
}

/// A `find`'s documents are edited a MongoDB command per change, each field keeping its type
/// (`docs/nosql.md`, N2·6).
mod mongo_documents {
    use dbrover_core::ColumnInfo;

    use super::*;

    /// A console that ran `command`, whose result was `documents`, each canonical Extended JSON.
    fn shown(command: &str, documents: &[&str]) -> QueryConsole {
        let mut console = console_in(command, QueryLanguage::Mongo);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: vec![ColumnInfo {
                name: "document".to_owned(),
                type_name: None,
            }],
            shape: ResultShape::Documents,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows: documents
                .iter()
                .map(|document| vec![Some((*document).to_owned())])
                .collect(),
            more: false,
        });
        assert!(console.tables_to_describe().is_empty());
        console
    }

    fn statement(console: &mut QueryConsole) -> &mut StatementView {
        &mut console.run.as_mut().unwrap().statements[0]
    }

    fn column(console: &mut QueryConsole, name: &str) -> usize {
        statement(console)
            .columns
            .iter()
            .position(|column| column.name == name)
            .unwrap()
    }

    #[test]
    fn edits_fields_by_id_keeping_their_types() {
        let mut console = shown(
            "db.orders.find({})",
            &[
                r#"{"_id": {"$oid": "507f1f77bcf86cd799439011"}, "qty": {"$numberLong": "7"}, "city": "Paris"}"#,
                r#"{"_id": {"$numberInt": "2"}, "qty": {"$numberLong": "1"}}"#,
            ],
        );
        let (id, qty, city) = (
            column(&mut console, "_id"),
            column(&mut console, "qty"),
            column(&mut console, "city"),
        );
        let shown = statement(&mut console);
        assert!(matches!(shown.edits.editability, Editability::Documents(_)));
        assert!(
            !shown.edits.is_editable(edit::RowRef::Loaded(0), id),
            "_id names the document"
        );
        assert!(shown.edits.is_editable(edit::RowRef::Loaded(0), qty));
        let rows = shown.rows.clone();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::Loaded(0), qty, Some("8".to_owned()));
        // Row 1 has no city: setting one adds the field.
        shown.edits.pending.set(
            &rows,
            edit::RowRef::Loaded(1),
            city,
            Some("Rome".to_owned()),
        );
        shown.edits.add_row();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::New(0), qty, Some("3".to_owned()));
        shown.edits.delete_rows(&[edit::RowRef::Loaded(1)]);

        let Some(Command::ApplyEdits { statements, .. }) = console.submit_edits() else {
            panic!(
                "nothing submitted: {:?}",
                statement(&mut console).edits.error
            );
        };
        let commands: Vec<&str> = statements.iter().map(|edit| edit.sql.as_str()).collect();
        assert_eq!(
            commands,
            [
                r#"db.getCollection("orders").deleteOne({_id: 2})"#,
                r#"db.getCollection("orders").updateOne({_id: ObjectId("507f1f77bcf86cd799439011")}, {$set: {"qty": NumberLong("8")}})"#,
                r#"db.getCollection("orders").insertOne({qty: 3})"#,
            ]
        );
        let revert = statements[1]
            .change
            .as_ref()
            .and_then(|change| change.revert.as_deref());
        assert_eq!(
            revert,
            Some(
                r#"db.getCollection("orders").updateOne({_id: ObjectId("507f1f77bcf86cd799439011")}, {$set: {"qty": NumberLong(7)}})"#
            )
        );
        for edit in &statements {
            assert!(
                QueryLanguage::Mongo.classify(&edit.sql).is_ok(),
                "{}",
                edit.sql
            );
        }

        console.apply(&Event::EditsApplied {
            console: ConsoleId(1),
            result: Ok(()),
        });
        assert!(
            console.take_reload().is_some(),
            "the documents are read again"
        );
    }

    #[test]
    fn an_aggregations_documents_are_read_only() {
        let mut console = shown("db.orders.aggregate([])", &[r#"{"_id": 1}"#]);
        assert!(matches!(
            statement(&mut console).edits.editability,
            Editability::ReadOnly(_)
        ));
    }

    #[test]
    fn text_that_isnt_the_fields_type_is_refused_before_sending() {
        let mut console = shown(
            "db.orders.find()",
            &[r#"{"_id": 1, "qty": {"$numberInt": "7"}}"#],
        );
        let qty = column(&mut console, "qty");
        let shown = statement(&mut console);
        let rows = shown.rows.clone();
        shown.edits.pending.set(
            &rows,
            edit::RowRef::Loaded(0),
            qty,
            Some("seven".to_owned()),
        );
        assert!(console.submit_edits().is_none());
        let error = statement(&mut console)
            .edits
            .error
            .clone()
            .unwrap_or_default();
        assert!(error.contains("32-bit integer"), "{error}");
    }
}

/// A search's hits are edited a request per change, each found by its own `_index` and `_id`
/// (`docs/nosql.md`, N3·6).
mod search_hits {
    use dbrover_core::ColumnInfo;

    use super::*;

    /// A console that ran `request`, whose hits were `documents`.
    fn shown(request: &str, documents: &[&str]) -> QueryConsole {
        let mut console = console_in(request, QueryLanguage::Search);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: vec![ColumnInfo {
                name: "document".to_owned(),
                type_name: None,
            }],
            shape: ResultShape::Documents,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows: documents
                .iter()
                .map(|document| vec![Some((*document).to_owned())])
                .collect(),
            more: false,
        });
        assert!(console.tables_to_describe().is_empty());
        console
    }

    fn statement(console: &mut QueryConsole) -> &mut StatementView {
        &mut console.run.as_mut().unwrap().statements[0]
    }

    fn column(console: &mut QueryConsole, name: &str) -> usize {
        statement(console)
            .columns
            .iter()
            .position(|column| column.name == name)
            .unwrap()
    }

    #[test]
    fn edits_hits_by_index_and_id_keeping_their_types() {
        let mut console = shown(
            "GET /orders-live/_search",
            &[
                r#"{"_index":"orders-2026","_id":"1","_score":1.0,"total":12.5,"status":"open"}"#,
                r#"{"_index":"orders-2025","_id":"2","_score":1.0,"total":3}"#,
            ],
        );
        let (index, id, score, total, status) = (
            column(&mut console, "_index"),
            column(&mut console, "_id"),
            column(&mut console, "_score"),
            column(&mut console, "total"),
            column(&mut console, "status"),
        );
        let shown = statement(&mut console);
        assert!(matches!(shown.edits.editability, Editability::Documents(_)));
        for key in [index, id, score] {
            assert!(!shown.edits.is_editable(edit::RowRef::Loaded(0), key));
        }
        assert!(shown.edits.is_editable(edit::RowRef::Loaded(0), total));
        let rows = shown.rows.clone();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::Loaded(0), total, Some("13".to_owned()));
        shown.edits.pending.set(
            &rows,
            edit::RowRef::Loaded(1),
            status,
            Some("done".to_owned()),
        );
        shown.edits.add_row();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::New(0), status, Some("new".to_owned()));
        shown.edits.delete_rows(&[edit::RowRef::Loaded(1)]);

        let Some(Command::ApplyEdits { statements, .. }) = console.submit_edits() else {
            panic!(
                "nothing submitted: {:?}",
                statement(&mut console).edits.error
            );
        };
        let requests: Vec<&str> = statements.iter().map(|edit| edit.sql.as_str()).collect();
        assert_eq!(
            requests,
            [
                "DELETE /orders-2025/_doc/2?refresh=true",
                "POST /orders-2026/_update/1?refresh=true\n{\"doc\":{\"total\":13}}",
                "POST /orders-live/_doc?refresh=true\n{\"status\":\"new\"}",
            ]
        );
        for edit in &statements {
            assert_eq!(
                QueryLanguage::Search.classify(&edit.sql),
                Ok(dbrover_language::StatementKind::Write),
                "{}",
                edit.sql
            );
        }
        console.apply(&Event::EditsApplied {
            console: ConsoleId(1),
            result: Ok(()),
        });
        assert!(console.take_reload().is_some(), "the hits are read again");
    }

    #[test]
    fn an_aggregations_answer_is_read_only() {
        let mut console = shown(
            "GET /orders/_search\n{ \"size\": 0, \"aggs\": {} }",
            &[r#"{"took":1}"#],
        );
        let Editability::ReadOnly(reason) = &statement(&mut console).edits.editability else {
            panic!("an aggregation's answer can be edited");
        };
        assert!(reason.contains("hits of a search"), "{reason}");
    }
}

#[test]
fn the_empty_results_panel_names_the_run_keys_the_keymap_has() {
    use crate::keymap::{Action, Keymap};
    use crate::test_support::ui_harness;
    use egui_kittest::kittest::Queryable;

    let file = [
        ("console.run".to_owned(), vec!["f5".to_owned()]),
        ("console.run-all".to_owned(), vec!["shift+f5".to_owned()]),
    ]
    .into_iter()
    .collect();
    let keymap = Keymap::from_file(&file, &[]);
    let mut console = console("select 1");
    let harness = ui_harness(|ui| {
        let mut actions = Vec::new();
        results::show(ui, &mut console, &keymap, &[], &mut actions);
    });
    let hint = |action| keymap.hint(&harness.ctx, action).unwrap_or_default();
    let (current, all) = (hint(Action::RunCurrent), hint(Action::RunAll));
    assert!(
        current.contains("F5") && all.contains("F5"),
        "{current} {all}"
    );
    harness.get_by_label(&format!(
        "Run the statement at the cursor with {current}, or the whole console with {all}."
    ));
}

/// Charts from results (W12): the chart tab beside the grid.
mod charts {
    use dbrover_chart::{ChartKind, Mark};
    use egui_kittest::kittest::Queryable;

    use crate::test_support::ui_harness;

    use super::*;

    const MONTHS: &str = "SELECT to_char(day, 'YYYY-MM') AS month, sum(total) AS total \
                          FROM orders GROUP BY 1 ORDER BY 1";

    /// A console that ran `sql`, whose result was `columns` and `rows`, with no declared types,
    /// as PostgreSQL's text protocol answers.
    fn ran(console: &mut QueryConsole, run: RunId, columns: &[&str], rows: &[&[Option<&str>]]) {
        let statements = console.statements_to_run(RunScope::All);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: columns
                .iter()
                .map(|name| ColumnInfo {
                    name: (*name).to_owned(),
                    type_name: None,
                })
                .collect(),
            shape: ResultShape::Rows,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows: rows
                .iter()
                .map(|row| row.iter().map(|value| value.map(str::to_owned)).collect())
                .collect(),
            more: false,
        });
    }

    fn months_and_totals() -> QueryConsole {
        let mut console = console(MONTHS);
        ran(
            &mut console,
            RunId(1),
            &["month", "total"],
            &[
                &[Some("2026-01"), Some("120.50")],
                &[Some("2026-02"), Some("80")],
                &[Some("2026-03"), Some("200")],
            ],
        );
        console
    }

    fn tab(console: &QueryConsole) -> &chart::ChartTab {
        let run = console.run.as_ref().unwrap();
        &run.statements[run.selected].chart
    }

    fn series_marks(console: &QueryConsole) -> (usize, usize) {
        let drawing = tab(console).drawing().expect("the chart is drawn");
        let bars = drawing
            .marks
            .iter()
            .filter(
                |mark| matches!(mark, Mark::Rect { rect, .. } if rect.min.y >= drawing.plot.min.y),
            )
            .count();
        let lines = drawing
            .marks
            .iter()
            .filter(|mark| matches!(mark, Mark::Polyline { .. }))
            .count();
        (bars, lines)
    }

    #[test]
    fn charts_months_and_totals_as_bars_then_as_lines() {
        let mut console = months_and_totals();
        let keymap = crate::keymap::Keymap::default();
        let mut harness = ui_harness(|ui| {
            let mut actions = Vec::new();
            results::show(ui, &mut console, &keymap, &[], &mut actions);
        });
        harness.get_by_label("2026-02");
        harness.get_by_label("Chart").click();
        harness.run();
        harness.get_by_value("X: month");
        harness.get_by_label("Y: total");
        harness.get_by_label("Charting 3 rows");
        assert!(
            harness.query_by_label("2026-02").is_none(),
            "the grid is hidden"
        );
        harness.get_by_label("Lines").click();
        harness.run();
        harness.get_by_label("Grid").click();
        harness.run();
        harness.get_by_label("2026-02");
        harness.get_by_label("Chart").click();
        harness.run();
        drop(harness);

        let choice = tab(&console).choice().unwrap();
        assert_eq!(choice.kind, ChartKind::Lines);
        assert_eq!((choice.x, choice.ys.clone()), (0, vec![1]));
        assert_eq!(series_marks(&console), (0, 1), "one line, no bars");
    }

    #[test]
    fn opens_as_bars_over_months_written_as_text() {
        let mut console = months_and_totals();
        let keymap = crate::keymap::Keymap::default();
        let mut harness = ui_harness(|ui| {
            let mut actions = Vec::new();
            results::show(ui, &mut console, &keymap, &[], &mut actions);
        });
        harness.get_by_label("Chart").click();
        harness.run();
        drop(harness);
        assert_eq!(series_marks(&console), (3, 0));
    }

    #[test]
    fn a_result_without_numbers_has_nothing_to_chart() {
        let mut console = console("SELECT name, city FROM customers");
        ran(
            &mut console,
            RunId(1),
            &["name", "city"],
            &[&[Some("Ada"), Some("Leeds")]],
        );
        let keymap = crate::keymap::Keymap::default();
        let mut harness = ui_harness(|ui| {
            let mut actions = Vec::new();
            results::show(ui, &mut console, &keymap, &[], &mut actions);
        });
        harness.get_by_label("Chart").click();
        harness.run();
        harness.get_by_label("A chart needs a column of numbers");
    }

    #[test]
    fn running_again_keeps_the_chart_while_the_columns_are_the_same() {
        let mut console = months_and_totals();
        let keymap = crate::keymap::Keymap::default();
        {
            let mut harness = ui_harness(|ui| {
                let mut actions = Vec::new();
                results::show(ui, &mut console, &keymap, &[], &mut actions);
            });
            harness.get_by_label("Chart").click();
            harness.run();
            harness.get_by_label("Lines").click();
            harness.run();
        }
        ran(
            &mut console,
            RunId(2),
            &["month", "total"],
            &[&[Some("2026-04"), Some("1")]],
        );
        let harness = ui_harness(|ui| {
            let mut actions = Vec::new();
            results::show(ui, &mut console, &keymap, &[], &mut actions);
        });
        harness.get_by_label("Charting 1 row");
        drop(harness);
        assert!(tab(&console).is_shown());
        assert_eq!(tab(&console).choice().unwrap().kind, ChartKind::Lines);

        ran(
            &mut console,
            RunId(3),
            &["customer", "total"],
            &[&[Some("Ada"), Some("1")]],
        );
        let mut harness = ui_harness(|ui| {
            let mut actions = Vec::new();
            results::show(ui, &mut console, &keymap, &[], &mut actions);
        });
        harness.run();
        drop(harness);
        assert!(tab(&console).is_shown());
        assert_eq!(
            tab(&console).choice().unwrap().kind,
            ChartKind::Bars,
            "other columns start afresh"
        );
    }
}

/// A DynamoDB table's items are edited a statement per change, each found by its whole
/// primary key, which the table's description gives (`docs/nosql.md`, N5·7).
mod dynamodb_items {
    use dbrover_core::{ColumnDescription, ColumnInfo, Identifier, RelationName, TableDescription};

    use super::*;

    /// A console that ran `statement` on the `orders` table, keyed by `pk` and `sk`, whose
    /// items were `documents`, and was told the table's key.
    fn shown(statement: &str, documents: &[&str]) -> QueryConsole {
        let mut console = console_in(statement, QueryLanguage::PartiQl);
        let statements = console.statements_to_run(RunScope::All);
        let run = RunId(1);
        console.begin_run(run, &statements);
        console.apply(&Event::StatementStarted {
            run,
            statement: 0,
            columns: vec![ColumnInfo {
                name: "item".to_owned(),
                type_name: None,
            }],
            shape: ResultShape::Documents,
        });
        console.apply(&Event::RowsFetched {
            run,
            statement: 0,
            rows: documents
                .iter()
                .map(|document| vec![Some((*document).to_owned())])
                .collect(),
            more: false,
        });
        let table = RelationName {
            schema: None,
            name: Identifier {
                name: "orders".to_owned(),
                quoted: true,
            },
        };
        assert_eq!(console.tables_to_describe(), std::slice::from_ref(&table));
        let key = |name: &str, data_type: &str| ColumnDescription {
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            nullable: false,
            default: None,
            identity: None,
        };
        console.apply(&Event::TableDescribed {
            data_source: console.data_source(),
            table,
            result: Ok(Some(TableDescription {
                schema: String::new(),
                name: "orders".to_owned(),
                columns: vec![key("pk", "S"), key("sk", "N")],
                primary_key: vec!["pk".to_owned(), "sk".to_owned()],
                foreign_keys: Vec::new(),
                referenced_by: Vec::new(),
            })),
        });
        console
    }

    fn statement(console: &mut QueryConsole) -> &mut StatementView {
        &mut console.run.as_mut().unwrap().statements[0]
    }

    fn column(console: &mut QueryConsole, name: &str) -> usize {
        statement(console)
            .columns
            .iter()
            .position(|column| column.name == name)
            .unwrap()
    }

    #[test]
    fn edits_items_by_their_whole_key_keeping_their_types() {
        let mut console = shown(
            r#"SELECT * FROM "orders" WHERE pk = 'c-1'"#,
            &[
                r#"{"pk":"c-1","sk":1,"total":12.5,"tags":{"$stringSet":["a"]}}"#,
                r#"{"pk":"c-1","sk":2,"total":3}"#,
            ],
        );
        let (pk, sk, total, tags) = (
            column(&mut console, "pk"),
            column(&mut console, "sk"),
            column(&mut console, "total"),
            column(&mut console, "tags"),
        );
        let shown = statement(&mut console);
        assert!(matches!(shown.edits.editability, Editability::Documents(_)));
        for key in [pk, sk] {
            assert!(!shown.edits.is_editable(edit::RowRef::Loaded(0), key));
        }
        let rows = shown.rows.clone();
        shown
            .edits
            .pending
            .set(&rows, edit::RowRef::Loaded(0), total, Some("13".to_owned()));
        shown.edits.pending.set(
            &rows,
            edit::RowRef::Loaded(0),
            tags,
            Some(r#"["a", "b"]"#.to_owned()),
        );
        shown.edits.add_row();
        for (column, value) in [(pk, "c-2"), (sk, "1")] {
            shown
                .edits
                .pending
                .set(&rows, edit::RowRef::New(0), column, Some(value.to_owned()));
        }
        shown.edits.delete_rows(&[edit::RowRef::Loaded(1)]);

        let Some(Command::ApplyEdits { statements, .. }) = console.submit_edits() else {
            panic!(
                "nothing submitted: {:?}",
                statement(&mut console).edits.error
            );
        };
        let written: Vec<&str> = statements.iter().map(|edit| edit.sql.as_str()).collect();
        assert_eq!(
            written,
            [
                r#"DELETE FROM "orders" WHERE "pk" = 'c-1' AND "sk" = 2"#,
                r#"UPDATE "orders" SET "total" = 13 WHERE "pk" = 'c-1' AND "sk" = 1"#,
                r#"UPDATE "orders" SET "tags" = <<'a', 'b'>> WHERE "pk" = 'c-1' AND "sk" = 1"#,
                r#"INSERT INTO "orders" VALUE {'pk': 'c-2', 'sk': 1}"#,
            ]
        );
        for edit in &statements {
            assert_eq!(
                QueryLanguage::PartiQl.classify(&edit.sql),
                Ok(dbrover_language::StatementKind::Write),
                "{}",
                edit.sql
            );
        }
    }

    #[test]
    fn an_indexs_items_are_read_only() {
        let mut console = console_in(
            r#"SELECT * FROM "orders"."by-status""#,
            QueryLanguage::PartiQl,
        );
        let statements = console.statements_to_run(RunScope::All);
        console.begin_run(RunId(1), &statements);
        console.apply(&Event::StatementStarted {
            run: RunId(1),
            statement: 0,
            columns: vec![ColumnInfo {
                name: "item".to_owned(),
                type_name: None,
            }],
            shape: ResultShape::Documents,
        });
        assert!(console.tables_to_describe().is_empty());
        assert!(matches!(
            statement(&mut console).edits.editability,
            Editability::ReadOnly(_)
        ));
    }
}
