#!/usr/bin/env python3
"""Build Benga and create native release packages using host platform tools."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = {
    "aarch64-apple-darwin": "macos-aarch64",
    "x86_64-apple-darwin": "macos-x86_64",
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
}


def run(*args, **kwargs):
    print("+", " ".join(str(arg) for arg in args), flush=True)
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def copy_resources(destination):
    destination.mkdir(parents=True)
    for name in ("Gogh-LICENSE", "benga-logo.svg", "themes.json"):
        shutil.copy2(ROOT / "assets" / name, destination / name)
    licenses = destination / "licenses"
    licenses.mkdir()
    shutil.copy2(ROOT / "assets/fonts/Iosevka/OFL.txt", licenses / "Iosevka-OFL.txt")
    shutil.copy2(ROOT / "assets/fonts/philly_sans/non_commercial_license.txt", licenses / "Philly-Sans.txt")
    shutil.copy2(ROOT / "assets/models/NOTICE.txt", licenses / "Models.txt")


def write_readme(directory):
    shutil.copy2(ROOT / "LICENSE", directory / "LICENSE")
    shutil.copy2(ROOT / "THIRD_PARTY_NOTICES.md", directory / "THIRD_PARTY_NOTICES.md")
    shutil.copy2(ROOT / "packaging" / "FIRST-RUN.txt", directory / "FIRST-RUN.txt")


def macos(stage, output, binary, basename, version, no_dmg):
    app = stage / "Benga.app"
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    resources = contents / "Resources"
    resources.mkdir()
    shutil.copy2(binary, contents / "MacOS" / "benga")
    (contents / "MacOS" / "benga").chmod(0o755)
    copy_resources(resources / "assets")
    write_readme(resources)
    shutil.copy2(ROOT / "assets/icons/benga.icns", resources / "benga.icns")
    plist = plistlib.loads((ROOT / "assets/Info.plist").read_bytes())
    version_core = version.split("-")[0].split("+")[0]
    plist["CFBundleShortVersionString"] = version_core
    plist["CFBundleVersion"] = version_core
    (contents / "Info.plist").write_bytes(plistlib.dumps(plist))
    (contents / "PkgInfo").write_bytes(b"APPL????")
    # Ad-hoc signing is needed for Apple Silicon. It is not Developer ID signing
    # or notarization, and does not remove Gatekeeper's downloaded-app warning.
    run("codesign", "--force", "--sign", "-", "--timestamp=none", app)
    run("codesign", "--verify", "--deep", "--strict", app)
    run("ditto", "-c", "-k", "--norsrc", "--noextattr", "--keepParent", app, output / f"{basename}.zip")
    if not no_dmg:
        image = stage / "disk-image"
        image.mkdir()
        shutil.copytree(app, image / "Benga.app", symlinks=True)
        (image / "Applications").symlink_to("/Applications", target_is_directory=True)
        write_readme(image)
        run("hdiutil", "create", "-volname", f"Benga {version}", "-srcfolder", image,
            "-ov", "-format", "UDZO", output / f"{basename}.dmg")


def windows(stage, output, binary, basename, version):
    app = stage / "Benga"
    app.mkdir()
    shutil.copy2(binary, app / "benga.exe")
    shutil.copy2(ROOT / "assets/icons/benga.ico", app / "benga.ico")
    copy_resources(app / "assets")
    write_readme(app)
    (app / ".benga-portable").touch()
    with zipfile.ZipFile(output / f"{basename}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(app.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(stage))
    compiler = shutil.which("ISCC") or Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Inno Setup 6/ISCC.exe"
    run(compiler, f"/DAppVersion={version}", f"/DStageDir={app}",
        f"/DOutputDir={output}", f"/DOutputName={basename}-setup", ROOT / "packaging/windows.iss")


def linux(stage, output, binary, basename, version, target):
    app = stage / "Benga"
    app.mkdir()
    shutil.copy2(binary, app / "benga")
    (app / "benga").chmod(0o755)
    copy_resources(app / "assets")
    write_readme(app)
    (app / ".benga-portable").touch()
    shutil.copy2(ROOT / "assets/icons/benga-256.png", app / "benga.png")
    shutil.copy2(ROOT / "packaging/benga.desktop", app / "benga.desktop")
    with tarfile.open(output / f"{basename}.tar.gz", "w:gz") as archive:
        archive.add(app, arcname="Benga")

    package = stage / "deb"
    (package / "DEBIAN").mkdir(parents=True)
    installed = package / "usr/lib/benga"
    shutil.copytree(app, installed)
    (installed / ".benga-portable").unlink()
    bin_dir = package / "usr/bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "benga").symlink_to("../lib/benga/benga")
    desktop = package / "usr/share/applications"
    desktop.mkdir(parents=True)
    shutil.copy2(ROOT / "packaging/benga.desktop", desktop / "benga.desktop")
    icons = package / "usr/share/icons/hicolor/256x256/apps"
    icons.mkdir(parents=True)
    shutil.copy2(ROOT / "assets/icons/benga-256.png", icons / "benga.png")
    docs = package / "usr/share/doc/benga"
    docs.mkdir(parents=True)
    shutil.copy2(ROOT / "LICENSE", docs / "copyright")
    architecture = "arm64" if target.startswith("aarch64") else "amd64"
    size = sum(path.stat().st_size for path in package.rglob("*") if path.is_file()) // 1024
    (package / "DEBIAN/control").write_text(
        "Package: benga\n"
        f"Version: {version}\n"
        "Section: sound\nPriority: optional\n"
        f"Architecture: {architecture}\n"
        "Maintainer: Sergey Ukolov <zezic@zezic.ru>\n"
        f"Installed-Size: {size}\n"
        "Depends: libc6 (>= 2.35), libasound2, libpulse0, libudev1, libx11-6, libxcb1, libxkbcommon0, libwayland-client0, libvulkan1\n"
        "Recommends: mesa-vulkan-drivers\n"
        "Homepage: https://benga.zezic.ru\n"
        "Description: Desktop audio visualizer\n"
        " Animated 3D wireframe models, color themes, and music-driven motion.\n",
        encoding="utf-8",
    )
    run("dpkg-deb", "--root-owner-group", "--build", package, output / f"{basename}.deb")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=PLATFORMS, help="Rust target triple (defaults to native)")
    parser.add_argument("--skip-build", action="store_true", help="Package the existing target release binary")
    parser.add_argument("--no-dmg", action="store_true", help="Skip macOS disk image creation")
    parser.add_argument("--output", type=Path, default=ROOT / "target/packages")
    args = parser.parse_args()
    target = args.target
    if target is None:
        host = subprocess.check_output(["rustc", "-vV"], text=True)
        target = next(line.removeprefix("host: ") for line in host.splitlines() if line.startswith("host: "))
    if target not in PLATFORMS:
        parser.error(f"Unsupported target: {target}")
    if ("apple" in target and sys.platform != "darwin") or ("windows" in target and sys.platform != "win32") or ("linux" in target and not sys.platform.startswith("linux")):
        parser.error("Native package tools require building on the target operating system")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        env = os.environ.copy()
        if "apple" in target:
            env["MACOSX_DEPLOYMENT_TARGET"] = "14.6"
        run("cargo", "build", "--locked", "--release", "--target", target, cwd=ROOT, env=env)
    binary_name = "benga.exe" if "windows" in target else "benga"
    binary = ROOT / "target" / target / "release" / binary_name
    if args.target is None and not binary.exists():
        binary = ROOT / "target/release" / binary_name
    if not binary.is_file():
        parser.error(f"Release binary does not exist: {binary}")
    basename = f"Benga-v{version}-{PLATFORMS[target]}"
    with tempfile.TemporaryDirectory(prefix="benga-package-") as temporary:
        stage = Path(temporary)
        if "apple" in target:
            macos(stage, output, binary, basename, version, args.no_dmg)
        elif "windows" in target:
            windows(stage, output, binary, basename, version)
        else:
            linux(stage, output, binary, basename, version, target)
    print(json.dumps({"version": version, "platform": PLATFORMS[target], "output": str(output)}))


if __name__ == "__main__":
    main()
