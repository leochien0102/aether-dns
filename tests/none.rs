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
use server::ip_test::IpTest;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::{Duration, sleep, timeout};

struct FakeIpTest {
    result: bool,
    failed: bool,
    calls: Mutex<Vec<IpAddr>>,
}
impl IpTest for FakeIpTest {
    fn is_china(&self, ip: IpAddr) -> bool {
        self.calls.lock().unwrap().push(ip);
        if self.failed {
            // IpTest's bool contract maps a backend failure to non-china.
            eprintln!("fake IP test: backend unavailable; non-china");
            false
        } else {
            self.result
        }
    }
}

#[derive(Clone, Copy)]
enum Reply {
    Ip,
    NoIp,
    Tc,
    Fail,
    Timeout,
}
struct Case {
    china: Option<Reply>,
    trust: Option<Reply>,
    china_delay: u64,
    trust_delay: u64,
    qtype: u16,
    member: bool,
    test_failure: bool,
    expected: Option<usize>,
    ip_calls: usize,
    repeat: usize,
    extra_upstreams: bool,
}
impl Default for Case {
    fn default() -> Self {
        Self {
            china: Some(Reply::Ip),
            trust: Some(Reply::Ip),
            china_delay: 30,
            trust_delay: 90,
            qtype: 1,
            member: false,
            test_failure: false,
            expected: Some(1),
            ip_calls: 1,
            repeat: 1,
            extra_upstreams: false,
        }
    }
}

fn reply(query: &[u8], source: usize, kind: Reply, qtype: u16) -> Vec<u8> {
    let mut wire = query.to_vec();
    // AA is a source marker: china=true, trust=false. RA must be set by server.
    wire[2] = 0x81 | if source == 0 { 4 } else { 0 };
    wire[3] = 0;
    if matches!(kind, Reply::Tc) {
        wire[2] |= 2;
        return wire;
    }
    if matches!(kind, Reply::Ip) {
        // Two answers ensure that only the first IP is tested.
        wire[7] = 2;
        for last in [1, 2] {
            let data = if qtype == 28 {
                let mut ip = vec![0; 16];
                ip[15] = last;
                ip
            } else {
                vec![10, 0, source as u8, last]
            };
            let rrtype = if qtype == 28 { 28u16 } else { 1 };
            wire.extend_from_slice(&[0xc0, 0x0c]);
            wire.extend_from_slice(&rrtype.to_be_bytes());
            wire.extend_from_slice(&[0, 1, 0, 0, 0, 30]);
            wire.extend_from_slice(&(data.len() as u16).to_be_bytes());
            wire.extend_from_slice(&data);
        }
    }
    wire[11] = 1;
    wire.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    wire
}

async fn check(case: Case) {
    let hits = Arc::new(Mutex::new(vec![0usize; 4]));
    let fake = Arc::new(FakeIpTest {
        result: case.member,
        failed: case.test_failure,
        calls: Mutex::new(Vec::new()),
    });
    let mut workers = Vec::new();
    let mut config = Config {
        bind_addr: vec![Ipv4Addr::LOCALHOST],
        default_tag: Some(Tag::None),
        ..Config::default()
    };
    for (source, kind, delay) in [
        (0, case.china, case.china_delay),
        (1, case.trust, case.trust_delay),
    ] {
        let Some(kind) = kind else { continue };
        for extra in 0..=usize::from(case.extra_upstreams) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream = Upstream {
                protocol: Some(Protocol::Tcp),
                host: None,
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: Some(listener.local_addr().unwrap().port()),
            };
            if source == 0 {
                config.china_dns.push(upstream);
            } else {
                config.trust_dns.push(upstream);
            }
            let hits = hits.clone();
            let qtype = case.qtype;
            workers.push(tokio::spawn(async move {
                let mut pending = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = pending.join_next(), if !pending.is_empty() => {},
                        accepted = listener.accept() => {
                            let (mut stream, _) = accepted.unwrap();
                            let hits = hits.clone();
                            pending.spawn(async move {
                                let len = stream.read_u16().await.unwrap();
                                let mut query = vec![0; len as usize];
                                stream.read_exact(&mut query).await.unwrap();
                                assert_ne!(&query[..2], &[0x42, 0x42]);
                                hits.lock().unwrap()[source + extra * 2] += 1;
                                sleep(Duration::from_millis(delay + extra as u64 * 20)).await;
                                match kind {
                                    Reply::Fail => return,
                                    Reply::Timeout => { sleep(Duration::from_secs(6)).await; return; }
                                    _ => {}
                                }
                                let wire = reply(&query, source, kind, qtype);
                                // The winner cancels other connections; broken pipes are expected.
                                if stream.write_u16(wire.len() as u16).await.is_ok() {
                                    let _ = stream.write_all(&wire).await;
                                }
                            });
                        }
                    }
                }
            }));
        }
    }
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let testing = fake.clone();
        async move {
            server::run_with_ip_test(config, receiver, testing)
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await;
    let mut query = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    query.extend_from_slice(b"\x04none\x07example\0");
    query.extend_from_slice(&case.qtype.to_be_bytes());
    query.extend_from_slice(&[0, 1]);
    for _ in 0..case.repeat {
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&query, (Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut wire = vec![0; 65535];
        let len = timeout(Duration::from_secs(7), client.recv(&mut wire))
            .await
            .unwrap()
            .unwrap();
        wire.truncate(len);
        assert_eq!(&wire[..2], &query[..2]);
        assert_ne!(wire[3] & 0x80, 0, "RA must be set");
        assert_eq!(wire[3] & 15, if case.expected.is_some() { 0 } else { 2 });
        if let Some(source) = case.expected {
            assert_eq!(wire[2] & 4 != 0, source == 0, "wrong source");
            let kind = if source == 0 { case.china } else { case.trust }.unwrap();
            let mut expected = reply(&query, source, kind, case.qtype);
            dns::set_ra(&mut expected).unwrap();
            if !matches!(kind, Reply::Tc) {
                dns::truncate_additional(&mut expected).unwrap();
            }
            assert_eq!(
                wire, expected,
                "preserve selected wire except ID/RA/additional"
            );
        }
    }
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), case.ip_calls * case.repeat);
    assert!(
        calls
            .iter()
            .all(|ip| *ip == "10.0.0.1".parse::<IpAddr>().unwrap())
    );
    let counts = hits.lock().unwrap().clone();
    let expected_hits = vec![
        usize::from(case.china.is_some()) * case.repeat,
        usize::from(case.trust.is_some()) * case.repeat,
        usize::from(case.china.is_some() && case.extra_upstreams) * case.repeat,
        usize::from(case.trust.is_some() && case.extra_upstreams) * case.repeat,
    ];
    assert_eq!(counts, expected_hits);
    eprintln!(
        "hits={counts:?}; IP tests={calls:?}; selected={:?}; repeat={}",
        case.expected, case.repeat
    );
    shutdown.send_replace(true);
    serving.await.unwrap().unwrap();
    for worker in workers {
        worker.abort();
    }
}

#[tokio::test]
async fn china_first_ip_in_set_selects_china() {
    check(Case {
        member: true,
        expected: Some(0),
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_non_china_selects_trust() {
    check(Case::default()).await;
}
#[tokio::test]
async fn ip_test_failure_selects_trust() {
    check(Case {
        member: true,
        test_failure: true,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_no_ip_tc_zero_selects_trust() {
    check(Case {
        china: Some(Reply::NoIp),
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_tc_selects_china_without_ip_test() {
    check(Case {
        china: Some(Reply::Tc),
        expected: Some(0),
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn trust_first_is_held_for_china_member() {
    check(Case {
        trust_delay: 10,
        china_delay: 90,
        member: true,
        expected: Some(0),
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn trust_first_is_used_for_non_china() {
    check(Case {
        trust_delay: 10,
        china_delay: 90,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_empty_uses_all_trust() {
    check(Case {
        china: None,
        ip_calls: 0,
        extra_upstreams: true,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn trust_empty_uses_china_without_ip_test() {
    check(Case {
        trust: None,
        expected: Some(0),
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn both_failed_servfail() {
    check(Case {
        china: Some(Reply::Fail),
        trust: Some(Reply::Fail),
        expected: None,
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn both_empty_servfail() {
    check(Case {
        china: None,
        trust: None,
        expected: None,
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn aaaa_without_v6_selects_trust_even_with_true_fake() {
    check(Case {
        qtype: 28,
        member: true,
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn non_a_first_legal_response_from_all_upstreams() {
    check(Case {
        qtype: 15,
        trust_delay: 10,
        china_delay: 90,
        ip_calls: 0,
        extra_upstreams: true,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_failure_uses_trust() {
    check(Case {
        china: Some(Reply::Fail),
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn trust_failure_does_not_hide_legal_china() {
    check(Case {
        trust: Some(Reply::Fail),
        expected: Some(0),
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn china_timeout_releases_stashed_trust() {
    check(Case {
        china: Some(Reply::Timeout),
        ip_calls: 0,
        ..Case::default()
    })
    .await;
}
#[tokio::test]
async fn repeated_none_query_is_not_a_verdict_cache() {
    check(Case {
        member: true,
        expected: Some(0),
        repeat: 2,
        ..Case::default()
    })
    .await;
}
