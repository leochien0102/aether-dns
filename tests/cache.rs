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
        let mut config = Config {
            cache: capacity,
            cache_stale: Some(stale),
            bind_port: vec![],
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
        let (port, shutdown, serving) = in_process::start(|port, receiver| {
            let mut config = config.clone();
            config.bind_port = vec![port];
            let (clock, ip, writer) = (clock.clone(), ip.clone(), writer.clone());
            async move {
                server::run_with_cache_clock(config, receiver, ip, writer, clock)
                    .await
                    .map_err(|e| e.to_string())
            }
        })
        .await;
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

fn unit_cache(stale: u64, ttl: u32) -> (server::cache::Cache, FakeClock, Vec<u8>) {
    let clock = FakeClock::default();
    let q = query(b'x', 1, 1, 42);
    let mut r = response(&q, Reply::default());
    // First RR's TTL determines the entry's refresh threshold.
    let at = q.len() + 6;
    r[at..at + 4].copy_from_slice(&ttl.to_be_bytes());
    dns::truncate_additional(&mut r).unwrap();
    let mut cache = server::cache::Cache::new(Some(8), Some(stale));
    assert!(cache.add(&q, &r, clock.now()));
    (cache, clock, q)
}

#[test]
fn refresh_threshold_and_inflight_deduplication() {
    let (mut cache, clock, q) = unit_cache(5, 12);
    clock.at(7999);
    assert!(!cache.begin_refresh(&q, clock.now()));
    clock.at(8000);
    assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), &[4, 22, 42]);
    assert!(cache.begin_refresh(&q, clock.now()));
    assert!(!cache.begin_refresh(&q, clock.now()));
}

#[test]
fn short_ttl_only_refreshes_when_stale() {
    let (mut cache, clock, q) = unit_cache(5, 9);
    clock.at(8999);
    assert!(!cache.begin_refresh(&q, clock.now()));
    clock.at(9000);
    assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), &[0, 0, 0]);
    assert!(cache.begin_refresh(&q, clock.now()));
}

#[test]
fn failed_refresh_retains_entry_and_cools_down_for_five_seconds() {
    let (mut cache, clock, q) = unit_cache(20, 10);
    clock.at(10000);
    assert!(cache.begin_refresh(&q, clock.now()));
    cache.finish_refresh(&q, None, clock.now());
    clock.at(14999);
    assert!(!cache.begin_refresh(&q, clock.now()));
    assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), &[0, 0, 0]);
    clock.at(15000);
    assert!(cache.begin_refresh(&q, clock.now()));
}

#[test]
fn expired_entries_miss_and_stale_zero_still_allows_refresh_ahead() {
    for stale in [0, 5] {
        let (mut cache, clock, q) = unit_cache(stale, 12);
        clock.at(8000);
        assert!(cache.begin_refresh(&q, clock.now()));
        clock.at((12 + stale) * 1000);
        assert!(cache.get(&q, clock.now()).is_none());
        assert!(!cache.begin_refresh(&q, clock.now()));
    }
}

#[test]
fn refresh_limit_is_four_and_released_after_failure() {
    let (mut cache, clock, _) = unit_cache(20, 10);
    let queries: Vec<_> = (b'a'..=b'e').map(|name| query(name, 1, 1, 42)).collect();
    for q in &queries {
        assert!(cache.add(q, &response(q, Reply::default()), clock.now()));
    }
    clock.at(10000);
    for q in &queries[..4] {
        assert!(cache.begin_refresh(q, clock.now()));
    }
    assert!(!cache.begin_refresh(&queries[4], clock.now()));
    cache.finish_refresh(&queries[0], None, clock.now());
    assert!(cache.begin_refresh(&queries[4], clock.now()));
}

#[test]
fn rejected_refresh_replies_never_overwrite_old_entry() {
    for mode in [
        Reply {
            tc: true,
            ..Reply::default()
        },
        Reply {
            empty: true,
            ..Reply::default()
        },
        Reply {
            code: 2,
            ..Reply::default()
        },
    ] {
        let (mut cache, clock, q) = unit_cache(20, 10);
        clock.at(10000);
        let before = cache.get(&q, clock.now()).unwrap();
        assert!(cache.begin_refresh(&q, clock.now()));
        cache.finish_refresh(&q, Some(&response(&q, mode)), clock.now());
        assert_eq!(cache.get(&q, clock.now()).unwrap(), before);
        assert!(!cache.begin_refresh(&q, clock.now()));
    }
}

fn reply_with_ttls(
    q: &[u8],
    answer_ttls: &[u32],
    authority_ttl: Option<u32>,
    opt_ttl: Option<u32>,
) -> Vec<u8> {
    let mut r = q.to_vec();
    r[2] = 0x81;
    r[3] = 0x80;
    r[7] = answer_ttls.len() as u8;
    for (i, &ttl) in answer_ttls.iter().enumerate() {
        rr(&mut r, 1, ttl, &[1, 2, 3, (i + 1) as u8]);
    }
    if let Some(ttl) = authority_ttl {
        r[9] = 1;
        rr(&mut r, 16, ttl, &[1, b'z']);
    }
    if let Some(pseudo_ttl) = opt_ttl {
        r[11] = 1;
        r.extend_from_slice(&[0, 0, 41, 2, 0]);
        r.extend_from_slice(&pseudo_ttl.to_be_bytes());
        r.extend_from_slice(&[0, 0]);
    }
    r
}

#[test]
fn zero_ttl_single_and_multi_answer_are_never_cached() {
    let q = query(b'x', 1, 1, 42);
    for answer_ttls in [&[0u32][..], &[30, 0][..], &[0, 30][..]] {
        let mut cache = server::cache::Cache::new(Some(8), Some(30));
        let r = reply_with_ttls(&q, answer_ttls, Some(50), None);
        assert!(!cache.add(&q, &r, Duration::ZERO));
        assert!(cache.get(&q, Duration::ZERO).is_none());
        assert!(!cache.begin_refresh(&q, Duration::ZERO));
    }

    // CNAME(TTL=0) + A(TTL=30): minimum answer TTL is 0, so must not be cached.
    let mut cname_zero = q.clone();
    cname_zero[2] = 0x81;
    cname_zero[3] = 0x80;
    cname_zero[7] = 2;
    rr(&mut cname_zero, 5, 0, &[1, b'y', 0]);
    rr(&mut cname_zero, 1, 30, &[1, 2, 3, 4]);
    let mut cache = server::cache::Cache::new(Some(8), Some(30));
    assert!(!cache.add(&q, &cname_zero, Duration::ZERO));
    assert!(cache.get(&q, Duration::ZERO).is_none());
    assert!(!cache.begin_refresh(&q, Duration::ZERO));
}

#[test]
fn opt_pseudo_ttl_zero_is_not_treated_as_answer_ttl() {
    let q = query(b'x', 1, 1, 42);
    let r = reply_with_ttls(&q, &[10], Some(0), Some(0));
    let mut cache = server::cache::Cache::new(Some(8), Some(5));
    assert!(cache.add(&q, &r, Duration::ZERO));
    assert_eq!(
        ttls(&cache.get(&q, Duration::from_secs(4)).unwrap()),
        &[6, 0]
    );
    assert!(!cache.begin_refresh(&q, Duration::from_secs(4)));
    assert_eq!(
        ttls(&cache.get(&q, Duration::from_secs(10)).unwrap()),
        &[0, 0]
    );
    assert!(cache.begin_refresh(&q, Duration::from_secs(10)));
}

#[test]
fn zero_ttl_refresh_evicts_existing_entry_while_invalid_or_timeout_preserves_it() {
    // Valid TTL=0 refresh (single answer and multi-answer min TTL=0) must evict old entry.
    for answer_ttls in [&[0u32][..], &[30, 0][..]] {
        let (mut cache, clock, q) = unit_cache(20, 10);
        clock.at(10000);
        assert!(cache.get(&q, clock.now()).is_some());
        assert!(cache.begin_refresh(&q, clock.now()));
        let r = reply_with_ttls(&q, answer_ttls, None, None);
        cache.finish_refresh(&q, Some(&r), clock.now());
        assert!(cache.get(&q, clock.now()).is_none());
        assert!(!cache.begin_refresh(&q, clock.now()));
    }

    // Direct add of valid TTL=0 over an existing entry must also evict it.
    let (mut cache, clock, q_existing) = unit_cache(20, 10);
    let zero_reply = reply_with_ttls(&q_existing, &[0], None, None);
    assert!(!cache.add(&q_existing, &zero_reply, clock.now()));
    assert!(cache.get(&q_existing, clock.now()).is_none());
    assert!(!cache.begin_refresh(&q_existing, clock.now()));

    // Timeout (None) and malformed wire bytes must preserve old entry and apply 5s cooldown.
    for invalid in [None, Some(&[0u8, 1, 2][..])] {
        let (mut cache, clock, q_inv) = unit_cache(20, 10);
        clock.at(10000);
        let before = cache.get(&q_inv, clock.now()).unwrap();
        assert!(cache.begin_refresh(&q_inv, clock.now()));
        cache.finish_refresh(&q_inv, invalid, clock.now());
        assert_eq!(cache.get(&q_inv, clock.now()).unwrap(), before);
        clock.at(14999);
        assert!(!cache.begin_refresh(&q_inv, clock.now()));
        clock.at(15000);
        assert!(cache.begin_refresh(&q_inv, clock.now()));
    }
}

#[test]
fn extreme_stale_window_lookup_and_refresh_boundaries_never_overflow() {
    for stale in [u64::MAX, u64::MAX - 5] {
        let (mut cache, _clock, q) = unit_cache(stale, 10);
        let t_stale_start = Duration::from_secs(10);
        assert_eq!(ttls(&cache.get(&q, t_stale_start).unwrap()), &[0, 0, 0]);
        assert!(cache.begin_refresh(&q, t_stale_start));
        cache.finish_refresh(&q, None, t_stale_start);

        // Even at Duration::MAX, elapsed (u64::MAX + 0.999999999s) - 10s < stale.
        assert_eq!(ttls(&cache.get(&q, Duration::MAX).unwrap()), &[0, 0, 0]);
        assert!(cache.begin_refresh(&q, Duration::MAX));
    }

    // Exact expiry boundary at u64::MAX seconds when ttl + stale == u64::MAX.
    let (mut cache, _clock, q) = unit_cache(u64::MAX - 10, 10);
    let just_before = Duration::new(u64::MAX - 1, 999_999_999);
    assert_eq!(ttls(&cache.get(&q, just_before).unwrap()), &[0, 0, 0]);
    assert!(cache.begin_refresh(&q, just_before));
    cache.finish_refresh(&q, None, just_before);

    let exact_end = Duration::from_secs(u64::MAX);
    assert!(cache.get(&q, exact_end).is_none());
    assert!(!cache.begin_refresh(&q, exact_end));
}

#[test]
fn retry_after_near_duration_max_never_overflows_and_keeps_cooldown() {
    let (mut cache, _clock, q) = unit_cache(u64::MAX, 10);
    let fail_at = Duration::MAX - Duration::from_secs(2);
    assert_eq!(ttls(&cache.get(&q, fail_at).unwrap()), &[0, 0, 0]);
    assert!(cache.begin_refresh(&q, fail_at));
    cache.finish_refresh(&q, None, fail_at);
    // Still within 5s cooldown at fail_at and at Duration::MAX (only 2s later).
    assert_eq!(ttls(&cache.get(&q, Duration::MAX).unwrap()), &[0, 0, 0]);
    assert!(!cache.begin_refresh(&q, fail_at));
    assert!(!cache.begin_refresh(&q, Duration::MAX));

    // Exact 5s boundary ending at Duration::MAX.
    let (mut cache, _clock, q) = unit_cache(u64::MAX, 10);
    let fail_at = Duration::MAX - Duration::from_secs(5);
    assert!(cache.begin_refresh(&q, fail_at));
    cache.finish_refresh(&q, None, fail_at);
    assert!(!cache.begin_refresh(&q, Duration::MAX - Duration::from_nanos(1)));
    assert!(cache.begin_refresh(&q, Duration::MAX));
}

#[tokio::test]
async fn hit_restores_id_and_normalization() {
    let h = standard(5).await;
    h.ask(&query(b'x', 1, 1, 42), false, 1, INITIAL).await;
    h.ask(&query(b'x', 1, 1, 99), false, 1, INITIAL).await;
    h.stop().await;
}
#[tokio::test]
async fn question_type_class_are_distinct_keys() {
    let h = Harness::new(Some(8), 5, Reply::default(), |_| {}).await;
    for (i, (name, kind, class)) in [(b'x', 1, 1), (b'y', 1, 1), (b'x', 28, 1), (b'x', 1, 3)]
        .into_iter()
        .enumerate()
    {
        h.ask(&query(name, kind, class, 42), false, i + 1, INITIAL)
            .await;
    }
    h.ask(&query(b'x', 1, 1, 99), false, 4, INITIAL).await;
    h.stop().await;
}
// T16.101: fresh answer hits, not just verdict reuse.
#[tokio::test]
async fn answer_case_fresh_hits_echo_each_question_without_upstream() {
    let h = standard(5).await;
    for i in 0..8 {
        h.clock.at(i * 500);
        let q = query(if i % 2 == 0 { b'x' } else { b'X' }, 1, 1, 100 + i as u16);
        let elapsed = (i / 2) as u32;
        let r = h
            .ask(
                &q,
                i % 2 != 0,
                1,
                &[10 - elapsed, 30 - elapsed, 50 - elapsed],
            )
            .await;
        assert_eq!(&r[12..q.len()], &q[12..]);
        assert_eq!(&r[q.len()..q.len() + 2], &[0xc0, 12]);
        assert_eq!(
            dns::inspect(&r).unwrap().answers,
            vec![
                ("1.2.3.4".parse::<IpAddr>().unwrap(), 10 - elapsed),
                ("2001:db8::2".parse().unwrap(), 30 - elapsed)
            ]
        );
    }
    h.stop().await;
}

fn named_question(name: &[u8], kind: u16, class: u16) -> Vec<u8> {
    let mut q = query(b'x', kind, class, 42);
    q.splice(12..15, name.iter().copied());
    q
}

#[test]
fn answer_case_alternating_echo_preserves_compression_and_ttls() {
    let q = named_question(b"\x03FoO\x03BaR\0", 1, 1);
    let variant = named_question(b"\x03fOo\x03bAr\0", 1, 1);
    let mut r = response(&q, Reply::default());
    dns::truncate_additional(&mut r).unwrap();
    let mut cache = server::cache::Cache::new(Some(1), Some(5));
    assert!(cache.add(&q, &r, Duration::ZERO));
    for i in 0..8 {
        let mut current = if i % 2 == 0 {
            q.clone()
        } else {
            variant.clone()
        };
        current[..2].copy_from_slice(&(100 + i as u16).to_be_bytes());
        let now = Duration::from_secs(i);
        let hit = cache
            .get(&current, now)
            .expect("case variant shares answer");
        let mut expected = r.clone();
        expected[..2].copy_from_slice(&current[..2]);
        expected[12..current.len()].copy_from_slice(&current[12..]);
        for (at, ttl) in [
            (q.len() + 6, 10 - i as u32),
            (q.len() + 22, 30 - i as u32),
            (q.len() + 50, 50 - i as u32),
        ] {
            expected[at..at + 4].copy_from_slice(&ttl.to_be_bytes());
        }
        assert_eq!(hit, expected, "only ID, Question and TTL may change");
        assert_eq!(
            ttls(&hit),
            vec![10 - i as u32, 30 - i as u32, 50 - i as u32]
        );
    }
}

#[test]
fn answer_case_keys_preserve_numeric_fields_non_ascii_and_label_boundaries() {
    for (a, b) in [
        (query(b'x', 0x41, 1, 42), query(b'X', 0x61, 1, 42)),
        (query(b'x', 1, 0x41, 42), query(b'X', 1, 0x61, 42)),
        (
            named_question(b"\x03\xc3\x89A\0", 1, 1),
            named_question(b"\x03\xc3\xa9a\0", 1, 1),
        ),
        (
            named_question(b"\x03A.B\0", 1, 1),
            named_question(b"\x01a\x01b\0", 1, 1),
        ),
        (
            named_question(b"\x03A\0B\0", 1, 1),
            named_question(b"\x01a\0", 1, 1),
        ),
    ] {
        let mut cache = server::cache::Cache::new(Some(2), Some(0));
        assert!(cache.add(&a, &response(&a, Reply::default()), Duration::ZERO));
        assert!(cache.get(&b, Duration::ZERO).is_none());
        assert!(cache.add(&b, &response(&b, Reply::default()), Duration::ZERO));
        assert!(cache.get(&a, Duration::ZERO).is_some());
        assert!(cache.get(&b, Duration::ZERO).is_some());
    }
    for name in [
        &b"\0"[..],
        &b"\x03\xc3\x89A\0"[..],
        &b"\x03A\0B\0"[..],
        &b"\x03A.B\0"[..],
    ] {
        let q = named_question(name, 1, 1);
        let mut variant = q.clone();
        variant[12..12 + name.len()].make_ascii_lowercase();
        let mut cache = server::cache::Cache::new(Some(1), Some(0));
        assert!(cache.add(&q, &response(&q, Reply::default()), Duration::ZERO));
        assert_eq!(
            &cache.get(&variant, Duration::ZERO).unwrap()[12..variant.len()],
            &variant[12..]
        );
    }
}

#[test]
fn answer_case_refresh_cooldown_completion_zero_ttl_and_expiry_share_key() {
    let (mut cache, clock, q) = unit_cache(20, 10);
    let variant = query(b'X', 1, 1, 99);
    clock.at(10000);
    let stale = cache.get(&variant, clock.now()).unwrap();
    assert_eq!(&stale[12..variant.len()], &variant[12..]);
    assert_eq!(ttls(&stale), &[0, 0, 0]);
    assert!(cache.begin_refresh(&variant, clock.now()));
    assert!(!cache.begin_refresh(&q, clock.now()));
    cache.finish_refresh(&q, None, clock.now());
    clock.at(14999);
    assert!(!cache.begin_refresh(&variant, clock.now()));
    clock.at(15000);
    assert!(cache.begin_refresh(&q, clock.now()));
    cache.finish_refresh(
        &variant,
        Some(&response(&variant, Reply::default())),
        clock.now(),
    );
    assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), INITIAL);
    clock.at(25000);
    assert!(cache.begin_refresh(&q, clock.now()));
    let mut zero = response(&variant, Reply::default());
    zero[variant.len() + 6..variant.len() + 10].fill(0);
    cache.finish_refresh(&variant, Some(&zero), clock.now());
    assert!(cache.get(&q, clock.now()).is_none());
    assert!(!cache.begin_refresh(&variant, clock.now()));
    assert!(cache.add(&q, &response(&q, Reply::default()), clock.now()));
    assert!(!cache.add(&variant, &zero, clock.now()));
    assert!(cache.get(&q, clock.now()).is_none());
    assert!(cache.add(&q, &response(&q, Reply::default()), clock.now()));
    clock.at(55000);
    assert!(cache.get(&variant, clock.now()).is_none());
    assert!(cache.get(&q, clock.now()).is_none());
}

#[test]
fn answer_case_lru_replacement_does_not_consume_extra_capacity() {
    let mut cache = server::cache::Cache::new(Some(2), Some(0));
    let a = query(b'a', 1, 1, 42);
    let upper = query(b'A', 1, 1, 99);
    let b = query(b'b', 1, 1, 42);
    let c = query(b'c', 1, 1, 42);
    for q in [&a, &b, &upper] {
        assert!(cache.add(q, &response(q, Reply::default()), Duration::ZERO));
    }
    assert!(cache.get(&b, Duration::ZERO).is_some());
    assert!(cache.get(&a, Duration::ZERO).is_some());
    assert!(cache.add(&c, &response(&c, Reply::default()), Duration::ZERO));
    assert!(cache.get(&b, Duration::ZERO).is_none());
    assert!(cache.get(&upper, Duration::ZERO).is_some());
    assert!(cache.get(&c, Duration::ZERO).is_some());
}

#[test]
fn answer_case_rejects_unsafe_question_layouts() {
    let q = query(b'x', 1, 1, 42);
    let mut cache = server::cache::Cache::new(Some(1), Some(0));
    assert!(!cache.add(
        &q,
        &response(&query(b'y', 1, 1, 42), Reply::default()),
        Duration::ZERO
    ));
    let root = named_question(b"\0", 1, 1);
    let mut compressed = response(&root, Reply::default());
    // A backward pointer to a zero header byte decodes as root, but changes layout.
    compressed.splice(12..13, [0xc0, 4]);
    assert!(dns::inspect(&compressed).is_ok());
    assert!(!cache.add(&root, &compressed, Duration::ZERO));
    assert!(!cache.add(&compressed, &compressed, Duration::ZERO));
    for cut in 0..q.len() {
        assert!(cache.get(&q[..cut], Duration::ZERO).is_none());
        assert!(!cache.begin_refresh(&q[..cut], Duration::ZERO));
        assert!(!cache.add(&q[..cut], &response(&q, Reply::default()), Duration::ZERO));
    }
    assert!(cache.get(&root, Duration::ZERO).is_none());
}

#[tokio::test]
async fn ttl_decreases_from_insertion_not_last_hit() {
    let h = standard(5).await;
    let q = query(b'x', 1, 1, 42);
    h.ask(&q, false, 1, INITIAL).await;
    h.clock.at(2000);
    h.ask(&q, false, 1, &[8, 28, 48]).await;
    h.clock.at(6000);
    h.ask(&q, false, 1, &[4, 24, 44]).await;
    h.stop().await;
}
#[test]
fn stale_starts_at_ttl_and_zeros_every_rr() {
    let (mut cache, clock, q) = unit_cache(5, 10);
    clock.at(9999);
    assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), &[1, 21, 41]);
    for millis in [10000, 14999] {
        clock.at(millis);
        assert_eq!(ttls(&cache.get(&q, clock.now()).unwrap()), &[0, 0, 0]);
    }
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
    h.clock.at(6000);
    h.ask(&q, false, 1, &[4, 24, 44]).await;
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
        h.clock.at(2000);
        h.ask(&q, false, 1, &[8, 28, 48]).await;
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
