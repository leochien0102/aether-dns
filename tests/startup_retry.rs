mod common;
#[path = "common/in_process.rs"]
mod in_process;

use std::net::{TcpListener, UdpSocket};
use std::time::Duration;

fn configs() -> Vec<std::path::PathBuf> {
    let prefix = format!("test-server-{}-", std::process::id());
    std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/target"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&prefix)
        })
        .collect()
}

#[test]
fn retries_collisions_and_early_exit_with_bounded_cleanup() {
    assert!(configs().is_empty());
    // Occupy the selected port after reservation release: no timing or sleeps.
    // Exercise each transport separately before allowing a successful startup.
    let mut calls = 0;
    let mut tcp = None;
    let mut udp = None;
    let mut seen = Vec::new();
    let mut server = common::Server::start(&["127.0.0.1"], 1, Duration::from_secs(1), |ports| {
        assert!(!seen.contains(&ports[0]), "retry must choose a new port");
        seen.push(ports[0]);
        tcp.take();
        udp.take();
        calls += 1;
        match calls {
            1 => tcp = Some(TcpListener::bind(("127.0.0.1", ports[0])).unwrap()),
            2 => udp = Some(UdpSocket::bind(("127.0.0.1", ports[0])).unwrap()),
            _ => {}
        }
        "trust-dns 127.0.0.1#1\n".to_owned()
    });
    assert_eq!(calls, 3);
    assert!(server.child.try_wait().unwrap().is_none());
    assert_eq!(configs().len(), 1, "only successful config remains");
    drop(server);
    assert!(configs().is_empty());

    // Exhaustion must fail, not silently accept a bind diagnostic as readiness.
    calls = 0;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        common::Server::start(&["127.0.0.1"], 1, Duration::from_secs(1), |ports| {
            calls += 1;
            tcp = Some(TcpListener::bind(("127.0.0.1", ports[0])).unwrap());
            "trust-dns 127.0.0.1#1\n".to_owned()
        })
    }));
    assert_eq!(calls, 10);
    let message = failed.err().unwrap().downcast::<String>().unwrap();
    assert!(message.contains("failed after 10 attempts"));
    assert!(message.contains("Address already in use"));
    assert!(configs().is_empty());
    drop(tcp);

    // A process exiting before readiness is also retried and cleaned up.
    calls = 0;
    let server = common::Server::start(&["127.0.0.1"], 1, Duration::from_secs(1), |_| {
        calls += 1;
        if calls == 1 {
            "unknown-option yes\n".to_owned()
        } else {
            "trust-dns 127.0.0.1#1\n".to_owned()
        }
    });
    assert_eq!(calls, 2);
    drop(server);
    assert!(configs().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn in_process_retries_join_failed_tasks_and_preserve_shutdown() {
    let mut calls = 0;
    let mut seen = Vec::new();
    let (port, shutdown, serving) = in_process::start(|port, mut receiver| {
        calls += 1;
        assert!(!seen.contains(&port));
        seen.push(port);
        let attempt = calls;
        async move {
            if attempt == 1 {
                return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse).to_string());
            }
            if attempt == 2 {
                return Err("exited before readiness".to_owned());
            }
            let udp = tokio::net::UdpSocket::bind(("127.0.0.1", port))
                .await
                .map_err(|error| error.to_string())?;
            let tcp = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .map_err(|error| error.to_string())?;
            receiver.changed().await.unwrap();
            drop((tcp, udp));
            Ok(())
        }
    })
    .await;
    assert_eq!(calls, 3);
    shutdown.send(true).unwrap();
    serving.await.unwrap().unwrap();
    let _tcp = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let _udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let failed = tokio::spawn(async move {
        in_process::start(|_, _| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err("startup failed".to_owned()) }
        })
        .await
    })
    .await;
    assert!(failed.unwrap_err().is_panic());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 10);
}
