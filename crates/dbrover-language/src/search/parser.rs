//! A request as the console holds it, read into its method, path, query string and body.

use std::fmt;
use std::ops::Range;

use dbrover_document::Value;
use dbrover_sql::{Token, TokenKind};

use super::lexer::tokenize;

/// The HTTP methods the servers' APIs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Head,
}

impl Method {
    pub const ALL: [Self; 5] = [Self::Get, Self::Post, Self::Put, Self::Delete, Self::Head];

    /// The method `word` names, in any case, as Kibana reads it.
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str().eq_ignore_ascii_case(word))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One request: a method and path, and a body, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// The path as typed, starting with `/`, without its query string.
    pub path: String,
    /// The query string as typed, without its `?`; empty when there is none.
    pub query: String,
    pub body: Option<Body>,
}

/// A request's body: one JSON value, or one a line for the endpoints that take NDJSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Json { text: String, value: Value },
    Lines(Vec<(String, Value)>),
}

impl Body {
    /// What is sent: the JSON as typed, or each line's value on a line of its own, the last
    /// line ended too, as `_bulk` requires.
    pub fn text(&self) -> String {
        match self {
            Self::Json { text, .. } => text.clone(),
            Self::Lines(lines) => lines.iter().map(|(text, _)| format!("{text}\n")).collect(),
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Json { .. } => "application/json",
            Self::Lines(_) => "application/x-ndjson",
        }
    }

    /// The body's one JSON value; `None` for NDJSON.
    pub fn value(&self) -> Option<&Value> {
        match self {
            Self::Json { value, .. } => Some(value),
            Self::Lines(_) => None,
        }
    }
}

impl Request {
    /// The path's parts, between its slashes, as typed.
    pub fn segments(&self) -> Vec<&str> {
        self.path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect()
    }

    /// The path and its query string, as sent.
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }

    /// The query string's parameters, each with its value when it has one.
    pub fn parameters(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (pair, None),
            })
    }

    /// The value of the query string's `name`, when it is there.
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters()
            .find(|(parameter, _)| *parameter == name)
            .map(|(_, value)| value.unwrap_or(""))
    }

    /// The field `name` of the body's top-level object.
    pub fn body_field(&self, name: &str) -> Option<&Value> {
        let Some(Value::Object(fields)) = self.body.as_ref()?.value() else {
            return None;
        };
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }
}

/// Why a request can't be read, with where in it the trouble is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// The bytes of the request the message is about.
    pub range: Range<usize>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

/// The endpoints whose body is NDJSON, one value a line, by the path's last part (and the one
/// before it for `_msearch/template`).
const NDJSON_ENDPOINTS: &[&str] = &["_bulk", "_msearch"];

/// Reads one request: a method and path on its first line, then an optional body. A line
/// starting with `#` or `//` is a comment, and isn't sent.
///
/// # Errors
///
/// [`ParseError`] when the text doesn't start with a method and a path, or the body isn't
/// JSON (a value a line, for `_bulk` and `_msearch`).
pub fn parse(text: &str) -> Result<Request, ParseError> {
    let tokens = tokenize(text);
    let mut meaningful = tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .peekable();
    let Some(first) = meaningful.next() else {
        return Err(error("there is no request", 0..0));
    };
    let Some(method) = Method::from_word(&text[first.range.clone()]) else {
        return Err(error(
            "a request starts with GET, POST, PUT, DELETE or HEAD, then its path",
            first.range.clone(),
        ));
    };
    let line_end = text[first.range.end..]
        .find('\n')
        .map_or(text.len(), |end| first.range.end + end);
    let after_method = &text[first.range.end..line_end];
    let path_start = first.range.end + (after_method.len() - after_method.trim_start().len());
    let path_end = text[path_start..line_end]
        .find(char::is_whitespace)
        .map_or(line_end, |end| path_start + end);
    if path_start == path_end {
        return Err(error(
            format!("{method} needs a path, such as /orders/_search"),
            first.range.clone(),
        ));
    }
    let typed = &text[path_start..path_end];
    let (path, query) = typed.split_once('?').unwrap_or((typed, ""));
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let mut request = Request {
        method,
        path,
        query: query.to_owned(),
        body: None,
    };
    request.body = body(text, &tokens, path_end, &request)?;
    Ok(request)
}

fn error(message: impl Into<String>, range: Range<usize>) -> ParseError {
    ParseError {
        message: message.into(),
        range,
    }
}

/// The body, from `start` to the end of the request, with its comment lines left out.
fn body(
    text: &str,
    tokens: &[Token],
    start: usize,
    request: &Request,
) -> Result<Option<Body>, ParseError> {
    // Comments become spaces of the same length, so a reader's offsets stay the request's.
    let mut source = String::with_capacity(text.len() - start);
    for token in tokens.iter().filter(|token| token.range.end > start) {
        let range = token.range.start.max(start)..token.range.end;
        if token.kind == TokenKind::LineComment {
            source.extend(std::iter::repeat_n(' ', range.len()));
        } else {
            source.push_str(&text[range]);
        }
    }
    if source.trim().is_empty() {
        return Ok(None);
    }
    if let Some(at) = source.find("\"\"\"") {
        return Err(error(
            "triple-quoted strings aren't read; write the string in JSON, with \\n for its line breaks",
            start + at..start + at + 3,
        ));
    }
    if takes_ndjson(request) {
        return lines(&source, start).map(|lines| Some(Body::Lines(lines)));
    }
    let value = Value::parse(&source).map_err(|failure| {
        let at = start + failure.offset;
        error(
            format!("the body isn't JSON: {}", failure.reason),
            at..(at + 1).min(text.len()),
        )
    })?;
    Ok(Some(Body::Json {
        text: source.trim().to_owned(),
        value,
    }))
}

fn takes_ndjson(request: &Request) -> bool {
    let segments = request.segments();
    let endpoint = match segments.as_slice() {
        [.., endpoint, "template"] => endpoint,
        [.., endpoint] => endpoint,
        [] => return false,
    };
    NDJSON_ENDPOINTS.contains(endpoint)
}

/// The values of an NDJSON body, each with its text on one line. A value may be typed over
/// several lines, as Kibana allows; it is sent on one, its line breaks made spaces, which JSON
/// reads the same since a string can't hold a raw line break.
fn lines(source: &str, start: usize) -> Result<Vec<(String, Value)>, ParseError> {
    let mut values = Vec::new();
    let mut depth: usize = 0;
    let mut value_start: Option<usize> = None;
    for token in tokenize(source) {
        if token.kind.is_trivia() {
            continue;
        }
        let begin = *value_start.get_or_insert(token.range.start);
        match &source[token.range.clone()] {
            "{" | "[" if token.kind == TokenKind::Punctuation => depth += 1,
            "}" | "]" if token.kind == TokenKind::Punctuation => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 {
            let typed = &source[begin..token.range.end];
            let value = Value::parse(typed).map_err(|failure| {
                let at = start + begin + failure.offset;
                error(
                    format!("a line of the body isn't JSON: {}", failure.reason),
                    at..at + 1,
                )
            })?;
            values.push((typed.replace(['\n', '\r'], " "), value));
            value_start = None;
        }
    }
    if let Some(begin) = value_start {
        let at = start + begin;
        return Err(error("a value of the body isn't closed", at..at + 1));
    }
    Ok(values)
}
