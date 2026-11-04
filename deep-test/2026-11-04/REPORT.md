# Klay 0.47.0 deep test — 2026-11-04

Build tested: the working tree as of tonight (0.47.0, uncommitted retopology work included).
Nothing in `src/` was changed. Testing ran from a scratch copy with an extra driver
(`deeptest.rs.txt` here). The driver feeds strokes and gestures through the real canvas handlers
(`cursor_moved` / `left_press` / `left_release`, Ctrl+Z via `pending_undo`). After each step it checks
the mesh (NaN, index range, degenerate and non-manifold edges, open edges, GPU copy vs CPU, per-vertex
array lengths), compares undo against a stored copy, and photographs the model from 8 angles:
default, front, right, back, left, top, bottom, back-three-quarter.

## What passed

- `cargo test --release`: **441 passed**, 0 failed, 14 ignored.
- All 23 GPU self-tests pass. `--selftest-cull` and `--selftest-splat` only pass at the mesh size they were written for (see flaws).
- Dense undo chains: 8.6M vertices (strokes → divide → L0 edit → L2 → clip, mask lasso and stroke → undo 3 / redo 3 / undo 20 / redo 20 / branch / undo 30). Every return to a stored state was exact (worst 0.0), with no crash.
- Remesh chain: Dynamesh 256 → stroke → ZRemesher 5k → divide ×2 → stroke → decimate 20% → UV unwrap → stroke, then single-step undo ×20, redo all and a branch. No crash; manifold throughout.
- 44 sculpt brushes with X symmetry on a 345k mesh: clean from all angles, and undo is exact for each.
- Clip / Trim / Slice / Knife / Select / Mask gestures on a sphere, a cube and a level-2 sphere, plus crossing clips in turned views: undo is exact everywhere.
- 12 Deformations, Mirror And Weld, Panel Loops, Crease PG + Divide, CurveTube, IMM insert, Layers, Sculptris Pro, Transpose, Live Boolean and Make Boolean Mesh: no crashes.

## Bugs found

**All six are fixed in 0.47.1** (see CHANGELOG). The fixed build was rerun through the same scenarios: no crash, the Retopology undo case included; finite framing with hidden polygons; a symmetric Clip Rect carve; a flat Clip Circle is a no-op; Close Holes is manifold and divisible; the config dir stays untouched.

1. **Crash: ZModeler / Retopology undo while hovering** *(this is your 21:59 crash: `main.rs:3651` index out of bounds, len 17 / index 18).*
   Reproduced on the first try: place a Retopo Quad, extrude a few edges, hover the newest point, press Ctrl+Z.
   `history_jump` (main.rs:7904) never clears `zm_hover`, and `paint_zmodel` (main.rs:3547) indexes `pm.pos` / `pm.faces` with it unchecked.
   The stale hover lives on for many frames after the undo. Plain ZModeler can hit it too.
2. **Black canvas whenever polygons are hidden and the view is framed.** `SculptMesh::visible_bounds` (mesh.rs:1299) merges rayon partial boxes with `grow(b.min); grow(b.max)`. An empty partial box (+inf/−inf) turns the result into ±infinity. `frame_camera` then sets a NaN target and infinite distance, so the whole canvas, background included, is black until Show All.
   Trigger: any Select Rect / Lasso / Ctrl+Shift hide followed by F, or anything that auto-frames. Evidence: `hidden-polygons-black-canvas.png`.
   The cull self-test's "half hidden" step can't catch this, because it compares two black frames.
3. **Clip Rect with X symmetry tears the mesh.** Torn, stair-stepped polygons and fins appear on the side, below and behind; they are invisible in the drawing view. It happens on the sphere and the cube; plain Clip Rect is clean. Evidence: `clip-rect-symmetry-sphere.png`, `clip-rect-symmetry-cube.png`.
4. **A flat Clip Circle (or Clip Rect) drag flattens the whole model** into a jagged sliver. A drag with zero height in one axis gives `radius.min_element() <= 1`, so `contains()` is false everywhere, which reads as "everything outside". Trim Circle and Mask Circle treat the same drag as a no-op, which is the right behaviour. Evidence: `clip-circle-flat-drag.png`.
5. **Close Holes makes a non-manifold edge** after Select Rect → Del Hidden (1 edge shared by >2 triangles). Divide is then refused ("has edges shared by more than two polygons"). Evidence: `extract-and-close-holes.png`, bottom row.
6. **Self-tests and snapshots overwrite the user's config.** `save_brush_assets` and the layout save run every frame even when `automated`. `--selftest-paint` leaves the default texture as Marble, so every later stroke got a pebbly grain. Running a self-test without a scratch `XDG_CONFIG_HOME` changes your real brush defaults.

## Flaws (works, but can be better)

1. **Smooth weakens as density rises.** It removed about 45% of a Draw bump at 9.6k vertices, 4.5% at 86k, 3% at 345k and 2% at 1.4M. At sculpting resolutions Smooth barely works. The strength (or iteration count) should scale with edge length relative to brush radius.
2. **Trim Rect / Trim Lasso / Make Boolean Mesh blow up density.** Sphere 86k → 516k / 540k vertices; cube 9.6k → 624k–645k; boolean of 307k + 8k triangles → 1.5M. The remesh resolution should follow the input's edge length.
3. **Close Holes fills with fans:** long thin spokes from one centre vertex, which give poor shading and poor divides. A grid or ear-clip fill with relaxation would be better.
4. **Extract border is jagged.** It follows the triangle stair-steps of the mask; ZBrush smooths it (S Smt).
5. **Clip Line with X symmetry leaves a visible seam/groove** on the cap at the symmetry plane (no hole: 0 open edges). Visible from the top and below only.
6. **ZModeler hover is only refreshed on pointer motion.** Besides the crash, the highlight can show a feature that no longer exists after undo, redo or a doc swap.
7. `--selftest-cull` / `--selftest-splat` **fail at the default 4k sphere** ("culling dropped chunks", "0 chunks drawn as points"). They need `--segments 400 --divide 2`. They should pick that size themselves or skip with a note.
8. The GPU copy lags the CPU by exactly one frame after undo/redo (worst 0.05–0.6 units on that frame). It's invisible at 60 fps; noted only because a frame-exact check sees it.

## Parity gaps (vs ZBrush defaults)

1. **Circle gestures** (Mask/Clip/Trim Circle) are an ellipse inside the drag's bounding box. ZBrush draws circles from the centre out.
2. **Del Lower / Del Higher clear the undo history**, so Dynamesh / ZRemesher steps before them can't be undone. ZBrush keeps them undoable.
3. Close Holes fill quality and Extract border smoothing (S Smt / Thickness controls), as above.
4. Clip Line symmetry is X only (known).
5. Smooth strength on dense meshes, as above.

## Planned next approach

1. **Fix the crash first:** clear `zm_hover` (and `zm_bridge`) in `history_jump`, `show_zmodel` and on every subtool switch, and bounds-check the hover at the top of `paint_zmodel`. Regression test: retopo quad → 3 extrudes → hover newest point → undo ×2 → paint.
2. Fix `visible_bounds` (merge with `min`/`max`) and make `frame_camera` ignore non-finite bounds. Unit test with hidden triangles across several chunks.
3. Treat degenerate Clip Circle / Rect outlines as "tiny" (no-op) like Trim and Mask.
4. Clip Rect + symmetry: apply the cross-section constraint per mirrored half, as Clip Line does since 0.2x. Add a symmetric-rect case to `--selftest-clip` that checks every vertex from turned views.
5. Close Holes: reject or repair pinch vertices before filling. Assert `subdivision_blocker() == None` after the op.
6. Gate config writes on `!automated`.
7. Smooth: scale per-dab iterations with radius / mean edge length. Trim and Boolean: derive the remesh resolution from input density.
8. Parity: circle-from-centre gestures; undoable Del Lower/Higher (store the packed stack like the remesh undo does).
9. Fold the deep-test driver into the repo as `--selftest-deep` (with scratch config), so every release runs these multi-angle checks.
