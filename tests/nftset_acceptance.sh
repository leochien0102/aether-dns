#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Debian installs nft in sbin, which need not be in an unprivileged PATH.
export PATH="$PATH:/usr/sbin:/sbin"
unshare -Urn -- bash -euo pipefail -c '
    nft -f - <<EOF
add table inet m2nft
add set inet m2nft addresses { type ipv4_addr; flags interval; auto-merge; elements = { 1.2.3.0/24 }; }
EOF
    NFT_TEST=1 cargo test --test nftset -- --nocapture
    # A child user namespace has no capabilities over its parent network namespace.
    unshare -Ur -- env NFT_TEST=1 NFT_EPERM_TEST=1 cargo test --test nftset -- --nocapture
'
