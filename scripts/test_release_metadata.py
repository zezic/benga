import base64
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

import release_metadata as release


class SignedReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.key = Ed25519PrivateKey.generate()
        self.public = release.public_key(self.key)
        seed = base64.b64encode(self.key.private_bytes_raw()).decode("ascii")
        self.environment = patch.dict(os.environ, {"BENGA_UPDATE_SIGNING_KEY": seed})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.version = tomllib.loads((release.ROOT / "Cargo.toml").read_text())["package"]["version"]
        self.tag = f"v{self.version}"
        for platform, extension in release.UPDATE_ARCHIVES.items():
            (self.directory / f"Benga-{self.tag}-{platform}{extension}").write_bytes(platform.encode())

    def create(self, public=None):
        release.write_metadata(self.directory, self.tag, "zezic/benga", public or self.public)

    def test_signature_authenticates_manifest_and_archive_hashes(self):
        self.create()
        payload = (self.directory / "latest.json").read_bytes()
        signature = base64.b64decode((self.directory / "latest.json.sig").read_text())
        self.key.public_key().verify(signature, payload)
        with self.assertRaises(InvalidSignature):
            self.key.public_key().verify(signature, payload.replace(b'"version"', b'"changed"'))
        manifest = json.loads(payload)
        for platform, extension in release.UPDATE_ARCHIVES.items():
            archive = self.directory / f"Benga-{self.tag}-{platform}{extension}"
            entry = manifest["platforms"][platform]
            self.assertEqual(entry["sha256"], release.sha256(archive))
            self.assertEqual(entry["size"], archive.stat().st_size)
            self.assertTrue(entry["url"].endswith(archive.name))
        self.assertIn("latest.json.sig", (self.directory / "SHA256SUMS").read_text())

    def test_incomplete_release_and_unexpected_key_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "match"):
            self.create(release.public_key(Ed25519PrivateKey.generate()))
        next(self.directory.iterdir()).unlink()
        with self.assertRaisesRegex(ValueError, "Missing updater archive"):
            self.create()
        self.assertFalse((self.directory / "latest.json").exists())

    def test_tag_must_match_cargo_version(self):
        with self.assertRaisesRegex(ValueError, "match Cargo.toml"):
            release.validate_tag("v99999.0.0")


if __name__ == "__main__":
    unittest.main()
