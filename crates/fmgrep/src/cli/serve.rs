//! `serve` and `--server`: newline-delimited JSON-RPC 2.0 over one loaded index.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Outcome;
use super::args::{ClientAction, ClientArgs};
use super::print;
use crate::expr::{Expr, REGEX_REFUSAL, TERMS_REFUSAL};
use crate::manifest::Manifest;
use crate::matching::{MatchMode, QueryOutput};
use crate::output::WRITE_FAILED;
use crate::query::{Loaded, QueryRequest, answer, load};
use crate::sink::Discard;
use crate::wire::OutputReport;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const QUERY_FAILED: i64 = -32000;

/// `query` params: the `--json --approx-counts|--occurrences` flags.
/// `terms` may be omitted only when `expr` is given.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    #[serde(default)]
    pub terms: Vec<String>,
    /// Exclusive with non-empty `terms` and with `regex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<Expr>,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub dotall: bool,
    /// Only the counts drive is served; false is refused.
    #[serde(default = "yes")]
    pub approx_counts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub occurrences: bool,
}

fn yes() -> bool {
    true
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            terms: Vec::new(),
            expr: None,
            regex: false,
            ignore_case: false,
            multiline: false,
            dotall: false,
            approx_counts: true,
            occurrences: false,
        }
    }
}

/// `status` result.
#[derive(Serialize, Deserialize)]
struct ServeStatus {
    index_name: String,
    store_docs: bool,
    files: usize,
    num_bytes: u64,
    index_size_bytes: u64,
    load_us: u64,
}

/// What `serve` serves.
pub struct ServeRequest<'a> {
    pub index_dir: &'a Path,
    pub manifest: &'a Manifest,
    pub corpus_root: Option<&'a Path>,
    /// None: requests on the input, responses on the output.
    pub listen: Option<SocketAddr>,
    /// Watch a files-layout index's root for changes.
    pub watch: bool,
}

/// One loaded index and what it was loaded from.
struct Served<'m> {
    loaded: Loaded,
    manifest: &'m Manifest,
    index_dir: &'m Path,
}

impl Served<'_> {
    fn query(&mut self, p: &QueryParams) -> Result<OutputReport, String> {
        let req = QueryRequest {
            index_dir: self.index_dir,
            manifest: self.manifest,
            corpus_root: None,
            regex: p.regex,
            mode: MatchMode {
                multiline: p.multiline,
                dotall: p.dotall,
                ignore_case: p.ignore_case,
            },
            terms: &p.terms,
            expr: p.expr.as_ref(),
            output: if p.occurrences {
                QueryOutput::Occurrences
            } else {
                QueryOutput::Counts
            },
        };
        answer(&mut self.loaded, &req, &mut Discard)
    }

    fn status(&self) -> ServeStatus {
        ServeStatus {
            index_name: crate::manifest::NAME.to_owned(),
            store_docs: self.manifest.store_docs,
            files: self.loaded.index.num_files_indexed(),
            num_bytes: self.manifest.num_bytes,
            index_size_bytes: self.loaded.index.index_size_bytes(),
            load_us: self.loaded.load_wall.as_micros() as u64,
        }
    }
}

#[derive(Serialize)]
struct Request<'a, P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Serialize)]
struct Success<'a, T> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: T,
}

#[derive(Serialize, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct Failure<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    error: RpcError,
}

fn success<T: Serialize>(id: &Value, result: T) -> String {
    serde_json::to_string(&Success {
        jsonrpc: "2.0",
        id,
        result,
    })
    .unwrap_or_else(|e| {
        failure(
            id,
            QUERY_FAILED,
            format!("error: failed to encode result: {e}"),
        )
    })
}

fn failure(id: &Value, code: i64, message: String) -> String {
    let error = RpcError { code, message };
    // A string and a JSON id always encode.
    serde_json::to_string(&Failure {
        jsonrpc: "2.0",
        id,
        error,
    })
    .unwrap_or_default()
}

/// One request line to its response line, and whether it asked to shut down.
fn respond(session: &mut Served<'_>, line: &str) -> (String, bool) {
    let null = Value::Null;
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return (
                failure(&null, PARSE_ERROR, format!("parse error: {e}")),
                false,
            );
        }
    };
    let Some(obj) = req.as_object() else {
        let what = if req.is_array() {
            "batch requests are not supported"
        } else {
            "request is not an object"
        };
        return (failure(&null, INVALID_REQUEST, what.to_owned()), false);
    };
    let id = match obj.get("id") {
        Some(id) if id.is_string() || id.is_number() => id,
        _ => {
            let msg = "id must be a string or number (notifications are not supported)";
            return (failure(&null, INVALID_REQUEST, msg.to_owned()), false);
        }
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return (
            failure(id, INVALID_REQUEST, "jsonrpc must be \"2.0\"".to_owned()),
            false,
        );
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return (
            failure(id, INVALID_REQUEST, "method must be a string".to_owned()),
            false,
        );
    };
    match method {
        "query" => {
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            let has_terms = params.get("terms").is_some();
            let p = match serde_json::from_value::<QueryParams>(params) {
                Ok(p) if p.approx_counts => p,
                Ok(_) => {
                    let msg = "invalid params: approx_counts must be true";
                    return (failure(id, INVALID_PARAMS, msg.to_owned()), false);
                }
                Err(e) => {
                    return (
                        failure(id, INVALID_PARAMS, format!("invalid params: {e}")),
                        false,
                    );
                }
            };
            let refusal = match &p.expr {
                None if !has_terms => Some("missing field `terms`".to_owned()),
                None => None,
                Some(_) if !p.terms.is_empty() => Some(TERMS_REFUSAL.to_owned()),
                Some(_) if p.regex => Some(REGEX_REFUSAL.to_owned()),
                Some(e) => e.check().err().map(|e| format!("expr: {e}")),
            };
            if let Some(msg) = refusal {
                return (
                    failure(id, INVALID_PARAMS, format!("invalid params: {msg}")),
                    false,
                );
            }
            match session.query(&p) {
                Ok(report) => (success(id, report), false),
                Err(e) => (failure(id, QUERY_FAILED, e), false),
            }
        }
        "status" => (success(id, session.status()), false),
        "shutdown" => (
            success(id, serde_json::json!({ "status": "shutting down" })),
            true,
        ),
        other => (
            failure(id, METHOD_NOT_FOUND, format!("unknown method {other:?}")),
            false,
        ),
    }
}

/// Answer `input` line by line until EOF (false) or a shutdown (true).
fn serve_lines(
    session: &mut Served<'_>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, String> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("error: cannot read request: {e}"))?
            == 0
        {
            return Ok(false);
        }
        let (response, shutdown) = match std::str::from_utf8(&buf) {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => respond(session, line),
            Err(e) => (
                failure(&Value::Null, PARSE_ERROR, format!("parse error: {e}")),
                false,
            ),
        };
        out.write_all(format!("{response}\n").as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| format!("{WRITE_FAILED} response: {e}"))?;
        if shutdown {
            return Ok(true);
        }
    }
}

/// Serve connections one at a time, each until EOF, until a shutdown.
fn serve_tcp(session: &mut Served<'_>, listener: TcpListener) -> Result<(), String> {
    for stream in listener.incoming() {
        let served = stream
            .and_then(|s| {
                s.set_nodelay(true)?;
                Ok((BufReader::new(s.try_clone()?), s))
            })
            .map_err(|e| format!("error: cannot accept: {e}"))
            .and_then(|(mut input, mut output)| serve_lines(session, &mut input, &mut output));
        match served {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => eprintln!("{e}"),
        }
    }
    Ok(())
}

/// Load once, then serve. Over TCP the bound address is printed on `out` as
/// the readiness line.
pub fn run_serve(
    req: &ServeRequest<'_>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), String> {
    let mut session = Served {
        loaded: load(req.index_dir, req.manifest, req.corpus_root, req.watch)?,
        manifest: req.manifest,
        index_dir: req.index_dir,
    };
    let listener = match req.listen {
        None => None,
        Some(addr) => {
            let listener = TcpListener::bind(addr)
                .map_err(|e| format!("error: cannot listen on {addr}: {e}"))?;
            let bound = listener
                .local_addr()
                .map_err(|e| format!("error: cannot listen on {addr}: {e}"))?;
            writeln!(out, "{bound}")
                .and_then(|()| out.flush())
                .map_err(|e| format!("{WRITE_FAILED} readiness line: {e}"))?;
            Some(listener)
        }
    };
    let watch = session.loaded.take_watch();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        if let Some(watch) = watch {
            let (stop, files, lens) = (&stop, &req.manifest.files, &req.manifest.doc_lens);
            scope.spawn(move || watch.run(files, lens, stop));
        }
        let served = match listener {
            None => serve_lines(&mut session, input, out).map(|_| ()),
            Some(listener) => serve_tcp(&mut session, listener),
        };
        stop.store(true, Ordering::Relaxed);
        served
    })
}

/// Send one request to a server and print its result as a search would.
pub fn run_client(args: &ClientArgs, out: &mut dyn Write) -> Result<Outcome, String> {
    let (method, params) = match &args.action {
        ClientAction::Query { params, .. } => ("query", serde_json::to_value(params)),
        ClientAction::Status => ("status", Ok(serde_json::json!({}))),
        ClientAction::Shutdown => ("shutdown", Ok(serde_json::json!({}))),
    };
    let params = params.map_err(|e| format!("error: failed to encode request: {e}"))?;
    let request = serde_json::to_string(&Request {
        jsonrpc: "2.0",
        id: 1,
        method,
        params,
    })
    .map_err(|e| format!("error: failed to encode request: {e}"))?;
    let connect = &args.server;
    let io = |e: std::io::Error| format!("error: {connect}: {e}");
    let mut stream = TcpStream::connect(connect).map_err(io)?;
    stream.set_nodelay(true).map_err(io)?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(io)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(io)?;
    if line.is_empty() {
        return Err(format!(
            "error: {connect}: connection closed without a response"
        ));
    }
    let mut response: Value = serde_json::from_str(&line)
        .map_err(|e| format!("error: {connect}: unparsable response: {e}"))?;
    if let Some(err) = response.get_mut("error") {
        let err: RpcError = serde_json::from_value(err.take())
            .map_err(|e| format!("error: {connect}: unparsable error: {e}"))?;
        let msg = err.message.strip_prefix("error: ").unwrap_or(&err.message);
        return Err(format!("error: server: {msg}"));
    }
    let result = response
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null);
    let bad = |e: serde_json::Error| format!("error: {connect}: unexpected result: {e}");
    match &args.action {
        ClientAction::Query { json, .. } => {
            let report: OutputReport = serde_json::from_value(result).map_err(bad)?;
            if *json {
                print::query_report(out, &report)?;
            } else {
                print::counts_summary(out, &report)?;
            }
            Ok(Outcome::Query(report))
        }
        ClientAction::Status => {
            let status: ServeStatus = serde_json::from_value(result).map_err(bad)?;
            print::line(out, &serde_json::to_string(&status).map_err(bad)?)?;
            Ok(Outcome::Done)
        }
        ClientAction::Shutdown => Ok(Outcome::Done),
    }
}
