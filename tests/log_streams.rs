//! Stream contract: stdout carries lifecycle/informational lines, stderr only errors.
//!
//! procd records a child's stderr as daemon.err and stdout as daemon.info, so a
//! lifecycle line on stderr shows up in syslog as a false error.

mod common;

use std::net::{TcpListener, UdpSocket};
use std::process::Command;
use std::time::Duration;

const QUERY: &[u8] =
    b"\x42\x42\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x01a\x07example\x00\x00\x01\x00\x01";

#[test]
fn lifecycle_and_upstream_diagnostics_use_stdout() {
    // A closed TCP port makes the first query fail immediately with a recoverable
    // upstream error, which must be reported on stdout (daemon.info), not stderr.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream = closed.local_addr().unwrap();
    let server = common::Server::start(&["127.0.0.1"], 1, Duration::from_secs(3), |_| {
        format!(
            "default-tag gfw\ntrust-dns tcp://127.0.0.1#{}\n",
            upstream.port()
        )
    });
    drop(closed);

    // Server::start already consumed the two readiness lines from stdout, so any
    // line still buffered on stderr means a lifecycle line leaked to the error stream.
    assert!(
        server.errors.try_recv().is_err(),
        "startup must not write to stderr"
    );

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.send_to(QUERY, server.endpoints[0]).unwrap();
    let mut response = vec![0; 65535];
    let len = client.recv(&mut response).unwrap();
    assert!(len > 12, "expected one synthetic response, got {len} bytes");

    let line = server
        .info
        .recv_timeout(Duration::from_secs(3))
        .expect("failed query must be diagnosed on stdout");
    assert!(line.starts_with("upstream failed for client "), "{line}");
    assert!(
        line.contains(&format!("tcp://{upstream}")),
        "diagnostic must name the real upstream: {line}"
    );
    assert!(
        server.errors.try_recv().is_err(),
        "recoverable upstream failure must not use stderr"
    );
}

#[test]
fn genuine_errors_use_stderr() {
    // A missing configuration file is a startup error: stdout stays empty.
    let output = Command::new(env!("CARGO_BIN_EXE_aether-dns"))
        .arg("-C")
        .arg("/nonexistent/aether-dns.conf")
        .output()
        .expect("run aether-dns");
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "startup errors must not be written to stdout"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("aether-dns.conf"),
        "stderr should carry the error: {stderr}"
    );
}
