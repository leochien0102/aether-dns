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
import subprocess


def require(ok, message):
    if not ok:
        raise SystemExit(message)


expected = {
    f'aether-dns{separator}1.0.0-r1_{arch}.{ext}'
    for ext, separator in [('ipk', '_'), ('apk', '-')]
    for arch in ['aarch64_generic', 'x86_64']
}
entries = list(pathlib.Path('.').iterdir())
require({p.name for p in entries} == expected | {'SHA256SUMS'},
        'Asset filename set mismatch (exactly four packages + SHA256SUMS required)')
require(all(p.is_file() and not p.is_symlink() for p in entries),
        'Assets must be regular files, not symlinks')
require(all(p.stat().st_size > 0 for p in entries), 'Empty asset file')

lines = pathlib.Path('SHA256SUMS').read_text().splitlines()
require(len(lines) == 4, 'SHA256SUMS must contain exactly four entries')
names = []
for line in lines:
    match = re.fullmatch(r'[0-9a-f]{64}  ([A-Za-z0-9_.-]+)', line)
    require(match is not None, 'Invalid checksum entry')
    names.append(match[1])
require(set(names) == expected and len(set(names)) == 4,
        'Checksum coverage mismatch')
subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], check=True)

for name in sorted(expected):
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
    required = {'Package': 'aether-dns', 'Version': '1.0.0-r1',
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
print('Verified exact four-asset set, checksum manifest and ipk metadata', flush=True)
PY
