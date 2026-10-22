# Klay Fold — color variants

The original first-round Fold icon in six colors: **Mint, Blue, Violet, Coral,
Amber, and Rose**. Mint is the original; the other five recolor only the K.

**Coral is the selected final color.** Canonical assets are now in `../../klay.svg`
and the adjacent `klay-*.png` exports.

The letterform, folded detail, gradient positions, highlights, shadows, charcoal
tile, and proportions are identical in every version. No icon has been installed.

## Files

- `comparison.png` — all six colors with 64 px and 32 px previews.
- `comparison.svg` — editable comparison sheet.
- `01-mint.svg` through `06-rose.svg` — self-contained vector masters.
- Each variant has transparent PNG exports at 32, 64, 128, 512, and 1024 px.
- `palettes.json` — color values, color roles, and the original source checksum.

`01-mint.svg` is a byte-for-byte copy of `../2026-10-21/03-fold.svg`.
The original file and previous proposal rounds remain unchanged.

Rebuild using system librsvg/Cairo:

```bash
python3 assets/icon/proposals/2026-10-21-fold-colors/render_colors.py
```

The script verifies that every variant has the same geometry and rendering
attributes as the source, allowing only mark-color and accessible-title changes.
