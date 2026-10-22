# Klay readability proposals

Three design references for the native sculpting workspace. Preserve the ZBrush-inspired menu palettes, docking trays, fixed brush/material shelf and central dark viewport. Material 3 informs typography, tonal surface levels, selected states and restrained rounding. UI text is functional.

| Proposal | Main change | Body labels | Tray widths |
| --- | --- | --- | --- |
| 01 Graphite | Compact three-column brush grid; brighter labels and separated values | 13 px | 258 / 276 px |
| 02 Slate | Larger two-column brush grid; 14 px controls and stronger tonal grouping | 14 px | 280 / 296 px |
| 03 Light Studio | Slate's larger layout with light panels around a dark canvas | 14 px | 280 / 296 px |

Each proposal has a 1600×1000 PNG, an editable self-contained SVG and a full-scale controls detail PNG. `comparison.png` shows all three simultaneously. `contrast.json` records computed text/surface contrast for specified color pairs; this is a color check, not an accessibility certification. Primary labels exceed 12:1 and secondary labels on panel backgrounds exceed 7:1 in all three concepts. Menu, shelf, status and shortcut labels have separate sizes appropriate to their hierarchy.

The sphere render and sphere thumbnails reuse `ui-native.png`; new UI geometry and text are SVG. The approved Coral Fold icon comes from `assets/icon/klay-64.png`. This keeps the same scene and existing brush imagery for comparison. The SVGs embed both images and need no runtime asset lookup. Inter is the preferred font, with DejaVu Sans as fallback; numeric fields use JetBrains Mono.

Reproduce with the system Python and installed librsvg/Cairo:

```sh
/usr/bin/python3 design/readability/2026-10-22/generate_mockups.py
```

`comparison.fragment.html` is the editable comparison shell. The generator inserts shared image definitions and all three mockups into the task's approved visualization directory. Its only interactions select a concept, toggle fit/100% sizing and navigate concept tabs with the keyboard. It makes no network requests. The application controls inside the drawings are design references.

Verification: all three SVGs parsed and rendered with librsvg; full layouts and control details visually inspected; local comparison handlers exercised for concept switching, zoom and keyboard navigation. Browser connection was unavailable, so no real-browser interaction verification is claimed.

These files do not change the native application or the user's persisted workspace. After the user chooses a direction, port the tokens and widget geometry into `src/workspace.rs`, preserve docking and canvas input coordinates, and run the existing native release tests plus visual inspection. The application remains Rust, egui, winit and wgpu.
