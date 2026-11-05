//! Read-only, lazy IPv4 membership testing. No verdicts or nft writes.
use crate::config::SetName;
use std::net::IpAddr;
use std::sync::Mutex;

// Kept under the server module so standalone forwarding integration tests can
// compile server.rs without adding unrelated crate-root module declarations.
#[allow(dead_code)]
#[path = "nftset.rs"]
mod nftset;

pub trait IpTest: Send + Sync {
    /// Errors are represented as non-china; IPv6 testing is outside P0.
    fn is_china(&self, ip: IpAddr) -> bool;
}

pub struct NftIpTest {
    name: Option<SetName>,
    socket: Mutex<Option<nftset::NftSet>>,
}

impl NftIpTest {
    pub fn new(name: Option<SetName>) -> Self {
        Self {
            name,
            socket: Mutex::new(None),
        }
    }
}

impl IpTest for NftIpTest {
    fn is_china(&self, ip: IpAddr) -> bool {
        let (Some(name), IpAddr::V4(ip)) = (&self.name, ip) else {
            return false;
        };
        let result = (|| {
            let name = format!("{}@{}@{}", name.family, name.table, name.set).parse()?;
            let mut socket = self
                .socket
                .lock()
                .map_err(|_| std::io::Error::other("IP test mutex poisoned"))?;
            if socket.is_none() {
                *socket = Some(nftset::NftSet::open()?);
            }
            socket.as_mut().expect("opened above").contains(&name, ip)
        })();
        match result {
            Ok(found) => found,
            Err(error) => {
                eprintln!("IP test: {error}; treating {ip} as non-china");
                false
            }
        }
    }
}
