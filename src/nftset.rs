//! Name-based, synchronous nfnetlink access to IPv4 interval sets.
//! The owner serializes operations through `&mut self`; no set handles are cached.
use std::io;
use std::mem::{size_of, zeroed};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::str::FromStr;

const REQUEST: u16 = 1;
const ACK: u16 = 4;
const NEW: u16 = (10 << 8) | 12;
const GET: u16 = (10 << 8) | 13;
const NESTED: u16 = 0x8000;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// A family/table/set name, resolved afresh by the kernel on each request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetName {
    family: u8,
    table: String,
    set: String,
}

impl FromStr for SetName {
    type Err = io::Error;

    fn from_str(value: &str) -> io::Result<Self> {
        let parts: Vec<_> = value.split('@').collect();
        if parts.len() != 3
            || parts[1..]
                .iter()
                .any(|s| s.is_empty() || s.len() > 255 || s.contains('\0'))
        {
            return Err(invalid("expected family@table@set with nonempty names"));
        }
        let family = match parts[0] {
            "inet" => 1,
            "ip" => 2,
            "ip6" => 10,
            _ => return Err(invalid("unsupported nft family")),
        };
        Ok(Self {
            family,
            table: parts[1].into(),
            set: parts[2].into(),
        })
    }
}

fn align(n: usize) -> usize {
    (n + 3) & !3
}

fn attribute(kind: u16, body: &[u8]) -> io::Result<Vec<u8>> {
    let length =
        u16::try_from(body.len() + 4).map_err(|_| invalid("netlink attribute too large"))?;
    let mut out = Vec::with_capacity(align(length as usize));
    out.extend_from_slice(&length.to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(body);
    out.resize(align(out.len()), 0);
    Ok(out)
}

fn element(ip: u32, end: bool) -> io::Result<Vec<u8>> {
    let value = attribute(1 | 0x4000, &ip.to_be_bytes())?;
    let mut body = attribute(1 | NESTED, &value)?;
    if end {
        body.extend(attribute(3, &1u32.to_be_bytes())?);
    }
    attribute(1 | NESTED, &body)
}

fn message(kind: u16, flags: u16, seq: u32, family: u8, resource: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + body.len());
    out.extend_from_slice(&((20 + body.len()) as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(&flags.to_ne_bytes());
    out.extend_from_slice(&seq.to_ne_bytes());
    out.extend_from_slice(&0u32.to_ne_bytes());
    out.extend_from_slice(&[family, 0]);
    out.extend_from_slice(&resource.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn body(set: &SetName, ips: &[Ipv4Addr], intervals: bool) -> io::Result<Vec<u8>> {
    let mut out = attribute(1, &[set.table.as_bytes(), &[0]].concat())?;
    out.extend(attribute(2, &[set.set.as_bytes(), &[0]].concat())?);
    let mut elements = Vec::new();
    for ip in ips {
        let key = u32::from(*ip);
        elements.extend(element(key, false)?);
        if intervals {
            elements.extend(element(key.wrapping_add(1), true)?);
        }
    }
    out.extend(attribute(3 | NESTED, &elements)?);
    Ok(out)
}

/// Kernel errno values are preserved in `io::Error::raw_os_error()` (EPERM,
/// ENOENT, EEXIST included). Membership ENOENT means absent, as in IMP-004.
pub struct NftSet {
    fd: OwnedFd,
    sequence: u32,
}

impl NftSet {
    pub fn open() -> io::Result<Self> {
        // SAFETY: socket returns a fresh descriptor, immediately owned by OwnedFd.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_NETFILTER,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: zero is a valid sockaddr_nl initialization; pointers and lengths
        // below refer to live, correctly sized structures throughout each syscall.
        unsafe {
            let mut address: libc::sockaddr_nl = zeroed();
            address.nl_family = libc::AF_NETLINK as u16;
            if libc::bind(
                fd.as_raw_fd(),
                &address as *const _ as *const libc::sockaddr,
                size_of::<libc::sockaddr_nl>() as _,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            let timeout = libc::timeval {
                tv_sec: 1,
                tv_usec: 0,
            };
            if libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                size_of::<libc::timeval>() as _,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { fd, sequence: 0 })
    }

    fn next(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn exchange(
        &self,
        bytes: &[u8],
        expected: u32,
        errors: &[u32],
        found_reply: bool,
    ) -> io::Result<()> {
        // SAFETY: buffers are valid for their respective lengths. The zeroed
        // destination is a kernel netlink address (pid/groups zero).
        let sent = unsafe {
            let mut kernel: libc::sockaddr_nl = zeroed();
            kernel.nl_family = libc::AF_NETLINK as u16;
            libc::sendto(
                self.fd.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                &kernel as *const _ as *const libc::sockaddr,
                size_of::<libc::sockaddr_nl>() as _,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        if sent as usize != bytes.len() {
            return Err(io::Error::other("short netlink send"));
        }
        let mut buffer = vec![0u8; 65536];
        loop {
            // MSG_TRUNC reports the original size, allowing truncation detection.
            let received = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_TRUNC,
                )
            };
            if received < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if received == 0 || received as usize > buffer.len() {
                return Err(io::Error::other("truncated netlink reply"));
            }
            let mut remaining = &buffer[..received as usize];
            while !remaining.is_empty() {
                if remaining.len() < 16 {
                    return Err(io::Error::other("short netlink header"));
                }
                let length = u32::from_ne_bytes(remaining[..4].try_into().unwrap()) as usize;
                if length < 16 || length > remaining.len() {
                    return Err(io::Error::other("invalid netlink length"));
                }
                let kind = u16::from_ne_bytes(remaining[4..6].try_into().unwrap());
                let seq = u32::from_ne_bytes(remaining[8..12].try_into().unwrap());
                if seq == expected || (kind == 2 && errors.contains(&seq)) {
                    if kind == 2 {
                        if length < 20 {
                            return Err(io::Error::other("short netlink error"));
                        }
                        let code = i32::from_ne_bytes(remaining[16..20].try_into().unwrap());
                        if code != 0 {
                            return Err(io::Error::from_raw_os_error(-code));
                        }
                        if seq == expected {
                            return Ok(());
                        }
                    }
                    if found_reply && kind == NEW {
                        return Ok(());
                    }
                }
                let advance = align(length);
                if advance > remaining.len() {
                    return Err(io::Error::other("invalid netlink padding"));
                }
                remaining = &remaining[advance..];
            }
        }
    }

    pub fn contains(&mut self, set: &SetName, ip: Ipv4Addr) -> io::Result<bool> {
        let seq = self.next();
        let request = message(GET, REQUEST, seq, set.family, 0, &body(set, &[ip], false)?);
        match self.exchange(&request, seq, &[seq], true) {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Low-level atomic batch. Normally use `add`, which avoids interval overlaps.
    pub fn add_batch(&mut self, set: &SetName, ips: &[Ipv4Addr]) -> io::Result<()> {
        if ips.is_empty() {
            return Ok(());
        }
        let begin = self.next();
        let seq = self.next();
        let end = self.next();
        let mut request = message(16, REQUEST, begin, 0, 10, &[]);
        request.extend(message(
            NEW,
            REQUEST | ACK | 0x400,
            seq,
            set.family,
            0,
            &body(set, ips, true)?,
        ));
        request.extend(message(17, REQUEST, end, 0, 10, &[]));
        self.exchange(&request, seq, &[begin, seq, end], false)
    }

    /// Test each address, add only missing addresses, retest once on EEXIST.
    pub fn add(&mut self, set: &SetName, ips: &[Ipv4Addr]) -> io::Result<()> {
        retry_once(|| {
            let mut missing = Vec::new();
            for ip in ips {
                if !missing.contains(ip) && !self.contains(set, *ip)? {
                    missing.push(*ip);
                }
            }
            self.add_batch(set, &missing)
        })
    }
}

fn retry_once(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    match operation() {
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => operation(),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_retry_limit() {
        for family in ["ip", "ip6", "inet"] {
            assert!(format!("{family}@t@s").parse::<SetName>().is_ok());
        }
        for bad in ["inet@t", "inet@@s", "foo@t@s", "inet@t@s@x", "inet@t@s\0"] {
            assert!(bad.parse::<SetName>().is_err());
        }
        let mut calls = 0;
        let error = retry_once(|| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EEXIST))
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        calls = 0;
        retry_once(|| {
            calls += 1;
            if calls == 1 {
                Err(io::Error::from_raw_os_error(libc::EEXIST))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
    }
}
