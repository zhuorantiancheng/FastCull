"""Stage an allowlisted public distribution, compile Setup, and hash release assets.

Requires Python 3.11+, a release build, Cargo's downloaded dependency sources,
and Inno Setup 6. Never copies the workspace wholesale or deletes existing output.
"""
import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / 'lrplugins/FastCull.lrplugin'
BINARIES = ['fastcull.exe', 'fastcull-keys.exe', 'fastcull_raw.dll']
PLUGIN_SUPPORT = ['LICENSE', 'THIRD-PARTY-NOTICES.txt']
LUA = ['Info.lua', 'common.lua', 'develop.lua', 'HotkeyInit.lua', 'HotkeyStop.lua',
       'OpenCuller.lua', 'organization.lua', 'PluginInfo.lua', 'sync.lua', 'photo_edits.lua',
       'SyncSelected.lua', 'SyncSidecars.lua']
SOURCE_FILES = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'LICENSE', 'README.md',
                '.gitignore', '.cargo/config.toml', '.github/workflows/windows-release.yml',
                'installer/FastCull.iss', 'docs/DISTRIBUTION.md', 'docs/RELEASE-NOTES.md',
                'docs/QUICKSTART.txt', 'docs/CROPPING.md', 'docs/LR-SYNC.md', 'vendor/LibRaw-0.22.2.zip']
SOURCE_SCRIPTS = ['build-release.ps1', 'build-native.ps1', 'release.py',
                  'register-integrations.ps1', 'register-adobe.cmd', 'unregister-adobe.cmd',
                  'analyze_startup.py', 'verify_feature_images.py', 'native_smoke.py', 'crop_smoke.py',
                  'install-bridge.ps1', 'verify_samples.py']


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def notices(registry, version):
    lock = tomllib.loads((ROOT/'Cargo.lock').read_text(encoding='utf-8'))
    packages = {(p['name'], p['version']) for p in lock['package'] if 'source' in p}
    chunks = [f'FastCull {version} — third-party notices\n'
              'FastCull code: MIT; dependency licenses below apply separately.\n'
              'Cargo.lock also includes dependencies for other platforms.\n']
    found = set()
    for manifest in sorted(registry.glob('*/*/Cargo.toml')):
        package = tomllib.loads(manifest.read_text(encoding='utf-8'))['package']
        key = (package['name'], package['version'])
        if key not in packages or key in found:
            continue
        found.add(key)
        chunks.append(f'\n===== {key[0]} {key[1]} =====\nLicense: {package.get("license", "see source")}\nRepository: {package.get("repository", "")}\n')
        licenses = set()
        for pattern in ['LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*', 'fonts/*.txt']:
            licenses.update(f for f in manifest.parent.glob(pattern) if f.is_file())
        for file in sorted(licenses):
            chunks.append(f'\n--- {file.relative_to(manifest.parent)} ---\n{file.read_text(encoding="utf-8", errors="replace")}\n')
    if len(found) < 50:
        raise RuntimeError('Dependency license sources missing: run cargo fetch --locked or pass --registry.')
    with zipfile.ZipFile(ROOT/'vendor/LibRaw-0.22.2.zip') as sdk:
        chunks.append('\n===== LibRaw 0.22.2 =====\nhttps://www.libraw.org/\n'
                      'Linked under the CDDL 1.0 license option. Complete unmodified upstream source is in vendor/LibRaw-0.22.2.zip.\n'
                      'Adapter source: native/raw_bridge.cpp and native/jpeg_scaled.cpp. Rebuild: scripts/build-native.ps1.\n')
        for name in ['LICENSE.CDDL', 'LICENSE.LGPL', 'COPYRIGHT']:
            chunks.append(sdk.read('LibRaw-0.22.2/'+name).decode('utf-8', errors='replace'))
    return '\n'.join(chunks), len(found)


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Windows PowerShell 5.1 must recognize Chinese strings as UTF-8.
    if source.suffix == '.ps1':
        destination.write_bytes(source.read_text(encoding='utf-8-sig').encode('utf-8-sig'))
    else:
        shutil.copy2(source, destination)


def source_paths():
    paths = [ROOT/p for p in SOURCE_FILES]
    paths += [ROOT/'scripts'/p for p in SOURCE_SCRIPTS]
    paths += [PLUGIN/p for p in LUA]
    for folder, suffixes in [('src', {'.rs'}), ('examples', {'.rs'}),
                             ('tests', {'.rs', '.py', '.cjs', '.jpg'}),
                             ('native', {'.cpp'}), ('bridge', {'.jsx'})]:
        paths += [p for p in (ROOT/folder).rglob('*') if p.is_file() and p.suffix in suffixes]
    return sorted(set(paths))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--registry', type=Path, default=Path(os.environ.get('CARGO_HOME', Path.home()/'.cargo'))/'registry/src')
    parser.add_argument('--iscc', type=Path)
    parser.add_argument('--skip-installer', action='store_true', help='Developer ZIP-only build; no Setup is produced')
    parser.add_argument('--stage-only', action='store_true', help='Prepare payload and print paths, without publishing archives')
    args = parser.parse_args()
    version = tomllib.loads((ROOT/'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    for name in BINARIES:
        if not (PLUGIN/name).is_file():
            raise RuntimeError(f'Build release first: missing {name}')
    info = (PLUGIN/'Info.lua').read_text(encoding='utf-8')
    major, minor, patch = version.split('.')
    if f'major = {major}, minor = {minor}, revision = {patch}' not in info:
        raise RuntimeError('Lightroom plugin version does not match Cargo.toml')
    if f'api.version = "{version}"' not in (ROOT/'bridge/FastCull.jsx').read_text(encoding='utf-8-sig'):
        raise RuntimeError('Bridge version does not match Cargo.toml')
    scratch = ROOT/'.agent-temp'/f'{datetime.now():%Y-%m-%d}-distribution'/uuid.uuid4().hex[:8]
    payload = scratch/'payload'
    output = ROOT/'dist'/f'public-{version}'
    payload.mkdir(parents=True)
    output.mkdir(parents=True, exist_ok=True)
    for name in BINARIES:
        copy(PLUGIN/name, payload/('FastCull.exe' if name == 'fastcull.exe' else name))
    for name in BINARIES+LUA:
        copy(PLUGIN/name, payload/'FastCull.lrplugin'/name)
    for path in ['bridge/FastCull.jsx', 'scripts/register-integrations.ps1', 'scripts/build-native.ps1',
                 'native/raw_bridge.cpp', 'native/jpeg_scaled.cpp', 'vendor/LibRaw-0.22.2.zip', 'LICENSE', 'README.md']:
        copy(ROOT/path, payload/path)
    copy(ROOT/'docs/QUICKSTART.txt', payload/'QUICKSTART.txt')
    copy(ROOT/'LICENSE', payload/'FastCull.lrplugin/LICENSE')
    for name in ['register-adobe.cmd', 'unregister-adobe.cmd']:
        copy(ROOT/'scripts'/name, payload/name)
    notice_text, count = notices(args.registry, version)
    (payload/'THIRD-PARTY-NOTICES.txt').write_text(notice_text, encoding='utf-8')
    (payload/'FastCull.lrplugin/THIRD-PARTY-NOTICES.txt').write_text(notice_text, encoding='utf-8')
    print(json.dumps({'version': version, 'payload': str(payload), 'output': str(output), 'dependency_notices': count}))
    if args.stage_only:
        return
    assets = []
    if not args.skip_installer:
        compiler = args.iscc or shutil.which('ISCC.exe') or Path(os.environ.get('ProgramFiles(x86)', 'C:/Program Files (x86)'))/'Inno Setup 6/ISCC.exe'
        if not Path(compiler).is_file():
            raise RuntimeError('Install Inno Setup 6 or pass --iscc; no installer was produced.')
        subprocess.run([str(compiler), '/Qp', f'/DPayload={payload}', f'/DVersion={version}', f'/O{output}', str(ROOT/'installer/FastCull.iss')], check=True)
        assets.append(output/f'FastCull-Setup-{version}-windows-x64.exe')
    portable = output/f'FastCull-Portable-{version}-windows-x64.zip'
    with zipfile.ZipFile(portable, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in sorted(payload.rglob('*')):
            if path.is_file():
                archive.write(path, path.relative_to(payload).as_posix())
    assets.append(portable)
    source = output/f'FastCull-Source-{version}.zip'
    with zipfile.ZipFile(source, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in source_paths():
            archive.write(path, path.relative_to(ROOT).as_posix())
    assets.append(source)
    (output/'SHA256SUMS.txt').write_text(''.join(f'{sha(p)}  {p.name}\n' for p in assets), encoding='ascii')
    print(f'Release ready: {output}; {len(assets)} artifacts; licenses for {count} dependency sources')


if __name__ == '__main__':
    main()
