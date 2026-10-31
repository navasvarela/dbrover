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
