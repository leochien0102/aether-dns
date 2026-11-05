#!/usr/bin/env bash
# M0 behavior baseline, isolated without host root or sudo.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
export PATH="$PATH:/usr/sbin:/sbin"
export PYTHONDONTWRITEBYTECODE=1
for tool in unshare ip nft mount python3 cargo; do
    command -v "$tool" >/dev/null || { echo "STOP: missing $tool" >&2; exit 2; }
done
# Check the required capabilities before building/running anything.
unshare -Urnm -- bash -euc '
    mount --make-rprivate /
    ip link set lo up
    nft add table inet aetherproxy
' || { echo 'STOP: unprivileged mount/netns/nft unavailable' >&2; exit 2; }
cargo build
mkdir -p target/m2-baseline
unshare -Urnm -- bash -euc '
    mount --make-rprivate /
    ip link set lo up
    for addr in 223.5.5.5 119.29.29.29 114.114.114.114 202.96.128.86 202.96.134.133; do
        ip addr add "$addr/32" dev lo
    done
    nft add table inet aetherproxy
    for name in chnroute4 gfw4 uproxy4 udirect4; do
        nft "add set inet aetherproxy $name { type ipv4_addr; flags interval; auto-merge; }"
    done
    # Private tmpfs parents let the read-only harness create its overlay mount
    # points even when the host paths do not exist. No host /etc files change.
    for parent in /etc /var /usr/share; do
        mount -t tmpfs tmpfs "$parent"
    done
    CHINADNS_BIN=target/debug/aether-dns python3 spikes/m0-fixtures/harness.py behavior --out target/m2-baseline
'
python3 tests/compare_baseline.py spikes/m0-fixtures/baseline.json target/m2-baseline/baseline.json
