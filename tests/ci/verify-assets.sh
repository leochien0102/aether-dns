#!/bin/sh
# Validate an already generated manifest; never silently repair it.
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
    arch = 'aarch64_generic' if 'aarch64_generic' in name else 'x86_64'
    if name.endswith('.ipk'):
        control_archive = subprocess.check_output(['tar', '-xzOf', name, './control.tar.gz'])
        text = subprocess.check_output(['tar', '-xzO', './control'], input=control_archive).decode()
        sep = ':'
        required = {'Package': 'aether-dns', 'Version': '1.0.0-r1',
                    'Provides': 'dns-engine', 'Architecture': arch}
    else:
        # APK v2 consists of concatenated gzip/tar streams (signature/control/data).
        text = subprocess.check_output(
            ['tar', '--ignore-zeros', '-xzOf', name, '.PKGINFO']).decode()
        sep = '='
        required = {'pkgname': 'aether-dns', 'pkgver': '1.0.0-r1',
                    'arch': arch, 'provides': 'dns-engine', 'provider-priority': '100'}
    fields = {}
    for line in text.splitlines():
        if sep in line:
            key, value = line.split(sep, 1)
            fields.setdefault(key.strip(), []).append(value.strip())
    print(f'=== {name} ===', flush=True)
    for key, value in required.items():
        print(f'{key}{sep} {fields.get(key)}', flush=True)
        require(fields.get(key) == [value], f'{name}: invalid {key}')
    if name.endswith('.ipk'):
        require(not any(re.search(r'\blibc\b', v) for v in fields.get('Depends', [])),
                f'{name}: unexpected libc dependency')
        source = fields.get('Source', [])
        print(f'Source: {source}; Depends: {fields.get("Depends", [])}', flush=True)
        require(len(source) == 1 and bool(source[0]), f'{name}: missing Source')
        require(not any(re.search(r'(^|[\s:=])/|[A-Za-z]:[\\/]', v) for v in source),
                f'{name}: absolute Source path')
print('Verified exact four-asset set, metadata and SHA256SUMS', flush=True)
PY
