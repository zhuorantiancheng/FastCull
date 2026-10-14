"""Package the already-built Windows plugin; include cached dependency license texts."""
import argparse
import os
from pathlib import Path
import tomllib
import zipfile

p = argparse.ArgumentParser()
p.add_argument("--registry", type=Path, default=Path(os.environ.get("CARGO_HOME", Path.home()/".cargo"))/"registry/src")
args = p.parse_args()
root = Path(__file__).resolve().parents[1]
version=tomllib.loads((root/"Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
plugin = root / "lrplugins/FastCull.lrplugin"
assert (plugin / "fastcull.exe").is_file(), "build release first"
lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
packages = {(p["name"], p["version"]) for p in lock["package"] if "source" in p}
notices = [f"FastCull {version} - Third-party notices\n\nRust dependencies are listed from Cargo.lock. "
           "The following installed dependency source packages and their bundled license texts were available during the Windows release build. "
           "Some packages support other platforms as well.\n"]
count = 0
for manifest in sorted(args.registry.glob("*/*/Cargo.toml")):
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
    if (package["name"], package["version"]) not in packages:
        continue
    count += 1
    notices.append(f'\n\n===== {package["name"]} {package["version"]} =====\nLicense: {package.get("license", "see source")}\nRepository: {package.get("repository", "")}\n')
    licenses = set()
    for pattern in ["LICENSE*", "LICENCE*", "COPYING*", "NOTICE*", "fonts/*.txt"]:
        licenses.update(f for f in manifest.parent.glob(pattern) if f.is_file())
    for file in sorted(licenses):
        notices.append(f'\n--- {file.relative_to(manifest.parent)} ---\n{file.read_text(encoding="utf-8", errors="replace")}\n')
assert count > 50, "dependency registry missing; supply --registry"
native_source=root/'vendor/LibRaw-0.22.2.zip'
assert (plugin/'fastcull_raw.dll').is_file(), 'build native RAW adapter first'
with zipfile.ZipFile(native_source) as sdk:
    notices.append('\n\n===== LibRaw 0.22.2 =====\nhttps://www.libraw.org/\nUnmodified upstream source is bundled in vendor/LibRaw-0.22.2.zip.\nThe native adapter links LibRaw under the CDDL 1.0 option; its source and rebuild script are included.\n')
    for license in ['LICENSE.CDDL','LICENSE.LGPL','COPYRIGHT']:
        notices.append('\n'+sdk.read('LibRaw-0.22.2/'+license).decode('utf-8',errors='replace')+'\n')
(plugin / "THIRD-PARTY-NOTICES.txt").write_text("".join(notices), encoding="utf-8")
dist = root / "dist"
dist.mkdir(exist_ok=True)
archive = dist / f"FastCull-Rust-{version}-win64.zip"
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for path in sorted(plugin.iterdir()):
        if path.is_file() and path.suffix.lower() in (".lua", ".exe", ".dll", ".py", ".txt") and not path.name.startswith('fastcull.previous-'):
            z.write(path, "FastCull.lrplugin/" + path.name)
    z.write(root / "README-culler.md", "README-culler.md")
    for path in sorted((root / "bridge").glob("*.jsx")):
        z.write(path, path.relative_to(root).as_posix())
    z.write(root / "scripts/install-bridge.ps1", "scripts/install-bridge.ps1")
    z.write(root / "scripts/build-native.ps1", "scripts/build-native.ps1")
    z.write(root / "native/raw_bridge.cpp", "native/raw_bridge.cpp")
    z.write(root / "native/jpeg_scaled.cpp", "native/jpeg_scaled.cpp")
    z.write(native_source,"vendor/LibRaw-0.22.2.zip")
    for path in sorted((root / "docs").rglob("*")):
        if path.is_file():
            z.write(path, path.relative_to(root).as_posix())
print(f"Packaged {archive.name}: {archive.stat().st_size:,} bytes; notices for {count} dependency sources")

standalone = dist / f"FastCull-Windows-{version}-win64.zip"
with zipfile.ZipFile(standalone, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for name in ['fastcull.exe', 'fastcull_raw.dll', 'THIRD-PARTY-NOTICES.txt']:
        z.write(plugin/name, 'FastCull.exe' if name == 'fastcull.exe' else name)
    z.write(root/'docs/STANDALONE.md', '使用说明.md')
    z.write(root/'scripts/install-standalone.cmd', '安装.cmd')
    for name in ['install-standalone.ps1', 'build-native.ps1']:
        z.writestr('scripts/'+name, (root/'scripts'/name).read_text(encoding='utf-8').encode('utf-8-sig'))
    z.write(root/'native/raw_bridge.cpp', 'native/raw_bridge.cpp')
    z.write(root/'native/jpeg_scaled.cpp', 'native/jpeg_scaled.cpp')
    z.write(native_source, 'vendor/LibRaw-0.22.2.zip')
print(f"Packaged {standalone.name}: {standalone.stat().st_size:,} bytes")
