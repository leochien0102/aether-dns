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
use server::{
    DEFAULT_MAX_TCP_CONNECTIONS, DEFAULT_MAX_UDP_QUERIES, TcpAdmissionStats, UdpAdmissionStats,
    cache::Clock,
};
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

async fn recv_udp_reply(socket: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let n = timeout(IO_TIMEOUT, socket.recv(&mut buf))
        .await
        .expect("timed out waiting for UDP DNS response")
        .unwrap();
    buf[..n].to_vec()
}

type UpstreamReq = (Vec<u8>, SocketAddr);

struct Harness {
    endpoints: Vec<SocketAddr>,
    upstream_socket: Arc<UdpSocket>,
    pending: mpsc::UnboundedReceiver<UpstreamReq>,
    udp_stats_rx: watch::Receiver<UdpAdmissionStats>,
    tcp_stats_rx: watch::Receiver<TcpAdmissionStats>,
    shutdown: watch::Sender<bool>,
    serving: Option<JoinHandle<Result<(), String>>>,
    worker: JoinHandle<()>,
}

impl Harness {
    async fn start(addresses: &[Ipv4Addr], port_count: usize, max_udp_queries: usize) -> Self {
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
                                failures.push(format!(
                                    "attempt {attempt}: bind tcp {addr}:{port}: {e}"
                                ));
                                ok = false;
                                break;
                            }
                        }
                    }
                    match UdpSocket::bind((addr, port)).await {
                        Ok(u) => udps.push(u),
                        Err(e) => {
                            failures.push(format!(
                                "attempt {attempt}: bind udp {addr}:{port}: {e}"
                            ));
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
            let (udp_stats_tx, udp_stats_rx) = watch::channel(UdpAdmissionStats::default());
            let (tcp_stats_tx, mut tcp_stats_rx) = watch::channel(TcpAdmissionStats::default());
            let mut serving = tokio::spawn(async move {
                server::run_with_limits(
                    config,
                    shutdown_rx,
                    Arc::new(NoopIpTest),
                    Arc::new(NoopIpWriter),
                    Arc::new(FixedClock),
                    max_udp_queries,
                    Some(udp_stats_tx),
                    DEFAULT_MAX_TCP_CONNECTIONS,
                    Some(tcp_stats_tx),
                )
                .await
                .map_err(|e| e.to_string())
            });

            // TCP listeners are spawned after all UDP listeners, so probing the last TCP
            // endpoint guarantees all UDP and TCP listeners are bound and live while
            // leaving UDP admission counters at 0.
            let last_endpoint = *endpoints.last().unwrap();
            let ready = timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        biased;
                        result = &mut serving => return Err(format!("startup exited: {result:?}")),
                        connected = TcpStream::connect(last_endpoint) => {
                            if let Ok(stream) = connected {
                                drop(stream);
                                tcp_stats_rx
                                    .wait_for(|s| s.admitted >= 1 && s.active == 0)
                                    .await
                                    .map_err(|e| format!("tcp stats closed: {e}"))?;
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
                    return Self {
                        endpoints,
                        upstream_socket,
                        pending,
                        udp_stats_rx,
                        tcp_stats_rx,
                        shutdown,
                        serving: Some(serving),
                        worker,
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

    async fn wait_udp_stats(
        &mut self,
        mut predicate: impl FnMut(&UdpAdmissionStats) -> bool,
    ) -> UdpAdmissionStats {
        *timeout(IO_TIMEOUT, self.udp_stats_rx.wait_for(|s| predicate(s)))
            .await
            .expect("timed out waiting for UdpAdmissionStats condition")
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
fn default_udp_limit_is_bounded_and_independent_of_tcp() {
    assert_eq!(DEFAULT_MAX_UDP_QUERIES, 256);
    assert_eq!(DEFAULT_MAX_TCP_CONNECTIONS, 128);
}

#[tokio::test(flavor = "current_thread")]
async fn shared_quota_across_listeners_tcp_independent_and_recovery() {
    // 4 listeners (2 addresses x 2 ports) sharing a small UDP quota of 2 (< listener count).
    let mut h = Harness::start(
        &[Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 0, 0, 2)],
        2,
        2,
    )
    .await;
    assert_eq!(h.endpoints.len(), 4);

    // 1. Idle listeners must not pre-occupy permits: a single UDP listener can use all 2 permits
    //    while the other 3 UDP listeners remain idle.
    let c1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let c2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c1.send_to(&build_query(0x1001, "idle1"), h.endpoints[0])
        .await
        .unwrap();
    c2.send_to(&build_query(0x1002, "idle2"), h.endpoints[0])
        .await
        .unwrap();

    let u1 = h.recv_upstream().await;
    let u2 = h.recv_upstream().await;
    let stats = h.wait_udp_stats(|s| s.active == 2).await;
    assert_eq!(stats.admitted, 2);
    assert_eq!(stats.peak_active, 2);
    assert_eq!(stats.rejected, 0);

    h.reply_upstream(&u1, 1).await;
    h.reply_upstream(&u2, 2).await;
    let r1 = recv_udp_reply(&c1).await;
    let r2 = recv_udp_reply(&c2).await;
    assert_eq!(&r1[..2], &0x1001u16.to_be_bytes());
    assert_eq!(&r2[..2], &0x1002u16.to_be_bytes());
    let stats = h.wait_udp_stats(|s| s.active == 0).await;
    assert_eq!(stats.rejected, 0);

    // Malformed UDP datagrams must be discarded before taking permits or spawning tasks.
    c1.send_to(&[0x12, 0x34, 0x00, 0x00], h.endpoints[1])
        .await
        .unwrap();

    // 2. Cross-listener sharing: occupy 1 UDP slot on listener[0] and 1 on listener[1],
    //    holding both at the controlled upstream barrier.
    let a1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let a2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    a1.send_to(&build_query(0x2001, "cross1"), h.endpoints[0])
        .await
        .unwrap();
    a2.send_to(&build_query(0x2002, "cross2"), h.endpoints[1])
        .await
        .unwrap();

    let u_first = h.recv_upstream().await;
    let u_second = h.recv_upstream().await;
    let (ua1, ua2) = if u_first.0.windows(6).any(|w| w == b"cross1") {
        (u_first, u_second)
    } else {
        (u_second, u_first)
    };
    let stats = h.wait_udp_stats(|s| s.active == 2).await;
    assert_eq!(stats.admitted, 4);
    assert_eq!(stats.peak_active, 2);
    assert_eq!(stats.rejected, 0);

    // At capacity (2/2): extra UDP queries on another listener (listener[2]) and an already-busy
    // listener (listener[0]) must be dropped before copying/spawning and must NOT reach upstream.
    let over = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    over.send_to(&build_query(0x2003, "over1"), h.endpoints[2])
        .await
        .unwrap();
    over.send_to(&build_query(0x2004, "over2"), h.endpoints[0])
        .await
        .unwrap();

    let stats = h.wait_udp_stats(|s| s.rejected == 2).await;
    assert_eq!(stats.admitted, 4, "overload must not spawn new UDP tasks");
    assert_eq!(stats.active, 2);
    assert_eq!(stats.peak_active, 2);
    h.assert_no_upstream_pending();

    // 3. While UDP is at capacity, TCP queries must remain completely independent and succeed.
    let tcp_base = h.tcp_stats_rx.borrow().admitted;
    let mut tcp_client = TcpStream::connect(h.endpoints[2]).await.unwrap();
    write_tcp_frame(&mut tcp_client, &build_query(0x3001, "tcp-ok")).await;
    let u_tcp = h.recv_upstream().await;
    assert!(
        u_tcp.0.windows(6).any(|w| w == b"tcp-ok"),
        "expected TCP query at upstream, not an overloaded UDP query"
    );
    h.reply_upstream(&u_tcp, 33).await;
    let tcp_reply = read_tcp_frame_and_eof(&mut tcp_client).await;
    assert_eq!(&tcp_reply[..2], &0x3001u16.to_be_bytes());
    assert!(h.tcp_stats_rx.borrow().admitted >= tcp_base + 1);
    h.assert_no_upstream_pending();

    // 4. Release one held UDP query -> permit returns -> subsequent UDP query on listener[3]
    //    succeeds cleanly.
    h.reply_upstream(&ua1, 11).await;
    let ra1 = recv_udp_reply(&a1).await;
    assert_eq!(&ra1[..2], &0x2001u16.to_be_bytes());
    let stats = h.wait_udp_stats(|s| s.active == 1).await;
    assert_eq!(stats.active, 1);

    let reuse = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    reuse
        .send_to(&build_query(0x4001, "reused"), h.endpoints[3])
        .await
        .unwrap();
    let u_reuse = h.recv_upstream().await;
    assert!(u_reuse.0.windows(6).any(|w| w == b"reused"));
    let stats = h.wait_udp_stats(|s| s.active == 2).await;
    assert_eq!(stats.admitted, 5);
    assert_eq!(stats.peak_active, 2);
    assert_eq!(stats.rejected, 2);

    h.reply_upstream(&u_reuse, 44).await;
    let r_reuse = recv_udp_reply(&reuse).await;
    assert_eq!(&r_reuse[..2], &0x4001u16.to_be_bytes());

    h.reply_upstream(&ua2, 22).await;
    let ra2 = recv_udp_reply(&a2).await;
    assert_eq!(&ra2[..2], &0x2002u16.to_be_bytes());
    let final_stats = h.wait_udp_stats(|s| s.active == 0).await;
    assert_eq!(final_stats.admitted, 5);
    assert_eq!(final_stats.peak_active, 2);
    assert_eq!(final_stats.rejected, 2);
    println!(
        "shared_udp_quota: listeners={} test_limit=2 prod_limit={} admitted={} peak_active={} rejected={}",
        h.endpoints.len(),
        DEFAULT_MAX_UDP_QUERIES,
        final_stats.admitted,
        final_stats.peak_active,
        final_stats.rejected,
    );

    let (_, res) = h.stop().await;
    res.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_converges_within_budget_when_at_capacity() {
    let mut h = Harness::start(&[Ipv4Addr::LOCALHOST], 2, 2).await;

    // Fill both UDP slots across the two listeners and hold them at the upstream barrier.
    let c1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let c2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c1.send_to(&build_query(0x6001, "drain1"), h.endpoints[0])
        .await
        .unwrap();
    c2.send_to(&build_query(0x6002, "drain2"), h.endpoints[1])
        .await
        .unwrap();
    let _held1 = h.recv_upstream().await;
    let _held2 = h.recv_upstream().await;
    h.wait_udp_stats(|s| s.active == 2).await;

    // Confirm saturation via a rejected overload datagram.
    let over = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    over.send_to(&build_query(0x6003, "drain-over"), h.endpoints[0])
        .await
        .unwrap();
    let s_sat = h.wait_udp_stats(|s| s.rejected == 1).await;
    assert_eq!(s_sat.admitted, 2);
    assert_eq!(s_sat.active, 2);
    assert_eq!(s_sat.peak_active, 2);
    h.assert_no_upstream_pending();

    // Trigger shutdown while at capacity: in-flight UDP queries drain up to the existing
    // 500ms global budget, then cancel cleanly and release all permits.
    let mut udp_stats_rx = h.udp_stats_rx.clone();
    let (elapsed, res) = h.stop().await;
    res.unwrap();
    assert!(
        elapsed >= Duration::from_millis(450),
        "drain should respect existing 500ms budget for in-flight UDP queries, got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(900),
        "full UDP quota must not block shutdown convergence, got {elapsed:?}"
    );
    let s_done = *timeout(IO_TIMEOUT, udp_stats_rx.wait_for(|s| s.active == 0))
        .await
        .expect("all UDP permits must be released on shutdown cancellation")
        .unwrap();
    assert_eq!(s_done.active, 0);
    assert_eq!(s_done.peak_active, 2);
    assert_eq!(s_done.rejected, 1);
    println!(
        "shutdown_udp_at_capacity: drain={elapsed:?} active_after={} peak_active={} rejected={}",
        s_done.active, s_done.peak_active, s_done.rejected,
    );
}
