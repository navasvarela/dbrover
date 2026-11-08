//! A server on the loopback address that answers the driver's requests with BigQuery's
//! exchanges from a file, in their order, and checks that each request is the one the file
//! expects (`docs/cloud-data.md`, C4, Testing without an account).
//!
//! A file is `{"exchanges": [...]}`, each exchange naming the request's method and path (below
//! `/bigquery/v2`), the query parameters and the parts of its JSON body that matter, and the
//! status and JSON body to answer with. Job ids are DbRover's own and random, so the last
//! `jobReference.jobId` a request sent is remembered, and `{job}` in an exchange's path or
//! answer stands for it. An exchange marked `"repeat": true` answers every request it
//! matches until one matches the exchange after it, for polls whose number depends on timing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What stands for the job's id in an exchanges file.
const JOB: &str = "{job}";

#[derive(Debug, Clone)]
struct Exchange {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    request: Value,
    status: u16,
    response: Value,
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
        Self {
            method: text("method"),
            path: text("path"),
            query: value
                .get("query")
                .and_then(Value::as_object)
                .map(|query| {
                    query
                        .iter()
                        .map(|(name, value)| {
                            (name.clone(), value.as_str().unwrap_or_default().to_owned())
                        })
                        .collect()
                })
                .unwrap_or_default(),
            request: value.get("request").cloned().unwrap_or(Value::Null),
            status: value
                .get("status")
                .and_then(Value::as_u64)
                .map_or(200, |status| u16::try_from(status).expect("an HTTP status")),
            response: value.get("response").cloned().unwrap_or(Value::Null),
            repeat: value
                .get("repeat")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    fn matches(&self, request: &Request, job: Option<&str>) -> bool {
        let path = with_job(&self.path, job);
        self.method == request.method
            && path == request.path
            && self
                .query
                .iter()
                .all(|(name, value)| request.query(name) == Some(value.as_str()))
            && contains(&request.body, &self.request)
    }

    fn describe(&self) -> String {
        format!(
            "{} {} {:?} {}",
            self.method, self.path, self.query, self.request
        )
    }
}

fn with_job(text: &str, job: Option<&str>) -> String {
    match job {
        Some(job) => text.replace(JOB, job),
        None => text.to_owned(),
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
    /// Below `/bigquery/v2`, still percent-encoded.
    path: String,
    query: Vec<(String, String)>,
    authorization: Option<String>,
    body: Value,
}

impl Request {
    fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct State {
    remaining: VecDeque<Exchange>,
    /// The job id the driver chose, from the last `jobReference.jobId` it sent.
    job: Option<String>,
    /// Every request's `Authorization` header, in order.
    authorizations: Vec<Option<String>>,
    /// Requests that didn't match the file, which fail the test in [`Replay::finish`].
    mismatches: Vec<String>,
}

impl State {
    fn answer(&mut self, request: &Request) -> (u16, Value) {
        if let Some(job) = request.body["jobReference"]["jobId"].as_str() {
            self.job = Some(job.to_owned());
        }
        self.authorizations.push(request.authorization.clone());
        let job = self.job.clone();
        let job = job.as_deref();
        if let Some(head) = self.remaining.front()
            && head.repeat
            && !head.matches(request, job)
            && self
                .remaining
                .get(1)
                .is_some_and(|next| next.matches(request, job))
        {
            self.remaining.pop_front();
        }
        match self.remaining.front() {
            Some(head) if head.matches(request, job) => {
                let response = with_job(&head.response.to_string(), job);
                let answer = (
                    head.status,
                    serde_json::from_str(&response).expect("the answer stays JSON"),
                );
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
                    serde_json::json!({"error": {"code": 400,
                        "message": format!("the replay expected {expected}"),
                        "errors": [{"reason": "replayMismatch"}]}}),
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
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&serving)));
            }
        });
        Self { endpoint, state }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("the replay's state")
    }

    /// The job id the driver chose last.
    pub fn job(&self) -> Option<String> {
        self.state().job.clone()
    }

    /// Every request's `Authorization` header, in order.
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.state().authorizations.clone()
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
async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    while let Some(request) = read_request(&mut stream).await {
        let (status, response) = state.lock().expect("the replay's state").answer(&request);
        let text = response.to_string();
        let answer = format!(
            "HTTP/1.1 {status} Replayed\r\ncontent-type: application/json\r\n\
             content-length: {}\r\n\r\n{text}",
            text.len()
        );
        if stream.write_all(answer.as_bytes()).await.is_err() {
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
    let target = target.strip_prefix("/bigquery/v2").unwrap_or(target);
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
        authorization: header(&head, "authorization"),
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
