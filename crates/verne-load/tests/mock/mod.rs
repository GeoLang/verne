//! A ptolemy scripted on a socket.
//!
//! The live test is where a request body is held against the real API, and a
//! mock cannot do that job. This is for the parts of a load that only a real
//! request shows: which routes were asked, in what order, and with what in them.
//! Reading a request and answering it is all that lives here, so each test
//! writes its own routes.

// each test binary compiles this module whole, so a helper one of them does not
// call is unused there and used in the next one over
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// One request the loader made, as the socket saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub body: String,
}

impl Seen {
    /// The body as JSON, which every body the loader sends is.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!(
                "{} {} carried {}: {error}",
                self.method, self.path, self.body
            )
        })
    }
}

/// A ptolemy answering from a handler instead of a database, remembering what it
/// was asked.
pub struct Ptolemy {
    pub url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Ptolemy {
    /// A ptolemy answering every request through `handler`. A route the handler
    /// does not know is a panic rather than a 404: a missing fixture must be
    /// louder than a wrong assertion.
    pub fn answering(handler: impl Fn(&Seen) -> String + Send + 'static) -> Ptolemy {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("the address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        // detached: the test ends with the process, and a listener with nothing
        // left to answer costs nothing
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.expect("a connection");
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                recorded
                    .lock()
                    .expect("the request log")
                    .push(request.clone());
                let answer = handler(&request);
                stream.write_all(answer.as_bytes()).expect("an answer");
                stream.flush().expect("a flushed answer");
            }
        });
        Ptolemy { url, seen }
    }

    /// What it was asked, as `(method, path)`, in order.
    pub fn calls(&self) -> Vec<(String, String)> {
        self.seen
            .lock()
            .expect("the request log")
            .iter()
            .map(|held| (held.method.clone(), held.path.clone()))
            .collect()
    }

    /// The one request that was `method path`.
    pub fn call(&self, method: &str, path: &str) -> Seen {
        self.seen
            .lock()
            .expect("the request log")
            .iter()
            .find(|held| held.method == method && held.path == path)
            .cloned()
            .unwrap_or_else(|| panic!("no {method} {path} was made: {:#?}", self.calls()))
    }

    /// Every request that was `method` on a path ending in `suffix`.
    pub fn matching(&self, method: &str, suffix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("the request log")
            .iter()
            .filter(|held| held.method == method && held.path.ends_with(suffix))
            .cloned()
            .collect()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Seen> {
    let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("a request line");
    let mut words = line.split_whitespace();
    let method = words.next()?.to_string();
    let path = words.next()?.to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("a header");
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().expect("a content length");
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("the body");
    Some(Seen {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// A 200 with a JSON body, which is how ptolemy answers a read.
pub fn ok(body: &serde_json::Value) -> String {
    answer("200 OK", &body.to_string())
}

/// A 201 with the row that was created, which is how it answers a POST.
pub fn created(body: &serde_json::Value) -> String {
    answer("201 Created", &body.to_string())
}

/// A 204, which is how it answers a delete and a schema PUT.
pub fn no_content() -> String {
    "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
}

fn answer(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: \
         close\r\n\r\n{body}",
        body.len()
    )
}
