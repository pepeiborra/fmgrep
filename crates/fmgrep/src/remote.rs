//! The JSON-RPC client connection.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
pub struct Request<'a, P> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: P,
}

#[derive(Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// One connection, one request in flight; every `Err` is printable.
pub struct Conn {
    addr: String,
    stream: BufReader<TcpStream>,
}

impl Conn {
    pub fn open(addr: &str) -> Result<Self, String> {
        let io = |e: std::io::Error| format!("error: {addr}: {e}");
        let stream = TcpStream::connect(addr).map_err(io)?;
        stream.set_nodelay(true).map_err(io)?;
        Ok(Self {
            addr: addr.to_owned(),
            stream: BufReader::with_capacity(64 << 10, stream),
        })
    }

    /// Frames go to `body` until the response line, whose `result` this is.
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        body: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<Value, String> {
        let addr = &self.addr;
        let io = |e: std::io::Error| format!("error: {addr}: {e}");
        let request = Request {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let request = serde_json::to_string(&request)
            .map_err(|e| format!("error: failed to encode request: {e}"))?;
        let stream = self.stream.get_mut();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(io)?;
        let mut line = String::new();
        loop {
            line.clear();
            self.stream.read_line(&mut line).map_err(io)?;
            if !line.starts_with(|c: char| c.is_ascii_digit()) {
                break;
            }
            let mut left: usize = line
                .trim_end()
                .parse()
                .map_err(|e| format!("error: {addr}: bad frame header: {e}"))?;
            while left > 0 {
                let chunk = self.stream.fill_buf().map_err(io)?;
                if chunk.is_empty() {
                    return Err(format!("error: {addr}: connection closed mid-frame"));
                }
                let n = chunk.len().min(left);
                body(&chunk[..n])?;
                self.stream.consume(n);
                left -= n;
            }
        }
        if line.is_empty() {
            return Err(format!(
                "error: {addr}: connection closed without a response"
            ));
        }
        let mut response: Value = serde_json::from_str(&line)
            .map_err(|e| format!("error: {addr}: unparsable response: {e}"))?;
        if let Some(err) = response.get_mut("error") {
            let e: RpcError = serde_json::from_value(err.take())
                .map_err(|e| format!("error: {addr}: unparsable error: {e}"))?;
            let message = e.message.strip_prefix("error: ").unwrap_or(&e.message);
            return Err(format!("error: server: {message}"));
        }
        Ok(response
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self, result: Value) -> Result<T, String> {
        serde_json::from_value(result)
            .map_err(|e| format!("error: {}: unexpected result: {e}", self.addr))
    }
}
