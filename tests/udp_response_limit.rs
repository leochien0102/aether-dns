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

use config::{Config, Protocol, Upstream};
use hickory_proto::op::{Message, ResponseCode};
use hickory_proto::rr::RecordType;
use server::cache::Clock;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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

fn encode_name(labels: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for label in labels {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

fn build_query(id: u16, labels: &[&str], qtype: u16, edns_payload: Option<u16>) -> Vec<u8> {
    let mut wire = Vec::new();
    wire.extend_from_slice(&id.to_be_bytes());
    wire.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    wire.extend_from_slice(&(u16::from(edns_payload.is_some())).to_be_bytes());
    wire.extend_from_slice(&encode_name(labels));
    wire.extend_from_slice(&qtype.to_be_bytes());
    wire.extend_from_slice(&1u16.to_be_bytes());
    if let Some(payload) = edns_payload {
        wire.push(0);
        wire.extend_from_slice(&41u16.to_be_bytes());
        wire.extend_from_slice(&payload.to_be_bytes());
        wire.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    }
    wire
}

fn question_end(wire: &[u8]) -> usize {
    dns::skip_name(wire, 12).unwrap() + 4
}

/// Fixed oversized answer: 40 A records on the query's question.
/// For `big.gfw.example.` (question_end = 33), wire length is 33 + 40 * 16 = 673 bytes.
fn build_oversized_a_response(query: &[u8], count: u16) -> Vec<u8> {
    let qend = question_end(query);
    let mut wire = query[..qend].to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire[6..8].copy_from_slice(&count.to_be_bytes());
    wire[8..12].fill(0);
    for i in 0..count {
        wire.extend_from_slice(&[0xc0, 0x0c]);
        wire.extend_from_slice(&1u16.to_be_bytes());
        wire.extend_from_slice(&1u16.to_be_bytes());
        wire.extend_from_slice(&60u32.to_be_bytes());
        wire.extend_from_slice(&4u16.to_be_bytes());
        wire.extend_from_slice(&[203, 0, 113, (i as u8).wrapping_add(1)]);
    }
    wire
}

/// Build an exact-byte-length response with one A record plus one TXT record
/// whose length is chosen to hit `target_len` bytes exactly.
fn build_exact_len_response(query: &[u8], target_len: usize) -> Vec<u8> {
    let qend = question_end(query);
    let mut wire = query[..qend].to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire[6..8].copy_from_slice(&2u16.to_be_bytes());
    wire[8..12].fill(0);
    // 1 A record (16 bytes) so cache admission also accepts it if cached.
    wire.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 0, 113, 1]);
    // Remaining bytes go into a single TXT RR (12 bytes RR header + TXT rdata).
    let txt_rdata_len = target_len
        .checked_sub(wire.len() + 12)
        .expect("target_len too small");
    let mut rdata = Vec::with_capacity(txt_rdata_len);
    let mut remaining = txt_rdata_len;
    while remaining > 0 {
        let chunk = remaining.saturating_sub(1).min(255);
        rdata.push(chunk as u8);
        rdata.extend(std::iter::repeat_n(b'x', chunk));
        remaining -= 1 + chunk;
    }
    wire.extend_from_slice(&[0xc0, 0x0c]);
    wire.extend_from_slice(&16u16.to_be_bytes());
    wire.extend_from_slice(&1u16.to_be_bytes());
    wire.extend_from_slice(&60u32.to_be_bytes());
    wire.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    wire.extend_from_slice(&rdata);
    assert_eq!(wire.len(), target_len);
    wire
}

struct MockTcpUpstream {
    port: u16,
    calls: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MockTcpUpstream {
    async fn start<F>(responder: F) -> Self
    where
        F: Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let responder = Arc::new(responder);
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let calls = calls_clone.clone();
                let responder = responder.clone();
                tokio::spawn(async move {
                    let Ok(len) = stream.read_u16().await else {
                        return;
                    };
                    let mut query = vec![0; len as usize];
                    if stream.read_exact(&mut query).await.is_err() {
                        return;
                    }
                    calls.fetch_add(1, Ordering::SeqCst);
                    let reply = responder(&query);
                    let _ = stream.write_u16(reply.len() as u16).await;
                    let _ = stream.write_all(&reply).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { port, calls, task }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Drop for MockTcpUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct TestServer {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), String>>,
}

impl TestServer {
    async fn start(upstream_port: u16, cache: Option<u64>) -> Self {
        let (port, shutdown, task) = in_process::start(|port, shutdown| {
            let mut config = Config::default();
            config.bind_addr = vec![Ipv4Addr::LOCALHOST];
            config.bind_port = vec![port];
            config.trust_dns = vec![Upstream {
                protocol: Some(Protocol::Tcp),
                host: None,
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: Some(upstream_port),
            }];
            config.cache = cache;
            async move {
                server::run_with_cache_clock(
                    config,
                    shutdown,
                    Arc::new(NoopIpTest),
                    Arc::new(NoopIpWriter),
                    Arc::new(FixedClock),
                )
                .await
                .map_err(|e| e.to_string())
            }
        })
        .await;
        Self {
            port,
            shutdown,
            task,
        }
    }

    async fn udp_query(&self, query: &[u8]) -> Vec<u8> {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.send_to(query, ("127.0.0.1", self.port)).await.unwrap();
        let mut buf = vec![0; 65535];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
            .await
            .expect("UDP reply timeout")
            .unwrap();
        buf.truncate(len);
        buf
    }

    async fn tcp_query(&self, query: &[u8]) -> Vec<u8> {
        let mut stream = timeout(
            Duration::from_secs(3),
            TcpStream::connect(("127.0.0.1", self.port)),
        )
        .await
        .expect("TCP connect timeout")
        .unwrap();
        stream.write_u16(query.len() as u16).await.unwrap();
        stream.write_all(query).await.unwrap();
        let len = timeout(Duration::from_secs(3), stream.read_u16())
            .await
            .expect("TCP read len timeout")
            .unwrap() as usize;
        let mut buf = vec![0; len];
        timeout(Duration::from_secs(3), stream.read_exact(&mut buf))
            .await
            .expect("TCP read body timeout")
            .unwrap();
        buf
    }

    async fn stop(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

fn summarize(label: &str, wire: &[u8]) -> Message {
    let msg = Message::from_vec(wire)
        .unwrap_or_else(|e| panic!("{label}: unparseable DNS wire ({} bytes): {e}", wire.len()));
    let q = msg
        .queries()
        .first()
        .map(|q| format!("{} {}", q.name(), q.query_type()))
        .unwrap_or_else(|| "<none>".into());
    eprintln!(
        "[{label}] len={} id={:#06x} q=({q}) rcode={:?} tc={} an={} ns={} ar={}",
        wire.len(),
        msg.id(),
        msg.response_code(),
        msg.truncated(),
        msg.answers().len(),
        msg.name_servers().len(),
        msg.additionals().len() + usize::from(msg.extensions().is_some()),
    );
    msg
}

#[tokio::test]
async fn udp_without_edns_truncates_and_tcp_retry_gets_full_answer() {
    let upstream = MockTcpUpstream::start(|q| build_oversized_a_response(q, 40)).await;
    let server = TestServer::start(upstream.port, None).await;

    let udp_q = build_query(0x1111, &["big", "gfw", "example"], 1, None);
    let udp_resp = server.udp_query(&udp_q).await;
    let udp_msg = summarize("udp-no-edns-673", &udp_resp);
    assert!(
        udp_resp.len() <= 512,
        "UDP reply without EDNS must be <= 512 bytes, got {}",
        udp_resp.len()
    );
    assert_eq!(udp_msg.id(), 0x1111);
    assert_eq!(udp_msg.queries()[0].name().to_ascii(), "big.gfw.example.");
    assert_eq!(udp_msg.queries()[0].query_type(), RecordType::A);
    assert_eq!(udp_msg.response_code(), ResponseCode::NoError);
    assert!(udp_msg.truncated(), "expected TC=1 on truncated UDP reply");
    assert_eq!(udp_msg.answers().len(), 0);

    let tcp_q = build_query(0x2222, &["big", "gfw", "example"], 1, None);
    let tcp_resp = server.tcp_query(&tcp_q).await;
    let tcp_msg = summarize("tcp-retry-673", &tcp_resp);
    assert_eq!(tcp_resp.len(), 673);
    assert_eq!(tcp_msg.id(), 0x2222);
    assert_eq!(tcp_msg.queries()[0].name().to_ascii(), "big.gfw.example.");
    assert_eq!(tcp_msg.response_code(), ResponseCode::NoError);
    assert!(!tcp_msg.truncated());
    assert_eq!(tcp_msg.answers().len(), 40);

    server.stop().await;
}

#[tokio::test]
async fn udp_edns_budget_controls_full_vs_truncated() {
    let upstream = MockTcpUpstream::start(|q| build_oversized_a_response(q, 40)).await;
    let server = TestServer::start(upstream.port, None).await;

    let large_q = build_query(0x1201, &["big", "gfw", "example"], 1, Some(1232));
    let large_resp = server.udp_query(&large_q).await;
    let large_msg = summarize("udp-edns-1232", &large_resp);
    assert_eq!(large_resp.len(), 673 + 11);
    assert_eq!(large_msg.id(), 0x1201);
    assert!(!large_msg.truncated());
    assert_eq!(large_msg.answers().len(), 40);
    assert!(large_msg.extensions().is_some());

    let small_q = build_query(0x1202, &["big", "gfw", "example"], 1, Some(400));
    let small_resp = server.udp_query(&small_q).await;
    let small_msg = summarize("udp-edns-400", &small_resp);
    assert!(small_resp.len() <= 512, "got {}", small_resp.len());
    assert_eq!(small_msg.id(), 0x1202);
    assert!(small_msg.truncated());
    assert_eq!(small_msg.answers().len(), 0);
    assert!(small_msg.extensions().is_some());

    server.stop().await;
}

#[tokio::test]
async fn cache_hit_enforces_per_client_udp_limit_without_polluting_cache() {
    let upstream = MockTcpUpstream::start(|q| build_oversized_a_response(q, 40)).await;
    let server = TestServer::start(upstream.port, Some(64)).await;

    // 1) Small-budget UDP client populates cache; cache must store full response.
    let q1 = build_query(0x3101, &["c1", "gfw", "example"], 1, None);
    let r1 = server.udp_query(&q1).await;
    let m1 = summarize("cache-fill-no-edns", &r1);
    assert_eq!(upstream.calls(), 1);
    assert!(r1.len() <= 512, "got {}", r1.len());
    assert_eq!(m1.id(), 0x3101);
    assert!(m1.truncated());

    // 2) Larger-budget UDP client hits cache and gets full answer (no second upstream call).
    let q2 = build_query(0x3102, &["c1", "gfw", "example"], 1, Some(1232));
    let r2 = server.udp_query(&q2).await;
    let m2 = summarize("cache-hit-edns-1232", &r2);
    assert_eq!(upstream.calls(), 1, "must be a cache hit");
    assert!(!m2.truncated());
    assert_eq!(m2.id(), 0x3102);
    assert_eq!(m2.answers().len(), 40);

    // 3) Another small-budget UDP client hits cache and is still truncated.
    let q3 = build_query(0x3103, &["c1", "gfw", "example"], 1, None);
    let r3 = server.udp_query(&q3).await;
    let m3 = summarize("cache-hit-no-edns", &r3);
    assert_eq!(upstream.calls(), 1, "must be a cache hit");
    assert!(r3.len() <= 512, "got {}", r3.len());
    assert_eq!(m3.id(), 0x3103);
    assert!(m3.truncated());

    // 4) TCP client hits cache and gets full answer without UDP cap.
    let q4 = build_query(0x3104, &["c1", "gfw", "example"], 1, None);
    let r4 = server.tcp_query(&q4).await;
    let m4 = summarize("cache-hit-tcp", &r4);
    assert_eq!(upstream.calls(), 1, "must be a cache hit");
    assert!(!m4.truncated());
    assert_eq!(m4.id(), 0x3104);
    assert_eq!(m4.answers().len(), 40);

    // 5) Reverse order: large-budget UDP client populates cache -> small-budget UDP client hits cache.
    let q5 = build_query(0x3201, &["c2", "gfw", "example"], 1, Some(1232));
    let r5 = server.udp_query(&q5).await;
    let m5 = summarize("cache-fill-edns-1232", &r5);
    assert_eq!(upstream.calls(), 2);
    assert!(!m5.truncated());
    assert_eq!(m5.answers().len(), 40);

    let q6 = build_query(0x3202, &["c2", "gfw", "example"], 1, None);
    let r6 = server.udp_query(&q6).await;
    let m6 = summarize("cache-hit-no-edns-after-large", &r6);
    assert_eq!(upstream.calls(), 2, "must be a cache hit");
    assert!(r6.len() <= 512, "got {}", r6.len());
    assert_eq!(m6.id(), 0x3202);
    assert!(m6.truncated());

    server.stop().await;
}

#[tokio::test]
async fn udp_exact_512_and_513_boundary() {
    let upstream = MockTcpUpstream::start(|q| {
        let msg = Message::from_vec(q).unwrap();
        let name = msg.queries()[0].name().to_ascii();
        if name.starts_with("b512.") {
            build_exact_len_response(q, 512)
        } else if name.starts_with("b513.") {
            build_exact_len_response(q, 513)
        } else if name.starts_with("e501.") {
            build_exact_len_response(q, 501)
        } else {
            build_exact_len_response(q, 502)
        }
    })
    .await;
    let server = TestServer::start(upstream.port, None).await;

    // No EDNS: 512 fits untouched; 513 truncates with TC=1.
    let q512 = build_query(0x4101, &["b512", "gfw", "example"], 1, None);
    let r512 = server.udp_query(&q512).await;
    let m512 = summarize("boundary-512-no-edns", &r512);
    assert_eq!(r512.len(), 512);
    assert!(!m512.truncated());
    assert_eq!(m512.answers().len(), 2);
    assert!(m512.extensions().is_none());

    let q513 = build_query(0x4102, &["b513", "gfw", "example"], 1, None);
    let r513 = server.udp_query(&q513).await;
    let m513 = summarize("boundary-513-no-edns", &r513);
    assert!(r513.len() <= 512, "got {}", r513.len());
    assert!(m513.truncated());
    assert_eq!(m513.answers().len(), 0);
    assert!(m513.extensions().is_none());

    // EDNS < 512 (e.g. 400) treated as 512 per RFC 6891, including minimal 11 B OPT.
    let q512_edns = build_query(0x4103, &["e501", "gfw", "example"], 1, Some(400));
    let r512_edns = server.udp_query(&q512_edns).await;
    let m512_edns = summarize("boundary-512-edns-400", &r512_edns);
    assert_eq!(r512_edns.len(), 512);
    assert!(!m512_edns.truncated());
    assert_eq!(m512_edns.answers().len(), 2);
    assert!(m512_edns.extensions().is_some());

    let q513_edns = build_query(0x4104, &["e502", "gfw", "example"], 1, Some(400));
    let r513_edns = server.udp_query(&q513_edns).await;
    let m513_edns = summarize("boundary-513-edns-400", &r513_edns);
    assert!(r513_edns.len() <= 512, "got {}", r513_edns.len());
    assert!(m513_edns.truncated());
    assert_eq!(m513_edns.answers().len(), 0);
    assert!(m513_edns.extensions().is_some());

    server.stop().await;
}

#[test]
fn udp_budget_rules() {
    assert_eq!(
        dns::udp_budget(&build_query(1, &["a", "example"], 1, None)),
        dns::UdpBudget {
            max_size: 512,
            client_edns: false,
        }
    );
    assert_eq!(
        dns::udp_budget(&build_query(2, &["a", "example"], 1, Some(0))),
        dns::UdpBudget {
            max_size: 512,
            client_edns: true,
        }
    );
    assert_eq!(
        dns::udp_budget(&build_query(3, &["a", "example"], 1, Some(400))),
        dns::UdpBudget {
            max_size: 512,
            client_edns: true,
        }
    );
    assert_eq!(
        dns::udp_budget(&build_query(4, &["a", "example"], 1, Some(512))),
        dns::UdpBudget {
            max_size: 512,
            client_edns: true,
        }
    );
    assert_eq!(
        dns::udp_budget(&build_query(5, &["a", "example"], 1, Some(1232))),
        dns::UdpBudget {
            max_size: 1232,
            client_edns: true,
        }
    );
    assert_eq!(
        dns::udp_budget(&build_query(6, &["a", "example"], 1, Some(4096))),
        dns::UdpBudget {
            max_size: 4096,
            client_edns: true,
        }
    );
    // Malformed / truncated query falls back to non-EDNS 512.
    assert_eq!(
        dns::udp_budget(&[0; 12]),
        dns::UdpBudget {
            max_size: 512,
            client_edns: false,
        }
    );
}

#[test]
fn optional_additional_omission_does_not_set_tc() {
    let no_edns = dns::UdpBudget {
        max_size: 512,
        client_edns: false,
    };
    let edns_512 = dns::UdpBudget {
        max_size: 512,
        client_edns: true,
    };
    let q = build_query(0x5101, &["opt", "gfw", "example"], 1, None);
    // 1) Non-OPT additional glue records push size > 512 while answers fit.
    let mut with_glue = build_exact_len_response(&q, 420);
    let extra_glue_count = 10u16; // 10 * 16 = 160 bytes -> 580 bytes total.
    with_glue[10..12].copy_from_slice(&extra_glue_count.to_be_bytes());
    for i in 0..extra_glue_count {
        with_glue.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 198, 51, 100, i as u8]);
    }
    assert_eq!(with_glue.len(), 580);
    dns::enforce_udp_limit(&mut with_glue, no_edns);
    let m_glue = summarize("pure-optional-glue", &with_glue);
    assert!(with_glue.len() <= 512);
    assert!(!m_glue.truncated(), "omitting optional additionals must not set TC");
    assert_eq!(m_glue.id(), 0x5101);
    assert_eq!(m_glue.answers().len(), 2);
    assert_eq!(m_glue.additionals().len(), 0);

    // 2) Large optional EDNS OPT padding pushes size > 512 while answers + minimal OPT fit.
    let mut with_opt_pad = build_exact_len_response(&q, 420);
    with_opt_pad[10..12].copy_from_slice(&1u16.to_be_bytes());
    // OPT RR with code 12 (padding), 120 bytes of zeros -> 11 + 4 + 120 = 135 bytes -> 555 bytes.
    with_opt_pad.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0]);
    with_opt_pad.extend_from_slice(&(124u16).to_be_bytes());
    with_opt_pad.extend_from_slice(&12u16.to_be_bytes());
    with_opt_pad.extend_from_slice(&120u16.to_be_bytes());
    with_opt_pad.extend(std::iter::repeat_n(0u8, 120));
    assert_eq!(with_opt_pad.len(), 555);
    let mut with_opt_pad_edns = with_opt_pad.clone();
    dns::enforce_udp_limit(&mut with_opt_pad, no_edns);
    let m_pad = summarize("pure-optional-opt-padding-no-edns", &with_opt_pad);
    assert!(with_opt_pad.len() <= 512);
    assert!(!m_pad.truncated(), "stripping optional OPT content must not set TC");
    assert_eq!(m_pad.answers().len(), 2);

    dns::enforce_udp_limit(&mut with_opt_pad_edns, edns_512);
    let m_pad_edns = summarize("pure-optional-opt-padding-edns512", &with_opt_pad_edns);
    assert!(with_opt_pad_edns.len() <= 512);
    assert!(!m_pad_edns.truncated(), "stripping OPT padding for EDNS client must not set TC");
    assert_eq!(m_pad_edns.answers().len(), 2);
    assert!(m_pad_edns.extensions().is_some(), "minimal OPT must remain for EDNS client");

    // 3) Minimal OPT (ext_rcode=0) pushes 509-byte answers to 520 bytes:
    //    - No-EDNS client: omitting OPT fits with TC=0.
    //    - EDNS 512 client: OPT must NOT be dropped -> TC=1 with minimal OPT preserved.
    let mut with_min_opt = build_exact_len_response(&q, 509);
    with_min_opt[10..12].copy_from_slice(&1u16.to_be_bytes());
    with_min_opt.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    assert_eq!(with_min_opt.len(), 520);
    let mut with_min_opt_edns = with_min_opt.clone();

    dns::enforce_udp_limit(&mut with_min_opt, no_edns);
    let m_min = summarize("pure-optional-min-opt-no-edns", &with_min_opt);
    assert!(with_min_opt.len() <= 512);
    assert!(!m_min.truncated(), "omitting optional OPT for non-EDNS client must not set TC");
    assert_eq!(m_min.answers().len(), 2);
    assert!(m_min.extensions().is_none());

    dns::enforce_udp_limit(&mut with_min_opt_edns, edns_512);
    let m_min_edns = summarize("pure-min-opt-edns512-tc", &with_min_opt_edns);
    assert!(with_min_opt_edns.len() <= 512);
    assert!(m_min_edns.truncated(), "EDNS 512 client must get TC=1 instead of dropping OPT");
    assert_eq!(m_min_edns.id(), 0x5101);
    assert_eq!(m_min_edns.answers().len(), 0);
    assert!(m_min_edns.extensions().is_some(), "minimal OPT must be preserved");
}

#[test]
fn existing_tc_is_preserved_in_all_cases() {
    let no_edns = dns::UdpBudget {
        max_size: 512,
        client_edns: false,
    };
    let q = build_query(0x5201, &["tc", "gfw", "example"], 1, None);

    // Under limit with TC=1: left untouched.
    let mut small_tc = build_exact_len_response(&q, 400);
    small_tc[2] |= 0x02;
    let before = small_tc.clone();
    dns::enforce_udp_limit(&mut small_tc, no_edns);
    assert_eq!(small_tc, before);
    assert!(Message::from_vec(&small_tc).unwrap().truncated());

    // Over limit only due to optional additionals, already TC=1: must keep TC=1.
    let mut over_tc = build_exact_len_response(&q, 420);
    over_tc[2] |= 0x02;
    over_tc[10..12].copy_from_slice(&10u16.to_be_bytes());
    for i in 0..10u8 {
        over_tc.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 198, 51, 100, i]);
    }
    dns::enforce_udp_limit(&mut over_tc, no_edns);
    let m = summarize("pure-existing-tc-over", &over_tc);
    assert!(over_tc.len() <= 512);
    assert!(m.truncated(), "existing TC=1 must never be cleared");
}

#[test]
fn extended_rcode_is_never_downgraded_to_noerror() {
    let edns_512 = dns::UdpBudget {
        max_size: 512,
        client_edns: true,
    };
    let q = build_query(0x5301, &["badvers", "gfw", "example"], 1, Some(1232));

    // Case 1: BADVERS (16 = low 0 + OPT ext_rcode 1) over 512 only due to OPT padding.
    let mut badvers_pad = build_exact_len_response(&q, 420);
    badvers_pad[10..12].copy_from_slice(&1u16.to_be_bytes());
    // OPT with ext_rcode = 1 (BADVERS), padding 120 bytes.
    badvers_pad.extend_from_slice(&[0, 0, 41, 4, 208, 1, 0, 0, 0]);
    badvers_pad.extend_from_slice(&124u16.to_be_bytes());
    badvers_pad.extend_from_slice(&12u16.to_be_bytes());
    badvers_pad.extend_from_slice(&120u16.to_be_bytes());
    badvers_pad.extend(std::iter::repeat_n(0u8, 120));
    dns::enforce_udp_limit(&mut badvers_pad, edns_512);
    let m1 = summarize("pure-badvers-strip-pad", &badvers_pad);
    assert!(badvers_pad.len() <= 512);
    assert!(!m1.truncated());
    assert_eq!(u16::from(m1.response_code()), 16);
    assert_eq!(dns::rcode(&badvers_pad), Ok(16));

    // Case 2: BADVERS (16) over 512 due to required answer section -> TC=1 AND still BADVERS!
    let mut badvers_big = build_oversized_a_response(&q, 40);
    badvers_big[10..12].copy_from_slice(&1u16.to_be_bytes());
    badvers_big.extend_from_slice(&[0, 0, 41, 4, 208, 1, 0, 0, 0, 0, 0]);
    assert!(badvers_big.len() > 512);
    dns::enforce_udp_limit(&mut badvers_big, edns_512);
    let m2 = summarize("pure-badvers-truncated", &badvers_big);
    assert!(badvers_big.len() <= 512);
    assert!(m2.truncated());
    assert_eq!(m2.id(), 0x5301);
    assert_eq!(u16::from(m2.response_code()), 16);
    assert_eq!(dns::rcode(&badvers_big), Ok(16));

    // Case 3: Malformed wire (>512 bytes, already TC=1 with complete OPT followed by truncated RR)
    // triggers wire-level fallback and still yields parseable TC=1 with RCODE 16.
    let mut malformed = build_exact_len_response(&q, 420);
    malformed[2] |= 0x02;
    malformed[10..12].copy_from_slice(&2u16.to_be_bytes());
    malformed.extend_from_slice(&[0, 0, 41, 4, 208, 1, 0, 0, 0, 0, 0]);
    // Second additional RR claims 200 bytes of rdata but is cut short (still total > 512).
    malformed.extend_from_slice(&[0xc0, 0x0c, 0, 16, 0, 1, 0, 0, 0, 60, 0, 200]);
    malformed.extend(std::iter::repeat_n(b'z', 100));
    assert!(malformed.len() > 512);
    assert!(Message::from_vec(&malformed).is_err());
    dns::enforce_udp_limit(&mut malformed, edns_512);
    let m3 = summarize("pure-badvers-fallback", &malformed);
    assert!(malformed.len() <= 512);
    assert!(m3.truncated());
    assert_eq!(m3.id(), 0x5301);
    assert_eq!(u16::from(m3.response_code()), 16);
    assert_eq!(dns::rcode(&malformed), Ok(16));
}

#[tokio::test]
async fn udp_edns_512_noerror_preserves_minimal_opt_and_badvers_keeps_rcode_16() {
    let upstream = MockTcpUpstream::start(|q| {
        let msg = Message::from_vec(q).unwrap();
        let name = msg.queries()[0].name().to_ascii();
        if name.starts_with("edns509.") {
            // 509 B answers + 11 B minimal OPT = 520 B (answers fit alone, +OPT exceeds 512).
            let mut r = build_exact_len_response(q, 509);
            r[10..12].copy_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
            assert_eq!(r.len(), 520);
            r
        } else if name.starts_with("ednspad.") {
            // 420 B answers + 135 B OPT (120 B padding) = 555 B (fits with minimal OPT once padding removed).
            let mut r = build_exact_len_response(q, 420);
            r[10..12].copy_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0]);
            r.extend_from_slice(&124u16.to_be_bytes());
            r.extend_from_slice(&12u16.to_be_bytes());
            r.extend_from_slice(&120u16.to_be_bytes());
            r.extend(std::iter::repeat_n(0u8, 120));
            assert_eq!(r.len(), 555);
            r
        } else {
            // BADVERS (RCODE 16 = low 0 + OPT ext_rcode 1), TC=0, through real shared forwarding path.
            let mut r = build_exact_len_response(q, 420);
            r[10..12].copy_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(&[0, 0, 41, 4, 208, 1, 0, 0, 0, 0, 0]);
            r
        }
    })
    .await;
    let server = TestServer::start(upstream.port, None).await;

    // 1) EDNS 512 client: 509 B answers + 11 B minimal OPT = 520 B > 512 -> TC=1, OPT preserved.
    let q_509 = build_query(0x6101, &["edns509", "gfw", "example"], 1, Some(512));
    let r_509 = server.udp_query(&q_509).await;
    let m_509 = summarize("real-udp-edns512-509-plus-opt", &r_509);

    // 2) EDNS 512 client: 420 B answers + padded OPT (555 B) -> padding dropped, TC=0, answers & OPT preserved.
    let q_pad = build_query(0x6102, &["ednspad", "gfw", "example"], 1, Some(512));
    let r_pad = server.udp_query(&q_pad).await;
    let m_pad = summarize("real-udp-edns512-strip-padding", &r_pad);

    // 3) Real shared forwarding path BADVERS (RCODE 16): must not be stripped as NOERROR.
    let q_bv = build_query(0x6103, &["badvers", "gfw", "example"], 1, Some(512));
    let r_bv = server.udp_query(&q_bv).await;
    let m_bv = summarize("real-udp-shared-badvers", &r_bv);

    server.stop().await;

    assert!(r_509.len() <= 512, "got {}", r_509.len());
    assert_eq!(m_509.id(), 0x6101);
    assert_eq!(m_509.queries()[0].name().to_ascii(), "edns509.gfw.example.");
    assert_eq!(m_509.queries()[0].query_type(), RecordType::A);
    assert_eq!(m_509.response_code(), ResponseCode::NoError);
    assert!(m_509.truncated(), "expected TC=1 when answers + minimal OPT exceed 512");
    assert!(m_509.extensions().is_some(), "minimal OPT must be preserved for EDNS client");

    assert!(r_pad.len() <= 512, "got {}", r_pad.len());
    assert_eq!(m_pad.id(), 0x6102);
    assert_eq!(m_pad.queries()[0].name().to_ascii(), "ednspad.gfw.example.");
    assert_eq!(m_pad.response_code(), ResponseCode::NoError);
    assert!(!m_pad.truncated(), "dropping padding alone must not set TC");
    assert_eq!(m_pad.answers().len(), 2);
    assert!(m_pad.extensions().is_some(), "minimal OPT must remain present");

    assert!(r_bv.len() <= 512, "got {}", r_bv.len());
    assert_eq!(m_bv.id(), 0x6103);
    assert_eq!(u16::from(m_bv.response_code()), 16, "BADVERS must remain RCODE 16");
    assert_eq!(dns::rcode(&r_bv), Ok(16));
    assert!(m_bv.extensions().is_some());
}
