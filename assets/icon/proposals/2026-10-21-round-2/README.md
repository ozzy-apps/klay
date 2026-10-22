# Klay icon proposals — round 2

Five new proposals focused on Klay as a digital sculpting application. The user
explicitly asked to disregard Sessions. This round explores material, carved
surfaces, head studies, tool use, and the deformation of a solid form.

## Proposals

| No. | Name | Idea | Final image |
| --- | --- | --- | --- |
| 1 | Gouge | A broad subtractive cut and a curled shaving in a clay block. | `01-gouge.png` |
| 2 | Gesture | An abstract clay sculpture with a strong negative-space opening. | `02-gesture.png` |
| 3 | Study | A head study transitioning between broad sculpting planes and a smooth surface. | `03-study.png` |
| 4 | Pull | A solid cube reshaped by a Grab-style pull of its surface. | `04-pull.png` |
| 5 | Loop | A compact loop tool carving a thick shaving from terracotta clay. | `05-loop-v2.png` |

Open `comparison.png` for large, 64 px, and 32 px presentations of all five.
The rounded corners are presentation masks; the original images are preserved.

## Method and source files

- Created with the built-in **imagegen** tool, one independent generation per proposal.
- Every original is a 1254×1254 PNG with its own opaque background.
- `prompts.json` records the complete five generation prompts.
- `refinement.json` records the targeted edit that shortened the Loop tool handle to fit within the icon.
- `05-loop.png` is the initial Loop draft; **`05-loop-v2.png` is the final proposal** shown in the comparison.
- `sources.json` records the original generated-image paths and the corresponding project copies.
- `comparison.svg` is a layout document referencing the PNGs, not vector versions of the generated artwork.
- `render_comparison.py` reproduces the comparison sheet using system librsvg/Cairo. The source image pixels are not retouched.

```bash
python3 assets/icon/proposals/2026-10-21-round-2/render_comparison.py
```

The earlier proposals remain in `../2026-10-21/`. No final app icon has been selected
or installed. After selection, refine the chosen design and prepare its production
icon sizes and packaging.
