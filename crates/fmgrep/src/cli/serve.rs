//! `serve` and `--server`: newline-delimited JSON-RPC 2.0 over one loaded index.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::params::{Mode, QueryParams};
use super::{Outcome, print, render};
use crate::expr::{REGEX_REFUSAL, TERMS_REFUSAL};
use crate::fetch::Ask;
use crate::manifest::{Commit, DOCS_SHARD, Manifest};
use crate::output::WRITE_FAILED;
use crate::output::writer::Pipe;
use crate::query::{Loaded, answer, load};
use crate::remote::{Conn, RpcError};
use crate::sharded::DocfmIndex;
use crate::sink::{Discard, MatchSink};
use crate::source::DOCS_STAMPS;
use crate::wire::OutputReport;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const QUERY_FAILED: i64 = -32000;
/// A query pinned to another index format or served commit than this
/// server's.
pub const OTHER_INDEX: i64 = -32001;

/// A rendering mode's result: the local stdout, as `text` when it is UTF-8
/// and standard base64 `bytes` otherwise; neither when streamed.
#[derive(Serialize, Deserialize)]
struct Rendered {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<String>,
    report: OutputReport,
}

impl Rendered {
    fn new(out: Vec<u8>, report: OutputReport) -> Self {
        let (text, bytes) = match String::from_utf8(out) {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(crate::base64::encode(e.as_bytes()))),
        };
        Self {
            text,
            bytes,
            report,
        }
    }
}

const FRAME: usize = 64 << 10;
/// Room for the longest header in front of a frame.
const HEAD: usize = 8;

/// `stream` output: `<len>\n<len bytes>` frames of at most [`FRAME`] bytes,
/// each one write.
struct Frames<'a> {
    out: &'a mut dyn Write,
    buf: Vec<u8>,
}

impl<'a> Frames<'a> {
    fn new(out: &'a mut dyn Write) -> Self {
        let mut buf = Vec::with_capacity(HEAD + FRAME);
        buf.resize(HEAD, 0);
        Self { out, buf }
    }

    fn emit(&mut self) -> std::io::Result<()> {
        let len = self.buf.len() - HEAD;
        if len == 0 {
            return Ok(());
        }
        let head = format!("{len}\n");
        let start = HEAD - head.len();
        self.buf[start..HEAD].copy_from_slice(head.as_bytes());
        let sent = self.out.write_all(&self.buf[start..]);
        self.buf.truncate(HEAD);
        sent
    }
}

impl Write for Frames<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = b.len().min(HEAD + FRAME - self.buf.len());
        self.buf.extend_from_slice(&b[..n]);
        if self.buf.len() == HEAD + FRAME {
            self.emit()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.emit()?;
        self.out.flush()
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Answer {
    Report(OutputReport),
    Rendered(Rendered),
}

/// `status` result.
#[derive(Serialize, Deserialize)]
pub struct ServeStatus {
    /// The manifest format of the served index; absent from a server older
    /// than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    pub store_docs: bool,
    files: usize,
    num_bytes: u64,
    index_size_bytes: u64,
    load_us: u64,
    /// The documents' directory: the tree, or the index directory holding `docs.shard`.
    pub root: String,
    /// The served commit: the commit whose files a `store_docs` index holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<Commit>,
}

/// What `serve` serves.
pub struct ServeRequest<'a> {
    pub index_dir: &'a Path,
    pub manifest: &'a Manifest,
    pub corpus_root: Option<&'a Path>,
    /// None: requests on the input, responses on the output.
    pub listen: Option<SocketAddr>,
    /// Ask watchman what changed, at start and before each query, for a
    /// files layout; else never.
    pub watch: bool,
}

struct Served<'m> {
    index: DocfmIndex,
    docs: Loaded,
    manifest: &'m Manifest,
    index_dir: &'m Path,
    root: PathBuf,
}

impl Served<'_> {
    /// Refuse a query pinned to another index format or served commit.
    fn pinned(&self, p: &QueryParams) -> Result<(), RpcError> {
        let ours = self.manifest.commit.as_ref().map(|c| c.id.as_str());
        let format_ok = p.format.is_none_or(|f| f == self.manifest.format);
        let commit_ok = p.commit.is_none() || p.commit.as_deref() == ours;
        if format_ok && commit_ok {
            return Ok(());
        }
        let format = self.manifest.format;
        let message = match ours {
            Some(id) => format!("this server serves commit {id} of index format {format}"),
            None => format!("this server serves no commit, of index format {format}"),
        };
        Err(RpcError {
            code: OTHER_INDEX,
            message,
        })
    }

    fn query(&mut self, p: &mut QueryParams, out: &mut dyn Write) -> Result<Answer, RpcError> {
        self.pinned(p)?;
        // A pinned query's cwd is in the client's checkout, not in the
        // served tree: its `paths` already say what to search.
        if p.commit.is_none() {
            p.scope(super::served_tree(&self.root, self.manifest.store_docs));
        }
        let req = p
            .request(self.index_dir, self.manifest, None)
            .map_err(|message| RpcError {
                code: QUERY_FAILED,
                message,
            })?;
        self.docs.recheck(self.manifest);
        let (index, loaded) = (&self.index, &mut self.docs);
        let mut run = |sink: &mut dyn MatchSink| answer(index, loaded, &req, sink);
        let answered = match p.mode {
            Mode::Counts => run(&mut Discard).map(Answer::Report),
            // Output so far precedes an error, as in a local search.
            _ if p.stream => {
                let mut frames = Frames::new(out);
                let rendered = render(p, false, &mut frames, run);
                let sent = frames.flush();
                rendered.and_then(|report| {
                    sent.map_err(|e| format!("{WRITE_FAILED} matches: {e}"))?;
                    Ok(Answer::Rendered(Rendered {
                        text: None,
                        bytes: None,
                        report,
                    }))
                })
            }
            _ => {
                let mut out = Vec::new();
                render(p, false, &mut out, run).map(|r| Answer::Rendered(Rendered::new(out, r)))
            }
        };
        answered.map_err(|message| RpcError {
            code: QUERY_FAILED,
            message,
        })
    }

    fn status(&self) -> ServeStatus {
        ServeStatus {
            format: Some(self.manifest.format),
            store_docs: self.manifest.store_docs,
            files: self.manifest.num_docs,
            num_bytes: self.manifest.num_bytes,
            index_size_bytes: self.manifest.index_size_bytes,
            load_us: self.docs.load_wall.as_micros() as u64,
            root: self.root.display().to_string(),
            commit: self.manifest.commit.clone(),
        }
    }
}

#[derive(Serialize)]
struct Success<'a, T> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: T,
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

/// One request line to its response line, after any frames on `out`, and
/// whether it asked to shut down.
fn respond(session: &mut Served<'_>, line: &str, out: &mut dyn Write) -> (String, bool) {
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
            let mut p = match serde_json::from_value::<QueryParams>(params) {
                Ok(p) => p,
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
            match session.query(&mut p, out) {
                Ok(answer) => (success(id, answer), false),
                Err(e) => (failure(id, e.code, e.message), false),
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
            Ok(line) => respond(session, line, out),
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

/// The tree, or the index dir holding `docs.shard`; else why they are missing.
fn docs_root(req: &ServeRequest<'_>) -> Result<PathBuf, String> {
    let dir = req.index_dir.display();
    if req.manifest.store_docs {
        return match req.index_dir.join(DOCS_SHARD).is_file() {
            true => Ok(req.index_dir.to_path_buf()),
            false => Err(format!("error: {dir} has no {DOCS_SHARD}")),
        };
    }
    let tree = req.manifest.tree(req.corpus_root);
    if !tree.is_dir() {
        return Err(format!("error: {} is not a directory", tree.display()));
    }
    match req.index_dir.join(DOCS_STAMPS).is_file() {
        true => Ok(tree),
        false => Err(format!("error: {dir} has no {DOCS_STAMPS}")),
    }
}

/// `req`'s index and documents; refused when over half the files are gone.
fn hold<'m>(req: &ServeRequest<'m>) -> Result<Served<'m>, String> {
    let root = docs_root(req)?;
    let root = std::fs::canonicalize(&root)
        .map_err(|e| format!("error: cannot resolve {}: {e}", root.display()))?;
    let ask = match req.watch {
        true => Ask::EachQuery,
        false => Ask::Never,
    };
    let (index, mut docs) = load(req.index_dir, req.manifest, req.corpus_root, ask)?;
    if let Some(cause) = docs.refusal() {
        return Err(format!(
            "error: cannot use watchman for {} ({cause}); `fmgrep serve` asks it before each \
             query (pass --no-watch to serve a tree that does not change)",
            root.display()
        ));
    }
    if let Some(why) = docs.mostly_gone(req.manifest.files.len()) {
        return Err(format!("error: {why} under {}", root.display()));
    }
    docs.measure_band(req.manifest);
    docs.measure_locate(&index);
    Ok(Served {
        index,
        docs,
        manifest: req.manifest,
        index_dir: req.index_dir,
        root,
    })
}

/// Load and check the tree once, then serve; over TCP the bound address
/// printed on `out` is the readiness line.
pub fn run_serve(
    req: &ServeRequest<'_>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), String> {
    let mut session = hold(req)?;
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
    match listener {
        None => serve_lines(&mut session, input, out).map(|_| ()),
        Some(listener) => serve_tcp(&mut session, listener),
    }
}

/// `--status` (printed) or `--shutdown`.
pub fn manage(addr: &str, shutdown: bool, out: &mut dyn Write) -> Result<Outcome, String> {
    if shutdown {
        ask::<Value>(addr, "shutdown")?;
    } else {
        let status = serde_json::to_string(&status(addr)?)
            .map_err(|e| format!("error: {addr}: unexpected result: {e}"))?;
        print::line(out, &status)?;
    }
    Ok(Outcome::Done)
}

/// One method without params.
fn ask<T: serde::de::DeserializeOwned>(addr: &str, method: &str) -> Result<T, String> {
    ask_on(&mut Conn::open(addr)?, method)
}

fn ask_on<T: serde::de::DeserializeOwned>(conn: &mut Conn, method: &str) -> Result<T, String> {
    let result = conn.call(method, serde_json::json!({}), &mut |_| Ok(()))?;
    conn.decode(result)
}

pub fn status(addr: &str) -> Result<ServeStatus, String> {
    ask(addr, "status")
}

/// `status`, on a connection that stays open.
pub fn status_on(conn: &mut Conn) -> Result<ServeStatus, String> {
    ask_on(conn, "status")
}

/// Send one request to a server and print its result as a search would.
pub fn run_client(
    conn: &mut Conn,
    p: &QueryParams,
    json: bool,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    let sent =
        serde_json::to_value(p).map_err(|e| format!("error: failed to encode request: {e}"))?;
    let printed = p.mode != Mode::Counts && (!json || p.mode == Mode::JsonMatches);
    let mut pipe = Pipe::new(&mut *out);
    let result = conn.call("query", sent, &mut |b| match printed {
        true => pipe.raw(b),
        false => Ok(()),
    })?;
    pipe.finish()?;
    let report = match p.mode {
        Mode::Counts => conn.decode(result)?,
        _ => conn.decode::<Rendered>(result)?.report,
    };
    if printed {
        return Ok(Outcome::Query(report));
    }
    render(p, json, out, |_| Ok(report)).map(Outcome::Query)
}
