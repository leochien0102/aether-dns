#[path = "../src/dns/mod.rs"]
mod dns;

use dns::*;
use hickory_proto::rr::RecordType;
use std::net::{IpAddr, Ipv4Addr};

fn query(kind: u16) -> Vec<u8> {
    let mut b = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 0];
    b.extend_from_slice(&kind.to_be_bytes());
    b.extend_from_slice(&[0, 1]);
    b
}
fn answer(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0xc0, 12];
    b.extend_from_slice(&kind.to_be_bytes());
    b.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
    b.extend_from_slice(&(data.len() as u16).to_be_bytes());
    b.extend_from_slice(data);
    b
}
fn response(kind: u16, data: Option<(u16, &[u8])>, flags: u8) -> Vec<u8> {
    let mut b = query(kind);
    b[2] = 0x81;
    b[3] = flags;
    if let Some((rtype, rdata)) = data {
        b[7] = 1;
        b.extend(answer(rtype, rdata));
    }
    b
}
fn opt(b: &mut Vec<u8>) {
    b[11] = 1;
    b.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn dns_wire_corpus() {
    let mut samples: Vec<(&str, Vec<u8>, usize)> = Vec::new();
    for (label, typ) in [("A", 1), ("AAAA", 28), ("HTTPS", 65)] {
        let b = query(typ);
        let end = b.len();
        samples.push((label, b, end));
    }
    let mut edns = query(1);
    let end = edns.len();
    opt(&mut edns);
    samples.push(("EDNS", edns, end));
    let mut aopt = response(1, Some((1, &[1, 2, 3, 4])), 0);
    let end = aopt.len();
    opt(&mut aopt);
    assert_eq!(end, aopt.len() - 11);
    samples.push(("resp-A+OPT", aopt, end));
    let mut cname = response(1, Some((5, &[1, b'b', 0])), 0);
    cname[7] = 2;
    cname.extend(answer(1, &[1, 2, 3, 4]));
    let end = cname.len();
    samples.push(("CNAME+A", cname, end));
    for (label, flags) in [("NXDOMAIN", 3), ("NODATA", 0)] {
        let b = response(1, None, flags);
        let end = b.len();
        samples.push((label, b, end));
    }
    let mut tc = response(1, None, 0);
    tc[2] |= 2;
    let end = tc.len();
    samples.push(("TC", tc, end));
    let b = response(1, Some((65280, &[1, 2, 3])), 0);
    let end = b.len();
    samples.push(("unknown RR", b, end));
    let b = response(1, Some((1, &[1, 2, 3, 4])), 0);
    let end = b.len();
    samples.push(("compressed", b, end));
    let mut two = query(1);
    two[5] = 2;
    two.extend_from_slice(&[1, b'b', 0, 0, 1, 0, 1]);
    let end = two.len();
    samples.push(("two questions", two, end));
    let mut garbage = query(1);
    let end = garbage.len();
    garbage.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    samples.push(("trailing garbage", garbage, end));
    for (label, b, end) in &samples {
        assert_eq!(cut_additional(b), Ok(*end), "{label}");
        if *label == "two questions" {
            assert!(check_query(b).is_err());
        } else {
            assert!(check_query(b).is_ok(), "{label}");
        }
        assert!(inspect(b).is_ok(), "{label}");
    }
    let parsed = inspect(&samples[4].1).unwrap();
    assert_eq!(parsed.id, 0x1234);
    assert_eq!(parsed.question.unwrap().1, RecordType::A);
    assert_eq!(
        parsed.answers,
        [(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 60)]
    );
    assert!(parsed.edns.is_some());
    println!("dns_wire corpus: {} samples", samples.len());
}

#[test]
fn dns_wire_raw_forward() {
    let mut b = response(1, Some((1, &[1, 2, 3, 4])), 0);
    opt(&mut b);
    let original = b.clone();
    let end = cut_additional(&b).unwrap();
    set_id(&mut b, 0xabcd).unwrap();
    set_ra(&mut b).unwrap();
    truncate_additional(&mut b).unwrap();
    assert_eq!(b.len(), original.len() - 11);
    for i in 0..end {
        match i {
            0 => assert_eq!(b[i], 0xab),
            1 => assert_eq!(b[i], 0xcd),
            3 => assert_eq!(b[i], original[i] | 0x80),
            10 | 11 => assert_eq!(b[i], 0),
            _ => assert_eq!(b[i], original[i], "byte {i}"),
        }
    }
}

#[test]
fn dns_wire_bounds_and_nodata() {
    assert!(check_query(&[0; 11]).is_err());
    let mut bad = query(1);
    bad[12] = 64;
    assert!(check_query(&bad).is_err());
    assert!(skip_name(&[0xc0], 0).is_err());
    assert!(skip_rr(&[0], 0).is_err());
    assert!(set_id(&mut [], 1).is_err());
    let mut q = query(1);
    q[2] |= 2;
    opt(&mut q);
    clear_tc(&mut q).unwrap();
    assert_eq!(q[2] & 2, 0);
    synthetic_nodata(&mut q).unwrap();
    assert_eq!(&q[2..4], &[0x81, 0x80]);
    assert_eq!(&q[6..12], &[0; 6]);
    assert_eq!(q.len(), 19);
}

#[test]
fn dns_wire_real_samples() {
    let samples = include_str!("../spikes/m0-fixtures/wire-samples.json");
    let mut count = 0;
    for line in samples.lines() {
        if let Some(hex) = line
            .trim()
            .strip_prefix("\"hex\": \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            assert!(inspect(&bytes).is_ok(), "sample {count}");
            assert!(check_query(&bytes).is_ok(), "sample {count}");
            count += 1;
        }
    }
    assert_eq!(count, 38);
    println!("dns_wire real samples: {count}/38 parsed");
}
