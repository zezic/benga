# Desktop releases

## Build

Requires Python 3.11+, Rust and the target OS's native tools:

```sh
python3 scripts/package.py
```

Packages go to `target/packages/`. Options: `--target <Rust triple>` selects the
architecture, `--skip-build` uses an existing binary, and `--no-dmg` omits the Mac
disk image. Builds use `cargo build --locked --release`.

| Release key | Packages | Native requirements |
| --- | --- | --- |
| `macos-aarch64` | APP in ZIP, DMG | macOS 14.6+, Xcode command line tools |
| `macos-x86_64` | APP in ZIP, DMG | macOS 14.6+, Xcode command line tools |
| `windows-x86_64` | portable ZIP, per-user setup EXE | MSVC/Windows SDK (`rc.exe`), Inno Setup 6 |
| `linux-x86_64` | tar.gz, DEB | glibc 2.35+, ALSA/PulseAudio/udev development libraries, `dpkg-deb` |
| `linux-aarch64` | tar.gz, DEB | same Linux requirements |

Windows setup installs to `%LOCALAPPDATA%\Programs\Benga`; DEB installs to
`/usr/lib/benga`. Portable users must keep the binary and `assets/` together.
Mac resources live in `Benga.app/Contents/Resources/assets/`; Linux needs a Vulkan driver.

## Signing and CI

`.github/workflows/release.yml` runs **only on `v*` tag pushes**. The stable
`vMAJOR.MINOR.PATCH` tag must match `Cargo.toml`; branch pushes and PRs do not build
packages. All five native jobs must pass before a complete draft GitHub Release
is published. Published releases are never overwritten; interrupted drafts can
be retried. Temporary Actions artifacts expire after three days; downloads use
permanent Release assets. [Standard runners are free for public repositories](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

Before the first tag, generate an Ed25519 key and store its **base64 raw 32-byte
seed** in the Actions secret `BENGA_UPDATE_SIGNING_KEY`. Keep an offline backup;
never commit the seed. A private key-generation environment can be prepared with:

```sh
python3 -m venv target/release-tools
target/release-tools/bin/python -m pip install cryptography==50.0.2
```

Use `Ed25519PrivateKey.generate().private_bytes_raw()` from that library, then
base64-encode the seed directly into your secret manager. CI derives the raw
32-byte public key, base64-encodes it and compiles it as `BENGA_UPDATE_PUBLIC_KEY`.
Install the same public key on the [mirror host](server.md). Local builds without
it run normally but cannot check signed updates. Keep this key stable across
releases; rotation requires a migration trusted by existing clients.

Current Mac packages are ad-hoc signed, not notarized; Windows publisher signing
is unconfigured. Update signatures do not replace OS signing.

## Update contract

Every release contains `latest.json`, `latest.json.sig` and `SHA256SUMS`:

```json
{"version":"0.1.0","tag":"v0.1.0","platforms":{
  "macos-aarch64":{"url":"https://github.com/zezic/benga/releases/download/v0.1.0/Benga-v0.1.0-macos-aarch64.zip","sha256":"<64 hex digits>","size":123456}
}}
```

The manifest includes all five release keys above. Mac/Windows entries use ZIP;
Linux entries use tar.gz. The detached signature is a base64 raw 64-byte Ed25519
signature over the exact JSON bytes, including its final newline. The app and
mirror authenticate the manifest, then check archive size and SHA-256. Preserve
signed bytes and versioned URLs; never rewrite a mirrored manifest.
`SHA256SUMS` also covers installers and feed files.

Startup checks default on; automatic installation defaults off. **F1** opens the
preferences. Self-replacement supports writable Mac bundles and Windows/Linux
archives marked `.benga-portable`; installer-managed copies use their installer
or package manager. Automatic updates install on quit and leave the app closed;
manual updates restart.
A failed swap/restart restores the previous app, retained until successful
startup. There is no crash watchdog; power loss between renames can require
restoring the sibling `.benga-backup-*` directory manually.

## First release checklist

- Audit model redistribution rights and embedded-font provenance. Packages
  include the upstream Iosevka OFL notice; unused noncommercial Philly Sans is excluded.
- Test installed startup, GPU support, audio permission denial/recovery, silence,
  output-device changes, window controls and updates on every supported OS.
- Exercise offline checks, invalid signatures/checksums and read-only installs.
- Decide on Developer ID/notarization and Windows publisher signing.
- Set version and keys, review sources, then push the matching stable tag.
- Obtain website design approval; test the real installer and complete the webroot
  migration before publishing to `/home/benga/web`. Follow [server activation](server.md).

Local Apple Silicon packages, unit tests and the native Mac titlebar probe have
been checked; Intel Mac/Windows/Linux still need native verification. Useful checks:
`cargo test --locked`, `cargo run --example window_chrome_probe`, and
`target/release-tools/bin/python -m unittest discover -s scripts -p test_release_metadata.py`.
