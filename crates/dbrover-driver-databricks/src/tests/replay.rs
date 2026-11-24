//! A server on the loopback address that answers the driver's requests with Databricks's
//! exchanges from a file, in their order, and checks that each request is the one the file
//! expects (`docs/cloud-data.md`, C6, Testing C6 without an account). Adapted from the
//! Snowflake driver's.
//!
//! A file is `{"exchanges": [...]}`, each exchange naming the request's method and path, the
//! query parameters, the headers and the parts of its JSON body that matter, and the status and
//! JSON body to answer with. The server serves the external links too, at paths of their own
//! (`/links/…`), so `{endpoint}` in an answer stands for the server's own address. An exchange
//! marked `"repeat": true` answers every request it matches until one matches the exchange after
//! it, for polls whose number depends on timing; `"retry_after"` sends that header.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What stands for the server's own address in an answer: `http://127.0.0.1:<port>`.
const ENDPOINT: &str = "{endpoint}";

#[derive(Debug, Clone)]
struct Exchange {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    request: Value,
    status: u16,
    response: Value,
    retry_after: Option<u64>,
    repeat: bool,
}

impl Exchange {
    fn read(value: &Value) -> Self {
        let text = |key: &str| {
            value[key]
                .as_str()
                .unwrap_or_else(|| panic!("an exchange has its {key}"))
                .to_owned()
        };
        let pairs = |key: &str| -> Vec<(String, String)> {
            value
                .get(key)
                .and_then(Value::as_object)
                .map(|pairs| {
                    pairs
                        .iter()
                        .map(|(name, value)| {
                            (name.clone(), value.as_str().unwrap_or_default().to_owned())
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            method: text("method"),
            path: text("path"),
            query: pairs("query"),
            headers: pairs("headers"),
            request: value.get("request").cloned().unwrap_or(Value::Null),
            status: value
                .get("status")
                .and_then(Value::as_u64)
                .map_or(200, |status| u16::try_from(status).expect("an HTTP status")),
            response: value.get("response").cloned().unwrap_or(Value::Null),
            retry_after: value.get("retry_after").and_then(Value::as_u64),
            repeat: value
                .get("repeat")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    fn matches(&self, request: &Request) -> bool {
        self.method == request.method
            && self.path == request.path
            && self
                .query
                .iter()
                .all(|(name, value)| request.query(name) == Some(value.as_str()))
            && self
                .headers
                .iter()
                .all(|(name, value)| request.header(name) == Some(value.as_str()))
            && contains(&request.body, &self.request)
    }

    fn describe(&self) -> String {
        format!(
            "{} {} {:?} {}",
            self.method, self.path, self.query, self.request
        )
    }
}

/// Whether `actual` holds everything `expected` says: each key of an object, recursively, and
/// anything else as it is. `null` expects nothing; `"absent"` in an object expects the key not
/// to be there.
fn contains(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Null => true,
        Value::Object(fields) => fields.iter().all(|(key, value)| match value {
            Value::String(absent) if absent == "absent" => actual.get(key).is_none(),
            value => actual.get(key).is_some_and(|found| contains(found, value)),
        }),
        other => actual == other,
    }
}

/// One request, as the server read it.
#[derive(Debug)]
struct Request {
    method: String,
    /// Still percent-encoded.
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Request {
    fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct State {
    remaining: VecDeque<Exchange>,
    /// Every request's path and headers, in order.
    requests: Vec<(String, Vec<(String, String)>)>,
    /// Requests that didn't match the file, which fail the test in [`Replay::finish`].
    mismatches: Vec<String>,
}

impl State {
    fn answer(&mut self, request: &Request) -> (u16, Value, Option<u64>) {
        self.requests
            .push((request.path.clone(), request.headers.clone()));
        if let Some(head) = self.remaining.front()
            && head.repeat
            && !head.matches(request)
            && self
                .remaining
                .get(1)
                .is_some_and(|next| next.matches(request))
        {
            self.remaining.pop_front();
        }
        match self.remaining.front() {
            Some(head) if head.matches(request) => {
                let answer = (head.status, head.response.clone(), head.retry_after);
                if !head.repeat {
                    self.remaining.pop_front();
                }
                answer
            }
            head => {
                let expected = head.map_or("nothing more".to_owned(), Exchange::describe);
                self.mismatches.push(format!(
                    "got {} {} {:?} {}, expected {expected}",
                    request.method, request.path, request.query, request.body
                ));
                (
                    400,
                    serde_json::json!({"error_code": "BAD_REQUEST",
                        "message": format!("the replay expected {expected}")}),
                    None,
                )
            }
        }
    }
}

/// A running replay: where it listens, and what is left of its file.
pub(crate) struct Replay {
    /// `http://127.0.0.1:<port>`, the data source's endpoint.
    pub endpoint: String,
    state: Arc<Mutex<State>>,
}

impl Replay {
    /// Starts answering with the exchanges of `file`, a file's JSON text.
    pub async fn start(file: &str) -> Self {
        let file: Value = serde_json::from_str(file).expect("an exchanges file is JSON");
        let remaining = file["exchanges"]
            .as_array()
            .expect("an exchanges file has its exchanges")
            .iter()
            .map(Exchange::read)
            .collect();
        let state = Arc::new(Mutex::new(State {
            remaining,
            ..State::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let endpoint = format!("http://{}", listener.local_addr().expect("its address"));
        let serving = Arc::clone(&state);
        let address = endpoint.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&serving), address.clone()));
            }
        });
        Self { endpoint, state }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("the replay's state")
    }

    /// The header `name` of every request whose path starts with `prefix`, in order.
    pub fn header(&self, prefix: &str, name: &str) -> Vec<Option<String>> {
        self.state()
            .requests
            .iter()
            .filter(|(path, _)| path.starts_with(prefix))
            .map(|(_, headers)| {
                headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.clone())
            })
            .collect()
    }

    /// Checks that every request matched and every exchange was used, past any that repeat
    /// at the end.
    pub fn finish(&self) {
        let state = self.state();
        assert!(state.mismatches.is_empty(), "{:#?}", state.mismatches);
        let unused: Vec<String> = state
            .remaining
            .iter()
            .filter(|left| !left.repeat)
            .map(Exchange::describe)
            .collect();
        assert!(unused.is_empty(), "exchanges never asked for: {unused:#?}");
    }
}

/// Answers the requests of one connection, which the client keeps open between them.
async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>, endpoint: String) {
    while let Some(request) = read_request(&mut stream).await {
        let (status, response, retry_after) =
            state.lock().expect("the replay's state").answer(&request);
        let body = response
            .to_string()
            .replace(ENDPOINT, &endpoint)
            .into_bytes();
        let retry_after = retry_after
            .map(|seconds| format!("retry-after: {seconds}\r\n"))
            .unwrap_or_default();
        let head = format!(
            "HTTP/1.1 {status} Replayed\r\ncontent-type: application/json\r\n{retry_after}\
             content-length: {}\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).await.is_err()
            || stream.write_all(&body).await.is_err()
        {
            return;
        }
    }
}

/// One request; `None` once the connection is closed.
async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let (head_end, length) = loop {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let length = header(&head, "content-length")
                .map_or(Some(0), |length| length.parse::<usize>().ok())?;
            break (end + 4, length);
        }
    };
    while bytes.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut words = head.lines().next()?.split_whitespace();
    let method = words.next()?.to_owned();
    let target = words.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let body_bytes = &bytes[head_end..head_end + length];
    Some(Request {
        method,
        path: path.to_owned(),
        query: query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (name.to_owned(), decode(value))
            })
            .collect(),
        headers: head
            .lines()
            .skip(1)
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                Some((key.trim().to_owned(), value.trim().to_owned()))
            })
            .collect(),
        body: if body_bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body_bytes).ok()?
        },
    })
}

/// A query value with its percent-encoding undone.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}
