//! DNS inspection and in-place wire operations. Forwarding never serializes a message.
#![allow(dead_code)] // Used by the forwarding card.

use hickory_proto::op::{Edns, Message, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use std::net::IpAddr;

#[derive(Debug)]
pub struct Inspection {
    pub id: u16,
    pub question: Option<(Name, RecordType, DNSClass)>,
    pub rcode: ResponseCode,
    pub truncated: bool,
    pub answers: Vec<(IpAddr, u32)>,
    pub edns: Option<Edns>,
}

/// Semantic inspection is independent of the deliberately loose forwarding check.
pub fn inspect(wire: &[u8]) -> Result<Inspection, hickory_proto::ProtoError> {
    let message = Message::from_vec(wire)?;
    let question = message
        .queries()
        .first()
        .map(|q| (q.name().clone(), q.query_type(), q.query_class()));
    let answers = message
        .answers()
        .iter()
        .filter_map(|rr| {
            let ip = match rr.data() {
                RData::A(a) => IpAddr::V4(a.0),
                RData::AAAA(a) => IpAddr::V6(a.0),
                _ => return None,
            };
            Some((ip, rr.ttl()))
        })
        .collect();
    Ok(Inspection {
        id: message.id(),
        question,
        rcode: message.response_code(),
        truncated: message.truncated(),
        answers,
        edns: message.extensions().clone(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireError;

fn word(wire: &[u8], at: usize) -> Result<u16, WireError> {
    let bytes = wire.get(at..at + 2).ok_or(WireError)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Skip the encoded name without interpreting compression pointers.
pub fn skip_name(wire: &[u8], mut at: usize) -> Result<usize, WireError> {
    loop {
        let n = *wire.get(at).ok_or(WireError)?;
        at += 1;
        match n {
            0 => return Ok(at),
            0xc0..=0xff => {
                return if at < wire.len() {
                    Ok(at + 1)
                } else {
                    Err(WireError)
                };
            }
            1..=63 => {
                at += usize::from(n);
                if at > wire.len() {
                    return Err(WireError);
                }
            }
            _ => return Err(WireError),
        }
    }
}

pub fn skip_rr(wire: &[u8], at: usize) -> Result<usize, WireError> {
    let at = skip_name(wire, at)?;
    let len = usize::from(word(wire, at + 8)?);
    let end = at + 10 + len;
    if end > wire.len() {
        Err(WireError)
    } else {
        Ok(end)
    }
}

fn question_end(wire: &[u8], at: usize) -> Result<usize, WireError> {
    let end = skip_name(wire, at)? + 4;
    if end > wire.len() {
        Err(WireError)
    } else {
        Ok(end)
    }
}

/// The forwarding gate intentionally does not depend on hickory's full parser.
pub fn check_query(wire: &[u8]) -> Result<(), WireError> {
    if wire.len() < 12 || wire[2] & 0x80 != 0 || word(wire, 4)? != 1 {
        return Err(WireError);
    }
    let end = question_end(wire, 12)?;
    let name_len = end - 12 - 4;
    if !(1..=255).contains(&name_len) {
        return Err(WireError);
    }
    Ok(())
}

pub fn cut_additional(wire: &[u8]) -> Result<usize, WireError> {
    if wire.len() < 12 {
        return Err(WireError);
    }
    let mut at = 12;
    for _ in 0..word(wire, 4)? {
        at = question_end(wire, at)?;
    }
    let count = u32::from(word(wire, 6)?) + u32::from(word(wire, 8)?);
    for _ in 0..count {
        at = skip_rr(wire, at)?;
    }
    Ok(at)
}

pub fn truncate_additional(wire: &mut Vec<u8>) -> Result<(), WireError> {
    let at = cut_additional(wire)?;
    wire[10..12].fill(0);
    wire.truncate(at);
    Ok(())
}

pub fn set_id(wire: &mut [u8], id: u16) -> Result<(), WireError> {
    wire.get_mut(..2)
        .ok_or(WireError)?
        .copy_from_slice(&id.to_be_bytes());
    Ok(())
}
pub fn set_ra(wire: &mut [u8]) -> Result<(), WireError> {
    *wire.get_mut(3).ok_or(WireError)? |= 0x80;
    Ok(())
}
pub fn clear_tc(wire: &mut [u8]) -> Result<(), WireError> {
    *wire.get_mut(2).ok_or(WireError)? &= !0x02;
    Ok(())
}

/// Construct an empty response by retaining the original header and question.
pub fn synthetic_nodata(wire: &mut Vec<u8>) -> Result<(), WireError> {
    check_query(wire)?;
    let end = question_end(wire, 12)?;
    wire[2] |= 0x80;
    wire[3] = (wire[3] & 0xf0) | 0x80;
    wire[6..12].fill(0);
    wire.truncate(end);
    Ok(())
}
