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
use server::ip_test::IpTest;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::{Duration, sleep, timeout};

struct NoIpTest;
impl IpTest for NoIpTest {
    fn is_china(&self, _: IpAddr) -> bool {
        panic!("query filtering must not perform IP tests");
    }
}

async fn check(tcp: bool, qtype: u16, no_ipv6: bool, filter: Vec<u16>, blocked: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let worker = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let len = stream.read_u16().await.unwrap();
            let mut wire = vec![0; len as usize];
            stream.read_exact(&mut wire).await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            // AA distinguishes a forwarded reply from local NODATA.
            dns::synthetic_nodata(&mut wire).unwrap();
            wire[2] |= 4;
            stream.write_u16(wire.len() as u16).await.unwrap();
            stream.write_all(&wire).await.unwrap();
        }
    });
    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = reservation.local_addr().unwrap().port();
    let config = Config {
        bind_addr: vec![Ipv4Addr::LOCALHOST],
        bind_port: vec![port],
        default_tag: Some(Tag::None),
        trust_dns: vec![Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: Some(upstream_port),
        }],
        no_ipv6: usize::from(no_ipv6),
        filter_qtype: filter.into_iter().collect(),
        ..Config::default()
    };
    drop(reservation);
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

    // RD/CD and mixed-case question must survive unchanged. Include OPT so
    // local filtering must remove actual additional bytes, not just ARCOUNT.
    let mut query = vec![0xbe, 0xef, 1, 0x10, 0, 1, 0, 0, 0, 0, 0, 1];
    query.extend_from_slice(b"\x06FiLtEr\x07example\0");
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
            let len = stream.read_u16().await.unwrap();
            let mut wire = vec![0; len as usize];
            stream.read_exact(&mut wire).await.unwrap();
            let mut extra = [0];
            assert_eq!(
                stream.read(&mut extra).await.unwrap(),
                0,
                "one frame then EOF"
            );
            wire
        } else {
            let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            client.connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
            client.send(&query).await.unwrap();
            let mut wire = vec![0; 65535];
            let len = client.recv(&mut wire).await.unwrap();
            wire.truncate(len);
            assert!(
                timeout(Duration::from_millis(50), client.recv(&mut [0; 512]))
                    .await
                    .is_err(),
                "exactly one UDP response"
            );
            wire
        }
    })
    .await
    .unwrap();
    let mut expected = query[..question_end].to_vec();
    expected[2] |= 0x80 | if blocked { 0 } else { 4 };
    expected[3] |= 0x80;
    expected[6..12].fill(0);
    assert_eq!(
        response, expected,
        "ID/question/flags and QR/RA/qd=1/an=ns=ar=0"
    );
    shutdown.send(true).unwrap();
    timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Server has drained: no query can still arrive after this count assertion.
    let count = hits.load(Ordering::SeqCst);
    assert_eq!(count, usize::from(!blocked));
    eprintln!(
        "{} qtype={qtype} {} upstream_hits={count}",
        if tcp { "TCP" } else { "UDP" },
        if blocked { "filtered" } else { "forwarded" }
    );
    worker.abort();
    let _ = worker.await;
}

macro_rules! cases {
    ($udp:ident, $tcp:ident, $qtype:expr, $no_ipv6:expr, $filter:expr, $blocked:expr) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $udp() {
            check(false, $qtype, $no_ipv6, $filter, $blocked).await;
        }
        #[tokio::test(flavor = "current_thread")]
        async fn $tcp() {
            check(true, $qtype, $no_ipv6, $filter, $blocked).await;
        }
    };
}
cases!(aaaa_filtered_udp, aaaa_filtered_tcp, 28, true, vec![], true);
cases!(
    https_filtered_udp,
    https_filtered_tcp,
    65,
    false,
    vec![65],
    true
);
cases!(a_allowed_udp, a_allowed_tcp, 1, true, vec![65], false);
cases!(
    aaaa_allowed_udp,
    aaaa_allowed_tcp,
    28,
    false,
    vec![65],
    false
);
cases!(
    https_allowed_udp,
    https_allowed_tcp,
    65,
    true,
    vec![],
    false
);
cases!(
    arbitrary_filtered_udp,
    arbitrary_filtered_tcp,
    65280,
    false,
    vec![65, 65280],
    true
);
