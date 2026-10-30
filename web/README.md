# Benga download site

This directory is a buildless static site for `benga.zezic.ru`. **It is a local
design preview. Do not publish it until the owner approves the design.**

Preview from the repository root:

```sh
python3 -m http.server 4173 --bind 127.0.0.1 --directory web
```

Open `http://127.0.0.1:4173`. The page contains the logo, three platform
downloads, the macOS installation command, always-visible audio setup instructions,
and a GitHub link. The command uses the Cordex-style soft recess, copies when clicked,
and provides the same interaction by keyboard. Downloads and GitHub use the
Benga hit symbol. It has no animation or external dependencies.

## Release integration

The page requests `/downloads/latest.json` with this schema:

```json
{
  "version": "0.1.0",
  "tag": "v0.1.0",
  "platforms": {
    "macos-aarch64": {
      "url": "https://github.com/zezic/benga/releases/download/v0.1.0/Benga-v0.1.0-macos-aarch64.zip",
      "sha256": "<64 lowercase hex digits>",
      "size": 12345678
    }
  }
}
```

Other platform keys are `macos-x86_64`, `windows-x86_64`, and `linux-x86_64`.
Until a release feed is available, the download buttons lead to the official
GitHub Releases page. Published metadata resolves them to the selected package.
macOS has separate Apple silicon and Intel links. Linux has separate `.deb` and
`.tar.gz` links; Debian packages use the matching
GitHub Release asset, while portable archives can use the local mirror.
Download URLs are accepted only from this repository's GitHub Release assets or
`https://benga.zezic.ru/releases/` or `/downloads/vVERSION/`. On the production
origin, a successful HEAD request with matching size selects the local mirror;
GitHub remains the fallback. An optional `download_url` can select an
installer instead of an updater ZIP; omit it to use `url` for both.

The signed updater manifest must remain byte-for-byte unchanged when mirrored.
The site only reads it to display download links. The macOS installer trusts the
TLS manifest and checks the downloaded archive's SHA-256; the application's
updater separately verifies the Ed25519 signature.

`install-macos.sh` requires macOS 14.6+, detects Rosetta, downloads the ZIP for the
physical CPU, verifies its checksum, validates archive paths and bundle identity,
then installs to `~/Applications/Benga.app`. It removes quarantine from that
downloaded app only, never requests sudo, and preserves the previous app if final
replacement fails. A running Benga must be quit before installation.

Run `python3 web/test-installer.py` from the repository root on macOS to exercise
installation, updating, invalid checksums, unsafe archive paths, failed-swap
rollback, and existing symlink protection with local fixtures. It mocks all
downloads and redirects installation to a temporary directory.

## Assets

The Benga palette in `assets/themes.json` uses the site's background and foreground
with muted green, cyan, blue, ochre, rose and violet accents. Regenerate the plain
OG artwork from that palette and the existing logo:

```sh
python3 web/generate-og.py
```

This creates a self-contained `og.svg` and the 1200 × 630 `og.png`, using Python's
standard library and `resvg`. Pass `--resvg /absolute/path/to/resvg` if it is not
on PATH.

App icon masters, PNGs, ICNS and ICO live in `../assets/icons/`; regenerate them
using `../tools/generate_icons.py` (Pillow and Inkscape, resvg or Sharp).

## Before publication

- Review the desktop and mobile design.
- Run the native release matrix and verify all platform packages on actual OSes.
- Publish the first tagged GitHub Release and activate the release mirror.
- Test the macOS installer against that real release, including an existing app.
- Revisit the unsigned macOS copy if signing and notarization are enabled.
- Serve HTML and `latest.json` with short/no caching; fingerprinted release files
  can be immutable. Set the normal security headers in the Nginx site config.
