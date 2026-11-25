use crate::config::{Protocol, Upstream};
use crate::dns;
use crate::upstream;
use std::future::{Future, poll_fn};
use std::io;
use std::sync::Arc;
use std::task::Poll;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// Shared cap on concurrent in-flight single-upstream exchange attempts for one
/// server instance (shared across client UDP/TCP, dual-group `none` queries,
/// and background cache refreshes).
///
/// Sized for OpenWrt home/SOHO routers: each active upstream attempt holds one
/// socket/stream and a 64 KiB receive buffer in `upstream::exchange`, while
/// client admission allows up to 256 UDP + 128 TCP concurrent queries and
/// multi-upstream / dual-group queries can fan out to 2–4+ attempts per query.
/// Capping concurrent upstream attempts at 256 bounds upstream attempt buffers
/// to ~16 MiB and keeps upstream socket count well below typical `ulimit -n`
/// (1024) while still supporting 128 concurrent 2-upstream queries or 64
/// concurrent 2+2 dual-group queries (above dnsmasq's default 150 forwards).
/// This cap governs upstream exchange attempts only and does not claim a
/// process-wide FD or RSS ceiling.
#[allow(dead_code)]
pub const DEFAULT_MAX_UPSTREAM_ATTEMPTS: usize = 256;

/// Application-level upstream attempt admission counters observed by tests via `watch`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub struct UpstreamAdmissionStats {
    pub admitted: usize,
    pub active: usize,
    pub peak_active: usize,
    pub rejected: usize,
}

/// Shared concurrency limiter for individual upstream exchange attempts.
#[derive(Clone)]
#[allow(dead_code)]
pub struct UpstreamLimiter {
    permits: Arc<Semaphore>,
    stats: Option<watch::Sender<UpstreamAdmissionStats>>,
}

struct UpstreamPermit {
    permit: Option<OwnedSemaphorePermit>,
    stats: Option<watch::Sender<UpstreamAdmissionStats>>,
}

impl Drop for UpstreamPermit {
    fn drop(&mut self) {
        // Release the semaphore permit before notifying observers of `active` drop.
        drop(self.permit.take());
        if let Some(stats) = &self.stats {
            stats.send_modify(|s| s.active -= 1);
        }
    }
}

impl UpstreamLimiter {
    #[allow(dead_code)]
    pub fn new(
        max_attempts: usize,
        stats: Option<watch::Sender<UpstreamAdmissionStats>>,
    ) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_attempts)),
            stats,
        }
    }

    fn try_acquire(&self) -> Option<UpstreamPermit> {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            if let Some(stats) = &self.stats {
                stats.send_modify(|s| s.rejected += 1);
            }
            return None;
        };
        if let Some(stats) = &self.stats {
            stats.send_modify(|s| {
                s.admitted += 1;
                s.active += 1;
                s.peak_active = s.peak_active.max(s.active);
            });
        }
        Some(UpstreamPermit {
            permit: Some(permit),
            stats: self.stats.clone(),
        })
    }
}

/// Race one exchange per upstream, preserving the single-upstream validation,
/// protocol selection and timeout. Dropping this future also cancels the group.
///
/// Exchanges are polled in this task (not spawned): returning the first usable
/// response (NOERROR or NXDOMAIN) drops every remaining future and its
/// socket/connection immediately. Validated DNS error replies are retained as
/// fallback so a fast SERVFAIL/REFUSED/extended error does not mask a slower
/// usable reply, while RCODE parse failures are recorded per upstream instead
/// of occupying the DNS error fallback slot.
#[allow(dead_code)]
pub async fn exchange(
    upstreams: &[Upstream],
    query: &[u8],
    client_protocol: Protocol,
) -> io::Result<Vec<u8>> {
    exchange_inner(upstreams, query, client_protocol, None).await
}

/// Race one exchange per upstream while enforcing a shared per-attempt
/// concurrency quota before allocating upstream I/O resources.
///
/// Rejected attempts never create an upstream future, socket/connection, or
/// buffer, and never mask a usable or fallback reply from already-admitted
/// attempts. If every attempt in the group is rejected, returns immediately
/// with `io::ErrorKind::WouldBlock`.
#[allow(dead_code)]
pub async fn exchange_with_limit(
    upstreams: &[Upstream],
    query: &[u8],
    client_protocol: Protocol,
    limiter: &UpstreamLimiter,
) -> io::Result<Vec<u8>> {
    exchange_inner(upstreams, query, client_protocol, Some(limiter)).await
}

async fn exchange_inner(
    upstreams: &[Upstream],
    query: &[u8],
    client_protocol: Protocol,
    limiter: Option<&UpstreamLimiter>,
) -> io::Result<Vec<u8>> {
    if upstreams.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty upstream group",
        ));
    }
    let mut pending = Vec::with_capacity(upstreams.len());
    let mut errors: Vec<_> = upstreams.iter().map(|_| None).collect();
    for (index, server) in upstreams.iter().enumerate() {
        let permit = match limiter {
            Some(limiter) => match limiter.try_acquire() {
                Some(permit) => Some(permit),
                None => {
                    errors[index] = Some(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "upstream concurrency limit reached",
                    ));
                    continue;
                }
            },
            None => None,
        };
        // Keep the future before the permit so dropping a tuple tears down its
        // socket/stream and buffer before releasing the concurrency slot.
        pending.push((
            index,
            Box::pin(upstream::exchange(server, query, client_protocol.clone())),
            permit,
        ));
    }
    if pending.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "upstream concurrency limit reached",
        ));
    }
    let mut last_error_kind = None;
    let mut fallback_response = None;
    poll_fn(|cx| {
        let mut index = 0;
        while index < pending.len() {
            match pending[index].1.as_mut().poll(cx) {
                Poll::Ready(Ok(response)) => {
                    let upstream_index = pending[index].0;
                    drop(pending.swap_remove(index));
                    match dns::rcode(&response) {
                        Ok(0 | 3) => return Poll::Ready(Ok(response)),
                        Ok(_) => {
                            fallback_response.get_or_insert(response);
                        }
                        Err(_) => {
                            let error = io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid DNS wire response",
                            );
                            last_error_kind = Some(error.kind());
                            errors[upstream_index] = Some(error);
                        }
                    }
                }
                Poll::Ready(Err(error)) => {
                    last_error_kind = Some(error.kind());
                    errors[pending[index].0] = Some(error);
                    drop(pending.swap_remove(index));
                }
                Poll::Pending => index += 1,
            }
        }
        if pending.is_empty() {
            if let Some(response) = fallback_response.take() {
                return Poll::Ready(Ok(response));
            }
            let failures: Vec<_> = upstreams
                .iter()
                .zip(&errors)
                .map(|(server, error)| {
                    let protocol = match server.protocol.as_ref().unwrap_or(&client_protocol) {
                        Protocol::Tcp => "tcp",
                        Protocol::Udp => "udp",
                        Protocol::Tls => "tls",
                    };
                    let address = std::net::SocketAddr::new(server.ip, server.port.unwrap_or(53));
                    let error = error.as_ref().expect("all upstreams failed");
                    format!("{protocol}://{address}: {error}")
                })
                .collect();
            Poll::Ready(Err(io::Error::new(
                last_error_kind.expect("nonempty admitted group completed"),
                format!("no upstream answered: {}", failures.join("; ")),
            )))
        } else {
            Poll::Pending
        }
    })
    .await
}
