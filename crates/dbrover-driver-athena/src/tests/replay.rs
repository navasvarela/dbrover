//! A server on the loopback address that answers AWS's SDK with Athena's exchanges from a file,
//! in their order, and checks that each request is the one the file expects
//! (`docs/cloud-data.md`, C3, Testing without an account).
//!
//! A file is `{"exchanges": [...]}`, each exchange naming the operation the request asks for
//! (`X-Amz-Target: AmazonAthena.<operation>`), the parts of its JSON body that matter, and the
//! status and JSON body to answer with. An exchange marked `"repeat": true` answers every
//! request it matches until one matches the exchange after it, for polls whose number depends
//! on timing. Athena's API is JSON over HTTP 1.1 (`awsJson1_1`), so the server reads only that.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
struct Exchange {
    operation: String,
    request: Value,
    status: u16,
    response: Value,
    repeat: bool,
}

impl Exchange {
    fn read(value: &Value) -> Self {
        Self {
            operation: value["operation"]
                .as_str()
                .expect("an exchange names its operation")
                .to_owned(),
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

    fn matches(&self, operation: &str, body: &Value) -> bool {
        self.operation == operation && contains(body, &self.request)
    }
}

/// Whether `actual` holds everything `expected` says: each key of an object, recursively, and
/// anything else as it is. `null` expects nothing.
fn contains(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Null => true,
        Value::Object(fields) => fields
            .iter()
            .all(|(key, value)| actual.get(key).is_some_and(|found| contains(found, value))),
        other => actual == other,
    }
}

#[derive(Default)]
struct State {
    remaining: VecDeque<Exchange>,
    /// Requests that didn't match the file, which fail the test in [`Replay::finish`].
    mismatches: Vec<String>,
}

impl State {
    /// The answer to a request for `operation` with `body`.
    fn answer(&mut self, operation: &str, body: &Value) -> (u16, Value) {
        if let Some(head) = self.remaining.front()
            && head.repeat
            && !head.matches(operation, body)
            && self
                .remaining
                .get(1)
                .is_some_and(|next| next.matches(operation, body))
        {
            self.remaining.pop_front();
        }
        match self.remaining.front() {
            Some(head) if head.matches(operation, body) => {
                let answer = (head.status, head.response.clone());
                if !head.repeat {
                    self.remaining.pop_front();
                }
                answer
            }
            head => {
                let expected = head.map_or("nothing more".to_owned(), |head| {
                    format!("{} {}", head.operation, head.request)
                });
                self.mismatches
                    .push(format!("got {operation} {body}, expected {expected}"));
                (
                    400,
                    serde_json::json!({
                        "__type": "ReplayMismatch",
                        "Message": format!("the replay expected {expected}"),
                    }),
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
            mismatches: Vec::new(),
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

    /// Checks that every request matched and every exchange was used, past any that repeat
    /// at the end.
    pub fn finish(&self) {
        let state = self.state();
        assert!(state.mismatches.is_empty(), "{:#?}", state.mismatches);
        let unused: Vec<&Exchange> = state.remaining.iter().filter(|left| !left.repeat).collect();
        assert!(unused.is_empty(), "exchanges never asked for: {unused:#?}");
    }
}

/// Answers the requests of one connection, which the SDK keeps open between them.
async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    while let Some((operation, body)) = read_request(&mut stream).await {
        let (status, response) = state
            .lock()
            .expect("the replay's state")
            .answer(&operation, &body);
        let text = response.to_string();
        let answer = format!(
            "HTTP/1.1 {status} Replayed\r\ncontent-type: application/x-amz-json-1.1\r\n\
             content-length: {}\r\n\r\n{text}",
            text.len()
        );
        if stream.write_all(answer.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// One request's operation and JSON body; `None` once the connection is closed.
async fn read_request(stream: &mut TcpStream) -> Option<(String, Value)> {
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
            break (
                end + 4,
                header(&head, "content-length")?.parse::<usize>().ok()?,
            );
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
    let target = header(&head, "x-amz-target")?;
    let operation = target.rsplit('.').next()?.to_owned();
    let body = serde_json::from_slice(&bytes[head_end..head_end + length]).ok()?;
    Some((operation, body))
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}
