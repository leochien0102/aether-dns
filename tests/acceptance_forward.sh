#!/usr/bin/env bash
# M1 forwarding only; not P0 routing/filter/cache parity.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
root=$PWD
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
fixtures=${AETHER_PROXY_FIXTURES:-"$common_dir/../../aether-proxy/tests/fixtures"}
EXPECTED_RA=1

cargo build --bin aether-dns --target-dir "$root/target"
exec python3 -B tests/fake_upstream.py \
    "$root/target/debug/aether-dns" \
    "$fixtures/t14-gfw-t1/expected/chinadns.conf" "$EXPECTED_RA"
