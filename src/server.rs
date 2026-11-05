use crate::{config::Config, config::Protocol, dns, upstream};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Duration, timeout};

pub async fn run(
    config: Config,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let mut tasks = JoinSet::new();
    for socket in sockets {
        eprintln!("listening UDP {}", socket.local_addr()?);
        let upstream = upstream.clone();
        let mut shutdown = shutdown.clone();
        tasks.spawn(async move {
            let mut queries = JoinSet::new();
            let mut buffer = vec![0; 65535];
            let mut next_id = 0u16;
            loop {
                let received = tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = queries.join_next(), if !queries.is_empty() => continue,
                    received = socket.recv_from(&mut buffer) => received,
                };
                let (len, peer) = match received {
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
                queries.spawn(async move {
                    if let Some(response) =
                        handle_query(&upstream, query, client_id, id, Protocol::Udp, peer).await
                    {
                        send(&socket, peer, &response).await;
                    }
                });
            }
            // Keep the socket only for replies, never receive during draining.
            while queries.join_next().await.is_some() {}
        });
    }
    // Keep all UDP readiness lines ahead of TCP readiness lines.
    for listener in listeners {
        eprintln!("listening TCP {}", listener.local_addr()?);
        let upstream = upstream.clone();
        let mut shutdown = shutdown.clone();
        tasks.spawn(async move {
            let mut queries = JoinSet::new();
            let mut next_id = 0u16;
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = queries.join_next(), if !queries.is_empty() => continue,
                    accepted = listener.accept() => accepted,
                };
                let (stream, peer) = match accepted {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("TCP accept: {error}");
                        continue;
                    }
                };
                next_id = next_id.wrapping_add(1);
                let id = next_id;
                let upstream = upstream.clone();
                let shutdown = shutdown.clone();
                queries.spawn(async move {
                    if let Err(error) = tcp_query(stream, &upstream, id, peer, shutdown).await {
                        eprintln!("TCP client {peer}: {error}");
                    }
                });
            }
            drop(listener);
            while queries.join_next().await.is_some() {}
        });
    }
    shutdown.changed().await?;
    eprintln!("shutdown: draining queries (500 ms maximum)");
    // One shared budget, not 500 ms per listener or per query. Dropping the
    // listener JoinSet aborts its query JoinSet as well, discarding late work.
    let _ = timeout(Duration::from_millis(500), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

// M1: exactly one framed query and response per client connection.
async fn tcp_query(
    mut stream: TcpStream,
    upstream: &crate::config::Upstream,
    mut id: u16,
    peer: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    // An accepted but idle/partially framed connection is not yet an in-flight
    // query. Stop reading it on shutdown; only fully received queries may drain.
    let query = tokio::select! {
        biased;
        _ = shutdown.changed() => return Ok(()),
        query = async {
            let len = stream.read_u16().await? as usize;
            let mut query = vec![0; len];
            stream.read_exact(&mut query).await?;
            Ok::<_, std::io::Error>(query)
        } => query?,
    };
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
