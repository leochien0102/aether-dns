#!/bin/sh
# Shared local/CI guard: compare source/package versions, optionally a real banner.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
cargo_ver=$(awk '
    /^\[package\]$/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version = "[^"]+"$/ {
        sub(/^version = "/, ""); sub(/"$/, ""); print
    }
' Cargo.toml)
pkg_ver=$(sed -n 's/^PKG_VERSION:=//p' openwrt/Makefile)
pkg_rel=$(sed -n 's/^PKG_RELEASE:=//p' openwrt/Makefile)
if [ -z "$cargo_ver" ] || [ -z "$pkg_ver" ] || [ -z "$pkg_rel" ] ||
    [ "$cargo_ver" != "$pkg_ver" ]; then
    printf 'version mismatch: Cargo=%s PKG_VERSION=%s PKG_RELEASE=%s\n' \
        "$cargo_ver" "$pkg_ver" "$pkg_rel" >&2
    exit 1
fi
if [ "$#" -gt 0 ] && [ "$1" != "aether-dns $cargo_ver" ]; then
    printf 'banner mismatch: actual=%s expected=aether-dns %s\n' "$1" "$cargo_ver" >&2
    exit 1
fi
printf 'version ok: %s-r%s\n' "$cargo_ver" "$pkg_rel"
