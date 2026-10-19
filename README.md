# Klay

A sculpting prototype in Rust + wgpu, exploring how far a chunked triangle mesh can go
before polygon count becomes the bottleneck.

## Run

```bash
cargo run --release                       # 960k vertices
cargo run --release -- --segments 1200    # 8.6M vertices
cargo run --release -- --demo             # apply scripted strokes on startup
cargo run --release -- --bench 1500       # headless CPU benchmark at 13.5M vertices
cargo run --release -- --segments 1500 --stress --novsync   # per-frame cost of a huge grab
```

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
presets, mesh rebuild, undo/redo, OBJ export and live statistics are connected to the engine.
Paint modes, Zcut, Focal Shift, stroke options, alpha, texture, masking, transform, symmetry and
the Persp/Local/Transp/Ghost/Solo toggles are drawn dimmed and marked **Not connected**.

Brush thumbnails are rendered on the CPU at startup: a sphere in the current material with the
stroke each brush produces (Draw and Inflate flip in Zsub). They re-render when the material
changes.

For a lighter preview: `cargo run --release -- --segments 200`.

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
| `S` / `U` | Draw-size / Z-intensity slider popup at the cursor (`I`, `O` are stubs) |
| `[` `]` | Brush radius |
| `1`–`5` | Draw, Smooth, Grab, Flatten, Inflate |
| `Q` `W` `E` `R` | Draw / Move / Scale / Rotate mode (only Draw sculpts so far) |
| `X` | Symmetry across X |
| `F` | Frame the mesh |
| `P` / Shift + `P` | Perspective toggle / floor grid |
| `Home` | Reset the view angle and frame |
| `Tab` | Toggle canvas focus |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |

## How it works

- `mesh.rs`: vertices are structure-of-arrays and physically sorted into spatial chunks of
  about 4096 vertices (recursive median split). Each chunk owns a contiguous vertex range,
  a triangle list, an AABB, the list of neighbouring chunks and the few vertices it borrows
  from them. CSR adjacency tables give vertex neighbours and incident triangles. Every heavy
  pass (selection, position writes, bounds) runs in parallel over chunks with rayon.
- `sculpt.rs`: a stroke gathers vertices under the brush by testing chunk bounds first,
  computes new positions in one parallel pass and scatters them in a second, then marks the
  touched chunks and their neighbours dirty. Undo snapshots the owned positions of each
  chunk the first time a stroke touches it. Grab runs on the GPU (`grab.wgsl`): the
  selection, original positions and weights go up once, each frame sends one delta, and
  the CPU applies the final result when the stroke ends.
- `render.rs`: positions, packed normals and indices live on the GPU. Normals are never
  stored on the CPU: a compute pass (`normals.wgsl`) recomputes them for flagged chunks each
  frame. Dirty position ranges are re-uploaded as merged contiguous runs. egui is drawn in a
  second pass.

## Numbers (RTX 5060, 16 cores, release build)

| Mesh | Build | Medium grab | Grab of a third of the mesh |
| --- | --- | --- | --- |
| 960k vertices | 0.4 s | 0.1 ms/frame | 0.4 ms CPU + GPU |
| 8.6M vertices | 4.4 s | 0.3 ms/frame | 6 ms/frame |
| 13.5M vertices | 6.5 s | 0.5 ms/frame | 8 ms/frame |

The remaining limit for CPU brushes is host-to-GPU upload bandwidth: about 5 GB/s on
this machine, so moving a third of a 13.5M-vertex mesh on the CPU would cost 12 ms of
upload per frame. That is why Grab runs on the GPU; the other brushes will follow.
