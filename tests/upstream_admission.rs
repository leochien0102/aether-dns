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
use server::{
    DEFAULT_MAX_TCP_CONNECTIONS, DEFAULT_MAX_UDP_QUERIES, DEFAULT_MAX_UPSTREAM_ATTEMPTS,
    UpstreamAdmissionStats, cache::Clock,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};
use upstream_group::UpstreamLimiter;

const IO_TIMEOUT: Duration = Duration::from_secs(3);

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
struct NoopIpTest;
impl server::ip_test::IpTest for NoopIpTest {
    fn is_china(&self, _: IpAddr) -> bool {
        false
    }
}

#[derive(Default)]
struct NoopIpWriter;
impl server::ip_writer::IpWriter for NoopIpWriter {
    fn add(&self, _: &config::SetName, _: &[Ipv4Addr]) -> std::io::Result<()> {
        Ok(())
    }
}

fn build_query(id: u16, label: &str) -> Vec<u8> {
    let mut wire = Vec::new();
    wire.extend_from_slice(&id.to_be_bytes());
    wire.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    wire.push(label.len() as u8);
    wire.extend_from_slice(label.as_bytes());
    wire.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
    wire
}

fn build_answer(query: &[u8], ttl: u32, octet: u8, rcode: u8) -> Vec<u8> {
    let mut wire = query.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80 | (rcode & 0x0f);
    if rcode == 0 {
        wire[7] = 1;
        wire.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
        wire.extend_from_slice(&ttl.to_be_bytes());
        wire.extend_from_slice(&[0, 4, 203, 0, 113, octet]);
    }
    wire
}

fn answer_ttl(wire: &[u8]) -> u32 {
    hickory_proto::op::Message::from_vec(wire)
        .unwrap()
        .answers()[0]
        .ttl()
}

fn answer_last_octet(wire: &[u8]) -> u8 {
    let msg = hickory_proto::op::Message::from_vec(wire).unwrap();
    let rdata = msg.answers()[0].data().as_a().unwrap();
    rdata.0.octets()[3]
}

async fn send_udp_query(endpoint: SocketAddr, query: Vec<u8>) -> Vec<u8> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(endpoint).await.unwrap();
    socket.send(&query).await.unwrap();
    let mut buf = [0u8; 4096];
    let n = timeout(IO_TIMEOUT, socket.recv(&mut buf))
        .await
        .expect("timed out waiting for UDP reply")
        .unwrap();
    buf[..n].to_vec()
}

async fn send_tcp_query(endpoint: SocketAddr, query: Vec<u8>) -> Vec<u8> {
    let mut stream = timeout(IO_TIMEOUT, TcpStream::connect(endpoint))
        .await
        .expect("timed out connecting TCP")
        .unwrap();
    stream.write_u16(query.len() as u16).await.unwrap();
    stream.write_all(&query).await.unwrap();
    timeout(IO_TIMEOUT, async {
        let len = stream.read_u16().await.unwrap() as usize;
        let mut wire = vec![0; len];
        stream.read_exact(&mut wire).await.unwrap();
        let mut extra = [0u8; 1];
        assert_eq!(
            stream.read(&mut extra).await.unwrap(),
            0,
            "expected EOF after framed TCP DNS response"
        );
        wire
    })
    .await
    .expect("timed out reading TCP reply")
}

struct GateReq {
    upstream_id: usize,
    wire: Vec<u8>,
    peer: SocketAddr,
}

struct GatedUdpPool {
    sockets: Vec<Arc<UdpSocket>>,
    upstreams: Vec<Upstream>,
    hits: Arc<AtomicUsize>,
    pending: mpsc::UnboundedReceiver<GateReq>,
    workers: Vec<JoinHandle<()>>,
}

impl GatedUdpPool {
    async fn new(count: usize) -> Self {
        let (tx, pending) = mpsc::unbounded_channel();
        let hits = Arc::new(AtomicUsize::new(0));
        let mut sockets = Vec::with_capacity(count);
        let mut upstreams = Vec::with_capacity(count);
        let mut workers = Vec::with_capacity(count);
        for id in 0..count {
            let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let addr = socket.local_addr().unwrap();
            upstreams.push(Upstream {
                protocol: Some(Protocol::Udp),
                host: None,
                ip: addr.ip(),
                port: Some(addr.port()),
            });
            let worker = {
                let socket = socket.clone();
                let tx = tx.clone();
                let hits = hits.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
                        hits.fetch_add(1, Ordering::SeqCst);
                        if tx
                            .send(GateReq {
                                upstream_id: id,
                                wire: buf[..n].to_vec(),
                                peer,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
            };
            sockets.push(socket);
            workers.push(worker);
        }
        Self {
            sockets,
            upstreams,
            hits,
            pending,
            workers,
        }
    }

    async fn recv_req(&mut self) -> GateReq {
        timeout(IO_TIMEOUT, self.pending.recv())
            .await
            .expect("timed out waiting for upstream request")
            .expect("upstream channel closed")
    }

    async fn reply(&self, req: &GateReq, ttl: u32, octet: u8, rcode: u8) {
        let wire = build_answer(&req.wire, ttl, octet, rcode);
        self.sockets[req.upstream_id]
            .send_to(&wire, req.peer)
            .await
            .unwrap();
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn abort(self) {
        for worker in self.workers {
            worker.abort();
        }
    }
}

async fn wait_stats(
    rx: &mut watch::Receiver<UpstreamAdmissionStats>,
    pred: impl Fn(UpstreamAdmissionStats) -> bool,
) -> UpstreamAdmissionStats {
    timeout(IO_TIMEOUT, rx.wait_for(|s| pred(*s)))
        .await
        .expect("timed out waiting for upstream admission stats")
        .unwrap()
        .to_owned()
}

#[test]
fn default_upstream_limit_is_bounded_and_independent() {
    assert_eq!(DEFAULT_MAX_UPSTREAM_ATTEMPTS, 256);
    assert_eq!(DEFAULT_MAX_UDP_QUERIES, 256);
    assert_eq!(DEFAULT_MAX_TCP_CONNECTIONS, 128);
}

#[tokio::test]
async fn shared_quota_across_groups_udp_tcp_and_dual_exchange() {
    let mut pool = GatedUdpPool::new(4).await;
    let config = Config {
        cache: Some(32),
        cache_stale: Some(30),
        default_tag: Some(Tag::None),
        china_dns: pool.upstreams[0..2].to_vec(),
        trust_dns: pool.upstreams[2..4].to_vec(),
        ..Config::default()
    };
    let clock = Arc::new(FakeClock::default());
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let clock = clock.clone();
        let stats_tx = stats_tx.clone();
        async move {
            server::run_with_upstream_limit(
                config,
                receiver,
                Arc::new(NoopIpTest),
                Arc::new(NoopIpWriter),
                clock,
                2,
                Some(stats_tx),
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;
    let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    // Query 1 (UDP, Tag::None dual exchange): 2 china + 2 trust = 4 attempts, quota = 2.
    let q1 = build_query(0x1101, "alpha");
    let c1 = tokio::spawn(send_udp_query(endpoint, q1.clone()));
    let r1 = pool.recv_req().await;
    let r2 = pool.recv_req().await;
    let s = wait_stats(&mut stats_rx, |s| s.active == 2 && s.rejected == 2).await;
    assert_eq!(s.admitted, 2);
    assert_eq!(s.peak_active, 2);
    assert_eq!(pool.hits(), 2, "rejected attempts must not reach upstreams");

    // While Query 1 holds 2/2 permits, Query 2 (TCP) must fail fast with SERVFAIL
    // without reaching any upstream or raising peak_active.
    let q2 = build_query(0x2202, "beta");
    let tcp_err = send_tcp_query(endpoint, q2.clone()).await;
    assert_eq!(&tcp_err[..2], &0x2202u16.to_be_bytes());
    assert_eq!(tcp_err[3] & 0x0f, 2, "TCP full-quota rejection must return SERVFAIL");
    let s = wait_stats(&mut stats_rx, |s| s.rejected == 6).await;
    assert_eq!(s.peak_active, 2);
    assert_eq!(pool.hits(), 2, "rejected TCP query must not reach upstreams");

    // Query 3 (UDP) while still at capacity must also fail fast with SERVFAIL.
    let q3 = build_query(0x3303, "gamma");
    let udp_err = send_udp_query(endpoint, q3).await;
    assert_eq!(&udp_err[..2], &0x3303u16.to_be_bytes());
    assert_eq!(udp_err[3] & 0x0f, 2, "UDP full-quota rejection must return SERVFAIL");
    let s = wait_stats(&mut stats_rx, |s| s.rejected == 10).await;
    assert_eq!(s.peak_active, 2);
    assert_eq!(pool.hits(), 2, "rejected UDP query must not reach upstreams");

    // Reply to one admitted attempt of Query 1; partial rejection must not mask
    // the admitted success, and the losing attempt must be cancelled and release its slot.
    pool.reply(&r1, 60, 11, 0).await;
    let rep1 = c1.await.unwrap();
    assert_eq!(&rep1[..2], &0x1101u16.to_be_bytes());
    assert_eq!(rep1[3] & 0x0f, 0, "admitted attempt must still succeed");
    assert_eq!(answer_last_octet(&rep1), 11);
    drop(r2);
    let s = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(s.peak_active, 2);

    // Re-issuing Query 2 (beta) over TCP must hit upstream (proving SERVFAIL was
    // not cached) and succeed now that permits were returned.
    let c2 = tokio::spawn(send_tcp_query(endpoint, q2.clone()));
    let b1 = pool.recv_req().await;
    let b2 = pool.recv_req().await;
    pool.reply(&b1, 60, 22, 0).await;
    let rep2 = c2.await.unwrap();
    assert_eq!(&rep2[..2], &0x2202u16.to_be_bytes());
    assert_eq!(rep2[3] & 0x0f, 0);
    assert_eq!(answer_last_octet(&rep2), 22);
    drop(b2);
    let final_stats = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(final_stats.peak_active, 2);
    assert_eq!(final_stats.admitted, 4);
    assert_eq!(final_stats.rejected, 12);

    println!(
        "shared_upstream_quota: test_limit=2 prod_limit={} admitted={} peak_active={} rejected={} upstream_hits={}",
        DEFAULT_MAX_UPSTREAM_ATTEMPTS,
        final_stats.admitted,
        final_stats.peak_active,
        final_stats.rejected,
        pool.hits(),
    );

    shutdown.send(true).unwrap();
    timeout(Duration::from_secs(1), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    pool.abort();
}

#[tokio::test]
async fn partial_admission_preserves_admitted_success_fallback_and_releases_on_error() {
    let mut pool = GatedUdpPool::new(2).await;
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let limiter = UpstreamLimiter::new(1, Some(stats_tx));
    let q = build_query(0x4404, "partial");

    // 1. Admitted U0 returns NOERROR while U1 is rejected -> Ok(NOERROR), U1 untouched.
    let task = tokio::spawn({
        let upstreams = pool.upstreams.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move { upstream_group::exchange_with_limit(&upstreams, &q, Protocol::Udp, &limiter).await }
    });
    let req = pool.recv_req().await;
    assert_eq!(req.upstream_id, 0);
    pool.reply(&req, 60, 33, 0).await;
    let ok = task.await.unwrap().unwrap();
    assert_eq!(ok[3] & 0x0f, 0);
    assert_eq!(answer_last_octet(&ok), 33);
    let s = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(s.admitted, 1);
    assert_eq!(s.peak_active, 1);
    assert_eq!(s.rejected, 1);
    assert_eq!(pool.hits(), 1);

    // 2. Admitted U0 returns DNS REFUSED (RCODE=5) while U1 is rejected -> fallback preserved.
    let task = tokio::spawn({
        let upstreams = pool.upstreams.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move { upstream_group::exchange_with_limit(&upstreams, &q, Protocol::Udp, &limiter).await }
    });
    let req = pool.recv_req().await;
    assert_eq!(req.upstream_id, 0);
    pool.reply(&req, 0, 0, 5).await;
    let refused = task.await.unwrap().unwrap();
    assert_eq!(refused[3] & 0x0f, 5, "admitted DNS error fallback must be preserved");
    let s = wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == 2).await;
    assert_eq!(s.peak_active, 1);
    assert_eq!(s.rejected, 2);
    assert_eq!(pool.hits(), 2);

    // 3. Admitted TCP upstream closes immediately (I/O error) while U1 is rejected ->
    //    returns I/O error (not WouldBlock) and releases permit for next exchange.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let close_addr = listener.local_addr().unwrap();
    let closer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    });
    let failing_group = vec![
        Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: close_addr.ip(),
            port: Some(close_addr.port()),
        },
        pool.upstreams[1].clone(),
    ];
    let err = upstream_group::exchange_with_limit(&failing_group, &q, Protocol::Tcp, &limiter)
        .await
        .unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::WouldBlock);
    closer.await.unwrap();
    let s = wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == 3).await;
    assert_eq!(s.peak_active, 1);
    assert_eq!(s.rejected, 3);
    assert_eq!(pool.hits(), 2, "rejected U1 must still have zero hits");

    // 4. Subsequent exchange acquires the released permit and succeeds.
    let task = tokio::spawn({
        let upstreams = vec![pool.upstreams[1].clone()];
        let q = q.clone();
        let limiter = limiter.clone();
        async move { upstream_group::exchange_with_limit(&upstreams, &q, Protocol::Udp, &limiter).await }
    });
    let req = pool.recv_req().await;
    assert_eq!(req.upstream_id, 1);
    pool.reply(&req, 60, 44, 0).await;
    let ok2 = task.await.unwrap().unwrap();
    assert_eq!(answer_last_octet(&ok2), 44);
    let final_stats = wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == 4).await;
    assert_eq!(final_stats.peak_active, 1);
    pool.abort();
}

#[tokio::test]
async fn cancellation_releases_permit_and_shutdown_converges_at_capacity() {
    // Part A: dropping an in-flight exchange_with_limit releases its permit immediately.
    let mut pool = GatedUdpPool::new(1).await;
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let limiter = UpstreamLimiter::new(1, Some(stats_tx));
    let q = build_query(0x5505, "cancel");
    let upstreams = pool.upstreams.clone();
    {
        let mut fut = Box::pin(upstream_group::exchange_with_limit(
            &upstreams,
            &q,
            Protocol::Udp,
            &limiter,
        ));
        tokio::select! {
            _ = &mut fut => panic!("silent upstream must stay pending"),
            req = pool.recv_req() => {
                assert_eq!(req.upstream_id, 0);
                let s = wait_stats(&mut stats_rx, |s| s.active == 1).await;
                assert_eq!(s.peak_active, 1);
            }
        }
        // `fut` is dropped here without completing.
    }
    let s = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(s.admitted, 1);

    // Follow-up exchange succeeds using the recovered permit.
    let task = tokio::spawn({
        let upstreams = pool.upstreams.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move { upstream_group::exchange_with_limit(&upstreams, &q, Protocol::Udp, &limiter).await }
    });
    let req = pool.recv_req().await;
    pool.reply(&req, 60, 55, 0).await;
    assert_eq!(answer_last_octet(&task.await.unwrap().unwrap()), 55);
    wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == 2).await;
    pool.abort();

    // Part B: server at full upstream capacity converges within 500 ms shutdown budget
    // and releases all permits on abort.
    let mut srv_pool = GatedUdpPool::new(2).await;
    let config = Config {
        default_tag: Some(Tag::Gfw),
        trust_dns: srv_pool.upstreams.clone(),
        ..Config::default()
    };
    let (srv_stats_tx, mut srv_stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let srv_stats_tx = srv_stats_tx.clone();
        async move {
            server::run_with_upstream_limit(
                config,
                receiver,
                Arc::new(NoopIpTest),
                Arc::new(NoopIpWriter),
                Arc::new(FakeClock::default()),
                2,
                Some(srv_stats_tx),
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;
    let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    // Occupy both upstream permits with a single 2-upstream query.
    let _holder = tokio::spawn(send_udp_query(endpoint, build_query(0x6601, "hold")));
    let _r1 = srv_pool.recv_req().await;
    let _r2 = srv_pool.recv_req().await;
    wait_stats(&mut srv_stats_rx, |s| s.active == 2).await;

    // Confirm overload rejection while at capacity.
    let rej = send_udp_query(endpoint, build_query(0x6602, "overflow")).await;
    assert_eq!(rej[3] & 0x0f, 2);
    wait_stats(&mut srv_stats_rx, |s| s.rejected == 2).await;

    let started = Instant::now();
    shutdown.send(true).unwrap();
    timeout(Duration::from_millis(850), serving)
        .await
        .expect("server must converge within 500 ms drain budget")
        .unwrap()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(800),
        "shutdown drain exceeded budget: {elapsed:?}"
    );
    let after = wait_stats(&mut srv_stats_rx, |s| s.active == 0).await;
    println!(
        "shutdown_upstream_at_capacity: drain={elapsed:?} active_after={} peak_active={} rejected={}",
        after.active, after.peak_active, after.rejected
    );
    srv_pool.abort();
}

#[tokio::test]
async fn background_refresh_shares_quota_and_cannot_bypass() {
    let mut pool = GatedUdpPool::new(1).await;
    let config = Config {
        cache: Some(32),
        cache_stale: Some(30),
        default_tag: Some(Tag::Gfw),
        trust_dns: pool.upstreams.clone(),
        ..Config::default()
    };
    let clock = Arc::new(FakeClock::default());
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let clock = clock.clone();
        let stats_tx = stats_tx.clone();
        async move {
            server::run_with_upstream_limit(
                config,
                receiver,
                Arc::new(NoopIpTest),
                Arc::new(NoopIpWriter),
                clock,
                1,
                Some(stats_tx),
            )
            .await
            .map_err(|e| e.to_string())
        }
    })
    .await;
    let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    // 1. Prime cache for "refresh" with TTL=12, IP=203.0.113.1 at t=0.
    let q_ref = build_query(0x7701, "refresh");
    let c = tokio::spawn(send_udp_query(endpoint, q_ref.clone()));
    let req = pool.recv_req().await;
    pool.reply(&req, 12, 1, 0).await;
    let rep = c.await.unwrap();
    assert_eq!(answer_ttl(&rep), 12);
    assert_eq!(answer_last_octet(&rep), 1);
    wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == 1).await;

    // 2. Move into refresh-ahead window (t=9s, remaining 3s <= 12/3=4s) and occupy the
    //    only upstream permit with an unrelated miss query "blocker".
    clock.at(9_000);
    let q_block = build_query(0x7702, "blocker");
    let blocker = tokio::spawn(send_udp_query(endpoint, q_block.clone()));
    let block_req = pool.recv_req().await;
    wait_stats(&mut stats_rx, |s| s.active == 1 && s.admitted == 2).await;
    assert_eq!(pool.hits(), 2);

    // 3. Query "refresh" while quota is full: client still gets cached hit immediately,
    //    but background refresh must be rejected without touching upstream.
    let cached_hit = send_udp_query(endpoint, q_ref.clone()).await;
    assert_eq!(cached_hit[3] & 0x0f, 0);
    assert_eq!(answer_ttl(&cached_hit), 3);
    assert_eq!(answer_last_octet(&cached_hit), 1);
    let s = wait_stats(&mut stats_rx, |s| s.rejected == 1).await;
    assert_eq!(s.peak_active, 1);
    assert_eq!(pool.hits(), 2, "rejected background refresh must not reach upstream");

    // Release the blocker so the single permit is free again.
    pool.reply(&block_req, 60, 9, 0).await;
    assert_eq!(answer_last_octet(&blocker.await.unwrap()), 9);
    wait_stats(&mut stats_rx, |s| s.active == 0).await;

    // 4. Advance past the 5s refresh failure cooldown (t=15s, stale hit with TTL=0).
    //    Now the background refresh acquires the freed permit and occupies 1/1.
    clock.at(15_000);
    let stale_hit = send_udp_query(endpoint, q_ref.clone()).await;
    assert_eq!(stale_hit[3] & 0x0f, 0);
    assert_eq!(answer_ttl(&stale_hit), 0);
    assert_eq!(answer_last_octet(&stale_hit), 1);
    let refresh_req = pool.recv_req().await;
    let s = wait_stats(&mut stats_rx, |s| s.active == 1 && s.admitted == 3).await;
    assert_eq!(s.peak_active, 1);
    assert_eq!(pool.hits(), 3);

    // 5. While the background refresh holds the 1/1 permit, a new miss query must be
    //    rejected (proving bidirectional quota sharing).
    let miss_during_refresh = send_udp_query(endpoint, build_query(0x7703, "during")).await;
    assert_eq!(miss_during_refresh[3] & 0x0f, 2);
    let s = wait_stats(&mut stats_rx, |s| s.rejected == 2).await;
    assert_eq!(s.peak_active, 1);
    assert_eq!(pool.hits(), 3);

    // 6. Complete the background refresh with IP=203.0.113.2, TTL=60 and verify cache updated.
    pool.reply(&refresh_req, 60, 2, 0).await;
    let final_stats = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(final_stats.peak_active, 1);
    let refreshed = send_udp_query(endpoint, q_ref).await;
    assert_eq!(answer_ttl(&refreshed), 60);
    assert_eq!(answer_last_octet(&refreshed), 2);

    println!(
        "refresh_shared_upstream_quota: test_limit=1 admitted={} peak_active={} rejected={} upstream_hits={}",
        final_stats.admitted,
        final_stats.peak_active,
        final_stats.rejected,
        pool.hits(),
    );

    shutdown.send(true).unwrap();
    timeout(Duration::from_secs(1), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    pool.abort();
}

#[tokio::test]
async fn dual_merge_preserves_actual_exchange_error_regardless_of_side() {
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let limiter = UpstreamLimiter::new(1, Some(stats_tx));
    let q = build_query(0x8801, "dualerr");

    // Two controlled TCP listeners whose accept loop can be released via a channel.
    let china_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let china_addr = china_listener.local_addr().unwrap();
    let trust_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let trust_addr = trust_listener.local_addr().unwrap();

    let china_upstream = Upstream {
        protocol: Some(Protocol::Tcp),
        host: None,
        ip: china_addr.ip(),
        port: Some(china_addr.port()),
    };
    let trust_upstream = Upstream {
        protocol: Some(Protocol::Tcp),
        host: None,
        ip: trust_addr.ip(),
        port: Some(trust_addr.port()),
    };

    // Case A: trust is admitted and fails with TCP EOF; china is rejected (WouldBlock).
    let (trust_accepted_tx, mut trust_accepted_rx) = mpsc::unbounded_channel();
    let (trust_close_tx, trust_close_rx) = tokio::sync::oneshot::channel();
    let trust_task = tokio::spawn({
        let trust_upstream = trust_upstream.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move {
            upstream_group::exchange_with_limit(&[trust_upstream], &q, Protocol::Tcp, &limiter)
                .await
        }
    });
    let trust_server = tokio::spawn(async move {
        let (stream, _) = trust_listener.accept().await.unwrap();
        trust_accepted_tx.send(()).unwrap();
        let _ = trust_close_rx.await;
        drop(stream);
        trust_listener
    });
    timeout(IO_TIMEOUT, trust_accepted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    wait_stats(&mut stats_rx, |s| s.active == 1).await;

    // While trust holds the only permit, china must be rejected without touching china_listener.
    let china_result =
        upstream_group::exchange_with_limit(&[china_upstream.clone()], &q, Protocol::Tcp, &limiter)
            .await;
    assert_eq!(
        china_result.as_ref().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    wait_stats(&mut stats_rx, |s| s.rejected == 1).await;

    // Close trust stream so trust fails with a real I/O error and releases its permit.
    trust_close_tx.send(()).unwrap();
    let trust_listener = trust_server.await.unwrap();
    let trust_result = trust_task.await.unwrap();
    assert_ne!(
        trust_result.as_ref().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let s = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(s.peak_active, 1);

    let merged_a = server::merge_dual_results(trust_result, china_result).unwrap_err();
    assert_ne!(
        merged_a.kind(),
        std::io::ErrorKind::WouldBlock,
        "china WouldBlock must not mask trust actual I/O error"
    );
    assert!(
        merged_a.to_string().contains(&format!("tcp://{trust_addr}")),
        "merged error must name admitted trust upstream: {merged_a}"
    );
    assert!(
        timeout(Duration::from_millis(20), china_listener.accept())
            .await
            .is_err(),
        "rejected china side must not have connected"
    );

    // Case B: china is admitted and fails with TCP EOF; trust is rejected (WouldBlock).
    let (china_accepted_tx, mut china_accepted_rx) = mpsc::unbounded_channel();
    let (china_close_tx, china_close_rx) = tokio::sync::oneshot::channel();
    let china_task = tokio::spawn({
        let china_upstream = china_upstream.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move {
            upstream_group::exchange_with_limit(&[china_upstream], &q, Protocol::Tcp, &limiter)
                .await
        }
    });
    let china_server = tokio::spawn(async move {
        let (stream, _) = china_listener.accept().await.unwrap();
        china_accepted_tx.send(()).unwrap();
        let _ = china_close_rx.await;
        drop(stream);
        china_listener
    });
    timeout(IO_TIMEOUT, china_accepted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    wait_stats(&mut stats_rx, |s| s.active == 1).await;

    let trust_result =
        upstream_group::exchange_with_limit(&[trust_upstream.clone()], &q, Protocol::Tcp, &limiter)
            .await;
    assert_eq!(
        trust_result.as_ref().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    wait_stats(&mut stats_rx, |s| s.rejected == 2).await;

    china_close_tx.send(()).unwrap();
    let _china_listener = china_server.await.unwrap();
    let china_result = china_task.await.unwrap();
    assert_ne!(
        china_result.as_ref().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let s = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(s.peak_active, 1);

    let merged_b = server::merge_dual_results(trust_result, china_result).unwrap_err();
    assert_ne!(
        merged_b.kind(),
        std::io::ErrorKind::WouldBlock,
        "trust WouldBlock must not mask china actual I/O error"
    );
    assert!(
        merged_b.to_string().contains(&format!("tcp://{china_addr}")),
        "merged error must name admitted china upstream: {merged_b}"
    );
    assert!(
        timeout(Duration::from_millis(20), trust_listener.accept())
            .await
            .is_err(),
        "rejected trust side must not have connected"
    );

    // Case C: both sides rejected -> remains WouldBlock (silent SERVFAIL path).
    let both_rejected = server::merge_dual_results(
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "upstream concurrency limit reached",
        )),
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "upstream concurrency limit reached",
        )),
    )
    .unwrap_err();
    assert_eq!(both_rejected.kind(), std::io::ErrorKind::WouldBlock);
}

#[tokio::test]
async fn none_exchange_preserves_actual_io_error_when_other_side_rejected() {
    let china_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let china_addr = china_listener.local_addr().unwrap();
    let trust_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let trust_addr = trust_listener.local_addr().unwrap();

    let china_hits = Arc::new(AtomicUsize::new(0));
    let trust_hits = Arc::new(AtomicUsize::new(0));
    let china_worker = {
        let hits = china_hits.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = china_listener.accept().await {
                hits.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        })
    };
    let trust_worker = {
        let hits = trust_hits.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = trust_listener.accept().await {
                hits.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        })
    };

    let config = Config {
        default_tag: Some(Tag::None),
        china_dns: vec![Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: china_addr.ip(),
            port: Some(china_addr.port()),
        }],
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: trust_addr.ip(),
            port: Some(trust_addr.port()),
        }],
        ..Config::default()
    };
    let (stats_tx, mut stats_rx) = watch::channel(UpstreamAdmissionStats::default());
    let limiter = UpstreamLimiter::new(1, Some(stats_tx));
    let q = build_query(0x8802, "dualboth");

    let mut seen_china_rejected = false;
    let mut seen_trust_rejected = false;
    let mut iterations = 0usize;

    for i in 0..64 {
        iterations = i + 1;
        let c_before = china_hits.load(Ordering::SeqCst);
        let t_before = trust_hits.load(Ordering::SeqCst);
        let err = server::exchange_once_with_limit(
            config.clone(),
            &q,
            Protocol::Tcp,
            limiter.clone(),
        )
        .await
        .unwrap_err();
        let s = wait_stats(&mut stats_rx, |s| s.active == 0 && s.admitted == iterations).await;
        assert_eq!(s.peak_active, 1);
        assert_eq!(s.rejected, iterations);
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::WouldBlock,
            "actual I/O error must not be masked by WouldBlock: {err}"
        );
        let c_delta = china_hits.load(Ordering::SeqCst) - c_before;
        let t_delta = trust_hits.load(Ordering::SeqCst) - t_before;
        match (c_delta, t_delta) {
            (0, 1) => {
                assert!(
                    err.to_string().contains(&format!("tcp://{trust_addr}")),
                    "expected trust error when china rejected, got: {err}"
                );
                seen_china_rejected = true;
            }
            (1, 0) => {
                assert!(
                    err.to_string().contains(&format!("tcp://{china_addr}")),
                    "expected china error when trust rejected, got: {err}"
                );
                seen_trust_rejected = true;
            }
            other => panic!("expected exactly one side admitted per exchange, got {other:?}"),
        }
        if seen_china_rejected && seen_trust_rejected {
            break;
        }
    }
    assert!(
        seen_china_rejected && seen_trust_rejected,
        "expected both china-rejected and trust-rejected directions within 64 iterations"
    );

    // Follow-up exchange on the same limiter succeeds after all permits were returned.
    let mut ok_pool = GatedUdpPool::new(1).await;
    let ok_task = tokio::spawn({
        let upstreams = ok_pool.upstreams.clone();
        let q = q.clone();
        let limiter = limiter.clone();
        async move {
            upstream_group::exchange_with_limit(&upstreams, &q, Protocol::Udp, &limiter).await
        }
    });
    let req = ok_pool.recv_req().await;
    ok_pool.reply(&req, 60, 77, 0).await;
    let reply = ok_task.await.unwrap().unwrap();
    assert_eq!(answer_last_octet(&reply), 77);
    let final_stats = wait_stats(&mut stats_rx, |s| s.active == 0).await;
    assert_eq!(final_stats.peak_active, 1);

    println!(
        "dual_exchange_error_preservation: iterations={} china_hits={} trust_hits={} peak_active={} active_after={}",
        iterations,
        china_hits.load(Ordering::SeqCst),
        trust_hits.load(Ordering::SeqCst),
        final_stats.peak_active,
        final_stats.active,
    );

    ok_pool.abort();
    china_worker.abort();
    trust_worker.abort();
}
