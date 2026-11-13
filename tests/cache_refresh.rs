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

use config::{Config, Tag, Upstream};
use server::cache::Clock;
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
    task::JoinHandle,
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
fn query(name: u8) -> Vec<u8> {
    vec![0, 42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, name, 0, 0, 1, 0, 1]
}
fn response(q: &[u8], ttl: u32, code: u8) -> Vec<u8> {
    let mut r = q.to_vec();
    r[2] = 0x81;
    r[3] = 0x80 | code;
    r[7] = 1;
    r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
    r.extend_from_slice(&ttl.to_be_bytes());
    r.extend_from_slice(&[0, 4, 1, 2, 3, 4]);
    r
}
fn ttl(r: &[u8]) -> u32 {
    hickory_proto::op::Message::from_vec(r).unwrap().answers()[0].ttl()
}
async fn ask(port: u16, name: u8) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket
            .send_to(&query(name), (Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut r = vec![0; 4096];
        let n = socket.recv(&mut r).await.unwrap();
        r.truncate(n);
        assert_eq!(&r[..2], &[0, 42]);
        assert_eq!(r[3] & 15, 0);
        r
    })
    .await
    .expect("cache hit must not wait for the gated upstream")
}
type Request = (Vec<u8>, SocketAddr);
struct Harness {
    port: u16,
    socket: Arc<UdpSocket>,
    pending: mpsc::UnboundedReceiver<Request>,
    hits: Arc<AtomicUsize>,
    clock: Arc<FakeClock>,
    shutdown: watch::Sender<bool>,
    serving: JoinHandle<Result<(), String>>,
    worker: JoinHandle<()>,
}
impl Harness {
    async fn new() -> Self {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = socket.local_addr().unwrap();
        let (sender, pending) = mpsc::unbounded_channel();
        let hits = Arc::new(AtomicUsize::new(0));
        let worker = {
            let socket = socket.clone();
            let hits = hits.clone();
            tokio::spawn(async move {
                let mut buf = [0; 4096];
                loop {
                    let (n, peer) = socket.recv_from(&mut buf).await.unwrap();
                    hits.fetch_add(1, Ordering::SeqCst);
                    sender.send((buf[..n].to_vec(), peer)).unwrap();
                }
            })
        };
        let config = Config {
            cache: Some(32),
            cache_stale: Some(30),
            bind_port: vec![],
            default_tag: Some(Tag::Gfw),
            trust_dns: vec![Upstream {
                protocol: None,
                host: None,
                ip: address.ip(),
                port: Some(address.port()),
            }],
            ..Config::default()
        };
        let clock = Arc::new(FakeClock::default());
        let (port, shutdown, serving) = in_process::start(|port, receiver| {
            let mut config = config.clone();
            config.bind_port = vec![port];
            let clock = clock.clone();
            async move {
                server::run_with_cache_clock(
                    config,
                    receiver,
                    Arc::new(NoIpTest),
                    Arc::new(server::ip_writer::NoopWriter),
                    clock,
                )
                .await
                .map_err(|e| e.to_string())
            }
        })
        .await;
        Self {
            port,
            socket,
            pending,
            hits,
            clock,
            shutdown,
            serving,
            worker,
        }
    }
    async fn request(&mut self) -> Request {
        timeout(Duration::from_secs(2), self.pending.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn reply(&self, request: Request, ttl: u32, code: u8) {
        self.socket
            .send_to(&response(&request.0, ttl, code), request.1)
            .await
            .unwrap();
    }
    async fn prime(&mut self, name: u8) {
        let client = tokio::spawn(ask(self.port, name));
        let request = self.request().await;
        self.reply(request, 12, 0).await;
        assert_eq!(ttl(&client.await.unwrap()), 12);
    }
    async fn updated(&self, name: u8, expected: u32) {
        timeout(Duration::from_secs(2), async {
            loop {
                if ttl(&ask(self.port, name).await) == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        timeout(Duration::from_millis(750), self.serving)
            .await
            .expect("refresh must not delay shutdown")
            .unwrap()
            .unwrap();
        self.worker.abort();
    }
}

#[tokio::test]
async fn refresh_ahead_returns_old_ttl_before_gated_upstream_and_updates_cache() {
    let mut h = Harness::new().await;
    h.prime(b'x').await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 1);
    assert_eq!(ttl(&ask(h.port, b'x').await), 12);
    assert_eq!(h.hits.load(Ordering::SeqCst), 1);
    h.clock.at(7999);
    assert_eq!(ttl(&ask(h.port, b'x').await), 5);
    assert_eq!(h.hits.load(Ordering::SeqCst), 1);
    h.clock.at(8000);
    // No upstream reply is possible until after this cache-hit response.
    assert_eq!(ttl(&ask(h.port, b'x').await), 4);
    let request = h.request().await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
    h.reply(request, 30, 0).await;
    h.updated(b'x', 30).await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
    h.stop().await;
}

#[tokio::test]
async fn ten_concurrent_stale_hits_refresh_once_and_shutdown_cancels_refresh() {
    let mut h = Harness::new().await;
    h.prime(b'x').await;
    h.clock.at(12000);
    let mut clients = Vec::new();
    for _ in 0..10 {
        clients.push(tokio::spawn(ask(h.port, b'x')));
    }
    for client in clients {
        assert_eq!(ttl(&client.await.unwrap()), 0);
    }
    let _blocked = h.request().await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
    assert!(h.pending.try_recv().is_err());
    h.stop().await;
}

#[tokio::test]
async fn failed_refresh_preserves_client_response_and_cools_down() {
    let mut h = Harness::new().await;
    h.prime(b'x').await;
    h.clock.at(12000);
    let before = ask(h.port, b'x').await;
    assert_eq!(ttl(&before), 0);
    let request = h.request().await;
    h.reply(request, 99, 2).await; // A valid upstream SERVFAIL is not cacheable.
    // Round trips let the ready upstream task finish; FakeClock stays fixed.
    for _ in 0..10 {
        assert_eq!(ask(h.port, b'x').await, before);
    }
    h.clock.at(16999);
    assert_eq!(ask(h.port, b'x').await, before);
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
    h.clock.at(17000);
    assert_eq!(ask(h.port, b'x').await, before);
    let request = h.request().await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 3);
    h.reply(request, 30, 0).await;
    h.updated(b'x', 30).await;
    h.stop().await;
}

#[tokio::test]
async fn at_most_four_distinct_keys_refresh_concurrently() {
    let mut h = Harness::new().await;
    for name in b'a'..=b'e' {
        h.prime(name).await;
    }
    h.clock.at(12000);
    for name in b'a'..=b'e' {
        assert_eq!(ttl(&ask(h.port, name).await), 0);
    }
    let mut requests = Vec::new();
    for _ in 0..4 {
        requests.push(h.request().await);
    }
    assert_eq!(h.hits.load(Ordering::SeqCst), 9);
    assert!(h.pending.try_recv().is_err());
    let request = requests.remove(0);
    h.reply(request, 30, 0).await;
    h.updated(b'a', 30).await;
    assert_eq!(ttl(&ask(h.port, b'e').await), 0);
    let _fifth = h.request().await;
    assert_eq!(h.hits.load(Ordering::SeqCst), 10);
    h.stop().await;
}
