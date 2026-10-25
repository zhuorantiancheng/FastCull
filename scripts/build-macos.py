"""Build a self-contained macOS app on a Mac; no Homebrew runtime dependencies."""
import argparse
import datetime
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
LIBRAW_SHA256 = "f4448371523f9960d26a131ed9dc21bee49f967adbcc4fb6f56b9b47c2ba87c6"


def info_plist(version):
    return dict(CFBundleName="FastCull", CFBundleDisplayName="FastCull",
                CFBundleIdentifier="com.zrtc.fastcull", CFBundleExecutable="fastcull",
                CFBundlePackageType="APPL", CFBundleInfoDictionaryVersion="6.0",
                CFBundleShortVersionString=version, CFBundleVersion=version,
                LSMinimumSystemVersion="11.0", NSHighResolutionCapable=True,
                NSPrincipalClass="NSApplication")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=["arm64", "x86_64"], default=platform.machine())
    parser.add_argument("--native-only", action="store_true", help="仅重建随包提供的 LibRaw 适配层")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("请在 macOS 上运行；Windows 不能生成可验证的 Mac 应用。")
    if args.arch != platform.machine():
        parser.error("请在对应架构的 Mac / CI runner 上构建，以便运行测试。")
    target = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}[args.arch]
    # Unique staging directories avoid mixing old app signatures or plugin files.
    stamp = datetime.datetime.now().strftime("%Y-%m-%d-macos-%H%M%S-%f")
    scratch = ROOT / ".agent-temp" / f"{stamp}-{args.arch}"
    scratch.mkdir(parents=True)
    tmp = scratch / "tmp"
    tmp.mkdir()
    env = os.environ.copy()
    env.update(TMPDIR=str(tmp), TEMP=str(tmp), TMP=str(tmp),
               MACOSX_DEPLOYMENT_TARGET="11.0", CARGO_TARGET_DIR=str(scratch / "target"),
               CARGO_BUILD_TARGET=target, FASTCULL_LOG_DIR=str(scratch / "logs"),
               FASTCULL_SETTINGS_DIR=str(scratch / "settings"))

    def run(command, cwd=ROOT):
        subprocess.run([str(arg) for arg in command], cwd=cwd, env=env, check=True)

    archive = ROOT / "vendor/LibRaw-0.22.2.zip"
    if hashlib.sha256(archive.read_bytes()).hexdigest() != LIBRAW_SHA256:
        raise RuntimeError("LibRaw source hash mismatch")
    with zipfile.ZipFile(archive) as source:
        source.extractall(scratch)
    raw = scratch / "LibRaw-0.22.2"
    for script in ("configure", "version.sh", "shlib-version.sh", "config.guess", "config.sub", "install-sh", "missing", "depcomp", "ltmain.sh"):
        for path in raw.rglob(script):
            path.chmod(path.stat().st_mode | 0o111)
    env.update(CC="clang", CXX="clang++", CFLAGS="-O2 -fPIC", CXXFLAGS="-O2 -fPIC")
    run(["/bin/sh", "./configure", "--disable-shared", "--enable-static", "--disable-openmp",
         "--disable-examples", "--disable-jpeg", "--disable-zlib", "--disable-lcms"], raw)
    # LibRaw names the libtool target with its subdirectory (lib/libraw_r.la);
    # the static archive then lives in lib/.libs.
    run(["make", f"-j{os.cpu_count() or 2}", "lib/libraw_r.la"], raw)
    dylib = scratch / "libfastcull_raw.dylib"
    run(["clang++", "-std=c++17", "-O2", "-dynamiclib", "-fvisibility=hidden",
         "-I", raw, ROOT / "native/raw_bridge.cpp", raw / "lib/.libs/libraw_r.a",
         "-install_name", "@rpath/libfastcull_raw.dylib", "-o", dylib])
    # This build must have no dependency on the build machine's Homebrew paths.
    linkage = subprocess.check_output(["otool", "-L", str(dylib)], text=True)
    for line in linkage.splitlines()[1:]:
        dependency = line.strip().split(" (", 1)[0]
        if not dependency.startswith(("@rpath/", "/usr/lib/", "/System/Library/")):
            raise RuntimeError(f"Unbundled native dependency: {dependency}")
    if args.native_only:
        output = ROOT / "dist" / f"macos-native-{args.arch}"
        output.mkdir(parents=True, exist_ok=True)
        shutil.copy2(dylib, output / dylib.name)
        print(f"Native adapter: {output / dylib.name}")
        return
    env["FASTCULL_RAW_DLL"] = str(dylib)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    run(["cargo", "test", "--locked", "--target", target])
    run(["cargo", "build", "--release", "--locked", "--target", target, "--bin", "fastcull"])
    # The packaged app must locate Contents/Frameworks itself, without the
    # development override used to exercise the native adapter in cargo tests.
    env.pop("FASTCULL_RAW_DLL", None)
    package = scratch / f"FastCull-{version}-macos-{args.arch}"
    app = package / "FastCull.app"
    contents = app / "Contents"
    for name in ("MacOS", "Frameworks", "Resources"):
        (contents / name).mkdir(parents=True, exist_ok=True)
    with (contents / "Info.plist").open("wb") as file:
        plistlib.dump(info_plist(version), file)
    shutil.copy2(scratch / f"target/{target}/release/fastcull", contents / "MacOS/fastcull")
    shutil.copy2(dylib, contents / "Frameworks/libfastcull_raw.dylib")
    plugin = package / "FastCull.lrplugin"
    plugin.mkdir()
    for path in (ROOT / "lrplugins/FastCull.lrplugin").glob("*.lua"):
        shutil.copy2(path, plugin / path.name)
    # The LR plugin has its own app, so moves of the standalone copy do not break it.
    run(["codesign", "--force", "--sign", "-", contents / "Frameworks/libfastcull_raw.dylib"])
    run(["codesign", "--force", "--sign", "-", app])
    run(["codesign", "--verify", "--deep", "--strict", app])
    run([contents / "MacOS/fastcull", "--inspect", ROOT / "tests/fixtures/preview-3x2.jpg"])
    run([sys.executable, ROOT / "tests/macos_raw_gui_smoke.py",
         contents / "MacOS/fastcull", scratch / "raw-gui"])
    shutil.copytree(app, plugin / "FastCull.app")
    for relative in ("bridge/FastCull.jsx", "scripts/install-macos.command", "scripts/build-macos.py",
                     "native/raw_bridge.cpp", "vendor/LibRaw-0.22.2.zip", "docs/MACOS.md",
                     "docs/RAW-EDITING.md", "docs/DNG.md", "docs/raw-window-validation-2026-10-24.json",
                     "docs/raw-edit-validation-2026-10-24.json", "docs/dng-validation-2026-10-23.json", "LICENSE"):
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / relative, destination)
    (package / "scripts/install-macos.command").chmod(0o755)
    # Include licenses for every fetched locked crate, including macOS-only crates.
    registry = Path(env.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src"
    locked = tomllib.loads((ROOT / "Cargo.lock").read_text())
    packages = {(p["name"], p["version"]) for p in locked["package"] if "source" in p}
    notices = [f"FastCull {version}\nLibRaw 0.22.2 is linked under CDDL 1.0. Full source and rebuild script included.\n"]
    count = 0
    for manifest in sorted(registry.glob("*/*/Cargo.toml")):
        pkg = tomllib.loads(manifest.read_text())["package"]
        if (pkg["name"], pkg["version"]) not in packages:
            continue
        count += 1
        notices.append(f"\n===== {pkg['name']} {pkg['version']} =====\n{pkg.get('license', '')}\n")
        for pattern in ("LICENSE*", "LICENCE*", "COPYING*", "NOTICE*", "fonts/*.txt"):
            for file in sorted(manifest.parent.glob(pattern)):
                if file.is_file():
                    notices.append(file.read_text(errors="replace"))
    if count < 50:
        raise RuntimeError("Dependency license registry incomplete")
    for name in ("LICENSE.CDDL", "LICENSE.LGPL", "COPYRIGHT"):
        notices.append((raw / name).read_text())
    (package / "THIRD-PARTY-NOTICES.txt").write_text("\n".join(notices))
    run([sys.executable, ROOT / "tests/macos_native_smoke.py", contents / "Frameworks/libfastcull_raw.dylib"])
    dist = ROOT / "dist" / "macos"
    dist.mkdir(parents=True, exist_ok=True)
    output = dist / f"{package.name}.zip"
    run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", package, output])
    output.with_suffix(".zip.sha256").write_text(f"{hashlib.sha256(output.read_bytes()).hexdigest()}  {output.name}\n")
    print(f"Built: {output}\nScratch: {scratch}\nAd-hoc signed only; not Developer ID signed or notarized.")


if __name__ == "__main__":
    main()
