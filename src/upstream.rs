use crate::config::{Protocol, Upstream};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// M1: one attempt, one socket/connection per query, including TCP upstreams.
pub async fn exchange(upstream: &Upstream, query: &[u8]) -> io::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let addr = SocketAddr::new(upstream.ip, upstream.port.unwrap_or(53));
        let mut response = vec![0; 65535];
        match upstream.protocol {
            Some(Protocol::Tcp) => {
                let mut stream = TcpStream::connect(addr).await?;
                stream.write_u16(query.len() as u16).await?;
                stream.write_all(query).await?;
                let len = stream.read_u16().await? as usize;
                response.resize(len, 0);
                stream.read_exact(&mut response).await?;
            }
            Some(Protocol::Tls) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "tls:// P0-conditional 未实现",
                ));
            }
            None | Some(Protocol::Udp) => {
                let bind = if addr.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                let socket = UdpSocket::bind(bind).await?;
                socket.connect(addr).await?;
                socket.send(query).await?;
                loop {
                    let len = socket.recv(&mut response).await?;
                    if valid_reply(&response[..len], query) {
                        response.truncate(len);
                        break;
                    }
                }
            }
        }
        if !valid_reply(&response, query) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid upstream response",
            ));
        }
        Ok(response)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "upstream timeout after 5 s"))?
}

fn valid_reply(response: &[u8], query: &[u8]) -> bool {
    response.len() >= 12
        && response[..2] == query[..2]
        && response[2] & 0x80 != 0
        && response[4..6] == [0, 1]
}
