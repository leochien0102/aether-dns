use std::future::Future;
use std::net::Ipv4Addr;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

// In-process equivalent of the subprocess handoff retry. Failed tasks are
// joined before selecting another port; there is no startup polling sleep.
pub async fn start<F, Fut>(mut run: F) -> (u16, watch::Sender<bool>, JoinHandle<Result<(), String>>)
where
    F: FnMut(u16, watch::Receiver<bool>) -> Fut,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let mut failures = Vec::new();
    let mut ports = Vec::new();
    for _ in 0..10 {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        if ports.contains(&port) {
            continue;
        }
        ports.push(port);
        let udp = match UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(socket) => socket,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => panic!("reserve UDP: {error}"),
        };
        drop((tcp, udp));
        let (shutdown, receiver) = watch::channel(false);
        let mut serving = tokio::spawn(run(port, receiver));
        let ready = timeout(Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    biased;
                    result = &mut serving => return Err(format!("startup exited: {result:?}")),
                    connected = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) => {
                        if connected.is_ok() {
                            // Let a failed local bind finish even if a competing
                            // listener answered the probe on the selected port.
                            tokio::task::yield_now().await;
                            if serving.is_finished() {
                                return Err(format!("startup exited: {:?}", (&mut serving).await));
                            }
                            return Ok(());
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        match ready {
            Ok(Ok(())) => return (port, shutdown, serving),
            Ok(Err(error)) => failures.push(error), // the task is already joined
            Err(error) => {
                serving.abort();
                let _ = serving.await;
                failures.push(format!("startup timeout: {error}"));
            }
        }
    }
    panic!("in-process startup failed after 10 attempts: {failures:?}");
}
