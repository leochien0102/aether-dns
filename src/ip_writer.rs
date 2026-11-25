//! Best-effort, lazy IPv4 nftset writes. No cache refresh or IPv6 writes.
use crate::{
    config::{Config, SetName},
    dns,
    lists::RouteTag,
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

// Bound running + queued writes across all listeners, independently of reads.
const MAX_IP_WRITES: usize = 8;
static IP_WRITE_PERMITS: Semaphore = Semaphore::const_new(MAX_IP_WRITES);

// As with ip_test, keep standalone server integration tests self-contained.
#[allow(dead_code)]
#[path = "nftset.rs"]
mod nftset;

pub trait IpWriter: Send + Sync {
    fn add(&self, set: &SetName, ips: &[Ipv4Addr]) -> io::Result<()>;
}

#[derive(Default)]
pub struct NftWriter {
    socket: Mutex<Option<nftset::NftSet>>,
}

impl IpWriter for NftWriter {
    fn add(&self, set: &SetName, ips: &[Ipv4Addr]) -> io::Result<()> {
        if ips.is_empty() {
            return Ok(());
        }
        let name = format!("{}@{}@{}", set.family, set.table, set.set).parse()?;
        let mut socket = self
            .socket
            .lock()
            .map_err(|_| io::Error::other("IP writer mutex poisoned"))?;
        if socket.is_none() {
            *socket = Some(nftset::NftSet::open()?);
        }
        socket.as_mut().expect("opened above").add(&name, ips)
    }
}

pub struct NoopWriter;
impl IpWriter for NoopWriter {
    fn add(&self, _: &SetName, _: &[Ipv4Addr]) -> io::Result<()> {
        Ok(())
    }
}

/// Await an admitted attempt, but skip silently on overload. The job owns its
/// quota so cancellation cannot admit more work while the syscall is running.
pub async fn write_reply_offloaded(
    config: &Config,
    tag: RouteTag,
    response: &[u8],
    writer: Arc<dyn IpWriter>,
) {
    let Some((set, ips)) = reply_write(config, tag, response) else {
        return;
    };
    let Ok(permit) = IP_WRITE_PERMITS.try_acquire() else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        write(&set, &ips, &*writer);
    })
    .await;
}

/// Side effects only: parsing and backend errors never alter the DNS response.
#[cfg(test)]
pub fn write_reply(config: &Config, tag: RouteTag, response: &[u8], writer: &dyn IpWriter) {
    if let Some((set, ips)) = reply_write(config, tag, response) {
        write(&set, &ips, writer);
    }
}

fn reply_write(
    config: &Config,
    tag: RouteTag,
    response: &[u8],
) -> Option<(SetName, Vec<Ipv4Addr>)> {
    if response.len() < 12 || !matches!(response[3] & 15, 0 | 3) || response[2] & 2 != 0 {
        return None;
    }
    let pair = match tag {
        RouteTag::Gfw => config.add_taggfw_ip.as_ref(),
        RouteTag::Group(i) => config.groups.get(i).and_then(|g| g.ipset.as_ref()),
        RouteTag::Chn | RouteTag::None => None,
    };
    let set = pair.and_then(|pair| pair.v4.as_ref())?;
    if set.set.is_empty() || set.table.is_empty() {
        return None;
    }
    let inspection = dns::inspect(response).ok()?;
    let mut ips = Vec::new();
    for (ip, _) in inspection.answers {
        // P0 v6 sets are null; IPv6 writing is deliberately not implemented.
        let IpAddr::V4(ip) = ip else { continue };
        if !matches!(ip.octets()[0], 0 | 127) && !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    (!ips.is_empty()).then(|| (set.clone(), ips))
}

fn write(set: &SetName, ips: &[Ipv4Addr], writer: &dyn IpWriter) {
    if let Err(error) = writer.add(set, ips) {
        eprintln!(
            "IP writer {}@{}@{}: {error}",
            set.family, set.table, set.set
        );
    }
}
