#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
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
use tokio::sync::Barrier;
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
