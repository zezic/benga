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
`/usr/lib/benga`. Models, catalogue, fonts, themes and logo are embedded in the
binary. Portable users should retain `.benga-portable` to enable self-updates.
Packages also carry license notices and first-run guidance. Linux needs a Vulkan driver.

## Signing and CI

`.github/workflows/release.yml` runs **only on `v*` tag pushes**. The stable
`vMAJOR.MINOR.PATCH` tag must match `Cargo.toml`; branch pushes and PRs do not build
packages. A tag such as `v0.1.0-check.1` validates the current Cargo version on
all five native runners without creating a release. The stable `v0.1.0` tag
stages a complete signed **draft** only after all jobs pass. Review the packages
and publish the draft manually. Published releases are never overwritten;
interrupted drafts can be retried. Temporary Actions artifacts expire after three days; downloads use
permanent Release assets. [Standard runners are free for public repositories](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

Native validation checks archive contents and license notices, launches the CLI
from outside the checkout, verifies the Mac ZIP/DMG signatures, exercises the
Windows installer/uninstaller, and checks the Linux portable archive and DEB.
Run it locally after packaging:

```sh
python3 scripts/validate_package.py --target aarch64-apple-darwin
```

The update key was configured on 2026-11-02. Its public half is
`deploy/update-public-key`; the private seed is stored outside the repository
and in the Actions secret `BENGA_UPDATE_SIGNING_KEY`. Keep an offline backup;
never commit the seed. A signing-tool environment can be prepared with:

```sh
python3 -m venv target/release-tools
target/release-tools/bin/python -m pip install cryptography==50.0.2
```

The secret contains a **base64 raw 32-byte Ed25519 seed**. CI derives the raw
32-byte public key, base64-encodes it and compiles it as `BENGA_UPDATE_PUBLIC_KEY`.
Install the same public key on the [mirror host](server.md). Local builds without
it run normally but cannot check signed updates. Keep this key stable across
releases; rotation requires a migration trusted by existing clients.

Mac notarization and Windows publisher signing are currently unavailable.
Mac packages remain ad-hoc signed and Windows builds unsigned. Update signatures
do not replace OS signing; first-run notes describe the resulting OS warnings.

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

The app checks the mirror first, then the signed GitHub Release feed if the
mirror is unavailable, invalid or has no newer release. Both sources must pass
the same signature/version checks. Feed requests time out after ten seconds;
archive downloads retain their longer timeout. GitHub draft and validation-tag
builds are not offered as public updates.

Startup checks default on; automatic installation defaults off. **F1** opens the
preferences. Self-replacement supports writable Mac bundles and Windows/Linux
archives marked `.benga-portable`; installer-managed copies use their installer
or package manager. Automatic updates install on quit and leave the app closed;
manual updates restart.
A failed swap/restart restores the previous app, retained until successful
startup. There is no crash watchdog; power loss between renames can require
restoring the sibling `.benga-backup-*` directory manually.

## First release checklist

- Record model redistribution permission: the maintainer confirmed no model
  license was supplied. Noncommercial intent does not supply that permission.
  The GPL project license and third-party notices are packaged; Iosevka OFL,
  Gogh MIT and the noncommercial Philly Sans logo-use notice are included.
  Philly Sans's binary is excluded. See [third-party notices](../THIRD_PARTY_NOTICES.md).
- Test installed startup, GPU support, audio permission denial/recovery, silence,
  output-device changes, window controls and updates on every supported OS.
- Exercise offline checks, invalid signatures/checksums and read-only installs.
- Review sources/version, run a validation tag, then stage the matching stable tag.
- Website design approved on 2026-11-02. Test the real installer and complete
  the webroot migration before publishing to `/home/benga/web` after the server
  recovers. Follow [server activation](server.md).

Interactive installed-app GPU/audio checks remain necessary on each platform;
CI package checks do not replace these. Useful checks:
`cargo test --locked`, `cargo run --example window_chrome_probe`, and
`target/release-tools/bin/python -m unittest discover -s scripts -p 'test_*.py'`.
