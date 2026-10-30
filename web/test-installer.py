#!/usr/bin/env python3
"""Exercise installer safety with local fixtures on macOS, without network.

The installer under test is redirected to a temporary Applications directory.
No real application is installed, launched, or quarantined.
"""
from pathlib import Path
import hashlib
import json
import os
import platform
import plistlib
import shlex
import shutil
import subprocess
import tempfile
import zipfile


def main():
    if platform.system() != 'Darwin':
        raise SystemExit('These tests use macOS plutil and ditto; run on macOS.')
    source = Path(__file__).with_name('install-macos.sh').read_text()
    with tempfile.TemporaryDirectory(prefix='benga-installer-test-') as directory:
        root = Path(directory)
        app = root / 'fixture/Benga.app'
        (app / 'Contents/MacOS').mkdir(parents=True)
        executable = app / 'Contents/MacOS/benga'
        executable.write_text('#!/bin/sh\nexit 0\n')
        executable.chmod(0o755)
        info = {'CFBundleIdentifier': 'com.zezic.benga', 'CFBundleShortVersionString': '0.1.0'}
        (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        archive = root / 'Benga.zip'
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as output:
            for path in app.rglob('*'):
                if path.is_file():
                    output.write(path, path.relative_to(app.parent))
        original_archive = archive.read_bytes()
        install_root = root / 'Applications'
        script = root / 'install.sh'
        assert source.count('install_root="$HOME/Applications"') == 1
        script.write_text(source.replace('install_root="$HOME/Applications"', 'install_root=' + shlex.quote(str(install_root))))
        mocks = root / 'bin'
        mocks.mkdir()
        curl = mocks / 'curl'
        curl.write_text('#!/usr/bin/env python3\nimport shutil,sys\nfrom pathlib import Path\na=sys.argv[1:]; out=a[a.index("-o")+1]; src=' + repr(str(root)) + '\nshutil.copyfile(Path(src)/("latest.json" if any(x.endswith("/latest.json") for x in a) else "Benga.zip"),out)\n')
        curl.chmod(0o755)
        pgrep = mocks / 'pgrep'
        pgrep.write_text('#!/bin/sh\nexit 1\n')
        pgrep.chmod(0o755)
        env = dict(os.environ, PATH=str(mocks) + ':' + os.environ['PATH'])
        arch = 'aarch64' if platform.machine() == 'arm64' else 'x86_64'

        def manifest(checksum=None):
            entry = {'url': f'https://github.com/zezic/benga/releases/download/v0.1.0/Benga-v0.1.0-macos-{arch}.zip', 'sha256': checksum or hashlib.sha256(archive.read_bytes()).hexdigest()}
            (root / 'latest.json').write_text(json.dumps({'version': '0.1.0', 'platforms': {f'macos-{arch}': entry}}))

        def run(expected_success, message):
            result = subprocess.run(['/bin/bash', str(script)], env=env, text=True, capture_output=True)
            assert (result.returncode == 0) == expected_success, result.stdout + result.stderr
            assert message in result.stdout + result.stderr, result.stdout + result.stderr

        destination = install_root / 'Benga.app'
        manifest()
        run(True, 'Installed Benga 0.1.0')
        assert (destination / 'Contents/MacOS/benga').is_file()
        (destination / 'preserve-marker').write_text('previous app')
        manifest('0' * 64)
        run(False, 'checksum did not match')
        assert (destination / 'preserve-marker').read_text() == 'previous app'

        with zipfile.ZipFile(archive, 'w') as output:
            output.writestr('../outside.txt', 'unsafe')
        manifest()
        run(False, 'unexpected files')
        assert (destination / 'preserve-marker').exists()
        assert not (root / 'outside.txt').exists()

        archive.write_bytes(original_archive)
        manifest()
        mv = mocks / 'mv'
        mv.write_text('#!/bin/sh\ncase "$1" in */.Benga-incoming-*) exit 1;; esac\nexec /bin/mv "$@"\n')
        mv.chmod(0o755)
        run(False, 'previous version was restored')
        assert (destination / 'preserve-marker').exists()
        assert not list(install_root.glob('.Benga-*'))
        mv.unlink()

        run(True, 'Installed Benga 0.1.0')
        assert not (destination / 'preserve-marker').exists()
        assert not list(install_root.glob('.Benga-*'))
        shutil.rmtree(destination)
        destination.symlink_to(app, target_is_directory=True)
        run(False, 'installation path is a symbolic link')
        assert (app / 'Contents/MacOS/benga').exists()
    print('PASS: install, update, checksum rejection, path rejection, rollback, and symlink protection')


if __name__ == '__main__':
    main()
