#![allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/dns/mod.rs"]
mod dns;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "../src/lists.rs"]
mod lists;
#[path = "../src/server.rs"]
mod server;
#[path = "../src/upstream.rs"]
mod upstream;
#[path = "../src/upstream_group.rs"]
mod upstream_group;

use config::{Config, Protocol, Tag, Upstream};
use server::{OsRandomIdSource, UpstreamIdSource, cache::Clock, select_upstream_id};
use std::{
    collections::VecDeque,
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    time::timeout,
};

#[derive(Default)]
struct FakeClock(AtomicU64);

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

impl FakeClock {
    fn at(&self, millis: u64) {
        self.0.store(millis, Ordering::SeqCst);
    }
}

struct NoIpTest;

impl server::ip_test::IpTest for NoIpTest {
    fn is_china(&self, _: std::net::IpAddr) -> bool {
        false
    }
}

/// Deterministic ID source that yields a scripted sequence of results and counts draws.
struct ScriptedIdSource {
    values: Mutex<VecDeque<io::Result<u16>>>,
    draws: AtomicUsize,
}

impl ScriptedIdSource {
    fn from_ids(ids: &[u16]) -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(ids.iter().copied().map(Ok).collect()),
            draws: AtomicUsize::new(0),
        })
    }

    fn from_results(results: Vec<io::Result<u16>>) -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(results.into()),
            draws: AtomicUsize::new(0),
        })
    }

    fn draws(&self) -> usize {
        self.draws.load(Ordering::SeqCst)
    }
}

impl UpstreamIdSource for ScriptedIdSource {
    fn next_u16(&self) -> io::Result<u16> {
        self.draws.fetch_add(1, Ordering::SeqCst);
        self.values
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::other("scripted ID source exhausted")))
    }
}

struct ConstantIdSource {
    value: u16,
    draws: AtomicUsize,
}

impl ConstantIdSource {
    fn new(value: u16) -> Self {
        Self {
            value,
            draws: AtomicUsize::new(0),
        }
    }
}

impl UpstreamIdSource for ConstantIdSource {
    fn next_u16(&self) -> io::Result<u16> {
        self.draws.fetch_add(1, Ordering::SeqCst);
        Ok(self.value)
    }
}

struct FailingIdSource;

impl UpstreamIdSource for FailingIdSource {
    fn next_u16(&self) -> io::Result<u16> {
        Err(io::Error::other("csprng unavailable"))
    }
}

fn build_query(client_id: u16, label: &str) -> Vec<u8> {
    let mut wire = Vec::with_capacity(24 + label.len());
    wire.extend_from_slice(&client_id.to_be_bytes());
    wire.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    wire.push(label.len() as u8);
    wire.extend_from_slice(label.as_bytes());
    wire.extend_from_slice(b"\x07example\x00\x00\x01\x00\x01");
    wire
}

fn build_answer(query: &[u8], ttl: u32, ip: [u8; 4]) -> Vec<u8> {
    let mut r = query.to_vec();
    r[2] = 0x81;
    r[3] = 0x80;
    r[7] = 1;
    r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
    r.extend_from_slice(&ttl.to_be_bytes());
    r.extend_from_slice(&[0, 4]);
    r.extend_from_slice(&ip);
    r
}

fn wire_id(packet: &[u8]) -> u16 {
    u16::from_be_bytes([packet[0], packet[1]])
}

fn first_ttl(response: &[u8]) -> u32 {
    hickory_proto::op::Message::from_vec(response)
        .unwrap()
        .answers()[0]
        .ttl()
}

async fn udp_ask(port: u16, query: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket
            .send_to(query, (Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut buf = vec![0; 4096];
        let n = socket.recv(&mut buf).await.unwrap();
        buf.truncate(n);
        buf
    })
    .await
    .expect("UDP query timed out")
}

async fn tcp_ask(port: u16, query: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream.write_u16(query.len() as u16).await.unwrap();
        stream.write_all(query).await.unwrap();
        let len = stream.read_u16().await.unwrap() as usize;
        let mut buf = vec![0; len];
        stream.read_exact(&mut buf).await.unwrap();
        buf
    })
    .await
    .expect("TCP query timed out")
}

#[test]
fn select_upstream_id_uses_source_and_redraws_on_collision() {
    let source = ScriptedIdSource::from_ids(&[0x4242, 0x4242, 0xBEEF, 0xCAFE]);
    assert_eq!(select_upstream_id(&*source, Some(0x4242)).unwrap(), 0xBEEF);
    assert_eq!(source.draws(), 3);
    assert_eq!(select_upstream_id(&*source, Some(0x1111)).unwrap(), 0xCAFE);
    assert_eq!(source.draws(), 4);
}

#[test]
fn select_upstream_id_fails_closed_on_source_error_and_stuck_collision() {
    let err_source = ScriptedIdSource::from_results(vec![
        Ok(0x4242),
        Err(io::Error::other("entropy failure")),
    ]);
    let err = select_upstream_id(&*err_source, Some(0x4242)).unwrap_err();
    assert!(err.to_string().contains("entropy failure"));
    assert_eq!(err_source.draws(), 2);

    let stuck = ConstantIdSource::new(0x4242);
    let err = select_upstream_id(&stuck, Some(0x4242)).unwrap_err();
    assert!(err.to_string().contains("collided"));
    assert_eq!(stuck.draws.load(Ordering::SeqCst), 16);
}

#[test]
fn os_random_id_source_reads_kernel_csprng() {
    let source = OsRandomIdSource;
    assert!(source.next_u16().is_ok());
}

#[tokio::test]
async fn udp_path_uses_scripted_id_skips_client_collision_and_restores_client_id() {
    let upstream_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let upstream_addr = upstream_sock.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<u16>();
    let worker = {
        let sock = upstream_sock.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            for _ in 0..2 {
                let (n, peer) = sock.recv_from(&mut buf).await.unwrap();
                let req = &buf[..n];
                tx.send(wire_id(req)).unwrap();
                let reply = build_answer(req, 60, [203, 0, 113, 10]);
                sock.send_to(&reply, peer).await.unwrap();
            }
        })
    };

    let id_source = ScriptedIdSource::from_ids(&[0x1111, 0xA101, 0xB202]);
    let config = Config {
        default_tag: Some(Tag::Gfw),
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Udp),
            host: None,
            ip: upstream_addr.ip(),
            port: Some(upstream_addr.port()),
        }],
        ..Config::default()
    };
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let id_source = id_source.clone();
        async move {
            server::run_with_id_source(
                config,
                receiver,
                Arc::new(NoIpTest),
                Arc::new(server::ip_writer::NoopWriter),
                Arc::new(FakeClock::default()),
                id_source,
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;

    let r1 = udp_ask(port, &build_query(0x1111, "u1")).await;
    assert_eq!(wire_id(&r1), 0x1111, "UDP client ID must be restored");
    assert_eq!(r1[3] & 15, 0);
    assert_eq!(
        rx.recv().await.unwrap(),
        0xA101,
        "colliding 0x1111 must be skipped by redrawing 0xA101"
    );

    let r2 = udp_ask(port, &build_query(0x2222, "u2")).await;
    assert_eq!(wire_id(&r2), 0x2222, "second UDP client ID must be restored");
    assert_eq!(r2[3] & 15, 0);
    assert_eq!(rx.recv().await.unwrap(), 0xB202);
    assert_eq!(id_source.draws(), 3);

    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    worker.await.unwrap();
}

#[tokio::test]
async fn tcp_path_uses_scripted_id_skips_client_collision_and_restores_client_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<u16>();
    let worker = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let len = stream.read_u16().await.unwrap() as usize;
            let mut req = vec![0u8; len];
            stream.read_exact(&mut req).await.unwrap();
            tx.send(wire_id(&req)).unwrap();
            let reply = build_answer(&req, 60, [203, 0, 113, 20]);
            stream.write_u16(reply.len() as u16).await.unwrap();
            stream.write_all(&reply).await.unwrap();
            let _ = stream.shutdown().await;
        }
    });

    let id_source = ScriptedIdSource::from_ids(&[0x3333, 0xC303, 0xD404]);
    let config = Config {
        default_tag: Some(Tag::Gfw),
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: upstream_addr.ip(),
            port: Some(upstream_addr.port()),
        }],
        ..Config::default()
    };
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let id_source = id_source.clone();
        async move {
            server::run_with_id_source(
                config,
                receiver,
                Arc::new(NoIpTest),
                Arc::new(server::ip_writer::NoopWriter),
                Arc::new(FakeClock::default()),
                id_source,
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;

    let r1 = tcp_ask(port, &build_query(0x3333, "t1")).await;
    assert_eq!(wire_id(&r1), 0x3333, "TCP client ID must be restored");
    assert_eq!(r1[3] & 15, 0);
    assert_eq!(
        rx.recv().await.unwrap(),
        0xC303,
        "colliding 0x3333 must be skipped by redrawing 0xC303"
    );

    let r2 = tcp_ask(port, &build_query(0x4444, "t2")).await;
    assert_eq!(wire_id(&r2), 0x4444, "second TCP client ID must be restored");
    assert_eq!(r2[3] & 15, 0);
    assert_eq!(rx.recv().await.unwrap(), 0xD404);
    assert_eq!(id_source.draws(), 3);

    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    worker.await.unwrap();
}

#[tokio::test]
async fn cache_hit_does_not_query_upstream_and_background_refresh_allocates_fresh_id() {
    let upstream_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let upstream_addr = upstream_sock.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<(Vec<u8>, SocketAddr)>();
    let hits = Arc::new(AtomicUsize::new(0));
    let worker = {
        let sock = upstream_sock.clone();
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let (n, peer) = sock.recv_from(&mut buf).await.unwrap();
                hits.fetch_add(1, Ordering::SeqCst);
                tx.send((buf[..n].to_vec(), peer)).unwrap();
            }
        })
    };

    let clock = Arc::new(FakeClock::default());
    let id_source = ScriptedIdSource::from_ids(&[0xE001, 0x5555, 0xE002]);
    let config = Config {
        cache: Some(32),
        cache_stale: Some(30),
        default_tag: Some(Tag::Gfw),
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Udp),
            host: None,
            ip: upstream_addr.ip(),
            port: Some(upstream_addr.port()),
        }],
        ..Config::default()
    };
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let clock = clock.clone();
        let id_source = id_source.clone();
        async move {
            server::run_with_id_source(
                config,
                receiver,
                Arc::new(NoIpTest),
                Arc::new(server::ip_writer::NoopWriter),
                clock,
                id_source,
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;

    // 1. Prime cache: client ID 0x1234 -> upstream receives 0xE001.
    let prime_client = tokio::spawn(async move { udp_ask(port, &build_query(0x1234, "ref")).await });
    let (req1, peer1) = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wire_id(&req1), 0xE001);
    upstream_sock
        .send_to(&build_answer(&req1, 12, [198, 51, 100, 1]), peer1)
        .await
        .unwrap();
    let r1 = prime_client.await.unwrap();
    assert_eq!(wire_id(&r1), 0x1234);
    assert_eq!(first_ttl(&r1), 12);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(id_source.draws(), 1);

    // 2. Fresh cache hit (no refresh): must restore client ID without contacting upstream or drawing an ID.
    clock.at(1000);
    let r2 = udp_ask(port, &build_query(0x9999, "ref")).await;
    assert_eq!(wire_id(&r2), 0x9999);
    assert_eq!(first_ttl(&r2), 11);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(id_source.draws(), 1);

    // 3. Refresh-ahead hit: returns cached answer with client ID 0x5555 immediately;
    // background refresh skips colliding 0x5555 and sends fresh 0xE002.
    clock.at(8000);
    let r3 = udp_ask(port, &build_query(0x5555, "ref")).await;
    assert_eq!(wire_id(&r3), 0x5555);
    assert_eq!(first_ttl(&r3), 4);

    let (req2, peer2) = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wire_id(&req2),
        0xE002,
        "background refresh must allocate a fresh ID and skip triggering client ID 0x5555"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(id_source.draws(), 3);
    upstream_sock
        .send_to(&build_answer(&req2, 30, [198, 51, 100, 2]), peer2)
        .await
        .unwrap();

    // Wait for refreshed cache entry to become visible and verify client ID restoration.
    timeout(Duration::from_secs(2), async {
        loop {
            let reply = udp_ask(port, &build_query(0x7777, "ref")).await;
            assert_eq!(wire_id(&reply), 0x7777);
            if first_ttl(&reply) == 30 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(id_source.draws(), 3);

    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    worker.abort();
}

#[tokio::test]
async fn parallel_attempts_for_one_logical_query_share_allocated_id() {
    let china_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let trust_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let china_addr = china_sock.local_addr().unwrap();
    let trust_addr = trust_sock.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<(&'static str, u16)>();

    let china_worker = {
        let sock = china_sock.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (n, peer) = sock.recv_from(&mut buf).await.unwrap();
            let req = &buf[..n];
            tx.send(("china", wire_id(req))).unwrap();
            // Return non-China IP so trust answer is selected after both finish.
            let reply = build_answer(req, 60, [203, 0, 113, 30]);
            sock.send_to(&reply, peer).await.unwrap();
        })
    };
    let trust_worker = {
        let sock = trust_sock.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (n, peer) = sock.recv_from(&mut buf).await.unwrap();
            let req = &buf[..n];
            tx.send(("trust", wire_id(req))).unwrap();
            let reply = build_answer(req, 60, [203, 0, 113, 31]);
            sock.send_to(&reply, peer).await.unwrap();
        })
    };

    let id_source = ScriptedIdSource::from_ids(&[0xF111]);
    let config = Config {
        default_tag: Some(Tag::None),
        china_dns: vec![Upstream {
            protocol: Some(Protocol::Udp),
            host: None,
            ip: china_addr.ip(),
            port: Some(china_addr.port()),
        }],
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Udp),
            host: None,
            ip: trust_addr.ip(),
            port: Some(trust_addr.port()),
        }],
        ..Config::default()
    };
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let id_source = id_source.clone();
        async move {
            server::run_with_id_source(
                config,
                receiver,
                Arc::new(NoIpTest),
                Arc::new(server::ip_writer::NoopWriter),
                Arc::new(FakeClock::default()),
                id_source,
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;

    let reply = udp_ask(port, &build_query(0x6666, "dual")).await;
    assert_eq!(wire_id(&reply), 0x6666);
    assert_eq!(reply[3] & 15, 0);

    let first = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let second = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.1, 0xF111);
    assert_eq!(second.1, 0xF111);
    assert_ne!(first.0, second.0);
    assert_eq!(id_source.draws(), 1);

    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    china_worker.await.unwrap();
    trust_worker.await.unwrap();
}

#[tokio::test]
async fn id_source_failure_fails_closed_without_contacting_upstream() {
    let upstream_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_sock.local_addr().unwrap();
    let config = Config {
        default_tag: Some(Tag::Gfw),
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Udp),
            host: None,
            ip: upstream_addr.ip(),
            port: Some(upstream_addr.port()),
        }],
        ..Config::default()
    };
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        async move {
            server::run_with_id_source(
                config,
                receiver,
                Arc::new(NoIpTest),
                Arc::new(server::ip_writer::NoopWriter),
                Arc::new(FakeClock::default()),
                Arc::new(FailingIdSource),
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;

    let udp_reply = udp_ask(port, &build_query(0xABCD, "failudp")).await;
    assert_eq!(wire_id(&udp_reply), 0xABCD);
    assert_eq!(udp_reply[3] & 15, 2, "UDP must fail closed with SERVFAIL");

    let tcp_reply = tcp_ask(port, &build_query(0xDCBA, "failtcp")).await;
    assert_eq!(wire_id(&tcp_reply), 0xDCBA);
    assert_eq!(tcp_reply[3] & 15, 2, "TCP must fail closed with SERVFAIL");

    let mut buf = [0u8; 64];
    assert_eq!(
        upstream_sock.try_recv(&mut buf).unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "failing ID source must never transmit any upstream packet"
    );

    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
}
