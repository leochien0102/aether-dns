#!/usr/bin/env bash
# M0 nftset spike evidence run. Creates a throwaway netns, an nft table shaped
# like aether-proxy's, and exercises the Rust netlink prototype end to end.
#
#   ./run-spike.sh            # build + run
#   ./run-spike.sh 2>&1 | tee evidence.log
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN="$HERE/target/release/m0-nftset"
NS=${NS:-m0-nftset}

run() { echo "+ $*"; "$@"; echo; }

cleanup() {
	sudo ip netns del "$NS" 2>/dev/null || true
}
trap cleanup EXIT

if [ ! -x "$BIN" ]; then
	(cd "$HERE" && cargo build --release) || exit 1
fi
command -v nft >/dev/null || {
	echo "nft not installed" >&2
	exit 1
}

cleanup
sudo ip netns add "$NS" || exit 1
sudo ip netns exec "$NS" ip link set lo up

echo "== create aetherproxy table (same shapes as aether-proxy's nft.uc)"
sudo ip netns exec "$NS" nft -f - <<'EOF'
table inet aetherproxy {
	set chnroute4 { type ipv4_addr; flags interval; auto-merge; elements = { 1.0.1.0/24, 1.0.2.0/23, 223.255.252.0/23 } }
	set gfw4 { type ipv4_addr; flags interval; auto-merge; }
	set uproxy4 { type ipv4_addr; flags interval; auto-merge; }
	set udirect4 { type ipv4_addr; flags interval; auto-merge; }
	set eexist4 { type ipv4_addr; flags interval; auto-merge; }
}
EOF

N=(sudo ip netns exec "$NS")

echo
echo "== 1. query members of chnroute4 over netlink (no nft CLI)"
run "${N[@]}" "$BIN" dump inet@aetherproxy@chnroute4

echo "== 2. per-IP membership test (what tag:none needs)"
run "${N[@]}" "$BIN" test inet@aetherproxy@chnroute4 1.0.1.5
run "${N[@]}" "$BIN" test inet@aetherproxy@chnroute4 1.0.4.1

echo "== 3. add a batch to gfw4 (interval begin+end, batch begin/end, one ACK)"
run "${N[@]}" "$BIN" add inet@aetherproxy@gfw4 10.2.0.1 10.2.0.2 10.2.0.3
run "${N[@]}" "$BIN" dump inet@aetherproxy@gfw4

echo "== 3b. nft CLI after chinadns-ng-style consecutive raw adds (known upstream interaction)"
echo "-- nft list still works:"; sudo ip netns exec "$NS" nft list set inet aetherproxy gfw4
echo "-- nft add element fails (same with the real chinadns-ng binary):"
sudo ip netns exec "$NS" nft add element inet aetherproxy gfw4 '{ 8.8.8.8 }'; echo "nft add rc=$?"
echo "-- direct netlink add still works:"
run "${N[@]}" "$BIN" add inet@aetherproxy@gfw4 10.2.0.4

echo "== 4. exact duplicate add is idempotent (no NLM_F_EXCL)"
run "${N[@]}" "$BIN" add inet@aetherproxy@gfw4 10.2.0.1

echo "== 5. adding an IP inside an existing range fails EEXIST (whole batch aborts)"
sudo ip netns exec "$NS" nft add element inet aetherproxy eexist4 '{ 1.2.3.0/24 }'
run "${N[@]}" "$BIN" add inet@aetherproxy@eexist4 1.2.3.88 10.8.8.8
run "${N[@]}" "$BIN" dump inet@aetherproxy@eexist4

echo "== 6. chinadns-ng-style test-then-add avoids EEXIST"
run "${N[@]}" "$BIN" add-safe inet@aetherproxy@eexist4 1.2.3.99 10.9.9.9
run "${N[@]}" "$BIN" dump inet@aetherproxy@eexist4

echo "== 7. benchmark (one process, one socket, no shell-outs)"
run "${N[@]}" "$BIN" bench-test inet@aetherproxy@chnroute4 1.0.1.5 1000
run "${N[@]}" "$BIN" bench-add inet@aetherproxy@uproxy4 500

echo "== 8. failure modes"
echo "-- missing set"
run "${N[@]}" "$BIN" add inet@aetherproxy@nope 10.1.1.1
echo "-- no CAP_NET_ADMIN"
run "${N[@]}" capsh --drop=cap_net_admin -- -c "$BIN test inet@aetherproxy@chnroute4 1.0.1.5"
echo "-- table deleted under us, then recreated (aether-proxy reload)"
sudo ip netns exec "$NS" nft 'add table inet reloadtest'
sudo ip netns exec "$NS" nft 'add set inet reloadtest s4 { type ipv4_addr; flags interval; auto-merge; }'
run "${N[@]}" "$BIN" add inet@reloadtest@s4 192.0.2.10
sudo ip netns exec "$NS" nft delete table inet reloadtest
run "${N[@]}" "$BIN" add inet@reloadtest@s4 192.0.2.10
sudo ip netns exec "$NS" nft 'add table inet reloadtest'
sudo ip netns exec "$NS" nft 'add set inet reloadtest s4 { type ipv4_addr; flags interval; auto-merge; }'
run "${N[@]}" "$BIN" add inet@reloadtest@s4 192.0.2.10

echo "== 9. IPv6 set path (P1, same code)"
sudo ip netns exec "$NS" nft 'add table inet aether6'
sudo ip netns exec "$NS" nft 'add set inet aether6 v6 { type ipv6_addr; flags interval; auto-merge; }'
run "${N[@]}" "$BIN" add inet@aether6@v6 2001:db8::1 2001:db8::2
run "${N[@]}" "$BIN" test inet@aether6@v6 2001:db8::1
run "${N[@]}" "$BIN" test inet@aether6@v6 2001:db8::3
run "${N[@]}" "$BIN" dump inet@aether6@v6

echo "== 10. no libnftnl dependency"
ldd "$BIN"

echo "DONE"
