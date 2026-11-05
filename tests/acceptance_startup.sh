#!/usr/bin/env bash
# M1 startup only: no queries, list files, nft sets, or network namespaces.
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
root=$PWD
# Resolve the sibling repository in both the main checkout and task worktrees.
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
fixtures=${AETHER_PROXY_FIXTURES:-"$common_dir/../../aether-proxy/tests/fixtures"}

if (( EUID == 0 )); then
    printf 'ERROR: run startup acceptance as a non-root user\n' >&2
    exit 1
fi

shopt -s nullglob
configs=("$fixtures"/*/expected/chinadns.conf)
if (( ${#configs[@]} != 22 )); then
    printf 'ERROR: expected 22 configs, found %s in %s\n' "${#configs[@]}" "$fixtures" >&2
    printf '0/22 configs stayed up\n'
    exit 1
fi

cargo build --bin aether-dns --target-dir "$root/target"
tmp=$(mktemp -d "$root/target/acceptance-startup.XXXXXX")
pid=
cleanup() {
    if [[ -n $pid ]]; then
        kill -TERM "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    rm -rf -- "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

passed=0
for conf in "${configs[@]}"; do
    name=${conf%/expected/chinadns.conf}
    name=${name##*/}
    "$root/target/debug/aether-dns" -C "$conf" >"$tmp/stdout" 2>"$tmp/stderr" &
    pid=$!
    sleep 0.3
    if kill -0 "$pid" 2>/dev/null; then
        printf 'PASS %s\n' "$name"
        passed=$((passed + 1))
        kill -TERM "$pid" 2>/dev/null || true
    else
        printf 'FAIL %s\n' "$name"
        printf '%s stderr:\n' "$name" >&2
        while IFS= read -r line || [[ -n $line ]]; do
            printf '%s\n' "$line" >&2
        done <"$tmp/stderr"
    fi
    # SIGTERM may yield status 143 before T1.7; startup is the only criterion.
    wait "$pid" 2>/dev/null || true
    pid=
done

printf '%s/22 configs stayed up\n' "$passed"
(( passed == 22 ))
