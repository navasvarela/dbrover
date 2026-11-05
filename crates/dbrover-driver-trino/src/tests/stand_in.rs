//! A coordinator on the loopback address that answers each request with the next of a list of
//! answers, and keeps every request it got: the protocol's handling (headers, `nextUri`,
//! retries, cancel) tried on every pull request, without Trino's container, which runs nightly
//! (`docs/cloud-data.md`, C3). Its answers are copied from what `trinodb/trino:484` answered.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One answer: its status, its headers, and its body.
pub(crate) struct Answer {
    pub status: u16,
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: String,
}

impl Answer {
    pub(crate) fn json(body: &str) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    pub(crate) fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    pub(crate) fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }
}

/// A request as the coordinator got it.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub method: String,
    pub path: String,
    /// Names lowercase.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Received {
    pub(crate) fn header(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }
}

#[derive(Default)]
struct State {
    answers: VecDeque<Answer>,
    received: Vec<Received>,
}

pub(crate) struct Coordinator {
    /// `127.0.0.1:<port>`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

impl Coordinator {
    pub(crate) async fn start(answers: Vec<Answer>) -> Self {
        let state = Arc::new(Mutex::new(State {
            answers: answers.into(),
            received: Vec::new(),
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("its address").to_string();
        let serving = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&serving)));
            }
        });
        Self { address, state }
    }

    pub(crate) fn received(&self) -> Vec<Received> {
        self.state.lock().expect("state").received.clone()
    }

    pub(crate) fn port(&self) -> u16 {
        self.address
            .rsplit(':')
            .next()
            .and_then(|port| port.parse().ok())
            .expect("a port")
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    while let Some(request) = read_request(&mut stream).await {
        let answer = {
            let mut state = state.lock().expect("state");
            state.received.push(request);
            state.answers.pop_front()
        };
        let Some(answer) = answer else {
            return;
        };
        let mut text = format!(
            "HTTP/1.1 {} Answered\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
            answer.status,
            answer.body.len()
        );
        for (name, value) in &answer.headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("\r\n");
        text.push_str(&answer.body);
        if stream.write_all(text.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let head_end = loop {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&bytes[head_end..head_end + length]).into_owned();
    Some(Received {
        method,
        path,
        headers,
        body,
    })
}
