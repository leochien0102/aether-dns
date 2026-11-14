#!/bin/sh
# Validate an already generated manifest; never silently repair it.
#
# Scope: asset naming, checksum coverage, and ipk control metadata. The apk
# payload is an apk-tools v3 ADB container (magic "ADBd"), not a tar stream, so
# it cannot be inspected here without the SDK's apk tool; the packages job
# installs each apk into a throwaway root with that tool and checks the binary.
set -eu
cd "${1:?usage: verify-assets.sh ASSET_DIRECTORY}"
python3 - <<'PY'
import pathlib
import re
import stat
import subprocess


def require(ok, message):
    if not ok:
        raise SystemExit(message)


# Version-agnostic: the package version comes from openwrt/Makefile, so a release
# must not need edits here. Names are aether-dns_<ver>-r<rel>_<arch>.ipk (opkg) and
# aether-dns-<ver>-r<rel>_<arch>.apk (apk), plus extensionless raw binaries;
# all six assets carry the same version.
NAME_RE = re.compile(r'^aether-dns([_-])([0-9][0-9A-Za-z.+~-]*)_(aarch64_generic|x86_64)\.(ipk|apk)$')
BINARY_RE = re.compile(r'^aether-dns-([0-9][0-9A-Za-z.+~-]*-r[0-9]+)-(aarch64_generic|x86_64)$')
ARCHES = ['aarch64_generic', 'x86_64']
FORMATS = ['ipk', 'apk']

entries = list(pathlib.Path('.').iterdir())
require({p.name for p in entries} >= {'SHA256SUMS'},
        'SHA256SUMS is missing')
assets = [p for p in entries if p.name != 'SHA256SUMS']
require(len(assets) == 6, 'Exactly four packages and two raw binaries are required')
packages = [p for p in assets if p.suffix in ('.ipk', '.apk')]
binaries = [p for p in assets if p not in packages]
require(len(packages) == 4 and len(binaries) == 2,
        'Exactly four packages and two raw binaries are required')
require(all(p.is_file() and not p.is_symlink() for p in entries),
        'Assets must be regular files, not symlinks')
require(all(p.stat().st_size > 0 for p in entries), 'Empty asset file')

parsed = {}
for path in packages:
    match = NAME_RE.fullmatch(path.name)
    require(match is not None, f'Invalid asset filename: {path.name}')
    separator, version, arch, ext = match.groups()
    require(separator == ('_' if ext == 'ipk' else '-'),
            f'Wrong separator for {ext}: {path.name}')
    parsed[(arch, ext)] = version
require(set(parsed) == {(arch, ext) for arch in ARCHES for ext in FORMATS},
        'Asset set mismatch (both formats for both architectures required)')
binary_versions = {}
for path in binaries:
    match = BINARY_RE.fullmatch(path.name)
    require(match is not None, f'Invalid raw binary filename: {path.name}')
    version, arch = match.groups()
    # & binds more tightly than ==; masking with 0o755 means "at least",
    # not exactly 0755. S_IMODE retains extra permission and special bits.
    require(stat.S_IMODE(path.stat().st_mode) == 0o755,
            f'Raw binary must have exactly 0755: {path.name}')
    binary_versions[arch] = version
require(set(binary_versions) == set(ARCHES),
        'Raw binary set mismatch (both architectures required)')
versions = set(parsed.values()) | set(binary_versions.values())
require(len(versions) == 1, f'Inconsistent asset versions: {sorted(versions)}')
version = versions.pop()
require(re.fullmatch(r'[0-9][0-9A-Za-z.+~-]*-r[0-9]+', version) is not None,
        f'Invalid version-release: {version}')

lines = pathlib.Path('SHA256SUMS').read_text().splitlines()
require(len(lines) == 6, 'SHA256SUMS must contain exactly six entries')
names = []
for line in lines:
    match = re.fullmatch(r'[0-9a-f]{64}  ([A-Za-z0-9_.-]+)', line)
    require(match is not None, 'Invalid checksum entry')
    names.append(match[1])
require(set(names) == {p.name for p in assets} and len(set(names)) == 6,
        'Checksum coverage mismatch')
subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], check=True)

for name in sorted({p.name for p in packages}):
    if not name.endswith('.ipk'):
        print(f'=== {name} === (apk metadata verified in the packages job)', flush=True)
        continue
    arch = 'aarch64_generic' if 'aarch64_generic' in name else 'x86_64'
    control_archive = subprocess.check_output(['tar', '-xzOf', name, './control.tar.gz'])
    text = subprocess.check_output(['tar', '-xzO', './control'], input=control_archive).decode()
    fields = {}
    for line in text.splitlines():
        if ':' in line:
            key, value = line.split(':', 1)
            fields.setdefault(key.strip(), []).append(value.strip())
    required = {'Package': 'aether-dns', 'Version': version,
                'Provides': 'dns-engine', 'Architecture': arch,
                'License': 'GPL-3.0-or-later'}
    print(f'=== {name} ===', flush=True)
    for key, value in required.items():
        print(f'{key}: {fields.get(key)}', flush=True)
        require(fields.get(key) == [value], f'{name}: invalid {key}')
    require(not any(re.search(r'\blibc\b', v) for v in fields.get('Depends', [])),
            f'{name}: unexpected libc dependency')
    source = fields.get('Source', [])
    print(f'Source: {source}; Depends: {fields.get("Depends", [])}', flush=True)
    require(len(source) == 1 and bool(source[0]), f'{name}: missing Source')
    # The package directory lives at /feed/<name> inside the SDK container, which is a
    # constant, not a host path. Reject only build-host specific prefixes.
    require(not re.search(r'^(/home/|/workspace/|/tmp/|[A-Za-z]:[\\/])', source[0]),
            f'{name}: host-specific Source path')
print(f'Verified six-asset set ({version}), checksum manifest and ipk metadata', flush=True)
PY
