# Desktop releases

Release packages are built natively for macOS (Apple Silicon and Intel), Windows
(x64), and Linux (x64 and ARM64). The release workflow runs **only when a `v*`
tag is pushed**. Branch pushes and pull requests do not start packaging jobs.

## Local packages

Use Python 3.11 or newer, a Rust toolchain, and the native platform's package tools:

```sh
python3 scripts/package.py
```

The script builds with `cargo build --locked --release` and writes packages to
`target/packages/`. Use `--target aarch64-apple-darwin` or another supported triple
to choose an architecture. Native packaging requires that operating system; this
is a build matrix, not an attempt to emulate all systems on one machine.

`--skip-build` packages an existing release binary. `--no-dmg` creates only the
macOS ZIP when disk image creation is unavailable in a sandbox. Local builds
without `BENGA_UPDATE_PUBLIC_KEY` can run, but authenticated app updates require
the release verification key to have been compiled in.

| Platform | Native tools | Outputs |
| --- | --- | --- |
| macOS 14.6+ | Xcode command line tools, `codesign`, `ditto`, `hdiutil` | `.app` inside ZIP, drag-to-Applications DMG |
| Windows x64 | MSVC/Windows SDK (`rc.exe` on PATH), Inno Setup 6 | portable ZIP, per-user setup EXE |
| Linux glibc 2.35+ | ALSA/PulseAudio/udev development packages, `dpkg-deb`, Vulkan driver | portable tar.gz, Debian package |

The Windows setup installs under `%LOCALAPPDATA%\Programs\Benga`; administrator
access is not required. The CI build statically links the Windows C runtime.
The Debian package installs to `/usr/lib/benga` and registers an application menu
entry. Archive users must keep the binary and assets together.

Resource layouts are `Benga.app/Contents/Resources/assets` on macOS, an adjacent
`assets` directory on Windows and portable Linux, and `/usr/lib/benga/assets` for
Debian packages. Unused Philly Sans files with a noncommercial license are
excluded from packages. Windows and Linux portable archives include an empty
`.benga-portable` marker; installer-managed installations omit it, so the updater
does not replace files owned by a package manager or uninstaller.
The embedded Iosevka subsets are accompanied by the official upstream SIL Open
Font License and copyright notice at `assets/licenses/Iosevka-OFL.txt` inside
every bundle. The repository's subset font name tables are empty, so the exact
original Iosevka build/version cannot be established from those files alone.

## Authenticated update feed

The app checks at startup by default; automatic installation is off by default.
Open **Help / updates** in the controls, or press **F1**, to change either
preference or check manually. Settings persist in `benga-config.ron` under
`updates: (check_on_start: true, auto_install: false)`. New users see the audio
setup panel once, after the themed startup splash; it remains available with
F1. If audio capture fails, the app opens with routing/permission help.

The small startup logo stays visible for two seconds over a 90%-opaque themed
background, then fades over half a second. The compact Help/updates dialog uses
the current scene colors and is also available below Analyzer Boost in the controls.
Profiling bypasses the splash, onboarding and update checks. `--help` and
`--version` work without initializing graphics or audio capture.

Window chrome uses Cordex-style client decorations: Windows caption buttons on
the right, Linux window controls on the left, and native traffic lights on macOS.
The vector Benga logo is centered in the window and follows the scene theme over a
50%-opaque, borderless titlebar background tinted 5% toward the foreground. It is
hidden while the splash holds, then fades in as the splash fades out.
Drag the titlebar to move the window; double-click it to maximize or restore.
Client decorations disappear in fullscreen. macOS measures the native titlebar
and button geometry through AppKit, including the taller macOS 26+ layout, and
fades its overlay using the controls' existing five-second inactivity timer.
Hovering the titlebar or controls keeps them visible. Developer inspectors stay
below the measured titlebar. Profiling excludes the titlebar overlay as well.

`cargo run --example window_chrome_probe` checks native macOS geometry and fades
the titlebar on a hidden probe window. It verifies that the app's content and
window remain fully opaque and the native controls restore after being hidden.
Windows/Linux chrome has geometry and GPU-render checks; native OS smoke tests
should also exercise drag, edge resizing, maximize, minimize, and close.

Before a public release, create an Ed25519 signing key and save the **base64 raw
32-byte seed** as the GitHub Actions secret `BENGA_UPDATE_SIGNING_KEY`. Keep an
offline backup. Do not commit the private key, paste it into issues, or put it in
workflow inputs. Use a private terminal/secret manager when generating it:

```sh
python3 -m venv target/release-tools
target/release-tools/bin/python -m pip install cryptography==50.0.2
```

Use that environment's `cryptography.hazmat.primitives.asymmetric.ed25519` API to
generate `Ed25519PrivateKey.generate().private_bytes_raw()`, base64 encode it, and
store the result directly in the secret manager. The release pipeline derives
the public key and compiles it into every platform binary as
`BENGA_UPDATE_PUBLIC_KEY`; the private key is used only in preparation/signing
steps. Release creation fails if a key is absent, malformed, or inconsistent.

The updater contract is:

```json
{
  "version": "0.1.0",
  "tag": "v0.1.0",
  "platforms": {
    "macos-aarch64": {
      "url": "https://github.com/zezic/benga/releases/download/v0.1.0/Benga-v0.1.0-macos-aarch64.zip",
      "sha256": "<64 lowercase hexadecimal characters>",
      "size": 123456
    }
  }
}
```

Every release includes all five keys: `macos-aarch64`, `macos-x86_64`,
`windows-x86_64`, `linux-x86_64`, and `linux-aarch64`. The detached
`latest.json.sig` is a standard-base64 raw 64-byte Ed25519 signature over the exact
bytes of `latest.json`, including its trailing newline. Clients verify this
signature before accepting URLs and verify each downloaded archive's size and
SHA-256 before staging an update. ZIPs contain `Benga.app` on macOS and a `Benga`
directory on Windows; Linux tarballs contain a `Benga` directory. `SHA256SUMS`
also covers the standalone installers and feed files.

Update installation needs a writable app parent directory. Benga does not
escalate privileges to replace installer-managed or system-managed files.
Automatic installation is opt-in, applies on quit, and leaves the app closed;
manual installation explicitly restarts it. A separate helper waits for the
running app to exit, swaps directories on the same filesystem, and restores the
previous directory if the swap or restart command fails. A cleanup receipt is
kept beside the app, outside the macOS code signature, and the old installation
is removed only after the updated app starts graphics successfully. Cleanup
waits briefly for the helper to exit and retries at a later startup if needed.

Signature checks establish publisher authorization for the exact archive; they
do not replace macOS notarization or Windows Authenticode. Backup retention
supports recovery from a failed launch, but there is no watchdog that
automatically rolls back a later application crash. Power loss between directory
renames can require manually restoring the sibling `.benga-backup-*` directory.
Native update tests should cover these recovery paths before public rollout.

Changing the signing key requires a migration release trusted by existing
clients. Keep the same public verification key across ordinary releases and
across all five platform binaries. Simply rotating the CI secret makes old
clients reject the new feed. Back up the seed before the first release: losing
it means existing installations cannot authenticate later releases.

## Publishing a release

1. Complete the asset redistribution/license audit and OS smoke tests below.
2. Set `package.version` in `Cargo.toml` and update `Cargo.lock`; commit the exact
   release sources. Tag protection should limit who can push release tags.
3. Push the matching stable tag, for example `v0.1.0`. The workflow rejects tags
   that do not exactly match the Cargo version or contain prerelease suffixes.
4. All five native jobs must pass their tests and package creation. The final
   job signs the feed, uploads everything into a draft GitHub Release, then
   publishes the complete release. Published releases are never overwritten;
   an interrupted draft can be retried.

Standard GitHub-hosted runners are free for public repositories. The temporary
Actions artifacts have a three-day retention period. Users download durable
GitHub **Release assets**, not expiring Actions artifacts. Larger runners are
not used. See [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
and [runner availability](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Website mirror

The server should pull completed public GitHub Releases as the unprivileged
`benga` account, instead of giving Actions a root SSH key. Download into a fresh
staging directory, verify the signed feed and all hashes, then rename the
completed release directory and atomically switch a `current` symlink. Keep at
least one previous release for rollback. Preserve the signed JSON bytes rather
than rewriting its URLs; the signed feed can retain GitHub download URLs while
the website's download buttons use matching mirrored assets.

Use a dedicated Nginx virtual host for `benga.zezic.ru`. Scope Certbot to that
hostname and validate the combined Nginx configuration before reloading. Website
content publication waits for the owner's design review. Mirror polling and
certificate renewal do not need to run a build or trigger GitHub Actions.

## Before the first public tag

- Audit model sources and asset redistribution rights. A code license alone
  does not establish rights to third-party models or embedded fonts. Include
  all required notices in the distribution.
- Test startup by opening the installed app from Finder, the Start menu, and a
  Linux application launcher with an unrelated working directory.
- Exercise first audio permission grant/denial and recovery, silent audio,
  changing output devices, and unsupported GPU behavior on each OS.
- Exercise update success, cancelled download, invalid signature, corrupt archive,
  offline startup, and a read-only/system-managed installation.
- Review the icon at Dock/menu sizes and the website at desktop/mobile widths.
- Sign and notarize macOS releases with a Developer ID for the smoothest install
  flow; ad-hoc signatures do not establish publisher identity or bypass
  Gatekeeper. Windows Authenticode signing similarly avoids an unsigned
  publisher identity. These credentials are separate from the update signing key.

Local verification on one Mac validates packaging structure and signatures. It
does not replace native Windows/Linux runs or prove that hosted runners have
completed a release. No tag or public release is created by local packaging.

## Preparation handoff: 2026-10-31

The desktop preparation is implemented locally: five-platform native packaging,
tag-triggered release workflow, platform icons, Benga theme, themed splash,
compact Help/updates dialog, signed app updates and client window decorations.
The centered vector titlebar logo fades in as the splash fades out. Its
borderless background is tinted 5% toward the foreground at 50% opacity.

The website remains minimal and unpublished: separate platform variant links,
macOS installation command, visible audio setup and a plain OG image. Model
backdrops are removed. Generated model renders are outside the website tree in
`target/brand-wireframes/`. Server provisioning and the disabled mirror are
recorded in `docs/server.md`.

Before release or deployment:

- Reconcile and review the concurrent optimisation changes, then commit the
  intended sources. The working tree contains work from both efforts.
- Configure and back up `BENGA_UPDATE_SIGNING_KEY`, then install its matching
  public key on the mirror host. Local development packages have no updater key.
- Run the release matrix and native install/audio/window/update smoke tests on
  Intel macOS, Windows and Linux. Only local Apple Silicon packages are verified.
- Complete the model and embedded-font licensing/provenance audit.
- Decide whether to add Developer ID/notarization and Windows publisher signing;
  current Mac packages are ad-hoc signed.
- Obtain website design approval, publish the first complete tagged release,
  test the real installer, then publish the site and enable mirror polling.

Further onboarding work can include audio-device reconnection and a silence
hint. Installer-managed Windows and Debian packages currently use their
installer/package manager for updates; app self-replacement covers writable Mac
bundles and marked portable installations. No commit, tag, push, public release,
or website publication was made as part of this preparation.
