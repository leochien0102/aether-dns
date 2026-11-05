#![allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/dns/mod.rs"]
mod dns;
#[path = "../src/lists.rs"]
mod lists;
#[path = "../src/server.rs"]
mod server;
#[path = "../src/upstream.rs"]
mod upstream;
#[path = "../src/upstream_group.rs"]
mod upstream_group;

use config::{Config, SetName, SetPair, Tag, Upstream};
use server::cache::Clock;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::watch,
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
#[derive(Default)]
struct IpTest(AtomicUsize);
impl server::ip_test::IpTest for IpTest {
    fn is_china(&self, _: IpAddr) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        true
    }
}
#[derive(Default)]
struct Writer(Mutex<Vec<(String, Vec<Ipv4Addr>)>>);
impl server::ip_writer::IpWriter for Writer {
    fn add(&self, set: &SetName, ips: &[Ipv4Addr]) -> std::io::Result<()> {
        self.0.lock().unwrap().push((set.set.clone(), ips.to_vec()));
        Ok(())
    }
}
fn query(name: u8, kind: u16, class: u16, id: u16) -> Vec<u8> {
    let mut q = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, name, 0];
    q[..2].copy_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&kind.to_be_bytes());
    q.extend_from_slice(&class.to_be_bytes());
    q
}
fn rr(w: &mut Vec<u8>, kind: u16, ttl: u32, data: &[u8]) {
    w.extend_from_slice(&[0xc0, 12]);
    w.extend_from_slice(&kind.to_be_bytes());
    w.extend_from_slice(&[0, 1]);
    w.extend_from_slice(&ttl.to_be_bytes());
    w.extend_from_slice(&(data.len() as u16).to_be_bytes());
    w.extend_from_slice(data);
}
#[derive(Clone, Copy, Default)]
struct Reply {
    code: u8,
    tc: bool,
    empty: bool,
    cname: bool,
    aaaa_only: bool,
    no_address: bool,
}
fn response(q: &[u8], mode: Reply) -> Vec<u8> {
    let mut r = q.to_vec();
    r[2] = 0x81 | if mode.tc { 2 } else { 0 };
    r[3] = mode.code;
    if !mode.empty {
        r[7] = 2;
        if mode.no_address {
            rr(&mut r, 16, 10, &[1, b'z']);
        } else if mode.cname {
            rr(&mut r, 5, 10, &[1, b'y', 0]);
        } else if mode.aaaa_only {
            rr(
                &mut r,
                28,
                10,
                &[0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            );
        } else {
            rr(&mut r, 1, 10, &[1, 2, 3, 4]);
        }
        if mode.no_address {
            rr(&mut r, 5, 30, &[1, b'y', 0]);
        } else {
            rr(
                &mut r,
                28,
                30,
                &[0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            );
        }
        r[9] = 1;
        rr(&mut r, 16, 50, &[1, b'z']);
    }
    r[11] = 1;
    // OPT pseudo-TTL and additional must be removed by normalization.
    r.extend_from_slice(&[0, 0, 41, 2, 0, 0, 0, 0x80, 0, 0, 0]);
    r
}
fn ttls(w: &[u8]) -> Vec<u32> {
    use hickory_proto::op::Message;
    let m = Message::from_vec(w).unwrap();
    m.answers()
        .iter()
        .chain(m.name_servers())
        .map(|r| r.ttl())
        .collect()
}

struct Harness {
    port: u16,
    hits: Arc<AtomicUsize>,
    clock: Arc<FakeClock>,
    ip: Arc<IpTest>,
    writer: Arc<Writer>,
    shutdown: watch::Sender<bool>,
    serving: JoinHandle<Result<(), String>>,
    worker: JoinHandle<()>,
}
impl Harness {
    async fn new(
        capacity: Option<u64>,
        stale: u64,
        mode: Reply,
        customize: impl FnOnce(&mut Config),
    ) -> Self {
        // UDP and TCP have separate port namespaces; bind both before use and
        // retry when another process owns the TCP side of the chosen port.
        let (udp, tcp) = loop {
            let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let address = udp.local_addr().unwrap();
            match TcpListener::bind(address).await {
                Ok(tcp) => break (udp, tcp),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(error) => panic!("TCP bind: {error}"),
            }
        };
        let address = udp.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let worker = tokio::spawn(async move {
            let mut buf = [0; 4096];
            loop {
                tokio::select! {
                    received = udp.recv_from(&mut buf) => {
                        let (n, peer) = received.unwrap();
                        counter.fetch_add(1, Ordering::SeqCst);
                        udp.send_to(&response(&buf[..n], mode), peer).await.unwrap();
                    }
                    accepted = tcp.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let mut q = vec![0; stream.read_u16().await.unwrap() as usize];
                        stream.read_exact(&mut q).await.unwrap();
                        counter.fetch_add(1, Ordering::SeqCst);
                        let r = response(&q, mode);
                        stream.write_u16(r.len() as u16).await.unwrap();
                        stream.write_all(&r).await.unwrap();
                    }
                }
            }
        });
        let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let mut config = Config {
            cache: capacity,
            cache_stale: Some(stale),
            bind_port: vec![port],
            default_tag: Some(Tag::Gfw),
            trust_dns: vec![Upstream {
                protocol: None,
                host: None,
                ip: address.ip(),
                port: Some(address.port()),
            }],
            add_taggfw_ip: Some(SetPair {
                v4: Some(SetName {
                    family: "inet".into(),
                    table: "test".into(),
                    set: "gfw4".into(),
                }),
                v6: None,
            }),
            ..Config::default()
        };
        customize(&mut config);
        let clock = Arc::new(FakeClock::default());
        let ip = Arc::new(IpTest::default());
        let writer = Arc::new(Writer::default());
        let (shutdown, receiver) = watch::channel(false);
        let serving = {
            let (clock, ip, writer) = (clock.clone(), ip.clone(), writer.clone());
            tokio::spawn(async move {
                server::run_with_cache_clock(config, receiver, ip, writer, clock)
                    .await
                    .map_err(|e| e.to_string())
            })
        };
        timeout(Duration::from_secs(2), async {
            loop {
                if TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Self {
            port,
            hits,
            clock,
            ip,
            writer,
            shutdown,
            serving,
            worker,
        }
    }
    async fn ask(
        &self,
        q: &[u8],
        tcp: bool,
        expected_hits: usize,
        expected_ttls: &[u32],
    ) -> Vec<u8> {
        let r = timeout(Duration::from_secs(2), async {
            if tcp {
                let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port))
                    .await
                    .unwrap();
                s.write_u16(q.len() as u16).await.unwrap();
                s.write_all(q).await.unwrap();
                let mut r = vec![0; s.read_u16().await.unwrap() as usize];
                s.read_exact(&mut r).await.unwrap();
                r
            } else {
                let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                s.send_to(q, (Ipv4Addr::LOCALHOST, self.port))
                    .await
                    .unwrap();
                let mut r = vec![0; 4096];
                let n = s.recv(&mut r).await.unwrap();
                r.truncate(n);
                r
            }
        })
        .await
        .unwrap();
        assert_eq!(&r[..2], &q[..2]);
        assert_ne!(r[3] & 0x80, 0);
        assert_eq!(self.hits.load(Ordering::SeqCst), expected_hits);
        assert_eq!(ttls(&r), expected_ttls);
        if r[3] & 15 == 0 && r[2] & 2 == 0 {
            assert_eq!(&r[10..12], &[0, 0]);
        }
        eprintln!(
            "clock={}ms upstream={expected_hits} TTL={expected_ttls:?} protocol={}",
            self.clock.0.load(Ordering::SeqCst),
            if tcp { "TCP" } else { "UDP" }
        );
        r
    }
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        self.serving.await.unwrap().unwrap();
        self.worker.abort();
    }
}
async fn standard(stale: u64) -> Harness {
    Harness::new(Some(2), stale, Reply::default(), |_| {}).await
}
const INITIAL: &[u32] = &[10, 30, 50];

#[tokio::test]
async fn hit_restores_id_and_normalization() {
    let h = standard(5).await;
    h.ask(&query(b'x', 1, 1, 42), false, 1, INITIAL).await;
    h.ask(&query(b'x', 1, 1, 99), false, 1, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn raw_question_name_type_class_are_distinct_keys() {
    let h = Harness::new(Some(8), 5, Reply::default(), |_| {}).await;
    for (i, (name, kind, class)) in [(b'x', 1, 1), (b'X', 1, 1), (b'x', 28, 1), (b'x', 1, 3)]
        .into_iter()
        .enumerate()
    {
        h.ask(&query(name, kind, class, 42), false, i + 1, INITIAL)
            .await;
    }
    h.ask(&query(b'x', 1, 1, 99), false, 4, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn ttl_decreases_from_insertion_not_last_hit() {
    let h = standard(5).await;
    let q = query(b'x', 1, 1, 42);
    h.ask(&q, false, 1, INITIAL).await;
    h.clock.at(2000);
    h.ask(&q, false, 1, &[8, 28, 48]).await;
    h.clock.at(9999);
    h.ask(&q, false, 1, &[1, 21, 41]).await;
    h.stop().await;
}
#[tokio::test]
async fn stale_starts_at_ttl_and_zeros_every_rr() {
    let h = standard(5).await;
    let q = query(b'x', 1, 1, 42);
    h.ask(&q, false, 1, INITIAL).await;
    h.clock.at(10000);
    h.ask(&q, false, 1, &[0, 0, 0]).await;
    h.clock.at(14999);
    h.ask(&q, false, 1, &[0, 0, 0]).await;
    h.stop().await;
}
#[tokio::test]
async fn exact_stale_end_is_miss_and_reinsert() {
    let h = standard(5).await;
    let q = query(b'x', 1, 1, 42);
    h.ask(&q, false, 1, INITIAL).await;
    h.clock.at(15000);
    h.ask(&q, false, 2, INITIAL).await;
    h.clock.at(31000);
    h.ask(&q, false, 3, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn stale_zero_expires_at_exact_ttl() {
    let h = standard(0).await;
    let q = query(b'x', 1, 1, 42);
    h.ask(&q, false, 1, INITIAL).await;
    h.clock.at(9999);
    h.ask(&q, false, 1, &[1, 21, 41]).await;
    h.clock.at(10000);
    h.ask(&q, false, 2, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn capacity_evicts_least_recently_used() {
    let h = standard(5).await;
    for (name, hits) in [
        (b'a', 1),
        (b'b', 2),
        (b'a', 2),
        (b'c', 3),
        (b'a', 3),
        (b'b', 4),
    ] {
        h.ask(&query(name, 1, 1, 42), false, hits, INITIAL).await;
    }
    h.stop().await;
}
#[tokio::test]
async fn absent_and_zero_capacity_disable_cache() {
    for capacity in [None, Some(0)] {
        let h = Harness::new(capacity, 5, Reply::default(), |_| {}).await;
        let q = query(b'x', 1, 1, 42);
        h.ask(&q, false, 1, INITIAL).await;
        h.ask(&q, false, 2, INITIAL).await;
        h.stop().await;
    }
}
#[tokio::test]
async fn servfail_empty_nxdomain_nodata_and_tc_are_not_cached() {
    for mode in [
        Reply {
            code: 2,
            ..Reply::default()
        },
        Reply {
            empty: true,
            ..Reply::default()
        },
        Reply {
            code: 3,
            empty: true,
            ..Reply::default()
        },
        Reply {
            tc: true,
            ..Reply::default()
        },
        Reply {
            no_address: true,
            ..Reply::default()
        },
    ] {
        let h = Harness::new(Some(2), 5, mode, |_| {}).await;
        let q = query(b'x', 1, 1, 42);
        let ttl = if mode.empty { &[][..] } else { INITIAL };
        h.ask(&q, false, 1, ttl).await;
        h.ask(&q, false, 2, ttl).await;
        h.stop().await;
    }
}
#[tokio::test]
async fn hit_writes_gfw_and_group_once_per_reply() {
    for group in [false, true] {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("target/cache-group-{}.txt", std::process::id()));
        std::fs::write(&path, "x\n").unwrap();
        let h = Harness::new(Some(2), 5, Reply::default(), |c| {
            if group {
                let mut pair = c.add_taggfw_ip.clone().unwrap();
                pair.v4.as_mut().unwrap().set = "group4".into();
                c.groups = vec![config::Group {
                    name: "test".into(),
                    dnl: vec![path.to_string_lossy().into_owned()],
                    upstream: c.trust_dns.clone(),
                    ipset: Some(pair),
                }];
            }
        })
        .await;
        let q = query(b'x', 1, 1, 42);
        h.ask(&q, false, 1, INITIAL).await;
        assert_eq!(h.writer.0.lock().unwrap().len(), 1);
        h.ask(&q, false, 1, INITIAL).await;
        let writes = h.writer.0.lock().unwrap().clone();
        assert_eq!(writes.len(), 2);
        assert!(
            writes
                .iter()
                .all(|(name, ips)| name == if group { "group4" } else { "gfw4" }
                    && ips == &[Ipv4Addr::new(1, 2, 3, 4)])
        );
        assert_eq!(h.ip.0.load(Ordering::SeqCst), 0);
        h.stop().await;
        std::fs::remove_file(path).unwrap();
    }
}
#[tokio::test]
async fn udp_and_tcp_share_cache_both_directions() {
    let h = standard(5).await;
    h.ask(&query(b'x', 1, 1, 42), false, 1, INITIAL).await;
    h.clock.at(2000);
    h.ask(&query(b'x', 1, 1, 99), true, 1, &[8, 28, 48]).await;
    h.ask(&query(b'y', 1, 1, 42), true, 2, INITIAL).await;
    h.ask(&query(b'y', 1, 1, 99), false, 2, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn cname_minimum_and_aaaa_answers_are_cacheable() {
    for mode in [
        Reply {
            cname: true,
            ..Reply::default()
        },
        Reply {
            aaaa_only: true,
            ..Reply::default()
        },
    ] {
        let h = Harness::new(Some(2), 5, mode, |_| {}).await;
        let q = query(b'x', 28, 1, 42);
        h.ask(&q, false, 1, INITIAL).await;
        h.clock.at(10000);
        h.ask(&q, false, 1, &[0, 0, 0]).await;
        h.stop().await;
    }
}
#[tokio::test]
async fn none_cache_hit_does_not_repeat_ip_test() {
    let h = Harness::new(Some(2), 5, Reply::default(), |c| {
        c.default_tag = Some(Tag::None);
        c.china_dns = c.trust_dns.clone();
    })
    .await;
    let q = query(b'x', 1, 1, 42);
    // Both upstream groups are polled; china acceptance invokes membership once.
    h.ask(&q, false, 2, INITIAL).await;
    assert_eq!(h.ip.0.load(Ordering::SeqCst), 1);
    h.ask(&q, false, 2, INITIAL).await;
    assert_eq!(h.ip.0.load(Ordering::SeqCst), 1);
    assert!(h.writer.0.lock().unwrap().is_empty());
    h.stop().await;
}
#[tokio::test]
async fn filters_run_before_cache() {
    let h = Harness::new(Some(2), 5, Reply::default(), |c| {
        c.no_ipv6 = 1;
        c.filter_qtype.insert(16);
    })
    .await;
    h.ask(&query(b'x', 28, 1, 42), false, 0, &[]).await;
    h.ask(&query(b'x', 16, 1, 42), true, 0, &[]).await;
    h.ask(&query(b'x', 1, 1, 42), false, 1, INITIAL).await;
    h.ask(&query(b'x', 28, 1, 99), true, 1, &[]).await;
    h.ask(&query(b'x', 1, 1, 99), true, 1, INITIAL).await;
    h.stop().await;
}
