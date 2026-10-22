# Readability round 2 — no Material 3

The user's latest instruction explicitly removes Material 3 as a guide. These are three different interface compositions, with square geometry and distinct control styles. Earlier proposals remain in the adjacent `2026-10-22` directory.

| Direction | Composition | Controls |
| --- | --- | --- |
| Classic | Twin trays, compact brush matrix, fixed brush/material and navigation shelves | Inset square buttons, amber selections, bar sliders with separate numeric compartments |
| Precision | Narrow mode rail, larger canvas, tabbed right inspector, horizontal brush rack | Technical labels, monospaced numeric fields, ruler tracks and rectangular handles |
| Atelier | Slim brush list, wide canvas, one open inspector | Larger type, text actions, fine rules, inline values and minimal enclosing boxes |

Files for each direction: a 1600×1000 PNG, an editable self-contained SVG and a full-scale inspector/control-detail PNG. `comparison.png` stacks all three at full size. The selected Coral Fold branding and the existing native render/brush/material imagery are preserved. All new interface geometry and text are authored in SVG. Rectangular interface geometry has zero corner radius.

The application remains native Rust/egui/wgpu; these are design references. No application source, executable or saved workspace is changed by generating them. Controls inside the drawings are illustrative. The conversation comparison only switches the concept and fit/100% sizing.

Reproduce with the installed system Python, librsvg and Cairo:

```sh
PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 design/readability/2026-10-22-non-material/generate_mockups.py
```

The generator reuses only the raw SVG drawing helpers, native imagery embedding, contrast calculation and librsvg renderer from the previous round. It does not reuse that round's layout or Material-style widgets. `comparison.fragment.html` is literal editable markup; the generated conversation comparison is written into the approved visualization directory outside the repository. No network resources are used.

Verification: all three SVGs parsed/rendered; square rectangle geometry asserted; complete previews and control details visually inspected; supplied primary, secondary and selected color pairs checked in `contrast.json`; concept-switch and fit/100% handlers exercised against a minimal local DOM model. This is not a full accessibility or live-browser audit.

If the user selects a direction for implementation: port the chosen composition and control grammar into `src/workspace.rs`, preserve docking and canvas coordinate handling, retain unavailable states for unimplemented engine controls, run existing release tests and inspect the native app. Do not treat a mockup choice as authorization to discard saved layouts.
