//! Minimal direct nfnetlink/nftables netlink client for the M0 spike.
//!
//! Deliberately dependency-light (libc only) and message-oriented: the point is to
//! prove that a Rust daemon can query and update nft sets via netlink without
//! shelling out to `nft`. This is spike code, not production code.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub const NFNL_SUBSYS_NFTABLES: u16 = 10;
pub const NFT_MSG_NEWSETELEM: u16 = 12;
pub const NFT_MSG_GETSETELEM: u16 = 13;
pub const NFNL_MSG_BATCH_BEGIN: u16 = 16;
pub const NFNL_MSG_BATCH_END: u16 = 17;

pub const NLM_F_REQUEST: u16 = 0x01;
pub const NLM_F_ACK: u16 = 0x04;
pub const NLM_F_EXCL: u16 = 0x200;
pub const NLM_F_CREATE: u16 = 0x400;
pub const NLM_F_DUMP: u16 = 0x300;
pub const NLM_F_MULTI: u16 = 0x02;

pub const NLMSG_ERROR: u16 = 2;
pub const NLMSG_DONE: u16 = 3;

pub const NFPROTO_INET: u8 = 1;
pub const NFPROTO_IPV4: u8 = 2;
pub const NFPROTO_IPV6: u8 = 10;

pub const NFTA_SET_ELEM_LIST_TABLE: u16 = 1;
pub const NFTA_SET_ELEM_LIST_SET: u16 = 2;
pub const NFTA_SET_ELEM_LIST_ELEMENTS: u16 = 3;
pub const NFTA_LIST_ELEM: u16 = 1;
pub const NFTA_SET_ELEM_KEY: u16 = 1;
pub const NFTA_SET_ELEM_FLAGS: u16 = 3;
pub const NFTA_DATA_VALUE: u16 = 1;

pub const NLA_F_NESTED: u16 = 0x8000;
pub const NLA_F_NET_BYTEORDER: u16 = 0x4000;

pub const NFT_SET_ELEM_INTERVAL_END: u32 = 1;

const AF_NETLINK: i32 = 16;
const NETLINK_NETFILTER: i32 = 12;
const SOL_NETLINK: i32 = 270;
const NETLINK_EXT_ACK: i32 = 11;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// A single netlink message under construction.
pub struct Msg {
    buf: Vec<u8>,
    seq: u32,
}

impl Msg {
    pub fn new(ty: u16, flags: u16, family: u8, res_id: u16, seq: u32) -> Self {
        let mut buf = vec![0u8; 20];
        // nlmsghdr
        buf[0..4].copy_from_slice(&0u32.to_ne_bytes()); // len patched in finish()
        buf[4..6].copy_from_slice(&ty.to_ne_bytes());
        buf[6..8].copy_from_slice(&flags.to_ne_bytes());
        buf[8..12].copy_from_slice(&seq.to_ne_bytes());
        buf[12..16].copy_from_slice(&0u32.to_ne_bytes()); // pid: 0 = kernel fills
        // nfgenmsg
        buf[16] = family;
        buf[17] = 0; // NFNETLINK_V0
        buf[18..20].copy_from_slice(&res_id.to_be_bytes());
        Self { buf, seq }
    }

    pub fn seq(&self) -> u32 {
        self.seq
    }

    pub fn attr(&mut self, ty: u16, data: &[u8]) {
        let len = 4 + data.len();
        self.buf.extend_from_slice(&(len as u16).to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(data);
        let pad = align4(len) - len;
        self.buf.extend(std::iter::repeat_n(0u8, pad));
    }

    pub fn attr_str(&mut self, ty: u16, s: &str) {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        self.attr(ty, &v);
    }

    /// Start a nested attribute; returns the byte offset for `nest_end`.
    pub fn nest(&mut self, ty: u16) -> usize {
        let off = self.buf.len();
        self.buf.extend_from_slice(&0u16.to_ne_bytes()); // patched
        self.buf.extend_from_slice(&(ty | NLA_F_NESTED).to_ne_bytes());
        off
    }

    pub fn nest_end(&mut self, off: usize) {
        let len = self.buf.len() - off;
        self.buf[off..off + 2].copy_from_slice(&(len as u16).to_ne_bytes());
        let pad = align4(len) - len;
        self.buf.extend(std::iter::repeat_n(0u8, pad));
    }

    pub fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len() as u32;
        self.buf[0..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ParsedMsg<'a> {
    pub ty: u16,
    pub flags: u16,
    pub seq: u32,
    pub nlmsg_len: u32,
    pub payload: &'a [u8],
}

impl<'a> ParsedMsg<'a> {
    pub fn nfgen_family(&self) -> Option<u8> {
        self.payload.first().copied()
    }
    /// Netlink error code (negative errno) for NLMSG_ERROR messages.
    pub fn error_code(&self) -> Option<i32> {
        if self.ty != NLMSG_ERROR || self.payload.len() < 4 {
            return None;
        }
        Some(i32::from_ne_bytes(self.payload[0..4].try_into().unwrap()))
    }
}

/// Iterate the nlmsghdrs contained in one recv buffer.
pub fn parse_msgs(buf: &[u8]) -> Vec<ParsedMsg<'_>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 16 <= buf.len() {
        let len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if len < 16 || off + len > buf.len() {
            break;
        }
        let ty = u16::from_ne_bytes(buf[off + 4..off + 6].try_into().unwrap());
        let flags = u16::from_ne_bytes(buf[off + 6..off + 8].try_into().unwrap());
        let seq = u32::from_ne_bytes(buf[off + 8..off + 12].try_into().unwrap());
        // nfgenmsg header (4 bytes) is part of the payload for nfnetlink messages.
        let payload = &buf[off + 16..off + len];
        out.push(ParsedMsg {
            ty,
            flags,
            seq,
            nlmsg_len: len as u32,
            payload,
        });
        off += align4(len);
    }
    out
}

/// Generic attribute iterator over a raw attribute blob.
pub struct AttrIter<'a> {
    buf: &'a [u8],
    off: usize,
}

impl<'a> AttrIter<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, off: 0 }
    }
}

impl<'a> Iterator for AttrIter<'a> {
    type Item = (u16, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.off + 4 > self.buf.len() {
            return None;
        }
        let len = u16::from_ne_bytes(self.buf[self.off..self.off + 2].try_into().unwrap()) as usize;
        if len < 4 || self.off + len > self.buf.len() {
            return None;
        }
        let ty = u16::from_ne_bytes(self.buf[self.off + 2..self.off + 4].try_into().unwrap());
        let data = &self.buf[self.off + 4..self.off + len];
        self.off += align4(len);
        Some((ty & !(NLA_F_NESTED | NLA_F_NET_BYTEORDER), data))
    }
}

pub struct Socket {
    fd: OwnedFd,
    seq: u32,
}

impl Socket {
    pub fn open() -> io::Result<Self> {
        unsafe {
            let raw = libc::socket(AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, NETLINK_NETFILTER);
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(raw);

            let one: libc::c_int = 1;
            // Extended ACKs are opt-in; ignore failure (older kernels).
            let _ = libc::setsockopt(
                fd.as_raw_fd(),
                SOL_NETLINK,
                NETLINK_EXT_ACK,
                &one as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            let mut addr: libc::sockaddr_nl = mem::zeroed();
            addr.nl_family = AF_NETLINK as u16;
            addr.nl_pid = 0;
            if libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }

            let tv = libc::timeval { tv_sec: 0, tv_usec: 500_000 };
            let _ = libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const _ as *const libc::c_void,
                mem::size_of::<libc::timeval>() as libc::socklen_t,
            );

            Ok(Self { fd, seq: 1 })
        }
    }

    pub fn next_seq(&mut self) -> u32 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<()> {
        let n = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                buf.as_ptr() as *const libc::c_void,
                buf.len(),
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != buf.len() {
            return Err(io::Error::new(io::ErrorKind::Other, "short netlink send"));
        }
        Ok(())
    }

    /// Blocking receive of one datagram (up to 64 KiB). Returns None on timeout.
    pub fn recv(&self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; 65536];
        let n = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::EAGAIN) {
                return Ok(None);
            }
            return Err(e);
        }
        buf.truncate(n as usize);
        Ok(Some(buf))
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// Build the `elements { elem }` body used by both GETSETELEM and NEWSETELEM.
pub fn add_elem(m: &mut Msg, ip: &[u8], interval_end: bool) {
    let elem = m.nest(NFTA_LIST_ELEM);
    if interval_end {
        m.attr(NFTA_SET_ELEM_FLAGS, &NFT_SET_ELEM_INTERVAL_END.to_be_bytes());
    }
    let key = m.nest(NFTA_SET_ELEM_KEY);
    m.attr(NFTA_DATA_VALUE | NLA_F_NET_BYTEORDER, ip);
    m.nest_end(key);
    m.nest_end(elem);
}

/// Increment a big-endian address as an integer (used for the exclusive interval end).
pub fn ip_plus_one(ip: &[u8]) -> Vec<u8> {
    let mut v = ip.to_vec();
    for i in (0..v.len()).rev() {
        let (nv, carry) = v[i].overflowing_add(1);
        v[i] = nv;
        if !carry {
            break;
        }
    }
    v
}
