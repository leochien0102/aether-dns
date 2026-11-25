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

use config::{Config, Protocol, SetName, SetPair, Tag, Upstream};
use server::ip_writer::{IpWriter, write_reply_offloaded};
use std::{
    future::{Future, poll_fn},
    io,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
};
use tokio::{net::UdpSocket, sync::Notify};

#[derive(Default)]
struct Writer {
    open: Mutex<bool>,
    gate: Condvar,
    entered: Notify,
    calls: AtomicUsize,
}
impl Writer {
    fn close(&self) {
        *self.open.lock().unwrap() = false;
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.gate.notify_all();
    }
}
impl IpWriter for Writer {
    fn add(&self, _: &SetName, _: &[Ipv4Addr]) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.gate.wait(open).unwrap();
        }
        Err(io::Error::other("injected write failure"))
    }
}
struct NoChina;
impl server::ip_test::IpTest for NoChina {
    fn is_china(&self, _: IpAddr) -> bool {
        false
    }
}
fn query(ipv6: bool) -> Vec<u8> {
    vec![
        0x42,
        0x42,
        1,
        0,
        0,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        b'x',
        0,
        0,
        if ipv6 { 28 } else { 1 },
        0,
        1,
    ]
}
fn reply(query: &[u8]) -> Vec<u8> {
    let mut wire = query.to_vec();
    wire[2] = 0x81;
    wire[7] = 1;
    let ipv6 = query[16] == 28;
    wire.extend_from_slice(&[
        0xc0,
        0x0c,
        0,
        if ipv6 { 28 } else { 1 },
        0,
        1,
        0,
        0,
        0,
        30,
        0,
        if ipv6 { 16 } else { 4 },
    ]);
    wire.extend_from_slice(if ipv6 { &[0; 16] } else { &[1, 2, 3, 4] });
    wire
}
async fn send(port: u16, ipv6: bool) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket
        .send_to(&query(ipv6), (Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    socket
}
async fn receive(socket: &UdpSocket, ipv6: bool) {
    let mut wire = vec![0; 512];
    let n = socket.recv(&mut wire).await.unwrap();
    wire.truncate(n);
    let mut expected = reply(&query(ipv6));
    dns::set_ra(&mut expected).unwrap();
    assert_eq!(wire, expected);
}
async fn start(
    config: &Config,
    writer: &Arc<Writer>,
) -> (
    u16,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    in_process::start(|port, receiver| {
        let mut config = config.clone();
        config.bind_port = vec![port];
        let writer = writer.clone();
        async move {
            server::run_with_ip_writer(config, receiver, Arc::new(NoChina), writer)
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await
}

#[test]
fn write_barriers_live_cache_overload_cancellation_shutdown_and_recovery() {
    let writer = Arc::new(Writer::default());
    let (done, watchdog_rx) = std::sync::mpsc::channel();
    let rescue = writer.clone();
    let watchdog = std::thread::spawn(move || {
        if watchdog_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .is_err()
        {
            rescue.release();
            eprintln!("write fixture watchdog expired");
            std::process::abort();
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let upstream = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = upstream.local_addr().unwrap();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let count = exchanges.clone();
        let worker = tokio::spawn(async move {
            let mut wire = [0;512];
            loop {
                let (n, peer) = upstream.recv_from(&mut wire).await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                upstream.send_to(&reply(&wire[..n]), peer).await.unwrap();
            }
        });
        let config = Config {
            bind_addr: vec![Ipv4Addr::LOCALHOST],
            default_tag: Some(Tag::Gfw),
            cache: Some(32),
            add_taggfw_ip: Some(SetPair { v4: Some(SetName {family:"inet".into(), table:"test".into(), set:"v4".into()}), v6: None }),
            trust_dns: vec![Upstream { protocol:Some(Protocol::Udp), host:None, ip:address.ip(), port:Some(address.port()) }],
            ..Config::default()
        };
        let (port, shutdown, serving) = start(&config, &writer).await;
        // Both the live answer and the subsequent cache hit must wait for an
        // actual attempt; unrelated AAAA traffic completes with the gate closed.
        for expected_calls in 1..=2 {
            let client = send(port, false).await;
            writer.entered.notified().await;
            let free = send(port, true).await;
            receive(&free, true).await;
            assert!(!*writer.open.lock().unwrap());
            assert_eq!(writer.calls.load(Ordering::SeqCst), expected_calls);
            assert_eq!(exchanges.load(Ordering::SeqCst), 2, "cache hit must not requery upstream");
            assert!(matches!(client.try_recv(&mut [0;512]), Err(e) if e.kind()==io::ErrorKind::WouldBlock), "admitted response escaped write barrier");
            writer.release();
            receive(&client, false).await;
            writer.close();
        }

        // One running + seven queued writes fill the process-wide bound.
        let client = send(port, false).await;
        writer.entered.notified().await;
        let response = reply(&query(false));
        let mut queued = Vec::new();
        for _ in 1..8 {
            let mut future = Box::pin(write_reply_offloaded(&config, lists::RouteTag::Gfw, &response, writer.clone()));
            poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            queued.push(future);
        }
        let calls = writer.calls.load(Ordering::SeqCst);
        // Saturated writes and no-write packets must not submit or wait.
        write_reply_offloaded(&config, lists::RouteTag::Gfw, &response, writer.clone()).await;
        write_reply_offloaded(&config, lists::RouteTag::Chn, &response, writer.clone()).await;
        let overflow = send(port, false).await;
        receive(&overflow, false).await;
        assert_eq!(writer.calls.load(Ordering::SeqCst), calls);
        assert!(matches!(client.try_recv(&mut [0;512]), Err(e) if e.kind()==io::ErrorKind::WouldBlock));
        drop(queued);
        shutdown.send_replace(true);
        serving.await.unwrap().unwrap();
        assert!(!*writer.open.lock().unwrap(), "service stop cannot kill a running syscall");
        // A second instance cannot evade the cap after waiter cancellation.
        let (port, shutdown, serving) = start(&config, &writer).await;
        receive(&send(port, false).await, false).await;
        assert_eq!(writer.calls.load(Ordering::SeqCst), calls);
        writer.release();
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert_eq!(writer.calls.load(Ordering::SeqCst), calls + 7, "overload never enqueued extra jobs");
        // Cache hit retries after overload/failure, with no permanent dedup.
        let count = exchanges.load(Ordering::SeqCst);
        receive(&send(port, false).await, false).await;
        assert_eq!(writer.calls.load(Ordering::SeqCst), calls + 8);
        assert_eq!(exchanges.load(Ordering::SeqCst), count);
        shutdown.send_replace(true);
        serving.await.unwrap().unwrap();
        worker.abort();
    });
    drop(runtime);
    done.send(()).unwrap();
    watchdog.join().unwrap();
}
