#![allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/dns/mod.rs"]
mod dns;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "../src/lists.rs"]
mod lists;
#[path = "../src/upstream.rs"]
mod upstream;
#[path = "../src/upstream_group.rs"]
mod upstream_group;

mod server {
    include!("../src/server.rs");

    mod offload {
        use super::*;
        use crate::config::Tag;
        use std::future::{Future, poll_fn};
        use std::net::IpAddr;
        use std::sync::{
            Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use std::task::Poll;
        use tokio::sync::Notify;

        #[derive(Default)]
        struct BlockingTest {
            open: Mutex<bool>,
            gate: Condvar,
            entered: Notify,
            calls: AtomicUsize,
        }
        impl BlockingTest {
            fn release(&self) {
                *self.open.lock().unwrap() = true;
                self.gate.notify_all();
            }
        }
        impl IpTest for BlockingTest {
            fn is_china(&self, _: IpAddr) -> bool {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.entered.notify_one();
                let mut open = self.open.lock().unwrap();
                while !*open {
                    open = self.gate.wait(open).unwrap();
                }
                true
            }
        }

        fn routing(test: Arc<dyn IpTest>) -> Routing {
            let config = Config {
                verdict_cache: Some(32),
                ..Config::default()
            };
            Routing {
                verdict: Mutex::new(verdict::VerdictCache::new(config.verdict_cache)),
                cache: Mutex::new(cache::Cache::new(config.cache, config.cache_stale)),
                clock: Arc::new(cache::MonotonicClock::default()),
                id_source: Arc::new(OsRandomIdSource),
                refresh_tasks: Mutex::new(None),
                upstream_limiter: upstream_group::UpstreamLimiter::new(
                    DEFAULT_MAX_UPSTREAM_ATTEMPTS,
                    None,
                ),
                router: Router::load(&config),
                config,
                ip_test: test,
                ip_writer: Arc::new(NoopWriter),
            }
        }

        fn query(name: &[u8], qtype: u16) -> Vec<u8> {
            let mut wire = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
            wire.extend_from_slice(name);
            wire.extend_from_slice(&qtype.to_be_bytes());
            wire.extend_from_slice(&[0, 1]);
            wire
        }

        fn reply(query: &[u8], china: bool) -> Vec<u8> {
            let mut wire = query.to_vec();
            wire[2] = 0x81 | if china { 4 } else { 0 };
            wire[7] = 1;
            let ipv6 = query[query.len() - 3] == 28;
            wire.extend_from_slice(&[0xc0, 0x0c]);
            wire.extend_from_slice(if ipv6 { &[0, 28] } else { &[0, 1] });
            wire.extend_from_slice(&[0, 1, 0, 0, 0, 30]);
            if ipv6 {
                wire.extend_from_slice(&[0, 16]);
                wire.extend_from_slice(&[0; 16]);
            } else {
                wire.extend_from_slice(&[0, 4, 10, 0, 0, 1]);
            }
            wire
        }

        async fn upstream(china: bool) -> (Upstream, tokio::task::JoinHandle<()>) {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let port = socket.local_addr().unwrap().port();
            let worker = tokio::spawn(async move {
                let mut buf = [0; 4096];
                loop {
                    let (n, peer) = socket.recv_from(&mut buf).await.unwrap();
                    socket
                        .send_to(&reply(&buf[..n], china), peer)
                        .await
                        .unwrap();
                }
            });
            (
                Upstream {
                    protocol: Some(Protocol::Udp),
                    host: None,
                    ip: "127.0.0.1".parse().unwrap(),
                    port: Some(port),
                },
                worker,
            )
        }

        async fn start(
            config: &Config,
            test: &Arc<BlockingTest>,
        ) -> (
            u16,
            watch::Sender<bool>,
            tokio::task::JoinHandle<Result<(), String>>,
        ) {
            crate::in_process::start(|port, receiver| {
                let mut config = config.clone();
                config.bind_port = vec![port];
                let test = test.clone();
                async move {
                    run_with_ip_test(config, receiver, test)
                        .await
                        .map_err(|e| e.to_string())
                }
            })
            .await
        }

        async fn request(port: u16, query: &[u8]) -> Vec<u8> {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            socket
                .send_to(query, (Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let mut buf = vec![0; 4096];
            let n = socket.recv(&mut buf).await.unwrap();
            buf.truncate(n);
            assert_eq!(&buf[..2], &query[..2]);
            assert_eq!(buf[3] & 15, 0);
            buf
        }

        #[test]
        fn blocked_membership_keeps_reactor_live_and_quota_until_real_completion() {
            let test = Arc::new(BlockingTest::default());
            // Independent of Tokio: even a reactor-blocking regression cannot hang CI.
            let (done, watchdog_rx) = std::sync::mpsc::channel();
            let rescue = test.clone();
            let watchdog = std::thread::spawn(move || {
                if watchdog_rx
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .is_err()
                {
                    rescue.release();
                    eprintln!("membership fixture watchdog expired");
                    std::process::abort();
                }
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            runtime.block_on(async {
                let (china, china_worker) = upstream(true).await;
                let (trust, trust_worker) = upstream(false).await;
                let config = Config {
                    bind_addr: vec![Ipv4Addr::LOCALHOST],
                    default_tag: Some(Tag::None),
                    china_dns: vec![china],
                    trust_dns: vec![trust],
                    verdict_cache: Some(32),
                    ..Config::default()
                };
                let (port, shutdown, serving) = start(&config, &test).await;
                let blocked_query = query(b"\x07blocked\x07example\0", 1);
                let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                client
                    .send_to(&blocked_query, (Ipv4Addr::LOCALHOST, port))
                    .await
                    .unwrap();
                test.entered.notified().await;
                assert_eq!(IP_TEST_PERMITS.available_permits(), MAX_IP_TESTS - 1);

                // Real unrelated DNS request completes while the synchronous gate is closed.
                let ipv6 = query(b"\x04free\x07example\0", 28);
                assert_eq!(request(port, &ipv6).await[2] & 4, 0);
                assert!(!*test.open.lock().unwrap());
                assert_eq!(test.calls.load(Ordering::SeqCst), 1);

                // Poll each future once: it submits a job then waits. With one worker,
                // all remaining jobs are provably queued behind the blocked syscall.
                let routing = routing(test.clone());
                let name = b"\x06queued\x07example\0";
                let response = reply(&query(name, 1), true);
                let mut queued = Vec::new();
                for _ in 1..MAX_IP_TESTS {
                    let mut future = Box::pin(use_china_reply(&response, &routing, name));
                    poll_fn(|cx| {
                        assert!(future.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    queued.push(future);
                }
                assert_eq!(IP_TEST_PERMITS.available_permits(), 0);
                assert!(!use_china_reply(&response, &routing, name).await);
                assert_eq!(routing.verdict.lock().unwrap().get(name), None);
                assert_eq!(test.calls.load(Ordering::SeqCst), 1);

                // Cancel queued waiters and shut down the server owning the running
                // waiter. Neither may return the blocking jobs' permits early.
                drop(queued);
                shutdown.send_replace(true);
                serving.await.unwrap().unwrap();
                assert!(!*test.open.lock().unwrap());
                assert_eq!(IP_TEST_PERMITS.available_permits(), 0);
                assert!(!use_china_reply(&response, &routing, name).await);

                // A new server still shares the cap. Saturation chooses trust, but
                // must not poison the verdict for the same name after recovery.
                let (port, shutdown, serving) = start(&config, &test).await;
                let retry = query(b"\x05retry\x07example\0", 1);
                assert_eq!(request(port, &retry).await[2] & 4, 0);
                assert!(!*test.open.lock().unwrap());
                assert_eq!(test.calls.load(Ordering::SeqCst), 1);
                assert_eq!(IP_TEST_PERMITS.available_permits(), 0);
                test.release();
                // A marker in the single-worker FIFO runs after all detached jobs.
                tokio::task::spawn_blocking(|| ()).await.unwrap();
                assert_eq!(test.calls.load(Ordering::SeqCst), MAX_IP_TESTS);
                assert_eq!(IP_TEST_PERMITS.available_permits(), MAX_IP_TESTS);
                assert_eq!(request(port, &retry).await[2] & 4, 4);
                let calls = test.calls.load(Ordering::SeqCst);
                assert_eq!(calls, MAX_IP_TESTS + 1);
                assert_eq!(request(port, &retry).await[2] & 4, 4);
                assert_eq!(
                    test.calls.load(Ordering::SeqCst),
                    calls,
                    "normal verdict hit"
                );
                shutdown.send_replace(true);
                serving.await.unwrap().unwrap();
                china_worker.abort();
                trust_worker.abort();

                struct PanicTest;
                impl IpTest for PanicTest {
                    fn is_china(&self, _: IpAddr) -> bool {
                        panic!("injected blocking job failure")
                    }
                }
                let failed = super::offload::routing(Arc::new(PanicTest));
                assert!(!use_china_reply(&response, &failed, name).await);
                assert_eq!(failed.verdict.lock().unwrap().get(name), None);
                assert_eq!(IP_TEST_PERMITS.available_permits(), MAX_IP_TESTS);
            });
            drop(runtime);
            done.send(()).unwrap();
            watchdog.join().unwrap();
        }
    }
}
