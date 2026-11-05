use crate::{config::Config, config::Protocol, dns, upstream};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

pub async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    config.validate_startup()?;
    let upstream = config.trust_dns.first().ok_or("missing trust-dns")?.clone();
    let addresses = if config.bind_addr.is_empty() {
        vec![Ipv4Addr::LOCALHOST]
    } else {
        config.bind_addr
    };
    let ports = if config.bind_port.is_empty() {
        vec![53]
    } else {
        config.bind_port
    };
    // Bind everything before starting any listener, so partial startup fails closed.
    let mut sockets = Vec::new();
    let mut listeners = Vec::new();
    for address in addresses {
        for &port in &ports {
            let socket = UdpSocket::bind((address, port)).await?;
            listeners.push(TcpListener::bind(socket.local_addr()?).await?);
            sockets.push(Arc::new(socket));
        }
    }
    for socket in sockets {
        eprintln!("listening UDP {}", socket.local_addr()?);
        let upstream = upstream.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0; 65535];
            let mut next_id = 0u16;
            loop {
                let (len, peer) = match socket.recv_from(&mut buffer).await {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("UDP receive: {error}");
                        continue;
                    }
                };
                let query = buffer[..len].to_vec();
                if dns::check_query(&query).is_err() {
                    continue;
                }
                let client_id = u16::from_be_bytes([query[0], query[1]]);
                next_id = next_id.wrapping_add(1);
                if next_id == client_id {
                    next_id = next_id.wrapping_add(1);
                }
                let id = next_id;
                let socket = socket.clone();
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    if let Some(response) =
                        handle_query(&upstream, query, client_id, id, Protocol::Udp, peer).await
                    {
                        send(&socket, peer, &response).await;
                    }
                });
            }
        });
    }
    // Keep all UDP readiness lines ahead of TCP readiness lines.
    for listener in listeners {
        eprintln!("listening TCP {}", listener.local_addr()?);
        let upstream = upstream.clone();
        tokio::spawn(async move {
            let mut next_id = 0u16;
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("TCP accept: {error}");
                        continue;
                    }
                };
                next_id = next_id.wrapping_add(1);
                let id = next_id;
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    if let Err(error) = tcp_query(stream, &upstream, id, peer).await {
                        eprintln!("TCP client {peer}: {error}");
                    }
                });
            }
        });
    }
    std::future::pending::<()>().await;
    Ok(())
}

// M1: exactly one framed query and response per client connection.
async fn tcp_query(
    mut stream: TcpStream,
    upstream: &crate::config::Upstream,
    mut id: u16,
    peer: SocketAddr,
) -> std::io::Result<()> {
    let len = stream.read_u16().await? as usize;
    let mut query = vec![0; len];
    stream.read_exact(&mut query).await?;
    if dns::check_query(&query).is_err() {
        return Ok(());
    }
    let client_id = u16::from_be_bytes([query[0], query[1]]);
    if id == client_id {
        id = id.wrapping_add(1);
    }
    if let Some(response) = handle_query(upstream, query, client_id, id, Protocol::Tcp, peer).await
    {
        stream.write_u16(response.len() as u16).await?;
        stream.write_all(&response).await?;
    }
    stream.shutdown().await
}

async fn handle_query(
    upstream: &crate::config::Upstream,
    mut query: Vec<u8>,
    client_id: u16,
    id: u16,
    client_protocol: Protocol,
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    let original = query.clone();
    // Callers check the query before rewriting its ID.
    dns::set_id(&mut query, id).expect("checked header");
    match forward(upstream, &mut query, client_id, client_protocol).await {
        Ok(response) => Some(response),
        Err(error) => {
            eprintln!("upstream {peer}: {error}");
            let mut response = original;
            dns::synthetic_nodata(&mut response).ok()?;
            response[3] = (response[3] & 0xf0) | 2;
            Some(response)
        }
    }
}

async fn forward(
    upstream: &crate::config::Upstream,
    query: &mut [u8],
    client_id: u16,
    client_protocol: Protocol,
) -> std::io::Result<Vec<u8>> {
    let mut response = upstream::exchange(upstream, query, client_protocol).await?;
    dns::set_id(&mut response, client_id).map_err(wire_error)?;
    dns::set_ra(&mut response).map_err(wire_error)?;
    if matches!(response[3] & 0x0f, 0 | 3) && response[2] & 2 == 0 {
        dns::truncate_additional(&mut response).map_err(wire_error)?;
    }
    Ok(response)
}

fn wire_error(_: dns::WireError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid DNS wire response")
}

async fn send(socket: &UdpSocket, peer: SocketAddr, response: &[u8]) {
    if let Err(error) = socket.send_to(response, peer).await {
        eprintln!("UDP send {peer}: {error}");
    }
}
