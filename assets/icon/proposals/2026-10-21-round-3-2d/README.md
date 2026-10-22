# Klay — flat 2D K directions

The user specified a **2D logo**, identified the earlier green Fold K as the closest
direction, and allowed different colors. This round refines that direction using
editable SVG artwork with solid fills and transparent negative space.

| No. | Mark | Shape | Palette |
| --- | --- | --- | --- |
| 1 | Soft | Original green K outline, flattened to one solid color. | Mint `#96D7B4` / forest `#193B35` |
| 2 | Cut | Sharper diagonal arms with restrained corner rounding. | Cream `#FFF4DA` / cobalt `#3B53DE` |
| 3 | Sweep | A curved lower leg and asymmetric terminals. | Coral `#DB5B49` / peach `#FFEBDF` |
| 4 | Slice | The Soft silhouette with an open diagonal cut. | Violet `#6952B5` / lavender `#EDE7FF` |
| 5 | Counter | The Soft silhouette reversed out of a circular mark. | Ink `#24372D` / lime `#DCE87C` |

Shapes and palettes can be combined independently. No final design has been selected
or installed; earlier proposal rounds and application files are unchanged.

## Files

- `comparison.png` — the five directions with monochrome and 64/32 px previews.
- `comparison.svg` — editable comparison sheet.
- `01-soft.svg` etc. — app-tile SVGs, with transparent outer margins.
- `01-soft-mark.svg` etc. — standalone logos on transparent backgrounds.
- `01-soft-mono.svg` etc. — one-color standalone SVGs.
- Each app tile has 32, 64, 512, and 1024 px PNG exports.
- Each standalone logo has a 512 px transparent PNG.

The logo SVGs contain no gradients, lighting, shadows, bevels, textures, raster images,
or font dependencies. Letterforms are paths; the open/reversed variants use vector masks.

Rebuild the set with system librsvg/Cairo:

```bash
python3 assets/icon/proposals/2026-10-21-round-3-2d/render_proposals.py
```

All geometry and palette values are defined in that script. No application dependencies were added.
