//! Best-effort, lazy IPv4 nftset writes. No cache refresh or IPv6 writes.
use crate::{
    config::{Config, SetName},
    dns,
    lists::RouteTag,
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr},
    sync::Mutex,
};

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

/// Side effects only: parsing and backend errors never alter the DNS response.
pub fn write_reply(config: &Config, tag: RouteTag, response: &[u8], writer: &dyn IpWriter) {
    if response.len() < 12 || !matches!(response[3] & 15, 0 | 3) || response[2] & 2 != 0 {
        return;
    }
    let pair = match tag {
        RouteTag::Gfw => config.add_taggfw_ip.as_ref(),
        RouteTag::Group(i) => config.groups.get(i).and_then(|g| g.ipset.as_ref()),
        RouteTag::Chn | RouteTag::None => None,
    };
    let Some(set) = pair.and_then(|pair| pair.v4.as_ref()) else {
        return;
    };
    if set.set.is_empty() || set.table.is_empty() {
        return;
    }
    let Ok(inspection) = dns::inspect(response) else {
        return;
    };
    let mut ips = Vec::new();
    for (ip, _) in inspection.answers {
        // P0 v6 sets are null; IPv6 writing is deliberately not implemented.
        let IpAddr::V4(ip) = ip else { continue };
        if !matches!(ip.octets()[0], 0 | 127) && !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    if !ips.is_empty()
        && let Err(error) = writer.add(set, &ips)
    {
        eprintln!(
            "IP writer {}@{}@{}: {error}",
            set.family, set.table, set.set
        );
    }
}
