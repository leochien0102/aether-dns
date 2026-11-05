#!/usr/bin/env python3
"""Strict M0 semantic diff; only DNS IDs, compression and hit order are masked.

No known-gap allowlist: any observed difference fails. TTLs, flags, hit
multiplicity, endpoints/protocols and all nft set elements remain significant.
"""
import argparse
import importlib.util
import ipaddress
import json
from pathlib import Path
import struct
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("m0_harness", ROOT / "spikes/m0-fixtures/harness.py")
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)


def wire(hex_string):
    """Decode fixture wire fields, not bytes/pointers; retain non-name RDATA."""
    data = bytes.fromhex(hex_string)
    _, flags, qd, an, ns, ar = struct.unpack_from(">6H", data)
    offset = 12
    questions = []
    for _ in range(qd):
        name, offset = harness.read_name(data, offset)
        qtype, qclass = struct.unpack_from(">2H", data, offset)
        offset += 4
        questions.append([name, qtype, qclass])
    sections = []
    for count in (an, ns, ar):
        records = []
        for _ in range(count):
            owner, offset = harness.read_name(data, offset)
            rtype, rclass, ttl, length = struct.unpack_from(">HHIH", data, offset)
            offset += 10
            end = offset + length
            if end > len(data):
                raise ValueError("truncated RDATA")
            raw = data[offset:end]
            if rtype in (1, 28):
                value = str(ipaddress.ip_address(raw))
            elif rtype in (2, 5, 12):
                value, consumed = harness.read_name(data, offset)
                if consumed != end:
                    raise ValueError("invalid name RDATA")
            elif rtype in (64, 65):
                priority = struct.unpack_from(">H", data, offset)[0]
                target, consumed = harness.read_name(data, offset + 2)
                if consumed > end:
                    raise ValueError("invalid SVCB/HTTPS target")
                value = [priority, target, data[consumed:end].hex()]
            else:
                value = raw.hex()
            records.append([owner, rtype, rclass, ttl, value])
            offset = end
        sections.append(records)
    if offset != len(data):
        raise ValueError("trailing wire bytes")
    return {"flags": flags, "questions": questions, "sections": sections}


def normalize_query(query):
    result = {key: query[key] for key in ("qname", "qtype", "edns", "error", "sets")}
    result["sets"] = {name: sorted(elements) for name, elements in query["sets"].items()}
    reply = query["reply"]
    result["reply"] = None if reply is None else {k: v for k, v in reply.items() if k != "id"}
    result["reply_wire"] = wire(query["reply_hex"]) if query["reply_hex"] else None
    hits = []
    for hit in query["upstream_hits"]:
        hits.append({**{k: hit[k] for k in ("endpoint", "addr", "proto", "qname", "qtype")},
                     "wire": wire(hit["raw_hex"])})
    # A multiset, not a set: duplicate sends must remain visible.
    result["upstream_hits"] = sorted(hits, key=lambda h: json.dumps(h, sort_keys=True))
    return result


def differences(expected, actual, path):
    if type(expected) is not type(actual):
        return [f"{path}: baseline={expected!r} candidate={actual!r}"]
    if isinstance(expected, dict):
        out = []
        for key in sorted(expected.keys() | actual.keys()):
            if key not in expected or key not in actual:
                out.append(f"{path}.{key}: baseline={expected.get(key, '<missing>')!r} candidate={actual.get(key, '<missing>')!r}")
            else:
                out.extend(differences(expected[key], actual[key], f"{path}.{key}"))
        return out
    if isinstance(expected, list):
        out = []
        if len(expected) != len(actual):
            out.append(f"{path}.length: baseline={len(expected)} candidate={len(actual)}")
        for index, (left, right) in enumerate(zip(expected, actual)):
            out.extend(differences(left, right, f"{path}[{index}]"))
        return out
    return [] if expected == actual else [f"{path}: baseline={expected!r} candidate={actual!r}"]


def index(items, key):
    result = {}
    for item in items:
        name = item[key]
        if name in result:
            raise ValueError(f"duplicate {key}: {name}")
        result[name] = item
    return result


def compare(baseline, candidate):
    diffs = []
    expected = index(baseline["scenarios"], "fixture")
    actual = index(candidate["scenarios"], "fixture")
    planned = dict(harness.SCENARIOS)
    diffs.extend(differences(sorted(planned), sorted(expected), "baseline.fixtures"))
    diffs.extend(differences(sorted(expected), sorted(actual), "candidate.fixtures"))
    passed = 0
    for fixture, queries in planned.items():
        left = expected.get(fixture, {})
        right = actual.get(fixture, {})
        for side, scenario in (("baseline", left), ("candidate", right)):
            if scenario.get("start_error") is not None:
                diffs.append(f"{fixture}.{side}.start_error: {scenario['start_error']!r}")
        lq = index(left.get("queries", []), "label")
        rq = index(right.get("queries", []), "label")
        labels = sorted(q[0] for q in queries)
        diffs.extend(differences(labels, sorted(lq), f"{fixture}.baseline.labels"))
        diffs.extend(differences(labels, sorted(rq), f"{fixture}.candidate.labels"))
        for label in labels:
            path = f"{fixture}/{label}"
            if label not in lq or label not in rq:
                diffs.append(f"{path}: missing query")
                continue
            query_diffs = differences(normalize_query(lq[label]), normalize_query(rq[label]), path)
            for side, query in (("baseline", lq[label]), ("candidate", rq[label])):
                if query["error"] is not None or query["reply"] is None:
                    query_diffs.append(f"{path}.{side}: query failed")
            diffs.extend(query_diffs)
            passed += not query_diffs
    ls = index(baseline["sanity"], "check")
    rs = index(candidate["sanity"], "check")
    diffs.extend(differences(sorted(ls), sorted(rs), "sanity.checks"))
    if len(ls) != 13:
        diffs.append(f"baseline.sanity.length: expected=13 actual={len(ls)}")
    # Recompute sanity so a candidate cannot pass by carrying stale ok values.
    computed = dict(harness.sanity_checks(candidate["scenarios"]))
    sanity_passed = 0
    for name in ls:
        ok = ls[name]["ok"] is True and rs.get(name, {}).get("ok") is True and computed.get(name) is True
        if not ok:
            diffs.append(f"sanity/{name}: baseline={ls[name]['ok']!r} candidate={rs.get(name)!r} recomputed={computed.get(name)!r}")
        sanity_passed += ok
    for diff in diffs:
        print(f"DIFF {diff}")
    print(f"{passed}/29 queries")
    print(f"{sanity_passed}/13 sanity")
    print(f"diff={len(diffs)}")
    return 1 if diffs else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        return compare(json.loads(args.baseline.read_text()), json.loads(args.candidate.read_text()))
    except (OSError, ValueError, KeyError, TypeError, struct.error, IndexError) as error:
        print(f"DIFF invalid baseline/candidate: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
