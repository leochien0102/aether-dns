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
