#!/usr/bin/env python3
"""M0 fixture harness for the aether-proxy chinadns-ng profile.

This is test scaffolding, not production code. It:

  1. parses every `aether-proxy/tests/fixtures/*/expected/chinadns.conf` into a
     directive model and records coverage;
  2. starts the real chinadns-ng on each config inside an isolated netns, with
     tmpfs overlays for the absolute list paths the configs reference;
  3. runs a deterministic fake upstream (UDP+TCP) on the upstream addresses the
     configs use, so routing decisions and nftset writes are observable;
  4. records a baseline (upstream hits, client reply, nft set contents) for the
     M1/M2 parity work.

Must run as root inside a network + mount namespace, e.g.:

    sudo ip netns exec aether-m0 unshare -m -- python3 harness.py all

`run-harness.sh` sets that up.
"""

import argparse
import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = "/workspace/openwrt/aether-proxy/tests/fixtures"
CHINADNS = os.environ.get("CHINADNS_BIN", "/tmp/ci-tools/chinadns-ng-2025.08.09/chinadns-ng")

# endpoint id, address, port. The id is encoded in fake A answers (10.0.0.<id>).
ENDPOINTS = [
    (1, "223.5.5.5", 53),
    (2, "119.29.29.29", 53),
    (3, "114.114.114.114", 53),
    (4, "202.96.128.86", 53),
    (5, "202.96.134.133", 53),
    (11, "127.0.0.1", 31082),
    (12, "127.0.0.1", 31083),
]

# Deterministic-ish latency per endpoint so the "first valid response" winner is stable.
LATENCY_MS = {1: 2, 2: 60, 3: 60, 4: 2, 5: 60, 11: 2, 12: 60}

SETS = ["chnroute4", "gfw4", "uproxy4", "udirect4"]

DIRECTIVES = [
    "bind-addr", "bind-port", "china-dns", "trust-dns", "ipset-name4", "ipset-name6",
    "gfwlist-file", "chnlist-file", "default-tag", "add-taggfw-ip", "group",
    "group-dnl", "group-upstream", "group-ipset", "no-ipv6", "filter-qtype",
    "cache", "cache-stale", "verdict-cache",
]

# ---------------------------------------------------------------------------
# DNS wire helpers (test-only, independent of the SUT)
# ---------------------------------------------------------------------------

def encode_name(name):
    out = bytearray()
    for label in name.strip(".").split("."):
        if not label:
            continue
        b = label.encode()
        out.append(len(b))
        out += b
    out.append(0)
    return bytes(out)


def read_name(buf, off):
    labels = []
    jumps = 0
    after_pointer = None
    while off < len(buf):
        n = buf[off]
        if n == 0:
            off += 1
            break
        if n & 0xC0 == 0xC0:
            ptr = ((n & 0x3F) << 8) | buf[off + 1]
            jumps += 1
            if jumps > 10:
                raise ValueError("compression loop")
            if after_pointer is None:
                after_pointer = off + 2
            off = ptr
            continue
        if n > 63:
            raise ValueError("bad label")
        labels.append(buf[off + 1:off + 1 + n].decode("ascii", "replace"))
        off += 1 + n
    return ".".join(labels) + ".", (after_pointer if after_pointer is not None else off)


def build_query(name, qtype, qid=0x4242, edns=False):
    flags = 0x0100  # RD
    hdr = struct.pack(">HHHHHH", qid, flags, 1, 0, 0, 1 if edns else 0)
    q = encode_name(name) + struct.pack(">HH", qtype, 1)
    opt = b""
    if edns:
        opt = b"\x00" + struct.pack(">HHIH", 41, 1232, 0, 0)
    return hdr + q + opt


def build_response(query, endpoint_id):
    """Build a fake upstream reply for one query (compressed owner for A)."""
    qid, qflags, _qd, _an, _ns, ar = struct.unpack(">HHHHHH", query[:12])
    off = 12
    qname, off = read_name(query, off)
    qtype, _qclass = struct.unpack(">HH", query[off:off + 4])
    question = query[12:off + 4]
    want_edns = ar > 0

    rcode = 0
    answers = []
    lower = qname.lower()

    if lower.startswith("nxdomain."):
        rcode = 3
    elif qtype == 1:  # A
        if "noncn" in lower and endpoint_id <= 5:
            ip = "203.0.113.9"  # china upstream sees a non-China answer
        else:
            ip = f"10.0.0.{endpoint_id}"
        answers.append((b"\xc0\x0c", 1, 30, socket.inet_aton(ip)))
    elif qtype == 28:  # AAAA
        answers.append((b"\xc0\x0c", 28, 30, socket.inet_pton(socket.AF_INET6, f"2001:db8::{endpoint_id}")))
    elif qtype == 65:  # HTTPS: priority 1 + root target, no params
        answers.append((b"\xc0\x0c", 65, 30, b"\x00\x01\x00"))
    elif qtype == 16:  # TXT
        answers.append((b"\xc0\x0c", 16, 30, b"\x04fake"))
    # else NODATA

    flags = 0x8000 | (qflags & 0x0100) | 0x0080 | rcode  # QR, RD copy, RA
    additional = b""
    if want_edns:
        additional = b"\x00" + struct.pack(">HHIH", 41, 1232, 0, 0)
    hdr = struct.pack(">HHHHHH", qid, flags, 1, len(answers), 0, 1 if want_edns else 0)
    body = b""
    for owner, rtype, ttl, rdata in answers:
        body += owner + struct.pack(">HHIH", rtype, 1, ttl, len(rdata)) + rdata
    return hdr + question + body + additional


def parse_message(buf):
    qid, flags, qd, an, ns, ar = struct.unpack(">HHHHHH", buf[:12])
    off = 12
    qname = None
    qtype = None
    for _ in range(qd):
        qname, off = read_name(buf, off)
        qtype, _qclass = struct.unpack(">HH", buf[off:off + 4])
        off += 4
    answers = []
    for _ in range(an):
        _owner, off = read_name(buf, off)
        rtype, _rclass, ttl, rdlen = struct.unpack(">HHIH", buf[off:off + 10])
        off += 10
        rdata = buf[off:off + rdlen]
        off += rdlen
        val = None
        if rtype == 1 and rdlen == 4:
            val = socket.inet_ntoa(rdata)
        elif rtype == 28 and rdlen == 16:
            val = socket.inet_ntop(socket.AF_INET6, rdata)
        answers.append({"type": rtype, "ttl": ttl, "value": val, "rdlen": rdlen})
    return {
        "id": qid,
        "flags": flags,
        "rcode": flags & 0x0F,
        "tc": bool(flags & 0x0200),
        "ra": bool(flags & 0x0080),
        "qname": qname,
        "qtype": qtype,
        "ancount": an,
        "nscount": ns,
        "arcount": ar,
        "answers": answers,
    }


# ---------------------------------------------------------------------------
# fake upstream
# ---------------------------------------------------------------------------

class FakeUpstream:
    def __init__(self):
        self.hits = []
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.socks = []
        self.threads = []

    def clear_hits(self):
        with self.lock:
            self.hits.clear()

    def snapshot_hits(self):
        with self.lock:
            return list(self.hits)

    def log(self, endpoint_id, addr, proto, qname, qtype, raw):
        with self.lock:
            self.hits.append({
                "endpoint": endpoint_id,
                "addr": addr,
                "proto": proto,
                "qname": qname,
                "qtype": qtype,
                "raw_hex": raw[:512].hex(),
            })

    def start(self):
        for eid, addr, port in ENDPOINTS:
            u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            u.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            u.bind((addr, port))
            u.settimeout(0.2)
            self.socks.append(u)
            t = threading.Thread(target=self.udp_loop, args=(u, eid, addr), daemon=True)
            t.start()
            self.threads.append(t)

            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s.bind((addr, port))
            s.listen(16)
            s.settimeout(0.2)
            self.socks.append(s)
            t = threading.Thread(target=self.tcp_loop, args=(s, eid, addr), daemon=True)
            t.start()
            self.threads.append(t)

    def udp_loop(self, sock, eid, addr):
        while not self.stop.is_set():
            try:
                data, peer = sock.recvfrom(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            try:
                off = 12
                qname, off = read_name(data, off)
                qtype = struct.unpack(">H", data[off:off + 2])[0]
            except Exception:
                continue
            self.log(eid, addr, "udp", qname, qtype, data)
            time.sleep(LATENCY_MS.get(eid, 5) / 1000.0)
            try:
                sock.sendto(build_response(data, eid), peer)
            except OSError:
                pass

    def tcp_loop(self, sock, eid, addr):
        while not self.stop.is_set():
            try:
                conn, _peer = sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=self.tcp_conn, args=(conn, eid, addr), daemon=True).start()

    def tcp_conn(self, conn, eid, addr):
        conn.settimeout(2.0)
        try:
            while not self.stop.is_set():
                hdr = recv_exact(conn, 2)
                if hdr is None:
                    return
                (n,) = struct.unpack(">H", hdr)
                data = recv_exact(conn, n)
                if data is None:
                    return
                off = 12
                qname, off = read_name(data, off)
                qtype = struct.unpack(">H", data[off:off + 2])[0]
                self.log(eid, addr, "tcp", qname, qtype, data)
                time.sleep(LATENCY_MS.get(eid, 5) / 1000.0)
                resp = build_response(data, eid)
                conn.sendall(struct.pack(">H", len(resp)) + resp)
        except (OSError, ValueError):
            return
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def stop_all(self):
        self.stop.set()
        for s in self.socks:
            try:
                s.close()
            except OSError:
                pass


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        try:
            chunk = sock.recv(n - len(buf))
        except socket.timeout:
            return None
        if not chunk:
            return None
        buf += chunk
    return buf


# ---------------------------------------------------------------------------
# environment
# ---------------------------------------------------------------------------

OVERLAY_DIRS = ["/etc/aether-proxy", "/var/run/aether-proxy", "/usr/share/aether-proxy"]

LIST_FILES = {
    "/etc/aether-proxy/data/gfwlist.txt": "gfw.example\n",
    "/etc/aether-proxy/data/chnlist.txt": "chn.example\n",
    "/etc/aether-proxy/data/apple.txt": "apple.example\n",
    "/etc/aether-proxy/custom/proxy_domain.txt": "proxy.example\n",
    "/etc/aether-proxy/custom/direct_domain.txt": "direct.example\n",
    "/var/run/aether-proxy/boot.txt": "boot.example\n",
    "/usr/share/aether-proxy/data/gfwlist.txt": "gfw.example\n",
    "/usr/share/aether-proxy/data/chnlist.txt": "chn.example\n",
    "/usr/share/aether-proxy/data/apple.txt": "apple.example\n",
}


def ensure_overlay_dirs():
    created = []
    for d in OVERLAY_DIRS:
        if not os.path.isdir(d):
            os.makedirs(d, exist_ok=True)
            created.append(d)
    return created


def setup_overlay(created):
    for d in OVERLAY_DIRS:
        subprocess.run(["mount", "-t", "tmpfs", "tmpfs", d], check=True)
    for path, content in LIST_FILES.items():
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)


def teardown_overlay(created):
    for d in OVERLAY_DIRS:
        subprocess.run(["umount", d], check=False)
    for d in created:
        try:
            os.rmdir(d)
        except OSError:
            pass


NFT_RULESET = """
delete table inet aetherproxy
table inet aetherproxy {
    set chnroute4 { type ipv4_addr; flags interval; auto-merge; elements = { 10.0.0.0/8 } }
    set gfw4 { type ipv4_addr; flags interval; auto-merge; }
    set uproxy4 { type ipv4_addr; flags interval; auto-merge; }
    set udirect4 { type ipv4_addr; flags interval; auto-merge; }
}
"""


def nft(args, data=None):
    return subprocess.run(["nft"] + args, input=data, text=True, capture_output=True)


def reset_sets():
    r = nft(["-f", "-"], NFT_RULESET)
    if r.returncode != 0:
        raise RuntimeError(f"nft reset failed: {r.stderr}")
    for name in SETS[1:]:
        nft(["flush", "set", "inet", "aetherproxy", name])


def list_set(name):
    r = nft(["-j", "list", "set", "inet", "aetherproxy", name])
    if r.returncode != 0:
        return {"error": r.stderr.strip()}
    data = json.loads(r.stdout)
    elems = []
    for obj in data.get("nftables", []):
        s = obj.get("set")
        if not s:
            continue
        for e in s.get("elem", []):
            if isinstance(e, str):
                elems.append(e)
            elif "range" in e:
                elems.append(f"{e['range'][0]}-{e['range'][1]}")
            elif "prefix" in e:
                p = e["prefix"]
                elems.append(f"{p['addr']}/{p['len']}")
            else:
                elems.append(json.dumps(e))
    return sorted(elems)


# ---------------------------------------------------------------------------
# chinadns-ng control + client
# ---------------------------------------------------------------------------

def start_chinadns(conf):
    p = subprocess.Popen(
        [CHINADNS, "-C", conf],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    deadline = time.time() + 2.0
    while time.time() < deadline:
        if p.poll() is not None:
            out, err = p.communicate(timeout=1)
            return None, (p.returncode, out, err)
        try:
            with socket.create_connection(("127.0.0.1", 31053), timeout=0.1):
                return p, None
        except OSError:
            time.sleep(0.02)
    p.terminate()
    out, err = p.communicate(timeout=2)
    return None, ("timeout waiting for 31053", out, err)


def stop_chinadns(p):
    p.terminate()
    try:
        out, err = p.communicate(timeout=2)
    except subprocess.TimeoutExpired:
        p.kill()
        out, err = p.communicate()
    return out, err


def udp_query(name, qtype, qid=0x4242, edns=False, timeout=2.0):
    q = build_query(name, qtype, qid=qid, edns=edns)
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(timeout)
    s.sendto(q, ("127.0.0.1", 31053))
    try:
        data, _ = s.recvfrom(65535)
    finally:
        s.close()
    msg = parse_message(data)
    msg["wire_hex"] = data.hex()
    return msg


def tcp_query(name, qtype, qid=0x4242, edns=False, timeout=2.0):
    q = build_query(name, qtype, qid=qid, edns=edns)
    s = socket.create_connection(("127.0.0.1", 31053), timeout=timeout)
    try:
        s.sendall(struct.pack(">H", len(q)) + q)
        hdr = recv_exact(s, 2)
        n = struct.unpack(">H", hdr)[0]
        data = recv_exact(s, n)
    finally:
        s.close()
    msg = parse_message(data)
    msg["wire_hex"] = data.hex()
    return msg


def quote(hits):
    return ",".join(f"{h['endpoint']}:{h['proto']}" for h in hits) if hits else "-"


# ---------------------------------------------------------------------------
# coverage
# ---------------------------------------------------------------------------

def fixture_configs():
    out = []
    for name in sorted(os.listdir(FIXTURES)):
        conf = os.path.join(FIXTURES, name, "expected", "chinadns.conf")
        if os.path.isfile(conf):
            out.append((name, conf))
    return out


def parse_conf(path):
    entries = []
    with open(path, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            s = line.strip()
            if not s or s.startswith("#"):
                continue
            key, _, value = s.partition(" ")
            entries.append((key, value.strip(), lineno))
    return entries


def build_coverage():
    report = {"fixtures": {}, "directive_counts": {}, "value_variants": {}}
    found = set()
    for name, conf in fixture_configs():
        entries = parse_conf(conf)
        keys = {}
        for key, value, _ in entries:
            keys.setdefault(key, []).append(value)
            found.add(key)
            report["directive_counts"][key] = report["directive_counts"].get(key, 0) + 1
            if key in ("china-dns", "trust-dns", "group-upstream", "default-tag", "add-taggfw-ip", "filter-qtype"):
                report["value_variants"].setdefault(key, set()).add(value)
        report["fixtures"][name] = {"directives": {k: len(v) for k, v in keys.items()}}
    report["missing_directives"] = [d for d in DIRECTIVES if d not in found]
    report["value_variants"] = {k: sorted(v) for k, v in report["value_variants"].items()}
    report["fixture_count"] = len(report["fixtures"])
    report["directive_counts"] = dict(sorted(report["directive_counts"].items()))
    return report


# ---------------------------------------------------------------------------
# scenarios
# ---------------------------------------------------------------------------

SCENARIOS = [
    ("t14-gfw-t1", [
        ("gfw", "q1.gfw.example", 1, False),
        ("chn-unlisted", "q1.chn.example", 1, False),
        ("other", "q1.other.example", 1, False),
        ("gfw-aaaa", "q2.gfw.example", 28, False),
        ("gfw-https", "q3.gfw.example", 65, False),
        ("gfw-edns", "q4.gfw.example", 1, True),
    ]),
    ("t14-bypass-cn-t1", [
        ("gfw", "q1.gfw.example", 1, False),
        ("chn", "q1.chn.example", 1, False),
        ("group-proxy", "q1.proxy.example", 1, False),
        ("group-direct", "q1.direct.example", 1, False),
        ("group-boot", "q1.boot.example", 1, False),
        ("none-china", "q1.none-china.example", 1, False),
        ("none-noncn", "q1.none-noncn.example", 1, False),
        ("aaaa", "q2.none-china.example", 28, False),
        ("https", "q3.none-china.example", 65, False),
    ]),
    ("t14-global-t1", [
        ("apple", "q1.apple.example", 1, False),
        ("other", "q1.other.example", 1, False),
        ("gfw-aaaa", "q2.other.example", 28, False),
    ]),
    ("t922-mixed", [
        ("mixed", "q1.other.example", 1, False),
    ]),
    ("t922-explicit-tcp", [
        ("tcp-mixed", "q1.other.example", 1, False),
    ]),
    ("t923-wan-t3", [
        ("t3", "q1.other.example", 1, False),
    ]),
    ("t14-no-v6-filter", [
        ("aaaa", "q1.none-china.example", 28, False),
        ("https", "q2.none-china.example", 65, False),
    ]),
    ("t940-degraded-global", [
        ("gfw", "q1.gfw.example", 1, False),
        ("other", "q1.other.example", 1, False),
        ("aaa", "q2.other.example", 1, False),
    ]),
    ("t14-single-trust", [
        ("cache-1", "cache.example", 1, False),
        ("cache-2", "cache.example", 1, False),
        ("cache-3", "cache.example", 1, False),
    ]),
]


def run_scenario(upstream, fixture, queries):
    conf = os.path.join(FIXTURES, fixture, "expected", "chinadns.conf")
    reset_sets()
    p, err = start_chinadns(conf)
    if p is None:
        return {"fixture": fixture, "start_error": err, "queries": []}
    results = []
    try:
        for label, qname, qtype, edns in queries:
            # Flush dynamic sets so each row attributes the write to this query.
            for name in SETS[1:]:
                nft(["flush", "set", "inet", "aetherproxy", name])
            upstream.clear_hits()
            try:
                if label.startswith("tcp-"):
                    resp = tcp_query(qname, qtype, edns=edns)
                else:
                    resp = udp_query(qname, qtype, edns=edns)
                error = None
            except Exception as e:  # noqa: BLE001 - harness must record failures
                resp = None
                error = f"{type(e).__name__}: {e}"
            time.sleep(0.06)
            hits = upstream.snapshot_hits()
            sets = {name: list_set(name) for name in SETS}
            results.append({
                "label": label,
                "qname": qname,
                "qtype": qtype,
                "edns": edns,
                "error": error,
                "reply": resp,
                "reply_hex": resp.pop("wire_hex") if resp else None,
                "upstream_hits": hits,
                "sets": sets,
            })
    finally:
        _out, stderr = stop_chinadns(p)
    return {"fixture": fixture, "start_error": None, "queries": results, "stderr": stderr}


def run_startup():
    results = []
    for name, conf in fixture_configs():
        reset_sets()
        p, err = start_chinadns(conf)
        if p is None:
            results.append({"fixture": name, "ok": False, "error": err})
            continue
        time.sleep(0.3)
        alive = p.poll() is None
        _out, stderr = stop_chinadns(p)
        results.append({"fixture": name, "ok": alive, "stderr": stderr})
    return results


def summarize(results):
    lines = []
    for scenario in results:
        if scenario.get("start_error"):
            lines.append(f"{scenario['fixture']}: START FAILED {scenario['start_error']}")
            continue
        lines.append(f"== {scenario['fixture']}")
        for q in scenario["queries"]:
            r = q["reply"]
            if q["error"]:
                desc = f"ERROR {q['error']}"
            else:
                ans = ",".join(f"{a['value']}" for a in r["answers"] if a["value"]) or "-"
                desc = f"rcode={r['rcode']} an={r['ancount']} ar={r['arcount']} ans={ans}"
            sets = q["sets"]
            written = " ".join(
                f"{name}={sets[name]}" for name in ("gfw4", "uproxy4", "udirect4") if sets[name]
            )
            lines.append(
                f"  {q['label']:<12} {q['qname']:<20} t{q['qtype']:<3} "
                f"hits=[{quote(q['upstream_hits'])}] {desc} {written}"
            )
    return "\n".join(lines)


def sanity_checks(results):
    by_fixture = {s["fixture"]: s for s in results}
    checks = []

    def q(fixture, label):
        for item in by_fixture.get(fixture, {}).get("queries", []):
            if item["label"] == label:
                return item
        return None

    item = q("t14-gfw-t1", "gfw")
    if item:
        eps = {h["endpoint"] for h in item["upstream_hits"]}
        checks.append(("gfw-mode: gfw.example only hits trust", bool(eps) and eps <= {11, 12}))
        checks.append(("gfw-mode: gfw4 got the answer", bool(item["sets"]["gfw4"])))
    item = q("t14-gfw-t1", "chn-unlisted")
    if item:
        eps = {h["endpoint"] for h in item["upstream_hits"]}
        checks.append(("gfw-mode: unlisted only hits china", bool(eps) and eps <= {1, 2, 3}))
    item = q("t14-gfw-t1", "gfw-aaaa")
    if item:
        checks.append(("no-ipv6: AAAA filtered (no upstream hit, NODATA)",
                       not item["upstream_hits"] and item["reply"]["ancount"] == 0))
    item = q("t14-gfw-t1", "gfw-https")
    if item:
        checks.append(("filter-qtype 65: HTTPS filtered (no upstream hit, NODATA)",
                       not item["upstream_hits"] and item["reply"]["ancount"] == 0))
    item = q("t14-gfw-t1", "gfw-edns")
    if item:
        checks.append(("reply strips EDNS OPT (arcount=0)", item["reply"]["arcount"] == 0))
    item = q("t14-bypass-cn-t1", "group-proxy")
    if item:
        eps = {h["endpoint"] for h in item["upstream_hits"]}
        checks.append(("group uproxy uses trust upstream", bool(eps) and eps <= {11, 12}))
    item = q("t14-bypass-cn-t1", "group-direct")
    if item:
        eps = {h["endpoint"] for h in item["upstream_hits"]}
        checks.append(("group udirect uses china upstream", bool(eps) and eps <= {1, 2, 3}))
    item = q("t14-bypass-cn-t1", "none-noncn")
    if item:
        answers = [a["value"] for a in item["reply"]["answers"] if a["value"]]
        checks.append(("tag:none non-China answer falls through to trust",
                       any(a.startswith("10.0.0.1") for a in answers)))
    item = q("t14-bypass-cn-t1", "none-china")
    if item:
        answers = [a["value"] for a in item["reply"]["answers"] if a["value"]]
        checks.append(("tag:none China answer accepted from china",
                       any(a in ("10.0.0.1", "10.0.0.2", "10.0.0.3") for a in answers)))
    item = q("t14-no-v6-filter", "aaaa")
    if item:
        checks.append(("no no-ipv6: AAAA forwarded", bool(item["upstream_hits"]) and item["reply"]["ancount"] > 0))
    item = q("t14-no-v6-filter", "https")
    if item:
        checks.append(("no filter-qtype: HTTPS forwarded", bool(item["upstream_hits"]) and item["reply"]["ancount"] > 0))
    item = q("t14-single-trust", "cache-3")
    if item:
        checks.append(("cache: third identical query does not hit upstream", not item["upstream_hits"]))
    return checks


def raw_edge_cases():
    out = []
    base = build_query("edge.example", 1, qid=0x5001)
    out.append(("normal", base))

    hdr = struct.pack(">HHHHHH", 0x5002, 0x0100, 2, 0, 0, 0)
    out.append(("two-questions", hdr + encode_name("a.example") + struct.pack(">HH", 1, 1)
                + encode_name("b.example") + struct.pack(">HH", 1, 1)))

    out.append(("zero-questions", struct.pack(">HHHHHH", 0x5003, 0x0100, 0, 0, 0, 0)))

    out.append(("trailing-garbage", build_query("garbage.example", 1, qid=0x5004) + b"\xde\xad\xbe\xef"))

    bad = struct.pack(">HHHHHH", 0x5005, 0x0100, 1, 0, 0, 0)
    bad += b"\x40" + b"x" * 64 + b"\x00" + struct.pack(">HH", 1, 1)
    out.append(("label-64", bad))

    out.append(("short-11", b"\x00" * 11))

    tc = bytearray(build_query("tc.example", 1, qid=0x5006))
    tc[2] |= 0x02  # TC in query
    out.append(("query-tc-bit", bytes(tc)))

    out.append(("mixed-case", build_query("WwW.Example.COM", 1, qid=0x5007)))
    return out


def run_probe(upstream):
    conf = os.path.join(FIXTURES, "t14-no-v6-filter", "expected", "chinadns.conf")
    reset_sets()
    p, err = start_chinadns(conf)
    if p is None:
        return {"start_error": err, "probes": []}
    results = []
    try:
        for label, packet in raw_edge_cases():
            upstream.clear_hits()
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.settimeout(0.6)
            reply = None
            try:
                s.sendto(packet, ("127.0.0.1", 31053))
                data, _ = s.recvfrom(65535)
                reply = data.hex()
            except socket.timeout:
                pass
            finally:
                s.close()
            time.sleep(0.05)
            hits = upstream.snapshot_hits()
            results.append({
                "label": label,
                "client_hex": packet.hex(),
                "reply_hex": reply,
                "upstream_hits": hits,
            })
    finally:
        _out, stderr = stop_chinadns(p)
    return {"start_error": None, "probes": results, "stderr": stderr}


def summarize_probe(probe):
    lines = []
    if probe.get("start_error"):
        return f"probe START FAILED {probe['start_error']}"
    for r in probe["probes"]:
        client = bytes.fromhex(r["client_hex"])
        hits = r["upstream_hits"]
        upstream = bytes.fromhex(hits[0]["raw_hex"]) if hits else b""
        # Compare after masking the 2-byte ID (chinadns-ng assigns its own ID).
        same_after_id = (
            len(client) == len(upstream) and client[2:] == upstream[2:]
        ) if hits else False
        lines.append(
            f"  {r['label']:<18} reply={'yes' if r['reply_hex'] else 'NO ':<3} "
            f"upstream={len(hits)} bytes={len(client)} forwarded={'same-after-id' if same_after_id else ('diff' if hits else '-')}"
        )
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("command", choices=["coverage", "startup", "behavior", "probe", "all"])
    ap.add_argument("--out", default=HERE)
    args = ap.parse_args()

    if args.command in ("coverage", "all"):
        cov = build_coverage()
        path = os.path.join(args.out, "coverage.json")
        with open(path, "w") as f:
            json.dump(cov, f, indent=2, sort_keys=True)
        print(f"coverage: {cov['fixture_count']} fixtures, "
              f"{len(cov['directive_counts'])} directives, missing={cov['missing_directives']}")
        print("  counts:", cov["directive_counts"])
        print(f"  wrote {path}")

    if not os.path.exists(CHINADNS):
        print(f"chinadns-ng not found at {CHINADNS}", file=sys.stderr)
        return 2
    if os.geteuid() != 0:
        print("must run as root (nft sets, port 53, netns)", file=sys.stderr)
        return 2

    created = ensure_overlay_dirs()
    setup_overlay(created)
    upstream = FakeUpstream()
    upstream.start()
    try:
        if args.command in ("startup", "all"):
            res = run_startup()
            ok = sum(1 for r in res if r["ok"])
            print(f"startup: {ok}/{len(res)} configs stayed up")
            for r in res:
                if not r["ok"]:
                    print(f"  FAIL {r['fixture']}: {r.get('error') or r.get('stderr')}")
            with open(os.path.join(args.out, "startup.json"), "w") as f:
                json.dump(res, f, indent=2)

        if args.command in ("behavior", "all"):
            results = [run_scenario(upstream, fixture, queries) for fixture, queries in SCENARIOS]
            print(summarize(results))
            checks = sanity_checks(results)
            passed = sum(1 for _, ok in checks if ok)
            print(f"\nsanity: {passed}/{len(checks)}")
            for label, ok in checks:
                print(f"  {'PASS' if ok else 'FAIL'} {label}")
            with open(os.path.join(args.out, "baseline.json"), "w") as f:
                json.dump({"scenarios": results, "sanity": [{"check": c, "ok": o} for c, o in checks]}, f, indent=2)
            print(f"wrote {os.path.join(args.out, 'baseline.json')}")

            # Wire samples: the exact bytes chinadns-ng sent upstream and returned to
            # the client, for the parser spike to re-parse with hickory-proto.
            samples = []
            seen = set()
            for sc in results:
                for q in sc["queries"]:
                    base = f"{sc['fixture']}/{q['label']}"
                    for h in q["upstream_hits"]:
                        if h["raw_hex"] not in seen:
                            seen.add(h["raw_hex"])
                            samples.append({
                                "label": f"{base}/upstream-{h['endpoint']}-{h['proto']}",
                                "direction": "upstream-query",
                                "hex": h["raw_hex"],
                            })
                    if q["reply_hex"] and q["reply_hex"] not in seen:
                        seen.add(q["reply_hex"])
                        samples.append({
                            "label": f"{base}/client-reply",
                            "direction": "client-reply",
                            "hex": q["reply_hex"],
                        })
            with open(os.path.join(args.out, "wire-samples.json"), "w") as f:
                json.dump(samples, f, indent=2)
            print(f"wrote {len(samples)} wire samples")

        if args.command in ("probe", "all"):
            probe = run_probe(upstream)
            print("== edge probes (chinadns-ng)")
            print(summarize_probe(probe))
            with open(os.path.join(args.out, "probe.json"), "w") as f:
                json.dump(probe, f, indent=2)
            # Same wire, labelled for the parser spike.
            edge = []
            for r in probe.get("probes", []):
                edge.append({"label": f"edge/{r['label']}/client-query", "direction": "client-query", "hex": r["client_hex"]})
                for h in r["upstream_hits"]:
                    edge.append({"label": f"edge/{r['label']}/upstream-{h['endpoint']}-{h['proto']}", "direction": "upstream-query", "hex": h["raw_hex"]})
                if r["reply_hex"]:
                    edge.append({"label": f"edge/{r['label']}/client-reply", "direction": "client-reply", "hex": r["reply_hex"]})
            with open(os.path.join(args.out, "edge-wire-samples.json"), "w") as f:
                json.dump(edge, f, indent=2)
            print(f"wrote {len(edge)} edge wire samples")
    finally:
        upstream.stop_all()
        teardown_overlay(created)
    return 0


if __name__ == "__main__":
    sys.exit(main())
