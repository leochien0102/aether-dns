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

use config::{Config, Protocol, Tag, Upstream};
use server::{DEFAULT_MAX_TCP_CONNECTIONS, TcpAdmissionStats, cache::Clock};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};

const IO_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Duration {
        Duration::from_secs(10)
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

fn build_answer(query: &[u8], octet: u8) -> Vec<u8> {
    let mut wire = query.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire[7] = 1;
    wire.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 0, 113, octet]);
    wire
}

async fn write_tcp_frame(stream: &mut TcpStream, wire: &[u8]) {
    stream.write_u16(wire.len() as u16).await.unwrap();
    stream.write_all(wire).await.unwrap();
}

async fn read_tcp_frame_and_eof(stream: &mut TcpStream) -> Vec<u8> {
    timeout(IO_TIMEOUT, async {
        let len = stream.read_u16().await.unwrap() as usize;
        let mut wire = vec![0; len];
        stream.read_exact(&mut wire).await.unwrap();
        let mut extra = [0u8; 1];
        assert_eq!(stream.read(&mut extra).await.unwrap(), 0, "expected EOF after reply");
        wire
    })
    .await
    .expect("timed out reading TCP DNS response")
}

async fn expect_tcp_eof_without_reply(stream: &mut TcpStream) {
    let mut buf = [0u8; 16];
    let result = timeout(IO_TIMEOUT, stream.read(&mut buf))
        .await
        .expect("timed out waiting for immediate close on overload/EOF");
    match result {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("expected immediate EOF/reset without DNS reply, got {other:?}"),
    }
}

type UpstreamReq = (Vec<u8>, SocketAddr);

struct Harness {
    endpoints: Vec<SocketAddr>,
    upstream_socket: Arc<UdpSocket>,
    pending: mpsc::UnboundedReceiver<UpstreamReq>,
    stats_rx: watch::Receiver<TcpAdmissionStats>,
    shutdown: watch::Sender<bool>,
    serving: Option<JoinHandle<Result<(), String>>>,
    worker: JoinHandle<()>,
    base_admitted: usize,
}

impl Harness {
    async fn start(addresses: &[Ipv4Addr], port_count: usize, max_tcp_connections: usize) -> Self {
        let upstream_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let upstream_addr = upstream_socket.local_addr().unwrap();
        let (tx, pending) = mpsc::unbounded_channel();
        let worker = {
            let socket = upstream_socket.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
                    if tx.send((buf[..n].to_vec(), peer)).is_err() {
                        break;
                    }
                }
            })
        };

        let mut failures = Vec::new();
        let mut used_ports = Vec::new();
        for attempt in 1..=10 {
            let mut ports = Vec::new();
            let mut reservations = Vec::new();
            let mut ok = true;
            for _ in 0..port_count {
                let first = match TcpListener::bind((addresses[0], 0)).await {
                    Ok(l) => l,
                    Err(e) => {
                        failures.push(format!("attempt {attempt}: bind tcp: {e}"));
                        ok = false;
                        break;
                    }
                };
                let port = first.local_addr().unwrap().port();
                if used_ports.contains(&port) || ports.contains(&port) {
                    ok = false;
                    break;
                }
                let mut tcps = vec![first];
                let mut udps = Vec::new();
                for (idx, &addr) in addresses.iter().enumerate() {
                    if idx != 0 {
                        match TcpListener::bind((addr, port)).await {
                            Ok(l) => tcps.push(l),
                            Err(e) => {
                                failures.push(format!("attempt {attempt}: bind tcp {addr}:{port}: {e}"));
                                ok = false;
                                break;
                            }
                        }
                    }
                    match UdpSocket::bind((addr, port)).await {
                        Ok(u) => udps.push(u),
                        Err(e) => {
                            failures.push(format!("attempt {attempt}: bind udp {addr}:{port}: {e}"));
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    break;
                }
                ports.push(port);
                reservations.push((tcps, udps));
            }
            if !ok {
                continue;
            }
            used_ports.extend_from_slice(&ports);
            let endpoints: Vec<SocketAddr> = addresses
                .iter()
                .flat_map(|&addr| ports.iter().map(move |&port| SocketAddr::from((addr, port))))
                .collect();
            drop(reservations);

            let config = Config {
                bind_addr: addresses.to_vec(),
                bind_port: ports,
                default_tag: Some(Tag::Gfw),
                trust_dns: vec![Upstream {
                    protocol: Some(Protocol::Udp),
                    host: None,
                    ip: upstream_addr.ip(),
                    port: Some(upstream_addr.port()),
                }],
                ..Config::default()
            };
            let (shutdown, shutdown_rx) = watch::channel(false);
            let (stats_tx, mut stats_rx) = watch::channel(TcpAdmissionStats::default());
            let mut serving = tokio::spawn(async move {
                server::run_with_tcp_limit(
                    config,
                    shutdown_rx,
                    Arc::new(NoopIpTest),
                    Arc::new(NoopIpWriter),
                    Arc::new(FixedClock),
                    max_tcp_connections,
                    Some(stats_tx),
                )
                .await
                .map_err(|e| e.to_string())
            });

            // Probe the last TCP endpoint so we know all listeners are bound and live.
            // Wait for the probe task to both admit and release its permit (active == 0)
            // before returning so tests start from a clean idle quota.
            let last_endpoint = *endpoints.last().unwrap();
            let ready = timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        biased;
                        result = &mut serving => return Err(format!("startup exited: {result:?}")),
                        connected = TcpStream::connect(last_endpoint) => {
                            if let Ok(stream) = connected {
                                drop(stream);
                                // Wait until the server's accept loop admits and completes the probe.
                                stats_rx
                                    .wait_for(|s| s.admitted >= 1 && s.active == 0)
                                    .await
                                    .map_err(|e| format!("stats closed: {e}"))?;
                                if serving.is_finished() {
                                    return Err(format!("startup exited: {:?}", (&mut serving).await));
                                }
                                return Ok(());
                            }
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;

            match ready {
                Ok(Ok(())) => {
                    let base_admitted = stats_rx.borrow().admitted;
                    return Self {
                        endpoints,
                        upstream_socket,
                        pending,
                        stats_rx,
                        shutdown,
                        serving: Some(serving),
                        worker,
                        base_admitted,
                    };
                }
                Ok(Err(err)) => failures.push(err),
                Err(err) => {
                    serving.abort();
                    let _ = serving.await;
                    failures.push(format!("startup timeout: {err}"));
                }
            }
        }
        panic!("in-process multi-listener startup failed: {failures:?}");
    }

    async fn wait_stats(&mut self, mut predicate: impl FnMut(&TcpAdmissionStats) -> bool) -> TcpAdmissionStats {
        *timeout(IO_TIMEOUT, self.stats_rx.wait_for(|s| predicate(s)))
            .await
            .expect("timed out waiting for TcpAdmissionStats condition")
            .unwrap()
    }

    async fn recv_upstream(&mut self) -> UpstreamReq {
        timeout(IO_TIMEOUT, self.pending.recv())
            .await
            .expect("timed out waiting for upstream query")
            .expect("upstream worker closed")
    }

    async fn reply_upstream(&self, req: &UpstreamReq, octet: u8) {
        let reply = build_answer(&req.0, octet);
        self.upstream_socket.send_to(&reply, req.1).await.unwrap();
    }

    fn assert_no_upstream_pending(&mut self) {
        assert!(
            self.pending.try_recv().is_err(),
            "unexpected extra query reached upstream"
        );
    }

    async fn stop(mut self) -> (Duration, Result<(), String>) {
        let start = Instant::now();
        self.shutdown.send_replace(true);
        let result = timeout(Duration::from_secs(2), self.serving.take().unwrap())
            .await
            .expect("shutdown timed out")
            .unwrap();
        let elapsed = start.elapsed();
        self.worker.abort();
        (elapsed, result)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.shutdown.send_replace(true);
        if let Some(serving) = self.serving.take() {
            serving.abort();
        }
        self.worker.abort();
    }
}

#[test]
fn default_tcp_limit_is_bounded_and_reasonable() {
    assert_eq!(DEFAULT_MAX_TCP_CONNECTIONS, 128);
}

#[tokio::test(flavor = "current_thread")]
async fn shared_quota_across_listeners_and_no_idle_preallocation() {
    // 4 listeners (2 addresses x 2 ports) sharing a small quota of 2 (< listener count).
    let mut h = Harness::start(
        &[Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 0, 0, 2)],
        2,
        2,
    )
    .await;
    assert_eq!(h.endpoints.len(), 4);

    // 1. Idle listeners must not pre-occupy permits: a single listener can use all 2 permits
    //    while the other 3 listeners are idle.
    let mut c1 = TcpStream::connect(h.endpoints[0]).await.unwrap();
    let mut c2 = TcpStream::connect(h.endpoints[0]).await.unwrap();
    write_tcp_frame(&mut c1, &build_query(0x1001, "idle1")).await;
    write_tcp_frame(&mut c2, &build_query(0x1002, "idle2")).await;

    let u1 = h.recv_upstream().await;
    let u2 = h.recv_upstream().await;
    let stats = h.wait_stats(|s| s.active == 2).await;
    assert_eq!(stats.admitted - h.base_admitted, 2);
    assert_eq!(stats.peak_active, 2);
    assert_eq!(stats.rejected, 0);

    h.reply_upstream(&u1, 1).await;
    h.reply_upstream(&u2, 2).await;
    let r1 = read_tcp_frame_and_eof(&mut c1).await;
    let r2 = read_tcp_frame_and_eof(&mut c2).await;
    assert_eq!(&r1[..2], &0x1001u16.to_be_bytes());
    assert_eq!(&r2[..2], &0x1002u16.to_be_bytes());
    let stats = h.wait_stats(|s| s.active == 0).await;
    assert_eq!(stats.rejected, 0);

    // 2. Cross-listener sharing: occupy 1 slot on listener[0] and 1 slot on listener[1].
    let mut a1 = TcpStream::connect(h.endpoints[0]).await.unwrap();
    let mut a2 = TcpStream::connect(h.endpoints[1]).await.unwrap();
    write_tcp_frame(&mut a1, &build_query(0x2001, "cross1")).await;
    write_tcp_frame(&mut a2, &build_query(0x2002, "cross2")).await;

    let u_first = h.recv_upstream().await;
    let u_second = h.recv_upstream().await;
    let (ua1, ua2) = if u_first.0.windows(6).any(|w| w == b"cross1") {
        (u_first, u_second)
    } else {
        (u_second, u_first)
    };
    let base_admitted_before_overload = h.wait_stats(|s| s.active == 2).await.admitted;

    // At capacity (2/2): extra connections on another listener (listener[2]) and an already-busy
    // listener (listener[0]) succeed at kernel TCP connect(), but the application must NOT spawn
    // tasks for them and must shed them immediately with EOF.
    let mut over1 = TcpStream::connect(h.endpoints[2]).await.unwrap();
    let mut over2 = TcpStream::connect(h.endpoints[0]).await.unwrap();
    let stats = h.wait_stats(|s| s.rejected == 2).await;
    assert_eq!(
        stats.admitted, base_admitted_before_overload,
        "overload must not spawn new connection tasks"
    );
    assert_eq!(stats.active, 2);
    assert_eq!(stats.peak_active, 2);
    expect_tcp_eof_without_reply(&mut over1).await;
    expect_tcp_eof_without_reply(&mut over2).await;
    h.assert_no_upstream_pending();

    // While TCP is at capacity, UDP queries must remain completely unblocked.
    let udp_client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_q = build_query(0x3001, "udp-ok");
    udp_client.send_to(&udp_q, h.endpoints[2]).await.unwrap();
    let u_udp = h.recv_upstream().await;
    h.reply_upstream(&u_udp, 33).await;
    let mut udp_buf = [0u8; 512];
    let n = timeout(IO_TIMEOUT, udp_client.recv(&mut udp_buf))
        .await
        .expect("UDP query blocked while TCP at capacity")
        .unwrap();
    assert_eq!(&udp_buf[..2], &0x3001u16.to_be_bytes());
    assert!(n > udp_q.len());

    // 3. Permit reuse: finish one active TCP query -> permit returns -> next normal TCP query
    //    on listener[3] succeeds.
    h.reply_upstream(&ua1, 11).await;
    let ra1 = read_tcp_frame_and_eof(&mut a1).await;
    assert_eq!(&ra1[..2], &0x2001u16.to_be_bytes());
    let stats = h.wait_stats(|s| s.active == 1).await;
    assert_eq!(stats.active, 1);

    let mut reuse = TcpStream::connect(h.endpoints[3]).await.unwrap();
    write_tcp_frame(&mut reuse, &build_query(0x4001, "reused")).await;
    let u_reuse = h.recv_upstream().await;
    let stats = h.wait_stats(|s| s.active == 2).await;
    assert_eq!(stats.admitted, base_admitted_before_overload + 1);
    assert_eq!(stats.peak_active, 2);
    assert_eq!(stats.rejected, 2);

    h.reply_upstream(&u_reuse, 44).await;
    let r_reuse = read_tcp_frame_and_eof(&mut reuse).await;
    assert_eq!(&r_reuse[..2], &0x4001u16.to_be_bytes());

    h.reply_upstream(&ua2, 22).await;
    let ra2 = read_tcp_frame_and_eof(&mut a2).await;
    assert_eq!(&ra2[..2], &0x2002u16.to_be_bytes());
    let final_stats = h.wait_stats(|s| s.active == 0).await;
    assert_eq!(final_stats.peak_active, 2);
    assert_eq!(final_stats.rejected, 2);
    println!(
        "shared_quota: listeners={} test_limit=2 prod_limit={} admitted={} peak_active={} rejected={}",
        h.endpoints.len(),
        DEFAULT_MAX_TCP_CONNECTIONS,
        final_stats.admitted - h.base_admitted,
        final_stats.peak_active,
        final_stats.rejected,
    );

    let (_, res) = h.stop().await;
    res.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn permits_released_on_eof_invalid_query_and_normal_exit() {
    let mut h = Harness::start(&[Ipv4Addr::LOCALHOST], 2, 1).await;
    let base = h.base_admitted;

    // 1. Immediate client EOF releases the single permit.
    let c_eof = TcpStream::connect(h.endpoints[0]).await.unwrap();
    drop(c_eof);
    let s1 = h
        .wait_stats(|s| s.admitted == base + 1 && s.active == 0)
        .await;
    assert_eq!(s1.rejected, 0);

    // 2. Invalid DNS query (truncated header) closes connection and releases the permit.
    let mut c_bad = TcpStream::connect(h.endpoints[1]).await.unwrap();
    write_tcp_frame(&mut c_bad, &[0x12, 0x34, 0x00, 0x00]).await;
    expect_tcp_eof_without_reply(&mut c_bad).await;
    let s2 = h
        .wait_stats(|s| s.admitted == base + 2 && s.active == 0)
        .await;
    assert_eq!(s2.rejected, 0);
    h.assert_no_upstream_pending();

    // 3. Normal query succeeds immediately after both error/EOF exits.
    let mut c_ok = TcpStream::connect(h.endpoints[0]).await.unwrap();
    write_tcp_frame(&mut c_ok, &build_query(0x5001, "after-err")).await;
    let req = h.recv_upstream().await;
    h.reply_upstream(&req, 55).await;
    let reply = read_tcp_frame_and_eof(&mut c_ok).await;
    assert_eq!(&reply[..2], &0x5001u16.to_be_bytes());
    let s3 = h
        .wait_stats(|s| s.admitted == base + 3 && s.active == 0)
        .await;
    assert_eq!(s3.peak_active, 1);
    assert_eq!(s3.rejected, 0);
    println!(
        "release_paths: test_limit=1 admitted={} peak_active={} rejected={}",
        s3.admitted - base,
        s3.peak_active,
        s3.rejected,
    );

    let (_, res) = h.stop().await;
    res.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_converges_within_budget_when_at_capacity() {
    let mut h = Harness::start(&[Ipv4Addr::LOCALHOST], 2, 2).await;

    // Fill slot 1 with an idle admitted TCP connection on listener[0].
    let mut idle = TcpStream::connect(h.endpoints[0]).await.unwrap();
    h.wait_stats(|s| s.active == 1).await;

    // Fill slot 2 with an unanswered in-flight TCP query on listener[1].
    let mut active = TcpStream::connect(h.endpoints[1]).await.unwrap();
    write_tcp_frame(&mut active, &build_query(0x6001, "drain")).await;
    let _held_upstream = h.recv_upstream().await;
    h.wait_stats(|s| s.active == 2).await;

    // Confirm saturation via a rejected overload connection.
    let mut over = TcpStream::connect(h.endpoints[0]).await.unwrap();
    let s_sat = h.wait_stats(|s| s.rejected == 1).await;
    assert_eq!(s_sat.active, 2);
    assert_eq!(s_sat.peak_active, 2);
    expect_tcp_eof_without_reply(&mut over).await;

    // Trigger shutdown while at capacity:
    // - idle connection must be preempted promptly (well before 5s frame timeout)
    // - unanswered in-flight query holds drain up to the existing 500ms global budget,
    //   then cancels cleanly and returns its permit.
    let mut stats_rx = h.stats_rx.clone();
    let (elapsed, res) = h.stop().await;
    res.unwrap();
    assert!(
        elapsed >= Duration::from_millis(450),
        "drain should respect existing 500ms budget for in-flight query, got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(900),
        "full TCP quota must not block shutdown convergence, got {elapsed:?}"
    );
    expect_tcp_eof_without_reply(&mut idle).await;
    expect_tcp_eof_without_reply(&mut active).await;
    let s_done = *timeout(IO_TIMEOUT, stats_rx.wait_for(|s| s.active == 0))
        .await
        .expect("all permits must be released on shutdown cancellation")
        .unwrap();
    assert_eq!(s_done.active, 0);
    assert_eq!(s_done.peak_active, 2);
    println!(
        "shutdown_at_capacity: drain={elapsed:?} active_after={} peak_active={} rejected={}",
        s_done.active, s_done.peak_active, s_done.rejected,
    );
}
