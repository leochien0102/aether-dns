#!/bin/sh
# Shared local/CI guard: verify an ELF binary has readable section headers
# and does not retain a static symbol table (.symtab / SHT_SYMTAB).
set -eu

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    printf 'usage: check-stripped.sh ELF_PATH\n' >&2
    exit 2
fi

elf=$1
if [ ! -f "$elf" ] || [ ! -r "$elf" ]; then
    printf 'strip check failed: cannot read ELF file: %s\n' "$elf" >&2
    exit 1
fi

if ! headers=$(LC_ALL=C readelf -SW "$elf"); then
    printf 'strip check failed: readelf -SW failed for %s\n' "$elf" >&2
    exit 1
fi

status=0
printf '%s\n' "$headers" | awk '
    /^Section Headers:/ { in_table = 1; next }
    in_table && /^[[:space:]]*\[[[:space:]]*[0-9]+\]/ {
        rest = $0
        sub(/^[[:space:]]*\[[[:space:]]*[0-9]+\][[:space:]]*/, "", rest)
        n = split(rest, f, /[[:space:]]+/)
        if (n >= 2 && f[1] != "NULL") {
            sections++
            if (f[1] == ".symtab" || f[2] == "SYMTAB") {
                has_symtab = 1
            }
        }
    }
    END {
        if (!in_table || sections == 0) {
            exit 3
        }
        if (has_symtab) {
            exit 4
        }
        exit 0
    }
' || status=$?

case "$status" in
    0)
        printf 'strip check ok: %s (section headers readable, no .symtab)\n' "$elf"
        ;;
    3)
        printf 'strip check failed: no readable ELF section headers in %s\n' "$elf" >&2
        exit 1
        ;;
    4)
        printf 'strip check failed: static symbol table (.symtab) still present in %s\n' "$elf" >&2
        exit 1
        ;;
    *)
        printf 'strip check failed: unexpected parser exit %s for %s\n' "$status" "$elf" >&2
        exit 1
        ;;
esac
