#!/usr/bin/env python3
"""Local TCP fake upstream and six-query M1 acceptance driver (stdlib only).

Only the fixed, uncompressed questions generated here are understood. This is
not a general DNS parser or the M0 baseline/parity harness.
"""

import contextlib
import os
import queue
import re
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

TIMEOUT = 7
OPT = bytes.fromhex("00002904d0000000000000")
# Fixed answers, independent of client transport, with TTL 60.
RDATA = {
    1: socket.inet_pton(socket.AF_INET, "203.0.113.7"),
    28: socket.inet_pton(socket.AF_INET6, "2001:db8::7"),
    65: bytes.fromhex("000100"),  # priority 1, root target, no parameters
}


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def read_exact(stream, length):
    data = bytearray()
    while len(data) < length:
        part = stream.recv(length - len(data))
        if not part:
            raise EOFError("short TCP DNS frame")
        data.extend(part)
    return bytes(data)


def read_frame(stream):
    return read_exact(stream, struct.unpack("!H", read_exact(stream, 2))[0])


def send_frame(stream, wire):
    stream.sendall(struct.pack("!H", len(wire)) + wire)


def question(index, qtype):
    # Same qname/qtype shape as t14-gfw-t1 in baseline.json, no client EDNS.
    name = f"q{index}.gfw.example"
    return b"".join(bytes([len(label)]) + label.encode() for label in name.split(".")) + b"\0" + struct.pack("!HH", qtype, 1)


def answer_record(qtype):
    data = RDATA[qtype]
    return b"\xc0\x0c" + struct.pack("!HHIH", qtype, 1, 60, len(data)) + data


class FakeUpstream:
    def __init__(self):
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.port = self.listener.getsockname()[1]
        self.hits = queue.Queue()
        self.stop = threading.Event()
        self.worker = threading.Thread(target=self.serve, daemon=True)

    def serve(self):
        while not self.stop.is_set():
            try:
                stream, _ = self.listener.accept()
            except socket.timeout:
                continue
            try:
                with stream:
                    stream.settimeout(TIMEOUT)
                    wire = read_frame(stream)
                    qtype = struct.unpack("!H", wire[-4:-2])[0]
                    index = {1: 1, 28: 2, 65: 3}[qtype]
                    require(wire[2:] == struct.pack("!HHHHH", 0x100, 1, 0, 0, 0) + question(index, qtype), "unexpected upstream query")
                    # Deliberately RA=0 and AR=1: SUT must set RA and strip OPT.
                    response = wire[:2] + struct.pack("!HHHHH", 0x8100, 1, 1, 0, 1) + wire[12:] + answer_record(qtype) + OPT
                    self.hits.put((wire, response))
                    send_frame(stream, response)
            except Exception as error:
                self.hits.put(error)

    def __enter__(self):
        self.worker.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.worker.join(TIMEOUT + 1)
        self.listener.close()


def stop_child(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


@contextlib.contextmanager
def server(binary, source, upstream_port, directory):
    require(re.search(r"(?m)^no-ipv6$", source), "fixture missing no-ipv6")
    require(re.search(r"(?m)^filter-qtype 65$", source), "fixture missing filter-qtype 65")
    # Reserve TCP and UDP together; retry the unavoidable release/start race.
    for attempt in range(10):
        with socket.socket() as tcp, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
            tcp.bind(("127.0.0.1", 0))
            port = tcp.getsockname()[1]
            try:
                udp.bind(("127.0.0.1", port))
            except OSError:
                continue
        config, count = re.subn(r"(?m)^(bind-port )\d+$", rf"\g<1>{port}", source)
        require(count == 1, "expected one bind-port")
        config, count = re.subn(r"(?m)^(trust-dns tcp://127\.0\.0\.1#)\d+", rf"\g<1>{upstream_port}", config)
        require(count == 1, "first trust must be local TCP")
        # Transport smoke needs all six queries to reach the fake upstream;
        # production filtering is covered separately by tests/filters.rs.
        if os.environ.get("AETHER_FORWARD_STRIP_FILTERS") == "1":
            config = re.sub(r"(?m)^(?:no-ipv6|filter-qtype)(?:[ \t]+[^\n]*)?$\n?", "", config)
        # q1/q2/q3 each repeat over UDP then TCP. Keep this transport smoke
        # independent of the shared cache; tests/cache.rs covers cache hits.
        if os.environ.get("AETHER_FORWARD_STRIP_CACHE") == "1":
            config = re.sub(r"(?m)^(?:cache|cache-stale)(?:[ \t]+[^\n]*)?$\n?", "", config)
        path = directory / "chinadns.conf"
        path.write_text(config)
        log = directory / f"server-{attempt}.log"
        with log.open("w") as stderr:
            child = subprocess.Popen([binary, "-C", str(path)], stdout=subprocess.DEVNULL, stderr=stderr)
        try:
            deadline = time.monotonic() + TIMEOUT
            while child.poll() is None and time.monotonic() < deadline:
                if f"listening TCP 127.0.0.1:{port}" in log.read_text():
                    break
                time.sleep(0.01)
            else:
                diagnostic = log.read_text()
                if "Address already in use" in diagnostic:
                    continue
                raise RuntimeError(f"SUT not ready: {diagnostic}")
            yield port
            return
        finally:
            stop_child(child)
    raise RuntimeError("unable to reserve a dynamic TCP/UDP port after 10 attempts")


def exchange(port, transport, wire):
    if transport == "TCP":
        with socket.create_connection(("127.0.0.1", port), TIMEOUT) as stream:
            stream.settimeout(TIMEOUT)
            send_frame(stream, wire)
            return read_frame(stream)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(TIMEOUT)
        udp.connect(("127.0.0.1", port))
        udp.send(wire)
        return udp.recv(65535)


def main():
    binary, fixture, expected_ra = sys.argv[1:]
    expected_ra = int(expected_ra)
    source = Path(fixture).read_text()  # never write to the original fixture
    passed = 0
    with tempfile.TemporaryDirectory(prefix="acceptance-forward-", dir="target") as tmp:
        with FakeUpstream() as upstream, server(binary, source, upstream.port, Path(tmp)) as port:
            for index, (label, qtype) in enumerate((("A", 1), ("AAAA", 28), ("HTTPS", 65)), 1):
                for transport in ("UDP", "TCP"):
                    try:
                        client_id = 0x4242 + (index - 1) * 2 + (transport == "TCP")
                        wire = struct.pack("!HHHHHH", client_id, 0x100, 1, 0, 0, 0) + question(index, qtype)
                        response = exchange(port, transport, wire)
                        hit = upstream.hits.get(timeout=TIMEOUT)
                        if isinstance(hit, Exception):
                            raise hit
                        received, sent = hit
                        require(received[2:] == wire[2:], "first trust query differs")
                        require(upstream.hits.empty(), "more than one first-trust hit")
                        require(len(response) >= 12, "short response")
                        ident, flags, qd, an, ns, ar = struct.unpack("!HHHHHH", response[:12])
                        require(ident == client_id, "client ID not restored")
                        require((flags >> 7) & 1 == expected_ra, f"RA={(flags >> 7) & 1}, expected {expected_ra}")
                        require(ar == 0, f"ar={ar}, expected 0")
                        require((qd, an, ns) == (1, 1, 0), "unexpected section counts")
                        require(response[12:] == sent[12:-len(OPT)], "question/answer differs from fake upstream")
                        require(response[12 + len(question(index, qtype)):] == answer_record(qtype), "fixed answer differs")
                        expected = wire[:2] + struct.pack("!HHHHH", 0x8180, 1, 1, 0, 0) + question(index, qtype) + answer_record(qtype)
                        require(response == expected, "unexpected response bytes")
                        print(f"{label:5} {transport} PASS first-trust=TCP ID=0x{client_id:04x} RA=1 ar=0 answer=match", flush=True)
                        passed += 1
                    except Exception as error:
                        print(f"{label:5} {transport} FAIL {error}", flush=True)
    print(f"{passed}/6 PASS", flush=True)
    return 0 if passed == 6 else 1


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        sys.exit(main())
    except Exception as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
