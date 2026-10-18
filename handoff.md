# Klay handoff

## Project
Klay is a native desktop sculpting prototype. The UI is a ZBrush-style workspace (menu-bar palettes, four dockable trays, fixed shelves), ported from the interactive HTML reference `ui-mockup.html`. The Material 3 version described in steps 1–4 below was replaced in step 5. UI copy should only be functional.

## Stack
- Rust 2024, winit 0.30, wgpu 30, egui/egui-wgpu/egui-winit 0.37.
- glam for camera mathematics, rayon for chunk-parallel mesh operations, WGSL for rendering, normals, and GPU Grab.
- Run with `cargo run --release`; `--segments 100` gives a lighter preview. Existing CLI benchmarks and stress modes must remain usable.

## Step 1: inspection complete
- Existing application and rendering/input paths inspected. UI currently lives in `App::ui` in `src/main.rs` and consists of one fixed left sidebar.
- Existing working features: Draw, Smooth, Grab, Flatten, Inflate, brush radius/strength, orbit/pan/zoom, frame, mesh rebuild, undo/redo, OBJ export, and live statistics.
- No existing repository AGENTS.md file. Follow the user's supplied instructions: maintain this file at each implementation step, use Material 3, avoid UI fluff.
- Repository contents were all untracked before this task; do not discard or overwrite unrelated work.

## Planned logical steps
1. Add a separate UI module containing Material 3 theme tokens and a reusable panel registry/layout model.
2. Build a native workspace shell with collapsible panels, drag scrolling, four docking targets, a pen-friendly explicit docking menu, and layout persistence/reset.
3. Connect existing sculpting controls; clearly disable future engine features in the mockup. Ensure the camera and input map to the central canvas as docks move.
4. Build, check core layout behavior, inspect the actual native UI, fix visual/input issues, and record verified results here.

## Architectural constraints
- Keep the current GPU/mesh/sculpt pipeline intact.
- UI-only additions belong in `src/workspace.rs`; adding a panel should require a registry entry and content renderer, not new shell code.
- Do not implement new sculpting algorithms as part of this mockup.

## Step 2: workspace framework built
- Added `src/workspace.rs`: 14-panel registry, Material 3 tokens, independent layout model, collapse/hide/restore, four-edge docking, drag reordering, pen/mouse drag scrolling, focus mode, and automatic layout persistence in `$XDG_CONFIG_HOME/klay/workspace.layout` (fallback `~/.config/klay/workspace.layout`).
- Added a native application header, brush tiles, shared brush settings, object/material panels, export, history, statistics, view controls, orientation gizmo, and functional shortcuts.
- Future engine features (stroke spacing/stabilizer, pen pressure, alpha, masking, transform, symmetry) are explicitly marked `Not connected` and disabled.
- `App::ui` now passes a view model to the workspace and handles returned actions. Existing brush, mesh, history, and export controls remain connected.
- The renderer now uses the central canvas rectangle for viewport/scissor and camera aspect. Picking subtracts the canvas origin, so docking should not offset sculpt strokes. Logical UI units are converted to physical pixels for high DPI.
- Added four material previews and material selection shading. Surface detail uses the existing reserved shader toggle.

## Step 3: native integration and initial verification
- Release build and actual GPU startup succeeded. The new native workspace was visually inspected on the desktop.
- Added an antialiased perspective floor grid as a background GPU pass. The Floor grid toggle is connected. All four material swatches affect the real mesh shading.
- Replaced unsupported icon glyphs with readable functional labels and dot menus after visual inspection.
- Headless egui pointer-event tests verify actual collapse clicks and drag/drop onto all four canvas edges, in addition to the layout model tests.
- Added responsive sidebar width limits and a 960×640 logical-pixel minimum window size; a test covers default, expanded, and focus layouts at 1600×1000, 1280×800, and 960×640.
- Sliders in narrow panels now adapt to panel width, with values on a separate row.

## Step 4: mockup complete
- Final release build succeeded with `cargo build --release --offline`.
- Existing headless sculpt benchmark also passed with `target/release/klay --bench 50`: raycast hit, all five brush kinds exercised, normals computed, and three undo/redo operations completed.
- All **7 tests pass** with `cargo test --release --offline`: layout moves/reordering, persistence round trip, damaged/old layouts, real egui collapse clicks, pointer drag/drop to all four edges, pen-style drag scrolling without accidental docking, and supported window sizes/focus mode.
- Formatting check passes for changed Rust files using `rustfmt --edition 2024 --config skip_children=true --check src/main.rs src/render.rs src/workspace.rs`.
- Final native binary launched successfully using `target/release/klay --segments 200`; GPU rendering, grid, four materials, controls, and typography visually inspected.
- `ui-mockup.png` is a 1600×1000 capture of the actual native window, including the currently customized saved workspace. The capture uses the window's compositor pixmap, so other desktop windows are excluded.
- Newly docked panels and the Export panel scroll into view automatically.
- README now explains panel gestures, layout persistence, mockup limitations, run/test commands, and how to add panels.
- No dependencies were added and no files were staged or committed.

## Step 5: ZBrush-style workspace port (2026-10-18)
- The user rejected the Material 3 look. An interactive HTML mockup (`ui-mockup.html`, renders `ui-mockup-v4*.png`) was iterated with the user to a ZBrush-like layout, then ported to egui.
- `src/workspace.rs` was rewritten (same public surface: `Workspace::new/show`, `Model`, `Actions`, `theme`, `BACKGROUND`, and the `subtract/material/wireframe/grid/focus/canvas` fields, so `main.rs` and `render.rs` were untouched).
- Model: `PaletteId` (14 palettes, alphabetical = menu-bar order) × `Place::{Menu, Tray(Side)}`; `DockLayout` holds order, open state, per-section overrides, tray open flags and tray width. Persistence format is `# Klay workspace v2` (`trays|…`, `palette|…`, `section|…`); v1 files decode to defaults.
- Shell: menu bar with dropdown palettes (`egui::Area`), top shelf, left shelf (sphere thumbnails + HSV picker), right shelf (icon+label nav buttons), trays as `egui::Panel`s with `ScrollArea::scroll_source(ALL)` for pen-drag scrolling, dividers toggling the side trays, dock button = `Sense::click_and_drag` (click → `Popup::menu`, drag → `DragAndDrop` payload with canvas edge zones and before-palette markers).
- Brush/material previews: `sphere_image` renders a shaded sphere with the brush's stroke (CPU, 96 px, cached per kind/material/subtract as egui textures).
- Widgets: `zslider` (ZBrush bar slider; `line` variant for the shelf), `seg`, `big_button`, `thumb`, `brush_tile`, `color_picker` (mesh gradients), `menu_item`, `kv`, `tag`.
- 8 tests pass (`cargo test --release --offline`): layout moves/reorder/undock, roundtrip, damaged files, title click collapses, dock-button drag to all four edges, pen-drag tray scrolling, window sizes/focus, preview images.
- Verified natively: `target/release/klay --segments 200` captured with `xwd -id <window> | python XWD→PNG` (no xdotool/ImageMagick on this machine; `wmctrl` positions the window). Capture: `ui-native.png`.
- Known gaps: Persp toggle, Sym, paint modes, Zcut, Focal Shift, stroke/alpha/texture/masking/transform are placeholders; History shows availability only (the engine has no stroke list); the colour picker has nothing to paint with yet.

## What remains for later implementation
The requested UI mockup is complete. Further product work can proceed independently:
1. Review the layout with the user; adjust panel defaults and Material 3 tokens in `src/workspace.rs`.
2. Implement actual pen-pressure/spacing/stabilizer support in the input/stroke path, then enable those controls.
3. Implement alpha, masking, transforms, and symmetry in the sculpting engine before enabling their panels.
4. Add document save/open and a richer object hierarchy when the mesh/document model supports them.
5. Expand workspace persistence to include user-resized dock dimensions and view/material settings if desired. Current persistence covers order, edge, visibility, and collapse state.

## Practical notes for the next AI
- Keep the user's saved layout; use Workspace → Reset layout only when explicitly desired. Tests use isolated temporary persistence files.
- Layout tests emulate pen-style pointer drags; hardware stylus pressure has not been implemented or verified.
- Run `cargo run --release -- --segments 200` for a light native preview. The default remains the original 960k-vertex sphere.
- The existing code was already untracked at the start of this task; do not infer that all untracked files were created by this change.
