# Klay guide

Everything Klay does, with its command-line options and the numbers behind it. The short
version is in the [README](../README.md).

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
cargo run --release -- --segments 400 --divide 2 --novsync --frames   # average frame time at the top level and what culling drew, then exit
cargo run --release -- --segments 400 --divide 3 --novsync --frames --camera-distance 0.35 --render triangles   # closer camera; draw without GPU culling
cargo run --release -- --segments 200 --divide 2 --novsync --selftest-cull   # culled vs whole frames, pixel for pixel, after camera moves and edits, log, exit
cargo run --release -- --segments 400 --divide 2 --novsync --selftest-splat  # points vs triangles (empty pixels, colour) with paint, a mask and a GPU Grab, log, exit
cargo run --release -- --subtools 4                         # start with four subtools in a row
cargo run --release -- --segments 400 --divide 2 --subtools 3 --selftest-subtools   # pick and switch subtools via the GPU path, log, exit
cargo run --release -- --open model.klay                    # open a saved project
cargo run --release -- --segments 400 --divide 2 --subtools 3 --selftest-project    # save, re-open, check byte-identical, log, exit
cargo run --release -- --segments 400 --divide 2 --novsync --selftest-transpose     # Transpose move / scale / rotate + undo through the input path, log, exit
cargo run --release -- --segments 400 --divide 2 --novsync --selftest-mask          # Ctrl masking, a masked move and undo through the input path, log, exit
cargo run --release -- --segments 200 --divide 1 --selftest-groups   # Ctrl + Shift rectangle, Ctrl + W, GPU draw and pick checks, undo, log, exit
cargo run --release -- --primitive cube --selftest-remesh            # Remesh on, Ctrl + drag remeshes again, GPU checks, undo, log, exit (--selftest-wedge still works)
cargo run --release -- --divide 2 --selftest-pressure                # tablet connection (moves the pointer once via XTest), strokes at set pen pressures, log, exit
cargo run --release -- --divide 2 --selftest-curve                   # Zipper along a curve vs freehand with a shaky hand, bend, GPU readback, undo/redo, log, exit
cargo run --release -- --segments 400 --selftest-clip                # three crossing Clip Line cuts (a pie slice), checks no cut pushes an earlier one out, log, exit (--hold keeps it open, turned)
cargo run --release -- --segments 400 --selftest-tube                # CurveTube: nine tubes, a bend, GPU readback, undo/redo, log, exit (--hold keeps the window open)
cargo run --release -- --segments 400 --selftest-insert              # Insert Mesh: one of each shape, GPU readback, undo/redo, log, exit
cargo run --release -- --segments 12 --selftest-sculptris            # Sculptris Pro: three strokes, timing, closed surface, GPU readback, undo/redo, log, exit
cargo run --release -- --segments 100 --selftest-boolean             # Live Boolean: a subtracted ball, frame readback vs Make Boolean Mesh, log, exit
cargo run --release -- --selftest-zmodeler                           # ZModeler: QMesh, Inset, Insert, Polygroup, Move on a cube, GPU readback, undo, log, exit
cargo run --release -- --segments 100 --divide 1 --selftest-surface    # mask tools + Clip Line, GPU readback and undo/redo
cargo run --release -- --segments 100 --divide 1 --selftest-brushes    # new brushes, GPU readback and undo/redo
cargo run --release -- --selftest-materials                            # every material drawn and read back, MatCaps against their images, polypaint under a MatCap
cargo run --release -- --selftest-document                             # Back gradient and Border on the GPU, the empty border zoomed in, Snapshot (--hold keeps it open)
cargo run --release -- --segments 100 --selftest-symmetry              # Draw, Grab, Snake Hook, Inflate with Y + Radial Z 6 through the canvas, GPU readback, undo/redo
cargo run --release -- --demo-brushes --symmetry y,rz8 --snapshot out.png   # start with symmetry (x, y, z, r<axis><count>), save the canvas after start-up and quit
cargo run --release -- --snapshot ui.png --snapshot-ui               # save the whole window, interface included, after start-up and quit
cargo run --release -- --segments 400 --divide 2 --demo-stamp texture:Scales --camera-distance 0.45 --snapshot s.png   # one Draw stroke at Draw Size 1.27 with a built-in pattern (texture:NAME or alpha:NAME)
cargo run --release -- --space-menu                                  # start with the Space menu open
cargo run --release -- --brush-catalog                               # start with the B catalog open
cargo run --release -- --theme "Graphite Blue"                       # start in an interface theme (Preferences › Theme)
cargo run --release -- --menu Preferences --ui-scale 0.8             # start with a palette's menu open, at an interface scale
cargo run --release -- --menu "3D Print" --print-check               # 3D Print open with Check Mesh already run
cargo run --release -- --fake-update 0.30.0                          # show the update card for a pretend release (no network)
cargo run --release -- --gizmo                                         # start in Move mode with Gizmo 3D
cargo run --release -- --segments 200 --demo-brushes --material "MatCap Red Wax"   # strokes with the 0.7 brushes, in a chosen material (name or number)
cargo run --release -- --segments 100 --divide 1 --selftest-paint      # texture painting + GPU readback + undo/redo
cargo run --release -- --version                                     # application version
cargo run --release -- --remesh-resolution 256                       # start with this Remesh resolution (--wedge-resolution still works)
cargo run --release -- --segments 200 --polyf                        # start with PolyF (polygroup colours and frame) on
```

`--segments N` sets the resolution of the starting primitive (`--primitive NAME`, Sphere by
default; ZBrush names such as `Cube3D` work too). `--bench N` builds the primitive at N
segments, 400 by default, so the benchmark numbers below keep their meshes. The numbers for
windowed runs below were taken with `--segments 400`.

`--budget N` caps the vertex count a Divide may reach (default 64 000 000, three divides of the
960k sphere); the Geometry section of the Mesh palette has the same slider. Each level costs
about 140 bytes per vertex of RAM before optional paint and 57 bytes per vertex of GPU memory while it is shown. Paint adds four bytes per vertex in RAM for each painted level; historical benchmark numbers below predate this buffer.

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

Cube3D starts with a polygroup per face and Cylinder3D with one for the wall and one per cap,
so Ctrl + Shift + click isolates a face or cap straight away; the others start as one
polygroup. The groups are carried through Divide and Remesh like any others.

### Pen pressure

With a drawing tablet on X11 (XP-Pen, Wacom, Huion: any pen whose driver reports an "Abs
Pressure" axis, as the libinput and wacom drivers do), pen pressure scales each dab like
ZBrush's Brush › Tablet Pressure with its default linear curves: **Draw Size** and **Z
Intensity** both follow the pen, and either can be turned off in Brush › Tablet pressure.
The lightest touch still leaves a dab 5 % of Draw Size wide, with no strength. Sculpting
brushes, Shift smoothing and Ctrl mask painting follow the pen, and RGB Intensity follows
Z Intensity's toggle. Drag brushes (Grab, Elastic, Snake Hook) keep what they picked up
with, and Zipper, Stitch and Rake keep their size so the teeth stay even. Curve strokes are
applied at full Draw Size and Z Intensity. A mouse, or any device without pressure, sculpts
at full Draw Size and Z Intensity, so switching between pen and mouse needs no setting.

Brush › Tablet pressure shows the live pressure and the pen found. The libinput driver adds
the pen's device the first time the pen comes near the screen, so it shows "none yet" until
then. Pressure is not read on Wayland yet (winit, the window library, does not report it):
run an X11 session.

### Curve strokes

ZBrush's Stroke › Curve: instead of following every wobble of the hand, the stroke is drawn
as a path, and on release it becomes a smooth curve on the surface and the brush is applied
along it. **Zipper, Stitch and Rake start in Curve mode**, as ZBrush's zipper and stitch
brushes do; any dab brush (Draw, Dam Standard, Clay Buildup, Paint…) can use it too. Switch
a brush between Freehand and Curve with the stroke thumbnail on the left shelf or in
**Stroke › Curve**.

- Draw the stroke: a dashed line follows the cursor and nothing changes until release.
- On release the path is smoothed (**Curve smooth**, times the brush's size on screen),
  laid onto the surface and the brush is applied along it as one undo step. Zipper, Stitch
  and Rake are laid out in the curve's own frame: every vertex near it is set once from its
  nearest point on the curve, so the teeth stay square to the line and evenly spaced by its
  length. Other brushes dab every **Curve step** (times Draw Size) along it.
- The curve stays on the model as a dashed line. It lights up under the cursor: **drag it
  to bend it**, and the brush is re-applied along the new shape from the surface the curve
  was first applied to (one undo step per bend). A new stroke starts a new curve.
- **Esc** lets the curve go (the result stays); Esc while drawing drops the stroke. Any
  other edit (another stroke, undo, a level change) lets it go as well.
- Drag brushes, masks and shape tools stroke freehand only. Curve strokes respect masks,
  hidden polygons and X symmetry, like freehand ones.

### CurveTube

ZBrush's CurveTube: a curve brush that adds geometry instead of moving the surface, for
dreadlocks, cables, straps or piping. Pick **CurveTube** (B catalog, Curve filter) and draw
a curve on the model; on release a closed round tube is built along it and added to the
active subtool as a **new polygroup**.

- The tube's centre runs on the surface, so its root sits half inside the model. Where the
  stroke runs off the model, the curve carries on at the depth where it left it, so a tube
  can hang in the air (a lock falling past the shoulder).
- **Draw Size** is the radius. **Stroke › Curve** has **Tube taper** (0 keeps the radius,
  1 narrows it to a point at the end) and **Tube sides** (3–32 around). Curve smooth
  applies as for any curve.
- The curve stays live: **drag it to bend the tube**. Pulled points snap to the model where
  it is under them and stay at their depth where it is not, so a tube can be pulled off the
  surface. Changing Draw Size, taper or sides rebuilds the live tube. Esc or any other edit
  lets the curve go; the tube stays.
- One undo step per tube and per bend. X symmetry adds the mirrored tube in the same
  polygroup.
- Like ZBrush's insert brushes it needs a subtool **without subdivision levels** (Del Lower,
  or Append a subtool for the tubes). The tube is ordinary mesh: it saves, exports, divides
  and can be remeshed with the rest.

## Workspace

The native egui workspace follows ZBrush's model. Every palette lives in the menu bar and opens
there as a dropdown; any palette can also be docked into the left, right, top or bottom tray.
Fixed shelves hold the live sculpt controls. The look is dark and flat with one clay accent;
`../ui-mockup.html` is the interactive HTML reference it was ported from.

The interface is set in Inter (bundled under the SIL Open Font License, `assets/fonts`)
with three levels: palette titles, small-capital section titles, then 26 px controls.
Sliders keep their name on the bar and show the value in a field of their own; one clay
accent marks what is on. Buttons that share a row clip their labels inside narrow trays.
`../ui-readable.png` shows the interface.

- **Klay logo and name**: one button opens About Klay, showing the Coral Fold logo, package
  version, build language and GitHub repository link. Close it with **Close**, **Escape** or a
  click outside the dialog; canvas input and sculpting shortcuts are suspended while it is open.
- **Menu bar**: click a palette name to drop it down. A dot marks palettes that are also docked.
- **Dock button** (the ◉ at the left of every palette header): click for a menu (left, right, top
  or bottom tray, or menu only), or drag it onto a highlighted canvas edge or between palettes in
  a tray. Click a docked palette's title to collapse it; sub-palettes collapse on their own.
- **Trays**: the thin dividers beside the canvas toggle the left and right trays. Top and bottom
  trays appear when something is docked there. Drag empty tray space with a pen or mouse to
  scroll.
- **Top shelf**: Sculpt mode, Z+RGB / RGB / Z, RGB Intensity, Zadd/Zsub, Z Intensity (strength), Focal Shift, Draw Size (radius), Undo/Redo.
- **Left shelf**: the current brush and material as rendered sphere thumbnails (click for a
  picker), stroke/alpha/texture slots, and an HSV colour picker with swap.
- **Right shelf**: Frame, Floor grid, Symmetry, PolyF (polygroup colours), Solo, Detail
  shading, Hide interface, plus reserved toggles.
- **Tab** hides the whole interface; **Tab** or **Escape** restores it.
- Layout (placement, order, collapse state, sections, trays, tray width) saves automatically to
  `$XDG_CONFIG_HOME/klay/workspace.layout`, falling back to `~/.config/klay/workspace.layout`.
  Older `v1` layout files are ignored and replaced.

The 49 brushes and tools, Sculptris Pro, alphas and textures, RGB painting, focal shift, pen pressure,
radius/strength, Add/Subtract, materials, floor grid, detail shading, view presets, primitives,
Remesh, polygroups, visibility, masking, subdivision, undo/redo, OBJ export, X / Y / Z and
radial symmetry, Auto Masking, LazyMouse and Spacing, Transpose, Curve strokes and live
statistics are connected to the engine. Zcut, parts of the Transform palette and reserved
Local/Transp/Ghost toggles remain marked **Not connected**.

### ZBrush tools added in 0.11

- **Pointer**: over the model the brush is the cursor, as in ZBrush: the system pointer hides
  while the brush ring is on the model or a stroke is under way (a small dot marks the
  brush centre), and shows over the interface, the empty canvas and the border.
- **Document › Import**: an OBJ or STL as a new subtool (OBJ: polygons of any size, `o` / `g`
  names as polygroups, vertex colours as polypaint; STL: binary or ASCII, corners at the same
  position welded into one closed mesh, flat and repeated triangles left out, turned from
  Z up to Y up while **3D Print › Z Up** is on). **Fit** centres and scales it to a new
  primitive's size, so Draw Size suits it. **Document › Export** writes OBJ or STL.
- **Mesh › Deformation**: besides Offset and Size, **Smooth, Polish** (keeps the volume),
  **Inflate, Spherize, Noise, Twist** and **Taper** (about Y). Drag and release; they reshape
  the shown level as one undo step and masks hold.
- **Mesh › Morph Target**: **StoreMT** keeps the shown level's shape, **Switch** swaps it
  with the current one (undoable), **DelMT** forgets it. The **Morph** brush brushes the
  surface back to it. Kept per subtool for the session.
- **Mesh › SubTool**: **Merge Down** joins the subtool below into this one (both keep their
  polygroups), **Split Hidden** and **Groups Split** make subtools of the hidden polygons or
  of each polygroup, **Extract** turns the masked surface into a new closed shell subtool,
  **Thick** (% of the subtool's size) thick. Split and Merge need subtools without
  subdivision levels and are not undoable (like Delete).
- **Stroke › DragRect / DragDot** for every dab brush: DragRect places one stamp where you
  press and sizes and turns it as you drag (alphas turn with it); DragDot slides one stamp of
  the current Draw Size until you release. The left shelf stroke thumbnail cycles Freehand /
  Curve / DragRect / DragDot.
- **View › Camera › Angle of view** (perspective).

### ZBrush tools added in 0.12

- **Gizmo 3D** (`Y`, or Transform › Gizmo 3D): in Move / Scale / Rotate mode a manipulator at
  the subtool's centre (or where you put it, 0.15) replaces the action line. Arrows move along X / Y / Z, rings turn
  about them, the outer ring turns about the view axis, the white circle moves freely and the
  yellow square scales evenly (drag up-right to grow). Live preview, one undo step on
  release, masks respected, like Transpose.
- **Mesh › Layers**: **New** adds a layer that records (**REC**) what you sculpt; the eye shows
  or hides it, **Intensity** (-1..2) scales it live, **Delete** removes it and its effect,
  **Bake** / **Bake All** keep the effect and remove the layer. Every change is an undo
  step. Layers live on the highest subdivision level: Divide and Del Higher wait until they
  are baked, and the level cannot change while one records. They stay with a subtool when
  you switch subtools; they are not saved in projects yet (the shape is).
- **Brush › Tablet pressure curves**: a Size and an Intensity curve (presets Linear, Soft,
  Firm, S; drag points, click to add, double-click to remove) with the pen's live pressure
  marked on them.
- **Edit › History**: every step by name (strokes by their brush); click one to undo or
  redo to just after it.
- **Preferences › QuickSave**: every N minutes (default 10), when something changed and you
  pause for a few seconds, every subtool goes to the oldest of `QuickSave_1–5.klay` in
  `~/.local/share/klay/quicksave` (`$XDG_DATA_HOME`); **QuickSave now** saves at once. Open
  one with Document › Open. Self-tests and scripted runs never QuickSave.

### ZBrush tools added in 0.13

- **Mesh › Geometry › Del Hidden** deletes the hidden polygons; **Close Holes** caps every
  open border with a fan (each cap a new polygroup). **Deformation › Mirror And Weld** cuts
  the subtool at X = 0, mirrors the +X half onto −X and welds the seam, so it becomes exactly
  symmetric. All three need a subtool without subdivision levels and without layers, and are
  one undo step each.
- **Light** palette: drag on the sphere to place the key light (it turns with the view, as in
  ZBrush), **Intensity** and **Ambient**. It lights the four clays; MatCaps carry their own
  light.
- **Projects** (format version 5) keep each subtool's **Layers** (names, intensity, shown or
  hidden; a recording layer is saved as not recording) and its **Morph Target**. Versions
  1–4 still open.

### Alphas in 0.14

- **Alpha rotation**, above the alphas (Alpha palette and the shelf's Alpha popup): **Fixed** (at **Angle**), **Follow** (the alpha
  rolls with the stroke direction, ZBrush's Roll) or **Random** (a new turn every dab, as
  ZBrush's spray-style texture painting does), for sculpting, painting and the mask pen.
- Nine new detail alphas, painted by Klay as tileable images: **Pores, Wrinkles,
  Scales, Dry Cracks, Weave, Rock, Bark, Leather, Fibers**. As alphas they fade softly at the
  stamp's edge; as textures (Texture palette) they repeat seamlessly for texture painting.
- Seven more in 0.27: **Skin, Creases, Cracked Earth, Panels, Wood Grain, Cobbles,
  Hammered**, and all 16 are painted at 2048 × 2048 in 16 bits and read back with a smooth
  cubic filter (imported images too), so even a large Draw Size over a dense mesh gives
  smooth edges, not stair steps. Each image is built the first time it is used (8 MB, about
  0.15 s); the thumbnails in the grid are drawn on their own.

### ZBrush tools added in 0.15

- **Squash and stretch along one axis** (flatten a sphere): in **Scale** mode (`E`) drag the
  action line's **middle circle** left to squash the subtool along the line, right to
  stretch it (about the middle circle; masks hold). **Gizmo 3D** has a small box on each
  axis that does the same along that axis (drag in to squash, out to stretch), and
  **Deformation › Size** has ZBrush's **X Y Z** toggles: with one axis lit it flattens or
  stretches along it, with two it scales those two.
- **Gizmo 3D pivot**: **Alt + click** the model to put the gizmo there, its Y along the
  surface normal; **Alt + drag** a handle moves or turns the gizmo alone; it stays on the
  model as you move or turn it (and through undo). Transform › **Unmasked Center** puts it at
  the centre of what is unmasked and shown, **Reset** back to the subtool's centre and world
  axes.
- **Tablet Pressure per brush**: Brush › Tablet pressure switches between **Global** and the
  brush's own settings (Size and Z Intensity toggles and both curves), so Smooth can ignore
  pressure while Clay Buildup follows a soft curve. Brushes without their own use Global.
- **IMM Primitives** (Insert Mesh, B catalog › Insert): drag on the model to insert a
  **Sphere, Cube, Cylinder** or **Ring** (Brush › Insert Mesh) standing on the surface; the
  drag sets its size and it is about as dense as the model. It becomes a **new polygroup**
  and the rest of the subtool is **masked**, ready for Transpose or the Gizmo. Symmetry
  inserts mirror copies. One undo step; needs a subtool without subdivision levels.

### Sculptris Pro (0.16)

ZBrush's dynamic tessellation: with **Sculptris Pro** on (top shelf, or Brush › Sculptris
Pro), strokes add triangles where the brush goes and remove them where there are more than
needed, so you can start from a ball of a few hundred polygons and sculpt any detail
without dividing or remeshing.

- Triangles follow **Draw Size**, as in ZBrush's adaptive mode: a small brush makes small
  triangles, a large one large ones. **Detail** (Brush › Sculptris Pro) sets how fine for a
  given size (50: edges a fifth of the radius; every 25 halves them).
- **Smooth** (Shift) relaxes and coarsens to its own size; masked surface, hidden
  polygons, open borders and polygroup borders stay as they are. Symmetry retessellates
  every copy.
- Every dab brush works with it except Layer, Morph, the detail brushes (Zipper, Stitch,
  Rake), drag brushes (Grab, Snake Hook, Elastic, Move Topological) and DragRect /
  DragDot strokes, which sculpt as usual. It needs a subtool without subdivision levels
  and without layers (a stroke then sculpts as usual and says why).
- A stroke is one undo step, holding only the triangles it changed (at two million
  triangles about 1–4 MB a stroke instead of a 35 MB copy). Fluid up to a few hundred
  thousand triangles; at around two million each pointer move takes ~20–30 ms.

### Live Boolean (0.17)

ZBrush's booleans between subtools. Every subtool has an operation (Mesh › Boolean, marked
beside its name in the SubTool list): **Add**, **Subtract** or **Intersect**. Visible
subtools combine top to bottom: each one joins, carves itself out of, or keeps only its
overlap with the result of the ones above it.

- **Live Boolean** (top shelf, or Mesh › Boolean) shows the result in the viewport as you
  work: move, scale or turn a Subtract subtool with Transpose or the Gizmo and the hole
  follows it while you drag; sculpt either one and the result updates when the stroke ends.
  Each subtool's inside is kept as a 96³ distance volume, so the cut line is accurate to
  about 1/90 of the subtool's size. Up to 20 visible subtools.
- **Make Boolean Mesh** builds the result as one new closed subtool (named Boolean 1, 2, …)
  at the Boolean **Resolution** (Remesh voxels across two units, default 400), with sharp
  edges where the subtools meet, polygroups and polypaint kept. The subtools it came from
  are hidden and Live Boolean goes off. Not undoable: delete the new subtool to go back.
- Projects (format version 6) save each subtool's operation; versions 1–5 still open.

### ZRemesher (0.18)

Geometry › **ZRemesher** rebuilds the active subtool as clean quads whose edge loops follow
its form, at about **Target Polygons Count** thousand quads (**Half / Same / Double** set it
from the subtool's polygons now). **AdaptiveSize** (0–100, default 50) makes quads smaller
where the surface curves more and larger where it is flat, by up to half and twice the
even size, keeping about the target count. With **KeepGroups** edge loops run along the polygroup
borders, and vertices on a border are put exactly on it, so a polygrouped hard-surface
part keeps crisp edges and corners. Holes in a closed surface are closed; the result is
all quads (a few three- and five-sided poles where the surface needs them). One undo
step; it needs a subtool without subdivision levels. A million-vertex sculpt goes to 5k
quads in under a second, to 30k in about two.

The method is Instant Meshes (Jakob et al. 2015): a four-way direction field and a quad
lattice are smoothed over a graph of surface samples, the lattice points become vertices,
and every face is split through its middle so the result is all quads.

### ZModeler (0.19)

The **ZModeler** brush (B catalog › Insert) models with polygons instead of sculpting.
Hover the model: the point, edge or polygon under the cursor lights up (a point within a
few pixels, then an edge, else the polygon), and a click or drag acts on it. Brush ›
ZModeler picks the actions, as ZBrush's Space menu does:

- **Polygon**: **QMesh** / **Extrude** (drag to pull out or push in along the normal),
  **Inset** (drag to size the inner copy), **Delete**, **Polygroup** (click), **Bridge**
  (click one polygon, then another with as many sides: a tube joins them). Targets: **A
  Single Poly**, **Polygroup All**, **Polyloop** (the ring of quads across the edge nearest
  the cursor).
- **Edge**: **Insert** an edge loop round the ring of quads through the edge (click where it
  goes, drag to slide it; a triangle at the end becomes a quad), **Delete** the edge or
  its whole **EdgeLoop**, **Bevel** the edge loop into a flat strip (drag for its width),
  or **Spin** the edge one step round its two polygons (click).
- **Point**: **Move** in the view plane.

With symmetry on (X, Y, Z or radial), every action also happens on the mirror images of
its target, mirrored (a spin turns the other way in a mirror).

Klay keeps triangles, so ZModeler pairs them into quads the way PolyF shows them and then
remembers the pairing from one action to the next. Each action is one undo step; it needs
a subtool without subdivision levels. Not yet: Crease, Slide, Spin Poly and the rest of
ZBrush's long list.

### Brushes added in 0.23

- **Sculpt**: Smooth Stronger, Slash, Pen A, Pen S, Noise, Gravity and Spiral (drag to twist
  the surface round the press point). Elastic is now named **Move Elastic**.
- **Ctrl + Shift tools**, as in ZBrush: picking one of them makes it the tool Ctrl + Shift
  uses; as the current brush a plain drag uses it too. Alt works on the other side.
  - **Select Rect / Select Lasso**: what the outline covers stays shown, the rest is hidden.
  - **Clip Rect / Clip Circle**: the model is pressed into the outline all the way through.
  - **Trim Curve**: the arrow's side is cut away exactly along the line and closed flat.
  - **Trim Rect / Circle / Lasso**: only what is inside is kept, rebuilt as a closed surface by
    the boolean remesher (Mesh › Boolean's resolution), as ZBrush trims a DynaMesh.
  - **Slice Curve**: polygons are cut along the line and the arrow's side becomes a new polygroup.
  - **Knife Curve**: the model is cut into two closed pieces with polygroups of their own
    (Groups Split separates them).
  Trim, Slice and Knife need a subtool without subdivision levels and are one undo step each.

### Interface: scale, themes, shelves

- **F11** switches fullscreen. **Preferences › Interface › UI scale** sizes the whole interface
  from 80 to 200 % (Ctrl + plus / minus too).
- **Preferences › Theme** has seven themes and your own: change a theme's panel, text and accent
  colours (a built-in theme is copied first), name it, add or delete themes.
- **Preferences › Interface › Customize**, as in ZBrush: every item on the top, left, right and
  bottom shelves (mode buttons, sliders, thumbnails, the colour picker, view buttons, separators)
  can be dragged to another place or shelf, or onto the canvas to take it off. Brushes and the
  outlined palette buttons can be dragged onto any shelf too, and taken-off items wait under
  Preferences › Interface to come back. **Reset shelves** returns to Klay's own. Palettes dock to
  any tray by the ring left of their name.
- The arrows at the right end of the menu bar step to the previous and next theme.
- Scale, theme, custom themes and shelves are saved with the workspace as you go.
- **Preferences › Config**: **Store Config** keeps the whole interface, and the model as the
  startup document; Klay then starts from them. **Load Config**, **Factory settings** and
  **Delete stored** go with it.

### Updates

At start and every six hours Klay asks GitHub for the latest release of its repository (with
the system's `curl`, in the background). A newer release shows a card with **Download**, **Later**
and **Skip this version**; **Preferences › Updates** turns checking off or checks now. To publish
an update: make the repository (or a releases repository) public, tag the version (`v0.25.0`),
and create a GitHub release with the packaged file attached (`scripts/package-linux.sh` writes
`dist/Klay-<version>-<arch>.AppDir.tar.gz`).

### Canvas and Document

The document frame is drawn inside the canvas, and the margin around it (the border) is
always empty canvas: models are drawn only inside the frame, as in ZBrush, so however far you
zoom in there is always somewhere to click and drag to rotate, move and zoom. The border is
40 points wide; **Document › Back › Border width** sets it from 8 to 120.

**Document** palette: **Open** (Ctrl + O) picks a project in the desktop's file dialog;
**Save** (Ctrl + S) saves to the project's file, and asks where the first time; **Save As**
picks a new file and keeps working there (each dialog starts in the project's folder, or
where the last file was picked); **Revert** opens the project again as it was last saved (it
asks first); **Snapshot** saves the canvas inside the frame, without the interface, as
`<project> snapshot.png` next to the project. **Back**: the **Back** colour of the canvas
with a vertical gradient (**Range**: how strong, **Center**: where it crosses the Back
colour, **Rate**: how soft) and the **Border** colour; Reset returns Klay's defaults. The
colours are kept with the workspace layout (Workspace › Reset leaves them alone).

### 3D printing

**3D Print** palette (ZBrush: 3D Print Hub), for sending a sculpt to a slicer:

- **Size**: the print's **Width**, **Depth** and **Height** in **mm**, **cm** or **in**.
  Drag or type any one and the print scales evenly; **1 unit** is how many of them a Klay
  unit becomes. The model itself does not change, only what is written. **Z Up** turns
  Klay's Y up into the printer's Z up, so the model stands on the bed as it stands on
  screen. STL files are always in millimetres. Kept with the workspace.
- **Export**: **Export STL** writes every visible subtool at its shown level as one binary
  STL (also Document › Export). **Send to …** writes it to `~/.cache/klay/print/` and opens
  it in the slicer.
- **Slicer**: the slicers found on this computer from their menu entries (packages,
  Flatpaks, Snaps, AppImages added to the menu) and `PATH`: PrusaSlicer, SuperSlicer,
  OrcaSlicer, Bambu Studio, UltiMaker Cura, Creality Print and a few others; two installs of
  one slicer are told apart (Flatpak, AppImage). **Custom** runs a command of your own,
  `{file}` standing for the STL (added at the end without it). **Rescan** looks again.
- **Check Mesh**: what a slicer wants is one closed, manifold surface. The check counts
  triangles, shells, holes (and their open edges), non-manifold edges (three or more
  triangles on one edge) and flipped edges (neighbours wound against each other), for all
  visible subtools and each one. When something is wrong, **Close Holes** and **Remesh** (the
  Mesh palette's tools, on the active subtool) are right there; Subtract / Intersect
  subtools are written as they are, so Make Boolean Mesh carves them first. The result says
  when the model changed since.

Export STL, Send to Slicer and Check Mesh can go on a shelf.

### Sculpting helpers

- **Brush › Auto Masking**: **BackfaceMask** leaves surface turned away from you alone (the
  far side of a thin part); **Topological** keeps a stroke to the surface joined to the point
  under the brush (the next finger or the other lip stays still). Every sculpt, drag, detail
  and paint brush, with symmetry too.
- **Stroke › Modifiers**: **Spacing** (dabs per Draw Size along a stroke; ZBrush's LazyStep).
  Strokes are now followed point by point between pointer events, so a fast stroke leaves no
  gaps. **LazyMouse** (`L`) makes the brush trail the cursor on a string of **LazyRadius**
  pixels, drawn on the canvas, for steady lines; drag brushes follow the cursor directly.
- **Masking**: **Grow / Shrink** move the mask by one ring of polygons (never across hidden
  ones); **Mask By Cavity** masks recessed surface, more where deeper (**Cavity**
  sensitivity); **Mask By Polygroups** masks the borders between polygroups (**Border
  width**; Invert to mask their insides instead).
- **Mesh › Visibility**: **Grow / Shrink** show one more ring of polygons or hide the outer
  ring. **Mesh › Polygroups**: **Auto Groups** gives each connected piece its own group;
  **Group By Normals** starts a new group wherever the surface turns by more than **Max
  Angle** from where the group began (flat faces of a hard surface, patches of a curved one).
- **Symmetry**: **X**, **Y** and **Z** mirrors and **Radial** about X, Y or Z with a
  **RadialCount**, in any combination (up to 64 copies; the palette shows how many). Every
  copy of a brush makes its own whole dab, one after another, so where copies overlap (a big
  brush, a stroke on a symmetry plane) both apply and the surface stays whole; drag brushes
  add up every copy's move there. Masks, mask shapes, curve reliefs and CurveTube follow
  every copy and the brush ring is drawn at each. Clip Line mirrors across X only.

### Brushes, alphas and textures

Press **B**, click the current brush thumbnail or choose **All (49)** for the catalog:
Quick Pick, search, All/Sculpt/Mask/Detail/Clip/Insert filters, alphabet filter, brush grid and a
large hover preview. The docked Brush palette has **Sculpt / Mask / All** tabs and a
three-column grid of working brushes; the last row holds the active and recent brushes.
Previews are original 160 px sculpted clay studies in the current material, including the
brush's subtract variant where applicable.

**ZBrush brush keys**: press **B**, then the brush's two letters, as in ZBrush. Every tile in the
catalog shows its code; the first letter keeps only the brushes whose code starts with it
(Backspace takes it back), the second picks the brush and closes the catalog. ZBrush's codes
are kept: **B S T** Standard (Draw), **B M V** Move (Grab), **B D S** Dam Standard,
**B C B** Clay Buildup, **B C L** Clay, **B C T** ClayTubes, **B H P** hPolish, **B T D** Trim
Dynamic, **B I N** Inflate, **B P I** Pinch, **B F L** Flatten, **B M T** Move Topological,
**B S H** Snake Hook, **B M P** Mask Pen, **B Z M** ZModeler. Brushes of Klay's own get a code
from their name (Zipper **Z I**, Clip Line **C C**). Clicking in the search box types there instead.

- Draw, Smooth, Grab, Flatten and Inflate retain shortcuts **1–5**.
- **6–9** select Dam Standard, Pinch, Clay and Clay Buildup. The catalog also provides
  Crease, Trim Dynamic, Polish and Paint. The library also includes:
- New standard tools: **Blob** builds soft domes; **Layer** caps thickness within each stroke
  (release to build another layer); **Fill** raises low areas to a plane; **Scrape** shaves peaks.
- New advanced tools: **Nudge** pushes in the stroke direction; **Sharpen** enhances local
  detail; **Elastic** pulls a broad area with directional deformation; **Snake Hook** pulls a
  tapered tip. Snake Hook uses GPU deformation; Elastic runs on the CPU. These tools move
  existing vertices—Divide before long pulls for finer topology.
- Dam Standard cuts a narrow groove; Pinch contracts the surface; Crease combines both.
  Clay and Clay Buildup deposit clay, Trim Dynamic planes raised areas and Polish smooths
  the local plane. **Alt** reverses direction where applicable. **Shift** temporarily smooths.
- **FormSoft / FormHard** build gentle rounded forms or firm planar forms. **Zipper** draws
  two staggered tooth rows and a recessed seam; **Stitch** draws thread-like marks; **Rake**
  draws parallel grooves. These are relief on the existing mesh, with stroke-distance spacing.
  **Divide** for finer detail; they do not insert a separate zipper or thread mesh.
- ZBrush's advanced brushes (0.7): **ClayTubes** lays square, flat-topped clay strips that
  turn with the stroke; **hPolish** flattens with a square, hard edge; **TrimAdaptive** trims
  to the plane right under the brush centre, so it follows curved forms; **Planar** flattens
  onto the plane where the stroke started, so one stroke cuts one facet; **TrimSmoothBorder**
  trims and softens the edge of the cut; **MalletFast** hammers flat-bottomed square dents
  (Alt raises a plateau). **Move Topological** pulls only the surface connected to the click
  point (lips, fingers and eyelids next to each other stay apart) and runs on the GPU like
  Grab. **Inflate Balloon** swells round, **Magnify** spreads and swells (Alt contracts),
  **Orb Cracks** cuts a sharp crack whose depth wanders. **SmoothValleys** / **SmoothPeaks**
  smooth only recessed / raised areas and leave the rest sharp.
- **Layer** builds a capped thickness per stroke. It is now in the compact Sculpt working
  set; release and start another stroke to add another layer.
- **Clip Line**: choose it, drag a guide across the model and release to push one side onto
  the line. It can start outside the model. **Alt** flips the clipped side, **Shift** snaps
  the angle and **Escape** cancels. The arrow shows the side that will move. It preserves
  topology, vertex paint, masks and hidden regions. This is straight-line clipping; it does
  not cut polygons or add curve control points. Ctrl+Shift also draws the line when this
  tool is selected; other tools retain Ctrl+Shift visibility selection. With **X symmetry**
  the half under the line is clipped to the guide and the other half gets its mirror image,
  so the result is symmetric from any view. Cuts can cross earlier cuts at any angle: a
  vertex moved onto the new cut stays inside the model's cross-section there, so an earlier
  cut face is never pushed back out.
- Click the left **Alpha** or **Texture** thumbnail, or open its menu-bar palette, to select
  a built-in pattern or **Import Alpha… / Import Texture…**. PNG, JPEG and BMP are supported.
  Alpha luminance and transparency shape the stamp. Texture **Relief** modulates sculpting;
  **Tiling** controls repetition within the brush footprint.
- **Z** sculpts, **RGB** paints color only and **Z+RGB** does both. Selecting Paint activates
  RGB mode. Choose color with the left HSV picker; RGB Intensity controls paint opacity.
  Textures supply their sampled color when painting, tinted by the chosen color. Masks,
  hidden vertices and X symmetry are respected by both paint and sculpting.
- Like ZBrush, every brush keeps its own **Z Intensity, Focal Shift, RGB Intensity and
  Zadd / Zsub**; switching brushes brings back the values you last used with that brush.
  **Draw Size** is shared by all brushes. Its sliders run from 0.01 to 2.0 on a gentle
  curve (size = 0.01 + 1.99 · position²): fine steps for small brushes, half the slider is
  about 0.5, and the top no longer jumps the way a logarithmic slider did. Smooth and the mask brushes start at full Z
  Intensity, the other brushes at 0.5. Holding **Shift** shows Smooth's own values in the
  panels, and changing Z Intensity while Shift is held sets Smooth's; Shift strokes and Ctrl
  masking use the Smooth and mask brush's own intensity. The values last for the session.
- **Focal Shift** changes the falloff from a soft center to a broad plateau. Its neutral
  value retains the original falloff. The Space palette places **Draw Size → Z Intensity →
  Focal Shift → RGB Intensity → brushes**, with a color picker on the left.

### Materials and MatCaps

Click the material thumbnail on the left shelf, or open the Material palette. Klay's four
procedural clays (Warm clay, Cool clay, Terracotta, Polished bronze) are joined by twelve
MatCaps: **Red Wax**, **Skin**, **Gray**, **White**, **Form** (hard key, dark shadow side and a
cool rim, for reading planes), **Chrome**, **Toy Plastic**, and for reading detail **Studio
Gray** (warm key, cool fill, like ZBrush's BasicMaterial), **Cavity White** (darkens hard
wherever the surface turns away: creases and pores read at once), **Slate** (cool, with a crisp
highlight that catches small forms), **Rim Dark** (dark body, bright rim: silhouettes) and
**Normal** (normal directions as colour, to check surface flow). A MatCap is a picture of a lit
sphere looked up by the view-space normal, as in ZBrush, so the lighting stays fixed to the
view while the model turns. Each one is two 256 px images painted by Klay at start-up
(`src/matcap.rs`, no borrowed artwork): a diffuse layer that the surface colour multiplies,
and a specular layer added on top. Polypaint and polygroup colours therefore tint a MatCap
the way they tint clay, and the highlight stays white. Brush thumbnails are shaded with the
current material.

### Masking brushes

Select **Mask** in the side Brush panel, open the catalog's Mask category, or use
**Masking → Mask type**. The six tools are **Mask Pen, Mask Soft, Mask Rect, Mask Lasso,
Mask Circle and Mask Line**. Mask Soft uses a gentler, broader profile. The pen tools
sample the chosen alpha and focal shift.

A selected mask works with **left drag**; **Alt** erases. When a sculpt brush is active,
**Ctrl** temporarily uses the last chosen mask brush. Z Intensity controls mask strength.
As in ZBrush, the brush panels show the brush a held key strokes with: **Ctrl** shows the
mask brush (picking a mask brush while holding Ctrl makes it the Ctrl mask brush) and
**Shift** shows Smooth; releasing the key shows your brush again.
Rectangle, ellipse and lasso masks select through the view depth; Mask Line masks one side
of the guide (reverse the drag to change sides). Live outlines show the region before release.
Masks preserve geometry, paint, hidden regions, X symmetry and the existing multires undo path.

**Ctrl click** still blurs/sharpens on the model or inverts on the background. With Mask Pen
or Mask Soft selected for Ctrl, dragging the background clears the mask; shape brushes draw
their region there instead. **Ctrl+Shift+A** and **Masking → Clear** always clear it.
**Escape** cancels a mask-shape guide. The compact Mask tab and catalog filter make these
brushes visible without expanding the side panel.

Imported image paths and selections save to `$XDG_CONFIG_HOME/klay/brush-assets.v1`
(`~/.config/klay/brush-assets.v1` by default). Keep imported files at those paths to reuse
them. Import uses a native Zenity picker when available and a path-entry fallback otherwise.
Large images are reduced to a maximum 1024 px side for sampling; inputs over 8192 px per
side or 64 MB are refused.

Color is vertex paint: use **Divide** for finer texture detail. Paint follows subdivision,
parked subtools, Remesh, undo/redo and `.klay` saves. This release does not create UV
maps or texture-image exports; **OBJ export contains geometry, normals and groups**. The baked
sculpt and paint remain in the project even when the source stamp image is unavailable.
`../ui-brush-panel.png`, `../ui-brush-catalog.png` and `../ui-quick-menu.png` show the native 0.4.0 controls.

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

`src/branding.rs` embeds the approved `assets/icon/klay-512.png` for the native window icon,
menu-bar control and About dialog. Version and repository metadata come from `Cargo.toml`.
`../ui-about.png` shows the native About dialog.

## Versioning and Linux packaging

Application versions follow semantic versioning from **0.2.0**: minor releases add features,
patch releases fix/polish and major releases introduce incompatible application changes.
`Cargo.toml` and `Cargo.lock` define the version shown by About and `--version`; releases are
recorded in `CHANGELOG.md` and `handoff.md`. Project format **4** is versioned independently.

```bash
cargo build --release --offline --locked
python3 scripts/install-desktop.py  # install the current build's launcher and Coral Fold icon
bash scripts/package-linux.sh      # create dist/Klay-0.4.0-<architecture>.AppDir
```

The executable embeds the window icon. The installer adds an application-menu launcher,
hicolor icons and GIO custom-icon metadata on desktops that support it, so the local Linux
executable also displays Klay's icon in the file manager. Rerun the installer after replacing
the binary. The versioned AppDir bundles the executable, launcher, icon and docs; run its
`AppRun`. It requires compatible Linux graphics/system libraries and is not an AppImage.

## Controls

| Input | Action |
| --- | --- |
| Left drag on the model | Sculpt with the current brush |
| Alt + left drag on the model | Alternate brush (carve), like ZBrush's ZSub |
| Shift + left drag | Smooth |
| Ctrl + left drag on the model | Paint a mask (Draw Size, Z Intensity, X symmetry); masked clay goes dark and stays still |
| Ctrl + Alt + left drag on the model | Erase the mask |
| Ctrl + click on the model / Ctrl + Alt + click | Blur / sharpen the mask |
| Ctrl + click on the background / Ctrl + drag on the background | Invert / clear the mask (the canvas border counts as background); with Remesh on, the drag also remeshes again on release (ZBrush: re-DynaMesh); with nothing to clear and Remesh off it says so |
| Ctrl + `I` / Ctrl + `A` / Ctrl + Shift + `A` | Invert / mask all / clear the mask (invert and mask all leave hidden polygons alone) |
| Ctrl + `H` | Show or hide the mask |
| Ctrl + Shift + drag | Keep what the rectangle covers visible, hide the rest (ZBrush SelectRect); hold Alt during the drag to hide what it covers instead (the rectangle turns red) |
| Ctrl + Shift + click on the model | Isolate the polygroup under the cursor (Alt: hide it) |
| Ctrl + Shift + click on empty canvas | Show everything |
| Ctrl + Shift + drag on empty canvas, around nothing | Invert visibility |
| Ctrl + `W` | With a mask: one new polygroup of the masked polygons, and the mask is cleared (Group Masked); without one: of the visible polygons (Group Visible) |
| Shift + `F` | Polygroup colours and the polygon frame (ZBrush PolyF). A new Sphere, Ring or Plane is one polygroup, shown in blue; a Cube has one per face, a Cylinder one for the wall and each cap. Ctrl + W or the Polygroups palette makes more |
| Left drag on empty canvas | Rotate (hold Shift to snap to axis views) |
| Alt + left drag on empty canvas | Move |
| Left drag in the border outside the canvas frame | Always navigates, even over the model, in every mode (as on empty canvas) |
| Alt + click, release Alt, drag | Zoom (ZBrush style; press Alt again to move) |
| Shift + click on empty canvas, release Shift, drag | Roll about the view axis |
| Right / middle drag | Rotate, anywhere (ZBrush right-click navigation) |
| Alt + right / middle drag | Move |
| Ctrl + right / middle drag | Zoom |
| Scroll | Zoom towards the cursor (Ctrl + scroll: brush radius) |
| Space (hold) or right-click tap | Quick menu: left color picker; Draw Size, Z Intensity, Focal Shift, RGB Intensity, brushes, then mode buttons |
| `S` / `U` / `I` / `O` | Draw Size / Z Intensity / RGB Intensity / Focal Shift popup at the cursor |
| `[` `]` | Brush radius |
| `1`–`9` | Draw, Smooth, Grab, Flatten, Inflate, Dam Standard, Pinch, Clay, Clay Buildup |
| `B` | Searchable brush catalog; Mask, Detail and Clip categories |
| Selected Mask brush, left drag / Alt + left drag | Apply / erase the mask; Ctrl temporarily restores the last mask brush while sculpting |
| Selected Clip Line, left drag | Push one side onto the guide on release; Alt flips, Shift snaps, Escape cancels |
| `Q` `W` `E` `R` | Draw / Move / Scale / Rotate mode |
| `W` / `E` / `R`, left drag on the active subtool | Draw a Transpose action line from the surface under the press (Shift snaps it to 45°) |
| `W`, drag the line's middle circle | Move the active subtool in the view plane |
| `E`, drag the line's end circle | Scale about the start circle by the change in distance to it (start circle: about the end) |
| `R`, drag the line's end circle | Rotate about the start circle around the view axis (start circle: about the end) |
| Drag a circle's outer ring / the line itself | Move that end of the action line / the whole line; the model stays |
| `X` | Symmetry across X (Y, Z and Radial: Symmetry palette) |
| `L` | LazyMouse |
| `Y` | Gizmo 3D / action line in Move, Scale, Rotate |
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

- `curve.rs`: curve strokes. The screen path is resampled every pixel or finer and
  smoothed in two passes: a light one takes out fast jitter so the path's length is honest
  (a shaky path is much longer than the line it follows, and a window measured along it
  would smooth a shaky stroke less than a steady one), then a Gaussian of Curve smooth
  times the brush's screen radius. Each end keeps its place along the stroke but takes the
  averaged line's place across it, and the path is mirrored through it so a straight run
  stays straight. The result is raycast every tenth of the brush's screen radius and
  resampled every 4 % of Draw Size in the world, with averaged normals and tangents kept
  in the surface. `band` finds each vertex's nearest point on the curve (chunks culled by
  their bounds, segments by chunk) and its arc length and signed distance across, so
  Zipper/Stitch/Rake are evaluated once per vertex in the curve's frame. With X symmetry
  the mirrored curve is searched too and the nearer one wins. A bend moves the curve's
  projected points with a smooth falloff from the grabbed point, restores the chunks the
  curve has touched to their first-application state, lays the points onto that surface
  again and re-applies; the bend's own stroke keeps the pre-bend state for undo.
- `curve_tube.rs`: CurveTube geometry. The curve's points are resampled into rings one
  side's width apart; each ring's frame starts along the surface normal and is carried to
  the next by the rotation between their tangents (no twist), and quarter-circle rings and
  a pole close each end. The tube is added after the model's last chunk
  (`SculptMesh::swap_tail`): the model's arrays, adjacency tables and chunks are only cut
  back and extended by the tube's own, so adding, bending or undoing a tube costs about the
  tube, not the model. The renderer gives the level's buffers room the first time and then
  writes only the tube's slice (`Gfx::swap_tail`). Undo keeps only the tube
  (`UndoStep::Part`). A bend re-raycasts pulled points against the model without the tube
  (`raycast_in`).
- `tablet.rs`: pen pressure on X11. winit does not report it on Linux, so Klay opens a
  second connection to the X server that only listens: XInput2 raw events (motion and
  buttons, from every device) and device changes, selected on the root window. Devices with
  an axis labelled "Abs Pressure" (or, unlabelled, valuator 2 of a device named like a pen)
  are pens; the device of the latest raw event decides whether the next dab has a pressure
  (its last value, as drivers only send the axes that changed) or none (a mouse). A thread
  owns the connection and blocks on it, so the events, sent for every pointer move on the
  screen, never pile up, even while Klay is minimised; the window reads the shared state on
  each pointer event and scales a copy of the brush per dab (`Brush::at_pressure`).
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
- `brush_asset.rs`, `asset_library.rs`, `brush_preview.rs`, `brush_ui.rs`: bounded image decoding,
  shared alpha/texture sampling, local library persistence, original clay previews, catalog and
  Space controls. `brush_ui` is a child module of `workspace.rs`.
- `surface_tools.rs`: screen-space masks and homogeneous screen-line clipping, retaining
  vertex identity, paint, visibility and masks. `detail_brush.rs` provides continuous repeating
  relief; `native_tests.rs` verifies masks/clipping through actual input and GPU readback.
- `paint.rs`: premultiplied sRGB vertex paint, chunk snapshots and blending. Multires edits
  carry down to lower levels and apply color deltas upward while retaining existing fine detail;
  all affected chunks are captured for exact undo. Remesh interpolates paint from the old surface.
- `primitive.rs`: the starting meshes (Sphere3D, Cube3D, Cylinder3D, Ring3D, Plane3D). Sphere,
  Cube and Cylinder are the surface of a lattice box mapped onto the shape, so `--segments`
  still gives the old cube sphere bit for bit. The box is built face by face, so the Cube's
  and Cylinder's starting polygroups are runs of that order (`Primitive::groups`).
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
  Delete and reordering. Under the list, **Save** (the active subtool alone, with all its
  levels, as a one-subtool `.klay` that Document › Open also reads; the file dialog offers
  the subtool's name), **Load** (pick one or more `.klay` files; every subtool in them is
  added to the end of the list, parked, without changing the active one; a bad file adds
  nothing) and **Export OBJ** (the active subtool at its shown level, to an `.obj` you pick); Deformation moves and sizes the active subtool exactly at every
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
  when it is over 1.1 times the next) and darkens the surface by about half on a line. Where
  polygons get under 3–7 pixels the lines blend into the frame's average darkness (about two
  one-pixel lines per polygon, at most 42 %), so a dense level far away keeps an even, light
  tint that shows how dense it is instead of shimmering, vanishing or turning black. No extra memory is used. With nothing hidden and PolyF off, the buffers and the
  pipeline are exactly the ones used before. Undo stores the previous groups or visibility
  of every level run-length coded; parked subtools and project files keep them the same way,
  and stand-ins of parked subtools leave hidden triangles out. OBJ export writes polygroups
  as OBJ groups.
- `wedge.rs`: Remesh (Mesh › Remesh; called Wedge before 0.9), a DynaMesh-style remesher. Every triangle is rasterised onto the lines of
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
  subtool's stack as a new base level; the old stack is kept packed for undo. Remesh refuses
  subtools with subdivision levels (as ZBrush does for DynaMesh; Geometry has Del Higher and
  Del Lower now) and open surfaces (Plane3D). Hidden triangles are remeshed too and the result
  is all visible.
- `project.rs`: `.klay` project files hold every subtool in the same packed form parked
  subtools use (base mesh in chunk order, replay labels, sculpted offsets), written as a few
  large binary blocks behind a magic number and version, through a temporary file that is
  moved into place. Version 2 added the shown level's mask, version 3 the polygroups and
  hidden triangles of every level, and version 4 adds per-level vertex paint; versions 1–3
  still open as unpainted clay. Reading checks every length against the bytes left before allocating;
  rebuilding checks index bounds, the chunk table, manifold edges and every level's labels
  and offsets, and turns anything else that fails into an error. Open rebuilds each
  subtool once to bake its stand-in and swaps the model only when all of them succeeded.
  Undo history is not saved. Export OBJ writes every visible subtool at its shown level as
  its own object, rebuilding parked ones one at a time.
- `stl.rs`: STL in and out. `parse` reads binary (the size matches the header's count, even
  when the header starts with `solid`) or ASCII and welds corners by exact position (−0 as 0)
  into an indexed mesh, dropping flat and repeated triangles. `BinaryWriter` writes binary
  STL to any `Write` one triangle or mesh at a time (each point through a placing function),
  `patch_count` fixes the count afterwards for seekable files, `write_binary` does a mesh in
  one call. Meant to be reused by other tools (a slicer).
- `print.rs`: the 3D Print palette's logic. `Settings` (unit, mm per Klay unit, Z up, slicer,
  custom command; a `print|…` workspace line) with `place` (Klay point → printer mm);
  `check` counts edges per vertex in parallel (each edge at its lower vertex: one triangle
  open, three or more non-manifold, two running the same way flipped) and holes and shells
  with union-find; `detect` reads desktop entries; `command_for` fills `%f %F` (path),
  `%u %U` (`file://` URL, which Flatpak's `--file-forwarding` needs) or `{file}`; `launch`
  starts the slicer and reaps it on a thread.
- `render.rs`: positions, packed normals, indices and fixed-point normal accumulators live
  on the GPU, with a separate four-byte vertex-paint buffer (about 57 bytes per vertex total), one set per subdivision level that has been shown, so
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
- GPU culling (`bounds.wgsl`, `cull.wgsl`): the active subtool is drawn as one indexed draw
  per chunk through a single `multi_draw_indexed_indirect`, and a compute pass empties the
  draws of chunks that cannot be seen: outside the view or the document frame, or with every
  triangle facing away (a normal cone per chunk, as in meshoptimizer's cluster cones). Each
  chunk's box and cone are refitted on the GPU after the normal pass, for the chunks that
  moved or were re-shaded and their edge neighbours, so GPU Grab, Transpose releases and
  every brush keep them right without CPU work. Draws stay in chunk order, so a culled frame
  is pixel-identical to one whole draw (`--selftest-cull` checks this after camera moves, a
  GPU Grab, a subtool move, hidden polygons and PolyF). Culling pauses while a Transpose
  preview moves vertices in the vertex shader, skips the back-face test under Live Boolean
  (which draws back faces), and gives PolyF each draw's first triangle through its first
  instance (`vs_active`). `--render triangles` draws the subtool whole for comparisons.
- Point splats (`splat.wgsl`, `fs_splat` in `shader.wgsl`): the cull pass also estimates
  each visible chunk's vertices per pixel (its vertex count over its box's screen
  rectangle, which errs towards triangles) and from 2 per pixel draws the chunk as points
  instead. A compute pass projects every front-facing vertex of those chunks into the
  pixel it lands in and keeps the nearest with a 64-bit `atomicMax` of
  `(inverted depth << 32) | vertex`; a full-screen pass right after the background shades
  each pixel's winner with the same `surface()` as triangles (MatCap, paint, mask, brush
  ring) and writes its depth, so the triangles of nearer, coarser chunks sort against it.
  An empty pixel with three or four filled neighbours takes the nearest of them. Points
  are off with PolyF or Live Boolean on, for chunks with hidden polygons, on GPUs without
  64-bit atomics, and with Performance › Drawing › Points for dense meshes unticked
  (saved with the workspace; the palette also shows what the last frames drew).
  `--selftest-splat` compares points with triangles for empty pixels and colour.

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

### GPU culling (400-segment sphere, `--novsync --frames --camera-distance K`)

Frame times with culling, against 0.25 drawing the whole level. Without culling the frame
costs the same at any distance (triangle setup bound, measured from 12.6 to 16.5 ms at
61M), so 0.25's column is one number per level.

| Level, camera | 0.25 (whole level) | Culled | Drawn |
| --- | --- | --- | --- |
| 15.4M vertices, close up (0.35×) | 3.9 ms | 1.1 ms | – |
| 15.4M vertices, framed (1×) | 3.9 ms | 2.2 ms | – |
| 15.4M vertices, far (4×) | 3.9 ms | 2.8 ms | – |
| 61.4M vertices, close up (0.35×) | 15.3 ms | 1.4 ms | 25 of 16 384 chunks |
| 61.4M vertices, framed (1×) | 15.2 ms | 8.0 ms | 36% of the triangles |
| 61.4M vertices, far (4×) | 15.2 ms | 10.4 ms | 47% of the triangles |

From far away more chunks are drawn (small chunks on screen turn less, so fewer are wholly
back-facing) and every drawn triangle is far under a pixel: point splats are for that
case.

### Point splats (400-segment sphere, `--novsync --frames --camera-distance K`)

| Level, camera | 0.25 (whole level) | Culled triangles | Culled, dense chunks as points |
| --- | --- | --- | --- |
| 15.4M vertices, close up (0.35×) | 3.9 ms | 1.1 ms | 1.1 ms (12 chunks, all triangles) |
| 15.4M vertices, framed (1×) | 3.9 ms | 2.2 ms | 1.1 ms (1 529 chunks, 5.7M points) |
| 15.4M vertices, far (4×) | 3.9 ms | 2.8 ms | 1.2 ms (1 978 chunks, 7.4M points) |
| 61.4M vertices, framed (1×) | 15.2 ms | 8.0 ms | 1.4 ms (5 862 chunks, 22M points) |
| 61.4M vertices, far (4×) | 15.2 ms | 10.3 ms | 1.5 ms (7 732 chunks, 29M points) |
| 61.4M vertices, very far (8×) | 15.2 ms | – | 1.6 ms (8 042 chunks, 30M points) |

About 1 ms of every frame is the rest of the frame (interface, present). Against triangles,
`--selftest-splat` finds no empty model pixels on the framed sphere (5 of 252 438 at the
edge of a long Grab pull) and a mean colour difference under 1/255.

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

### Remesh (same 4-core VM, `--bench 400 --divide 2`)

Resolution chosen for about a million output vertices (460 voxels across two units);
"chunk" is building the chunked mesh (`SculptMesh::from_raw`) from the result.

| Input | Remesh | + Project (Blur 2) | Chunk |
| --- | --- | --- | --- |
| 960k-vertex sphere (sculpted) → 997k vertices | 0.31 s (scan 0.13, cells 0.03, surface 0.15) | 0.44 s | 0.16–0.21 s |
| 15.4M-vertex level → 1.0M vertices | 0.67 s (scan 0.4, surface 0.2) | 1.1 s | 0.19–0.24 s |

In the window (`--selftest-remesh`, software GPU): a 4k-vertex primitive remeshes at the default
resolution 128 to 77k (sphere) or 98k (cube) vertices in 45–50 ms, remeshing again at 192 takes
90–120 ms, and undoing both takes about 10 ms. Twice the resolution gives about four times
the vertices; the output is closed and manifold (tested on primitives, overlapping shells and
a sphere folded through itself), keeps the volume to within a voxel, and divides like any
mesh.

ZBrush's DynaMesh decisions and what Remesh does:

- **Resolution** (default 128) is relative to two world units, the size of a new primitive,
  so like ZBrush a bigger model gets more polygons at the same value. The section shows an
  estimate from the surface area.
- **Blur** (default 2) and **Project** (off): as in ZBrush, Blur only acts with Project.
- **Groups**: polygroups are always carried over with clean borders. ZBrush's Groups button,
  which splits the result into separate pieces at group borders, is not implemented.
- **Remesh again gesture**: in ZBrush, Ctrl + drag on the background clears the mask, and
  while DynaMesh is on it remeshes. Klay does both: with Remesh off it clears the mask (and,
  with no mask to clear, says how to turn Remesh on), with Remesh on it remeshes again, and the new surface starts without a mask (undo brings back the old
  surface with its mask). A Ctrl click on the background still inverts the mask.
- **Merge of overlapping subtools and Alt carving** (subtractive inserts) are not done: they
  need a SubTool Merge first. Within one subtool, overlapping parts already merge.
- The Remesh on/off state is per subtool and not saved in project files. **Update** remeshes
  again at the current settings, like the gesture.

The remaining limit for CPU brushes is host-to-GPU upload bandwidth: about 5 GB/s on
this machine, so moving a third of a 13.5M-vertex mesh on the CPU would cost 12 ms of
upload per frame. That is why Grab runs on the GPU; the other brushes will follow.
