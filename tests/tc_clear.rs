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

use config::{Config, Tag, Upstream};
use server::ip_test::IpTest;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep, timeout};

struct NoIpTest;
impl IpTest for NoIpTest {
    fn is_china(&self, _: IpAddr) -> bool {
        panic!("gfw routing and query filtering do not need IP tests");
    }
}

async fn check(tcp: bool, qtype: u16, no_ipv6: bool, filter: bool) {
    // Keep both transports reserved at the fake upstream's port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, upstream_port))
        .await
        .unwrap();
    let (capture, mut captured) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        loop {
            let mut wire = vec![0; 65535];
            tokio::select! {
                received = socket.recv_from(&mut wire) => {
                    let (len, peer) = received.unwrap();
                    wire.truncate(len);
                    capture.send(wire.clone()).unwrap();
                    dns::synthetic_nodata(&mut wire).unwrap();
                    wire[2] |= 4; // AA distinguishes upstream from local NODATA.
                    socket.send_to(&wire, peer).await.unwrap();
                }
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    let len = stream.read_u16().await.unwrap() as usize;
                    wire.resize(len, 0);
                    stream.read_exact(&mut wire).await.unwrap();
                    capture.send(wire.clone()).unwrap();
                    dns::synthetic_nodata(&mut wire).unwrap();
                    wire[2] |= 4;
                    stream.write_u16(wire.len() as u16).await.unwrap();
                    stream.write_all(&wire).await.unwrap();
                }
            }
        }
    });
    // Reserve a port available for both server transports, without depending on
    // independent ephemeral TCP/UDP allocations choosing different ports.
    let (reservation, udp_reservation) = loop {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        if let Ok(udp) = UdpSocket::bind(tcp.local_addr().unwrap()).await {
            break (tcp, udp);
        }
    };
    let port = reservation.local_addr().unwrap().port();
    let config = Config {
        bind_addr: vec![Ipv4Addr::LOCALHOST],
        bind_port: vec![port],
        default_tag: Some(Tag::Gfw),
        trust_dns: vec![Upstream {
            protocol: None,
            host: None,
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: Some(upstream_port),
        }],
        no_ipv6: usize::from(no_ipv6),
        filter_qtype: if filter { vec![qtype] } else { vec![] }
            .into_iter()
            .collect(),
        ..Config::default()
    };
    drop((reservation, udp_reservation));
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(async move {
        server::run_with_ip_test(config, receiver, Arc::new(NoIpTest))
            .await
            .map_err(|e| e.to_string())
    });
    timeout(Duration::from_secs(2), async {
        loop {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
            {
                break;
            }
            assert!(!serving.is_finished());
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    // TC + RD + CD, mixed-case question and OPT: only TC and the existing
    // upstream ID rewrite may change on the forwarded query.
    let mut query = vec![0xbe, 0xef, 3, 0x10, 0, 1, 0, 0, 0, 0, 0, 1];
    query.extend_from_slice(b"\x02Tc\x07example\0");
    query.extend_from_slice(&qtype.to_be_bytes());
    query.extend_from_slice(&[0, 1]);
    let question_end = query.len();
    query.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
    let response = timeout(Duration::from_secs(2), async {
        if tcp {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            stream.write_u16(query.len() as u16).await.unwrap();
            stream.write_all(&query).await.unwrap();
            let len = stream.read_u16().await.unwrap() as usize;
            let mut wire = vec![0; len];
            stream.read_exact(&mut wire).await.unwrap();
            wire
        } else {
            let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            client.connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
            client.send(&query).await.unwrap();
            let mut wire = vec![0; 65535];
            let len = client.recv(&mut wire).await.unwrap();
            wire.truncate(len);
            wire
        }
    })
    .await
    .unwrap();
    shutdown.send(true).unwrap();
    timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let blocked = no_ipv6 || filter;
    let mut expected = query[..question_end].to_vec();
    expected[2] = 0x81 | if blocked { 0 } else { 4 };
    expected[3] = 0x90;
    expected[6..12].fill(0);
    assert_eq!(
        response, expected,
        "TC cleared, ID/question/RD/CD preserved; NODATA"
    );
    let transport = if tcp { "TCP" } else { "UDP" };
    if blocked {
        assert!(matches!(
            captured.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        eprintln!(
            "{transport} qtype={qtype} no-ipv6={no_ipv6} filter-qtype={filter}: input=0x0310 upstream=none response=0x8190 hits=0 ID=0xbeef/question preserved"
        );
    } else {
        let forwarded = captured.try_recv().expect("fake upstream captured query");
        assert_ne!(&forwarded[..2], &query[..2], "existing upstream ID rewrite");
        let mut expected = query.clone();
        expected[..2].copy_from_slice(&forwarded[..2]);
        expected[2] &= !2;
        assert_eq!(forwarded, expected, "upstream query changes only ID and TC");
        assert!(matches!(
            captured.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        eprintln!(
            "{transport} qtype={qtype}: input=0x0310 upstream=0x{:04x} response=0x8590 hits=1 client ID=0xbeef restored/question preserved",
            u16::from_be_bytes([forwarded[2], forwarded[3]])
        );
    }
    worker.abort();
    let _ = worker.await;
}

macro_rules! cases {
    ($udp:ident, $tcp:ident, $qtype:expr, $no_ipv6:expr, $filter:expr) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $udp() {
            check(false, $qtype, $no_ipv6, $filter).await;
        }
        #[tokio::test(flavor = "current_thread")]
        async fn $tcp() {
            check(true, $qtype, $no_ipv6, $filter).await;
        }
    };
}
cases!(a_udp, a_tcp, 1, false, false);
cases!(aaaa_udp, aaaa_tcp, 28, false, false);
cases!(no_ipv6_udp, no_ipv6_tcp, 28, true, false);
cases!(filter_qtype_udp, filter_qtype_tcp, 1, false, true);
