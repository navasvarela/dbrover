//! Requests to Elasticsearch's and OpenSearch's REST APIs, as Kibana's Dev Tools and
//! OpenSearch Dashboards write them, and what each may change: the language of their consoles
//! (`docs/nosql.md`, N3).
//!
//! A request is a line with its method and path, then an optional body:
//!
//! ```text
//! GET /orders/_search
//! { "query": { "match": { "status": "open" } } }
//! ```
//!
//! The body is JSON, or one JSON value a line for `_bulk` and `_msearch`. A line starting with
//! `#` or `//` is a comment. The body is read to classify the request and is sent as typed, so
//! numbers keep every digit and the server reads what is on screen.

mod classify;
mod lexer;
mod parser;
#[cfg(test)]
mod tests;
mod words;

use std::ops::Range;

use dbrover_document::Value;
use dbrover_sql::{StatementKind, Token, Unclassified};

pub use parser::{Body, Method, ParseError, Request, parse};

pub(crate) use lexer::tokenize;
pub(crate) use words::{completion_words, detail as syntax, is_keyword};

/// One range per request, from its first to its last meaningful token. A request starts at a
/// line whose first word is a method, so its body may span any number of lines; text before
/// the first such line is a request of its own, which doesn't parse.
pub(crate) fn split(script: &str) -> Vec<Range<usize>> {
    let tokens: Vec<Token> = tokenize(script);
    let mut requests = Vec::new();
    let mut current: Option<Range<usize>> = None;
    let mut line_start = true;
    for token in &tokens {
        if token.kind.is_trivia() {
            line_start = line_start || script[token.range.clone()].contains('\n');
            continue;
        }
        let starts_request =
            line_start && Method::from_word(&script[token.range.clone()]).is_some();
        line_start = false;
        if starts_request {
            requests.extend(current.take());
        }
        current = Some(match current {
            Some(request) => request.start..token.range.end,
            None => token.range.clone(),
        });
    }
    requests.extend(current);
    requests
}

/// What **Open data** runs on an index, alias or data stream: a search of every document of
/// it, read a page at a time.
pub fn search_all(index: &str) -> String {
    format!("GET /{index}/_search\n{{\n  \"query\": {{ \"match_all\": {{}} }}\n}}")
}

/// Whether `request` is a search of an index's documents: `GET` or `POST` to `_search`, with
/// or without indices before it.
pub fn is_search(request: &Request) -> bool {
    matches!(request.method, Method::Get | Method::Post)
        && matches!(request.segments().as_slice(), [.., "_search"])
}

/// Whether `request` returns documents, as Run to file must know before it runs: a search,
/// whose hits are documents, or whose whole answer is one.
pub fn returns_documents(request: &str) -> bool {
    parse(request).is_ok_and(|request| is_search(&request))
}

/// `request`, a search, with `"profile": true` at the end of its body, which runs it and
/// says what each part of its query cost on each shard; `None` for any other request. The
/// text before the insertion is kept, so a position in it still points where it did.
pub fn explain(request: &str) -> Option<String> {
    let parsed = parse(request).ok()?;
    if !is_search(&parsed) {
        return None;
    }
    let Some(body) = &parsed.body else {
        return Some(format!("{}\n{{ \"profile\": true }}", request.trim_end()));
    };
    let Body::Json {
        text,
        value: Value::Object(fields),
        offset,
    } = body
    else {
        return None;
    };
    if fields.iter().any(|(name, _)| name == "profile") {
        return Some(request.to_owned());
    }
    // The body's text ends with the object's closing brace; the field goes after the last
    // thing before it.
    let close = offset + text.len() - 1;
    let at = request[..close].trim_end().len();
    let separator = if fields.is_empty() { "" } else { ", " };
    Some(format!(
        "{}{separator}\"profile\": true{}",
        &request[..at],
        &request[at..]
    ))
}

/// The one request `text` holds, read.
fn single(text: &str) -> Result<Request, Unclassified> {
    match split(text).as_slice() {
        [] => Err(Unclassified::Empty),
        [range] => {
            parse(&text[range.clone()]).map_err(|error| Unclassified::Unparseable(error.message))
        }
        several => Err(Unclassified::Several(several.len())),
    }
}

/// What `request` may change.
pub(crate) fn classify(request: &str) -> Result<StatementKind, Unclassified> {
    single(request).map(|request| classify::kind(&request))
}

/// Why `request` may touch everything, for the warning before it runs.
pub(crate) fn hazard(request: &str) -> Option<&'static str> {
    single(request).ok().as_ref().and_then(classify::hazard)
}
