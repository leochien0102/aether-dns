#!/bin/sh
# Exercise real assets only on disposable copies; never repair the input modes.
set -eu
assets=$(cd "${1:?usage: verify-assets-selftest.sh ASSET_DIRECTORY}" && pwd)
checker=$(cd "$(dirname "$0")" && pwd)/verify-assets.sh
temp=$(mktemp -d)
trap 'rm -rf "$temp"' EXIT
trap 'exit 1' HUP INT TERM

passed=0
failed=0
for scenario in baseline-0755 world-writable-0777 setgid-02755 setuid-04755 no-execute-0644; do
    copy=$(mktemp -d "$temp/assets.XXXXXX")
    cp -Rp "$assets/." "$copy/"
    case "$scenario" in
        baseline-0755) mode= ;;
        world-writable-0777) mode=0777 ;;
        setgid-02755) mode=02755 ;;
        setuid-04755) mode=04755 ;;
        no-execute-0644) mode=0644 ;;
    esac
    # The baseline is deliberately left untouched, including its copied modes.
    if [ -n "$mode" ]; then
        chmod "$mode" "$copy"/aether-dns-*-aarch64_generic
    fi
    status=0
    sh "$checker" "$copy" > "$temp/stdout" 2> "$temp/stderr" || status=$?
    ok=false
    if [ -z "$mode" ]; then
        [ "$status" -eq 0 ] && ok=true
    elif [ "$status" -ne 0 ] && grep -Fq 'must have exactly 0755' "$temp/stderr"; then
        ok=true
    fi
    if "$ok"; then
        printf 'PASS %s (exit %s)\n' "$scenario" "$status"
        passed=$((passed + 1))
    else
        printf 'FAIL %s (exit %s)\n' "$scenario" "$status"
        cat "$temp/stdout" "$temp/stderr"
        failed=$((failed + 1))
    fi
    rm -rf "$copy"
done
printf 'PASS=%s FAIL=%s TOTAL=5\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
