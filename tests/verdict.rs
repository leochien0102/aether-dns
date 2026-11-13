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
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::{Duration, sleep, timeout};

struct FakeIpTest {
    member: bool,
    calls: Mutex<usize>,
}
impl IpTest for FakeIpTest {
    fn is_china(&self, _: IpAddr) -> bool {
        *self.calls.lock().unwrap() += 1;
        self.member
    }
}

#[derive(Clone, Copy, Debug)]
enum Reply {
    Ip,
    NoIp,
    Tc,
    Other,
    Fail,
    Timeout,
}
fn reply(query: &[u8], side: usize, kind: Reply) -> Vec<u8> {
    let mut wire = query.to_vec();
    wire[2] = 0x81 | if side == 0 { 4 } else { 0 };
    wire[3] = 0;
    match kind {
        Reply::Tc => wire[2] |= 2,
        Reply::Other => wire[3] = 2,
        Reply::Ip => {
            wire[7] = 1;
            let end = dns::skip_name(query, 12).unwrap();
            let v6 = query[end..end + 2] == [0, 28];
            wire.extend_from_slice(&[0xc0, 12, 0, if v6 { 28 } else { 1 }, 0, 1, 0, 0, 0, 30]);
            if v6 {
                wire.extend_from_slice(&[0, 16]);
                wire.extend_from_slice(&[0; 16]);
            } else {
                wire.extend_from_slice(&[0, 4, 10, 0, side as u8, 1]);
            }
        }
        _ => {}
    }
    wire
}

struct Step {
    name: &'static str,
    qtype: u16,
    class: u16,
    tcp: bool,
    modes: [Reply; 2],
    selected: Option<usize>,
    hits: [usize; 2],
    tests: usize,
}
impl Step {
    fn new(selected: usize, hits: [usize; 2], tests: usize) -> Self {
        Self {
            name: "verdict",
            qtype: 1,
            class: 1,
            tcp: false,
            modes: [Reply::Ip; 2],
            selected: Some(selected),
            hits,
            tests,
        }
    }
}

async fn check(capacity: Option<u64>, member: bool, upstream_tcp: bool, steps: Vec<Step>) {
    let hits = Arc::new(Mutex::new([0usize; 2]));
    let modes = Arc::new(Mutex::new([Reply::Ip; 2]));
    let fake = Arc::new(FakeIpTest {
        member,
        calls: Mutex::new(0),
    });
    let mut config = Config {
        bind_addr: vec![Ipv4Addr::LOCALHOST],
        default_tag: Some(Tag::None),
        verdict_cache: capacity,
        ..Config::default()
    };
    let mut workers = Vec::new();
    for side in 0..2 {
        // Bind UDP and TCP atomically for the fake upstream's chosen port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let socket = UdpSocket::bind(address).await.unwrap();
        let endpoint = Upstream {
            protocol: Some(if upstream_tcp {
                Protocol::Tcp
            } else {
                Protocol::Udp
            }),
            host: None,
            ip: address.ip(),
            port: Some(address.port()),
        };
        if side == 0 {
            config.china_dns.push(endpoint);
        } else {
            config.trust_dns.push(endpoint);
        }
        let hits = hits.clone();
        let modes = modes.clone();
        workers.push(tokio::spawn(async move {
            let mut pending = tokio::task::JoinSet::new();
            let mut buffer = vec![0; 65535];
            loop {
                tokio::select! {
                    _ = pending.join_next(), if !pending.is_empty() => {},
                    accepted = listener.accept(), if upstream_tcp => {
                        let (mut stream, _) = accepted.unwrap();
                        let hits = hits.clone();
                        let modes = modes.clone();
                        pending.spawn(async move {
                            let len = stream.read_u16().await.unwrap();
                            let mut query = vec![0; len as usize];
                            stream.read_exact(&mut query).await.unwrap();
                            hits.lock().unwrap()[side] += 1;
                            let mode = modes.lock().unwrap()[side];
                            sleep(Duration::from_millis(20 + side as u64 * 40)).await;
                            match mode {
                                Reply::Fail => return,
                                Reply::Timeout => { sleep(Duration::from_secs(6)).await; return; }
                                _ => {}
                            }
                            let wire = reply(&query, side, mode);
                            if stream.write_u16(wire.len() as u16).await.is_ok() {
                                let _ = stream.write_all(&wire).await;
                            }
                        });
                    }
                    received = socket.recv_from(&mut buffer), if !upstream_tcp => {
                        let (len, peer) = received.unwrap();
                        hits.lock().unwrap()[side] += 1;
                        let mode = modes.lock().unwrap()[side];
                        let wire = reply(&buffer[..len], side, mode);
                        sleep(Duration::from_millis(20 + side as u64 * 40)).await;
                        if !matches!(mode, Reply::Fail | Reply::Timeout) {
                            socket.send_to(&wire, peer).await.unwrap();
                        }
                    }
                }
            }
        }));
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
    let address = (Ipv4Addr::LOCALHOST, port);
    for (index, step) in steps.into_iter().enumerate() {
        *modes.lock().unwrap() = step.modes;
        let mut query = vec![0x42, index as u8, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        query.push(step.name.len() as u8);
        query.extend_from_slice(step.name.as_bytes());
        query.extend_from_slice(b"\x07example\0");
        query.extend_from_slice(&step.qtype.to_be_bytes());
        query.extend_from_slice(&step.class.to_be_bytes());
        let wire = timeout(Duration::from_secs(7), async {
            if step.tcp {
                let mut stream = TcpStream::connect(address).await.unwrap();
                stream.write_u16(query.len() as u16).await.unwrap();
                stream.write_all(&query).await.unwrap();
                let len = stream.read_u16().await.unwrap();
                let mut wire = vec![0; len as usize];
                stream.read_exact(&mut wire).await.unwrap();
                wire
            } else {
                let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                socket.send_to(&query, address).await.unwrap();
                let mut wire = vec![0; 65535];
                let len = socket.recv(&mut wire).await.unwrap();
                wire.truncate(len);
                wire
            }
        })
        .await
        .unwrap();
        assert_eq!(&wire[..2], &query[..2]);
        assert_eq!(wire[3] & 15, if step.selected.is_some() { 0 } else { 2 });
        if let Some(side) = step.selected {
            assert_eq!(wire[2] & 4 != 0, side == 0, "step {index}: wrong source");
        }
        assert_eq!(
            *hits.lock().unwrap(),
            step.hits,
            "step {index}: upstream hits"
        );
        assert_eq!(
            *fake.calls.lock().unwrap(),
            step.tests,
            "step {index}: IP tests"
        );
        eprintln!(
            "step={index} name={} qtype={} client={} selected={:?} hits={:?} IP-tests={} capacity={capacity:?}",
            step.name,
            step.qtype,
            if step.tcp { "TCP" } else { "UDP" },
            step.selected,
            step.hits,
            step.tests
        );
    }
    shutdown.send_replace(true);
    serving.await.unwrap().unwrap();
    for worker in workers {
        worker.abort();
    }
}

#[tokio::test]
async fn true_then_only_china_udp() {
    check(
        Some(4),
        true,
        false,
        vec![Step::new(0, [1, 1], 1), Step::new(0, [2, 1], 1)],
    )
    .await;
}
#[tokio::test]
async fn false_then_only_trust_tcp() {
    let mut second = Step::new(1, [1, 2], 1);
    second.tcp = true;
    check(Some(4), false, true, vec![Step::new(1, [1, 1], 1), second]).await;
}
#[tokio::test]
async fn qname_key_shares_qtype_class_id_and_transport() {
    let mut second = Step::new(0, [2, 1], 1);
    second.qtype = 28;
    second.class = 3;
    second.tcp = true;
    check(Some(4), true, true, vec![Step::new(0, [1, 1], 1), second]).await;
}
#[tokio::test]
async fn aaaa_false_without_ipv4_ip_test() {
    let mut first = Step::new(1, [1, 1], 0);
    first.qtype = 28;
    check(Some(4), true, true, vec![first, Step::new(1, [1, 2], 0)]).await;
}
#[tokio::test]
async fn non_a_does_not_write() {
    let mut first = Step::new(0, [1, 1], 0);
    first.qtype = 16;
    check(
        Some(4),
        true,
        true,
        vec![first, Step::new(0, [2, 2], 1), Step::new(0, [3, 2], 1)],
    )
    .await;
}
#[tokio::test]
async fn non_a_does_not_read() {
    let mut second = Step::new(0, [2, 2], 1);
    second.qtype = 16;
    check(
        Some(4),
        false,
        true,
        vec![Step::new(1, [1, 1], 1), second, Step::new(1, [2, 3], 1)],
    )
    .await;
}
#[tokio::test]
async fn capacity_one_evicts_old_qname() {
    let mut second = Step::new(0, [2, 2], 2);
    second.name = "another";
    check(
        Some(1),
        true,
        true,
        vec![
            Step::new(0, [1, 1], 1),
            second,
            Step::new(0, [3, 3], 3),
            Step::new(0, [4, 3], 3),
        ],
    )
    .await;
}
#[tokio::test]
async fn missing_capacity_disables() {
    check(
        None,
        true,
        true,
        vec![Step::new(0, [1, 1], 1), Step::new(0, [2, 2], 2)],
    )
    .await;
}
#[tokio::test]
async fn zero_capacity_disables() {
    check(
        Some(0),
        false,
        false,
        vec![Step::new(1, [1, 1], 1), Step::new(1, [2, 2], 2)],
    )
    .await;
}
#[tokio::test]
async fn cached_china_failure_servfail_no_fallback() {
    let mut second = Step::new(0, [2, 1], 1);
    second.modes[0] = Reply::Fail;
    second.selected = None;
    check(Some(4), true, true, vec![Step::new(0, [1, 1], 1), second]).await;
}
#[tokio::test]
async fn cached_trust_timeout_servfail_no_fallback() {
    let mut second = Step::new(1, [1, 2], 1);
    second.modes[1] = Reply::Timeout;
    second.selected = None;
    second.tcp = true;
    check(Some(4), false, true, vec![Step::new(1, [1, 1], 1), second]).await;
}

async fn not_cached(kind: Reply, selected: usize) {
    let mut first = Step::new(selected, [1, 1], 0);
    first.modes[0] = kind;
    check(
        Some(4),
        true,
        true,
        vec![first, Step::new(0, [2, 2], 1), Step::new(0, [3, 2], 1)],
    )
    .await;
}
#[tokio::test]
async fn no_ip_not_cached() {
    not_cached(Reply::NoIp, 1).await;
}
#[tokio::test]
async fn tc_not_cached() {
    not_cached(Reply::Tc, 0).await;
}
#[tokio::test]
async fn other_case_not_cached() {
    not_cached(Reply::Other, 1).await;
}
#[tokio::test]
async fn china_failure_not_cached() {
    not_cached(Reply::Fail, 1).await;
}

#[test]
fn cache_update_does_not_evict_and_wire_key_is_exact() {
    let mut cache = server::verdict::VerdictCache::new(Some(2));
    cache.add(b"\x01a\0", true);
    cache.add(b"\x01b\0", false);
    cache.add(b"\x01a\0", false);
    assert_eq!(cache.get(b"\x01a\0"), Some(false));
    assert_eq!(cache.get(b"\x01b\0"), Some(false));
    assert_eq!(cache.get(b"\x01A\0"), None);
    eprintln!(
        "direct map: capacity=2; update preserves both keys; exact wire key; no upstream/IP tests"
    );
}
