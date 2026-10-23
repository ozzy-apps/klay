# Changelog

## 0.6.2 — 2026-10-23

- Fixed Clip Line squashing the model with X symmetry: the mirrored plane was applied to the whole model, so in any view turned or panned off the symmetry plane it cut deeper than the guide (a cut drawn at 77 % of the height took it to 59 %), more with every clip. Now the half under the line is clipped by the guide's plane and the other half by its mirror image; vertices on the symmetry plane stay on it.
- Holding Ctrl shows the Ctrl mask brush in the brush panels, and Shift shows Smooth, as in ZBrush; a mask brush picked while Ctrl is held becomes the Ctrl mask brush.
- Verification: 160 release tests (new: symmetric clip in a turned view against the guide and mirror-symmetric, three clips at levels 0–2 leave the kept side and the levels exact, held Ctrl/Shift brush display through the real UI); --selftest-surface, --selftest-brushes, --selftest-mask pass on the GPU.

## 0.6.1 — 2026-10-23

- Fixed a crash with Grab or Snake Hook when the brush is smaller than the polygons under it: the press picked up no vertex and the GPU grab bound empty buffers, which wgpu rejects. Such a stroke now does nothing (no undo step).
- Verification: 156 release tests (new: drag brushes smaller than a polygon, CPU and GPU paths); --selftest-brushes now ends with tiny Grab and Snake Hook strokes on the GPU.

## 0.6.0 — 2026-10-23

- CurveTube brush (ZBrush: Brush › CurveTube): draw a curve on the model and, on release, a closed round tube is built along it and added to the active subtool as a new polygroup (dreadlocks, cables, straps). Its centre runs on the surface, so the root sits half inside the model; where the stroke runs off the model it carries on at the depth where it left, so a tube can hang free. Rings are as far apart as a side is wide, carried along the curve without twisting, with round caps at both ends. X symmetry adds the mirror tube in the same polygroup.
- The curve stays live: drag it to bend the tube (pulled points snap to the model under them, or stay in the air off it); changing Draw Size, Tube taper or Tube sides (Stroke › Curve) rebuilds it. Esc or any other edit lets it go. One undo step per tube and per bend; undo keeps only the tube, not a copy of the model.
- Like ZBrush's insert brushes it needs a subtool without subdivision levels, and says so otherwise. The tube is part of the mesh: it saves, exports, divides and can be Wedged with the rest.
- Catalog: 34 tools, new Curve filter. The tube is swapped in place, on the CPU (cut and extend the model's arrays and tables, never rebuild them) and on the GPU (buffers get room once, then only the tube's slice is written): a bend re-builds in 5–6 ms on a 966k-vertex model, against 78 ms rebuilding the model.
- Verification: 155 release tests (tube shape, closed and outward winding, no twist, append/cut/swap identical to a full rebuild, input path with bend/resize/undo/redo/project round trip/refusal); new --selftest-tube (nine tubes, a bend, GPU readback exact, undo/redo of all of them) at 4k and 966k vertices, plus every earlier native self-test.

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
