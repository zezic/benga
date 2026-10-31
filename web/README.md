# Download website

Static site for `benga.zezic.ru`. Target webroot: `/home/benga/web`, **after design
approval and path migration**. For local preview, run from the repository root:

```sh
python3 -m http.server 4173 --bind 127.0.0.1 --directory web
```

The page has no runtime dependencies. It uses the Benga palette, separate
platform download links, a click-to-copy install command, audio setup and GitHub.

## Downloads and installation

`/downloads/latest.json` follows the [release contract](../docs/releases.md).
Before a feed exists, links open GitHub Releases. With metadata, Mac architecture
links and Linux DEB/tar.gz links select the matching assets. Portable archives
prefer the local mirror when its HEAD size matches; DEB installers stay on GitHub.
Keep signed manifest bytes unchanged. An optional `download_url` can select a
standalone installer instead of the updater archive.

`install-macos.sh` requires macOS 14.6+, detects the physical CPU through Rosetta,
checks the TLS manifest and archive SHA-256, validates the bundle and installs to
`~/Applications/Benga.app`. It removes quarantine only from that app, never uses
sudo, and restores the previous app if replacement fails. Quit Benga first.
The app updater additionally verifies Ed25519 signatures.

```sh
python3 web/test-installer.py
```

This Mac-only check uses local fixtures for install/update, checksum/path rejection,
rollback and symlink protection.

## Assets and publication

`python3 web/generate-og.py` creates the plain, self-contained OG SVG and
1200×630 PNG; it needs `resvg` on PATH or `--resvg /absolute/path/to/resvg`.
App icons are in `assets/icons/` at the repository root; regenerate with
`tools/generate_icons.py` using Pillow and Inkscape, resvg or Sharp.

Publish only `index.html`, `style.css`, `app.js`, `favicon.ico`, `assets/` and
`install-macos.sh` to `/home/benga/web`, owned by `benga`. Exclude this README,
generators and tests. Complete native release/installer checks and the pending
webroot migration, then follow [server activation](../docs/server.md). The public
host remains in maintenance until the reviewed site is deployed.
