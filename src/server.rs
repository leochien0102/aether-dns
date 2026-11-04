use crate::{config::Config, dns, upstream};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;

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
    for address in addresses {
        for &port in &ports {
            sockets.push(Arc::new(UdpSocket::bind((address, port)).await?));
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
                let mut query = buffer[..len].to_vec();
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
                    let original = query.clone();
                    // check_query guarantees the header exists.
                    dns::set_id(&mut query, id).expect("checked header");
                    match forward(&upstream, &mut query, client_id).await {
                        Ok(response) => send(&socket, peer, &response).await,
                        Err(error) => {
                            eprintln!("upstream {peer}: {error}");
                            let mut response = original;
                            if dns::synthetic_nodata(&mut response).is_ok() {
                                response[3] = (response[3] & 0xf0) | 2;
                                send(&socket, peer, &response).await;
                            }
                        }
                    }
                });
            }
        });
    }
    std::future::pending::<()>().await;
    Ok(())
}

async fn forward(
    upstream: &crate::config::Upstream,
    query: &mut [u8],
    client_id: u16,
) -> std::io::Result<Vec<u8>> {
    let mut response = upstream::exchange(upstream, query).await?;
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
