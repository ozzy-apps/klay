# Changelog

## 0.25.0 — 2026-10-25

- Every shelf item can go anywhere, as in ZBrush: the top, left, right and a new bottom shelf are lists of single items (the four mode buttons, Sculptris Pro, Live Boolean, Z+RGB / RGB / Z, RGB Intensity, Zadd / Zsub, Z Intensity, Focal Shift, Draw Size, Undo, Redo, the Brush / Stroke / Alpha / Texture / Material thumbnails, the colour picker, Alternate, the view buttons, separators and springs) plus any brush or palette command dragged on. In Customize mode drag an item to another place or shelf, or onto the canvas to take it off; taken-off built-in items wait under Preferences › Interface to be dragged back. Items draw to fit (half-height pairs on the top and bottom shelves, full width on the side shelves; sliders on a narrow shelf show their value, the name on hover). My shelf's buttons move onto the top shelf; workspaces from 0.23–0.24 open with their shelves converted.
- Preferences › Config (ZBrush's Store Config): Store Config keeps the whole interface (palettes, trays, shelves, theme, custom themes, scale, document look) and, when ticked, the model as the startup document; Klay then starts from them instead of the last session (Start with the stored config). Load Config brings the stored interface back, Factory settings (click twice) returns to Klay's own, Delete stored removes both files.
- Update checks: at start and every six hours Klay asks GitHub for the latest release of its repository (with the system's curl, in the background); a newer one shows a card with Download (this platform's file, or the release page), Later and Skip this version. Preferences › Updates turns it off, checks now and shows the result. Releases must be published on a public GitHub repository for this to find them.
- Packages for the release: `scripts/package-deb.sh` (.deb), `scripts/package-flatpak.sh` (a Flatpak bundle on the Freedesktop 25.08 runtime, app id nl.co.ozzy.Klay, with AppStream metadata) and `scripts/package-appimage.sh` (AppImage, with appimagetool); package binaries are stripped (24 MB instead of 241 MB). `scripts/package-linux.sh` also writes a .tar.gz of the AppDir.
- A short README with screenshots; the full manual moved to docs/guide.md.
- New flags: --fake-update X (show the update card), --no-update-check.
- Verification: 281 release tests (new: moving shelf items between shelves and taking them off on the canvas, brushes and commands dropped on the bottom shelf and run from it, old shelf lines converted, stored config round trip, update card and skip, release JSON parsing, version comparison); every native self-test passes on the GPU.

## 0.24.0 — 2026-10-25

- My shelf (ZBrush's custom shelf): in Customize mode (Preferences › Interface › Customize) drag brushes (Brush palette and B catalog), right-shelf buttons and the outlined palette buttons onto it: mask operations, Divide, Del Lower / Higher, Remesh, ZRemesher, Del Hidden, Close Holes, Mirror And Weld, the SubTool buttons, Show all / Invert / Group Visible, Undo / Redo. Its buttons do what the originals do (toggles light up, brushes show their preview). Drag them to reorder, click to remove while customizing. It sits in the top shelf as a group of its own (movable and hideable like the others), two rows, narrowing its buttons to leave room for Undo / Redo; hidden while empty. Saved with the workspace.
- Customize mode shows a banner at the top of the canvas with Done.
- Previous / next theme: small arrows at the right end of the menu bar, with the current theme's accent between them.
- Menu-bar dropdowns grow down to the status bar before they scroll (Preferences showed only its first theme row) and are 360 px wide.
- A tray no longer widens and slides its content sideways: Deformation's Size row and Masking's buttons fit narrow trays (buttons in fixed cells that clip their labels).
- Fix: a drop on one kind of target no longer throws away a drag of another kind.
- New flags: --menu NAME, --ui-scale X.
- Verification: 276 release tests (new: every palette and section open in a 200 and 284 px tray leaves the shelf in place; dragging the right shelf's Frame and a brush onto My shelf, reordering it, saving it, and its Frame button framing; the theme arrows; the Preferences dropdown showing every theme); every native self-test passes on the GPU.

## 0.23.1 — 2026-10-25

- The border round the document frame (always empty canvas that navigates, even over the model) is 40 points wide instead of 16, and **Document › Back › Border width** sets it from 8 to 120; saved with the workspace (older workspaces get the new default).
- Verification: 273 release tests (the layout round trip keeps the border width and reads workspaces from before it); the document, transpose, mask and clip self-tests pass on the GPU.

## 0.23.0 — 2026-10-25

- 17 new brushes (66 in all):
  - Sculpt: Smooth Stronger (four relax passes a dab), Slash (a thin, sharp-lipped cut), Pen A and Pen S (crisp and soft raised lines), Noise (surface noise that settles at its height within a stroke), Gravity (pulls the surface down world −Y), Spiral (a drag brush that twists the surface round the press point).
  - Elastic is now named Move Elastic, as in ZBrush.
  - The Ctrl + Shift family: Select Rect and Select Lasso (keep what the outline covers shown, Alt hides it), Clip Rect and Clip Circle (press the model into the outline all the way through, Alt pushes the inside out), Trim Curve (cut away the arrow's side exactly along the line and close it flat), Trim Rect / Circle / Lasso (keep only what is inside, through the boolean remesher), Slice Curve (cut polygons along the line, the arrow's side a new polygroup) and Knife Curve (two closed pieces with polygroups of their own). Picking one makes it the Ctrl + Shift tool, as in ZBrush; as the current brush a plain drag uses it.
  - Every new brush has a two-letter code (B S S, B S A, B P N, B N O, B G R, B S W, B S R, B S L, B T C, B S C, B K C…); Sharpen moved to S N and Scrape to S B for Select Rect and Slice Curve.
- Interface:
  - F11 switches fullscreen (also Preferences › Interface).
  - Preferences › Interface › UI scale: 80–200 % in steps, saved with the workspace; egui's Ctrl + plus / minus zoom stays in step.
  - Preferences › Theme: seven themes (Klay Dark, ZBrush Classic, Graphite Blue, Moss, Plum, Studio Light, High Contrast) and your own: change a theme's panel, text and accent colours (a built-in one is copied first), name it, make new ones, delete them; every other colour is derived. Saved with the workspace.
  - Preferences › Interface › Customize: drag the top shelf's groups and the right shelf's buttons to new places, click one to hide or show it; Reset shelves puts them back. Saved with the workspace. (Palettes already dock to any tray by their ring button.)
  - About Klay: "Made with love by Ozzy ♥" and "Found a bug? Have a request?" with a mail link to klay@ozzy.co.nl.
- New flags: --theme NAME, --about, --customize (snapshots).
- Verification: 273 release tests (new: exact plane cuts that close flat, slice and knife pieces, ear clipping, prisms; Select, Clip, Trim, Slice and Knife through the input path with undo; Ctrl + Shift remembering its tool; Smooth Stronger, Gravity, Noise and Spiral behaviour plus undo, masks and symmetry for the new sculpt brushes; theme contrast for every preset; layout round trip with scale, theme and shelves; dragging a shelf button in Customize mode; the About lines); every native self-test passes on the GPU.

## 0.22.1 — 2026-10-25

- Pen drag scrolls the brush catalog (B), the Alpha and Texture shelf pop-ups and the History list, like the trays; a drag that started on a brush tile scrolls instead of picking it.
- Verification: 257 release tests (new: a pen-style drag over the catalog grid scrolls it, picks nothing and leaves it open).

## 0.22.0 — 2026-10-25

- ZBrush brush keys: B, then a brush's two letters picks it (B D S Dam Standard, B C B Clay Buildup, B M V Move, B S T Standard, B H P hPolish, B T D Trim Dynamic…). All 49 brushes have a code, ZBrush's own where ZBrush has the brush; the catalog shows each code on its tile, the first letter keeps only the brushes it starts, Backspace takes it back.
- New --brush-catalog (start with the B catalog open).
- Verification: 256 release tests (new: every code unique and found again; B D S and B M P through the key path, with Backspace and a letter no code starts with).

## 0.21.0 — 2026-10-25

- Readable interface, same ZBrush layout (from the "Klay Readable UI" mockup): the interface font is Inter (bundled, OFL) in three weights instead of Ubuntu Light; 13 px controls, 12 px notes, brighter secondary text.
- Three clear levels: palette titles (14 px semibold, accent tick, chevron on the right), section titles (small capitals with a divider), then the controls.
- Sliders keep their name on the bar (ZBrush style) and show the value in a field of its own; the clay fill has an accent rule along its bottom edge instead of a marker through the name.
- One accent for "on": segmented buttons, mode buttons and toggles fill with it; tiles, rows and view toggles that are selected get a dark clay fill and an accent border.
- Controls are 26 px tall on an even spacing; the top shelf has 56 px mode buttons, the right shelf 52 × 50 px buttons, the left shelf thumbnails say what they are (Brush, Stroke, Alpha, Texture, Material) above the image.
- Brush palette: Sculpt / Mask / All (49) tabs and a fixed three-column grid of brush tiles (the active and recent brushes fill the last row); the full catalog opens from All.
- Space menu: a larger colour picker (172 px square) with the hue bar underneath it, then the colour, Swap colors and RGB / Sculpt buttons below.
- Canvas: the projection and subtool name sit in chips, axis labels are red / green / blue, and the brush readout (brush, size, Z intensity, Zadd / Zsub) sits in a chip at the bottom left.
- Trays are 284 px by default (layouts saved at the old 262 px default open at the new width); buttons that share a row clip their label instead of widening a narrow tray.
- Sculptris Pro crash fix: undoing a Sculptris Pro stroke could leave the mesh in a different internal order, so undoing further into an earlier sculpt, mask or paint stroke crashed. Its undo now restores the exact layout (chunks carried over by number, the rest stored), every undo step checks that it still fits the mesh and is skipped with a warning when it does not, Sculptris Pro stops adding detail at 8 million vertices, and running out of GPU memory warns instead of closing Klay.
- New --snapshot-ui (with --snapshot FILE: the whole window, interface included) and --space-menu (start with the Space menu open).
- Verification: 254 release tests (new: Sculptris Pro on a low-poly model at the highest Detail with mixed sculpt, mask and Sculptris strokes and random undo / redo, the exact state checked at every step); every native self-test passes on the GPU.

## 0.20.0 — 2026-10-25

- ZModeler: Bevel (an edge loop chamfered into a flat strip, drag for width), Spin (an edge turned round its two polygons), Bridge (two clicks join two polygons by a tube of quads); every action follows symmetry (mirror and radial), mirrored.
- ZRemesher: AdaptiveSize (0–100, default 50): smaller quads where the surface curves more, larger where it is flat (half to twice the even size), the total kept near the target.
- Sculptris Pro: a stroke's undo holds only the triangles it changed (by position) instead of a copy of the subtool: 1–4 MB instead of ~35 MB a stroke at two million triangles.
- Verification: 253 release tests (new: bevel of a rim and of a closed loop, spin both ways, bridge through a cube; symmetric QMesh, Spin and two-click Bridge through the input path; adaptive density on a big and a small ball; Sculptris undo by region restoring the same surface); every native self-test passes on the GPU.

## 0.19.0 — 2026-10-25

- ZModeler brush (catalog: 49 tools): hover a point, edge or polygon (lit up) and click or drag. Polygons: QMesh, Extrude, Inset, Delete, Polygroup, on A Single Poly, Polygroup All or Polyloop; edges: Insert an edge loop (click to place, drag to slide), Delete the edge or its loop; points: Move. One undo step each; subtools without subdivision levels.
- Klay's triangles are paired into quads as PolyF shows them, and the pairing is kept through ZModeler actions (`SculptMesh::from_raw_tracked`).
- New --selftest-zmodeler.
- Verification: 251 release tests (new: quads pair back exactly; extrude, inset, delete, regroup and an edge loop round a cube keep it closed with the right counts and volume; the loop dissolves back; QMesh and Insert through the input path with undo); every native self-test passes on the GPU.

## 0.18.0 — 2026-10-25

- ZRemesher (Geometry › ZRemesher): automatic retopology into all-quad meshes whose edge loops follow the form (Instant Meshes: four-way direction field and quad lattice on a multi-level graph of surface samples, lattice points extracted as quads, stray diagonals merged away, holes closed, every face split through its middle). Target Polygons Count with Half / Same / Double; KeepGroups holds loops along polygroup borders and puts border vertices and corners exactly on the old ones. One undo step.
- New --zremesh K flag (ZRemesher to K thousand quads at startup).
- Speed: a 960k-vertex sphere to 5k quads in 0.7 s, to 30k quads in 2.0 s.
- Verification: 247 release tests (new: a sphere to even, closed, mostly regular quads near the target count on the sphere; ZRemesher as one undo step through the app and its refusal with levels); every native self-test passes on the GPU.

## 0.17.0 — 2026-10-25

- Booleans between subtools: Add / Subtract / Intersect per subtool (Mesh › Boolean, marked in the SubTool list), combining the visible subtools top to bottom.
- Live Boolean (top shelf, Mesh › Boolean): the viewport draws only the result's surface, carved walls turned inside out, from a 96³ distance volume per subtool; a Transpose or Gizmo drag of the active subtool moves its cut live.
- Make Boolean Mesh: the result as a new closed subtool, built by Remesh's exact winding-number scan with the boolean rule per grid line and a full dual-contouring fit where subtools meet (sharp edges); polygroups and polypaint kept; the inputs are hidden.
- Projects: format version 6 saves each subtool's boolean operation (1–5 still open).
- New --selftest-boolean and --snapshot-frames N.
- Verification: 245 release tests (new: the fold of operations; made meshes' volumes add up for subtract, intersect and union and stay closed; distance volumes; Make Boolean Mesh through the app; version 6 round trip); every native self-test passes on the GPU, the live view within 27/255 of the made mesh at every probe.

## 0.16.0 — 2026-10-25

- Sculptris Pro (top shelf, Brush › Sculptris Pro): strokes retessellate the surface under the brush (split long edges, collapse short ones, flip toward valence six, relax along the surface), with triangle size following Draw Size and a Detail slider. Masks, hidden polygons, open borders and polygroup borders are kept; symmetry copies retessellate too; one undo step per stroke; subtools without subdivision levels and layers.
- Faster local mesh rebuilds: only the chunks a change touches are put together again, the rest copied and renumbered in parallel (about 4× quicker than a full rebuild at a million vertices), and the GPU copy is rewritten in place from the first change.
- Fixed a deadlock: built-in alphas (0.14) were painted lazily on rayon threads, and a first use inside a brush's parallel loop could wait on itself. They are now painted on plain threads.
- New --selftest-sculptris.
- Verification: 241 release tests (new: refining under a brush and coarsening with masks and polygroup borders kept, the surface closed and outward; patched meshes equal to full rebuilds, tables and chunks consistent, a grown chunk split; a Sculptris Pro stroke through the input path with undo/redo and the refusal with levels); every native self-test passes on the GPU, Sculptris Pro at 1.7k to 1.9M triangles.

## 0.15.0 — 2026-10-25

- Squash and stretch along one axis: Transpose Scale mode's middle circle (along the action line), Gizmo 3D axis boxes, and Deformation › Size X / Y / Z toggles. Flattening a sphere works; masks hold; live preview; one undo step.
- Gizmo 3D pivot and orientation: Alt + click the model (on the surface, Y along the normal), Alt + drag a handle (the gizmo alone), Unmasked Center and Reset; the gizmo rides along with its moves, turns and undo.
- Tablet Pressure per brush: Global or the brush's own Size / Z Intensity toggles and curves.
- IMM Primitives (Insert Mesh) brush: drag to insert a Sphere, Cube, Cylinder or Ring on the surface, sized by the drag and as dense as the model; new polygroup, the rest masked, symmetry copies, one undo step (catalog: 48 tools; the Curve filter is now Insert).
- New --selftest-insert.
- Verification: 235 release tests (new: stretch maths, inverse, matrix and masked weights; Size on picked axes; middle-circle squash through the input path; gizmo axis boxes, turned gizmos, Alt-drag, Alt-click and Reset; per-brush pressure; inserts standing on the surface with outward mirror copies, mask, undo/redo and the refusal with levels); every native self-test passes on the GPU.

## 0.14.0 — 2026-10-24

- Stroke › Alpha rotation: Fixed (Angle), Follow (rolls with the stroke) or Random (per dab), for sculpt, paint and mask stamps.
- Nine detail alphas and textures: Pores, Wrinkles, Scales, Dry Cracks, Weave, Rock, Bark, Leather, Fibers (tileable, soft-edged as alphas).
- Five MatCaps for reading detail: Studio Gray, Cavity White, Slate, Rim Dark, Normal (16 materials).
- Verification: 230 release tests (new: alpha frames turn, fixed/random/follow stamps; library images fill the range, tile and differ; the normal MatCap); every native self-test passes on the GPU (MatCaps within 1–3/255 of their images).

## 0.13.0 — 2026-10-24

- Geometry: Del Hidden and Close Holes; Deformation: Mirror And Weld. Each one undo step (the old stack kept packed, as for Remesh); base level only, layers baked first.
- Light palette: key light placement on a sphere (view-relative), Intensity, Ambient.
- Projects save each subtool's layers and Morph Target (format version 5; 1–4 still open).
- Verification: 226 release tests (new: closed outward caps, welded symmetric mirror, the three topology ops with undo and refusals, version 5 round trip with layers and Morph Target); every native self-test passes on the GPU.

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
