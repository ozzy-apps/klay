# Klay

A sculpting prototype in Rust + wgpu, exploring how far a chunked triangle mesh can go
before polygon count becomes the bottleneck.

## Run

```bash
cargo run --release                       # a 4 058-vertex sphere, ZBrush's starting resolution
cargo run --release -- --segments 400     # the 960k-vertex sphere Klay used to start with
cargo run --release -- --primitive cube   # start from Cube3D: sphere, cube, cylinder, ring (torus), plane
cargo run --release -- --primitive ring --segments 100      # resolution of the chosen primitive
cargo run --release -- --segments 1200    # 8.6M vertices
cargo run --release -- --segments 400 --demo                # apply scripted strokes on startup
cargo run --release -- --bench 1500       # headless CPU benchmark at 13.5M vertices
cargo run --release -- --segments 1500 --stress --novsync   # per-frame cost of a huge grab
cargo run --release -- --segments 400 --divide 2            # start two subdivision levels up (15.4M vertices)
cargo run --release -- --bench 400 --divide 2               # divide timings, base-level stroke, step-up cost
cargo run --release -- --segments 400 --divide 2 --novsync --frames   # average frame time at the top level, then exit
cargo run --release -- --subtools 4                         # start with four subtools in a row
cargo run --release -- --segments 400 --divide 2 --subtools 3 --selftest-subtools   # pick and switch subtools via the GPU path, log, exit
cargo run --release -- --open model.klay                    # open a saved project
cargo run --release -- --segments 400 --divide 2 --subtools 3 --selftest-project    # save, re-open, check byte-identical, log, exit
cargo run --release -- --segments 400 --divide 2 --novsync --selftest-transpose     # Transpose move / scale / rotate + undo through the input path, log, exit
cargo run --release -- --segments 400 --divide 2 --novsync --selftest-mask          # Ctrl masking, a masked move and undo through the input path, log, exit
cargo run --release -- --segments 200 --divide 1 --selftest-groups   # Ctrl + Shift rectangle, Ctrl + W, GPU draw and pick checks, undo, log, exit
cargo run --release -- --primitive cube --selftest-wedge             # Wedge on, Ctrl + drag re-wedge, GPU checks, undo, log, exit
cargo run --release -- --wedge-resolution 256                        # start with this Wedge resolution
```

`--segments N` sets the resolution of the starting primitive (`--primitive NAME`, Sphere by
default; ZBrush names such as `Cube3D` work too). `--bench N` builds the primitive at N
segments, 400 by default, so the benchmark numbers below keep their meshes. The numbers for
windowed runs below were taken with `--segments 400`.

`--budget N` caps the vertex count a Divide may reach (default 64 000 000, three divides of the
960k sphere); the Geometry section of the Mesh palette has the same slider. Each level costs
about 140 bytes per vertex of RAM and 52 bytes per vertex of GPU memory while it is shown.

### Primitives

Models start low and gain detail through Divide, as in ZBrush. Mesh → Initialize picks the
primitive and its resolution; Document → New, SubTool → Append and Initialize → Rebuild (which
replaces the active subtool in place) all use it. Each primitive keeps its own resolution, and
the slider shows the vertex count it gives.

| Primitive | Shape | Segments mean | Default (Low) | Dense |
| --- | --- | --- | --- | --- |
| Sphere3D | cube sphere, no poles | quads per cube edge | 26: 4 058 vertices | 400: 960 002 |
| Cube3D | box | quads per edge | 26: 4 058 | 400: 960 002 |
| Cylinder3D | box mapped to a cylinder, caps without a centre pole | quads per quarter turn | 24: 4 130 | 380: 1.0M |
| Ring3D | torus, outer radius 1, tube 0.25 | quads around the tube (3× around the ring) | 37: 4 107 | 577: 1.0M |
| Plane3D | square facing the front view | quads per side | 63: 4 096 | 999: 1.0M |

All of them are triangulated quad grids with interior vertices of valence 6, Loop
subdivision's regular case, so dividing them twice gives clean, even meshes. All are closed
except Plane3D, which has a border: Divide uses Loop's boundary rules there (border vertices
follow the border, the four corners stay put), so the plane keeps its square outline and
stays flat. It is drawn from the front only, like any open surface (back faces are culled).

## Workspace

The native egui workspace follows ZBrush's model. Every palette lives in the menu bar and opens
there as a dropdown; any palette can also be docked into the left, right, top or bottom tray.
Fixed shelves hold the live sculpt controls. The look is dark and flat with one clay accent;
`ui-mockup.html` is the interactive HTML reference it was ported from.

- **Menu bar**: click a palette name to drop it down. A dot marks palettes that are also docked.
- **Dock button** (the ◉ at the left of every palette header): click for a menu (left, right, top
  or bottom tray, or menu only), or drag it onto a highlighted canvas edge or between palettes in
  a tray. Click a docked palette's title to collapse it; sub-palettes collapse on their own.
- **Trays**: the thin dividers beside the canvas toggle the left and right trays. Top and bottom
  trays appear when something is docked there. Drag empty tray space with a pen or mouse to
  scroll.
- **Top shelf**: Sculpt mode, Zadd/Zsub, Z Intensity (strength), Draw Size (radius), Undo/Redo.
- **Left shelf**: the current brush and material as rendered sphere thumbnails (click for a
  picker), stroke/alpha/texture slots, and an HSV colour picker with swap.
- **Right shelf**: Frame, Floor grid, Symmetry, PolyF (polygroup colours), Solo, Detail
  shading, Hide interface, plus reserved toggles.
- **Tab** hides the whole interface; **Tab** or **Escape** restores it.
- Layout (placement, order, collapse state, sections, trays, tray width) saves automatically to
  `$XDG_CONFIG_HOME/klay/workspace.layout`, falling back to `~/.config/klay/workspace.layout`.
  Older `v1` layout files are ignored and replaced.

The five brushes, radius/strength, Add/Subtract, materials, floor grid, detail shading, view
presets, primitives (Mesh → Initialize), Wedge remeshing and Del Higher / Del Lower (Mesh →
Wedge, Geometry), polygroups and visibility (Mesh → Visibility and Polygroups, Ctrl + Shift
gestures, Ctrl + W, PolyF), masking (Ctrl, Masking palette), subdivision levels (Mesh →
Geometry: Divide, SDiv slider, vertex counts, vertex budget), undo/redo, OBJ export, the Move /
Scale / Rotate modes (Transpose action line) and live statistics are connected to the engine.
Paint modes, Zcut, Focal Shift, stroke options, alpha, texture, mask types (rect / lasso), the
Transform palette, symmetry and
the Persp/Local/Transp/Ghost/Solo toggles are drawn dimmed and marked **Not connected**.

Brush thumbnails are rendered on the CPU at startup: a sphere in the current material with the
stroke each brush produces (Draw and Inflate flip in Zsub). They re-render when the material
changes.

The default start is light (4k vertices); `cargo run --release -- --segments 400` gives the
960k-vertex sphere.

### Extending the UI

`src/workspace.rs` owns the theme, the `PaletteId` registry, `DockLayout`, the palette shell and
the palette contents. To add a palette, add a `PaletteId` variant with its name and default
place, and a `palette_body` arm built from `sec` sections and the widget helpers (`zslider`,
`seg`, `kv`, `brush_tile`, …). The shell supplies the menu-bar entry, dropdown, docking,
collapsing, scrolling and persistence. The workspace reads `Model` and returns `Actions`;
`App::ui` connects these to the sculpting engine. Canvas projection, viewport/scissor and picking
follow the available canvas rectangle.

Run the layout and pointer-interaction checks with `cargo test --release --offline`.

## Controls

| Input | Action |
| --- | --- |
| Left drag on the model | Sculpt with the current brush |
| Alt + left drag on the model | Alternate brush (carve), like ZBrush's ZSub |
| Shift + left drag | Smooth |
| Ctrl + left drag on the model | Paint a mask (Draw Size, Z Intensity, X symmetry); masked clay goes dark and stays still |
| Ctrl + Alt + left drag on the model | Erase the mask |
| Ctrl + click on the model / Ctrl + Alt + click | Blur / sharpen the mask |
| Ctrl + click on the background / Ctrl + drag on the background | Invert / clear the mask (the canvas border counts as background); with Wedge on, the drag also Wedges again on release (ZBrush: re-DynaMesh) |
| Ctrl + `I` / Ctrl + `A` / Ctrl + Shift + `A` | Invert / mask all / clear the mask (invert and mask all leave hidden polygons alone) |
| Ctrl + `H` | Show or hide the mask |
| Ctrl + Shift + drag | Keep what the rectangle covers visible, hide the rest (ZBrush SelectRect); hold Alt during the drag to hide what it covers instead (the rectangle turns red) |
| Ctrl + Shift + click on the model | Isolate the polygroup under the cursor (Alt: hide it) |
| Ctrl + Shift + click on empty canvas | Show everything |
| Ctrl + Shift + drag on empty canvas, around nothing | Invert visibility |
| Ctrl + `W` | With a mask: one new polygroup of the masked polygons, and the mask is cleared (Group Masked); without one: of the visible polygons (Group Visible) |
| Shift + `F` | Polygroup colours and the polygon frame (ZBrush PolyF). A new model is one polygroup, shown in blue; Ctrl + W or the Polygroups palette makes more |
| Left drag on empty canvas | Rotate (hold Shift to snap to axis views) |
| Alt + left drag on empty canvas | Move |
| Left drag in the border outside the canvas frame | Always navigates, even over the model, in every mode (as on empty canvas) |
| Alt + click, release Alt, drag | Zoom (ZBrush style; press Alt again to move) |
| Shift + click on empty canvas, release Shift, drag | Roll about the view axis |
| Right / middle drag | Rotate, anywhere (ZBrush right-click navigation) |
| Alt + right / middle drag | Move |
| Ctrl + right / middle drag | Zoom |
| Scroll | Zoom towards the cursor (Ctrl + scroll: brush radius) |
| Space (hold) or right-click tap | Quick menu at the cursor: brushes, Zadd/Zsub, Z Intensity, Draw Size, modes |
| `S` / `U` | Draw-size / Z-intensity slider popup at the cursor (`I`, `O` are stubs) |
| `[` `]` | Brush radius |
| `1`–`5` | Draw, Smooth, Grab, Flatten, Inflate |
| `Q` `W` `E` `R` | Draw / Move / Scale / Rotate mode |
| `W` / `E` / `R`, left drag on the active subtool | Draw a Transpose action line from the surface under the press (Shift snaps it to 45°) |
| `W`, drag the line's middle circle | Move the active subtool in the view plane |
| `E`, drag the line's end circle | Scale about the start circle by the change in distance to it (start circle: about the end) |
| `R`, drag the line's end circle | Rotate about the start circle around the view axis (start circle: about the end) |
| Drag a circle's outer ring / the line itself | Move that end of the action line / the whole line; the model stays |
| `X` | Symmetry across X |
| Ctrl + `D` | Divide: add a subdivision level above the top one and show it |
| `D` / Shift + `D` | Higher / lower subdivision level |
| Alt + click another subtool | Select it (Alt + drag on the active one still carves) |
| `F` | Frame the visible subtools (the shown part of the active one) |
| `P` / Shift + `P` | Perspective toggle / floor grid |
| `Home` | Reset the view angle and frame |
| `Tab` | Toggle canvas focus |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+S / Ctrl+O | Save / open the project at the path in the Document palette |

## How it works

- `mesh.rs`: vertices are structure-of-arrays and physically sorted into spatial chunks of
  about 4096 vertices (recursive median split). Each chunk owns a contiguous vertex range,
  a contiguous range of the index buffer (the triangles whose first vertex it owns, those
  reaching into another chunk first), an AABB, the list of neighbouring chunks and the few
  vertices it borrows from them. CSR adjacency tables give vertex neighbours and incident
  triangles. Every heavy pass (selection, position writes, bounds) runs in parallel over
  chunks with rayon.
- `sculpt.rs`: a stroke gathers vertices under the brush by testing chunk bounds first,
  computes new positions in one parallel pass and scatters them in a second, then marks the
  touched chunks and their neighbours dirty. Undo snapshots the owned positions of each
  chunk the first time a stroke touches it. Grab runs on the GPU (`grab.wgsl`): the
  selection, original positions and weights go up once, each frame sends one delta, and
  the CPU applies the final result when the stroke ends.
- `primitive.rs`: the starting meshes (Sphere3D, Cube3D, Cylinder3D, Ring3D, Plane3D). Sphere,
  Cube and Cylinder are the surface of a lattice box mapped onto the shape, so `--segments`
  still gives the old cube sphere bit for bit.
- `multires.rs`: ZBrush-style subdivision levels. Divide Loop-subdivides the top level 1:4,
  keeping chunk locality (each parent chunk's vertices and edge midpoints are split spatially
  into its child chunks) and building every table of the new mesh in parallel over chunks.
  A level above the base is `smooth(parent) + offset` per vertex; offsets are stored per
  chunk and only for chunks that were sculpted at that level. Sculpting edits the active
  level; its offsets are refreshed for the touched chunks when the stroke ends. Levels above
  are synced lazily: edited chunks are marked stale, and stepping up re-smooths only the
  dependent child chunks and re-applies their offsets, one level at a time. Meshes with a
  border (Plane3D) get Loop's boundary rules, chosen from topology alone so a replay gets
  the same stencils; on a closed mesh nothing changes. Edits at the top
  level touch nothing else. Undo stores positions at level 0 and offsets above it, so it
  works at any level, and the displayed level follows; Divide itself is undoable.
- `subtool.rs`: ZBrush-style SubTools. One subtool is active and holds its full level
  stack. Every other one is parked and kept light: in RAM only a lossless packed form of its
  stack (the base mesh in chunk order, one byte per vertex per upper level recording which
  child chunk each vertex went to, and the offsets of chunks that were sculpted) plus its
  undo list; on the GPU only a draw-only stand-in of at most 2M vertices, using the highest
  level that fits with every vertex placed on the top level's surface and normals baked
  once (40 bytes per vertex, no normal pass). Activating a parked subtool replays its
  subdivisions from those labels, so vertex order and chunks come back identical and its
  undo list stays valid. Alt + click picks a subtool with a one-pixel GPU id pass. The
  SubTool section of the Mesh palette lists them with eye toggles and has Append, Duplicate,
  Delete and reordering; Deformation moves and sizes the active subtool exactly at every
  level, undoably.
- `transform.rs`: moving, scaling and rotating a whole subtool, ZBrush Transpose style.
  The map is `p -> c + s R (p - c) + t` (rotation, uniform scale, translation). Loop weights
  sum to one, so subdivision commutes with it: every level maps exactly by mapping its
  positions and turning its offsets by `s R`, stale levels included, and its exact inverse is
  the undo step. The action line lives in world space; a drag on one of its circles turns
  the cursor into a map (the press point follows the cursor on its view plane for Move, the
  ratio of distances to the pivot on screen for Scale, the angle about the pivot for
  Rotate, around the line of sight through it). While dragging nothing moves on the CPU: the
  map goes to the vertex shader as a model matrix for the active subtool, so the drag costs
  nothing per frame. On release the CPU maps every level once, and the GPU copy of the shown
  level is mapped in place by a compute pass (`affine.wgsl`) instead of being uploaded again;
  normals are recomputed only after a rotation, and the normal accumulators' fixed-point
  scale follows the change in face area.
- `mask.rs`: ZBrush masking. The mask is one byte per vertex of the shown level only, and
  it moves with the level: going up an even vertex takes its parent's value and an odd
  vertex the mean of its edge's ends (a hard edge stays hard), going down a vertex takes its
  even child's value, so a mask survives D / Shift + D and comes back unchanged. Every brush
  multiplies its falloff by how free each vertex is, the GPU Grab included. Mask strokes
  undo per touched chunk; invert, clear, blur and sharpen undo the whole mask. The renderer
  keeps it as one byte per vertex beside the level and the vertex shader reads it, both for
  the dark tint and for a masked Transpose preview; with no mask the shader skips it, so an
  unmasked model draws exactly as fast as before. A masked Transpose moves each vertex part
  of the way (rotation angle, log scale and translation times its freedom): the shown level
  exactly as previewed, so masked vertices stay put to the bit; the levels below are mapped
  with the mask carried down, so the pose shows at every level; the shown level's offsets
  absorb the difference and the offsets above turn with the part they sit on. The inverse
  map with the same mask undoes it, so the undo step keeps the mask, not the positions.
- `polygroup.rs` with `mesh.rs` / `multires.rs`: ZBrush polygroups and visibility. Every
  level keeps a polygroup per triangle (16 bits) and a hidden flag per triangle, plus a
  per-vertex flag for the vertices that touch a visible triangle; all three arrays stay empty
  while the mesh is one group with nothing hidden, so a plain model costs nothing. Divide
  hands them to the four children of each triangle. An edit at any level (Ctrl + Shift
  gestures, Ctrl + W) is carried to every other level: upwards exactly (a child takes its
  parent's flag and group), downwards by rule (a triangle is hidden when all four children
  are, and takes its centre child's group). The child-to-parent triangle map is not stored
  (it would cost 8 bytes per vertex); it is derived from the subdivision stencils when an
  edit needs it. Brushes, mask strokes, Mask All / Invert and Transpose leave alone the
  vertices that touch only hidden triangles (Transpose treats them as masked); the ray cast
  skips hidden triangles, Frame frames the visible part. Ctrl + W with a mask groups the
  masked polygons and clears the mask, one undo step for both. On the GPU a level with hidden triangles
  draws and picks from a second index buffer of the visible ones (12 bytes per visible
  triangle, only while something is hidden); PolyF draws with a separate pipeline whose
  fragment shader reads the triangle's group from a 2-bytes-per-triangle buffer by primitive
  index (only while PolyF is on). The same shader draws the polyframe: it fetches the
  triangle's three corners from the index and position buffers it already has, measures the
  pixel distance to each edge, leaves out a quad's diagonal (the longest edge in the world
  when it is over 1.1 times the next) and fades the lines where polygons are under a few
  pixels, so a dense level does not turn black. No extra memory is used. With nothing hidden and PolyF off, the buffers and the
  pipeline are exactly the ones used before. Undo stores the previous groups or visibility
  of every level run-length coded; parked subtools and project files keep them the same way,
  and stand-ins of parked subtools leave hidden triangles out. OBJ export writes polygroups
  as OBJ groups.
- `wedge.rs`: Wedge, a DynaMesh-style remesher. Every triangle is rasterised onto the lines of
  a regular grid along x, y and z with exact, watertight edge tests (a shared edge is
  evaluated the same way from both sides, ties broken by one fixed rule), giving where each
  line crosses the surface; summing the crossing signs along a line is the winding number,
  so overlapping or self-intersecting parts merge into their union. Only the changes between
  outside and inside are kept, per line, so memory follows the surface, not the volume.
  Walking the crossings of four neighbouring z lines together lists every cell the surface
  passes through with its corner pattern, column by column. Cell faces whose corners
  alternate (the ambiguous case) get one outside corner turned inside until none is left,
  which makes the result provably manifold. Each cell holds one vertex per patch of surface
  through it (dual marching cubes), at the mean of its edge crossings, which lie exactly on
  the old surface, then slid along its normal to fit the old triangles' tangent planes; every
  crossed grid edge gives a quad, split along its shorter diagonal. Quads take the polygroup
  of the old triangle their edge crossed, and vertices on a group border move onto the old
  border, so groups keep clean borders. Project (with Blur passes first) moves every vertex
  to the closest point of the old surface by walking the old mesh. The result replaces the
  subtool's stack as a new base level; the old stack is kept packed for undo. Wedge refuses
  subtools with subdivision levels (as ZBrush does for DynaMesh; Geometry has Del Higher and
  Del Lower now) and open surfaces (Plane3D). Hidden triangles are remeshed too and the result
  is all visible.
- `project.rs`: `.klay` project files hold every subtool in the same packed form parked
  subtools use (base mesh in chunk order, replay labels, sculpted offsets), written as a few
  large binary blocks behind a magic number and version, through a temporary file that is
  moved into place. Version 2 added the shown level's mask, version 3 the polygroups and
  hidden triangles of every level (ZBrush keeps both in its tools); version 1 and 2 files
  still open. Reading checks every length against the bytes left before allocating;
  rebuilding checks index bounds, the chunk table, manifold edges and every level's labels
  and offsets, and turns anything else that fails into an error. Open rebuilds each
  subtool once to bake its stand-in and swaps the model only when all of them succeeded.
  Undo history is not saved. Export OBJ writes every visible subtool at its shown level as
  its own object, rebuilding parked ones one at a time.
- `render.rs`: positions, packed normals, indices and fixed-point normal accumulators live
  on the GPU (52 bytes per vertex), one set per subdivision level that has been shown, so
  stepping between levels is a pointer swap. Levels that are not shown keep their copies up
  to 1 GB in total; beyond that the largest are dropped and re-uploaded on the next visit,
  and a level that still does not fit is refused with a notice while the previous level
  stays up. Normals are never stored on the CPU, and the GPU holds no vertex-to-triangle
  table: `normals.wgsl` runs three dispatches over the flagged chunks and their neighbours.
  One workgroup per flagged chunk walks the chunk's triangle range and sums face normals for
  its own vertices in workgroup memory; one per listed chunk walks only its border triangles
  and adds, atomically, to the vertices they touch in other flagged chunks; a third turns the
  sums into packed unit normals. Dirty position ranges are re-uploaded as merged contiguous
  runs. egui is drawn in a second pass.

## Numbers (RTX 5060, 16 cores, release build)

| Mesh | Build | Medium grab | Grab of a third of the mesh |
| --- | --- | --- | --- |
| 960k vertices | 0.4 s | 0.1 ms/frame | 0.4 ms CPU + GPU |
| 8.6M vertices | 4.4 s | 0.3 ms/frame | 6 ms/frame |
| 13.5M vertices | 6.5 s | 0.5 ms/frame | 8 ms/frame |

### Subdivision levels (400-segment sphere, `--bench 400 --divide 2`)

| Step | Result |
| --- | --- |
| Divide 960k → 3.84M vertices | 260 ms |
| Divide 3.84M → 15.4M vertices | 1.03 s |
| Divide 15.4M → 61.4M vertices | 4.2 s, 9 GB RAM for the four levels, 3.2 GB of GPU memory to show it (was 4.4 GB with the vertex-to-triangle tables on the GPU) |
| Draw dab at level 0 with two levels above (7.7k vertices under the brush) | 0.7 ms/dab, same as without levels: upper levels are synced when you step up |
| Step up to level 1 after that stroke | 75 of 1 024 chunks re-smoothed, 0.6 ms |
| Step up to level 2 | 368 of 4 096 chunks re-smoothed, 3 ms |
| Step up to level 3 | 1 619 of 16 384 chunks re-smoothed, 14 ms |
| Undo / redo that base stroke while level 2 is shown | 4 ms each |
| Frame at level 0 / 1 / 2 / 3 (`--novsync --frames`) | 0.7 / 1.1 / 4.0 / 14.4 ms |
| GPU memory at level 2 / 3 while shown | 0.8 / 3.2 GB (52 bytes per vertex) |
| Memory for the three levels | 2.2 GB |
| Grab of a third of a 13.5M-vertex mesh (`--segments 1500 --stress --novsync`), with the normal passes each frame | 8.9 ms/frame (9.3 ms before the accumulate passes) |

### SubTools (400-segment sphere, `--bench 400 --divide N` and `--selftest-subtools`)

| Subtool | Active, in RAM | Parked, in RAM | Parked, on GPU | Park | Activate |
| --- | --- | --- | --- | --- | --- |
| 960k vertices, one level | 92 MB | 34 MB | 38 MB | 16 ms | 39 ms |
| Two levels, 3.84M at the top | 526 MB | 38 MB | 38 MB | 49 ms | 0.19 s |
| Three levels, 15.4M at the top | 2.26 GB | 53 MB | 38 MB | 0.19 s | 0.97 s |

### Transpose (`--divide 2 --novsync --selftest-transpose`, 15.4M vertices shown, three levels)

| Step | Result |
| --- | --- |
| Frame while dragging (model matrix in the vertex shader) | 3.4 ms, idle 3.7 ms: no cost |
| Release of a move / scale: CPU map of all three levels, GPU map in place | 16–17 ms |
| Release of a rotation (chunk boxes refitted, full normal pass) | 25 ms |
| Same releases re-uploading the shown level (184 MB) instead | 52–62 ms |
| GPU copy vs CPU after three transforms and three undos | 9e-7 |

### Masking (`--divide 2 --novsync --selftest-mask`, 15.4M vertices shown, three levels)

| Step | Result |
| --- | --- |
| Frame with no mask, with a mask shown, while dragging a masked move | no measurable difference (also at 61M: 15.2 ms with and without) |
| Mask dab (Ctrl + drag) | 0.5 ms (1.5 ms at 61M) |
| Invert / blur the whole mask | 2 / 20 ms (7 / 73 ms at 61M) |
| Carry the mask between 15.4M and 3.84M with Shift + D / D | 16 / 9 ms (43 / 35 ms between 61M and 15.4M) |
| Release of a masked move: CPU over all three levels | 100 ms, plus 38 ms to upload the moved chunks |

Project files (`--selftest-project`, one 15.4M-vertex three-level subtool plus two 960k
spheres): 123 MB, saved in 0.09 s and opened in 1.35 s; after the demo strokes, including a
grab that moved 5.2M top-level vertices, 189 MB, 0.12 s and 1.32 s. Both round trips are
byte-identical.

Parking bakes the stand-in and frees the stack (most of the time at three levels is giving
2 GB back to the system). Activating replays the subdivisions from the stored labels, a
little faster than dividing because there is no spatial split. Frame time at level 2 is
unchanged by the natural-order chunks.

### Polygroups and visibility (`--bench 400 --divide 2`, 15.4M vertices at level 2)

Measured on a 4-core cloud VM (not the RTX machine above); the work is parallel, so expect
these to drop by about the core ratio there.

| Step at level 2 (30.7M triangles) | Time |
| --- | --- |
| Ctrl + Shift rectangle around the middle (1.35M triangles kept), all three levels updated | 0.35 s |
| Of that: which triangles the rectangle covers / the level-to-level triangle map | 27 ms / 0.2 s |
| Visible index buffer for the GPU (16 MB) | 12–60 ms |
| Ctrl + W (Group Visible), all levels | 0.35 s |
| Ctrl + Shift + click on the background (show all) | 27 ms |
| Ctrl + Shift + click on a group (isolate) | 0.33 s |
| Undo of those four steps | 0.14 s |

Memory while something is hidden and there are two groups: 7 bytes per vertex at the level
in RAM (groups 4, hidden flags 2, shown flags 1); GPU: 12 bytes per visible triangle for the
visible index buffer, and 2 bytes per drawn triangle while PolyF is on. A plain model
(one group, nothing hidden) keeps 52 bytes per vertex and the same draw path; frame time in
the A/B run on software rendering (llvmpipe, 960k vertices, `--novsync --frames`) was within
the noise of `main`. In-window checks at 15.4M vertices were not possible on that VM, whose
software GPU caps a buffer binding at 128 MB.

### Wedge (same 4-core VM, `--bench 400 --divide 2`)

Resolution chosen for about a million output vertices (460 voxels across two units);
"chunk" is building the chunked mesh (`SculptMesh::from_raw`) from the result.

| Input | Wedge | + Project (Blur 2) | Chunk |
| --- | --- | --- | --- |
| 960k-vertex sphere (sculpted) → 997k vertices | 0.31 s (scan 0.13, cells 0.03, surface 0.15) | 0.44 s | 0.16–0.21 s |
| 15.4M-vertex level → 1.0M vertices | 0.67 s (scan 0.4, surface 0.2) | 1.1 s | 0.19–0.24 s |

In the window (`--selftest-wedge`, software GPU): a 4k-vertex primitive wedges at the default
resolution 128 to 77k (sphere) or 98k (cube) vertices in 45–50 ms, re-wedging at 192 takes
90–120 ms, and undoing both takes about 10 ms. Twice the resolution gives about four times
the vertices; the output is closed and manifold (tested on primitives, overlapping shells and
a sphere folded through itself), keeps the volume to within a voxel, and divides like any
mesh.

ZBrush's DynaMesh decisions and what Wedge does:

- **Resolution** (default 128) is relative to two world units, the size of a new primitive,
  so like ZBrush a bigger model gets more polygons at the same value. The section shows an
  estimate from the surface area.
- **Blur** (default 2) and **Project** (off): as in ZBrush, Blur only acts with Project.
- **Groups**: polygroups are always carried over with clean borders. ZBrush's Groups button,
  which splits the result into separate pieces at group borders, is not implemented.
- **Re-wedge gesture**: in ZBrush, Ctrl + drag on the background clears the mask, and while
  DynaMesh is on it remeshes. Klay does both: with Wedge off it clears the mask, with Wedge
  on it re-wedges, and the new surface starts without a mask (undo brings back the old
  surface with its mask). A Ctrl click on the background still inverts the mask.
- **Merge of overlapping subtools and Alt carving** (subtractive inserts) are not done: they
  need a SubTool Merge first. Within one subtool, overlapping parts already merge.
- The Wedge on/off state is per subtool and not saved in project files.

The remaining limit for CPU brushes is host-to-GPU upload bandwidth: about 5 GB/s on
this machine, so moving a third of a 13.5M-vertex mesh on the CPU would cost 12 ms of
upload per frame. That is why Grab runs on the GPU; the other brushes will follow.
