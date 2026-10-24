# Changelog

## 0.12.0 — 2026-10-24

- Gizmo 3D (Y): axis arrows, axis rings, view ring, free move and even scale, with live preview, one undo step, masks respected.
- Mesh › Layers: New (recording), REC, show/hide, Intensity, Delete, Bake, Bake All; every change undoable; layers stay with parked subtools (not saved in projects yet).
- Brush › Tablet pressure curves for Size and Intensity, with presets and live pressure.
- Edit › History: named steps; click to undo or redo to any of them.
- Preferences › QuickSave: rotating QuickSave_1–5.klay every N minutes when idle and changed, and QuickSave now.
- New --gizmo flag.
- Verification: 222 release tests (new: gizmo hit testing, drag maps and the input path; layer record/intensity/eye/delete/bake with undo and parking, layer state swaps; pressure curve shape and editing, curves in the brush; history names and jumps; QuickSave rotation); every native self-test passes on the GPU.

## 0.11.0 — 2026-10-24

- The system pointer hides over the model while the brush ring is shown or a stroke is under way, as in ZBrush; a dot marks the brush centre. It shows over the interface, empty canvas, border and while drawing shape gestures.
- Document › Import OBJ as a new subtool (any polygons, groups as polygroups, vertex colours as polypaint, Fit to size).
- Mesh › Deformation: Smooth, Polish, Inflate, Spherize, Noise, Twist and Taper, undoable, masks respected.
- Mesh › Morph Target (StoreMT, Switch, DelMT) and the Morph brush (catalog: 47 tools).
- Mesh › SubTool: Merge Down, Split Hidden, Groups Split and Extract (with Thick).
- Stroke: DragRect and DragDot for every dab brush; the stroke thumbnail cycles the modes.
- View › Camera › Angle of view.
- Verification: 212 release tests (new: every deformation shape, masks and undo across levels; OBJ parsing, Klay export round trip and import as a subtool; Morph brush and Switch through the input path; merge, split, extract and their refusals; closed outward extract shells; DragRect sizing from the pre-stroke surface and DragDot sliding; the pointer rule); every native self-test passes on the GPU.

## 0.10.0 — 2026-10-24

- Canvas border: models are drawn only inside the document frame, as in ZBrush; the border around it is always empty canvas, so there is somewhere to rotate, move and zoom however far you zoom in.
- Document palette: Save As, Revert (asks first) and Snapshot (the canvas inside the frame as a PNG, without the interface); Back: background colour with ZBrush's vertical gradient (Range, Center, Rate) and the Border colour, kept with the workspace.
- Brush › Auto Masking: BackfaceMask and Topological, for every sculpt, drag, detail and paint brush.
- Stroke › Modifiers: Spacing, and LazyMouse (L) with LazyRadius and the lazy string drawn on the canvas. Strokes now follow the pointer point by point, so fast strokes leave no gaps.
- Masking: Grow, Shrink, Mask By Cavity (sensitivity) and Mask By Polygroups (group borders, width).
- Mesh › Visibility: Grow and Shrink. Mesh › Polygroups: Auto Groups (one per connected piece) and Group By Normals (Max Angle).
- Symmetry: Y and Z mirrors and Radial about X, Y or Z with RadialCount, combined freely (up to 64 copies), for every brush (each copy dabs in full, so overlapping copies no longer tear the surface; drag brushes add up overlapping copies on the CPU and stay on the GPU otherwise), masks, mask shapes, curve reliefs, CurveTube and the brush ring. Clip Line stays X only.
- New flags: --selftest-document, --selftest-symmetry, --symmetry, --snapshot.
- Verification: 200 release tests (new: document look encode/decode and gradient, auto masking, spacing and LazyMouse through the input path, mask grow/shrink/cavity/group borders, visibility grow/shrink, auto groups, group by normals, every symmetry combination for brushes, GPU grab, masks and tubes, overlapping copies stay whole); every native self-test passes on the GPU.

## 0.9.0 — 2026-10-24

- Wedge is now called **Remesh** everywhere you see it: the Mesh › Remesh section, its Remesh (on/off) and Update buttons, notices and status lines. `--selftest-remesh` and `--remesh-resolution` are the new flags (the old ones still work); project and layout files are unchanged.
- SubTool › Save / Load / Export OBJ: a path field under the SubTool list. Save writes the active subtool alone with all its levels (a one-subtool `.klay` that Document › Open also reads); Load adds every subtool of a `.klay` to the end of the list, parked, without changing the active one (a bad file adds nothing); Export OBJ writes the active subtool at its shown level next to the path.
- PolyF on dense meshes: lines are lighter, stay drawn down to polygons about 3 px across, and below that blend into an even tint at the frame's average darkness (at most 42 %), so the frame stays visible from a distance instead of fading out; it no longer shimmers in between.
- Draw Size sliders use a gentle curve (0.01 + 1.99 · position²) instead of a logarithmic one: same 0.01–2.0 range, half the slider is about 0.5 and the last tenth covers under a fifth of the range (it was 40 %).
- Ctrl + drag on the background with nothing to clear and Remesh off now says that it remeshes while Remesh is on (it already did, as ZBrush's re-DynaMesh).
- New `--polyf` flag starts with PolyF on.
- Verification: 183 release tests (new: subtool save, load into another model with levels and sculpting exact, bad file, OBJ of the active subtool; Draw Size curve; the remesh hint); every native self-test passes on the GPU.

## 0.8.0 — 2026-10-24

- Z Intensity, Focal Shift, RGB Intensity and Zadd/Zsub are now per brush, as in ZBrush (Maxon: every Draw palette setting except Draw Size is stored per tool). Switching brushes restores that brush's values; Draw Size stays shared. Smooth (and SmoothValleys/SmoothPeaks) and the mask brushes start at full intensity, other brushes at 0.5. Values last for the session.
- Shift smoothing uses Smooth's own Z Intensity (before, it used the sculpt brush's), and holding Shift shows and edits Smooth's values in the panels. Ctrl masking and mask shapes use the mask brush's own intensity instead of the sculpt brush's.
- Verification: 181 release tests (new: settings follow the brush with Draw Size shared; Shift shows and sets Smooth's intensity through the held-brush path and a UI frame; Ctrl masking at the mask brush's intensity, not the sculpt brush's); every native self-test passes on the GPU.

## 0.7.0 — 2026-10-23

- Twelve of ZBrush's advanced brushes (catalog: 46 tools): ClayTubes (square, flat-topped clay strips that turn with the stroke), hPolish (flatten with a hard square edge), TrimAdaptive (trim to the plane under the brush centre), Planar (flatten onto the plane where the stroke started), TrimSmoothBorder (trim and soften the cut's edge), MalletFast (square, flat-bottomed dents; Alt raises a plateau), Move Topological (pull only the surface connected to the click point; GPU like Grab), Inflate Balloon (round swell), Magnify (spread and swell; Alt contracts), Orb Cracks (sharp crack of varying depth; Alt raises a ridge), SmoothValleys and SmoothPeaks (smooth only recessed or only raised areas). Each works through canvas input, undo/redo, X symmetry, masks, hidden faces, pen pressure, Curve strokes and subdivision levels, with a catalog preview and description.
- Seven MatCaps in the Material palette and left-shelf thumbnail: Red Wax, Skin, Gray, White, Form (high-contrast form reading), Chrome and Toy Plastic, next to the four procedural clays. Each is a pair of 256 px sphere images (diffuse, specular) painted by Klay and sampled on the GPU by the view-space normal; polypaint and polygroup colours tint the diffuse layer. Brush thumbnails use the current material.
- New --selftest-materials: draws every material on the GPU, reads the frame back at five points and compares each MatCap with the CPU lookup of its images (2–4/255), then paints red under a MatCap through the Paint brush and checks it tints.
- Verification: 178 release tests (17 new: every added brush across levels with undo/redo, masks and hidden faces, exact mirror symmetry, Move Topological leaving a nearby separate surface still and matching on the GPU, SmoothValleys/Peaks one-sided, Planar's locked plane, square footprints, Orb Cracks narrower and uneven; MatCap orientation, tint and texture layers); --selftest-brushes runs the twelve through the canvas and GPU, plus every earlier native self-test.

## 0.6.3 — 2026-10-23

- Fixed Clip Line pushing earlier cuts back out when cuts cross (with or without symmetry): a cut moved every vertex on its side straight onto its plane, and where the cut crossed an earlier cut face at a wide angle (a pie slice), vertices of that face and around its edge landed outside the model, past the earlier cut, visible once the model is turned. On a solid, a clipped vertex now lands inside the model's cross-section on the cut plane, or on the nearest point of its outline (outline segments in a grid; each edge's crossing computed in one order so the outline has no rounding gaps).
- New --selftest-clip (three crossing cuts through the real handlers, every vertex checked against every cut, view turned; --hold to look at it). A cut on a 960k-vertex sphere takes 13–35 ms.
- Verification: 162 release tests (crossing cuts in orthographic and perspective views, canvas sizes, turned views, 4k to 960k vertices); --selftest-clip at 4k and 960k vertices and --selftest-surface on the GPU.

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
