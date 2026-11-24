#[path = "../src/nftset.rs"]
mod nftset;

use nftset::{NftSet, SetName};
use std::io::Write;
use std::net::Ipv4Addr;
use std::process::Command;
use std::time::Instant;

fn nft(input: &str) {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("nft test fixture executable");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "nft fixture failed");
}

fn ip(value: &str) -> Ipv4Addr {
    value.parse().unwrap()
}
fn pass(name: &str, start: Instant) {
    println!("PASS {name} ({:?})", start.elapsed());
}

#[test]
fn kernel_scenarios() {
    if std::env::var("NFT_TEST").as_deref() != Ok("1") {
        // Bypass libtest capture so direct `cargo test --test nftset` shows SKIP.
        writeln!(
            std::io::stdout(),
            "SKIP nftset: NFT_TEST=1 and isolated unshare -Urn required"
        )
        .unwrap();
        return;
    }
    let start = Instant::now();
    let set: SetName = "inet@m2nft@addresses".parse().unwrap();
    let mut client = NftSet::open().unwrap();
    if std::env::var("NFT_EPERM_TEST").as_deref() == Ok("1") {
        let error = client.contains(&set, ip("1.2.3.77")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        let error = client.add_batch(&set, &[ip("192.0.2.1")]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        pass("no CAP_NET_ADMIN: identifiable EPERM, no panic", start);
        return;
    }
    assert!(client.contains(&set, ip("1.2.3.77")).unwrap());
    assert!(!client.contains(&set, ip("192.0.2.1")).unwrap());
    pass("found/not-found", start);

    let start = Instant::now();
    client
        .add(&set, &[ip("192.0.2.1"), ip("192.0.2.2"), ip("192.0.2.3")])
        .unwrap();
    for address in ["192.0.2.1", "192.0.2.2", "192.0.2.3"] {
        assert!(client.contains(&set, ip(address)).unwrap());
    }
    assert!(!client.contains(&set, ip("192.0.2.4")).unwrap());
    client.add_batch(&set, &[ip("192.0.2.1")]).unwrap();
    pass("batch add, interval ends, exact duplicate ACK", start);

    let start = Instant::now();
    let error = client
        .add_batch(&set, &[ip("198.51.100.1"), ip("1.2.3.77")])
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
    assert!(
        !client.contains(&set, ip("198.51.100.1")).unwrap(),
        "failed batch must roll back"
    );
    client
        .add(
            &set,
            &[ip("1.2.3.77"), ip("198.51.100.1"), ip("198.51.100.1")],
        )
        .unwrap();
    assert!(client.contains(&set, ip("198.51.100.1")).unwrap());
    pass("interval EEXIST rollback and test-then-add", start);

    let start = Instant::now();
    let missing: SetName = "inet@m2nft@missing".parse().unwrap();
    assert!(!client.contains(&missing, ip("192.0.2.1")).unwrap());
    let error = client.add(&missing, &[ip("192.0.2.1")]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    pass("missing set ENOENT", start);

    let start = Instant::now();
    nft("delete table inet m2nft\n");
    let error = client.add(&set, &[ip("203.0.113.1")]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    nft(
        "add table inet m2nft\nadd set inet m2nft addresses { type ipv4_addr; flags interval; auto-merge; }\n",
    );
    client.add(&set, &[ip("203.0.113.1")]).unwrap();
    assert!(client.contains(&set, ip("203.0.113.1")).unwrap());
    assert!(!client.contains(&set, ip("192.0.2.1")).unwrap());
    pass("same socket delete/recreate without cached handle", start);
}

#[test]
fn max_ipv4_interval_kernel_scenarios() {
    if std::env::var("NFT_TEST").as_deref() != Ok("1")
        || std::env::var("NFT_EPERM_TEST").as_deref() == Ok("1")
    {
        writeln!(
            std::io::stdout(),
            "SKIP nftset max-ipv4: NFT_TEST=1 and isolated unshare -Urn required"
        )
        .unwrap();
        return;
    }

    let mut client = NftSet::open().unwrap();
    let assert_members = |client: &mut NftSet, set: &SetName, present: &[&str], absent: &[&str]| {
        for addr in present {
            assert!(
                client.contains(set, ip(addr)).unwrap(),
                "expected {addr} to be present in {set:?}"
            );
        }
        for addr in absent {
            assert!(
                !client.contains(set, ip(addr)).unwrap(),
                "expected {addr} to be absent in {set:?}"
            );
        }
    };

    // 1. Max IPv4 alone in an empty set (auto-merge) and duplicate ACK.
    let start = Instant::now();
    nft("add table inet t1674\nadd set inet t1674 max_empty { type ipv4_addr; flags interval; auto-merge; }\n");
    let max_empty: SetName = "inet@t1674@max_empty".parse().unwrap();
    client.add(&max_empty, &[ip("255.255.255.255")]).unwrap();
    assert_members(
        &mut client,
        &max_empty,
        &["255.255.255.255"],
        &["0.0.0.0", "0.0.0.1", "128.0.0.0", "255.255.255.254"],
    );
    client
        .add_batch(&max_empty, &[ip("255.255.255.255")])
        .unwrap();
    client.add(&max_empty, &[ip("0.0.0.0")]).unwrap();
    assert_members(
        &mut client,
        &max_empty,
        &["0.0.0.0", "255.255.255.255"],
        &["0.0.0.1", "255.255.255.254"],
    );
    pass("max IPv4 alone in empty set + duplicate ACK + later 0.0.0.0", start);

    // 2. Max IPv4 alone in a non-empty set (both with 1.2.3.0/24 and with existing 0.0.0.0)
    //    and in a non-auto-merge interval set.
    let start = Instant::now();
    nft(
        "add set inet t1674 max_nonempty { type ipv4_addr; flags interval; auto-merge; elements = { 1.2.3.0/24 }; }\n\
         add set inet t1674 max_with_zero { type ipv4_addr; flags interval; auto-merge; elements = { 0.0.0.0 }; }\n\
         add set inet t1674 max_no_automerge { type ipv4_addr; flags interval; elements = { 1.2.3.0/24 }; }\n",
    );
    let max_nonempty: SetName = "inet@t1674@max_nonempty".parse().unwrap();
    client.add(&max_nonempty, &[ip("255.255.255.255")]).unwrap();
    assert_members(
        &mut client,
        &max_nonempty,
        &["1.2.3.77", "255.255.255.255"],
        &["0.0.0.0", "0.0.0.1", "255.255.255.254"],
    );
    let max_with_zero: SetName = "inet@t1674@max_with_zero".parse().unwrap();
    client
        .add(&max_with_zero, &[ip("255.255.255.255")])
        .unwrap();
    assert_members(
        &mut client,
        &max_with_zero,
        &["0.0.0.0", "255.255.255.255"],
        &["0.0.0.1", "255.255.255.254"],
    );
    let max_no_automerge: SetName = "inet@t1674@max_no_automerge".parse().unwrap();
    client
        .add(&max_no_automerge, &[ip("255.255.255.255")])
        .unwrap();
    assert_members(
        &mut client,
        &max_no_automerge,
        &["1.2.3.77", "255.255.255.255"],
        &["0.0.0.0", "255.255.255.254"],
    );
    pass("max IPv4 in non-empty, zero-present, and non-auto-merge sets", start);

    // 3. Max IPv4 mixed with normal addresses in one batch (both orderings, plus same-batch with 0.0.0.0).
    let start = Instant::now();
    nft(
        "add set inet t1674 mix_max_first { type ipv4_addr; flags interval; auto-merge; elements = { 1.2.3.0/24 }; }\n\
         add set inet t1674 mix_max_last { type ipv4_addr; flags interval; auto-merge; elements = { 1.2.3.0/24 }; }\n\
         add set inet t1674 mix_with_zero { type ipv4_addr; flags interval; auto-merge; }\n",
    );
    let mix_max_first: SetName = "inet@t1674@mix_max_first".parse().unwrap();
    client
        .add(&mix_max_first, &[ip("255.255.255.255"), ip("192.0.2.10")])
        .unwrap();
    assert_members(
        &mut client,
        &mix_max_first,
        &["1.2.3.77", "192.0.2.10", "255.255.255.255"],
        &["0.0.0.0", "0.0.0.1", "192.0.2.9", "192.0.2.11", "255.255.255.254"],
    );
    let mix_max_last: SetName = "inet@t1674@mix_max_last".parse().unwrap();
    client
        .add(&mix_max_last, &[ip("192.0.2.10"), ip("255.255.255.255")])
        .unwrap();
    assert_members(
        &mut client,
        &mix_max_last,
        &["1.2.3.77", "192.0.2.10", "255.255.255.255"],
        &["0.0.0.0", "0.0.0.1", "192.0.2.9", "192.0.2.11", "255.255.255.254"],
    );
    let mix_with_zero: SetName = "inet@t1674@mix_with_zero".parse().unwrap();
    client
        .add(&mix_with_zero, &[ip("255.255.255.255"), ip("0.0.0.0")])
        .unwrap();
    assert_members(
        &mut client,
        &mix_with_zero,
        &["0.0.0.0", "255.255.255.255"],
        &["0.0.0.1", "128.0.0.0", "255.255.255.254"],
    );
    pass("max IPv4 mixed with normal addresses and 0.0.0.0 in one batch", start);

    // 4. Previous-to-max IPv4 (255.255.255.254) alone and alongside 255.255.255.255.
    let start = Instant::now();
    nft(
        "add set inet t1674 prev_max { type ipv4_addr; flags interval; auto-merge; }\n\
         add set inet t1674 prev_and_max { type ipv4_addr; flags interval; auto-merge; }\n",
    );
    let prev_max: SetName = "inet@t1674@prev_max".parse().unwrap();
    client.add(&prev_max, &[ip("255.255.255.254")]).unwrap();
    assert_members(
        &mut client,
        &prev_max,
        &["255.255.255.254"],
        &["0.0.0.0", "255.255.255.253", "255.255.255.255"],
    );
    let prev_and_max: SetName = "inet@t1674@prev_and_max".parse().unwrap();
    client
        .add(&prev_and_max, &[ip("255.255.255.254"), ip("255.255.255.255")])
        .unwrap();
    assert_members(
        &mut client,
        &prev_and_max,
        &["255.255.255.254", "255.255.255.255"],
        &["0.0.0.0", "0.0.0.1", "255.255.255.253"],
    );
    pass("255.255.255.254 alone and with 255.255.255.255", start);

    // 5. Normal batch and EEXIST rollback with overlapping 255.255.255.0/24.
    let start = Instant::now();
    nft(
        "add set inet t1674 normal_batch { type ipv4_addr; flags interval; auto-merge; }\n\
         add set inet t1674 max_overlap { type ipv4_addr; flags interval; auto-merge; elements = { 255.255.255.0/24 }; }\n",
    );
    let normal_batch: SetName = "inet@t1674@normal_batch".parse().unwrap();
    client
        .add(
            &normal_batch,
            &[ip("192.0.2.10"), ip("192.0.2.20"), ip("192.0.2.30")],
        )
        .unwrap();
    assert_members(
        &mut client,
        &normal_batch,
        &["192.0.2.10", "192.0.2.20", "192.0.2.30"],
        &[
            "0.0.0.0",
            "192.0.2.9",
            "192.0.2.11",
            "192.0.2.19",
            "192.0.2.21",
            "192.0.2.29",
            "192.0.2.31",
            "255.255.255.255",
        ],
    );
    let max_overlap: SetName = "inet@t1674@max_overlap".parse().unwrap();
    let err = client
        .add_batch(&max_overlap, &[ip("198.51.100.1"), ip("255.255.255.255")])
        .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EEXIST));
    assert_members(
        &mut client,
        &max_overlap,
        &["255.255.255.255"],
        &["0.0.0.0", "198.51.100.1"],
    );
    client
        .add(&max_overlap, &[ip("255.255.255.255"), ip("198.51.100.1")])
        .unwrap();
    assert_members(
        &mut client,
        &max_overlap,
        &["198.51.100.1", "255.255.255.255"],
        &["0.0.0.0", "198.51.100.2"],
    );
    pass("normal batch and 255.255.255.0/24 EEXIST rollback", start);

    nft("delete table inet t1674\n");
}
