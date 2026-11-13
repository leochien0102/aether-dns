use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const ATTEMPTS: usize = 10;
static NEXT_CONFIG: AtomicUsize = AtomicUsize::new(0);

pub struct Server {
    pub child: Child,
    path: PathBuf,
    pub endpoints: Vec<SocketAddr>,
    /// stdout: lifecycle/informational lines (readiness, shutdown, upstream diagnostics).
    pub info: Receiver<String>,
    /// stderr: genuine errors only. Only some test targets assert on it.
    #[allow(dead_code)]
    pub errors: Receiver<String>,
}

impl Server {
    /// Reserve every address/transport, then retry the unavoidable handoff race.
    /// `config` runs after release so tests can deterministically inject a collision.
    pub fn start(
        addresses: &[&str],
        port_count: usize,
        startup_timeout: Duration,
        mut config: impl FnMut(&[u16]) -> String,
    ) -> Self {
        let mut failures = Vec::new();
        let mut previous_ports = Vec::new();
        for attempt in 1..=ATTEMPTS {
            let mut reservations = Vec::new();
            let mut ports = Vec::new();
            let reserved = (|| -> std::io::Result<()> {
                for _ in 0..port_count {
                    let first = TcpListener::bind((addresses[0], 0))?;
                    let port = first.local_addr()?.port();
                    if previous_ports.contains(&port) {
                        return Err(std::io::ErrorKind::AddrInUse.into());
                    }
                    let mut tcp = vec![first];
                    let mut udp = Vec::new();
                    for (index, address) in addresses.iter().enumerate() {
                        if index != 0 {
                            tcp.push(TcpListener::bind((*address, port))?);
                        }
                        udp.push(UdpSocket::bind((*address, port))?);
                    }
                    ports.push(port);
                    reservations.push((tcp, udp));
                }
                Ok(())
            })();
            if let Err(error) = reserved {
                assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse, "{error}");
                failures.push(format!("attempt {attempt}: reservation: {error}"));
                continue;
            }
            previous_ports.extend_from_slice(&ports);
            let endpoints: Vec<SocketAddr> = addresses
                .iter()
                .flat_map(|address| {
                    ports
                        .iter()
                        .map(move |port| format!("{address}:{port}").parse().unwrap())
                })
                .collect();
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "target/test-server-{}-{}.conf",
                std::process::id(),
                NEXT_CONFIG.fetch_add(1, Ordering::Relaxed)
            ));
            drop(reservations);
            let body = config(&ports);
            let mut text = String::new();
            for address in addresses {
                text.push_str(&format!("bind-addr {address}\n"));
            }
            for port in &ports {
                text.push_str(&format!("bind-port {port}\n"));
            }
            text.push_str(&body);
            std::fs::write(&path, text).unwrap();
            let child = Command::new(env!("CARGO_BIN_EXE_aether-dns"))
                .arg("-C")
                .arg(&path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(child) => child,
                Err(error) => {
                    std::fs::remove_file(&path).unwrap();
                    panic!("spawn test server: {error}");
                }
            };
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let (info_sender, info) = mpsc::channel();
            let (error_sender, errors) = mpsc::channel();
            let info_reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) => {
                            println!("[stdout] {line}");
                            // Always drain stdout even when the consumer is gone.
                            let _ = info_sender.send(line);
                        }
                        Err(_) => break,
                    }
                }
            });
            let error_reader = std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    match line {
                        Ok(line) => {
                            eprintln!("[stderr] {line}");
                            // Always drain stderr even when the consumer is gone.
                            let _ = error_sender.send(line);
                        }
                        Err(_) => break,
                    }
                }
            });
            let mut server = Self {
                child,
                path,
                endpoints,
                info,
                errors,
            };
            let ready = server.ready(startup_timeout);
            match ready {
                Ok(()) => return server,
                Err(error) => {
                    failures.push(format!("attempt {attempt}, ports {ports:?}: {error}"));
                    // Drop performs kill + wait + unlink before the next attempt.
                    drop(server);
                    info_reader.join().unwrap();
                    error_reader.join().unwrap();
                }
            }
        }
        panic!(
            "test server failed after {ATTEMPTS} attempts:\n{}",
            failures.join("\n")
        );
    }

    /// Collect stderr lines seen so far, waiting briefly for the reader thread.
    /// Startup failures (bind errors included) are reported there, so the retry
    /// message must carry them to stay diagnosable.
    fn drain_errors(&self) -> String {
        let mut text = String::new();
        while let Ok(line) = self.errors.recv_timeout(Duration::from_millis(100)) {
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    fn ready(&mut self, timeout: Duration) -> Result<(), String> {
        for protocol in ["UDP", "TCP"] {
            for endpoint in &self.endpoints {
                let expected = format!("listening {protocol} {endpoint}");
                let line = self.info.recv_timeout(timeout).map_err(|error| {
                    format!(
                        "before {expected}: stdout closed or timed out: {error}; stderr: {}",
                        self.drain_errors()
                    )
                })?;
                // Any unexpected diagnostic (including bind errors / Address already
                // in use) fails this port; it must never be mistaken for readiness.
                if line != expected {
                    return Err(format!(
                        "expected {expected:?} on stdout, got {line:?}; stderr: {}",
                        self.drain_errors()
                    ));
                }
            }
        }
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "exited during startup: {status}; stderr: {}",
                self.drain_errors()
            ));
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::fs::remove_file(&self.path).expect("remove test server config");
    }
}
