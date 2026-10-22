#!/usr/bin/env python3
"""Lay out the original generated proposals without retouching their pixels.

Rounded corners and size previews are presentation masks only. Source PNGs remain
exact copies of image_gen outputs. Requires system librsvg/PyGObject and Cairo.
"""

import json
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg

ROOT = Path(__file__).resolve().parent
items = json.loads((ROOT / "sources.json").read_text())
descriptions = ["Carved clay", "Abstract sculpture", "Head study", "Brush deformation", "Loop-tool carving"]
parts = ['''<svg xmlns="http://www.w3.org/2000/svg" width="1800" height="740" viewBox="0 0 1800 740">
<rect width="1800" height="740" fill="#161a1d"/>
<g font-family="DejaVu Sans, sans-serif">
<text x="40" y="61" fill="#f0eae1" font-size="29" font-weight="bold">Klay</text>
<text x="132" y="60" fill="#a2acae" font-size="17">/ Sculpting app icons</text>
<text x="1760" y="59" fill="#7f8b90" font-size="12" text-anchor="end" letter-spacing="1.8">ROUND 02</text>
''']
for i, item in enumerate(items):
    x = 40 + 348 * i
    uri = item["file"]
    parts.append(f'''
      <defs>
        <clipPath id="large-{i}"><rect x="{x+20}" y="156" width="288" height="288" rx="57"/></clipPath>
        <clipPath id="medium-{i}"><rect x="{x+68}" y="559" width="64" height="64" rx="13"/></clipPath>
        <clipPath id="small-{i}"><rect x="{x+208}" y="575" width="32" height="32" rx="6.5"/></clipPath>
      </defs>
      <rect x="{x}" y="108" width="328" height="568" rx="24" fill="#20262b" stroke="#343e43"/>
      <text x="{x+22}" y="145" fill="#93a0a6" font-size="12" letter-spacing="1">0{i+1}</text>
      <image href="{uri}" x="{x+20}" y="156" width="288" height="288" clip-path="url(#large-{i})"/>
      <text x="{x+164}" y="483" text-anchor="middle" fill="#f0eae1" font-size="21" font-weight="bold">{item['name']}</text>
      <text x="{x+164}" y="511" text-anchor="middle" fill="#a0acb2" font-size="13">{descriptions[i]}</text>
      <path d="M{x+22} 539H{x+306}" stroke="#3b454b"/>
      <image href="{uri}" x="{x+68}" y="559" width="64" height="64" clip-path="url(#medium-{i})"/>
      <image href="{uri}" x="{x+208}" y="575" width="32" height="32" clip-path="url(#small-{i})"/>
      <text x="{x+100}" y="650" text-anchor="middle" fill="#8b999f" font-size="11">64 px</text>
      <text x="{x+224}" y="650" text-anchor="middle" fill="#8b999f" font-size="11">32 px</text>
    ''')
parts.append('</g></svg>')
svg = "".join(parts)
ET.fromstring(svg)
(ROOT / "comparison.svg").write_text(svg)
handle = Rsvg.Handle.new_from_file(str(ROOT / "comparison.svg"))
surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, 1800, 740)
viewport = Rsvg.Rectangle()
viewport.x = viewport.y = 0
viewport.width, viewport.height = 1800, 740
assert handle.render_document(cairo.Context(surface), viewport)
surface.write_to_png(str(ROOT / "comparison.png"))
print("Rendered comparison sheet with large, 64 px, and 32 px presentations.")
