//! DNS inspection and in-place wire operations. Forwarding never serializes a message.
#![allow(dead_code)] // Used by the forwarding card.

use hickory_proto::op::{Edns, Message, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType, rdata::OPT};
use std::net::IpAddr;

pub const MIN_UDP_PAYLOAD_SIZE: usize = 512;
const MINIMAL_OPT_LEN: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpBudget {
    pub max_size: usize,
    pub client_edns: bool,
    pub client_do: bool,
}

fn minimal_opt_bytes(ext_rcode: u8, do_bit: bool) -> [u8; MINIMAL_OPT_LEN] {
    [
        0,
        0,
        41,
        2,
        0,
        ext_rcode,
        0,
        if do_bit { 0x80 } else { 0 },
        0,
        0,
        0,
    ]
}

fn minimal_edns(do_bit: bool) -> Edns {
    let mut edns = Edns::new();
    edns.set_dnssec_ok(do_bit);
    edns
}

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

struct NameCursor<'a> {
    wire: &'a [u8],
    pos: usize,
    total_len: usize,
    max_ptr: usize,
    end_pos: Option<usize>,
}

impl<'a> NameCursor<'a> {
    fn new(wire: &'a [u8], start: usize) -> Self {
        Self {
            wire,
            pos: start,
            total_len: 0,
            max_ptr: usize::MAX,
            end_pos: None,
        }
    }

    fn next_label(&mut self) -> Result<Option<&'a [u8]>, WireError> {
        loop {
            let len = *self.wire.get(self.pos).ok_or(WireError)?;
            match len {
                0 => {
                    self.total_len += 1;
                    if self.total_len > 255 {
                        return Err(WireError);
                    }
                    if self.end_pos.is_none() {
                        self.end_pos = Some(self.pos + 1);
                    }
                    return Ok(None);
                }
                1..=63 => {
                    let n = usize::from(len);
                    let start = self.pos + 1;
                    let end = start.checked_add(n).ok_or(WireError)?;
                    let label = self.wire.get(start..end).ok_or(WireError)?;
                    self.total_len += 1 + n;
                    if self.total_len >= 255 {
                        return Err(WireError);
                    }
                    self.pos = end;
                    return Ok(Some(label));
                }
                0xc0..=0xff => {
                    let lo = *self.wire.get(self.pos + 1).ok_or(WireError)?;
                    let ptr = (usize::from(len & 0x3f) << 8) | usize::from(lo);
                    if ptr < 12 || ptr >= self.pos || ptr >= self.max_ptr {
                        return Err(WireError);
                    }
                    if self.end_pos.is_none() {
                        self.end_pos = Some(self.pos + 2);
                    }
                    self.max_ptr = ptr;
                    self.pos = ptr;
                }
                _ => return Err(WireError),
            }
        }
    }

    fn qtype_qclass(&self) -> Result<(u16, u16), WireError> {
        let end = self.end_pos.ok_or(WireError)?;
        Ok((word(self.wire, end)?, word(self.wire, end + 2)?))
    }
}

/// Compare the first Question (QNAME by DNS labels with ASCII-only case
/// folding, plus exact QTYPE and QCLASS) of two DNS wire messages starting at
/// byte 12. Stops at the end of the Question section so legal truncated
/// (`TC=1`) replies with an intact matching question are not rejected due to
/// truncated trailing sections.
pub fn same_question(response: &[u8], query: &[u8]) -> bool {
    if response.len() < 12 || query.len() < 12 {
        return false;
    }
    let mut r = NameCursor::new(response, 12);
    let mut q = NameCursor::new(query, 12);
    loop {
        match (r.next_label(), q.next_label()) {
            (Ok(Some(rl)), Ok(Some(ql))) => {
                if !rl.eq_ignore_ascii_case(ql) {
                    return false;
                }
            }
            (Ok(None), Ok(None)) => break,
            _ => return false,
        }
    }
    match (r.qtype_qclass(), q.qtype_qclass()) {
        (Ok(rt), Ok(qt)) => rt == qt,
        _ => false,
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

/// Effective UDP DNS payload budget advertised by this client query.
/// Distinguishes "no EDNS (512 bytes)" from "EDNS present (advertised size,
/// clamped to at least 512 bytes per RFC 6891 section 6.2.3)" and records
/// whether the client set the DNSSEC OK (DO) bit.
pub fn udp_budget(query: &[u8]) -> UdpBudget {
    match Message::from_vec(query) {
        Ok(message) => match message.extensions().as_ref() {
            Some(edns) => UdpBudget {
                max_size: usize::from(message.max_payload()),
                client_edns: true,
                client_do: edns.flags().dnssec_ok,
            },
            None => UdpBudget {
                max_size: MIN_UDP_PAYLOAD_SIZE,
                client_edns: false,
                client_do: false,
            },
        },
        Err(_) => UdpBudget {
            max_size: MIN_UDP_PAYLOAD_SIZE,
            client_edns: false,
            client_do: false,
        },
    }
}

/// Ensure a DNS wire response fits within the client's UDP payload budget
/// without slicing raw bytes or corrupting records.
///
/// - For non-EDNS clients, normal responses omit OPT (while extended RCODEs
///   that cannot fit in 4 header bits never silently downgrade to NOERROR).
/// - For EDNS clients, final wire size is evaluated including a minimal
///   response OPT record whose DO bit follows the client query (without
///   copying client options), and OPT is never dropped merely because extended
///   RCODE is 0.
/// - If omitting optional non-OPT additional records or optional EDNS OPT
///   options (such as padding) is sufficient to fit, `TC` is not forced to 1.
/// - When required sections (plus minimal OPT if required) still exceed
///   `max_size`, returns a valid, parseable `TC=1` response preserving client
///   ID, Question, flags, minimal OPT (when applicable), and full RCODE.
pub fn enforce_udp_limit(response: &mut Vec<u8>, budget: UdpBudget) {
    let limit = budget.max_size.max(MIN_UDP_PAYLOAD_SIZE);

    if !budget.client_edns
        && response.len() <= limit
        && (response.len() < 12 || word(response, 10) == Ok(0))
    {
        return;
    }

    if budget.client_edns
        && response.len() >= 12
        && word(response, 10) == Ok(0)
        && response.len() + MINIMAL_OPT_LEN <= limit
    {
        response[10..12].copy_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&minimal_opt_bytes(0, budget.client_do));
        return;
    }

    if let Ok(mut message) = Message::from_vec(response) {
        let has_extended_rcode = message
            .extensions()
            .as_ref()
            .is_some_and(|edns| edns.rcode_high() != 0);

        if !budget.client_edns && has_extended_rcode {
            message.set_response_code(ResponseCode::ServFail);
            message.set_truncated(false);
            message.take_answers();
            message.take_name_servers();
            message.take_additionals();
            *message.extensions_mut() = None;
            if try_replace_if_fits(response, &message, limit) {
                return;
            }
        }

        if !budget.client_edns && message.extensions().is_some() {
            *message.extensions_mut() = None;
            if try_replace_if_fits(response, &message, limit) {
                return;
            }
        } else if !budget.client_edns && response.len() <= limit {
            return;
        }

        if budget.client_edns {
            match message.extensions_mut() {
                Some(edns) => {
                    if edns.flags().dnssec_ok == budget.client_do && response.len() <= limit {
                        return;
                    }
                    edns.set_dnssec_ok(budget.client_do);
                    if try_replace_if_fits(response, &message, limit) {
                        return;
                    }
                }
                None => {
                    message.set_edns(minimal_edns(budget.client_do));
                    if try_replace_if_fits(response, &message, limit) {
                        return;
                    }
                }
            }
        }

        if !message.additionals().is_empty() {
            message.take_additionals();
            if try_replace_if_fits(response, &message, limit) {
                return;
            }
        }

        if let Some(edns) = message.extensions_mut()
            && !edns.options().as_ref().is_empty()
        {
            *edns.options_mut() = OPT::default();
            if try_replace_if_fits(response, &message, limit) {
                return;
            }
        }

        message.set_truncated(true);
        message.take_answers();
        message.take_name_servers();
        message.take_additionals();
        if budget.client_edns {
            let edns = message
                .extensions_mut()
                .get_or_insert_with(|| minimal_edns(budget.client_do));
            edns.set_dnssec_ok(budget.client_do);
            *edns.options_mut() = OPT::default();
        } else {
            *message.extensions_mut() = None;
        }
        if try_replace_if_fits(response, &message, limit) {
            return;
        }
    }

    fallback_truncate_wire(response, limit, budget.client_edns, budget.client_do);
}

fn try_replace_if_fits(response: &mut Vec<u8>, message: &Message, limit: usize) -> bool {
    if let Ok(encoded) = message.to_vec()
        && encoded.len() <= limit
    {
        *response = encoded;
        return true;
    }
    false
}

fn fallback_truncate_wire(response: &mut Vec<u8>, limit: usize, keep_opt: bool, do_bit: bool) {
    if response.len() < 12 {
        return;
    }
    let full_rcode = rcode(response).unwrap_or(u16::from(response[3] & 0x0f));
    let ext_rcode = (full_rcode >> 4) as u8;
    let qdcount = word(response, 4).unwrap_or(0);
    let mut end = 12;
    let mut kept_questions = 0u16;
    for _ in 0..qdcount {
        match question_end(response, end) {
            Ok(next) if next <= limit => {
                end = next;
                kept_questions += 1;
            }
            _ => break,
        }
    }
    if !keep_opt && ext_rcode != 0 {
        response[2] &= !0x02;
        response[3] = (response[3] & 0xf0) | 2;
        response[4..6].copy_from_slice(&kept_questions.to_be_bytes());
        response[6..12].fill(0);
        response.truncate(end);
        return;
    }
    response[2] |= 0x02;
    response[4..6].copy_from_slice(&kept_questions.to_be_bytes());
    response[6..12].fill(0);
    response.truncate(end);
    if keep_opt && response.len() + MINIMAL_OPT_LEN <= limit {
        response[10..12].copy_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&minimal_opt_bytes(ext_rcode, do_bit));
    }
}
