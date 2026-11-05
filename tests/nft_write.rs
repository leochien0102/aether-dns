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

use config::{Config, Group, SetName, SetPair, Tag, Upstream};
use lists::RouteTag;
use server::ip_writer::{IpWriter, write_reply};
use std::{
    io,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use tokio::{
    net::{TcpListener, UdpSocket},
    time::{Duration, timeout},
};

#[derive(Default)]
struct FakeWriter {
    calls: Mutex<Vec<(SetName, Vec<Ipv4Addr>)>>,
    fail: bool,
}
impl IpWriter for FakeWriter {
    fn add(&self, set: &SetName, ips: &[Ipv4Addr]) -> io::Result<()> {
        self.calls.lock().unwrap().push((set.clone(), ips.to_vec()));
        if self.fail {
            Err(io::Error::other("missing nft table"))
        } else {
            Ok(())
        }
    }
}
fn set(name: &str) -> SetName {
    SetName {
        family: "inet".into(),
        table: "aetherproxy".into(),
        set: name.into(),
    }
}
fn pair(name: &str) -> Option<SetPair> {
    Some(SetPair {
        v4: Some(set(name)),
        v6: None,
    })
}
fn config() -> Config {
    Config {
        add_taggfw_ip: pair("gfw4"),
        ipset_name4: Some(set("chnroute4")),
        groups: vec![
            Group {
                ipset: pair("uproxy4"),
                ..Group::default()
            },
            Group {
                ipset: pair("udirect4"),
                ..Group::default()
            },
            Group::default(),
        ],
        ..Config::default()
    }
}
fn query() -> Vec<u8> {
    vec![
        0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'x', 0, 0, 1, 0, 1,
    ]
}
fn reply(ips: &[&str], code: u8, tc: bool) -> Vec<u8> {
    let mut wire = query();
    wire[2] = 0x81 | if tc { 2 } else { 0 };
    wire[3] = code;
    wire[6..8].copy_from_slice(&(ips.len() as u16).to_be_bytes());
    for ip in ips {
        let (kind, data) = match ip.parse::<IpAddr>().unwrap() {
            IpAddr::V4(ip) => (1u16, ip.octets().to_vec()),
            IpAddr::V6(ip) => (28u16, ip.octets().to_vec()),
        };
        wire.extend_from_slice(&[0xc0, 0x0c]);
        wire.extend_from_slice(&kind.to_be_bytes());
        wire.extend_from_slice(&[0, 1, 0, 0, 0, 30]);
        wire.extend_from_slice(&(data.len() as u16).to_be_bytes());
        wire.extend_from_slice(&data);
    }
    wire
}
fn check(
    config: Config,
    tag: RouteTag,
    ips: &[&str],
    code: u8,
    tc: bool,
    expected: Option<(&str, &[&str])>,
) {
    let writer = FakeWriter::default();
    write_reply(&config, tag, &reply(ips, code, tc), &writer);
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(name, ips)| {
            (
                set(name),
                ips.iter()
                    .map(|ip| ip.parse().unwrap())
                    .collect::<Vec<Ipv4Addr>>(),
            )
        })
        .collect();
    assert_eq!(*writer.calls.lock().unwrap(), expected);
}
#[test]
fn gfw() {
    check(
        config(),
        RouteTag::Gfw,
        &["1.2.3.4"],
        0,
        false,
        Some(("gfw4", &["1.2.3.4"])),
    );
}
#[test]
fn group_proxy() {
    check(
        config(),
        RouteTag::Group(0),
        &["1.2.3.4"],
        0,
        false,
        Some(("uproxy4", &["1.2.3.4"])),
    );
}
#[test]
fn group_direct() {
    check(
        config(),
        RouteTag::Group(1),
        &["1.2.3.4"],
        0,
        false,
        Some(("udirect4", &["1.2.3.4"])),
    );
}
#[test]
fn chn_no_write() {
    check(config(), RouteTag::Chn, &["1.2.3.4"], 0, false, None);
}
#[test]
fn none_no_write() {
    check(config(), RouteTag::None, &["1.2.3.4"], 0, false, None);
}
#[test]
fn all_a_deduplicated() {
    check(
        config(),
        RouteTag::Gfw,
        &["1.2.3.4", "5.6.7.8", "1.2.3.4"],
        0,
        false,
        Some(("gfw4", &["1.2.3.4", "5.6.7.8"])),
    );
}
#[test]
fn blacklist() {
    check(
        config(),
        RouteTag::Gfw,
        &[
            "0.0.0.0",
            "0.255.1.2",
            "127.0.0.1",
            "127.255.1.2",
            "::",
            "::1",
            "1.2.3.4",
        ],
        0,
        false,
        Some(("gfw4", &["1.2.3.4"])),
    );
}
#[test]
fn aaaa_null() {
    check(
        config(),
        RouteTag::Gfw,
        &["2001:db8::1", "::", "::1"],
        0,
        false,
        None,
    );
}
#[test]
fn tc_no_write() {
    check(config(), RouteTag::Gfw, &["1.2.3.4"], 0, true, None);
}
#[test]
fn servfail_no_write() {
    check(config(), RouteTag::Gfw, &["1.2.3.4"], 2, false, None);
}
#[test]
fn nxdomain_good() {
    check(
        config(),
        RouteTag::Gfw,
        &["1.2.3.4"],
        3,
        false,
        Some(("gfw4", &["1.2.3.4"])),
    );
}
#[test]
fn missing_sets() {
    let mut config = config();
    config.add_taggfw_ip = None;
    check(config.clone(), RouteTag::Gfw, &["1.2.3.4"], 0, false, None);
    for i in [2, 99] {
        check(
            config.clone(),
            RouteTag::Group(i),
            &["1.2.3.4"],
            0,
            false,
            None,
        );
    }
    config.add_taggfw_ip = Some(SetPair { v4: None, v6: None });
    check(config.clone(), RouteTag::Gfw, &["1.2.3.4"], 0, false, None);
    config.add_taggfw_ip = pair("");
    check(config, RouteTag::Gfw, &["1.2.3.4"], 0, false, None);
}
#[test]
fn empty_answers() {
    check(config(), RouteTag::Gfw, &[], 0, false, None);
}

struct NoChina;
impl server::ip_test::IpTest for NoChina {
    fn is_china(&self, _: IpAddr) -> bool {
        false
    }
}

async fn forwarding(writer_fails: bool, tcp: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fake_upstream = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = fake_upstream.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let mut buffer = [0; 512];
        let (_, peer) = fake_upstream.recv_from(&mut buffer).await.unwrap();
        let mut response = reply(&["1.2.3.4", "5.6.7.8", "1.2.3.4"], 0, false);
        response[..2].copy_from_slice(&buffer[..2]);
        fake_upstream.send_to(&response, peer).await.unwrap();
    });
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let mut config = config();
    config.default_tag = Some(Tag::Gfw);
    config.bind_port = vec![port];
    config.trust_dns = vec![Upstream {
        protocol: Some(config::Protocol::Udp),
        host: None,
        ip: address.ip(),
        port: Some(address.port()),
    }];
    let writer = Arc::new(FakeWriter {
        fail: writer_fails,
        ..FakeWriter::default()
    });
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let injected = writer.clone();
    let serving = tokio::spawn(async move {
        server::run_with_ip_writer(config, receiver, Arc::new(NoChina), injected)
            .await
            .map_err(|e| e.to_string())
    });
    timeout(Duration::from_secs(2), async {
        loop {
            if tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
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
    let response = timeout(Duration::from_secs(2), async {
        if tcp {
            let mut client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            client.write_u16(query().len() as u16).await.unwrap();
            client.write_all(&query()).await.unwrap();
            let mut wire = vec![0; client.read_u16().await.unwrap() as usize];
            client.read_exact(&mut wire).await.unwrap();
            wire
        } else {
            let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            client
                .send_to(&query(), (Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let mut wire = vec![0; 512];
            let (len, _) = client.recv_from(&mut wire).await.unwrap();
            wire.truncate(len);
            wire
        }
    })
    .await
    .unwrap();
    let mut expected = reply(&["1.2.3.4", "5.6.7.8", "1.2.3.4"], 0, false);
    dns::set_ra(&mut expected).unwrap();
    assert_eq!(
        response, expected,
        "writer outcome must not change any response byte"
    );
    assert_eq!(
        *writer.calls.lock().unwrap(),
        vec![(
            set("gfw4"),
            vec![
                "1.2.3.4".parse::<Ipv4Addr>().unwrap(),
                "5.6.7.8".parse().unwrap()
            ]
        )]
    );
    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    worker.await.unwrap();
}
#[tokio::test]
async fn udp_writer_error_preserves_response() {
    forwarding(true, false).await;
}
#[tokio::test]
async fn tcp_writer_error_preserves_response() {
    forwarding(true, true).await;
}
#[tokio::test]
async fn udp_success_writes() {
    forwarding(false, false).await;
}
