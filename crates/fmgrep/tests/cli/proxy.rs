//! A proxy in front of a server: it lists the requests that pass, and
//! sends each new connection to the server it points at now, so that a
//! test can put another server behind one address.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

pub struct Proxy {
    pub addr: String,
    target: Arc<Mutex<String>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Proxy {
    /// A proxy to the server at `target`.
    pub fn new(target: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let target = Arc::new(Mutex::new(target.to_owned()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (to, seen) = (Arc::clone(&target), Arc::clone(&requests));
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { return };
                let server = TcpStream::connect(&*to.lock().unwrap()).unwrap();
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || relay(client, server, &seen));
            }
        });
        Self {
            addr,
            target,
            requests,
        }
    }

    /// Send the next connections to the server at `target`.
    pub fn point(&self, target: &str) {
        *self.target.lock().unwrap() = target.to_owned();
    }

    /// The requests since the last call, in order.
    pub fn requests(&self) -> Vec<serde_json::Value> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }

    /// The methods of the requests since the last call, in order.
    pub fn methods(&self) -> Vec<String> {
        let method = |r: serde_json::Value| r["method"].as_str().unwrap_or("?").to_owned();
        self.requests().into_iter().map(method).collect()
    }
}

/// Each request line of `client` to `server`, the request to `seen`, and
/// every byte of `server` back to `client`, until the client closes.
fn relay(client: TcpStream, server: TcpStream, seen: &Mutex<Vec<serde_json::Value>>) {
    let (mut back, mut answers) = (client.try_clone().unwrap(), server.try_clone().unwrap());
    let replies = std::thread::spawn(move || {
        let _ = std::io::copy(&mut answers, &mut back);
        let _ = back.shutdown(Shutdown::Write);
    });
    let mut lines = BufReader::new(client);
    let mut line = String::new();
    while lines.read_line(&mut line).is_ok_and(|n| n > 0) {
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        seen.lock().unwrap().push(request.clone());
        (&server).write_all(line.as_bytes()).unwrap();
        line.clear();
    }
    let _ = server.shutdown(Shutdown::Write);
    replies.join().unwrap();
}

/// A server of one connection that answers its requests, one by one, with
/// `replies`, each one line, then closes.
pub fn scripted(replies: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut input = BufReader::new(&stream);
        for reply in replies {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            (&stream)
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
        }
    });
    addr
}
