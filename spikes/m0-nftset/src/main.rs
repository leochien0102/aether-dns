//! M0 nftset spike: direct netlink query/add against `inet@aetherproxy@<set>`.
//!
//! Usage:
//!   m0-nftset test  <family@table@set> <ip>
//!   m0-nftset dump  <family@table@set>
//!   m0-nftset add   <family@table@set> <ip>... [--flags req|req+ack|req+create+ack|req+create+excl+ack]
//!   m0-nftset bench-test <family@table@set> <ip> <n>
//!   m0-nftset bench-add  <family@table@set> <n>
//!
//! Everything is one process, one netlink socket, no `nft` shell-out.

mod netlink;

use netlink::*;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::process::ExitCode;
use std::time::Instant;

fn errno_str(e: i32) -> String {
    unsafe {
        let p = libc::strerror(-e);
        if p.is_null() {
            format!("errno {}", -e)
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

struct SetName {
    family: u8,
    table: String,
    set: String,
}

fn parse_set(s: &str) -> Result<SetName, String> {
    let parts: Vec<&str> = s.split('@').collect();
    if parts.len() != 3 {
        return Err(format!("want family@table@set, got '{s}'"));
    }
    let family = match parts[0] {
        "ip" => NFPROTO_IPV4,
        "ip6" => NFPROTO_IPV6,
        "inet" => NFPROTO_INET,
        other => return Err(format!("unsupported family '{other}'")),
    };
    if parts[1].is_empty() || parts[2].is_empty() {
        return Err(format!("empty table/set in '{s}'"));
    }
    Ok(SetName { family, table: parts[1].to_string(), set: parts[2].to_string() })
}

fn parse_ip(s: &str) -> Result<(Vec<u8>, bool), String> {
    if let Ok(v4) = s.parse::<Ipv4Addr>() {
        return Ok((v4.octets().to_vec(), true));
    }
    if let Ok(v6) = s.parse::<Ipv6Addr>() {
        return Ok((v6.octets().to_vec(), false));
    }
    Err(format!("not an IP: '{s}'"))
}

fn build_test(set: &SetName, ip: &[u8], seq: u32) -> Vec<u8> {
    let mut m = Msg::new((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_GETSETELEM, NLM_F_REQUEST, set.family, 0, seq);
    m.attr_str(NFTA_SET_ELEM_LIST_TABLE, &set.table);
    m.attr_str(NFTA_SET_ELEM_LIST_SET, &set.set);
    let elems = m.nest(NFTA_SET_ELEM_LIST_ELEMENTS);
    add_elem(&mut m, ip, false);
    m.nest_end(elems);
    m.finish()
}

fn build_add(set: &SetName, ips: &[(Vec<u8>, bool)], flags: u16, seq: u32) -> Vec<u8> {
    let mut begin = Msg::new(NFNL_MSG_BATCH_BEGIN, NLM_F_REQUEST, 0, NFNL_SUBSYS_NFTABLES, seq).finish();
    let mut m = Msg::new((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_NEWSETELEM, flags, set.family, 0, seq);
    m.attr_str(NFTA_SET_ELEM_LIST_TABLE, &set.table);
    m.attr_str(NFTA_SET_ELEM_LIST_SET, &set.set);
    let elems = m.nest(NFTA_SET_ELEM_LIST_ELEMENTS);
    for (ip, _v4) in ips {
        add_elem(&mut m, ip, false);
        add_elem(&mut m, &ip_plus_one(ip), true);
    }
    m.nest_end(elems);
    let mut body = m.finish();
    let mut end = Msg::new(NFNL_MSG_BATCH_END, NLM_F_REQUEST, 0, NFNL_SUBSYS_NFTABLES, seq).finish();
    begin.append(&mut body);
    begin.append(&mut end);
    begin
}

/// Drain responses until ACK / error / timeout. Returns the netlink error code, if any.
fn recv_ack(sock: &Socket, seq: u32) -> io::Result<Option<i32>> {
    let deadline = Instant::now() + std::time::Duration::from_millis(500);
    loop {
        if Instant::now() > deadline {
            return Ok(None);
        }
        match sock.recv()? {
            None => return Ok(None),
            Some(buf) => {
                for msg in parse_msgs(&buf) {
                    if msg.seq != seq {
                        continue;
                    }
                    if msg.ty == NLMSG_ERROR {
                        let code = msg.error_code().unwrap_or(0);
                        return Ok(Some(code));
                    }
                }
            }
        }
    }
}

fn cmd_test(set: &SetName, ip: &[u8], v4: bool) -> io::Result<()> {
    let mut sock = Socket::open()?;
    let seq = sock.next_seq();
    let req = build_test(set, ip, seq);
    let t0 = Instant::now();
    sock.send(&req)?;
    let resp = loop {
        match sock.recv()? {
            None => break None,
            Some(buf) => {
                let mut found = false;
                let mut err = None;
                let mut any = false;
                for m in parse_msgs(&buf) {
                    if m.seq != seq && m.seq != 0 {
                        continue;
                    }
                    any = true;
                    if m.ty == ((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_NEWSETELEM) {
                        found = true;
                    }
                    if m.ty == NLMSG_ERROR {
                        err = m.error_code();
                    }
                }
                if any {
                    break Some((found, err));
                }
            }
        }
    };
    let dt = t0.elapsed();
    let ip_s = if v4 {
        Ipv4Addr::from(<[u8; 4]>::try_from(ip).unwrap()).to_string()
    } else {
        Ipv6Addr::from(<[u8; 16]>::try_from(ip).unwrap()).to_string()
    };
    match resp {
        None => println!("test {ip_s}: NO_RESPONSE ({:?})", dt),
        Some((found, err)) => {
            if found {
                println!("test {ip_s}: FOUND ({:?})", dt);
            } else {
                match err {
                    Some(0) => println!("test {ip_s}: ACK_ONLY ({:?})", dt),
                    Some(code) => println!("test {ip_s}: NOT_FOUND errno={code} ({}) ({:?})", errno_str(code), dt),
                    None => println!("test {ip_s}: unexpected response ({:?})", dt),
                }
            }
        }
    }
    Ok(())
}

/// Dump all members. For interval sets the kernel reports begin elements and
/// exclusive end elements (flag INTERVAL_END); pair them back into ranges.
fn cmd_dump(set: &SetName) -> io::Result<()> {
    let mut sock = Socket::open()?;
    let seq = sock.next_seq();
    let mut m = Msg::new(
        (NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_GETSETELEM,
        NLM_F_REQUEST | NLM_F_DUMP,
        set.family,
        0,
        seq,
    );
    m.attr_str(NFTA_SET_ELEM_LIST_TABLE, &set.table);
    m.attr_str(NFTA_SET_ELEM_LIST_SET, &set.set);
    sock.send(&m.finish())?;

    let mut elems: Vec<(Vec<u8>, bool)> = Vec::new();
    let mut done = false;
    let mut errors: Vec<i32> = Vec::new();
    let mut msgs_total = 0usize;
    while !done {
        let Some(buf) = sock.recv()? else { break };
        for msg in parse_msgs(&buf) {
            msgs_total += 1;
            if msg.ty == NLMSG_DONE {
                done = true;
                continue;
            }
            if msg.ty == NLMSG_ERROR {
                errors.push(msg.error_code().unwrap_or(0));
                done = true;
                continue;
            }
            if msg.ty != ((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_NEWSETELEM) {
                continue;
            }
            // payload = nfgenmsg (4) + attrs
            let attrs = &msg.payload[4..];
            for (ty, data) in AttrIter::new(attrs) {
                if ty != NFTA_SET_ELEM_LIST_ELEMENTS {
                    continue;
                }
                for (ety, edata) in AttrIter::new(data) {
                    if ety != NFTA_LIST_ELEM {
                        continue;
                    }
                    let mut flags = 0u32;
                    let mut key: Option<Vec<u8>> = None;
                    for (aty, adata) in AttrIter::new(edata) {
                        match aty {
                            NFTA_SET_ELEM_FLAGS if adata.len() >= 4 => {
                                flags = u32::from_be_bytes(adata[0..4].try_into().unwrap());
                            }
                            NFTA_SET_ELEM_KEY => {
                                for (kty, kdata) in AttrIter::new(adata) {
                                    if kty == NFTA_DATA_VALUE {
                                        key = Some(kdata.to_vec());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(k) = key {
                        elems.push((k, flags & NFT_SET_ELEM_INTERVAL_END != 0));
                    }
                }
            }
        }
    }

    println!("set {}@{}@{}: {} messages, {} elements, errors={:?}", family_name(set.family), set.table, set.set, msgs_total, elems.len(), errors);
    if std::env::var("M0_RAW").is_ok() {
        for (k, end) in &elems {
            println!("  raw end={end} key={}", fmt_ip(k));
        }
    }

    // Pair ranges. The kernel dumps begin and INTERVAL_END endpoints in hash order,
    // with a trailing 0.0.0.0 end sentinel. Sort both sides and pair; drop the sentinel.
    let mut starts: Vec<Vec<u8>> = elems.iter().filter(|(_, end)| !*end).map(|(k, _)| k.clone()).collect();
    let mut ends: Vec<Vec<u8>> = elems.iter().filter(|(_, end)| *end).map(|(k, _)| k.clone()).collect();
    starts.sort();
    ends.sort();
    let zero = vec![0u8; starts.first().map(|s| s.len()).unwrap_or(4)];
    let had_sentinel = ends.first() == Some(&zero);
    if had_sentinel {
        ends.remove(0);
    }
    if starts.len() != ends.len() {
        println!("  ERROR: {} starts vs {} ends (sentinel={had_sentinel})", starts.len(), ends.len());
    }
    for (s, e) in starts.iter().zip(ends.iter()) {
        let single = ip_plus_one(e) == *s;
        if single {
            println!("  {}", fmt_ip(s));
        } else {
            println!("  {} - {}", fmt_ip(s), fmt_ip(&ip_minus_one(e)));
        }
    }
    println!("({} members, sentinel={had_sentinel})", starts.len());
    Ok(())
}

fn ip_minus_one(ip: &[u8]) -> Vec<u8> {
    let mut v = ip.to_vec();
    for i in (0..v.len()).rev() {
        let (nv, borrow) = v[i].overflowing_sub(1);
        v[i] = nv;
        if !borrow {
            break;
        }
    }
    v
}

fn family_name(f: u8) -> &'static str {
    match f {
        NFPROTO_INET => "inet",
        NFPROTO_IPV4 => "ip",
        NFPROTO_IPV6 => "ip6",
        _ => "?",
    }
}

fn fmt_ip(b: &[u8]) -> String {
    match b.len() {
        4 => Ipv4Addr::from(<[u8; 4]>::try_from(b).unwrap()).to_string(),
        16 => Ipv6Addr::from(<[u8; 16]>::try_from(b).unwrap()).to_string(),
        _ => format!("<{b:02x?}>"),
    }
}

fn test_one(sock: &mut Socket, set: &SetName, ip: &[u8]) -> io::Result<bool> {
    let seq = sock.next_seq();
    let req = build_test(set, ip, seq);
    sock.send(&req)?;
    loop {
        match sock.recv()? {
            None => return Ok(false),
            Some(buf) => {
                let mut found = false;
                let mut any = false;
                for m in parse_msgs(&buf) {
                    if m.seq != seq && m.seq != 0 {
                        continue;
                    }
                    any = true;
                    if m.ty == ((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_NEWSETELEM) {
                        found = true;
                    }
                }
                if any {
                    return Ok(found);
                }
            }
        }
    }
}

fn add_batch(sock: &mut Socket, set: &SetName, ips: &[(Vec<u8>, bool)], flags: u16) -> io::Result<Option<i32>> {
    let seq = sock.next_seq();
    let req = build_add(set, ips, flags, seq);
    sock.send(&req)?;
    recv_ack(sock, seq)
}

fn cmd_add(set: &SetName, ips: &[(Vec<u8>, bool)], flags: u16) -> io::Result<()> {
    let mut sock = Socket::open()?;
    let t0 = Instant::now();
    let code = add_batch(&mut sock, set, ips, flags)?;
    let dt = t0.elapsed();
    match code {
        Some(0) => println!("add {} ip(s): ACK_OK ({:?})", ips.len(), dt),
        Some(c) => println!("add {} ip(s): ERROR errno={c} ({}) ({:?})", ips.len(), errno_str(c), dt),
        None => println!("add {} ip(s): NO_ACK ({:?})", ips.len(), dt),
    }
    Ok(())
}

/// chinadns-ng-style safe update: test every IP, add only the missing ones in one
/// batch, retry once if a concurrent writer made one of them exist in the meantime.
fn cmd_add_safe(set: &SetName, ips: &[(Vec<u8>, bool)]) -> io::Result<()> {
    let mut sock = Socket::open()?;
    let t0 = Instant::now();
    let mut rounds = 0;
    loop {
        rounds += 1;
        let mut missing = Vec::new();
        for ip in ips {
            if !test_one(&mut sock, set, &ip.0)? {
                missing.push(ip.clone());
            }
        }
        if missing.is_empty() {
            println!("add-safe {} ip(s): all present after {rounds} round(s) ({:?})", ips.len(), t0.elapsed());
            return Ok(());
        }
        // Test-only hook: widen the test->add window so a concurrent writer can be
        // made to collide deterministically (M0 race evidence).
        if let Ok(ms) = std::env::var("M0_TEST_ADD_GAP_MS") {
            if let Ok(ms) = ms.parse::<u64>() {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
        let code = add_batch(&mut sock, set, &missing, NLM_F_REQUEST | NLM_F_CREATE | NLM_F_ACK)?;
        match code {
            Some(0) => {
                println!(
                    "add-safe {} ip(s): added {} missing ({} round(s)) ({:?})",
                    ips.len(),
                    missing.len(),
                    rounds,
                    t0.elapsed()
                );
                return Ok(());
            }
            Some(c) if c == -libc::EEXIST && rounds < 3 => {
                println!("add-safe: race (errno=-EEXIST), retrying (round {rounds})");
                continue;
            }
            Some(c) => {
                println!("add-safe {} ip(s): ERROR errno={c} ({}) after {rounds} round(s)", ips.len(), errno_str(c));
                return Ok(());
            }
            None => {
                println!("add-safe {} ip(s): NO_ACK after {rounds} round(s)", ips.len());
                return Ok(());
            }
        }
    }
}

fn cmd_bench_test(set: &SetName, ip: &[u8], n: usize) -> io::Result<()> {
    let mut sock = Socket::open()?;
    let seq = sock.next_seq();
    let mut req = build_test(set, ip, seq);
    let t0 = Instant::now();
    let mut hits = 0;
    for _ in 0..n {
        // reuse the same request buffer: no per-test allocation
        sock.send(&req)?;
        let resp = loop {
            match sock.recv()? {
                None => break None,
                Some(buf) => {
                    let mut found = false;
                    let mut any = false;
                    for m in parse_msgs(&buf) {
                        if m.seq != seq {
                            continue;
                        }
                        any = true;
                        if m.ty == ((NFNL_SUBSYS_NFTABLES << 8) | NFT_MSG_NEWSETELEM) {
                            found = true;
                        }
                    }
                    if any {
                        break Some(found);
                    }
                }
            }
        };
        if resp == Some(true) {
            hits += 1;
        }
    }
    let dt = t0.elapsed();
    println!("bench-test n={n} hits={hits} total={:?} per_op={:?}", dt, dt / n as u32);
    let _ = &mut req;
    Ok(())
}

fn cmd_bench_add(set: &SetName, n: usize) -> io::Result<()> {
    let mut sock = Socket::open()?;
    // batches of 10, like chinadns-ng IP_N
    let mut done = 0usize;
    let t0 = Instant::now();
    let mut seq = sock.next_seq();
    while done < n {
        let batch: Vec<(Vec<u8>, bool)> = (0..10.min(n - done))
            .map(|i| {
                let x = done + i;
                (vec![10, 99, (x >> 8) as u8, (x & 0xff) as u8], true)
            })
            .collect();
        let req = build_add(set, &batch, NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE, seq);
        sock.send(&req)?;
        let _ = recv_ack(&sock, seq)?;
        seq = sock.next_seq();
        done += batch.len();
    }
    let dt = t0.elapsed();
    println!("bench-add n={n} total={:?} per_ip={:?}", dt, dt / n as u32);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: m0-nftset test|dump|add|bench-test|bench-add <family@table@set> ...");
        return ExitCode::from(2);
    }
    let cmd = args[0].as_str();
    let set = match parse_set(&args[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let r: Result<(), String> = match cmd {
        "test" => match parse_ip(&args[2]) {
            Ok((ip, v4)) => cmd_test(&set, &ip, v4).map_err(|e| e.to_string()),
            Err(e) => Err(e),
        },
        "dump" => cmd_dump(&set).map_err(|e| e.to_string()),
        "add" => {
            let mut flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE;
            let mut ips = Vec::new();
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--flags" {
                    i += 1;
                    flags = match args.get(i).map(String::as_str) {
                        Some("req") => NLM_F_REQUEST,
                        Some("req+ack") => NLM_F_REQUEST | NLM_F_ACK,
                        Some("req+create+ack") => NLM_F_REQUEST | NLM_F_CREATE | NLM_F_ACK,
                        Some("req+create+excl+ack") => NLM_F_REQUEST | NLM_F_CREATE | NLM_F_EXCL | NLM_F_ACK,
                        Some("req+excl+ack") => NLM_F_REQUEST | NLM_F_EXCL | NLM_F_ACK,
                        other => {
                            eprintln!("bad flags {other:?}");
                            return ExitCode::from(2);
                        }
                    };
                } else {
                    match parse_ip(&args[i]) {
                        Ok(v) => ips.push(v),
                        Err(e) => {
                            eprintln!("{e}");
                            return ExitCode::from(2);
                        }
                    }
                }
                i += 1;
            }
            if ips.is_empty() {
                eprintln!("add needs at least one ip");
                return ExitCode::from(2);
            }
            cmd_add(&set, &ips, flags).map_err(|e| e.to_string())
        }
        "add-safe" => {
            let mut ips = Vec::new();
            for a in &args[2..] {
                match parse_ip(a) {
                    Ok(v) => ips.push(v),
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(2);
                    }
                }
            }
            if ips.is_empty() {
                eprintln!("add-safe needs at least one ip");
                return ExitCode::from(2);
            }
            cmd_add_safe(&set, &ips).map_err(|e| e.to_string())
        }
        "bench-test" => parse_ip(&args[2])
            .and_then(|(ip, _)| {
                let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
                cmd_bench_test(&set, &ip, n).map_err(|e| e.to_string())
            }),
        "bench-add" => {
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
            cmd_bench_add(&set, n).map_err(|e| e.to_string())
        }
        other => {
            eprintln!("unknown command {other}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
