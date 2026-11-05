#!/usr/bin/env bash
# M1 forwarding only; not P0 routing/filter/cache parity.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
root=$PWD
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
fixtures=${AETHER_PROXY_FIXTURES:-"$common_dir/../../aether-proxy/tests/fixtures"}
EXPECTED_RA=1

cargo build --bin aether-dns --target-dir "$root/target"
# t14-global-t1 default-tag gfw，六条都走 trust。
# M1 的“全部到第一个 trust”假设已被 M2-04 路由取代，本脚本只做传输 smoke。
exec python3 -B tests/fake_upstream.py \
    "$root/target/debug/aether-dns" \
    "$fixtures/t14-global-t1/expected/chinadns.conf" "$EXPECTED_RA"
