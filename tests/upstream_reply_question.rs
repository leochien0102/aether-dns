#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/dns/mod.rs"]
mod dns;
#[path = "../src/upstream.rs"]
mod upstream;

use config::{Protocol, Upstream};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::timeout;

fn build_wire(
    id: u16,
    flags_hi: u8,
    flags_lo: u8,
    qdcount: u16,
    ancount: u16,
    arcount: u16,
    qname: &[u8],
    qtype: u16,
    qclass: u16,
) -> Vec<u8> {
    let mut wire = Vec::with_capacity(12 + qname.len() + 4);
    wire.extend_from_slice(&id.to_be_bytes());
    wire.push(flags_hi);
    wire.push(flags_lo);
    wire.extend_from_slice(&qdcount.to_be_bytes());
    wire.extend_from_slice(&ancount.to_be_bytes());
    wire.extend_from_slice(&0u16.to_be_bytes());
    wire.extend_from_slice(&arcount.to_be_bytes());
    wire.extend_from_slice(qname);
    wire.extend_from_slice(&qtype.to_be_bytes());
    wire.extend_from_slice(&qclass.to_be_bytes());
    wire
}

fn query_wire(id: u16, qname: &[u8], qtype: u16, qclass: u16) -> Vec<u8> {
    build_wire(id, 0x01, 0x00, 1, 0, 0, qname, qtype, qclass)
}

fn reply_wire(id: u16, qname: &[u8], qtype: u16, qclass: u16) -> Vec<u8> {
    let mut wire = build_wire(id, 0x81, 0x80, 1, 1, 0, qname, qtype, qclass);
    // Append a minimal A RR answer referencing offset 12.
    wire.extend_from_slice(&[
        0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 203, 0, 113, 9,
    ]);
    wire
}

fn max_len_qname(last_label_len: usize) -> Vec<u8> {
    let mut qname = Vec::with_capacity(256);
    for _ in 0..3 {
        qname.push(63);
        qname.extend_from_slice(&[b'a'; 63]);
    }
    qname.push(last_label_len as u8);
    qname.extend_from_slice(&vec![b'b'; last_label_len]);
    qname.push(0);
    qname
}

#[test]
fn valid_reply_accepts_exact_and_ascii_case_only_matches() {
    let q = query_wire(0x4242, b"\x03www\x07example\x03com\x00", 1, 1);

    // Exact match.
    let exact = reply_wire(0x4242, b"\x03www\x07example\x03com\x00", 1, 1);
    assert!(upstream::valid_reply(&exact, &q));

    // ASCII case-only difference across multiple labels.
    let mixed = reply_wire(0x4242, b"\x03WwW\x07ExAmPlE\x03COM\x00", 1, 1);
    assert!(upstream::valid_reply(&mixed, &q));

    // Root domain `.` with non-A QTYPE / non-IN QCLASS.
    let root_q = query_wire(0x1357, b"\x00", 2, 1);
    let root_r = build_wire(0x1357, 0x81, 0x80, 1, 0, 0, b"\x00", 2, 1);
    assert!(upstream::valid_reply(&root_r, &root_q));

    // Maximum legal 255-byte QNAME with ASCII case difference.
    let max_qname = max_len_qname(61);
    assert_eq!(max_qname.len(), 255);
    let mut max_qname_upper = max_qname.clone();
    for b in &mut max_qname_upper {
        if *b == b'a' {
            *b = b'A';
        } else if *b == b'b' {
            *b = b'B';
        }
    }
    let max_q = query_wire(0x2468, &max_qname, 28, 1);
    let max_r = build_wire(0x2468, 0x81, 0x80, 1, 0, 0, &max_qname_upper, 28, 1);
    assert!(upstream::valid_reply(&max_r, &max_q));

    // Identical non-ASCII label octets still match.
    let utf8_q = query_wire(0x55aa, b"\x02\xc3\x89\x07example\x00", 1, 1);
    let utf8_same = reply_wire(0x55aa, b"\x02\xc3\x89\x07EXAMPLE\x00", 1, 1);
    assert!(upstream::valid_reply(&utf8_same, &utf8_q));
}

#[test]
fn valid_reply_rejects_qname_qtype_qclass_and_non_ascii_case_mismatches() {
    let q = query_wire(0x4242, b"\x03www\x07example\x03com\x00", 1, 1);

    // QNAME label content mismatch.
    let bad_qname = reply_wire(0x4242, b"\x03api\x07example\x03com\x00", 1, 1);
    assert!(!upstream::valid_reply(&bad_qname, &q));

    // QNAME label count mismatch (extra and fewer labels).
    let extra_label = reply_wire(0x4242, b"\x01a\x03www\x07example\x03com\x00", 1, 1);
    let fewer_label = reply_wire(0x4242, b"\x07example\x03com\x00", 1, 1);
    assert!(!upstream::valid_reply(&extra_label, &q));
    assert!(!upstream::valid_reply(&fewer_label, &q));

    // Label boundary mismatch with identical concatenated bytes (`a.bc` vs `ab.c`).
    let q_boundary = query_wire(0x4242, b"\x01a\x02bc\x00", 1, 1);
    let r_boundary = reply_wire(0x4242, b"\x02ab\x01c\x00", 1, 1);
    assert!(!upstream::valid_reply(&r_boundary, &q_boundary));

    // Non-ASCII case must NOT be folded (`É` 0xc3 0x89 != `é` 0xc3 0xa9).
    let q_non_ascii = query_wire(0x55aa, b"\x02\xc3\x89\x07example\x00", 1, 1);
    let r_non_ascii = reply_wire(0x55aa, b"\x02\xc3\xa9\x07example\x00", 1, 1);
    assert!(!upstream::valid_reply(&r_non_ascii, &q_non_ascii));

    // QTYPE mismatch (A=1 vs AAAA=28).
    let bad_qtype = reply_wire(0x4242, b"\x03www\x07example\x03com\x00", 28, 1);
    assert!(!upstream::valid_reply(&bad_qtype, &q));

    // QCLASS mismatch (IN=1 vs CH=3).
    let bad_qclass = reply_wire(0x4242, b"\x03www\x07example\x03com\x00", 1, 3);
    assert!(!upstream::valid_reply(&bad_qclass, &q));
}

#[test]
fn valid_reply_preserves_header_checks() {
    let q = query_wire(0x4242, b"\x01a\x07example\x00", 1, 1);

    // QR=0 (query instead of response).
    let mut qr0 = reply_wire(0x4242, b"\x01a\x07example\x00", 1, 1);
    qr0[2] &= 0x7f;
    assert!(!upstream::valid_reply(&qr0, &q));

    // ID mismatch.
    let bad_id = reply_wire(0x4243, b"\x01a\x07example\x00", 1, 1);
    assert!(!upstream::valid_reply(&bad_id, &q));

    // QDCOUNT=0.
    let mut qd0 = reply_wire(0x4242, b"\x01a\x07example\x00", 1, 1);
    qd0[4..6].copy_from_slice(&0u16.to_be_bytes());
    assert!(!upstream::valid_reply(&qd0, &q));

    // QDCOUNT=2.
    let mut qd2 = reply_wire(0x4242, b"\x01a\x07example\x00", 1, 1);
    qd2[4..6].copy_from_slice(&2u16.to_be_bytes());
    assert!(!upstream::valid_reply(&qd2, &q));
}

#[test]
fn valid_reply_safely_rejects_short_and_unreadable_questions() {
    let q = query_wire(0x4242, b"\x01a\x07example\x00", 1, 1);
    let r = reply_wire(0x4242, b"\x01a\x07example\x00", 1, 1);

    // Every prefix of a valid reply up to (but not including) the end of the question must fail without panic.
    let question_end = q.len();
    for cut in 0..question_end {
        assert!(
            !upstream::valid_reply(&r[..cut], &q),
            "truncated response prefix len={cut} must be rejected"
        );
    }

    // Short or malformed query inputs must also return false without panic.
    for cut in 0..question_end {
        assert!(
            !upstream::valid_reply(&r, &q[..cut]),
            "truncated query prefix len={cut} must be rejected"
        );
    }
    let mut q_qd0 = q.clone();
    q_qd0[4..6].copy_from_slice(&0u16.to_be_bytes());
    assert!(!upstream::valid_reply(&r, &q_qd0));

    // Illegal label type bits (0x40, 0x80, 0xbf).
    for bad_len in [0x40u8, 0x80, 0xbf] {
        let mut wire = build_wire(0x4242, 0x81, 0x80, 1, 0, 0, b"", 1, 1);
        wire.truncate(12);
        wire.extend_from_slice(&[bad_len, b'a', 0, 0, 1, 0, 1]);
        assert!(!upstream::valid_reply(&wire, &q));
    }

    // Malformed compression pointers in QNAME: truncated, into header, self-loop, forward, OOB, cycle.
    let bad_pointers: &[&[u8]] = &[
        &[0xc0],
        &[0xc0, 0x00, 0x00, 0x01, 0x00, 0x01],
        &[0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01],
        &[0xc0, 0x20, 0x00, 0x01, 0x00, 0x01],
        &[0xff, 0xff, 0x00, 0x01, 0x00, 0x01],
        &[0x01, b'a', 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01],
    ];
    for payload in bad_pointers {
        let mut wire = vec![0x42, 0x42, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        wire.extend_from_slice(payload);
        assert!(
            !upstream::valid_reply(&wire, &q),
            "malformed pointer {payload:?} must be rejected"
        );
    }

    // Backward compression pointer inside the first QNAME landing inside a prior
    // label's payload bytes: if followed, offset 13 (`\x01x\x00`) would produce
    // the exact same second label and root terminator as `q_embedded`, so this
    // rejection proves compression pointers are disallowed in the first QNAME
    // rather than failing due to a name mismatch.
    let q_embedded = query_wire(0x4242, b"\x03\x01x\x00\x01x\x00", 1, 1);
    let r_embedded_uncompressed = reply_wire(0x4242, b"\x03\x01x\x00\x01x\x00", 1, 1);
    assert!(
        upstream::valid_reply(&r_embedded_uncompressed, &q_embedded),
        "uncompressed baseline with Answer compression pointer must match"
    );
    let mut r_embedded_ptr = build_wire(0x4242, 0x81, 0x80, 1, 1, 0, b"\x03\x01x\x00\xc0\x0d", 1, 1);
    r_embedded_ptr.extend_from_slice(&[
        0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 203, 0, 113, 9,
    ]);
    assert!(
        !upstream::valid_reply(&r_embedded_ptr, &q_embedded),
        "backward pointer into prior label data inside first QNAME must be rejected"
    );

    // Overlong QNAME (256 wire bytes).
    let overlong = max_len_qname(62);
    assert_eq!(overlong.len(), 256);
    let overlong_q = query_wire(0x4242, &overlong, 1, 1);
    let overlong_r = build_wire(0x4242, 0x81, 0x80, 1, 0, 0, &overlong, 1, 1);
    assert!(!upstream::valid_reply(&overlong_r, &overlong_q));
}

#[test]
fn valid_reply_accepts_legal_tc_with_truncated_tail_and_rejects_bad_tc() {
    let q = query_wire(0x4242, b"\x03www\x07example\x03com\x00", 1, 1);

    // Legal TC=1: header advertises ANCOUNT=2, ARCOUNT=1, question is complete & matches,
    // but packet ends right after Question.
    let tc_no_tail = build_wire(
        0x4242,
        0x83, // QR=1, RD=1, TC=1
        0x80,
        1,
        2,
        1,
        b"\x03www\x07example\x03com\x00",
        1,
        1,
    );
    assert!(upstream::valid_reply(&tc_no_tail, &q));

    // Legal TC=1: question matches (ASCII case-only diff) and trailing answer RR is cut mid-record.
    let mut tc_partial_rr = build_wire(
        0x4242,
        0x83,
        0x80,
        1,
        2,
        1,
        b"\x03WWW\x07Example\x03COM\x00",
        1,
        1,
    );
    tc_partial_rr.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00]);
    assert!(upstream::valid_reply(&tc_partial_rr, &q));

    // TC=1 with mismatched QNAME/QTYPE/QCLASS must still be rejected.
    let tc_bad_qname = build_wire(0x4242, 0x83, 0x80, 1, 1, 0, b"\x03bad\x07example\x03com\x00", 1, 1);
    let tc_bad_qtype = build_wire(0x4242, 0x83, 0x80, 1, 1, 0, b"\x03www\x07example\x03com\x00", 28, 1);
    let tc_bad_qclass = build_wire(0x4242, 0x83, 0x80, 1, 1, 0, b"\x03www\x07example\x03com\x00", 1, 3);
    assert!(!upstream::valid_reply(&tc_bad_qname, &q));
    assert!(!upstream::valid_reply(&tc_bad_qtype, &q));
    assert!(!upstream::valid_reply(&tc_bad_qclass, &q));

    // TC=1 with truncated Question itself must be rejected.
    assert!(!upstream::valid_reply(&tc_no_tail[..tc_no_tail.len() - 1], &q));
}

#[tokio::test(flavor = "current_thread")]
async fn udp_exchange_ignores_mismatched_question_and_accepts_subsequent_match() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let q = query_wire(0x7171, b"\x03www\x07example\x03com\x00", 1, 1);

    let wrong_qname = reply_wire(0x7171, b"\x05other\x07example\x03com\x00", 1, 1);
    let wrong_qtype = reply_wire(0x7171, b"\x03www\x07example\x03com\x00", 28, 1);
    let wrong_qclass = reply_wire(0x7171, b"\x03www\x07example\x03com\x00", 1, 3);
    let valid_case_tc = build_wire(
        0x7171,
        0x83,
        0x80,
        1,
        2,
        0,
        b"\x03WWW\x07Example\x03COM\x00",
        1,
        1,
    );
    let expected = valid_case_tc.clone();

    let worker = tokio::spawn(async move {
        let mut buf = [0u8; 512];
        let (_, peer) = server.recv_from(&mut buf).await.unwrap();
        for pkt in [&wrong_qname, &wrong_qtype, &wrong_qclass, &valid_case_tc] {
            server.send_to(pkt, peer).await.unwrap();
        }
    });

    let upstream_cfg = Upstream {
        protocol: Some(Protocol::Udp),
        host: None,
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: Some(port),
    };
    let reply = timeout(
        Duration::from_secs(2),
        upstream::exchange(&upstream_cfg, &q, Protocol::Udp),
    )
    .await
    .expect("must finish well within 5s UDP timeout budget")
    .expect("valid matching TC reply after mismatched packets must succeed");
    assert_eq!(reply, expected);
    worker.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn tcp_exchange_returns_invalid_data_on_question_mismatch_and_accepts_valid_tc() {
    let q = query_wire(0x8282, b"\x03www\x07example\x03com\x00", 1, 1);

    // Mismatched QNAME, QTYPE, and QCLASS each yield io::ErrorKind::InvalidData on TCP.
    let bad_replies = [
        reply_wire(0x8282, b"\x05other\x07example\x03com\x00", 1, 1),
        reply_wire(0x8282, b"\x03www\x07example\x03com\x00", 28, 1),
        reply_wire(0x8282, b"\x03www\x07example\x03com\x00", 1, 3),
    ];
    for bad in bad_replies {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let len = stream.read_u16().await.unwrap() as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_u16(bad.len() as u16).await.unwrap();
            stream.write_all(&bad).await.unwrap();
        });
        let upstream_cfg = Upstream {
            protocol: Some(Protocol::Tcp),
            host: None,
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: Some(port),
        };
        let err = timeout(
            Duration::from_secs(2),
            upstream::exchange(&upstream_cfg, &q, Protocol::Tcp),
        )
        .await
        .expect("TCP exchange must not hang")
        .expect_err("mismatched question on TCP must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        worker.await.unwrap();
    }

    // Legal TC=1 reply with ASCII case-different QNAME and truncated trailing RR succeeds on TCP.
    let mut valid_tc = build_wire(
        0x8282,
        0x83,
        0x80,
        1,
        1,
        0,
        b"\x03WWW\x07Example\x03COM\x00",
        1,
        1,
    );
    valid_tc.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x01]);
    let expected = valid_tc.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let len = stream.read_u16().await.unwrap() as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await.unwrap();
        stream.write_u16(valid_tc.len() as u16).await.unwrap();
        stream.write_all(&valid_tc).await.unwrap();
    });
    let upstream_cfg = Upstream {
        protocol: Some(Protocol::Tcp),
        host: None,
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: Some(port),
    };
    let reply = timeout(
        Duration::from_secs(2),
        upstream::exchange(&upstream_cfg, &q, Protocol::Tcp),
    )
    .await
    .expect("TCP exchange must finish promptly")
    .expect("legal TC reply with matching question must succeed");
    assert_eq!(reply, expected);
    worker.await.unwrap();
}
