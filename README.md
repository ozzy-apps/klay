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
- **Right shelf**: Frame, Floor grid, Detail shading, Hide interface, plus reserved toggles.
- **Tab** hides the whole interface; **Tab** or **Escape** restores it.
- Layout (placement, order, collapse state, sections, trays, tray width) saves automatically to
  `$XDG_CONFIG_HOME/klay/workspace.layout`, falling back to `~/.config/klay/workspace.layout`.
  Older `v1` layout files are ignored and replaced.

The five brushes, radius/strength, Add/Subtract, materials, floor grid, detail shading, view
presets, primitives (Mesh → Initialize), subdivision levels (Mesh → Geometry: Divide, SDiv slider, vertex counts,
vertex budget), undo/redo, OBJ export and live statistics are connected to the engine.
Paint modes, Zcut, Focal Shift, stroke options, alpha, texture, masking, transform, symmetry and
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
| Ctrl + left drag | Reserved for masking (not connected yet) |
| Left drag on empty canvas | Rotate (hold Shift to snap to axis views) |
| Alt + left drag on empty canvas | Move |
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
| `Q` `W` `E` `R` | Draw / Move / Scale / Rotate mode (only Draw sculpts so far) |
| `X` | Symmetry across X |
| Ctrl + `D` | Divide: add a subdivision level above the top one and show it |
| `D` / Shift + `D` | Higher / lower subdivision level |
| Alt + click another subtool | Select it (Alt + drag on the active one still carves) |
| `F` | Frame the visible subtools |
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
- `project.rs`: `.klay` project files hold every subtool in the same packed form parked
  subtools use (base mesh in chunk order, replay labels, sculpted offsets), written as a few
  large binary blocks behind a magic number and version, through a temporary file that is
  moved into place. Reading checks every length against the bytes left before allocating;
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

Project files (`--selftest-project`, one 15.4M-vertex three-level subtool plus two 960k
spheres): 123 MB, saved in 0.09 s and opened in 1.35 s; after the demo strokes, including a
grab that moved 5.2M top-level vertices, 189 MB, 0.12 s and 1.32 s. Both round trips are
byte-identical.

Parking bakes the stand-in and frees the stack (most of the time at three levels is giving
2 GB back to the system). Activating replays the subdivisions from the stored labels, a
little faster than dividing because there is no spatial split. Frame time at level 2 is
unchanged by the natural-order chunks.

The remaining limit for CPU brushes is host-to-GPU upload bandwidth: about 5 GB/s on
this machine, so moving a third of a 13.5M-vertex mesh on the CPU would cost 12 ms of
upload per frame. That is why Grab runs on the GPU; the other brushes will follow.
