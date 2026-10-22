# Klay app icon proposals

Five proposals, created 2026-10-21. No final icon has been selected or installed.

Open `comparison.png` for the complete set, including 64 px and 32 px previews.

| No. | Proposal | Direction | Vector master |
| --- | --- | --- | --- |
| 1 | Sculpt | A segmented, warm clay K; closest to the Sessions icon's visual language. | `01-sculpt.svg` |
| 2 | Strata | Nested contours suggesting sculpted volume and surface depth. | `02-strata.svg` |
| 3 | Fold | A continuous mint K with a folded surface and soft edges. | `03-fold.svg` |
| 4 | Imprint | A debossed K in terracotta, on an ivory tile. | `04-imprint.svg` |
| 5 | Core | A shaded clay sphere with a sweeping carved groove. | `05-core.svg` |

All five are original SVG artwork. The installed Sessions icon was inspected as a
reference for its charcoal tile, warm accents, geometric clarity, and restrained depth.
These are vector proposals, not AI-generated raster images.

## Files

- Each SVG has a 512×512 viewBox with a 448×448 rounded tile and transparent outer margins.
- Every proposal has transparent PNG exports at 32, 64, 128, 512, and 1024 px.
- `comparison.svg` is the editable comparison sheet; `comparison.png` is its 1800×740 render.
- The icons contain no labels; names and size labels only appear on the comparison sheet.

## Rebuild

From the Klay project root:

```bash
python3 assets/icon/proposals/2026-10-21/render_proposals.py
```

Rendering uses the system PyGObject/librsvg and Cairo. It does not add application dependencies.
`render_proposals.py` contains all vector geometry, colors, gradients, and sheet layout.
After an icon is selected, refine that proposal and prepare application packaging sizes separately.
