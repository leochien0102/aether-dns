#!/usr/bin/env bash
# Run the M0 fixture harness in an isolated netns + mount ns.
# Usage: ./run-harness.sh coverage|startup|behavior|all
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
NETNS=${NETNS:-aether-m0}
export CHINADNS_BIN=${CHINADNS_BIN:-/tmp/ci-tools/chinadns-ng-2025.08.09/chinadns-ng}

if [ ! -x "$CHINADNS_BIN" ]; then
	echo "chinadns-ng not found at $CHINADNS_BIN" >&2
	exit 2
fi

if ! ip netns list | grep -qx "$NETNS"; then
	sudo ip netns add "$NETNS"
fi
sudo ip netns exec "$NETNS" ip link set lo up
for ip in 223.5.5.5 119.29.29.29 114.114.114.114 202.96.128.86 202.96.134.133; do
	sudo ip netns exec "$NETNS" ip addr add "$ip/32" dev lo 2>/dev/null || true
done

exec sudo --preserve-env=CHINADNS_BIN ip netns exec "$NETNS" unshare -m -- python3 "$HERE/harness.py" "$@"
