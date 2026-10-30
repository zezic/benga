#!/usr/bin/env python3
"""Build the minimal 1200x630 OG image with Benga's brand colors."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'web/assets'
SVG = 'http://www.w3.org/2000/svg'


def brand_theme():
    themes = json.loads((ROOT / 'assets/themes.json').read_text())
    return next(theme for theme in themes if theme['name'] == 'Benga')


def logo_artwork():
    document = ET.fromstring((DEST / 'benga-logo.svg').read_text())
    layer = document.find(f'{{{SVG}}}g')
    if layer is None:
        raise ValueError('The Benga logo must contain its SVG artwork layer')
    for element in layer.iter():
        element.attrib = {key: value for key, value in element.attrib.items() if not key.startswith('{')}
    ET.register_namespace('', SVG)
    return ET.tostring(layer, encoding='unicode')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--resvg', default=shutil.which('resvg'), help='Path to the resvg rasterizer')
    args = parser.parse_args()
    if not args.resvg:
        parser.error('Install resvg or pass its executable path with --resvg')
    theme = brand_theme()
    background = theme['background']
    foreground = theme['foreground']
    muted = '#9bac98'
    artwork = logo_artwork()
    og = f'''<svg xmlns="{SVG}" width="1200" height="630" viewBox="0 0 1200 630">
<rect width="1200" height="630" fill="{background}"/>
<g transform="translate(380 145) scale(.859375)">{artwork}</g>
<text x="600" y="383" text-anchor="middle" font-family="Arial,Helvetica,sans-serif" font-size="27" fill="{foreground}">Desktop audio visualizer.</text>
<text x="600" y="445" text-anchor="middle" font-family="Arial,Helvetica,sans-serif" font-size="18" fill="{muted}">macOS · Windows · Linux</text>
</svg>
'''
    svg = DEST / 'og.svg'
    png = DEST / 'og.png'
    svg.write_text(og)
    subprocess.run([args.resvg, str(svg), str(png)], check=True)
    print(f'Generated {svg.relative_to(ROOT)} ({svg.stat().st_size:,} bytes)')
    print(f'Generated {png.relative_to(ROOT)} ({png.stat().st_size:,} bytes), 1200x630')


if __name__ == '__main__':
    main()
