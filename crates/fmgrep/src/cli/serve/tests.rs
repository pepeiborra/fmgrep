use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use super::*;
use crate::output::MatchWriter;

/// `payload` as one frame.
fn frame(payload: &str) -> String {
    format!("{}\n{payload}", payload.len())
}

/// A one-connection server that answers the first request line with
/// `reply`, then closes.
fn fake_server(reply: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream).write_all(reply.as_bytes()).unwrap();
    });
    addr
}

/// The client that merges its own files with the server's reads one frame
/// per file: what it printed before a server error or a cut connection
/// stays printed.
#[test]
fn a_server_error_after_a_frame_per_file_keeps_the_output() {
    let error =
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32000,\"message\":\"error: boom\"}}\n";
    let sent = [
        frame("{\"term\":\"printk\"}\n"),
        frame("{\"doc\":\"a\",\"path\":\"a\",\"stats\":[1,1,6]}\nhello!"),
        error.to_owned(),
    ]
    .concat();
    let cut = [frame("{\"term\":\"printk\"}\n"), "60\n{\"doc\"".to_owned()].concat();
    // A length no frame can have allocates nothing before its bytes.
    let huge = format!("{}\n{{\"doc\"", usize::MAX);
    for (reply, printed, error) in [
        (sent, &b"a\nhello!"[..], "server: boom"),
        (cut, b"", "{addr}: connection closed mid-frame"),
        (huge, b"", "{addr}: connection closed mid-frame"),
    ] {
        let addr = fake_server(reply);
        let mut conn = Conn::open(&addr).unwrap();
        let p = QueryParams {
            terms: vec!["printk".to_owned()],
            stream: true,
            ..QueryParams::default()
        };
        let mut out = Vec::new();
        let got = remote(
            &mut conn,
            &p,
            &mut MatchWriter::new(&mut out, p.output_config()),
        );
        assert_eq!(
            got.err().unwrap(),
            format!("error: {}", error.replace("{addr}", &addr))
        );
        assert_eq!(out, printed);
    }
}
