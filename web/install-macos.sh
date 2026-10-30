#!/bin/bash
# Benga installer for macOS. Review at https://benga.zezic.ru/install-macos.sh
# Uses a TLS release manifest and verifies the archive's SHA-256 before extraction.
# Installs only to ~/Applications; removes quarantine only from the staged Benga.
set -euo pipefail

fail() { printf 'Benga: %s\n' "$*" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || fail 'This installer requires macOS.'
[ "$(id -u)" -ne 0 ] || fail 'Run this installer as your normal user, without sudo.'
for utility in curl plutil shasum ditto unzip xattr; do
  command -v "$utility" >/dev/null || fail "Required macOS utility is missing: $utility"
done

os_version=$(sw_vers -productVersion)
IFS=. read -r os_major os_minor os_patch <<< "$os_version"
if [ "$os_major" -lt 14 ] || { [ "$os_major" -eq 14 ] && [ "${os_minor:-0}" -lt 6 ]; }; then
  fail 'Benga requires macOS 14.6 or later for system audio capture.'
fi
case "$(uname -m)" in
  arm64) arch=aarch64 ;;
  x86_64)
    # A terminal running under Rosetta still needs the native Apple silicon app.
    if [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then arch=aarch64; else arch=x86_64; fi ;;
  *) fail 'Unsupported Mac processor.' ;;
esac

if pgrep -x benga >/dev/null 2>&1; then
  fail 'Quit Benga before installing an update, then run this command again.'
fi

stage=$(mktemp -d "${TMPDIR:-/tmp}/benga-install.XXXXXX")
incoming=''
backup=''
destination=''
cleanup() {
  if [ -n "$backup" ] && [ -d "$backup" ] && [ ! -e "$destination" ]; then mv "$backup" "$destination"; fi
  [ -z "$incoming" ] || [ ! -d "$incoming" ] || rm -rf "$incoming"
  rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fetch() { curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 2 "$1" -o "$2"; }

printf 'Finding the latest Benga release for %s…\n' "$arch"
fetch 'https://benga.zezic.ru/downloads/latest.json' "$stage/latest.json" || fail 'No release is available yet, or the release server could not be reached.'
version=$(plutil -extract version raw -o - "$stage/latest.json") || fail 'Invalid release manifest.'
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'The release has an invalid stable version.'
key="platforms.macos-$arch"
url=$(plutil -extract "$key.url" raw -o - "$stage/latest.json") || fail 'No release was found for this Mac.'
checksum=$(plutil -extract "$key.sha256" raw -o - "$stage/latest.json") || fail 'The release has no checksum.'
[[ "$checksum" =~ ^[a-fA-F0-9]{64}$ ]] || fail 'Invalid archive checksum.'
filename="Benga-v$version-macos-$arch.zip"
case "$url" in
  "https://github.com/zezic/benga/releases/download/v$version/$filename"|"https://benga.zezic.ru/releases/v$version/$filename") ;;
  *) fail 'The release URL does not belong to the Benga release archive.' ;;
esac

printf 'Downloading Benga %s…\n' "$version"
fetch "$url" "$stage/Benga.zip"
actual=$(shasum -a 256 "$stage/Benga.zip")
actual=${actual%% *}
checksum=$(printf '%s' "$checksum" | tr 'A-F' 'a-f')
[ "$actual" = "$checksum" ] || fail 'The archive checksum did not match. Nothing was installed.'

# Refuse archive paths outside the one expected application directory.
unzip -Z -1 "$stage/Benga.zip" > "$stage/entries.txt"
while IFS= read -r entry; do
  case "$entry" in Benga.app|Benga.app/*) ;; *) fail 'The archive contains unexpected files.' ;; esac
  case "/$entry/" in */../*|*/./*) fail 'The archive contains an unsafe path.' ;; esac
done < "$stage/entries.txt"
mkdir "$stage/unpacked"
ditto -x -k "$stage/Benga.zip" "$stage/unpacked"
app="$stage/unpacked/Benga.app"
[ -x "$app/Contents/MacOS/benga" ] || fail 'The download did not contain a valid Benga application.'
[ "$(plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")" = 'com.zezic.benga' ] || fail 'The application bundle identifier did not match.'
[ "$(plutil -extract CFBundleShortVersionString raw -o - "$app/Contents/Info.plist")" = "$version" ] || fail 'The application version did not match.'
[ -z "$(find "$app" -type l -print -quit)" ] || fail 'The archive contains unexpected symbolic links.'

# Quarantine removal is deliberately limited to the downloaded app, never the
# user's Applications directory or an unrelated file tree.
xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
install_root="$HOME/Applications"
destination="$install_root/Benga.app"
mkdir -p "$install_root"
[ ! -L "$destination" ] || fail 'The installation path is a symbolic link; move it away before retrying.'
[ ! -e "$destination" ] || [ -d "$destination" ] || fail 'The installation path is not an application directory.'
if [ -e "$destination" ]; then
  [ "$(plutil -extract CFBundleIdentifier raw -o - "$destination/Contents/Info.plist" 2>/dev/null)" = 'com.zezic.benga' ] || fail 'The existing application is not a Benga bundle.'
fi
incoming="$install_root/.Benga-incoming-$$.app"
backup="$install_root/.Benga-previous-$$.app"
[ ! -e "$incoming" ] && [ ! -L "$incoming" ] && [ ! -e "$backup" ] && [ ! -L "$backup" ] || fail 'A previous installer staging path already exists.'
ditto "$app" "$incoming"
if [ -e "$destination" ]; then mv "$destination" "$backup"; fi
if ! mv "$incoming" "$destination"; then
  [ ! -e "$backup" ] || mv "$backup" "$destination"
  rm -rf "$incoming"
  fail 'Could not install the application; the previous version was restored.'
fi
[ ! -e "$backup" ] || rm -rf "$backup"
printf '\nInstalled Benga %s to %s\nOpen it from Finder, then allow system audio recording when prompted.\n' "$version" "$destination"
