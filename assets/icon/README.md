# Klay icon

**Selected design: Coral Fold**, approved by the user on 2026-10-21.

- `klay.svg` — canonical scalable master.
- `klay-32.png`, `klay-64.png`, `klay-128.png`, `klay-256.png`, `klay-512.png`, `klay-1024.png` — transparent PNG exports.

The canonical master is an exact copy of the approved
`proposals/2026-10-21-fold-colors/04-coral.svg`. Preserve its proportions, fold,
gradients, highlights, shadows, and charcoal tile.

Palette: body `#ffdbc9` → `#f2a58c` → `#cf6b5a`; fold `#b85648` → `#df9077` →
`#fbd2bb`; rim `#fff4e9`; edge highlight `#ffe0cc`.

Earlier explorations remain in `proposals/` for reference. Coral Fold is the
current approved identity; the other proposals are not selected alternatives.

## Application integration

`src/branding.rs` embeds `klay-512.png` in the executable and shares it with the
native window icon, the top-left Klay button, and the About dialog. No asset files
are required beside the compiled executable. The SVG remains the editable master.
