#!/usr/bin/env python3
"""Recolor only the original Fold K; retain its exact paths and rendering.

The mint source is copied verbatim. Other variants change only its eight mark
colors and the accessible title, leaving the tile and all geometry unchanged.
"""

import base64
import hashlib
import json
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT.parent / "2026-10-21" / "03-fold.svg"

# Body top/middle/base; fold base/middle/top; rim; edge highlight.
ORIGINAL_COLORS = ["#d3f4d7", "#93d9bd", "#3d9f90", "#328e7d", "#7bc9ac", "#c5edcf", "#efffe6", "#d8f7db"]

PALETTES = [
    {"slug": "01-mint", "name": "Mint", "colors": ORIGINAL_COLORS},
    {"slug": "02-blue", "name": "Blue", "colors": ["#d6eaff", "#91c4ed", "#4585bf", "#2c6d9d", "#73b0d8", "#c3e2f8", "#eff7ff", "#d8ebfa"]},
    {"slug": "03-violet", "name": "Violet", "colors": ["#eadbff", "#c1a2e8", "#8c61c4", "#7250aa", "#ad89d5", "#e0cdf8", "#f9f0ff", "#eadcfc"]},
    {"slug": "04-coral", "name": "Coral", "colors": ["#ffdbc9", "#f2a58c", "#cf6b5a", "#b85648", "#df9077", "#fbd2bb", "#fff4e9", "#ffe0cc"]},
    {"slug": "05-amber", "name": "Amber", "colors": ["#ffe9ae", "#f0c469", "#c29034", "#a77a28", "#dbac4f", "#fbe3a0", "#fff9de", "#fff1be"]},
    {"slug": "06-rose", "name": "Rose", "colors": ["#ffdceb", "#eb9dbd", "#bf5c8e", "#a84678", "#d484aa", "#f6c9de", "#fff0f7", "#fcdce9"]},
]


def recolor(source: str, palette: dict) -> str:
    if palette["slug"] == "01-mint":
        return source
    result = source
    for original, replacement in zip(ORIGINAL_COLORS, palette["colors"]):
        result = result.replace(original, replacement)
    return result.replace("Klay — Fold</title>", f'Klay — Fold · {palette["name"]}</title>')


def render(document: str, target: Path, width: int, height: int | None = None) -> None:
    height = width if height is None else height
    handle = Rsvg.Handle.new_from_data(document.encode())
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, width, height)
    viewport = Rsvg.Rectangle()
    viewport.x = viewport.y = 0
    viewport.width, viewport.height = width, height
    assert handle.render_document(cairo.Context(surface), viewport)
    surface.write_to_png(str(target))


def embed(svg: str, x: int, y: int, size: int) -> str:
    data = "data:image/svg+xml;base64," + base64.b64encode(svg.encode()).decode()
    return f'<image href="{data}" x="{x}" y="{y}" width="{size}" height="{size}"/>'


def comparison(variants: list[tuple[dict, str]]) -> str:
    parts = ['''<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="1080" viewBox="0 0 1280 1080">
      <rect width="1280" height="1080" fill="#121418"/>
      <g font-family="DejaVu Sans, sans-serif">
      <text x="40" y="62" fill="#f0e8e1" font-size="30" font-weight="bold">Klay</text>
      <text x="131" y="61" fill="#9b9fa8" font-size="17">/ Fold · Colour variants</text>
    ''']
    for i, (palette, icon) in enumerate(variants):
        x = 40 + (i % 3) * 408
        y = 112 + (i // 3) * 464
        title = palette["name"] + (" · original" if i == 0 else "")
        parts.append(f'''
          <rect x="{x}" y="{y}" width="384" height="440" rx="23" fill="#1c1f25" stroke="#2e323a"/>
          {embed(icon, x+48, y+8, 288)}
          <text x="{x+192}" y="{y+319}" text-anchor="middle" fill="#eee7df" font-size="20" font-weight="bold">{title}</text>
          {embed(icon, x+124, y+340, 64)}
          {embed(icon, x+228, y+356, 32)}
          <text x="{x+156}" y="{y+423}" text-anchor="middle" fill="#8e97a3" font-size="11">64 px</text>
          <text x="{x+244}" y="{y+423}" text-anchor="middle" fill="#8e97a3" font-size="11">32 px</text>
        ''')
    parts.append('</g></svg>')
    return "".join(parts)


def verify_recolor(original: str, variant: str, palette: dict) -> None:
    before = list(ET.fromstring(original).iter())
    after = list(ET.fromstring(variant).iter())
    assert len(before) == len(after)
    replacements = dict(zip(ORIGINAL_COLORS, palette["colors"]))
    for old, new in zip(before, after):
        assert old.tag == new.tag
        assert old.attrib.keys() == new.attrib.keys()
        for key, value in old.attrib.items():
            expected = replacements.get(value, value) if key in {"stop-color", "stroke"} else value
            assert new.attrib[key] == expected, (key, value, new.attrib[key])
        if old.tag.split('}')[-1] != "title":
            assert old.text == new.text
        assert old.tail == new.tail


def main() -> None:
    source_bytes = SOURCE.read_bytes()
    source = source_bytes.decode()
    variants = []
    for palette in PALETTES:
        icon = recolor(source, palette)
        verify_recolor(source, icon, palette)
        (ROOT / f'{palette["slug"]}.svg').write_text(icon)
        for size in (32, 64, 128, 512, 1024):
            render(icon, ROOT / f'{palette["slug"]}-{size}.png', size)
        variants.append((palette, icon))
    sheet = comparison(variants)
    (ROOT / "comparison.svg").write_text(sheet)
    render(sheet, ROOT / "comparison.png", 1280, 1080)
    (ROOT / "palettes.json").write_text(json.dumps({
        "source": "../2026-10-21/03-fold.svg",
        "source_sha256": hashlib.sha256(source_bytes).hexdigest(),
        "color_roles": ["body_top", "body_middle", "body_base", "fold_base", "fold_middle", "fold_top", "rim", "edge_highlight"],
        "palettes": PALETTES,
    }, indent=2) + "\n")
    assert SOURCE.read_bytes() == source_bytes
    assert (ROOT / "01-mint.svg").read_bytes() == source_bytes
    print("Rendered original mint + five Fold color variants. Verified identical geometry, tile, and rendering attributes.")


if __name__ == "__main__":
    main()
