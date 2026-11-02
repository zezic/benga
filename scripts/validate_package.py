#!/usr/bin/env python3
"""Validate native packages and run their CLI from outside the source checkout.

This checks packaging and native process startup; interactive GPU/audio behavior
still requires a person on each supported platform.
"""

import argparse
from pathlib import Path
import plistlib
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

from package import PLATFORMS, ROOT


def run(*args, **kwargs):
    print("+", " ".join(str(arg) for arg in args), flush=True)
    return subprocess.run([str(arg) for arg in args], check=True, timeout=60, **kwargs)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_resources(directory):
    expected = {
        "LICENSE": ROOT / "LICENSE",
        "THIRD_PARTY_NOTICES.md": ROOT / "THIRD_PARTY_NOTICES.md",
        "FIRST-RUN.txt": ROOT / "packaging/FIRST-RUN.txt",
        "assets/Gogh-LICENSE": ROOT / "assets/Gogh-LICENSE",
        "assets/licenses/Iosevka-OFL.txt": ROOT / "assets/fonts/Iosevka/OFL.txt",
        "assets/licenses/Philly-Sans.txt": ROOT / "assets/fonts/philly_sans/non_commercial_license.txt",
        "assets/licenses/Models.txt": ROOT / "assets/models/NOTICE.txt",
    }
    for relative, source in expected.items():
        path = directory / relative
        require(path.is_file(), f"Missing package notice: {path}")
        require(path.read_bytes() == source.read_bytes(), f"Outdated package notice: {path}")
    for relative in ("assets/models", "assets/models-spec", "assets/fonts"):
        require(not (directory / relative).exists(), f"Unexpected loose embedded asset: {relative}")


def smoke(executable, version, working_directory):
    require(executable.is_file(), f"Missing executable: {executable}")
    result = run(executable, "--version", cwd=working_directory, capture_output=True, text=True, encoding="utf-8")
    require(result.stdout.strip() == f"Benga {version}", f"Wrong packaged binary version: {result.stdout!r}")
    result = run(executable, "--help", cwd=working_directory, capture_output=True, text=True, encoding="utf-8")
    require("Usage: benga" in result.stdout, "Packaged CLI help did not start correctly")


def check_mac_app(app, version, working_directory):
    contents = app / "Contents"
    metadata = plistlib.loads((contents / "Info.plist").read_bytes())
    require(metadata["CFBundleIdentifier"] == "com.zezic.benga", "Incorrect Mac bundle identifier")
    require(metadata["CFBundleShortVersionString"] == version, "Incorrect Mac bundle version")
    require(metadata["CFBundleVersion"] == version, "Incorrect Mac build version")
    require(metadata["LSMinimumSystemVersion"] == "14.6", "Incorrect Mac deployment target")
    require((contents / "Resources/benga.icns").is_file(), "Missing Mac app icon")
    check_resources(contents / "Resources")
    run("codesign", "--verify", "--deep", "--strict", app)
    smoke(contents / "MacOS/benga", version, working_directory)


def validate_mac(archive, basename, version, temporary):
    extracted = temporary / "zip"
    run("ditto", "-x", "-k", archive, extracted)
    check_mac_app(extracted / "Benga.app", version, temporary)
    image = archive.parent / f"{basename}.dmg"
    require(image.is_file(), f"Missing disk image: {image}")
    run("hdiutil", "verify", image)
    mountpoint = temporary / "dmg"
    mountpoint.mkdir()
    run("hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mountpoint, image)
    try:
        check_mac_app(mountpoint / "Benga.app", version, temporary)
        require((mountpoint / "Applications").is_symlink(), "Missing DMG Applications shortcut")
    finally:
        run("hdiutil", "detach", mountpoint)


def validate_windows(archive, basename, version, temporary):
    with zipfile.ZipFile(archive) as source:
        require(source.testzip() is None, "Portable ZIP checksum failed")
        source.extractall(temporary / "zip")
    portable = temporary / "zip/Benga"
    check_resources(portable)
    require((portable / ".benga-portable").is_file(), "Missing portable update marker")
    smoke(portable / "benga.exe", version, temporary)
    installer = archive.parent / f"{basename}-setup.exe"
    require(installer.is_file(), f"Missing Windows installer: {installer}")
    # Inno Setup records this AppId even with /DIR. Do not replace an existing
    # user's uninstall registration during a local validation run.
    import winreg

    registration = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{29AA6F93-C24A-4A0C-A631-1645D7460F6B}_is1"
    for view in (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY):
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, registration, access=winreg.KEY_READ | view):
                raise ValueError("Validate the installer on a clean Windows account without an existing Benga installation")
        except FileNotFoundError:
            pass
    installed = temporary / "installed"
    run(installer, "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", f"/DIR={installed}")
    try:
        check_resources(installed)
        require(not (installed / ".benga-portable").exists(), "Installer enables portable updates")
        smoke(installed / "benga.exe", version, temporary)
    finally:
        uninstaller = installed / "unins000.exe"
        if uninstaller.is_file():
            run(uninstaller, "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART")


def validate_linux(archive, basename, version, target, temporary):
    with tarfile.open(archive) as source:
        source.extractall(temporary / "tar", filter="data")
    portable = temporary / "tar/Benga"
    check_resources(portable)
    require((portable / ".benga-portable").is_file(), "Missing portable update marker")
    smoke(portable / "benga", version, temporary)
    deb = archive.parent / f"{basename}.deb"
    architecture = "arm64" if target.startswith("aarch64") else "amd64"
    for field, expected in (("Version", version), ("Architecture", architecture)):
        result = run("dpkg-deb", "--field", deb, field, capture_output=True, text=True)
        require(result.stdout.strip() == expected, f"Incorrect DEB {field}")
    unpacked = temporary / "deb"
    run("dpkg-deb", "--extract", deb, unpacked)
    installed = unpacked / "usr/lib/benga"
    check_resources(installed)
    require(not (installed / ".benga-portable").exists(), "DEB enables portable updates")
    require((unpacked / "usr/bin/benga").resolve() == installed / "benga", "Broken DEB command link")
    require((unpacked / "usr/share/applications/benga.desktop").is_file(), "Missing DEB desktop entry")
    require((unpacked / "usr/share/icons/hicolor/256x256/apps/benga.png").is_file(), "Missing DEB icon")
    smoke(unpacked / "usr/bin/benga", version, temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=PLATFORMS, required=True)
    parser.add_argument("--directory", type=Path, default=ROOT / "target/packages")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    basename = f"Benga-v{version}-{PLATFORMS[args.target]}"
    extension = ".tar.gz" if "linux" in args.target else ".zip"
    archive = args.directory.resolve() / f"{basename}{extension}"
    with tempfile.TemporaryDirectory(prefix="benga-package-check-") as directory:
        temporary = Path(directory)
        if "apple" in args.target:
            validate_mac(archive, basename, version, temporary)
        elif "windows" in args.target:
            validate_windows(archive, basename, version, temporary)
        else:
            validate_linux(archive, basename, version, args.target, temporary)
    print(f"Validated {PLATFORMS[args.target]} packages, notices, and CLI startup.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Package validation: {error}", file=sys.stderr)
        sys.exit(1)
