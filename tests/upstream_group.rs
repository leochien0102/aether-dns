#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/dns/mod.rs"]
mod dns;
#[path = "../src/upstream.rs"]
mod upstream;
#[path = "../src/upstream_group.rs"]
mod upstream_group;

use config::{Protocol, Upstream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{Barrier, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const QUERY: &[u8] =
    b"\x42\x42\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x01a\x07example\x00\x00\x01\x00\x01";
static PASSED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
enum Action {
    Reply(u64),
    BadId,
    Invalid,
    Close,
    Silent,
}

struct Peer {
    upstream: Upstream,
    worker: JoinHandle<bool>,
    tcp: bool,
}

fn response(index: usize, action: Action) -> Vec<u8> {
    let mut wire = QUERY.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire[7] = 1;
    wire.extend_from_slice(&[
        0xc0,
        12,
        0,
        1,
        0,
        1,
        0,
        0,
        0,
        30,
        0,
        4,
        10,
        0,
        0,
        index as u8,
    ]);
    match action {
        Action::BadId => wire[0] ^= 1,
        Action::Invalid => wire.truncate(3),
        _ => {}
    }
    wire
}

async fn peer(
    index: usize,
    protocol: Option<Protocol>,
    client: &Protocol,
    action: Action,
    barrier: Arc<Barrier>,
) -> Peer {
    let tcp = protocol.as_ref().unwrap_or(client) == &Protocol::Tcp;
    let (port, worker) = if tcp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let len = stream.read_u16().await.unwrap();
            let mut query = vec![0; len as usize];
            stream.read_exact(&mut query).await.unwrap();
            assert_eq!(query, QUERY, "one unchanged query per upstream");
            // No peer can reply before all peers have received their query.
            // A sequential implementation deadlocks here and fails the deadline.
            barrier.wait().await;
            if matches!(action, Action::Close) {
                return false;
            }
            let mut byte = [0];
            if matches!(action, Action::Silent) {
                assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                return false;
            }
            let delay = match action {
                Action::Reply(ms) => ms,
                _ => 0,
            };
            tokio::select! {
                result = stream.read(&mut byte) => {
                    assert_eq!(result.unwrap(), 0, "losing exchange must close its connection");
                    return false;
                }
                _ = sleep(Duration::from_millis(delay)) => {}
            }
            let wire = response(index, action);
            stream.write_u16(wire.len() as u16).await.unwrap();
            stream.write_all(&wire).await.unwrap();
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0, "exchange closed");
            // There must not be a retry/second connection.
            assert!(
                timeout(Duration::from_millis(10), listener.accept())
                    .await
                    .is_err()
            );
            true
        });
        (port, worker)
    } else {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let worker = tokio::spawn(async move {
            let mut query = [0; 1024];
            let (len, addr) = socket.recv_from(&mut query).await.unwrap();
            assert_eq!(&query[..len], QUERY);
            barrier.wait().await;
            if let Action::Reply(ms) = action {
                sleep(Duration::from_millis(ms)).await;
            }
            socket
                .send_to(&response(index, action), addr)
                .await
                .unwrap();
            true
        });
        (port, worker)
    };
    Peer {
        upstream: Upstream {
            protocol,
            host: None,
            ip: "127.0.0.1".parse().unwrap(),
            port: Some(port),
        },
        worker,
        tcp,
    }
}

async fn run(client: Protocol, specs: Vec<(Option<Protocol>, Action)>, winner: Option<usize>) {
    let barrier = Arc::new(Barrier::new(specs.len()));
    let mut peers = Vec::new();
    for (index, (protocol, action)) in specs.iter().enumerate() {
        peers.push(peer(index, protocol.clone(), &client, *action, barrier.clone()).await);
    }
    let upstreams: Vec<_> = peers.iter().map(|p| p.upstream.clone()).collect();
    let start = Instant::now();
    let result = timeout(
        Duration::from_secs(7),
        upstream_group::exchange(&upstreams, QUERY, client),
    )
    .await
    .expect("group must finish within the single-attempt deadline");
    if let Some(index) = winner {
        assert_eq!(
            result.unwrap(),
            response(index, specs[index].1),
            "exactly the first legal response, not a merge"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a slow/failed peer must not hold up the winner"
        );
    } else {
        assert!(result.is_err(), "no legal reply must fail the group");
        if specs.iter().all(|(_, a)| matches!(a, Action::Silent)) && !specs.is_empty() {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert!(start.elapsed() >= Duration::from_secs(5));
        }
    }
    for (index, peer) in peers.into_iter().enumerate() {
        let sent = timeout(Duration::from_secs(1), peer.worker)
            .await
            .expect("all TCP exchanges must already be cancelled/closed")
            .unwrap();
        if peer.tcp && matches!(specs[index].1, Action::Reply(500)) {
            assert!(!sent, "slow legal peer cancelled before sending its reply");
        }
    }
    let passed = PASSED.fetch_add(1, Ordering::SeqCst) + 1;
    if passed == 11 {
        println!("11/11 group cases passed");
    }
}

macro_rules! case {
    ($name:ident, $client:expr, $specs:expr, $winner:expr) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $name() {
            run($client, $specs, $winner).await;
        }
    };
}

use Action::*;
use Protocol::*;
case!(
    slow_first_fast_second,
    Tcp,
    vec![(None, Reply(500)), (None, Reply(20))],
    Some(1)
);
case!(
    both_legal_first_arrival_not_list_order,
    Tcp,
    vec![(None, Reply(20)), (None, Reply(500))],
    Some(0)
);
case!(
    invalid_first_then_legal,
    Tcp,
    vec![(None, Invalid), (None, Reply(20))],
    Some(1)
);
case!(
    wrong_id_first_then_legal,
    Tcp,
    vec![(None, BadId), (None, Reply(20))],
    Some(1)
);
case!(
    all_failed,
    Tcp,
    vec![(None, Close), (None, Invalid), (None, BadId)],
    None
);
case!(
    silent_peer_does_not_delay_winner,
    Tcp,
    vec![(None, Silent), (None, Reply(20))],
    Some(1)
);
case!(
    all_silent_use_five_second_timeout,
    Tcp,
    vec![(None, Silent), (None, Silent)],
    None
);
case!(
    udp_client_explicit_tcp_and_implicit_udp,
    Udp,
    vec![(Some(Tcp), Reply(500)), (None, Reply(20))],
    Some(1)
);
case!(
    tcp_client_explicit_udp_and_implicit_tcp,
    Tcp,
    vec![(None, Reply(500)), (Some(Udp), Reply(20))],
    Some(1)
);
case!(
    udp_wrong_id_does_not_win,
    Udp,
    vec![(None, BadId), (Some(Tcp), Reply(20))],
    Some(1)
);
case!(empty_group_is_error, Udp, vec![], None);

#[tokio::test(flavor = "current_thread")]
async fn failure_names_every_upstream() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_addr = closed.local_addr().unwrap();
    let silent = peer(0, None, &Tcp, Silent, Arc::new(Barrier::new(1))).await;
    let silent_addr = format!("127.0.0.1:{}", silent.upstream.port.unwrap());
    let refused = Upstream {
        protocol: Some(Tcp),
        host: None,
        ip: closed_addr.ip(),
        port: Some(closed_addr.port()),
    };
    drop(closed);
    // Silent is configured first but finishes last: output must not follow completion order.
    let error = timeout(
        Duration::from_secs(7),
        upstream_group::exchange(&[silent.upstream, refused], QUERY, Tcp),
    )
    .await
    .expect("single-attempt deadline")
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let message = error.to_string();
    assert!(message.starts_with("no upstream answered: "), "{message}");
    let silent_at = message
        .find(&format!("tcp://{silent_addr}: upstream timeout after 5 s"))
        .expect("implicit protocol and silent upstream timeout must be named");
    let refused_at = message
        .find(&format!("; tcp://{closed_addr}: "))
        .expect("explicit protocol and refused upstream must be named");
    assert!(silent_at < refused_at, "configuration order: {message}");
    assert!(
        message[refused_at..]
            .to_ascii_lowercase()
            .contains("refused"),
        "refusal reason: {message}"
    );
    assert!(
        !timeout(Duration::from_secs(1), silent.worker)
            .await
            .expect("silent connection cancelled")
            .unwrap()
    );
}

enum ControlledReply {
    Wire(Vec<u8>),
    Close,
}

fn wire_reply(rcode_low: u8, answer_ip: Option<u8>, ext_rcode: Option<u8>) -> Vec<u8> {
    let mut wire = QUERY.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80 | (rcode_low & 0x0f);
    if let Some(last_octet) = answer_ip {
        wire[7] = 1;
        wire.extend_from_slice(&[
            0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 10, 0, 0, last_octet,
        ]);
    }
    if let Some(ext) = ext_rcode {
        wire[11] = 1;
        // EDNS0 OPT RR: root name (0), TYPE=41, UDP payload=1232, TTL=[ext, ver=0, flags=0, 0], RDLEN=0.
        wire.extend_from_slice(&[0, 0, 41, 4, 208, ext, 0, 0, 0, 0, 0]);
    }
    wire
}

async fn controlled_tcp_peer(
    barrier: Arc<Barrier>,
    release: Option<oneshot::Receiver<()>>,
    done: Option<oneshot::Sender<()>>,
    reply: ControlledReply,
) -> (Upstream, JoinHandle<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let len = stream.read_u16().await.unwrap();
        let mut query = vec![0; len as usize];
        stream.read_exact(&mut query).await.unwrap();
        assert_eq!(query, QUERY);
        barrier.wait().await;
        let mut byte = [0];
        if let Some(release) = release {
            tokio::select! {
                read = stream.read(&mut byte) => {
                    assert_eq!(read.unwrap(), 0, "cancelled peer must observe EOF");
                    return false;
                }
                res = release => {
                    if res.is_err() {
                        return false;
                    }
                }
            }
        }
        let sent = match reply {
            ControlledReply::Wire(wire) => {
                if stream.write_u16(wire.len() as u16).await.is_err() {
                    return false;
                }
                if stream.write_all(&wire).await.is_err() {
                    return false;
                }
                // Wait until the client finishes reading and drops its stream.
                let _ = stream.read(&mut byte).await;
                true
            }
            ControlledReply::Close => {
                drop(stream);
                false
            }
        };
        if let Some(done) = done {
            let _ = done.send(());
        }
        sent
    });
    (
        Upstream {
            protocol: Some(Tcp),
            host: None,
            ip: "127.0.0.1".parse().unwrap(),
            port: Some(port),
        },
        worker,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn fast_servfail_then_slow_noerror_wins() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let servfail = wire_reply(2, None, None);
    let noerror = wire_reply(0, Some(1), None);
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(servfail),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(noerror.clone()),
    )
    .await;
    let result = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("must finish without waiting for timeout")
    .expect("NOERROR should win over earlier SERVFAIL");
    assert_eq!(result, noerror);
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn fast_error_then_slow_nxdomain_wins() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let refused = wire_reply(5, None, None);
    let nxdomain = wire_reply(3, None, None);
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(refused),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(nxdomain.clone()),
    )
    .await;
    let result = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("must finish without waiting for timeout")
    .expect("NXDOMAIN should win over earlier REFUSED");
    assert_eq!(result, nxdomain);
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn fast_extended_rcode_does_not_mask_slow_noerror() {
    for (low, ext) in [(0u8, 1u8), (3u8, 1u8)] {
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = oneshot::channel();
        // Low 4 bits look like NOERROR(0) or NXDOMAIN(3), but OPT extended-RCODE=1 makes it 16 (BADVERS) or 19 (BADMODE).
        let ext_error = wire_reply(low, None, Some(ext));
        let noerror = wire_reply(0, Some(2), Some(0));
        let (u0, w0) = controlled_tcp_peer(
            barrier.clone(),
            None,
            Some(tx),
            ControlledReply::Wire(ext_error),
        )
        .await;
        let (u1, w1) = controlled_tcp_peer(
            barrier,
            Some(rx),
            None,
            ControlledReply::Wire(noerror.clone()),
        )
        .await;
        let result = timeout(
            Duration::from_secs(2),
            upstream_group::exchange(&[u0, u1], QUERY, Tcp),
        )
        .await
        .expect("must finish without waiting for timeout")
        .expect("NOERROR should win over extended-RCODE error");
        assert_eq!(result, noerror, "low={low} ext={ext}");
        assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
        assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn valid_reply_wins_immediately_without_waiting_for_slow_peer() {
    let cases = [
        ("nodata", wire_reply(0, None, None)),
        ("nxdomain", wire_reply(3, None, None)),
        ("noerror-edns0", wire_reply(0, Some(7), Some(0))),
    ];
    for (label, winner) in cases {
        let barrier = Arc::new(Barrier::new(2));
        // Keep _hold alive until after exchange finishes so peer 1 is never released.
        let (_hold, never_release) = oneshot::channel::<()>();
        let (u0, w0) = controlled_tcp_peer(
            barrier.clone(),
            None,
            None,
            ControlledReply::Wire(winner.clone()),
        )
        .await;
        let (u1, w1) = controlled_tcp_peer(
            barrier,
            Some(never_release),
            None,
            ControlledReply::Wire(wire_reply(0, Some(9), None)),
        )
        .await;
        let result = timeout(
            Duration::from_secs(2),
            upstream_group::exchange(&[u0, u1], QUERY, Tcp),
        )
        .await
        .expect("valid reply must not wait for remaining peer")
        .expect("valid reply must succeed");
        assert_eq!(result, winner, "{label}");
        assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
        assert!(
            !timeout(Duration::from_secs(1), w1).await.unwrap().unwrap(),
            "{label}: slow peer must be cancelled before sending"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn all_dns_errors_returns_first_observed_and_single_error_does_not_wait() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let refused = wire_reply(5, None, None);
    let servfail = wire_reply(2, None, None);
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(refused.clone()),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(servfail),
    )
    .await;
    let result = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("all-error group must finish immediately once all peers answer")
    .expect("DNS error reply must be returned as Ok");
    assert_eq!(result, refused, "must return the first observed DNS error");
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());

    // Single upstream returning SERVFAIL finishes immediately with no extra wait.
    let single_barrier = Arc::new(Barrier::new(1));
    let single_servfail = wire_reply(2, None, None);
    let (single_u, single_w) = controlled_tcp_peer(
        single_barrier,
        None,
        None,
        ControlledReply::Wire(single_servfail.clone()),
    )
    .await;
    let single_res = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[single_u], QUERY, Tcp),
    )
    .await
    .expect("single DNS error must not wait")
    .expect("single DNS error must return Ok");
    assert_eq!(single_res, single_servfail);
    assert!(
        timeout(Duration::from_secs(1), single_w)
            .await
            .unwrap()
            .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dns_error_plus_io_failure_returns_dns_error_reply() {
    // Order 1: DNS error arrives first, then second upstream fails with I/O close.
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let servfail = wire_reply(2, None, None);
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(servfail.clone()),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(barrier, Some(rx), None, ControlledReply::Close).await;
    let res1 = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("DNS error + I/O close must finish promptly")
    .expect("DNS error must be returned instead of I/O error");
    assert_eq!(res1, servfail);
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(!timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());

    // Order 2: I/O close happens first, then second upstream replies with extended-RCODE error.
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let badvers = wire_reply(0, None, Some(1));
    let (u0, w0) = controlled_tcp_peer(barrier.clone(), None, Some(tx), ControlledReply::Close).await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(badvers.clone()),
    )
    .await;
    let res2 = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("I/O close + DNS error must finish promptly")
    .expect("DNS error must be returned instead of I/O error");
    assert_eq!(res2, badvers);
    assert!(!timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn all_io_failures_keeps_aggregated_io_error() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let (u0, w0) = controlled_tcp_peer(barrier.clone(), None, Some(tx), ControlledReply::Close).await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(vec![0x42, 0x42, 0x81]), // truncated invalid header -> InvalidData
    )
    .await;
    let addr0 = format!("tcp://127.0.0.1:{}", u0.port.unwrap());
    let addr1 = format!("tcp://127.0.0.1:{}", u1.port.unwrap());
    let error = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("all I/O failures must finish promptly")
    .unwrap_err();
    let message = error.to_string();
    assert!(message.starts_with("no upstream answered: "), "{message}");
    let p0 = message.find(&addr0).expect("first upstream must be named");
    let p1 = message.find(&addr1).expect("second upstream must be named");
    assert!(p0 < p1, "configuration order preserved: {message}");
    assert!(!timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}

fn rcode_parse_error_wire() -> Vec<u8> {
    // Passes upstream::valid_reply (len>=12, ID match, QR=1, QDCOUNT=1) with TC=0,
    // but claims ARCOUNT=1 without any additional RR bytes so dns::rcode() returns Err.
    let mut wire = QUERY.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire[11] = 1;
    wire
}

#[tokio::test(flavor = "current_thread")]
async fn rcode_parse_error_does_not_occupy_fallback_before_refused() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let bad_wire = rcode_parse_error_wire();
    let refused = wire_reply(5, None, None);
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(bad_wire),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(refused.clone()),
    )
    .await;
    let result = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("must finish promptly")
    .expect("legal REFUSED must win over earlier RCODE parse error");
    assert_eq!(
        result, refused,
        "RCODE parse error must not occupy the first DNS error fallback slot"
    );
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn only_rcode_parse_error_returns_err_not_bad_packet() {
    // Case 1: RCODE parse error + I/O close -> aggregated Err naming both upstreams in order.
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(rcode_parse_error_wire()),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(barrier, Some(rx), None, ControlledReply::Close).await;
    let addr0 = format!("tcp://127.0.0.1:{}", u0.port.unwrap());
    let addr1 = format!("tcp://127.0.0.1:{}", u1.port.unwrap());
    let error = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("must finish promptly")
    .expect_err("parse failure with no usable DNS reply must return Err");
    let message = error.to_string();
    assert!(message.starts_with("no upstream answered: "), "{message}");
    let p0 = message
        .find(&format!("{addr0}: invalid DNS wire response"))
        .expect("parse-failed upstream must be named with parse error");
    let p1 = message.find(&addr1).expect("closed upstream must be named");
    assert!(p0 < p1, "configuration order preserved: {message}");
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(!timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());

    // Case 2: Single upstream with RCODE parse error -> Err(InvalidData), no extra wait.
    let single_barrier = Arc::new(Barrier::new(1));
    let (su, sw) = controlled_tcp_peer(
        single_barrier,
        None,
        None,
        ControlledReply::Wire(rcode_parse_error_wire()),
    )
    .await;
    let saddr = format!("tcp://127.0.0.1:{}", su.port.unwrap());
    let serr = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[su], QUERY, Tcp),
    )
    .await
    .expect("single parse error must finish immediately")
    .expect_err("single parse failure must return Err");
    assert_eq!(serr.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        serr.to_string()
            .contains(&format!("{saddr}: invalid DNS wire response")),
        "{serr}"
    );
    assert!(timeout(Duration::from_secs(1), sw).await.unwrap().unwrap());

    // Case 3: Legal truncated reply (TC=1) with ARCOUNT=1 cut off before OPT still returns Ok.
    let tc_barrier = Arc::new(Barrier::new(1));
    let mut tc_wire = rcode_parse_error_wire();
    tc_wire[2] |= 0x02; // TC=1
    let (tu, tw) = controlled_tcp_peer(
        tc_barrier,
        None,
        None,
        ControlledReply::Wire(tc_wire.clone()),
    )
    .await;
    let tc_res = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[tu], QUERY, Tcp),
    )
    .await
    .expect("truncated reply must finish immediately")
    .expect("legal TC=1 reply must preserve Ok contract");
    assert_eq!(tc_res, tc_wire);
    assert!(timeout(Duration::from_secs(1), tw).await.unwrap().unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_after_complete_opt_keeps_extended_rcode_badvers() {
    // TC=1, low RCODE=0, ARCOUNT=2: first additional RR is a complete OPT with ext_rcode=1 (BADVERS=16),
    // followed by a truncated second additional RR.
    let mut tc_badvers_truncated_tail = wire_reply(0, None, Some(1));
    tc_badvers_truncated_tail[2] |= 0x02; // TC=1
    tc_badvers_truncated_tail[11] = 2; // ARCOUNT=2
    // Truncated second additional RR (name + partial header, cut off before RDLEN/RDATA).
    tc_badvers_truncated_tail.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);

    // Direct rcode check: must still recognize 16 (BADVERS), not downgrade to 0.
    assert_eq!(dns::rcode(&tc_badvers_truncated_tail), Ok(16));

    // In-group race: must NOT beat a slower valid NOERROR.
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = oneshot::channel();
    let noerror = wire_reply(0, Some(3), Some(0));
    let (u0, w0) = controlled_tcp_peer(
        barrier.clone(),
        None,
        Some(tx),
        ControlledReply::Wire(tc_badvers_truncated_tail),
    )
    .await;
    let (u1, w1) = controlled_tcp_peer(
        barrier,
        Some(rx),
        None,
        ControlledReply::Wire(noerror.clone()),
    )
    .await;
    let result = timeout(
        Duration::from_secs(2),
        upstream_group::exchange(&[u0, u1], QUERY, Tcp),
    )
    .await
    .expect("must finish promptly")
    .expect("NOERROR must win over TC=1 BADVERS with truncated trailing additional RR");
    assert_eq!(result, noerror);
    assert!(timeout(Duration::from_secs(1), w0).await.unwrap().unwrap());
    assert!(timeout(Duration::from_secs(1), w1).await.unwrap().unwrap());
}
