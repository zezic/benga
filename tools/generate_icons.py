#!/usr/bin/env python3
"""Render Benga's vector icon masters. Requires Pillow and Inkscape, resvg or Sharp.

The full-bleed layers are retained for Icon Composer. The .icns artwork has
traditional macOS padding and a rounded plate for macOS 14 and 15 compatibility.
"""
from pathlib import Path
import argparse
import os
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "assets/icons"
SYMBOL = '<path d="M340 340 454 454M570 570 684 684M684 340 570 454M454 570 340 684" fill="none" stroke="#d1dfba" stroke-width="64" stroke-linecap="butt"/>'
GRADIENT = '<defs><linearGradient id="plate" x2="0" y2="1"><stop stop-color="#435b4a"/><stop offset="1" stop-color="#2c4035"/></linearGradient></defs>'


def svg(content):
    return '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">' + content + '</svg>\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inkscape', default=shutil.which('inkscape') or '/Applications/Inkscape.app/Contents/MacOS/inkscape')
    parser.add_argument('--resvg', help='Use this resvg executable instead of Inkscape')
    parser.add_argument('--node-modules', help='Use the sharp package from this node_modules directory instead of Inkscape')
    args = parser.parse_args()
    from PIL import Image
    DEST.mkdir(parents=True, exist_ok=True)
    (DEST / 'background.svg').write_text(svg(GRADIENT + '<rect width="1024" height="1024" fill="url(#plate)"/>'))
    (DEST / 'symbol.svg').write_text(svg(SYMBOL))
    (DEST / 'benga-square.svg').write_text(svg(GRADIENT + '<rect width="1024" height="1024" fill="url(#plate)"/>' + SYMBOL))
    mac = GRADIENT + '<rect x="96" y="106" width="832" height="832" rx="184" fill="#000" opacity=".16"/>'
    mac += '<rect x="96" y="96" width="832" height="832" rx="184" fill="url(#plate)"/>'
    mac += '<rect x="97" y="97" width="830" height="830" rx="183" fill="none" stroke="#95ad8c" stroke-opacity=".28" stroke-width="2"/>' + SYMBOL
    (DEST / 'benga.svg').write_text(svg(mac))
    for size in [16, 32, 48, 64, 128, 256, 512, 1024]:
        path = DEST / ('benga.png' if size == 1024 else f'benga-{size}.png')
        if args.node_modules:
            env = dict(os.environ, NODE_PATH=args.node_modules)
            subprocess.run(['node', '-e', 'require("sharp")(process.argv[1]).resize(Number(process.argv[3])).png().toFile(process.argv[2]).catch(e=>{console.error(e);process.exit(1)})', str(DEST / 'benga.svg'), str(path), str(size)], env=env, check=True)
        elif args.resvg:
            subprocess.run([args.resvg, '--width', str(size), str(DEST / 'benga.svg'), str(path)], check=True)
        else:
            subprocess.run([args.inkscape, str(DEST / 'benga.svg'), '--export-width=' + str(size), '--export-filename=' + str(path)], check=True, stdout=subprocess.DEVNULL)
    Image.open(DEST / 'benga.png').save(DEST / 'benga.ico', sizes=[(s, s) for s in [16, 32, 48, 64, 128, 256]])
    Image.open(DEST / 'benga.png').save(DEST / 'benga.icns')
    print('Wrote SVG masters, PNG sizes, ICO and ICNS to', DEST)


if __name__ == '__main__':
    main()
