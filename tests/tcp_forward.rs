mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};

use std::time::{Duration, Instant};

const IO_TIMEOUT: Duration = Duration::from_secs(7);

struct Server {
    _process: common::Server,
    endpoints: Vec<SocketAddr>,
}

impl Server {
    fn start(upstream: &str) -> Self {
        let process = common::Server::start(&["127.0.0.1", "127.0.0.2"], 2, IO_TIMEOUT, |_| {
            format!("trust-dns {upstream},udp://127.0.0.1#1\n")
        });
        Self {
            endpoints: process.endpoints.clone(),
            _process: process,
        }
    }

    fn query(&self, endpoint: SocketAddr, query: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect_timeout(&endpoint, IO_TIMEOUT).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        // Split both prefix and payload across writes; the server must read fully.
        let prefix = (query.len() as u16).to_be_bytes();
        stream.write_all(&prefix[..1]).unwrap();
        stream.write_all(&prefix[1..]).unwrap();
        stream.write_all(&query[..13]).unwrap();
        stream.write_all(&query[13..]).unwrap();
        let response = read_frame(&mut stream);
        assert_eq!(stream.read(&mut [0]).unwrap(), 0, "one query then EOF");
        response
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(start.elapsed() < IO_TIMEOUT, "TCP upstream not contacted");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("TCP accept: {error}"),
        }
    }
}

fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut prefix = [0; 2];
    stream.read_exact(&mut prefix).unwrap();
    let mut wire = vec![0; u16::from_be_bytes(prefix) as usize];
    stream.read_exact(&mut wire).unwrap();
    wire
}

fn query() -> Vec<u8> {
    let mut wire = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1];
    wire.extend_from_slice(b"\x02q1\x03gfw\x07example\x00\x00\x01\x00\x01");
    wire.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    wire
}

fn answer(query: &[u8], tc: bool, rcode: u8) -> Vec<u8> {
    let mut wire = query[..32].to_vec();
    wire[2] = 0x81 | if tc { 2 } else { 0 };
    wire[3] = rcode;
    wire[7] = 1;
    wire.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 0, 113, 7]);
    wire.extend_from_slice(&query[32..]);
    wire
}

fn roundtrip(proto: &str, tcp: bool, tc: bool, rcode: u8) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    let port = if tcp {
        listener.local_addr().unwrap().port()
    } else {
        udp.local_addr().unwrap().port()
    };
    let server = Server::start(&format!("{proto}127.0.0.1#{port}"));
    let count = server.endpoints.len();
    let worker = std::thread::spawn(move || {
        for _ in 0..count {
            let mut stream = tcp.then(|| accept(&listener));
            let mut buffer = [0; 65535];
            let (received, peer) = if let Some(stream) = stream.as_mut() {
                (read_frame(stream), None)
            } else {
                let (len, peer) = udp.recv_from(&mut buffer).unwrap();
                (buffer[..len].to_vec(), Some(peer))
            };
            assert_ne!(received[..2], query()[..2], "rewritten upstream ID");
            assert_eq!(received[2..], query()[2..], "raw query including EDNS");
            let response = answer(&received, tc, rcode);
            if let Some(stream) = stream.as_mut() {
                let prefix = (response.len() as u16).to_be_bytes();
                stream.write_all(&prefix[..1]).unwrap();
                stream.write_all(&prefix[1..]).unwrap();
                for part in response.chunks(7) {
                    stream.write_all(part).unwrap();
                }
                assert_eq!(stream.read(&mut [0]).unwrap(), 0, "upstream closed");
            } else {
                udp.send_to(&response, peer.unwrap()).unwrap();
            }
        }
    });
    for &endpoint in &server.endpoints {
        let response = server.query(endpoint, &query());
        let mut expected = answer(&query(), tc, rcode);
        expected[3] |= 0x80;
        if matches!(rcode, 0 | 3) && !tc {
            expected.truncate(expected.len() - 11);
            expected[11] = 0;
        }
        assert_eq!(response, expected, "client ID/RA/conditional AR/raw answer");
    }
    worker.join().unwrap();
    println!(
        "TCP client -> {proto}upstream (tcp={tcp}): four bind endpoints OK; ID=0x4242 RA=1 TC={tc} rcode={rcode}"
    );
}

#[test]
fn tcp_upstream() {
    for (tc, rcode) in [(false, 0), (false, 3), (true, 0), (false, 2)] {
        roundtrip("tcp://", true, tc, rcode);
    }
}

#[test]
fn explicit_udp_upstream() {
    roundtrip("udp://", false, false, 0);
}

#[test]
fn implicit_protocol_follows_tcp_client() {
    roundtrip("", true, false, 0);
}

#[test]
fn timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = Server::start(&format!(
        "tcp://127.0.0.1#{}",
        listener.local_addr().unwrap().port()
    ));
    let worker = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let received = read_frame(&mut stream);
        assert_ne!(received[..2], query()[..2]);
        assert_eq!(received[2..], query()[2..]);
        // Keep the connection silent until the server times out and closes it.
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "one upstream attempt"
        );
    });
    let start = Instant::now();
    let response = server.query(server.endpoints[0], &query());
    assert!(start.elapsed() >= Duration::from_secs(5));
    let mut expected = query()[..32].to_vec();
    expected[2] = 0x81;
    expected[3] = 0x82;
    expected[6..12].fill(0);
    assert_eq!(response, expected, "SERVFAIL with client ID/RA/question");
    worker.join().unwrap();
    println!("TCP timeout: >=5 s, one attempt, SERVFAIL ID=0x4242 RA=1 AR=0, client EOF");
}
