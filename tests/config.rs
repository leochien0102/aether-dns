#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;

use config::{Config, Protocol, Tag};
use std::path::Path;

fn parse(input: &str) -> Config {
    Config::parse("test.conf", input.as_bytes()).unwrap()
}
fn invalid(input: &[u8], line: usize) {
    let error = Config::parse("bad.conf", input).unwrap_err();
    assert!(
        error.to_string().contains(&format!("bad.conf:{line}:")),
        "{error}"
    );
}

#[test]
fn config_fixtures() {
    let root = Path::new("/workspace/openwrt/aether-proxy/tests/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path().join("expected/chinadns.conf"))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    assert_eq!(files.len(), 22);
    for file in &files {
        let c = Config::read(file).unwrap_or_else(|e| panic!("{e}"));
        assert!(!c.trust_dns.is_empty(), "{}", file.display());
        c.validate_startup().unwrap();
    }
    println!("22/22 fixtures parsed");
}
#[test]
fn config_append() {
    let c = parse(
        "bind-addr 127.0.0.1\nbind-addr 127.0.0.2\nbind-port 53\nbind-port 54\nchina-dns 1.1.1.1\nchina-dns 2.2.2.2\ntrust-dns 3.3.3.3\ntrust-dns 4.4.4.4\nchnlist-file a,b\nchnlist-file c\ngfwlist-file d\ngfwlist-file e\nno-ipv6\nno-ipv6\n",
    );
    assert_eq!(c.bind_addr.len(), 2);
    assert_eq!(c.bind_port, [53, 54]);
    assert_eq!(c.china_dns.len(), 2);
    assert_eq!(c.trust_dns.len(), 2);
    assert_eq!(c.chnlist_file, ["a", "b", "c"]);
    assert_eq!(c.gfwlist_file, ["d", "e"]);
    assert_eq!(c.no_ipv6, 2);
}
#[test]
fn config_union() {
    let c = parse("filter-qtype 1,65\nfilter-qtype 65,28\n");
    assert_eq!(c.filter_qtype.into_iter().collect::<Vec<_>>(), [1, 28, 65]);
}
#[test]
fn config_last_wins() {
    let c = parse(
        "default-tag chn\ndefault-tag gfw\nipset-name4 inet@t@a\nipset-name4 inet@t@b\nipset-name6 inet@t@a\nipset-name6 null\nadd-taggfw-ip inet@t@a,null\nadd-taggfw-ip inet@t@b,null\ncache 1\ncache 2\ncache-stale 3\ncache-stale 4\nverdict-cache 5\nverdict-cache 6\n",
    );
    assert_eq!(c.default_tag, Some(Tag::Gfw));
    assert_eq!(c.ipset_name4.unwrap().set, "b");
    assert_eq!(c.ipset_name6, None);
    assert_eq!(c.add_taggfw_ip.unwrap().v4.unwrap().set, "b");
    assert_eq!(c.cache, Some(2));
    assert_eq!(c.cache_stale, Some(4));
    assert_eq!(c.verdict_cache, Some(6));
}
#[test]
fn config_groups() {
    let c = parse(
        "group a\ngroup-dnl a,b\ngroup-dnl c\ngroup-upstream 1.1.1.1\ngroup-upstream 2.2.2.2\ngroup-ipset inet@t@a,null\ngroup-ipset inet@t@b,null\ngroup a\ngroup-dnl d\n",
    );
    assert_eq!(c.groups.len(), 2);
    assert_eq!(c.groups[0].dnl, ["a", "b", "c"]);
    assert_eq!(c.groups[0].upstream.len(), 2);
    assert_eq!(
        c.groups[0].ipset.as_ref().unwrap().v4.as_ref().unwrap().set,
        "b"
    );
    assert_eq!(c.groups[1].dnl, ["d"]);
}
#[test]
fn config_tls_startup() {
    let c = parse("trust-dns tls://dns.example@1.1.1.1#853\n");
    assert_eq!(c.trust_dns[0].protocol, Some(Protocol::Tls));
    assert_eq!(c.trust_dns[0].host.as_deref(), Some("dns.example"));
    assert_eq!(
        c.trust_dns[0].ip,
        "1.1.1.1".parse::<std::net::IpAddr>().unwrap()
    );
    assert_eq!(c.trust_dns[0].port, Some(853));
    assert!(c.validate_startup().unwrap_err().contains("P0-conditional"));
}
#[test]
fn config_invalid_unknown() {
    invalid(b"# hi\nunknown 1", 2);
}
#[test]
fn config_invalid_missing() {
    invalid(b"bind-port", 1);
}
#[test]
fn config_invalid_ipv4() {
    invalid(b"bind-addr ::1", 1);
}
#[test]
fn config_invalid_port() {
    invalid(b"bind-port 0", 1);
}
#[test]
fn config_invalid_port_proto() {
    invalid(b"bind-port 53@udp", 1);
}
#[test]
fn config_invalid_tag() {
    invalid(b"default-tag x", 1);
}
#[test]
fn config_invalid_qtype() {
    invalid(b"filter-qtype 65536", 1);
}
#[test]
fn config_invalid_cache() {
    invalid(b"cache -1", 1);
}
#[test]
fn config_invalid_group() {
    invalid(b"group-dnl file", 1);
}
#[test]
fn config_invalid_no_ipv6() {
    invalid(b"no-ipv6 rule", 1);
}
#[test]
fn config_invalid_upstream() {
    invalid(b"trust-dns tcp://bad", 1);
}
#[test]
fn config_invalid_list() {
    invalid(b"chnlist-file a,,b", 1);
}
#[test]
fn config_invalid_comment() {
    invalid(b"cache 2 # comment", 1);
}
#[test]
fn config_invalid_set() {
    invalid(b"ipset-name4 broken", 1);
}
#[test]
fn config_invalid_utf8() {
    invalid(b"# comment\n\xff", 2);
}
