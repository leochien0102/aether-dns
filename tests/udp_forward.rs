mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::process::Command;
use std::time::{Duration, Instant};

struct Server {
    _process: common::Server,
    port: u16,
}
impl Server {
    fn start(config: &str) -> Self {
        Self::start_with_ports(config, 1, &["127.0.0.1"])
    }
    fn start_with_ports(config: &str, count: usize, addresses: &[&str]) -> Self {
        let process = common::Server::start(addresses, count, Duration::from_secs(7), |_| {
            config.to_owned()
        });
        Self {
            port: process.endpoints[0].port(),
            _process: process,
        }
    }
    fn query(&self, query: &[u8]) -> Vec<u8> {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        socket.send_to(query, ("127.0.0.1", self.port)).unwrap();
        let mut response = vec![0; 65535];
        let len = socket.recv(&mut response).unwrap();
        response.truncate(len);
        response
    }
}
fn query(qtype: u16, edns: bool) -> Vec<u8> {
    let mut wire = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, u8::from(edns)];
    wire.extend_from_slice(b"\x02q1\x03gfw\x07example\x00");
    wire.extend_from_slice(&qtype.to_be_bytes());
    wire.extend_from_slice(&[0, 1]);
    if edns {
        wire.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    }
    wire
}
fn answer(query: &[u8], qtype: u16, flags: u8, rcode: u8) -> Vec<u8> {
    let question_end = 32;
    let mut wire = query[..question_end].to_vec();
    wire[2] = 0x81 | flags;
    wire[3] = rcode;
    wire[7] = 1;
    wire[11] = 1;
    wire.extend_from_slice(&[0xc0, 0x0c]);
    wire.extend_from_slice(&qtype.to_be_bytes());
    wire.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
    let data: Vec<u8> = match qtype {
        1 => vec![203, 0, 113, 7],
        28 => vec![0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
        65 => vec![0, 1, 0],
        _ => unreachable!(),
    };
    wire.extend_from_slice(&(data.len() as u16).to_be_bytes());
    wire.extend(data);
    wire.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    wire
}
fn roundtrip(qtype: u16, edns: bool, tcp: bool, flags: u8, rcode: u8) {
    let original = query(qtype, edns);
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = if tcp {
        tcp_listener.local_addr().unwrap().port()
    } else {
        udp.local_addr().unwrap().port()
    };
    let sent = original.clone();
    let worker = std::thread::spawn(move || {
        let mut buffer = vec![0; 65535];
        let (received, mut stream, peer) = if tcp {
            let (mut stream, _) = tcp_listener.accept().unwrap();
            let mut prefix = [0; 2];
            stream.read_exact(&mut prefix).unwrap();
            let len = u16::from_be_bytes(prefix) as usize;
            stream.read_exact(&mut buffer[..len]).unwrap();
            (buffer[..len].to_vec(), Some(stream), None)
        } else {
            let (len, peer) = udp.recv_from(&mut buffer).unwrap();
            (buffer[..len].to_vec(), None, Some(peer))
        };
        assert_ne!(received[..2], sent[..2]);
        assert_eq!(received[2..], sent[2..], "raw query including EDNS");
        let response = answer(&received, qtype, flags, rcode);
        if let Some(stream) = stream.as_mut() {
            stream
                .write_all(&(response.len() as u16).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        } else {
            udp.send_to(&response, peer.unwrap()).unwrap();
        }
    });
    let proto = if tcp { "tcp://" } else { "udp://" };
    // Filtering is covered by tests/filters.rs; this transport matrix uses
    // an unfiltered configuration. Keep the second trust and cache settings.
    let server = Server::start(&format!(
        "trust-dns {proto}127.0.0.1#{port},udp://127.0.0.1#1\ncache 4096\n"
    ));
    let response = server.query(&original);
    worker.join().unwrap();
    let mut expected = answer(&original, qtype, flags, rcode);
    expected[3] |= 0x80;
    if matches!(rcode, 0 | 3) && flags & 2 == 0 {
        expected.truncate(expected.len() - 11);
        expected[11] = 0;
    }
    assert_eq!(
        response, expected,
        "raw response, client ID, RA, conditional additional"
    );
}
#[test]
fn a() {
    roundtrip(1, false, false, 0, 0);
}
#[test]
fn aaaa() {
    roundtrip(28, false, false, 0, 0);
}
#[test]
fn https() {
    roundtrip(65, false, true, 0, 0);
}
#[test]
fn edns() {
    roundtrip(1, true, true, 0, 0);
}
#[test]
fn conditional_additional() {
    roundtrip(1, true, false, 0, 3);
    roundtrip(1, true, false, 2, 0);
    roundtrip(1, true, false, 0, 2);
}
#[test]
fn timeout() {
    let upstream = UdpSocket::bind("127.0.0.1:0").unwrap();
    let server = Server::start(&format!(
        "trust-dns 127.0.0.1#{}\n",
        upstream.local_addr().unwrap().port()
    ));
    let query = query(1, true);
    let start = Instant::now();
    let response = server.query(&query);
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert_eq!(&response[..2], &query[..2]);
    assert_eq!(response[3] & 0x8f, 0x82);
    assert_eq!(&response[6..12], &[0; 6]);
    assert_eq!(&response[12..], &query[12..32]);
    upstream.set_nonblocking(true).unwrap();
    let mut buffer = [0; 512];
    upstream.recv(&mut buffer).unwrap();
    assert_eq!(
        upstream.recv(&mut buffer).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
#[test]
fn startup_rejects_invalid_config() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/udp-forward-invalid.conf");
    for (config, error) in [
        ("# comment\nunknown-option yes\n", ":2:"),
        ("trust-dns tls://127.0.0.1\n", "tls:// P0-conditional"),
        ("bind-port 31053@udp\n", "P1"),
    ] {
        std::fs::write(&path, config).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_aether-dns"))
            .arg("-C")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().contains(error));
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn all_bind_addresses_and_ports() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = Server::start_with_ports(
        &format!("trust-dns tcp://127.0.0.1#{port}\n"),
        2,
        &["127.0.0.1", "127.0.0.2"],
    );
    let second = server._process.endpoints[1].port();
    for address in ["127.0.0.1", "127.0.0.2"] {
        for port in [server.port, second] {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket.send_to(&query(1, false), (address, port)).unwrap();
            let mut buffer = [0; 512];
            let len = socket.recv(&mut buffer).unwrap();
            assert_eq!(buffer[3] & 0x8f, 0x82);
            assert_eq!(len, 32);
        }
    }
}

#[test]
fn connection_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = Server::start(&format!("trust-dns tcp://127.0.0.1#{port}\n"));
    let response = server.query(&query(1, false));
    assert_eq!(response[3] & 0x8f, 0x82);
    assert_eq!(&response[12..], &query(1, false)[12..]);
}
