# Changelog

## 0.5.0 — 2026-10-22

- Curve strokes (ZBrush Stroke › Curve): the stroke is drawn as a path and, on release, smoothed, laid onto the surface and applied along it. Zipper, Stitch and Rake start in Curve mode and are laid out in the curve's own frame, so a shaky hand gives even, square teeth; other dab brushes step evenly along it. The curve stays as a dashed line and can be dragged to bend it, re-applying the brush along the new shape; Esc lets it go. Freehand / Curve per brush on the left shelf stroke thumbnail and in Stroke › Curve, with Curve smooth and Curve step.
- Pen pressure on X11 (XInput2, any pen with an "Abs Pressure" axis, e.g. XP-Pen, Wacom, Huion): Draw Size and Z / RGB Intensity follow the pen, linear like ZBrush's default curves, with Size and Z Intensity toggles and a live readout in Brush › Tablet pressure. Drag brushes and the detail reliefs keep their size; a mouse strokes at full strength.
- Merged with the polygroup, primitive, Wedge and PolyF work from main (0.4.0 was built on it).
- Verification: 148 release tests; --selftest-curve (a shaky stroke's curve lies within 0.24 % of Draw Size of a steady stroke's in the middle; its Zipper strays 6–8 % from the steady line's, against 46 % freehand; bend, GPU readback, undo/redo exact) and --selftest-pressure, plus every earlier native self-test, on Xvfb/llvmpipe.

## 0.4.0 — 2026-10-22

- Expanded the library to 33 tools with Mask Pen, Mask Soft, Mask Rect, Mask Lasso, Mask Circle, Mask Line, Clip Line, FormSoft, FormHard, Zipper, Stitch and Rake.
- Connected real mask brushes and shape guides, direct masking, remembered Ctrl mask selection, alpha/focal sampling for pen masks, Alt erase, and existing mask undo/subdivision/project persistence.
- Added topology-preserving Clip Line: drag from background or surface, preview the affected side, Alt flip, Shift angle snap, Escape cancel. Masks, hidden regions, paint and multires undo are retained.
- Added continuous, distance-spaced Zipper/Stitch/Rake relief on existing vertices and soft/hard form buildup. Layer is now directly visible in the side panel.
- Added compact Sculpt/Mask tabs that restore the last tool, a functional Masking palette picker, searchable catalog and All/Sculpt/Mask/Detail/Clip filters, with original clay/mask previews.
- Verification: 130 release tests, native input/GPU/undo/redo checks for all six masks and Clip Line, and expanded brush checks including all new sculpt/detail tools. Native visual/package results are tracked in handoff.md.

## 0.3.0 — 2026-10-22

- Compacted the side Brush palette into one larger active preview, two recent picks and twelve small paired brush cells. All Brushes / B opens the complete library; adding brushes no longer expands the side panel.
- Expanded the library from 13 to 21 brushes: Blob, Layer, Fill, Scrape, Nudge, Sharpen, Elastic and Snake Hook. Existing 1–9 shortcuts are preserved.
- Added stroke-local capped Layer buildup, directional Nudge/Elastic deformation and tapered Snake Hook using the GPU Grab kernel; all tools integrate with masks, hidden geometry, images and undo.
- Added distinct original clay previews and functional hover descriptions. The larger catalog adapts to supported window sizes.
- Verification: 117 release tests; all eight new tools pass native canvas, GPU readback and undo/redo checks at 240k vertices. Compact palette/catalog checked at 1600×1000, 1280×800 and 960×640. Added --selftest-brushes for repeatable native verification; details are recorded in handoff.md.

## 0.2.0 — 2026-10-22

- Expanded from five to 13 working brushes: Dam Standard, Pinch, Clay, Clay Buildup, Crease, Trim Dynamic, Polish and Paint.
- Added built-in alphas/textures, PNG/JPEG/BMP imports, persistent image library, relief and tiling controls.
- Connected Focal Shift, RGB Intensity and Z / RGB / Z+RGB modes; texture painting respects masks, visibility and symmetry.
- Added per-level vertex paint with exact undo/redo, subdivision, parked-subtool and Wedge support. Project format 4 retains paint and still opens formats 1–3.
- Rebuilt previews as original clay studies in Klay's colors. Added the ZBrush-style Quick Pick/alphabet/grid catalog with large hover preview, available with B.
- Reordered the Space menu: Draw Size, Z Intensity, Focal Shift, RGB Intensity, brushes, with a color picker at the left.
- Added --version, Linux desktop/executable icon installation and versioned AppDir packaging using the Coral Fold app icon.
- Verification: 111 release tests, native GPU paint/undo/redo readback, polygroup rendering and project round trips, native PNG import, and layout checks at 1600×1000 and 960×640. Detailed final results are recorded in handoff.md.

## 0.1.0 — baseline

- Native Rust sculpting application with multiresolution meshes, subtools, masks, polygroups, symmetry, transforms, project persistence and the ZBrush-style dockable workspace.
- Embedded Coral Fold branding and About dialog.
- Readability touch-up: brighter labels, larger text and clearer slider values.

Versions are maintained from 0.2.0 onward. Earlier work was developed under 0.1.0.
