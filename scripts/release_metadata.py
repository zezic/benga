#!/usr/bin/env python3
"""Create authenticated updater metadata for the selected release platforms."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tomllib

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat


ROOT = Path(__file__).resolve().parents[1]
UPDATE_ARCHIVES = {
    "macos-aarch64": ".zip",
    "macos-x86_64": ".zip",
    "windows-x86_64": ".zip",
    "linux-x86_64": ".tar.gz",
    "linux-aarch64": ".tar.gz",
}


def signing_key():
    encoded = os.environ.get("BENGA_UPDATE_SIGNING_KEY", "")
    if not encoded:
        raise ValueError("Set BENGA_UPDATE_SIGNING_KEY to the base64-encoded 32-byte Ed25519 seed")
    try:
        seed = base64.b64decode(encoded, validate=True)
        if len(seed) != 32:
            raise ValueError()
    except ValueError:
        raise ValueError("BENGA_UPDATE_SIGNING_KEY must encode exactly 32 bytes") from None
    return Ed25519PrivateKey.from_private_bytes(seed)


def public_key(key):
    return base64.b64encode(key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)).decode("ascii")


def validate_tag(tag):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("Public releases currently require a stable MAJOR.MINOR.PATCH Cargo version")
    if tag != f"v{version}":
        raise ValueError("The pushed vMAJOR.MINOR.PATCH tag must match Cargo.toml exactly")
    return version


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def artifact_metadata(directory, tag, repository, filename):
    path = directory / filename
    size = path.stat().st_size if path.is_file() else 0
    if not size:
        raise ValueError(f"Missing update asset: {filename}")
    return {
        "url": f"https://github.com/{repository}/releases/download/{tag}/{filename}",
        "sha256": sha256(path),
        "size": size,
    }


def write_metadata(directory, tag, repository, expected_public_key, platforms=tuple(UPDATE_ARCHIVES)):
    version = validate_tag(tag)
    key = signing_key()
    if not expected_public_key or public_key(key) != expected_public_key:
        raise ValueError("The signing key must match the public key compiled into this build")
    if not platforms or not set(platforms) <= UPDATE_ARCHIVES.keys():
        raise ValueError("Select at least one known updater platform")
    packages = {}
    for platform in platforms:
        basename = f"Benga-{tag}-{platform}"
        package = artifact_metadata(directory, tag, repository, basename + UPDATE_ARCHIVES[platform])
        if platform == "windows-x86_64":
            package["installer"] = artifact_metadata(directory, tag, repository, basename + "-setup.exe")
        packages[platform] = package
    payload = (json.dumps({"version": version, "tag": tag, "platforms": packages}, indent=2) + "\n").encode("utf-8")
    (directory / "latest.json").write_bytes(payload)
    (directory / "latest.json.sig").write_text(base64.b64encode(key.sign(payload)).decode("ascii") + "\n", encoding="ascii")
    # Include all user-facing installers as well as the authenticated feed.
    files = sorted(path for path in directory.iterdir() if path.is_file() and path.name != "SHA256SUMS")
    (directory / "SHA256SUMS").write_text("".join(f"{sha256(path)}  {path.name}\n" for path in files), encoding="ascii")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("public-key", help="Print only the public key derived from the environment secret")
    validate = commands.add_parser("validate-tag")
    validate.add_argument("tag")
    create = commands.add_parser("create")
    create.add_argument("directory", type=Path)
    create.add_argument("--tag", required=True)
    create.add_argument("--repository", default="zezic/benga")
    create.add_argument("--platforms", nargs="+", choices=UPDATE_ARCHIVES, default=tuple(UPDATE_ARCHIVES))
    args = parser.parse_args()
    try:
        if args.command == "public-key":
            print(public_key(signing_key()))
        elif args.command == "validate-tag":
            print(validate_tag(args.tag))
        else:
            if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
                raise ValueError("Repository must use owner/name syntax")
            write_metadata(
                args.directory, args.tag, args.repository,
                os.environ.get("BENGA_UPDATE_PUBLIC_KEY", ""), args.platforms,
            )
    except (ValueError, OSError) as error:
        print(f"Release metadata: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
