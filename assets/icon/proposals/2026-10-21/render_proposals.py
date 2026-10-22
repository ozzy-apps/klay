#!/usr/bin/env python3
"""Rebuild Klay's five original SVG icon proposals and their rendered previews.

Uses the system's librsvg/PyGObject and Cairo; no application dependencies change.
"""

from __future__ import annotations

import base64
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg


ROOT = Path(__file__).resolve().parent
COMMON_DEFS = """
  <linearGradient id="tile" x1="0" y1="0" x2="0.35" y2="1">
    <stop stop-color="#393a40"/><stop offset="1" stop-color="#1c1d22"/>
  </linearGradient>
  <linearGradient id="rim" x1="0" y1="0" x2="0.6" y2="1">
    <stop stop-color="#fff" stop-opacity=".23"/><stop offset=".5" stop-color="#fff" stop-opacity=".06"/>
    <stop offset="1" stop-color="#fff" stop-opacity=".02"/>
  </linearGradient>
  <filter id="tile-shadow" x="-20%" y="-20%" width="140%" height="145%" color-interpolation-filters="sRGB">
    <feGaussianBlur in="SourceAlpha" stdDeviation="6"/><feOffset dy="6"/>
    <feComponentTransfer><feFuncA type="linear" slope=".3"/></feComponentTransfer>
    <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter>
  <filter id="lift" x="-30%" y="-30%" width="160%" height="170%" color-interpolation-filters="sRGB">
    <feGaussianBlur in="SourceAlpha" stdDeviation="5"/><feOffset dy="7"/>
    <feComponentTransfer><feFuncA type="linear" slope=".3"/></feComponentTransfer>
    <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter>
  <filter id="soft" x="-50%" y="-100%" width="200%" height="300%"><feGaussianBlur stdDeviation="8"/></filter>
"""


def icon(name: str, defs: str, mark: str, light: bool = False) -> str:
    light_defs = """<linearGradient id="ivory" x1="0" y1="0" x2=".5" y2="1">
        <stop stop-color="#fff6e9"/><stop offset="1" stop-color="#dfcdb7"/></linearGradient>"""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512" role="img" aria-labelledby="title desc">
  <title id="title">Klay — {name}</title>
  <desc id="desc">App icon proposal. Original scalable vector artwork, with transparent outer margins.</desc>
  <defs>{COMMON_DEFS}{light_defs if light else ""}{defs}</defs>
  <rect x="32" y="32" width="448" height="448" rx="104" fill="url(#{"ivory" if light else "tile"})" filter="url(#tile-shadow)"/>
  <rect x="33" y="33" width="446" height="446" rx="103" fill="none" stroke="url(#rim)" stroke-width="2"/>
  {mark}
</svg>
'''


CONCEPTS = [
    (
        "01-sculpt", "Sculpt", "Clay monogram",
        icon("Sculpt", """
          <linearGradient id="stem" x1="0" y1="0" x2=".8" y2="1"><stop stop-color="#ffd2a3"/><stop offset=".5" stop-color="#f4ad74"/><stop offset="1" stop-color="#db8655"/></linearGradient>
          <linearGradient id="upper" x1="0" y1="0" x2=".6" y2="1"><stop stop-color="#ffce96"/><stop offset="1" stop-color="#ee925d"/></linearGradient>
          <linearGradient id="lower" x1="0" y1="0" x2=".6" y2="1"><stop stop-color="#f9ac78"/><stop offset="1" stop-color="#e57958"/></linearGradient>
          <linearGradient id="glaze" x1="0" y1="0" x2="1" y2="1"><stop stop-color="#fff" stop-opacity=".2"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/></linearGradient>
        """, """
          <g filter="url(#lift)" transform="translate(0 -4)">
            <rect x="127" y="126" width="70" height="272" rx="25" fill="url(#stem)"/>
            <path d="M222 226 303 139Q317 125 331 139L358 166Q372 180 358 194L277 275Q263 289 249 275L222 248Q211 237 222 226Z" fill="url(#upper)"/>
            <path d="M222 281 249 254Q263 240 277 254L367 344Q381 358 367 372L340 399Q326 413 312 399L222 309Q208 295 222 281Z" fill="url(#lower)"/>
            <path d="M143 153Q143 141 155 141H176" fill="none" stroke="#ffe2be" stroke-opacity=".5" stroke-width="2.5" stroke-linecap="round"/>
            <path d="M235 232 313 154Q317 150 321 153" fill="none" stroke="#fff0d3" stroke-opacity=".35" stroke-width="2" stroke-linecap="round"/>
          </g>
        """),
    ),
    (
        "02-strata", "Strata", "Contour layers",
        icon("Strata", """
          <linearGradient id="contour" x1=".1" y1="0" x2=".9" y2="1"><stop stop-color="#ffd1a1"/><stop offset=".48" stop-color="#f5a66b"/><stop offset="1" stop-color="#d47754"/></linearGradient>
          <path id="contour-shape" d="M261 113C331 111 390 163 390 229 390 278 356 321 316 359 278 397 225 405 180 378 134 354 114 310 121 263 126 218 164 199 185 165 205 134 227 114 261 113Z"/>
        """, """
          <g filter="url(#lift)" fill="none" stroke="url(#contour)" stroke-linejoin="round">
            <use href="#contour-shape" stroke-width="15"/>
            <use href="#contour-shape" transform="translate(256 256) scale(.79) translate(-256 -256)" stroke-width="18"/>
            <use href="#contour-shape" transform="translate(256 256) scale(.58) translate(-256 -256)" stroke-width="23"/>
            <use href="#contour-shape" transform="translate(256 256) scale(.37) translate(-256 -256)" stroke-width="32"/>
            <use href="#contour-shape" transform="translate(256 256) scale(.16) translate(-256 -256)" stroke-width="50"/>
          </g>
        """),
    ),
    (
        "03-fold", "Fold", "Sculpted ribbon",
        icon("Fold", """
          <linearGradient id="jade" x1=".15" y1="0" x2=".8" y2="1"><stop stop-color="#d3f4d7"/><stop offset=".4" stop-color="#93d9bd"/><stop offset="1" stop-color="#3d9f90"/></linearGradient>
          <linearGradient id="fold" x1="0" y1="0" x2="1" y2=".8"><stop stop-color="#328e7d"/><stop offset=".45" stop-color="#7bc9ac"/><stop offset="1" stop-color="#c5edcf"/></linearGradient>
          <linearGradient id="fold-rim" x1="0" y1="0" x2="1" y2="1"><stop stop-color="#efffe6" stop-opacity=".5"/><stop offset="1" stop-color="#efffe6" stop-opacity="0"/></linearGradient>
        """, """
          <g filter="url(#lift)" transform="translate(0 -3)">
            <path d="M160 129C136 129 124 148 124 176V350C124 380 143 399 171 399 199 399 218 379 218 352V312Q218 285 240 307L317 384C336 403 363 398 377 379 392 359 387 341 371 325L298 251 367 181C385 163 386 142 369 126 352 109 331 112 314 129L236 208Q218 227 218 199V176C218 147 202 129 177 129Z" fill="url(#jade)"/>
            <path d="M219 287Q220 263 245 244L314 175Q325 163 338 173L351 186 287 252 371 336Q387 353 375 372L365 383 260 279Q242 262 219 287Z" fill="url(#fold)"/>
            <path d="M143 348V174Q143 148 165 147" fill="none" stroke="url(#fold-rim)" stroke-width="3" stroke-linecap="round"/>
            <path d="M219 284Q240 259 260 278L365 383" fill="none" stroke="#d8f7db" stroke-opacity=".4" stroke-width="2.5" stroke-linecap="round"/>
          </g>
        """),
    ),
    (
        "04-imprint", "Imprint", "Pressed clay",
        icon("Imprint", """
          <linearGradient id="clay" x1=".15" y1="0" x2=".85" y2="1"><stop stop-color="#e6a080"/><stop offset=".45" stop-color="#d48663"/><stop offset="1" stop-color="#ba6a4c"/></linearGradient>
          <linearGradient id="edge" x1="0" y1="0" x2="0" y2="1"><stop stop-color="#b97051"/><stop offset="1" stop-color="#9c573c"/></linearGradient>
          <linearGradient id="cut" x1="0" y1="0" x2=".75" y2="1"><stop stop-color="#774331"/><stop offset=".35" stop-color="#9a583e"/><stop offset="1" stop-color="#b36e50"/></linearGradient>
          <path id="pressed-k" d="M180 168Q180 161 187 161H211Q218 161 218 168V232L278 168Q284 161 294 161H326Q338 161 329 172L254 251 334 336Q345 349 331 349H298Q289 349 283 342L218 274V342Q218 349 211 349H187Q180 349 180 342Z"/>
        """, """
          <ellipse cx="258" cy="381" rx="113" ry="18" fill="#72513a" opacity=".14" filter="url(#soft)"/>
          <g filter="url(#lift)">
            <rect x="119" y="121" width="274" height="282" rx="72" fill="url(#edge)"/>
            <rect x="119" y="111" width="274" height="282" rx="72" fill="url(#clay)"/>
            <rect x="120.5" y="112.5" width="271" height="279" rx="70.5" fill="none" stroke="#ffd3b7" stroke-opacity=".36" stroke-width="2"/>
            <use href="#pressed-k" transform="translate(1 3)" fill="#f2b795" opacity=".6"/>
            <use href="#pressed-k" fill="url(#cut)"/>
            <path d="M187 168H210V238" fill="none" stroke="#653a2c" stroke-opacity=".32" stroke-width="3" stroke-linejoin="round"/>
          </g>
        """, light=True),
    ),
    (
        "05-core", "Core", "Carved surface",
        icon("Core", """
          <radialGradient id="orb" cx=".32" cy=".22" r=".85"><stop stop-color="#ffe0b5"/><stop offset=".35" stop-color="#f6b986"/><stop offset=".72" stop-color="#df9469"/><stop offset="1" stop-color="#ad654d"/></radialGradient>
          <linearGradient id="groove" x1=".1" y1="1" x2=".8" y2=".1"><stop stop-color="#9a533f"/><stop offset=".6" stop-color="#a35d43"/><stop offset="1" stop-color="#c47e54"/></linearGradient>
          <linearGradient id="lip" x1="0" y1="1" x2="1" y2="0"><stop stop-color="#f4b286" stop-opacity="0"/><stop offset=".35" stop-color="#ffdbab"/><stop offset=".8" stop-color="#ffdfb1"/><stop offset="1" stop-color="#ffdfb1" stop-opacity="0"/></linearGradient>
          <clipPath id="orb-clip"><circle cx="256" cy="249" r="141"/></clipPath>
        """, """
          <ellipse cx="258" cy="397" rx="102" ry="15" fill="#000" opacity=".25" filter="url(#soft)"/>
          <circle cx="256" cy="249" r="141" fill="url(#orb)" filter="url(#lift)"/>
          <g clip-path="url(#orb-clip)" fill="none" stroke-linecap="round">
            <path d="M137 314C172 269 212 243 255 258 303 276 339 251 371 200" stroke="#da9164" stroke-width="34"/>
            <path d="M137 308C172 263 212 237 255 252 303 270 339 245 371 194" stroke="url(#groove)" stroke-width="24"/>
            <path d="M139 321C177 276 211 252 254 268 303 286 344 259 374 209" stroke="url(#lip)" stroke-width="4.5"/>
            <path d="M148 204C162 166 196 141 233 137" stroke="#ffebcc" stroke-opacity=".3" stroke-width="3"/>
          </g>
        """),
    ),
]


def render(svg: str, output: Path, width: int, height: int | None = None) -> None:
    ET.fromstring(svg)
    height = width if height is None else height
    handle = Rsvg.Handle.new_from_data(svg.encode("utf-8"))
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, width, height)
    context = cairo.Context(surface)
    viewport = Rsvg.Rectangle()
    viewport.x = viewport.y = 0
    viewport.width, viewport.height = width, height
    if not handle.render_document(context, viewport):
        raise RuntimeError(f"Could not render {output}")
    surface.write_to_png(str(output))


def sheet() -> str:
    parts = ['''<svg xmlns="http://www.w3.org/2000/svg" width="1800" height="740" viewBox="0 0 1800 740">
      <rect width="1800" height="740" fill="#121418"/>
      <g font-family="DejaVu Sans, sans-serif">
      <text x="40" y="62" fill="#f1e8e0" font-size="30" font-weight="bold">Klay</text>
      <text x="130" y="61" fill="#969aa3" font-size="17">/ App icon proposals</text>
      <text x="1760" y="60" text-anchor="end" fill="#747984" font-size="12" letter-spacing="1.5">01 — 05</text>
    ''']
    for i, (_, name, description, svg) in enumerate(CONCEPTS):
        x = 40 + i * 348
        uri = "data:image/svg+xml;base64," + base64.b64encode(svg.encode()).decode()
        parts.append(f'''
          <rect x="{x}" y="108" width="328" height="568" rx="24" fill="#1c1f25" stroke="#2b2f37"/>
          <text x="{x + 22}" y="146" fill="#7d8591" font-size="12" letter-spacing="1">0{i + 1}</text>
          <image href="{uri}" x="{x + 20}" y="156" width="288" height="288"/>
          <text x="{x + 164}" y="483" text-anchor="middle" fill="#f0e8e1" font-size="21" font-weight="bold">{name}</text>
          <text x="{x + 164}" y="511" text-anchor="middle" fill="#959ca7" font-size="13">{description}</text>
          <path d="M{x + 22} 539H{x + 306}" stroke="#30343c"/>
          <image href="{uri}" x="{x + 68}" y="559" width="64" height="64"/>
          <image href="{uri}" x="{x + 208}" y="575" width="32" height="32"/>
          <text x="{x + 100}" y="650" text-anchor="middle" fill="#747e8b" font-size="11">64 px</text>
          <text x="{x + 224}" y="650" text-anchor="middle" fill="#747e8b" font-size="11">32 px</text>
        ''')
    parts.append('</g></svg>')
    return "".join(parts)


def main() -> None:
    for slug, _, _, svg in CONCEPTS:
        (ROOT / f"{slug}.svg").write_text(svg, encoding="utf-8")
        for size in (32, 64, 128, 512, 1024):
            render(svg, ROOT / f"{slug}-{size}.png", size)
    comparison = sheet()
    (ROOT / "comparison.svg").write_text(comparison, encoding="utf-8")
    render(comparison, ROOT / "comparison.png", 1800, 740)
    print(f"Rendered five SVG proposals, 25 PNG exports, and comparison sheet in {ROOT}")


if __name__ == "__main__":
    main()
