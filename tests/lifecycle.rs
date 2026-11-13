use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const SECOND: Duration = Duration::from_secs(1);

struct Server {
    child: Child,
    path: std::path::PathBuf,
    endpoint: SocketAddr,
    info: Receiver<String>,
    errors: Receiver<String>,
}

impl Server {
    fn start(upstream: &UdpSocket) -> Self {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "target/lifecycle-{}-{}.conf",
            std::process::id(),
            upstream.local_addr().unwrap().port()
        ));
        std::fs::write(
            &path,
            format!(
                "bind-addr 127.0.0.1\nbind-port {port}\ntrust-dns udp://127.0.0.1#{}\n",
                upstream.local_addr().unwrap().port()
            ),
        )
        .unwrap();
        drop(reserved);
        let mut child = Command::new(env!("CARGO_BIN_EXE_aether-dns"))
            .arg("-C")
            .arg(&path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // Readiness and shutdown are lifecycle lines on stdout; stderr is errors only.
        // Both streams are drained so the child never blocks on a full pipe.
        let (info_sender, info) = mpsc::channel();
        let (error_sender, errors) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if info_sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if error_sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            path,
            endpoint: "127.0.0.1:0".parse().unwrap(),
            info,
            errors,
        };
        let first = server.info.recv_timeout(SECOND).unwrap();
        server.endpoint = first
            .strip_prefix("listening UDP ")
            .expect("first stdout line must remain listening UDP")
            .parse()
            .unwrap();
        assert_eq!(
            server.info.recv_timeout(SECOND).unwrap(),
            format!("listening TCP {}", server.endpoint)
        );
        server
    }

    fn signal(&self, signal: &str) -> Instant {
        let start = Instant::now();
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(self.child.id().to_string())
                .status()
                .unwrap()
                .success()
        );
        start
    }

    fn stopping(&self) {
        assert_eq!(
            self.info.recv_timeout(SECOND).unwrap(),
            "shutdown: draining queries (500 ms maximum)"
        );
        // This lifecycle line must not reach stderr.
        assert!(
            self.errors.try_recv().is_err(),
            "shutdown must not write to stderr"
        );
    }

    fn exited(&mut self, start: Instant, limit: Duration, success: bool) {
        let status: ExitStatus = loop {
            assert!(start.elapsed() < limit, "shutdown exceeded {limit:?}");
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(status.code(), Some(if success { 0 } else { 1 }));
        println!("exit={status} elapsed={:?}", start.elapsed());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn upstream() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(SECOND)).unwrap();
    socket
}

fn query() -> Vec<u8> {
    let mut wire = vec![0x42, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    wire.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
    wire
}

fn response(query: &[u8]) -> Vec<u8> {
    let mut wire = query.to_vec();
    wire[2] = 0x81;
    wire[3] = 0x80;
    wire
}

enum Client {
    Udp(UdpSocket),
    Tcp(TcpStream),
}

impl Client {
    fn send(endpoint: SocketAddr, tcp: bool) -> Self {
        let query = query();
        if tcp {
            let mut stream = TcpStream::connect_timeout(&endpoint, SECOND).unwrap();
            stream.set_read_timeout(Some(SECOND)).unwrap();
            stream.set_write_timeout(Some(SECOND)).unwrap();
            stream
                .write_all(&(query.len() as u16).to_be_bytes())
                .unwrap();
            stream.write_all(&query).unwrap();
            Self::Tcp(stream)
        } else {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.set_read_timeout(Some(SECOND)).unwrap();
            socket.send_to(&query, endpoint).unwrap();
            Self::Udp(socket)
        }
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::Udp(socket) => {
                let mut buffer = [0; 512];
                match socket.recv_from(&mut buffer) {
                    Ok((len, _)) => Some(buffer[..len].to_vec()),
                    Err(error) => {
                        assert!(matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ));
                        None
                    }
                }
            }
            Self::Tcp(stream) => {
                let mut prefix = [0; 2];
                match stream.read_exact(&mut prefix) {
                    Ok(()) => {
                        let mut wire = vec![0; u16::from_be_bytes(prefix) as usize];
                        stream.read_exact(&mut wire).unwrap();
                        Some(wire)
                    }
                    Err(error) => {
                        assert!(matches!(
                            error.kind(),
                            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                        ));
                        None
                    }
                }
            }
        }
    }
}

#[test]
fn idle_term_and_int_exit_zero_within_one_second() {
    for signal in ["TERM", "INT"] {
        let upstream = upstream();
        let mut server = Server::start(&upstream);
        let start = server.signal(signal);
        server.exited(start, SECOND, true);
    }
}

fn completes_in_flight(tcp: bool) {
    for signal in ["TERM", "INT"] {
        let upstream = upstream();
        let mut server = Server::start(&upstream);
        let mut idle = TcpStream::connect_timeout(&server.endpoint, SECOND).unwrap();
        idle.set_read_timeout(Some(SECOND)).unwrap();
        idle.set_write_timeout(Some(SECOND)).unwrap();
        let prefix = (query().len() as u16).to_be_bytes();
        idle.write_all(&prefix[..1]).unwrap();
        let mut client = Client::send(server.endpoint, tcp);
        let mut buffer = [0; 512];
        let (len, peer) = upstream.recv_from(&mut buffer).unwrap();
        let answer = response(&buffer[..len]);
        let start = server.signal(signal);
        server.stopping();

        // Give listener tasks a turn to close their receive/accept paths. The
        // outstanding query keeps the process alive, not a fixed shutdown sleep.
        std::thread::sleep(Duration::from_millis(30));
        assert!(server.child.try_wait().unwrap().is_none());
        // Completing a TCP frame after shutdown must not create new work.
        let _ = idle.write_all(&prefix[1..]);
        let _ = idle.write_all(&query());
        assert_eq!(Client::Tcp(idle).receive(), None);
        let late_udp = Client::send(server.endpoint, false);
        let late_tcp = TcpStream::connect_timeout(&server.endpoint, Duration::from_millis(50));
        assert!(late_tcp.is_err(), "TCP listener must close while draining");
        upstream
            .set_read_timeout(Some(Duration::from_millis(60)))
            .unwrap();
        assert!(matches!(
            upstream.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        upstream.send_to(&answer, peer).unwrap();
        assert_eq!(client.receive(), Some(response(&query())));
        server.exited(start, SECOND, true);
        drop(late_udp);
    }
}

#[test]
fn udp_in_flight_completes_and_new_queries_stop() {
    completes_in_flight(false);
}

#[test]
fn tcp_in_flight_completes_and_new_queries_stop() {
    completes_in_flight(true);
}

fn discards_in_flight(tcp: bool) {
    for signal in ["TERM", "INT"] {
        let upstream = upstream();
        let mut server = Server::start(&upstream);
        let mut client = Client::send(server.endpoint, tcp);
        let mut buffer = [0; 512];
        let (len, peer) = upstream.recv_from(&mut buffer).unwrap();
        let answer = response(&buffer[..len]);
        let start = server.signal(signal);
        server.stopping();
        // Keep the upstream silent past the 500 ms drain budget.
        server.exited(start, SECOND, true);
        assert!(start.elapsed() >= Duration::from_millis(450));
        upstream.send_to(&answer, peer).unwrap();
        assert_eq!(client.receive(), None, "expired work must not send a reply");
    }
}

#[test]
fn udp_in_flight_is_discarded_after_budget() {
    discards_in_flight(false);
}

#[test]
fn tcp_in_flight_is_discarded_after_budget() {
    discards_in_flight(true);
}

#[test]
fn second_signal_exits_immediately_without_draining() {
    for (first, second) in [
        ("TERM", "TERM"),
        ("TERM", "INT"),
        ("INT", "TERM"),
        ("INT", "INT"),
    ] {
        let upstream = upstream();
        let mut server = Server::start(&upstream);
        let _client = Client::send(server.endpoint, false);
        upstream.recv_from(&mut [0; 512]).unwrap();
        server.signal(first);
        server.stopping();
        assert!(server.child.try_wait().unwrap().is_none());
        let start = server.signal(second);
        server.exited(start, Duration::from_millis(250), false);
    }
}
