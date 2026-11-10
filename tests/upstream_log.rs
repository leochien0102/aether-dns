mod common;

use common::Server;
use std::net::{TcpListener, UdpSocket};
use std::time::Duration;

#[test]
fn failure_log_names_upstream_not_client() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream = closed.local_addr().unwrap();
    let server = Server::start(&["127.0.0.1"], 1, Duration::from_secs(3), |_| {
        format!(
            "default-tag gfw\ntrust-dns tcp://127.0.0.1#{}\n",
            upstream.port()
        )
    });
    drop(closed);
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let client_addr = client.local_addr().unwrap();
    let query =
        b"\x42\x42\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x01a\x07example\x00\x00\x01\x00\x01";
    client.send_to(query, server.endpoints[0]).unwrap();
    let line = server
        .logs
        .recv_timeout(Duration::from_secs(3))
        .expect("next stderr line must diagnose the failed query");
    assert!(line.starts_with("upstream failed for client "), "{line}");
    assert!(line.contains(&format!("tcp://{upstream}: ")), "{line}");
    assert!(!line.contains(&format!("upstream {client_addr}")), "{line}");
}
