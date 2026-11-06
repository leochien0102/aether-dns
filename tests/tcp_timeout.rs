mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::process::Command;
use std::time::{Duration, Instant};

const SECOND: Duration = Duration::from_secs(1);

struct Server {
    process: common::Server,
    endpoint: SocketAddr,
}

impl Server {
    fn start(upstream: &UdpSocket) -> Self {
        let process = common::Server::start(&["127.0.0.1"], 1, SECOND, |_| {
            format!(
                "trust-dns udp://127.0.0.1#{}\n",
                upstream.local_addr().unwrap().port()
            )
        });
        Self {
            endpoint: process.endpoints[0],
            process,
        }
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect_timeout(&self.endpoint, SECOND).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        stream.set_write_timeout(Some(SECOND)).unwrap();
        stream
    }
}

fn upstream() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(SECOND)).unwrap();
    socket
}

fn query() -> Vec<u8> {
    let mut wire = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    wire.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
    wire
}

fn send_frame(stream: &mut TcpStream) {
    let query = query();
    stream
        .write_all(&(query.len() as u16).to_be_bytes())
        .unwrap();
    stream.write_all(&query).unwrap();
}

fn eof(stream: &mut TcpStream) {
    assert_eq!(
        stream.read(&mut [0]).unwrap(),
        0,
        "EOF without response bytes"
    );
}

fn no_upstream(socket: &UdpSocket) {
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert!(matches!(
        socket.recv_from(&mut [0; 512]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
}

fn roundtrip(server: &Server, socket: &UdpSocket) {
    let mut client = server.connect();
    send_frame(&mut client);
    let mut buffer = [0; 512];
    let (len, peer) = socket.recv_from(&mut buffer).unwrap();
    assert_eq!(buffer[2..len], query()[2..]);
    assert_ne!(buffer[..2], query()[..2]);
    let mut answer = buffer[..len].to_vec();
    answer[2] = 0x81;
    answer[3] = 0x80;
    socket.send_to(&answer, peer).unwrap();
    let mut prefix = [0; 2];
    client.read_exact(&mut prefix).unwrap();
    let mut received = vec![0; u16::from_be_bytes(prefix) as usize];
    client.read_exact(&mut received).unwrap();
    answer[..2].copy_from_slice(&query()[..2]);
    assert_eq!(received, answer);
    eof(&mut client);
}

fn timed_eof(client: &mut TcpStream, start: Instant, label: &str) {
    eof(client);
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(4800),
        "early EOF: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(6), "late EOF: {elapsed:?}");
    println!("{label}: elapsed={elapsed:?}, EOF without DNS response");
}

#[test]
fn complete_frame_roundtrip() {
    let socket = upstream();
    let server = Server::start(&socket);
    roundtrip(&server, &socket);
    no_upstream(&socket);
    println!("complete frame: response ID/question preserved, EOF, one upstream query");
}

#[test]
fn idle_timeout_does_not_block_new_connections() {
    let socket = upstream();
    let mut server = Server::start(&socket);
    let start = Instant::now();
    let mut idle = server.connect();
    std::thread::sleep(Duration::from_millis(200));
    let serving = Instant::now();
    roundtrip(&server, &socket);
    assert!(serving.elapsed() < SECOND);
    println!(
        "new connection served during idle timeout: {:?}",
        serving.elapsed()
    );
    timed_eof(&mut idle, start, "no bytes");
    no_upstream(&socket);
    assert!(server.process.child.try_wait().unwrap().is_none());
}

#[test]
fn partial_length_times_out() {
    let socket = upstream();
    let server = Server::start(&socket);
    let start = Instant::now();
    let mut client = server.connect();
    client.write_all(&[0]).unwrap();
    timed_eof(&mut client, start, "one length byte");
    no_upstream(&socket);
}

#[test]
fn partial_body_times_out() {
    let socket = upstream();
    let server = Server::start(&socket);
    let start = Instant::now();
    let mut client = server.connect();
    client
        .write_all(&(query().len() as u16).to_be_bytes())
        .unwrap();
    client.write_all(&query()[..10]).unwrap();
    timed_eof(&mut client, start, "partial body");
    no_upstream(&socket);
}

#[test]
fn length_and_body_share_one_five_second_budget() {
    let socket = upstream();
    let server = Server::start(&socket);
    let start = Instant::now();
    let mut client = server.connect();
    let prefix = (query().len() as u16).to_be_bytes();
    client.write_all(&prefix[..1]).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    client.write_all(&prefix[1..]).unwrap();
    client.write_all(&query()[..10]).unwrap();
    timed_eof(
        &mut client,
        start,
        "length completed after 3 s; shared budget",
    );
    no_upstream(&socket);
}

#[test]
fn shutdown_preempts_timeout_and_keeps_shared_drain_budget() {
    for signal in ["TERM", "INT"] {
        let socket = upstream();
        let mut server = Server::start(&socket);
        let mut idle = server.connect();
        let mut partial = server.connect();
        partial.write_all(&[0]).unwrap();
        let mut active = server.connect();
        send_frame(&mut active);
        // An unanswered complete query holds drain open to its 500 ms budget.
        socket.recv_from(&mut [0; 512]).unwrap();
        let start = Instant::now();
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(server.process.child.id().to_string())
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            server.process.logs.recv_timeout(SECOND).unwrap(),
            "shutdown: draining queries (500 ms maximum)"
        );
        idle.set_read_timeout(Some(SECOND)).unwrap();
        partial.set_read_timeout(Some(SECOND)).unwrap();
        eof(&mut idle);
        eof(&mut partial);
        loop {
            assert!(start.elapsed() < SECOND, "frame timeout blocked drain");
            if let Some(status) = server.process.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(450));
        eof(&mut active);
        no_upstream(&socket);
        println!("{signal}: idle/partial EOF, exit=0, drain={elapsed:?}");
    }
}
