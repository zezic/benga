"""Release-boundary checks using throwaway Ed25519 keys and no network."""

import base64
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import mirror


class MirrorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="benga-mirror-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        subprocess.run([
            "openssl", "genpkey", "-algorithm", "ED25519", "-out", str(self.directory / "test-key.pem"),
        ], check=True, capture_output=True)
        public = subprocess.check_output([
            "openssl", "pkey", "-in", str(self.directory / "test-key.pem"), "-pubout", "-outform", "DER",
        ])
        self.public = base64.b64encode(public[-32:])

    def signed_release(self, version="1.2.3"):
        tag = "v" + version
        downloads = {}
        platforms = {}
        for platform, extension in mirror.PLATFORMS.items():
            name = f"Benga-{tag}-{platform}{extension}"
            url = f"https://github.com/zezic/benga/releases/download/{tag}/{name}"
            content = ("test archive: " + platform).encode()
            downloads[url] = content
            platforms[platform] = {"url": url, "size": len(content), "sha256": hashlib.sha256(content).hexdigest()}
        payload = json.dumps({"version": version, "tag": tag, "platforms": platforms}).encode()
        signature = self.sign(payload)
        for name, content in [("latest.json", payload), ("latest.json.sig", signature)]:
            downloads[f"https://github.com/zezic/benga/releases/download/{tag}/{name}"] = content
        release = {
            "tag_name": tag, "draft": False, "prerelease": False,
            "assets": [{"name": url.rsplit('/', 1)[1], "size": len(content), "browser_download_url": url}
                       for url, content in downloads.items()],
        }
        downloads[mirror.API_URL] = json.dumps(release).encode()
        return payload, signature, downloads

    def sign(self, payload):
        (self.directory / "payload").write_bytes(payload)
        subprocess.run([
            "openssl", "pkeyutl", "-sign", "-inkey", str(self.directory / "test-key.pem"),
            "-rawin", "-in", str(self.directory / "payload"), "-out", str(self.directory / "signature"),
        ], check=True, capture_output=True)
        return base64.b64encode((self.directory / "signature").read_bytes()) + b"\n"

    @staticmethod
    def fetcher(downloads):
        def fetch(url, limit, output=None):
            data = downloads[url]
            mirror.require(len(data) <= limit, "oversize")
            if output is not None:
                output.write(data)
            return (data if output is None else None), hashlib.sha256(data).hexdigest(), len(data)
        return fetch

    def test_signature_authenticates_exact_bytes(self):
        payload, signature, _ = self.signed_release()
        mirror.verify_signature(payload, signature, self.public)
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            mirror.verify_signature(payload + b" ", signature, self.public)

    def test_manifest_rejects_foreign_urls_and_unbounded_sizes(self):
        payload, _, _ = self.signed_release()
        manifest = json.loads(payload)
        entry = manifest["platforms"]["macos-aarch64"]
        original_url = entry["url"]
        entry["url"] = original_url.replace("zezic/benga", "stranger/benga")
        with self.assertRaisesRegex(ValueError, "does not belong"):
            mirror.parse_manifest(json.dumps(manifest).encode(), "v1.2.3")
        entry["url"] = original_url
        entry["size"] = mirror.MAX_ARCHIVE + 1
        with self.assertRaisesRegex(ValueError, "archive size"):
            mirror.parse_manifest(json.dumps(manifest).encode(), "v1.2.3")

    def test_redirect_must_remain_on_https_github_hosts(self):
        for url in ["http://github.com/zezic/benga", "https://evil.example/file", "https://github.com:444/file", "https://user@github.com/file"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                mirror.require_https(url)

    def test_atomic_mirror_retains_signed_bytes_and_verifies_files(self):
        payload, signature, downloads = self.signed_release()
        root = self.directory / "downloads"
        root.mkdir()
        with patch.object(mirror, "fetch", self.fetcher(downloads)):
            mirror.mirror(root, self.public)
            mirror.mirror(root, self.public)
        self.assertEqual((root / "current").readlink(), Path("v1.2.3"))
        self.assertEqual((root / "current/latest.json").read_bytes(), payload)
        self.assertEqual((root / "current/latest.json.sig").read_bytes(), signature)

    def test_failed_download_never_publishes_partial_version(self):
        _, _, downloads = self.signed_release()
        archive_url = next(url for url in downloads if url.endswith(".zip"))
        downloads[archive_url] = b"corrupt"
        root = self.directory / "downloads"
        root.mkdir()
        with patch.object(mirror, "fetch", self.fetcher(downloads)):
            with self.assertRaisesRegex(ValueError, "size/SHA256"):
                mirror.mirror(root, self.public)
        self.assertFalse((root / "current").exists())
        self.assertFalse((root / "v1.2.3").exists())
        self.assertEqual(list(root.iterdir()), [])

    def test_older_release_cannot_replace_current_mirror(self):
        _, _, newer = self.signed_release("2.0.0")
        _, _, older = self.signed_release("1.9.9")
        root = self.directory / "downloads"
        root.mkdir()
        with patch.object(mirror, "fetch", self.fetcher(newer)):
            mirror.mirror(root, self.public)
        with patch.object(mirror, "fetch", self.fetcher(older)):
            with self.assertRaisesRegex(ValueError, "roll the mirror back"):
                mirror.mirror(root, self.public)
        self.assertEqual((root / "current").readlink(), Path("v2.0.0"))


if __name__ == "__main__":
    unittest.main()
