use dbrover_document::Value;

use super::*;

fn requests(script: &str) -> Vec<&str> {
    split(script)
        .into_iter()
        .map(|range| &script[range])
        .collect()
}

fn request(text: &str) -> Request {
    parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn error(text: &str) -> ParseError {
    match parse(text) {
        Err(error) => error,
        Ok(request) => panic!("{text} parsed as {request:?}"),
    }
}

#[test]
fn splits_where_a_line_starts_with_a_method() {
    let cases: [(&str, &[&str]); 5] = [
        (
            "GET /\nGET /orders/_search\n{\n  \"query\": {}\n}\n\nDELETE /orders",
            &[
                "GET /",
                "GET /orders/_search\n{\n  \"query\": {}\n}",
                "DELETE /orders",
            ],
        ),
        (
            "# health\nGET _cluster/health\n// then\npost /orders/_count",
            &["GET _cluster/health", "post /orders/_count"],
        ),
        (
            "POST /_bulk\n{\"index\":{}}\n{\"get\":1}\n  GET /x",
            &["POST /_bulk\n{\"index\":{}}\n{\"get\":1}", "GET /x"],
        ),
        ("{}\nGET /", &["{}", "GET /"]),
        ("", &[]),
    ];
    for (script, expected) in cases {
        assert_eq!(requests(script), expected, "{script:?}");
    }
}

#[test]
fn a_method_inside_a_string_or_mid_line_does_not_split() {
    assert_eq!(
        requests("POST /a/_search\n{ \"q\": \"GET /b\" } GET\n"),
        ["POST /a/_search\n{ \"q\": \"GET /b\" } GET"]
    );
}

#[test]
fn reads_the_method_the_path_and_the_query_string() {
    let read = request("get orders/_search?q=status:open&size=5");
    assert_eq!(read.method, Method::Get);
    assert_eq!(read.path, "/orders/_search");
    assert_eq!(read.query, "q=status:open&size=5");
    assert_eq!(read.segments(), ["orders", "_search"]);
    assert_eq!(read.parameter("size"), Some("5"));
    assert_eq!(read.parameter("from"), None);
    assert_eq!(read.target(), "/orders/_search?q=status:open&size=5");
    assert_eq!(read.body, None);
    assert_eq!(request("HEAD /").segments(), Vec::<&str>::new());
    assert_eq!(request("GET /_cat/indices?v").parameter("v"), Some(""));
}

#[test]
fn keeps_the_body_as_typed() {
    let text =
        "POST /orders/_search\n{\n  \"size\": 10.50,\n  \"query\": { \"match_all\": {} }\n}\n";
    let body = request(text).body.unwrap();
    assert_eq!(
        body.text(),
        "{\n  \"size\": 10.50,\n  \"query\": { \"match_all\": {} }\n}"
    );
    assert_eq!(body.content_type(), "application/json");
    let Some(Value::Object(fields)) = body.value() else {
        panic!("{body:?}");
    };
    assert_eq!(
        fields[0],
        ("size".to_owned(), Value::Number("10.50".to_owned()))
    );
}

#[test]
fn a_body_may_start_on_the_request_line() {
    let read = request("POST /orders/_count { \"query\": { \"match_all\": {} } }");
    assert_eq!(read.path, "/orders/_count");
    assert_eq!(
        read.body.map(|body| body.text()),
        Some("{ \"query\": { \"match_all\": {} } }".to_owned())
    );
}

#[test]
fn comment_lines_in_a_body_are_left_out() {
    let read = request("POST /a/_search\n{\n  # every document\n  \"query\": {}\n}");
    let text = read.body.unwrap().text();
    assert!(!text.contains('#'), "{text}");
    assert!(Value::parse(&text).is_ok(), "{text}");
}

#[test]
fn an_ndjson_body_is_sent_a_value_a_line() {
    let read = request(
        "POST /_bulk\n{ \"index\": { \"_index\": \"orders\" } }\n{\n  \"status\": \"open\"\n}\n",
    );
    let body = read.body.unwrap();
    assert_eq!(body.content_type(), "application/x-ndjson");
    assert_eq!(
        body.text(),
        "{ \"index\": { \"_index\": \"orders\" } }\n{   \"status\": \"open\" }\n"
    );
    assert_eq!(body.value(), None);
    let Body::Lines(lines) = request("GET /a/_msearch/template\n{}\n{\"id\": \"t\"}")
        .body
        .unwrap()
    else {
        panic!("expected NDJSON");
    };
    assert_eq!(lines.len(), 2);
}

#[test]
fn errors_point_at_what_cant_be_read() {
    let cases: [(&str, &str, &str); 5] = [
        (
            "/orders/_search",
            "a request starts with GET, POST, PUT, DELETE or HEAD, then its path",
            "/",
        ),
        ("GET", "GET needs a path, such as /orders/_search", "GET"),
        (
            "POST /a/_search\n{ query: {} }",
            "the body isn't JSON: expected a field name",
            "q",
        ),
        (
            "POST /_bulk\n{}\n{ \"a\": 1",
            "a value of the body isn't closed",
            "{",
        ),
        (
            "POST /a/_search\n{ \"q\": \"\"\"x\"\"\" }",
            "triple-quoted strings aren't read; write the string in JSON, with \\n for its line breaks",
            "\"\"\"",
        ),
    ];
    for (text, message, pointed) in cases {
        let error = error(text);
        assert_eq!(
            (error.message.as_str(), &text[error.range.clone()]),
            (message, pointed),
            "{text}"
        );
    }
}

#[test]
fn tokens_cover_every_byte_and_cut_the_path() {
    let text =
        "GET /orders/_search?size=5\n{ \"query\": { \"term\": { \"n\": -1.5e3 } } } // x\n# note\n";
    let tokens = tokenize(text);
    let mut end = 0;
    for token in &tokens {
        assert_eq!(token.range.start, end, "{tokens:?}");
        end = token.range.end;
    }
    assert_eq!(end, text.len());
    let kinds: Vec<(dbrover_sql::TokenKind, &str)> = tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| (token.kind, &text[token.range.clone()]))
        .take(9)
        .collect();
    use dbrover_sql::TokenKind::{Punctuation, String, Word};
    assert_eq!(
        kinds,
        [
            (Word, "GET"),
            (Punctuation, "/"),
            (Word, "orders"),
            (Punctuation, "/"),
            (Word, "_search"),
            (Punctuation, "?"),
            (Word, "size"),
            (Punctuation, "="),
            (Word, "5"),
        ]
    );
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == String && &text[token.range.clone()] == "\"query\"")
    );
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == dbrover_sql::TokenKind::LineComment)
    );
}

#[test]
fn an_unclosed_string_is_a_word_to_complete() {
    let text = "POST /a/_search\n{ \"query\": { \"mat";
    let tokens = tokenize(text);
    let last = tokens.last().unwrap();
    assert_eq!(
        (last.kind, &text[last.range.clone()]),
        (dbrover_sql::TokenKind::Word, "mat")
    );
}

#[test]
fn classifies_one_request_at_a_time() {
    assert_eq!(classify(""), Err(Unclassified::Empty));
    assert_eq!(classify("GET /\nGET /"), Err(Unclassified::Several(2)));
    assert_eq!(classify("GET /orders/_search"), Ok(StatementKind::Read));
}

/// Each request in `classify.txt` follows a comment naming what it must be classified as,
/// `# expect: read` (or `write`, `schema`, `admin`, `unparseable`), as the other languages'
/// corpora do.
#[test]
fn classifies_its_corpus() {
    const EXPECT: &str = "# expect:";
    let corpus = include_str!("classify.txt");
    let mut previous_end = 0;
    let mut checked = 0;
    for range in split(corpus) {
        let before = &corpus[previous_end..range.start];
        previous_end = range.end;
        let request = &corpus[range];
        let expected = before
            .lines()
            .filter_map(|line| line.trim().strip_prefix(EXPECT))
            .next_back()
            .unwrap_or_else(|| panic!("no `{EXPECT}` line before {request}"))
            .trim();
        let actual = match classify(request) {
            Ok(StatementKind::Read) => "read",
            Ok(StatementKind::Write) => "write",
            Ok(StatementKind::Schema) => "schema",
            Ok(StatementKind::Transaction) => "transaction",
            Ok(StatementKind::Admin) => "admin",
            Err(Unclassified::Unparseable(_)) => "unparseable",
            Err(error) => panic!("{request}: {error}"),
        };
        assert_eq!(actual, expected, "{request}");
        checked += 1;
    }
    assert!(checked > 70, "only {checked} requests checked");
}

#[test]
fn warns_of_requests_that_may_touch_everything() {
    let cases: [(&str, bool); 9] = [
        ("POST /orders/_delete_by_query", true),
        (
            "POST /orders/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
            true,
        ),
        (
            "POST /orders/_delete_by_query\n{ \"query\": { \"term\": { \"status\": \"done\" } } }",
            false,
        ),
        ("POST /orders/_update_by_query\n{ \"script\": \"x\" }", true),
        ("DELETE /orders", true),
        ("DELETE /logs-*", true),
        ("DELETE /_all", true),
        ("DELETE /orders/_doc/1", false),
        ("GET /orders/_search", false),
    ];
    for (request, warns) in cases {
        assert_eq!(hazard(request).is_some(), warns, "{request}");
    }
    assert_eq!(
        hazard("DELETE /logs-*"),
        Some("DELETE of a pattern or _all deletes every index it matches, with their documents.")
    );
}

#[test]
fn completes_methods_endpoints_and_query_words() {
    let words = completion_words();
    assert!(words.is_sorted());
    for word in ["GET", "_search", "match_all", "date_histogram", "keyword"] {
        assert!(words.contains(&word), "{word}");
    }
    assert_eq!(syntax("_search"), Some("GET /<index>/_search"));
    assert_eq!(syntax("match"), Some("query"));
    assert_eq!(syntax("size"), None);
    assert!(is_keyword("get"));
    assert!(is_keyword("_bulk"));
    assert!(!is_keyword("orders"));
}

#[test]
fn opening_an_index_searches_every_document() {
    let text = search_all("orders-live");
    let read = request(&text);
    assert_eq!(read.path, "/orders-live/_search");
    assert_eq!(classify(&text), Ok(StatementKind::Read));
}

#[test]
fn explains_a_search_by_profiling_it() {
    let cases: [(&str, Option<&str>); 6] = [
        (
            "GET /orders/_search",
            Some("GET /orders/_search\n{ \"profile\": true }"),
        ),
        (
            "POST /orders/_search\n{\n  \"query\": { \"match_all\": {} }\n}\n# done",
            Some(
                "POST /orders/_search\n{\n  \"query\": { \"match_all\": {} }, \"profile\": true\n}\n# done",
            ),
        ),
        ("GET /_search {}", Some("GET /_search {\"profile\": true}")),
        (
            "GET /orders/_search\n{ \"profile\": true }",
            Some("GET /orders/_search\n{ \"profile\": true }"),
        ),
        ("GET /orders/_count", None),
        ("DELETE /orders/_search", None),
    ];
    for (request, expected) in cases {
        let explained = explain(request);
        assert_eq!(explained.as_deref(), expected, "{request}");
        if let Some(explained) = explained {
            assert_eq!(classify(&explained), Ok(StatementKind::Read), "{explained}");
        }
    }
    assert!(returns_documents("GET /orders/_search"));
    assert!(!returns_documents("GET /orders/_count"));
}

mod edits {
    use dbrover_document::{Path, Step, Value};

    use crate::{DocumentView, QueryLanguage};

    use super::super::typed_value;

    fn view(command: &str) -> DocumentView {
        QueryLanguage::Search
            .document_view(command)
            .unwrap_or_else(|| panic!("{command} has no view"))
    }

    fn hit(json: &str) -> Value {
        Value::parse(json).unwrap()
    }

    fn path(names: &[&str]) -> Path {
        names.iter().fold(Path::default(), |path, name| {
            path.child(Step::Field((*name).to_owned()))
        })
    }

    #[test]
    fn only_a_searchs_hits_are_edited() {
        let search = QueryLanguage::Search;
        assert!(search.document_view("GET /orders/_search").is_some());
        assert!(
            search
                .document_view("GET /orders,logs/_search?q=a")
                .is_some()
        );
        for command in [
            "GET /orders/_search\n{ \"aggs\": {} }",
            "GET /orders/_search\n{ \"size\": 0 }",
            "GET /orders/_count",
            "GET /_cat/indices",
        ] {
            assert!(search.document_view(command).is_none(), "{command}");
        }
        let orders = view("GET /orders/_search");
        assert_eq!(orders.name(), "orders");
        assert_eq!(view("GET /_search").name(), "_all");
        for (column, key) in [
            ("_index", true),
            ("_id", true),
            ("_score", true),
            ("total", false),
        ] {
            assert_eq!(orders.is_key(column), key, "{column}");
        }
    }

    #[test]
    fn a_changed_field_is_a_partial_document_with_its_old_type() {
        let orders = view("GET /orders/_search");
        let document = hit(
            r#"{"_index":"orders-2026","_id":"a/1","_score":1.0,"total":12.50,"address":{"city":"Madrid"}}"#,
        );
        let (edit, label) = orders
            .update(
                &document,
                &path(&["address", "city"]),
                Some(&Value::String("Madrid".to_owned())),
                Some("8"),
            )
            .unwrap();
        assert_eq!(label, "address.city");
        assert_eq!(
            edit.command,
            "POST /orders-2026/_update/a%2F1?refresh=true\n{\"doc\":{\"address\":{\"city\":\"8\"}}}"
        );
        assert_eq!(
            edit.revert.as_deref(),
            Some(
                "POST /orders-2026/_update/a%2F1?refresh=true\n{\"doc\":{\"address\":{\"city\":\"Madrid\"}}}"
            )
        );
        assert_eq!(
            edit.key,
            [
                ("_index".to_owned(), "orders-2026".to_owned()),
                ("_id".to_owned(), "a/1".to_owned())
            ]
        );
        let (edit, _) = orders
            .update(&document, &path(&["note"]), None, Some("[1, 2]"))
            .unwrap();
        assert!(
            edit.command.ends_with("{\"doc\":{\"note\":[1,2]}}"),
            "{}",
            edit.command
        );
        let revert = edit.revert.unwrap();
        assert!(
            revert.contains("ctx._source.remove(params.last)"),
            "{revert}"
        );
        let wrong = orders.update(
            &document,
            &path(&["total"]),
            Some(&Value::Number("12.50".to_owned())),
            Some("abc"),
        );
        assert_eq!(wrong, Err("\u{201c}abc\u{201d} isn't a number".to_owned()));
        assert!(
            orders
                .update(&document, &path(&["_id"]), None, Some("2"))
                .is_err()
        );
    }

    #[test]
    fn a_deleted_hit_can_be_put_back_with_its_routing() {
        let orders = view("GET /orders/_search");
        let edit = orders
            .delete(&hit(
                r#"{"_index":"orders","_id":"1","_score":null,"_routing":"eu","status":"open"}"#,
            ))
            .unwrap();
        assert_eq!(
            edit.command,
            "DELETE /orders/_doc/1?refresh=true&routing=eu"
        );
        assert_eq!(
            edit.revert.as_deref(),
            Some("PUT /orders/_doc/1?refresh=true&routing=eu\n{\"status\":\"open\"}")
        );
        let highlighted = orders
            .delete(&hit(
                r#"{"_index":"orders","_id":"2","highlight":{"a":["x"]}}"#,
            ))
            .unwrap();
        assert_eq!(highlighted.revert, None);
        assert!(orders.delete(&hit(r#"{"_id":"3"}"#)).is_err());
    }

    #[test]
    fn an_added_row_is_a_new_document_in_the_searchs_index() {
        let orders = view("GET /orders/_search");
        assert_eq!(
            orders.insert(&[("status", Some("open")), ("address.city", Some("Madrid")), ("total", Some("12.50"))]),
            Ok("POST /orders/_doc?refresh=true\n{\"status\":\"open\",\"address\":{\"city\":\"Madrid\"},\"total\":12.50}".to_owned())
        );
        assert_eq!(
            orders.insert(&[("_id", Some("7")), ("n", Some("1"))]),
            Ok("PUT /orders/_doc/7?refresh=true\n{\"n\":1}".to_owned())
        );
        let every = view("GET /_search");
        assert!(every.insert(&[("n", Some("1"))]).is_err());
        assert_eq!(
            every.insert(&[("_index", Some("logs")), ("n", Some("1"))]),
            Ok("POST /logs/_doc?refresh=true\n{\"n\":1}".to_owned())
        );
        assert!(orders.insert(&[]).is_err());
        let edits = [
            orders.insert(&[("n", Some("1"))]).unwrap(),
            orders
                .delete(&hit(r#"{"_index":"orders","_id":"1"}"#))
                .unwrap()
                .command,
        ];
        for edit in edits {
            assert_eq!(
                QueryLanguage::Search.classify(&edit),
                Ok(StatementKind::Write),
                "{edit}"
            );
        }
    }

    /// A field's old value, the text typed over it, and what it becomes.
    type TypedCase = (Option<Value>, Option<&'static str>, Result<Value, String>);

    #[test]
    fn typed_text_keeps_the_old_values_json_type() {
        let cases: [TypedCase; 8] = [
            (
                Some(Value::String("a".to_owned())),
                Some("8"),
                Ok(Value::String("8".to_owned())),
            ),
            (
                Some(Value::Number("1".to_owned())),
                Some("8.50"),
                Ok(Value::Number("8.50".to_owned())),
            ),
            (
                Some(Value::Bool(true)),
                Some("false"),
                Ok(Value::Bool(false)),
            ),
            (
                Some(Value::Bool(true)),
                Some("no"),
                Err("\u{201c}no\u{201d} isn't true or false".to_owned()),
            ),
            (None, Some("12"), Ok(Value::Number("12".to_owned()))),
            (None, Some("open"), Ok(Value::String("open".to_owned()))),
            (
                Some(Value::Null),
                Some("{\"a\": 1}"),
                Ok(Value::Object(vec![(
                    "a".to_owned(),
                    Value::Number("1".to_owned()),
                )])),
            ),
            (Some(Value::Number("1".to_owned())), None, Ok(Value::Null)),
        ];
        for (old, text, expected) in cases {
            assert_eq!(
                typed_value(old.as_ref(), text),
                expected,
                "{old:?} {text:?}"
            );
        }
    }

    use dbrover_sql::StatementKind;
}
