use crate::{
    config::{Config, Protocol, Upstream},
    dns,
    lists::{RouteTag, Router},
    upstream_group,
};

// Standalone integration targets have no crate-root nftset module. The reader
// and writer each include the unchanged M2-03 backend in their own namespace.
#[allow(clippy::duplicate_mod)]
#[path = "ip_test.rs"]
pub mod ip_test;
use ip_test::{IpTest, NftIpTest};
#[path = "ip_writer.rs"]
pub mod ip_writer;
use ip_writer::{IpWriter, NftWriter, NoopWriter};

#[path = "cache.rs"]
pub mod cache;
#[path = "verdict.rs"]
pub mod verdict;

struct Routing {
    verdict: std::sync::Mutex<verdict::VerdictCache>,
    cache: std::sync::Mutex<cache::Cache>,
    clock: Arc<dyn cache::Clock>,
    id_source: Arc<dyn UpstreamIdSource>,
    refresh_tasks: std::sync::Mutex<Option<JoinSet<()>>>,
    upstream_limiter: upstream_group::UpstreamLimiter,
    config: Config,
    router: Router,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
}

impl Routing {
    fn route(&self, query: &[u8]) -> std::io::Result<(RouteTag, hickory_proto::rr::RecordType)> {
        let inspection = dns::inspect(query).map_err(|_| wire_error(dns::WireError))?;
        let (name, qtype, _) = inspection
            .question
            .ok_or_else(|| wire_error(dns::WireError))?;
        let name = name.to_ascii();
        let tag = self.router.resolve(name.strip_suffix('.').unwrap_or(&name));
        Ok((tag, qtype))
    }

    fn upstreams(&self, tag: RouteTag) -> &[Upstream] {
        match tag {
            RouteTag::Chn => &self.config.china_dns,
            RouteTag::Gfw => &self.config.trust_dns,
            RouteTag::Group(i) => self
                .config
                .groups
                .get(i)
                .map_or(&[], |g| g.upstream.as_slice()),
            RouteTag::None => unreachable!("none uses dual exchange"),
        }
    }
}
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::{Duration, timeout};

/// Shared cap on concurrent client UDP query tasks across all listeners.
///
/// Sized for OpenWrt home/SOHO routers: UDP is the primary LAN DNS transport
/// and browsers/clients emit parallel bursts (e.g. A/AAAA/HTTPS), so 256
/// provides higher burst headroom than TCP (128) and dnsmasq's default
/// `dns-forward-max` (150) while bounding concurrent client-side UDP tasks.
/// This cap governs client UDP admission only: an upstream group may
/// concurrently attempt multiple upstreams per query, so this task does not
/// cap total upstream attempts or promise a process-wide FD or memory ceiling.
pub const DEFAULT_MAX_UDP_QUERIES: usize = 256;

/// Shared cap on concurrent client TCP connection tasks across all listeners.
///
/// Sized for OpenWrt routers (typical 128–512 MiB RAM, default `ulimit -n` 1024):
/// each active TCP client holds 1 client FD (+ up to a few upstream FDs) and at
/// most ~128 KiB worst-case wire buffers (64 KiB query + 64 KiB reply), so 128
/// keeps worst-case memory bounded (~16 MiB) and FDs well below `RLIMIT_NOFILE`,
/// while leaving ample headroom for normal LAN DNS-over-TCP bursts (comparable to
/// BIND9 `tcp-clients` 150 / dnsmasq 150).
pub const DEFAULT_MAX_TCP_CONNECTIONS: usize = 128;

/// Application-level admission counters observed by tests via `watch`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub struct AdmissionStats {
    pub admitted: usize,
    pub active: usize,
    pub peak_active: usize,
    pub rejected: usize,
}

#[allow(dead_code)]
pub type TcpAdmissionStats = AdmissionStats;
#[allow(dead_code)]
pub type UdpAdmissionStats = AdmissionStats;
#[allow(dead_code)]
pub use crate::upstream_group::{DEFAULT_MAX_UPSTREAM_ATTEMPTS, UpstreamAdmissionStats};

const MAX_ID_COLLISION_RETRIES: usize = 16;

/// Source of unpredictable 16-bit upstream DNS transaction IDs.
pub trait UpstreamIdSource: Send + Sync {
    fn next_u16(&self) -> std::io::Result<u16>;
}

/// Production ID source backed directly by the Linux kernel CSPRNG (`getrandom(2)`).
/// Never falls back to a counter, timestamp, or other predictable value on failure.
#[derive(Default)]
pub struct OsRandomIdSource;

impl UpstreamIdSource for OsRandomIdSource {
    fn next_u16(&self) -> std::io::Result<u16> {
        let mut bytes = [0u8; 2];
        let mut filled = 0;
        while filled < bytes.len() {
            let ret = unsafe {
                libc::getrandom(
                    bytes.as_mut_ptr().add(filled) as *mut libc::c_void,
                    bytes.len() - filled,
                    0,
                )
            };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if ret == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "getrandom returned 0 bytes",
                ));
            }
            filled += ret as usize;
        }
        Ok(u16::from_be_bytes(bytes))
    }
}

/// Draw a 16-bit upstream transaction ID from `source`, redrawing if it
/// collides with `avoid_id` (the client's transaction ID). Fails closed on
/// source error or if bounded retries are exhausted; never degrades to a
/// predictable fallback.
pub fn select_upstream_id(
    source: &dyn UpstreamIdSource,
    avoid_id: Option<u16>,
) -> std::io::Result<u16> {
    for _ in 0..MAX_ID_COLLISION_RETRIES {
        let id = source.next_u16()?;
        if Some(id) != avoid_id {
            return Ok(id);
        }
    }
    Err(std::io::Error::other(
        "upstream ID source repeatedly collided with client ID",
    ))
}

struct AdmissionPermit {
    permit: Option<OwnedSemaphorePermit>,
    stats: Option<watch::Sender<AdmissionStats>>,
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        // Release the semaphore permit before notifying observers of `active` drop.
        drop(self.permit.take());
        if let Some(stats) = &self.stats {
            stats.send_modify(|s| s.active -= 1);
        }
    }
}

pub async fn run(
    config: Config,
    shutdown: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ip_test = Arc::new(NftIpTest::new(config.ipset_name4.clone()));
    run_with_ip_writer(config, shutdown, ip_test, Arc::new(NftWriter::default())).await
}

/// Injection point for deterministic membership tests, independent of nft tables.
#[allow(dead_code)]
pub async fn run_with_ip_test(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_ip_writer(config, shutdown, ip_test, Arc::new(NoopWriter)).await
}

/// Injection point for deterministic best-effort writes without nft tables.
pub async fn run_with_ip_writer(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_cache_clock(
        config,
        shutdown,
        ip_test,
        ip_writer,
        Arc::new(cache::MonotonicClock::default()),
    )
    .await
}

/// The production clock is monotonic; tests can advance it without sleeping.
#[allow(dead_code)]
pub async fn run_with_cache_clock(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_limits(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        DEFAULT_MAX_UDP_QUERIES,
        None,
        DEFAULT_MAX_TCP_CONNECTIONS,
        None,
    )
    .await
}

/// Injection point for deterministic TCP admission tests with a small shared quota.
#[allow(dead_code)]
pub async fn run_with_tcp_limit(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    max_tcp_connections: usize,
    tcp_stats: Option<watch::Sender<TcpAdmissionStats>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_limits(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        DEFAULT_MAX_UDP_QUERIES,
        None,
        max_tcp_connections,
        tcp_stats,
    )
    .await
}

/// Injection point for deterministic UDP/TCP admission tests with small shared quotas.
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn run_with_limits(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    max_udp_queries: usize,
    udp_stats: Option<watch::Sender<UdpAdmissionStats>>,
    max_tcp_connections: usize,
    tcp_stats: Option<watch::Sender<TcpAdmissionStats>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_all_limits(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        max_udp_queries,
        udp_stats,
        max_tcp_connections,
        tcp_stats,
        DEFAULT_MAX_UPSTREAM_ATTEMPTS,
        None,
    )
    .await
}

/// Injection point for deterministic upstream-attempt admission tests with a small shared quota.
#[allow(dead_code)]
pub async fn run_with_upstream_limit(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    max_upstream_attempts: usize,
    upstream_stats: Option<watch::Sender<UpstreamAdmissionStats>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_all_limits(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        DEFAULT_MAX_UDP_QUERIES,
        None,
        DEFAULT_MAX_TCP_CONNECTIONS,
        None,
        max_upstream_attempts,
        upstream_stats,
    )
    .await
}

/// Injection point for deterministic upstream transaction ID tests.
#[allow(dead_code)]
pub async fn run_with_id_source(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    id_source: Arc<dyn UpstreamIdSource>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_inner(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        id_source,
        DEFAULT_MAX_UDP_QUERIES,
        None,
        DEFAULT_MAX_TCP_CONNECTIONS,
        None,
        DEFAULT_MAX_UPSTREAM_ATTEMPTS,
        None,
    )
    .await
}

/// Injection point for deterministic admission tests across UDP, TCP, and upstream attempts.
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn run_with_all_limits(
    config: Config,
    shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    max_udp_queries: usize,
    udp_stats: Option<watch::Sender<UdpAdmissionStats>>,
    max_tcp_connections: usize,
    tcp_stats: Option<watch::Sender<TcpAdmissionStats>>,
    max_upstream_attempts: usize,
    upstream_stats: Option<watch::Sender<UpstreamAdmissionStats>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_inner(
        config,
        shutdown,
        ip_test,
        ip_writer,
        clock,
        Arc::new(OsRandomIdSource),
        max_udp_queries,
        udp_stats,
        max_tcp_connections,
        tcp_stats,
        max_upstream_attempts,
        upstream_stats,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    config: Config,
    mut shutdown: watch::Receiver<bool>,
    ip_test: Arc<dyn IpTest>,
    ip_writer: Arc<dyn IpWriter>,
    clock: Arc<dyn cache::Clock>,
    id_source: Arc<dyn UpstreamIdSource>,
    max_udp_queries: usize,
    udp_stats: Option<watch::Sender<UdpAdmissionStats>>,
    max_tcp_connections: usize,
    tcp_stats: Option<watch::Sender<TcpAdmissionStats>>,
    max_upstream_attempts: usize,
    upstream_stats: Option<watch::Sender<UpstreamAdmissionStats>>,
) -> Result<(), Box<dyn std::error::Error>> {
    config.validate_startup()?;
    let router = Router::load(&config);
    let addresses = if config.bind_addr.is_empty() {
        vec![Ipv4Addr::LOCALHOST]
    } else {
        config.bind_addr.clone()
    };
    let ports = if config.bind_port.is_empty() {
        vec![53]
    } else {
        config.bind_port.clone()
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
    let upstream = Arc::new(Routing {
        cache: std::sync::Mutex::new(cache::Cache::new(config.cache, config.cache_stale)),
        verdict: std::sync::Mutex::new(verdict::VerdictCache::new(config.verdict_cache)),
        clock,
        id_source,
        refresh_tasks: std::sync::Mutex::new(Some(JoinSet::new())),
        upstream_limiter: upstream_group::UpstreamLimiter::new(
            max_upstream_attempts,
            upstream_stats,
        ),
        config,
        router,
        ip_test,
        ip_writer,
    });
    let mut tasks = JoinSet::new();
    // All UDP listeners share one permit pool, independent of TCP permits.
    // Permits are acquired via try_acquire_owned after recv_from() and before
    // copying the datagram or spawning a query task, so idle listeners hold
    // zero permits and overload drops immediately without queuing or logging.
    let udp_permits = Arc::new(Semaphore::new(max_udp_queries));
    for socket in sockets {
        println!("listening UDP {}", socket.local_addr()?);
        let upstream = upstream.clone();
        let mut shutdown = shutdown.clone();
        let udp_permits = udp_permits.clone();
        let udp_stats = udp_stats.clone();
        tasks.spawn(async move {
            let mut queries = JoinSet::new();
            let mut buffer = vec![0; 65535];
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
                let packet = &buffer[..len];
                if dns::check_query(packet).is_err() {
                    continue;
                }
                let Ok(permit) = udp_permits.clone().try_acquire_owned() else {
                    if let Some(stats) = &udp_stats {
                        stats.send_modify(|s| s.rejected += 1);
                    }
                    continue;
                };
                if let Some(stats) = &udp_stats {
                    stats.send_modify(|s| {
                        s.admitted += 1;
                        s.active += 1;
                        s.peak_active = s.peak_active.max(s.active);
                    });
                }
                let permit = AdmissionPermit {
                    permit: Some(permit),
                    stats: udp_stats.clone(),
                };
                let query = packet.to_vec();
                let client_id = u16::from_be_bytes([query[0], query[1]]);
                let socket = socket.clone();
                let upstream = upstream.clone();
                queries.spawn(async move {
                    let _permit = permit;
                    let udp_budget = dns::udp_budget(&query);
                    if let Some(mut response) =
                        handle_query(&upstream, query, client_id, Protocol::Udp, peer).await
                    {
                        dns::enforce_udp_limit(&mut response, udp_budget);
                        send(&socket, peer, &response).await;
                    }
                });
            }
            // Keep the socket only for replies, never receive during draining.
            while queries.join_next().await.is_some() {}
        });
    }
    // Keep all UDP readiness lines ahead of TCP readiness lines.
    // All TCP listeners share one permit pool. Permits are acquired via
    // try_acquire_owned only after accept(), so idle listeners hold zero permits
    // and overload sheds immediately (close without spawning a waiting task).
    let tcp_permits = Arc::new(Semaphore::new(max_tcp_connections));
    for listener in listeners {
        println!("listening TCP {}", listener.local_addr()?);
        let upstream = upstream.clone();
        let mut shutdown = shutdown.clone();
        let tcp_permits = tcp_permits.clone();
        let tcp_stats = tcp_stats.clone();
        tasks.spawn(async move {
            let mut queries = JoinSet::new();
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
                let Ok(permit) = tcp_permits.clone().try_acquire_owned() else {
                    drop(stream);
                    if let Some(stats) = &tcp_stats {
                        stats.send_modify(|s| s.rejected += 1);
                    }
                    continue;
                };
                if let Some(stats) = &tcp_stats {
                    stats.send_modify(|s| {
                        s.admitted += 1;
                        s.active += 1;
                        s.peak_active = s.peak_active.max(s.active);
                    });
                }
                let permit = AdmissionPermit {
                    permit: Some(permit),
                    stats: tcp_stats.clone(),
                };
                let upstream = upstream.clone();
                let shutdown = shutdown.clone();
                queries.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = tcp_query(stream, &upstream, peer, shutdown).await {
                        eprintln!("TCP client {peer}: {error}");
                    }
                });
            }
            drop(listener);
            while queries.join_next().await.is_some() {}
        });
    }
    // Stream contract: lifecycle lines (readiness, shutdown) go to stdout so procd
    // records them as daemon.info; genuine errors stay on stderr (daemon.err).
    shutdown.changed().await?;
    // Drop cancels all refreshes; None also prevents spawning during query drain.
    upstream.refresh_tasks.lock().unwrap().take();
    println!("shutdown: draining queries (500 ms maximum)");
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
    upstream: &Arc<Routing>,
    peer: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    // An accepted but idle/partially framed connection is not yet an in-flight
    // query. Stop reading it on shutdown; only fully received queries may drain.
    let query = tokio::select! {
        biased;
        _ = shutdown.changed() => return Ok(()),
        // One budget covers both the length prefix and the entire body.
        query = timeout(Duration::from_secs(5), async {
            let len = stream.read_u16().await? as usize;
            let mut query = vec![0; len];
            stream.read_exact(&mut query).await?;
            Ok::<_, std::io::Error>(query)
        }) => match query {
            Ok(query) => query?,
            Err(_) => return Ok(()), // Drop the stream: EOF, no reply or upstream work.
        },
    };
    if dns::check_query(&query).is_err() {
        return Ok(());
    }
    let client_id = u16::from_be_bytes([query[0], query[1]]);
    if let Some(response) = handle_query(upstream, query, client_id, Protocol::Tcp, peer).await {
        stream.write_u16(response.len() as u16).await?;
        stream.write_all(&response).await?;
    }
    stream.shutdown().await
}

async fn handle_query(
    upstream: &Arc<Routing>,
    mut query: Vec<u8>,
    client_id: u16,
    client_protocol: Protocol,
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    // Both callers have passed check_query. Clear client TC before filtering,
    // routing, cache/verdict lookup or upstream exchange, as in check_msg.
    dns::clear_tc(&mut query).ok()?;
    // Read the wire qtype before any list lookup, ID rewrite or IP test.
    let qtype_at = dns::skip_name(&query, 12).ok()?;
    let qtype = u16::from_be_bytes([query[qtype_at], query[qtype_at + 1]]);
    if (upstream.config.no_ipv6 > 0 && qtype == 28) || upstream.config.filter_qtype.contains(&qtype)
    {
        dns::synthetic_nodata(&mut query).ok()?;
        return Some(query);
    }
    let original = query.clone();
    match forward(upstream, &query, client_id, client_protocol).await {
        Ok(response) => Some(response),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                println!("upstream failed for client {peer}: {error}");
            }
            let mut response = original;
            dns::synthetic_nodata(&mut response).ok()?;
            response[3] = (response[3] & 0xf0) | 2;
            Some(response)
        }
    }
}

async fn forward(
    upstream: &Arc<Routing>,
    query: &[u8],
    client_id: u16,
    client_protocol: Protocol,
) -> std::io::Result<Vec<u8>> {
    let (tag, _) = upstream.route(query)?;
    let (cached, refresh) = {
        let mut cache = upstream.cache.lock().unwrap();
        let now = upstream.clock.now();
        let cached = cache.get(query, now);
        let refresh = cached.is_some() && cache.begin_refresh(query, now);
        (cached, refresh)
    };
    if let Some(mut response) = cached {
        if refresh {
            let routing = upstream.clone();
            let query = query.to_vec();
            let mut tasks = upstream.refresh_tasks.lock().unwrap();
            if let Some(tasks) = tasks.as_mut() {
                while tasks.try_join_next().is_some() {}
                tasks.spawn(async move {
                    let response =
                        exchange_response(&routing, &query, Some(client_id), client_protocol).await;
                    routing.cache.lock().unwrap().finish_refresh(
                        &query,
                        response.as_deref().ok(),
                        routing.clock.now(),
                    );
                });
            }
        }
        dns::set_id(&mut response, client_id).map_err(wire_error)?;
        ip_writer::write_reply_offloaded(
            &upstream.config,
            tag,
            &response,
            upstream.ip_writer.clone(),
        )
        .await;
        return Ok(response);
    }
    let mut response = exchange_response(upstream, query, Some(client_id), client_protocol).await?;
    dns::set_id(&mut response, client_id).map_err(wire_error)?;
    upstream
        .cache
        .lock()
        .unwrap()
        .add(query, &response, upstream.clock.now());
    ip_writer::write_reply_offloaded(&upstream.config, tag, &response, upstream.ip_writer.clone())
        .await;
    Ok(response)
}

/// Shared route/exchange and normalization; never consult the answer cache here.
/// Every logical upstream query allocates a fresh unpredictable 16-bit ID here.
async fn exchange_response(
    upstream: &Routing,
    query: &[u8],
    avoid_id: Option<u16>,
    client_protocol: Protocol,
) -> std::io::Result<Vec<u8>> {
    let (tag, qtype) = upstream.route(query)?;
    let upstream_id = select_upstream_id(&*upstream.id_source, avoid_id)?;
    let mut upstream_query = query.to_vec();
    dns::set_id(&mut upstream_query, upstream_id).map_err(wire_error)?;
    let mut response = if tag == RouteTag::None {
        none_exchange(upstream, &upstream_query, qtype, client_protocol).await?
    } else {
        upstream_group::exchange_with_limit(
            upstream.upstreams(tag),
            &upstream_query,
            client_protocol,
            &upstream.upstream_limiter,
        )
        .await?
    };
    dns::set_ra(&mut response).map_err(wire_error)?;
    if matches!(dns::rcode(&response).map_err(wire_error)?, 0 | 3) && response[2] & 2 == 0 {
        dns::truncate_additional(&mut response).map_err(wire_error)?;
    }
    Ok(response)
}

// The backend serializes Netlink reads. Bound running + queued reads across
// all listeners/server instances, rather than filling Tokio's blocking queue.
const MAX_IP_TESTS: usize = 8;
static IP_TEST_PERMITS: Semaphore = Semaphore::const_new(MAX_IP_TESTS);

// Only a concrete IP decision is reusable; TC/no-IP/other_case are not.
async fn use_china_reply(response: &[u8], routing: &Routing, qname: &[u8]) -> bool {
    // Like dns_test_ip / use_china_reply: non-good replies are other_case.
    if response[2] & 2 != 0 {
        return true;
    }
    if !matches!(response[3] & 15, 0 | 3) {
        return false;
    }
    let ip = dns::inspect(response)
        .ok()
        .and_then(|inspection| inspection.answers.first().copied());
    let Some((ip, _)) = ip else {
        return false;
    };
    let accepted = if ip.is_ipv4() {
        let Ok(permit) = IP_TEST_PERMITS.try_acquire() else {
            // Local overload is not a non-member verdict. Fall back silently.
            return false;
        };
        let ip_test = routing.ip_test.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Cancellation of the async waiter must not release this quota.
            let _permit = permit;
            ip_test.is_china(ip)
        })
        .await;
        let Ok(accepted) = result else {
            return false;
        };
        accepted
    } else {
        false
    };
    routing.verdict.lock().unwrap().add(qname, accepted);
    accepted
}

async fn none_exchange(
    routing: &Routing,
    query: &[u8],
    qtype: hickory_proto::rr::RecordType,
    protocol: Protocol,
) -> std::io::Result<Vec<u8>> {
    use hickory_proto::rr::RecordType;
    let config = &routing.config;
    if !matches!(qtype, RecordType::A | RecordType::AAAA) {
        let all: Vec<_> = config
            .china_dns
            .iter()
            .chain(&config.trust_dns)
            .cloned()
            .collect();
        return upstream_group::exchange_with_limit(
            &all,
            query,
            protocol,
            &routing.upstream_limiter,
        )
        .await;
    }
    let qname_end = dns::skip_name(query, 12).map_err(wire_error)?;
    let qname = &query[12..qname_end];
    let verdict = routing.verdict.lock().unwrap().get(qname);
    if let Some(is_china) = verdict {
        let side = if is_china {
            &config.china_dns
        } else {
            &config.trust_dns
        };
        // A cached verdict is authoritative: failure must not query the other side.
        return upstream_group::exchange_with_limit(
            side,
            query,
            protocol,
            &routing.upstream_limiter,
        )
        .await;
    }
    if config.china_dns.is_empty() {
        return upstream_group::exchange_with_limit(
            &config.trust_dns,
            query,
            protocol,
            &routing.upstream_limiter,
        )
        .await;
    }
    if config.trust_dns.is_empty() {
        return upstream_group::exchange_with_limit(
            &config.china_dns,
            query,
            protocol,
            &routing.upstream_limiter,
        )
        .await;
    }
    let china = upstream_group::exchange_with_limit(
        &config.china_dns,
        query,
        protocol.clone(),
        &routing.upstream_limiter,
    );
    let trust = upstream_group::exchange_with_limit(
        &config.trust_dns,
        query,
        protocol,
        &routing.upstream_limiter,
    );
    tokio::pin!(china, trust);
    tokio::select! {
        china_result = &mut china => {
            if let Ok(reply) = &china_result
                && use_china_reply(reply, routing, qname).await
            {
                return china_result;
            }
            // A failed side cannot hide the other side's legal response, and
            // a quota-rejected side must not mask an actual exchange error.
            merge_dual_results(trust.await, china_result)
        }
        trust_result = &mut trust => {
            // Trust arriving first is held until china decides (or fails).
            let china_result = china.await;
            if let Ok(reply) = &china_result
                && use_china_reply(reply, routing, qname).await
            {
                china_result
            } else {
                merge_dual_results(trust_result, china_result)
            }
        }
    }
}

/// Merge dual-group (`none`) results after `china` was not selected via
/// `use_china_reply`. Preserves any `Ok` reply (including DNS error replies)
/// and, when both sides fail, keeps an actual upstream exchange error over a
/// `WouldBlock` concurrency rejection regardless of side or completion order.
#[allow(dead_code)]
pub fn merge_dual_results(
    trust: std::io::Result<Vec<u8>>,
    china: std::io::Result<Vec<u8>>,
) -> std::io::Result<Vec<u8>> {
    match (trust, china) {
        (Ok(reply), _) | (Err(_), Ok(reply)) => Ok(reply),
        (Err(trust_err), Err(china_err)) => {
            if china_err.kind() == std::io::ErrorKind::WouldBlock
                && trust_err.kind() != std::io::ErrorKind::WouldBlock
            {
                Err(trust_err)
            } else {
                Err(china_err)
            }
        }
    }
}

/// Test-only helper to invoke `exchange_response` directly with a small
/// upstream attempt limiter and observe the exact `io::Result`.
#[allow(dead_code)]
pub async fn exchange_once_with_limit(
    config: Config,
    query: &[u8],
    client_protocol: Protocol,
    limiter: upstream_group::UpstreamLimiter,
) -> std::io::Result<Vec<u8>> {
    let router = Router::load(&config);
    let routing = Routing {
        verdict: std::sync::Mutex::new(verdict::VerdictCache::new(config.verdict_cache)),
        cache: std::sync::Mutex::new(cache::Cache::new(config.cache, config.cache_stale)),
        clock: Arc::new(cache::MonotonicClock::default()),
        id_source: Arc::new(OsRandomIdSource),
        refresh_tasks: std::sync::Mutex::new(None),
        upstream_limiter: limiter,
        config,
        router,
        ip_test: Arc::new(NftIpTest::new(None)),
        ip_writer: Arc::new(NoopWriter),
    };
    let avoid_id = query.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    exchange_response(&routing, query, avoid_id, client_protocol).await
}

fn wire_error(_: dns::WireError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid DNS wire response")
}

async fn send(socket: &UdpSocket, peer: SocketAddr, response: &[u8]) {
    if let Err(error) = socket.send_to(response, peer).await {
        eprintln!("UDP send {peer}: {error}");
    }
}
