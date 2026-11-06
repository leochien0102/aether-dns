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

use config::{Config, Group, Tag, Upstream};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::{Duration, sleep, timeout};

static SERIAL: AtomicUsize = AtomicUsize::new(0);

struct Files(std::path::PathBuf);
impl Files {
    fn new() -> Self {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "target/routing-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn list(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().into()
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn response(query: &[u8]) -> Vec<u8> {
    assert_ne!(&query[..2], &[0x42, 0x42], "upstream ID rewritten");
    let mut result = query.to_vec();
    result[2] = 0x81;
    result[3] = 0; // server must set RA
    result[11] = 1;
    result.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    result
}

// Five separately observed endpoints: china, trust, group0, group1, second trust.
async fn route_case(
    name: &str,
    default: Option<Tag>,
    tcp: bool,
    expected: Option<usize>,
    empty: Option<usize>,
    remove_lists: bool,
) {
    let second_trust =
        expected == Some(1) && (name.ends_with("gfw.example") || matches!(default, Some(Tag::Gfw)));
    let files = Files::new();
    let hits = Arc::new(Mutex::new(vec![0usize; 5]));
    let mut workers = Vec::new();
    let mut endpoints = Vec::new();
    for i in 0..5 {
        // UDP and TCP have separate port namespaces; bind both before use and
        // retry when another process owns the TCP side of the chosen port.
        let (socket, listener) = loop {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let port = socket.local_addr().unwrap().port();
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
                Ok(listener) => break (socket, listener),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(error) => panic!("TCP bind: {error}"),
            }
        };
        let port = socket.local_addr().unwrap().port();
        endpoints.push(Upstream {
            protocol: None,
            host: None,
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: Some(port),
        });
        let hits = hits.clone();
        workers.push(tokio::spawn(async move {
            let mut buf = vec![0; 65535];
            loop {
                tokio::select! {
                    received = socket.recv_from(&mut buf) => {
                        let (len, peer) = received.unwrap();
                        hits.lock().unwrap()[i] += 1;
                        socket.send_to(&response(&buf[..len]), peer).await.unwrap();
                    }
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let len = stream.read_u16().await.unwrap();
                        let mut query = vec![0; len as usize];
                        stream.read_exact(&mut query).await.unwrap();
                        hits.lock().unwrap()[i] += 1;
                        let reply = response(&query);
                        stream.write_u16(reply.len() as u16).await.unwrap();
                        stream.write_all(&reply).await.unwrap();
                    }
                }
            }
        }));
    }
    let mut config = Config {
        bind_addr: vec![Ipv4Addr::LOCALHOST],
        bind_port: vec![],
        china_dns: vec![endpoints[0].clone()],
        trust_dns: vec![endpoints[1].clone(), endpoints[4].clone()],
        gfwlist_file: vec![files.list("gfw", "gfw.example\nshared.example\n")],
        chnlist_file: vec![files.list("chn", "chn.example\nshared.example\n")],
        default_tag: default,
        groups: (0..2)
            .map(|i| Group {
                name: format!("g{i}"),
                dnl: vec![files.list(
                    &format!("g{i}"),
                    &format!("group{i}.example\nshared.example\n"),
                )],
                upstream: vec![endpoints[i + 2].clone()],
                ipset: None,
            })
            .collect(),
        ..Config::default()
    };
    // Missing files must not prevent startup or discard the other lists.
    config
        .gfwlist_file
        .push(files.0.join("missing").to_str().unwrap().into());
    match empty {
        Some(0) => config.china_dns.clear(),
        Some(1) => config.trust_dns.clear(),
        Some(i) => config.groups[i - 2].upstream.clear(),
        None => {}
    }
    let (port, shutdown, serving) = in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        async move {
            server::run(config, receiver)
                .await
                .map_err(|error| error.to_string())
        }
    })
    .await;
    if remove_lists {
        std::fs::remove_dir_all(&files.0).unwrap();
    }
    let mut query = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    let reply = timeout(Duration::from_secs(2), async {
        if tcp {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            stream.write_u16(query.len() as u16).await.unwrap();
            stream.write_all(&query).await.unwrap();
            let len = stream.read_u16().await.unwrap();
            let mut reply = vec![0; len as usize];
            stream.read_exact(&mut reply).await.unwrap();
            reply
        } else {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            socket
                .send_to(&query, (Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let mut reply = vec![0; 65535];
            let len = socket.recv(&mut reply).await.unwrap();
            reply.truncate(len);
            reply
        }
    })
    .await
    .unwrap();
    assert_eq!(&reply[..2], &query[..2]);
    assert_ne!(reply[3] & 0x80, 0, "RA set");
    assert_eq!(reply[3] & 15, if expected.is_some() { 0 } else { 2 });
    assert_eq!(&reply[10..12], &[0, 0], "additional stripped");
    assert_eq!(&reply[12..], &query[12..], "question preserved");
    sleep(Duration::from_millis(30)).await;
    let counts = hits.lock().unwrap().clone();
    for (i, count) in counts.iter().enumerate() {
        assert_eq!(
            *count,
            usize::from(expected == Some(i) || (i == 4 && second_trust)),
            "{name}: endpoint {i}, counts={counts:?}"
        );
    }
    eprintln!(
        "{name} client={} -> {expected:?}; hits={counts:?}",
        if tcp { "TCP" } else { "UDP" }
    );
    shutdown.send_replace(true);
    serving.await.unwrap().unwrap();
    for worker in workers {
        worker.abort();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn gfw_routes_trust_udp() {
    route_case("q.gfw.example", None, false, Some(1), None, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn chn_routes_china_tcp() {
    route_case("q.chn.example", None, true, Some(0), None, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn group0_routes_own_upstream() {
    route_case("q.group0.example", None, false, Some(2), None, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn group1_routes_own_upstream_tcp() {
    route_case("q.group1.example", None, true, Some(3), None, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn default_chn_routes_china() {
    route_case(
        "unlisted.example",
        Some(Tag::Chn),
        false,
        Some(0),
        None,
        false,
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn default_gfw_routes_trust() {
    route_case(
        "unlisted.example",
        Some(Tag::Gfw),
        false,
        Some(1),
        None,
        false,
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn none_uses_china_when_trust_empty() {
    // M2-05: an absent trust group must not prevent a legal china reply.
    // Dual-group decisions and all-upstream hit counts are covered in none.rs.
    route_case(
        "unlisted.example",
        Some(Tag::None),
        false,
        Some(0),
        Some(1),
        false,
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn lists_loaded_once_and_later_group_wins() {
    route_case("q.shared.example", None, false, Some(3), None, true).await;
}
#[tokio::test(flavor = "current_thread")]
async fn empty_custom_group_servfail_without_fallback() {
    route_case("q.group0.example", None, false, None, Some(2), false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn missing_china_servfail_without_startup_failure() {
    route_case("q.chn.example", None, true, None, Some(0), false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn missing_trust_servfail_without_startup_failure() {
    route_case("q.gfw.example", None, false, None, Some(1), false).await;
}
