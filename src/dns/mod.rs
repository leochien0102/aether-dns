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

/// Full 12-bit RCODE, combining the header's low 4 bits with any EDNS0 OPT
/// extended-RCODE byte in the additional section (RFC 6891 section 6.1.3).
/// Legal truncated replies (`TC=1`) keep any extended RCODE already read from
/// a complete OPT record, and only fall back to the header's low 4 bits when
/// truncation occurs before a complete OPT record is reached.
pub fn rcode(wire: &[u8]) -> Result<u16, WireError> {
    if wire.len() < 12 {
        return Err(WireError);
    }
    let low = u16::from(wire[3] & 0x0f);
    let additional = word(wire, 10)?;
    if additional == 0 {
        return Ok(low);
    }
    let truncated = wire[2] & 0x02 != 0;
    let mut at = match cut_additional(wire) {
        Ok(at) => at,
        Err(_) if truncated => return Ok(low),
        Err(err) => return Err(err),
    };
    let mut extended = 0u8;
    for _ in 0..additional {
        let fields = match skip_name(wire, at) {
            Ok(fields) => fields,
            Err(_) if truncated => break,
            Err(err) => return Err(err),
        };
        let end = match skip_rr(wire, at) {
            Ok(end) => end,
            Err(_) if truncated => break,
            Err(err) => return Err(err),
        };
        if word(wire, fields)? == 41 {
            let high = *wire.get(fields + 4).ok_or(WireError)?;
            if extended == 0 {
                extended = high;
            }
        }
        at = end;
    }
    Ok((u16::from(extended) << 4) | low)
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
