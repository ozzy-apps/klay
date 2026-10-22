#!/usr/bin/env python3
"""Flat vector refinements of the Klay K the user identified in round one.

All artwork uses solid fills, paths, and transparency. There are no gradients,
filters, highlights, shadows, textures, or simulated depth in the logo assets.
"""

import base64
from html import escape
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg

ROOT = Path(__file__).resolve().parent

# The exact outer silhouette of the earlier green Fold K, without its shading.
SOFT = "M160 129C136 129 124 148 124 176V350C124 380 143 399 171 399 199 399 218 379 218 352V312Q218 285 240 307L317 384C336 403 363 398 377 379 392 359 387 341 371 325L298 251 367 181C385 163 386 142 369 126 352 109 331 112 314 129L236 208Q218 227 218 199V176C218 147 202 129 177 129Z"

CUT = "M150 128H192Q208 128 208 144V226L300 134Q306 128 316 128H366Q381 128 370 140L267 249 375 379Q384 392 369 392H318Q310 392 303 384L208 273V376Q208 392 192 392H150Q134 392 134 376V144Q134 128 150 128Z"

SWEEP = "M157 128C138 128 126 143 126 163V352C126 378 145 396 171 396 197 396 216 377 216 351V308Q216 285 234 303L295 370C317 394 345 403 375 393Q386 389 380 377L287 265Q280 256 289 247L369 167Q380 156 369 145L343 119Q332 108 320 120L235 205Q216 224 216 198V163C216 143 200 128 179 128Z"

CONCEPTS = [
    {"slug": "01-soft", "name": "Soft", "description": "Rounded · mint", "background": "#193B35", "foreground": "#96D7B4", "kind": "soft"},
    {"slug": "02-cut", "name": "Cut", "description": "Angular · cobalt", "background": "#3B53DE", "foreground": "#FFF4DA", "kind": "cut"},
    {"slug": "03-sweep", "name": "Sweep", "description": "Curved · coral", "background": "#FFEBDF", "foreground": "#DB5B49", "kind": "sweep"},
    {"slug": "04-slice", "name": "Slice", "description": "Open · violet", "background": "#EDE7FF", "foreground": "#6952B5", "kind": "slice"},
    {"slug": "05-counter", "name": "Counter", "description": "Reversed · lime", "background": "#DCE87C", "foreground": "#24372D", "kind": "counter"},
]


def geometry(kind: str, color: str) -> str:
    if kind in ("soft", "cut", "sweep"):
        return f'<path d="{dict(soft=SOFT, cut=CUT, sweep=SWEEP)[kind]}" fill="{color}"/>'
    if kind == "slice":
        return f'''<defs><mask id="slice-mask" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512">
          <rect width="512" height="512" fill="#fff"/>
          <path d="M205 290 304 245" fill="none" stroke="#000" stroke-width="18"/>
        </mask></defs>
        <path d="{SOFT}" fill="{color}" mask="url(#slice-mask)"/>'''
    if kind == "counter":
        return f'''<defs><mask id="counter-mask" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512">
          <rect width="512" height="512" fill="#fff"/>
          <path d="{SOFT}" fill="#000" transform="translate(256 256) scale(.69) translate(-256 -256)"/>
        </mask></defs>
        <circle cx="256" cy="256" r="150" fill="{color}" mask="url(#counter-mask)"/>'''
    raise ValueError(kind)


def svg(concept: dict, mode: str = "icon", mono: bool = False) -> str:
    viewbox = "0 0 512 512" if mode == "icon" else "96 96 320 320"
    color = "#26332E" if mono else concept["foreground"]
    background = f'<rect x="32" y="32" width="448" height="448" rx="104" fill="{concept["background"]}"/>' if mode == "icon" else ""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="{viewbox}" role="img" aria-labelledby="title desc">
      <title id="title">Klay — {escape(concept["name"])}</title>
      <desc id="desc">Flat two-dimensional K logo. Solid colors and transparent negative space.</desc>
      {background}{geometry(concept["kind"], color)}
    </svg>'''


def render(document: str, path: Path, width: int, height: int | None = None) -> None:
    ET.fromstring(document)
    height = width if height is None else height
    handle = Rsvg.Handle.new_from_data(document.encode())
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, width, height)
    viewport = Rsvg.Rectangle()
    viewport.x = viewport.y = 0
    viewport.width, viewport.height = width, height
    assert handle.render_document(cairo.Context(surface), viewport)
    surface.write_to_png(str(path))


def embed(document: str, x: int, y: int, size: int) -> str:
    uri = "data:image/svg+xml;base64," + base64.b64encode(document.encode()).decode()
    return f'<image href="{uri}" x="{x}" y="{y}" width="{size}" height="{size}"/>'


def comparison() -> str:
    parts = ['''<svg xmlns="http://www.w3.org/2000/svg" width="1800" height="770" viewBox="0 0 1800 770">
      <rect width="1800" height="770" fill="#F3F2ED"/>
      <g font-family="DejaVu Sans, sans-serif">
      <text x="40" y="62" fill="#24332E" font-size="30" font-weight="bold">Klay</text>
      <text x="131" y="61" fill="#707C74" font-size="17">/ 2D logo directions</text>
      <text x="1760" y="60" text-anchor="end" fill="#7C877F" font-size="12" letter-spacing="1.5">ROUND 03</text>
    ''']
    for i, concept in enumerate(CONCEPTS):
        x = 40 + i * 348
        parts.append(f'''
          <rect x="{x}" y="108" width="328" height="604" rx="22" fill="#FFFEFA" stroke="#DEDFD6"/>
          <text x="{x+22}" y="146" fill="#818C84" font-size="12" letter-spacing="1">0{i+1}</text>
          {embed(svg(concept), x+20, 150, 288)}
          <text x="{x+164}" y="483" text-anchor="middle" fill="#24332E" font-size="21" font-weight="bold">{concept['name']}</text>
          <text x="{x+164}" y="510" text-anchor="middle" fill="#7B847D" font-size="13">{concept['description']}</text>
          <path d="M{x+22} 538H{x+306}" stroke="#E3E5DC"/>
          {embed(svg(concept, 'mark', mono=True), x+31, 567, 74)}
          {embed(svg(concept), x+140, 572, 64)}
          {embed(svg(concept), x+246, 588, 32)}
          <text x="{x+68}" y="670" text-anchor="middle" fill="#818A83" font-size="11">1 colour</text>
          <text x="{x+172}" y="670" text-anchor="middle" fill="#818A83" font-size="11">64 px</text>
          <text x="{x+262}" y="670" text-anchor="middle" fill="#818A83" font-size="11">32 px</text>
        ''')
    parts.append('</g></svg>')
    return "".join(parts)


def main() -> None:
    for concept in CONCEPTS:
        stem = ROOT / concept["slug"]
        icon = svg(concept)
        mark = svg(concept, "mark")
        mono = svg(concept, "mark", mono=True)
        stem.with_suffix(".svg").write_text(icon)
        (ROOT / f'{concept["slug"]}-mark.svg').write_text(mark)
        (ROOT / f'{concept["slug"]}-mono.svg').write_text(mono)
        for size in (32, 64, 512, 1024):
            render(icon, ROOT / f'{concept["slug"]}-{size}.png', size)
        render(mark, ROOT / f'{concept["slug"]}-mark.png', 512)
    sheet = comparison()
    (ROOT / "comparison.svg").write_text(sheet)
    render(sheet, ROOT / "comparison.png", 1800, 770)
    print("Rendered five flat K directions: app icons, standalone marks, monochrome masters, and comparison.")


if __name__ == "__main__":
    main()
