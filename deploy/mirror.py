#!/usr/bin/env python3
"""Mirror signed public Benga Releases; no GitHub token or root access needed."""

import argparse
import base64
import binascii
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import build_opener, HTTPRedirectHandler, Request


REPOSITORY = "zezic/benga"
API_URL = f"https://api.github.com/repos/{REPOSITORY}/releases/latest"
VERSION_PATTERN = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
PLATFORMS = {
    "macos-aarch64": ".zip",
    "macos-x86_64": ".zip",
    "windows-x86_64": ".zip",
    "linux-x86_64": ".tar.gz",
    "linux-aarch64": ".tar.gz",
}
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_TOTAL = 2 * 1024 * 1024 * 1024
MAX_MANIFEST = 64 * 1024
ALLOWED_HOSTS = {
    "api.github.com",
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def require_https(url):
    parsed = urlsplit(url)
    require(
        parsed.scheme == "https"
        and parsed.hostname in ALLOWED_HOSTS
        and parsed.port in (None, 443)
        and not parsed.username
        and not parsed.password,
        "Download left the allowed HTTPS GitHub hosts",
    )


class HTTPSRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        require_https(new_url)
        return super().redirect_request(request, fp, code, message, headers, new_url)


def fetch(url, limit, output=None):
    """Bound both declared and streamed body sizes, including redirected assets."""
    require_https(url)
    request = Request(url, headers={
        "User-Agent": "Benga-release-mirror/1",
        "Accept": "application/vnd.github+json" if url == API_URL else "application/octet-stream",
    })
    with build_opener(HTTPSRedirects()).open(request, timeout=45) as response:
        require_https(response.geturl())
        length = response.headers.get("Content-Length")
        if length is not None:
            require(int(length) <= limit, "Download exceeds its size bound")
        digest = hashlib.sha256()
        contents = bytearray() if output is None else None
        total = 0
        while chunk := response.read(128 * 1024):
            total += len(chunk)
            require(total <= limit, "Streamed download exceeds its size bound")
            digest.update(chunk)
            if output is not None:
                output.write(chunk)
            else:
                contents.extend(chunk)
        return (bytes(contents) if contents is not None else None), digest.hexdigest(), total


def verify_signature(payload, signature_text, public_key_text):
    try:
        key = base64.b64decode(public_key_text.strip(), validate=True)
        signature = base64.b64decode(signature_text.strip(), validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("Update verification key or signature is not valid base64") from error
    require(len(key) == 32 and len(signature) == 64, "Invalid Ed25519 key or signature length")
    # Ed25519 SubjectPublicKeyInfo DER prefix. Only a PUBLIC key is ever stored.
    with tempfile.TemporaryDirectory(prefix="benga-signature-") as temporary:
        directory = Path(temporary)
        (directory / "public.der").write_bytes(bytes.fromhex("302a300506032b6570032100") + key)
        (directory / "manifest").write_bytes(payload)
        (directory / "signature").write_bytes(signature)
        result = subprocess.run([
            "openssl", "pkeyutl", "-verify", "-pubin", "-keyform", "DER",
            "-inkey", str(directory / "public.der"), "-rawin",
            "-in", str(directory / "manifest"), "-sigfile", str(directory / "signature"),
        ], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False, timeout=15)
    require(result.returncode == 0, "The release manifest's Ed25519 signature is invalid")


def parse_manifest(payload, release_tag):
    require(len(payload) <= MAX_MANIFEST, "Manifest is too large")
    manifest = json.loads(payload)
    require(isinstance(manifest, dict), "Manifest must be an object")
    version = manifest.get("version")
    require(isinstance(version, str) and re.fullmatch(VERSION_PATTERN, version), "Invalid stable version")
    require(manifest.get("tag") == release_tag == f"v{version}", "Release tag and manifest disagree")
    platforms = manifest.get("platforms")
    require(isinstance(platforms, dict) and set(platforms) == set(PLATFORMS), "Incomplete platform set")
    archives = []
    for platform, extension in PLATFORMS.items():
        entry = platforms[platform]
        require(isinstance(entry, dict), "Invalid platform entry")
        name = f"Benga-{release_tag}-{platform}{extension}"
        url = f"https://github.com/{REPOSITORY}/releases/download/{release_tag}/{name}"
        require(entry.get("url") == url, "Archive URL does not belong to this release")
        digest = entry.get("sha256")
        require(isinstance(digest, str) and re.fullmatch(r"[a-f0-9]{64}", digest), "Invalid SHA256 digest")
        size = entry.get("size")
        require(type(size) is int and 0 < size <= MAX_ARCHIVE, "Invalid archive size")
        archives.append({"name": name, "url": url, "sha256": digest, "size": size})
    require(sum(archive["size"] for archive in archives) <= MAX_TOTAL, "Release exceeds total mirror bound")
    return manifest, archives


def digest_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(128 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def require_complete(directory, payload, signature, archives):
    require((directory / "latest.json").read_bytes() == payload, "Published version was modified upstream")
    require((directory / "latest.json.sig").read_bytes() == signature, "Published signature changed upstream")
    for archive in archives:
        path = directory / archive["name"]
        require(path.is_file() and path.stat().st_size == archive["size"], "Existing mirror archive has the wrong size")
        require(digest_file(path) == archive["sha256"], "Existing mirror archive failed SHA256 verification")


def mirror(root, public_key):
    try:
        release_payload, _, _ = fetch(API_URL, 1024 * 1024)
    except HTTPError as error:
        if error.code == 404:
            print("No public stable Benga release is available yet.")
            return
        raise
    release = json.loads(release_payload)
    require(isinstance(release, dict), "Invalid GitHub release metadata")
    tag = release.get("tag_name")
    require(isinstance(tag, str) and re.fullmatch("v" + VERSION_PATTERN, tag), "Latest release is not a stable version tag")
    require(release.get("draft") is False and release.get("prerelease") is False, "Latest release must be public and stable")
    assets = release.get("assets")
    require(isinstance(assets, list) and len(assets) <= 50, "Unexpected release asset list")
    asset_map = {}
    for asset in assets:
        require(isinstance(asset, dict) and isinstance(asset.get("name"), str), "Invalid release asset")
        require(asset["name"] not in asset_map, "Duplicate release asset name")
        asset_map[asset["name"]] = asset

    def release_asset(name, limit):
        require(name in asset_map, f"Release is missing {name}")
        expected = f"https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"
        require(asset_map[name].get("browser_download_url") == expected, "Release asset has an unexpected origin")
        require(type(asset_map[name].get("size")) is int and 0 < asset_map[name]["size"] <= limit, "Release asset exceeds its bound")
        return fetch(expected, limit)[0]

    payload = release_asset("latest.json", MAX_MANIFEST)
    signature = release_asset("latest.json.sig", 1024)
    verify_signature(payload, signature, public_key)
    manifest, archives = parse_manifest(payload, tag)
    for archive in archives:
        require(archive["name"] in asset_map, "Signed archive is absent from the public release")
        require(asset_map[archive["name"]].get("size") == archive["size"], "Archive size differs from its signed size")
        require(asset_map[archive["name"]].get("browser_download_url") == archive["url"], "Archive origin differs from the signed URL")

    current = root / "current"
    if current.exists():
        previous = json.loads((current / "latest.json").read_bytes())["version"]
        require(re.fullmatch(VERSION_PATTERN, previous), "Current mirror version is invalid")
        require(tuple(map(int, manifest["version"].split('.'))) >= tuple(map(int, previous.split('.'))), "Refusing to roll the mirror back to an older version")

    final = root / tag
    if final.exists():
        require_complete(final, payload, signature, archives)
    else:
        stage = Path(tempfile.mkdtemp(prefix=".stage-", dir=root))
        try:
            stage.chmod(0o755)
            for archive in archives:
                with (stage / archive["name"]).open("xb") as output:
                    _, digest, size = fetch(archive["url"], archive["size"], output)
                    output.flush()
                    os.fsync(output.fileno())
                require(size == archive["size"] and digest == archive["sha256"], "Downloaded archive failed signed size/SHA256 verification")
            # Preserve the exact signed bytes and their signature. Never rewrite URLs.
            (stage / "latest.json").write_bytes(payload)
            (stage / "latest.json.sig").write_bytes(signature)
            os.replace(stage, final)
        finally:
            if stage.exists():
                shutil.rmtree(stage)

    if current.is_symlink() and os.readlink(current) == tag:
        print(f"Benga {tag} is already mirrored and verified.")
        return
    # One atomic pointer switch publishes a complete version, manifest and signature.
    temporary_link = root / ".current-next"
    temporary_link.unlink(missing_ok=True)
    temporary_link.symlink_to(tag, target_is_directory=True)
    os.replace(temporary_link, current)
    print(f"Mirrored Benga {tag}: {len(archives)} authenticated archives.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("/srv/benga/downloads"))
    parser.add_argument("--public-key-file", type=Path, default=Path("/etc/benga/update-public-key"))
    args = parser.parse_args()
    try:
        require(args.root.is_dir(), "Create the dedicated download directory first")
        public_key = args.public_key_file.read_bytes()
        with (args.root / ".mirror.lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                print("Another Benga mirror process is active.")
                return
            mirror(args.root, public_key)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Benga mirror: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
