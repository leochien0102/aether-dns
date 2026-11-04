//! M0 parser spike: can hickory-proto 0.25 parse the packets aether-proxy's
//! chinadns-ng profile actually sees, and what does re-encoding cost?
//!
//! This is test scaffolding, not production code. Packets are built byte-for-byte
//! so we can exercise wire forms hickory itself would not produce (compression,
//! EDNS OPT in the additional section, unknown RR types, trailing garbage).

use hickory_proto::op::{Message, MessageType, ResponseCode};
use hickory_proto::rr::{RData, RecordType};

// ---------------------------------------------------------------------------
// minimal wire builder (fixture construction only)
// ---------------------------------------------------------------------------

fn name_wire(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

fn header(id: u16, flags: u16, qd: u16, an: u16, ns: u16, ar: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(&flags.to_be_bytes());
    v.extend_from_slice(&qd.to_be_bytes());
    v.extend_from_slice(&an.to_be_bytes());
    v.extend_from_slice(&ns.to_be_bytes());
    v.extend_from_slice(&ar.to_be_bytes());
    v
}

fn question(name: &str, qtype: u16) -> Vec<u8> {
    let mut v = name_wire(name);
    v.extend_from_slice(&qtype.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes()); // IN
    v
}

fn rr(owner: &[u8], rtype: u16, ttl: u32, rdata: &[u8]) -> Vec<u8> {
    let mut v = owner.to_vec();
    v.extend_from_slice(&rtype.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&ttl.to_be_bytes());
    v.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    v.extend_from_slice(rdata);
    v
}

fn opt_rr(udp_size: u16) -> Vec<u8> {
    // root owner, type 41, class = UDP size, ttl = ext-rcode/version/flags, no options
    let mut v = vec![0u8];
    v.extend_from_slice(&41u16.to_be_bytes());
    v.extend_from_slice(&udp_size.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes());
    v
}

fn query(id: u16, name: &str, qtype: u16, edns: bool) -> Vec<u8> {
    let mut v = header(id, 0x0100, 1, 0, 0, edns as u16);
    v.extend_from_slice(&question(name, qtype));
    if edns {
        v.extend_from_slice(&opt_rr(1232));
    }
    v
}

fn response(id: u16, qname: &str, qtype: u16, answers: &[Vec<u8>], authority: &[Vec<u8>], additional: &[Vec<u8>], rcode: u16, tc: bool) -> Vec<u8> {
    let mut flags = 0x8000 | 0x0080 | rcode; // QR + RA
    if tc {
        flags |= 0x0200;
    }
    let mut v = header(id, flags, 1, answers.len() as u16, authority.len() as u16, additional.len() as u16);
    v.extend_from_slice(&question(qname, qtype));
    for a in answers {
        v.extend_from_slice(a);
    }
    for a in authority {
        v.extend_from_slice(a);
    }
    for a in additional {
        v.extend_from_slice(a);
    }
    v
}

fn a_record(ip: [u8; 4], ttl: u32) -> Vec<u8> {
    rr(&name_wire("www.example.com"), 1, ttl, &ip)
}

fn cname_record(target: &str, ttl: u32) -> Vec<u8> {
    rr(&name_wire("www.example.com"), 5, ttl, &name_wire(target))
}

// ---------------------------------------------------------------------------
// corpus
// ---------------------------------------------------------------------------

struct Case {
    name: &'static str,
    wire: Vec<u8>,
}

fn corpus() -> Vec<Case> {
    let mut cases = Vec::new();

    cases.push(Case { name: "query-A", wire: query(0x1234, "www.example.com", 1, false) });
    cases.push(Case { name: "query-AAAA", wire: query(0x1235, "www.example.com", 28, false) });
    cases.push(Case { name: "query-HTTPS-65", wire: query(0x1236, "www.example.com", 65, false) });
    cases.push(Case { name: "query-EDNS", wire: query(0x1237, "www.example.com", 1, true) });
    cases.push(Case { name: "query-mixedcase", wire: query(0x1238, "WwW.ExAmPlE.CoM", 1, false) });

    // Two questions: chinadns-ng rejects any qdcount != 1.
    let mut twoq = header(0x1239, 0x0100, 2, 0, 0, 0);
    twoq.extend_from_slice(&question("a.example.com", 1));
    twoq.extend_from_slice(&question("b.example.com", 1));
    cases.push(Case { name: "query-2questions", wire: twoq });

    // Trailing bytes after the question.
    let mut garbage = query(0x123a, "www.example.com", 1, false);
    garbage.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    cases.push(Case { name: "query-trailing-garbage", wire: garbage });

    // Truncated header.
    cases.push(Case { name: "query-11-bytes", wire: vec![0u8; 11] });

    // qdcount=1 but no question section.
    cases.push(Case { name: "query-missing-question", wire: header(0x123b, 0x0100, 1, 0, 0, 0) });

    // Label length 64 (invalid: max 63).
    let mut badlabel = header(0x123c, 0x0100, 1, 0, 0, 0);
    badlabel.push(64);
    badlabel.extend_from_slice(&[b'x'; 64]);
    badlabel.push(0);
    badlabel.extend_from_slice(&1u16.to_be_bytes());
    badlabel.extend_from_slice(&1u16.to_be_bytes());
    cases.push(Case { name: "query-label-64", wire: badlabel });

    // Normal response: 1 A + EDNS OPT in additional.
    cases.push(Case {
        name: "resp-A+OPT",
        wire: response(0x2234, "www.example.com", 1, &[a_record([93, 184, 216, 34], 60)], &[], &[opt_rr(1232)], 0, false),
    });

    // Normal response without EDNS.
    cases.push(Case {
        name: "resp-A",
        wire: response(0x2235, "www.example.com", 1, &[a_record([1, 2, 3, 4], 300)], &[], &[], 0, false),
    });

    // CNAME chain + A.
    cases.push(Case {
        name: "resp-cname+A",
        wire: response(
            0x2236,
            "www.example.com",
            1,
            &[cname_record("edge.example.net", 120), a_record([203, 0, 113, 9], 60)],
            &[],
            &[],
            0,
            false,
        ),
    });

    // NXDOMAIN with SOA in authority.
    let mut soa_rdata = name_wire("ns.example.com");
    soa_rdata.extend_from_slice(&name_wire("hostmaster.example.com"));
    soa_rdata.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5]);
    let soa = rr(&name_wire("example.com"), 6, 3600, &soa_rdata);
    cases.push(Case {
        name: "resp-nxdomain+SOA",
        wire: response(0x2237, "nope.example.com", 1, &[], &[soa.clone()], &[opt_rr(1232)], 3, false),
    });

    // NODATA: NOERROR, no answers, SOA in authority.
    cases.push(Case {
        name: "resp-nodata+SOA",
        wire: response(0x2238, "nodata.example.com", 1, &[], &[soa], &[], 0, false),
    });

    // Truncated UDP response (TC bit).
    cases.push(Case {
        name: "resp-TC",
        wire: response(0x2239, "www.example.com", 1, &[], &[], &[], 0, true),
    });

    // Unknown RR type 65280 (private use), RFC 3597.
    let unknown = rr(&name_wire("www.example.com"), 65280, 60, &[0x01, 0x02, 0x03, 0x04, 0x05]);
    cases.push(Case {
        name: "resp-unknown-rr-65280",
        wire: response(0x223a, "www.example.com", 1, &[unknown], &[], &[], 0, false),
    });

    // HTTPS (type 65) answer with a minimal SvcPriority/TargetName.
    let mut https_rdata = vec![0x00, 0x01]; // priority 1
    https_rdata.extend_from_slice(&name_wire("."));
    let https = rr(&name_wire("www.example.com"), 65, 60, &https_rdata);
    cases.push(Case {
        name: "resp-HTTPS-65",
        wire: response(0x223b, "www.example.com", 65, &[https], &[], &[], 0, false),
    });

    // Compressed answer name: pointer to offset 12 (question name).
    let mut compressed = vec![0xc0, 0x0c];
    compressed.extend_from_slice(&1u16.to_be_bytes());
    compressed.extend_from_slice(&1u16.to_be_bytes());
    compressed.extend_from_slice(&60u32.to_be_bytes());
    compressed.extend_from_slice(&4u16.to_be_bytes());
    compressed.extend_from_slice(&[198, 51, 100, 7]);
    cases.push(Case {
        name: "resp-compressed-name",
        wire: response(0x223c, "www.example.com", 1, &[compressed], &[], &[], 0, false),
    });

    // Response with an OPT whose extended rcode is non-zero (BADVERS-ish).
    cases.push(Case {
        name: "resp-OPT-extrcode",
        wire: response(0x223d, "www.example.com", 1, &[a_record([192, 0, 2, 1], 30)], &[], &[opt_rr(4096)], 0, false),
    });

    cases
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

fn qtype_name(t: RecordType) -> String {
    match t {
        RecordType::A => "A".into(),
        RecordType::AAAA => "AAAA".into(),
        RecordType::HTTPS => "HTTPS".into(),
        other => format!("{other:?}"),
    }
}

fn hex_prefix(b: &[u8], n: usize) -> String {
    b.iter().take(n).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

fn skip_name(buf: &[u8], off: &mut usize) -> Result<(), String> {
    loop {
        if *off >= buf.len() {
            return Err("truncated name".into());
        }
        let n = buf[*off];
        if n == 0 {
            *off += 1;
            return Ok(());
        }
        if n & 0xC0 == 0xC0 {
            if *off + 2 > buf.len() {
                return Err("truncated pointer".into());
            }
            *off += 2;
            return Ok(());
        }
        if n > 63 {
            return Err(format!("bad label length {n}"));
        }
        *off += 1 + n as usize;
        if *off > buf.len() {
            return Err("label overflow".into());
        }
    }
}

fn skip_rr(buf: &[u8], off: &mut usize) -> Result<(), String> {
    skip_name(buf, off)?;
    if *off + 10 > buf.len() {
        return Err("truncated RR header".into());
    }
    let rdlen = u16::from_be_bytes([buf[*off + 8], buf[*off + 9]]) as usize;
    *off += 10 + rdlen;
    if *off > buf.len() {
        return Err("truncated RDATA".into());
    }
    Ok(())
}

/// The thin adapter chinadns-ng needs: byte offset where the additional section
/// starts, i.e. end of the header+question+answer+authority. No re-encoding.
fn cut_additional(buf: &[u8]) -> Result<usize, String> {
    if buf.len() < 12 {
        return Err("short header".into());
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let ns = u16::from_be_bytes([buf[8], buf[9]]) as usize;
    let mut off = 12usize;
    for _ in 0..qd {
        skip_name(buf, &mut off)?;
        off += 4;
        if off > buf.len() {
            return Err("truncated question".into());
        }
    }
    for _ in 0..(an + ns) {
        skip_rr(buf, &mut off)?;
    }
    Ok(off)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    for i in (0..b.len()).step_by(2) {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[derive(serde::Deserialize)]
struct Sample {
    label: String,
    direction: String,
    hex: String,
}

/// Parse the exact bytes captured from chinadns-ng (upstream queries + client
/// replies) with hickory, and measure how a re-encode would differ.
fn run_wire(path: &str) {
    let data = std::fs::read_to_string(path).expect("read wire samples");
    let samples: Vec<Sample> = serde_json::from_str(&data).expect("parse wire samples");
    let mut ok = 0;
    let mut fail = 0;
    let mut diff = 0;
    let mut edns_kept = 0;
    for s in &samples {
        let Some(wire) = hex_decode(&s.hex) else {
            println!("{:<50} BAD HEX", s.label);
            fail += 1;
            continue;
        };
        match Message::from_vec(&wire) {
            Ok(msg) => {
                ok += 1;
                let q = msg.queries().first();
                let qname = q.map(|q| q.name().to_string()).unwrap_or_else(|| "-".into());
                let qtype = q.map(|q| qtype_name(q.query_type())).unwrap_or_else(|| "-".into());
                let re = msg.to_vec().unwrap_or_default();
                let same = re == wire;
                if !same {
                    diff += 1;
                }
                let ar = msg.additionals().len() + usize::from(msg.extensions().is_some());
                if msg.extensions().is_some() {
                    edns_kept += 1;
                }
                let cut = cut_additional(&wire);
                let cut_desc = match cut {
                    Ok(c) if c == wire.len() => "cut=end".to_string(),
                    Ok(c) => format!("cut=end-{}", wire.len() - c),
                    Err(e) => format!("cut=ERR({e})"),
                };
                println!(
                    "{:<50} {:<14} ok id=0x{:04x} {:<18} {:<5} rcode={:?} an={} ar={} re={} {}",
                    s.label,
                    s.direction,
                    msg.header().id(),
                    qname,
                    qtype,
                    msg.response_code(),
                    msg.answers().len(),
                    ar,
                    if same { "same".to_string() } else { format!("diff {}->{}", wire.len(), re.len()) },
                    cut_desc
                );
            }
            Err(e) => {
                fail += 1;
                println!("{:<50} {:<14} FAIL ({e})", s.label, s.direction);
            }
        }
    }
    println!();
    println!("wire samples: {} parsed, {} failed; re-encode differs on {}; EDNS present on {}", ok, fail, diff, edns_kept);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "wire" {
        run_wire(&args[2]);
        return;
    }
    let mut ok = 0;
    let mut parse_fail = 0;
    let mut reencode_equal = 0;
    let mut reencode_diff = 0;
    let mut unknown_preserved = 0;
    let mut unknown_total = 0;

    println!("{:<26} {:<5} {:<7} {:<8} {:<6} {:<5} {:<5} {:<5} {}", "case", "parse", "qname", "qtype", "rcode", "an", "ns", "ar", "re-encode");
    for case in corpus() {
        match Message::from_vec(&case.wire) {
            Ok(msg) => {
                ok += 1;
                let q = msg.queries().first();
                let qname = q.map(|q| q.name().to_string()).unwrap_or_else(|| "-".into());
                let qtype = q.map(|q| qtype_name(q.query_type())).unwrap_or_else(|| "-".into());
                let rcode = msg.response_code();
                let ar = msg.additionals().len() + usize::from(msg.extensions().is_some());
                // Re-encode and compare.
                let re = msg.to_vec().unwrap_or_default();
                let eq = re == case.wire;
                if eq {
                    reencode_equal += 1;
                } else {
                    reencode_diff += 1;
                }
                // Does the unknown type survive a re-encode (checked by re-parsing)?
                let re_parsed = Message::from_vec(&re).ok();
                for r in msg.answers() {
                    if let RData::Unknown { code, .. } = r.data() {
                        unknown_total += 1;
                        let survived = re_parsed.as_ref().is_some_and(|m| {
                            m.answers().iter().any(|rr| matches!(rr.data(), RData::Unknown { code: c, .. } if c == code))
                        });
                        if survived {
                            unknown_preserved += 1;
                        }
                    }
                }
                println!(
                    "{:<26} {:<5} {:<7} {:<8} {:<6} {:<5} {:<5} {:<5} {}",
                    case.name,
                    "ok",
                    qname,
                    qtype,
                    format!("{rcode:?}"),
                    msg.answers().len(),
                    msg.name_servers().len(),
                    ar,
                    if eq {
                        "identical".to_string()
                    } else {
                        format!("diff ({} vs {} bytes)", case.wire.len(), re.len())
                    }
                );
                if !eq {
                    println!("    wire: {}", hex_prefix(&case.wire, 32));
                    println!("    re  : {}", hex_prefix(&re, 32));
                }
            }
            Err(e) => {
                parse_fail += 1;
                println!("{:<26} {:<5} error: {e}", case.name, "FAIL");
            }
        }
    }

    println!();
    println!("parsed {ok}, failed {parse_fail}; re-encode identical {reencode_equal}, different {reencode_diff}");
    println!("unknown RRs seen {unknown_total}, preserved on re-encode {unknown_preserved}");

    // Thin-adapter check: a byte-offset record scanner must find the start of the
    // additional section without decoding/re-encoding the message.
    let mut scan_ok = 0;
    let mut scan_bad = Vec::new();
    for case in corpus() {
        match cut_additional(&case.wire) {
            Ok(c) if c <= case.wire.len() => {
                if case.name == "resp-A+OPT" && c != case.wire.len() - 11 {
                    scan_bad.push(format!("{}: cut {c}, expected {}", case.name, case.wire.len() - 11));
                } else {
                    scan_ok += 1;
                }
            }
            Ok(c) => scan_bad.push(format!("{}: cut {c} > len {}", case.name, case.wire.len())),
            Err(e) => {
                if matches!(case.name, "query-11-bytes" | "query-missing-question" | "query-label-64") {
                    scan_ok += 1;
                } else {
                    scan_bad.push(format!("{}: {e}", case.name));
                }
            }
        }
    }
    println!("wire scanner: {scan_ok} ok, {} bad", scan_bad.len());
    for b in &scan_bad {
        println!("  BAD {b}");
    }

    // Extra probes: what does hickory do with a compressed QNAME in the question?
    let mut compressed_q = header(0x3301, 0x0100, 1, 0, 0, 0);
    compressed_q.extend_from_slice(&[0xc0, 0x0c]); // pointer to itself
    compressed_q.extend_from_slice(&1u16.to_be_bytes());
    compressed_q.extend_from_slice(&1u16.to_be_bytes());
    match Message::from_vec(&compressed_q) {
        Ok(m) => println!("compressed-qname: parsed, queries={}", m.queries().len()),
        Err(e) => println!("compressed-qname: rejected ({e})"),
    }

    // A response with a compression loop in the answer owner name.
    let mut loop_resp = header(0x3302, 0x8080, 1, 1, 0, 0);
    loop_resp.extend_from_slice(&question("www.example.com", 1));
    loop_resp.extend_from_slice(&[0xc0, 0x1e]); // points into itself
    loop_resp.extend_from_slice(&1u16.to_be_bytes());
    loop_resp.extend_from_slice(&1u16.to_be_bytes());
    loop_resp.extend_from_slice(&60u32.to_be_bytes());
    loop_resp.extend_from_slice(&4u16.to_be_bytes());
    loop_resp.extend_from_slice(&[1, 2, 3, 4]);
    match Message::from_vec(&loop_resp) {
        Ok(m) => println!("compression-loop: parsed, answers={}", m.answers().len()),
        Err(e) => println!("compression-loop: rejected ({e})"),
    }

    // qdcount=1 but the question name is a pointer to an answer (unusual).
    let mut unusual = header(0x3303, 0x0100, 1, 0, 0, 0);
    unusual.extend_from_slice(&name_wire("www.example.com"));
    unusual.extend_from_slice(&1u16.to_be_bytes());
    unusual.extend_from_slice(&1u16.to_be_bytes());
    match Message::from_vec(&unusual) {
        Ok(m) => {
            let _ = (m.header().message_type(), MessageType::Query, ResponseCode::NoError);
            println!("plain-query: ok");
        }
        Err(e) => println!("plain-query: rejected ({e})"),
    }
}
