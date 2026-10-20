mod camera;
mod mask;
mod mesh;
mod multires;
mod project;
mod render;
mod sculpt;
mod subtool;
mod transform;
mod workspace;

use camera::Camera;
use glam::{Vec2, Vec3};
use mesh::{SculptMesh, cube_sphere};
use multires::Multires;
use render::{Gfx, Uniforms};
use mesh::Aabb;
use sculpt::{Brush, BrushKind, History, Stroke, UndoStep};
use std::path::Path;
use subtool::{Deform, Parked, STAND_IN_VERTICES, SubTool};
use transform::{ActionLine, Affine, Hit, Part, View};
use mask::{MaskData, MaskOp};
use workspace::{Mode, QuickPopup, SubToolRow};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NavMode {
    Orbit,
    Pan,
    Zoom,
    Roll,
}

/// A camera drag in progress. The mode is not stored: it is derived from the button and
/// the current modifiers on every move, so modifiers can be changed mid-drag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct NavDrag {
    button: MouseButton,
    /// Alt has been held at some point during a left-button drag. From then on the drag
    /// pans while Alt is down and zooms once Alt is released (ZBrush: Alt + click,
    /// release Alt, drag = zoom). Before Alt is ever used a left drag orbits.
    alt_used: bool,
    /// Shift was held when the left button went down: releasing Shift then turns the
    /// drag into a roll about the view axis (ZBrush: Shift, click, release Shift, drag).
    shift_at_press: bool,
}

impl NavDrag {
    fn mode(&self, m: ModifiersState) -> NavMode {
        match self.button {
            MouseButton::Left => {
                if m.alt_key() {
                    NavMode::Pan
                } else if self.alt_used {
                    NavMode::Zoom
                } else if self.shift_at_press && !m.shift_key() {
                    NavMode::Roll
                } else {
                    NavMode::Orbit
                }
            }
            // Right/middle (ZBrush right-click navigation): orbit; Alt pans; Ctrl zooms.
            _ => {
                if m.control_key() {
                    NavMode::Zoom
                } else if m.alt_key() {
                    NavMode::Pan
                } else {
                    NavMode::Orbit
                }
            }
        }
    }
}

/// Document operations queued by the interface or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocOp {
    New,
    Open,
    Save,
}

/// SubTool operations queued by the interface.
#[derive(Clone, Copy, Debug)]
enum SubToolOp {
    Select(usize),
    Append,
    Duplicate,
    Delete,
    Deform(Deform, f32),
}

/// A left press with Ctrl that has not yet become a drag: on the model a drag paints the
/// mask and a click blurs it; on the background a click inverts and a drag clears it.
#[derive(Clone, Copy, Debug)]
struct MaskPress {
    at: Vec2,
    /// Surface point under the press, when it was on the active subtool.
    hit: Option<Vec3>,
    /// Alt at the press: erase instead of paint, sharpen instead of blur.
    alt: bool,
    moved: bool,
}

/// State of `--selftest-mask`: masking through the same handlers the window events call,
/// one input event per frame, then a masked Transpose move, a clear and undo of it all.
#[derive(Default)]
struct MaskTest {
    phase: usize,
    k: u32,
    original: Vec<Vec3>,
    before: Vec<Vec3>,
    press: Vec2,
    /// Frame times (ms): no mask, mask shown, dragging a masked move.
    idle: Vec<f32>,
    shaded: Vec<f32>,
    drag: Vec<f32>,
    dabs: Vec<f32>,
    /// Named costs in ms, logged at the end.
    costs: Vec<(&'static str, f32)>,
    ok: bool,
}

/// State of `--selftest-transpose`: a scripted Move, Scale and Rotate through the same press /
/// move / release handlers the window events call, one input event per frame, then three
/// undos.
#[derive(Default)]
struct TransposeTest {
    phase: usize,
    /// Frame within the phase.
    k: u32,
    /// Shown level before the first move, and before the current action.
    original: Vec<Vec3>,
    before: Vec<Vec3>,
    /// Pixel positions of the current action: press and the drag's full offset.
    press: Vec2,
    pivot: Vec2,
    /// Frame times (ms): idle, while dragging with the GPU preview, and the whole frame of
    /// each release (Move, Scale, Rotate).
    idle: Vec<f32>,
    drag: Vec<f32>,
    release_frame: Vec<f32>,
    /// CPU transform time of each release (ms).
    release: Vec<f32>,
    /// The map applied by the last release and the line as shown when it was let go.
    applied: Option<(Affine, ActionLine)>,
    ok: bool,
}

/// A left drag in Move / Scale / Rotate mode (ZBrush Transpose, see `transform.rs`).
#[derive(Clone, Copy, Debug)]
enum TransposeDrag {
    /// Drawing a new action line from the point on the surface under the press.
    Draw { start: Vec3 },
    /// Moving the line, or one end of it (`ends`: start, end), without touching the model.
    Line {
        line: ActionLine,
        press: Vec2,
        ends: [bool; 2],
    },
    /// Moving, scaling or rotating the active subtool about `anchor`. `map` is only shown
    /// (the vertex shader applies it to the shown level) until the release applies it to
    /// every level as one undo step.
    Act {
        mode: Mode,
        anchor: Vec3,
        press: Vec2,
        line: ActionLine,
        map: Affine,
    },
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,

    /// The level stack of the active subtool; its active level is shown and sculpted.
    doc: Multires,
    /// Every subtool, the active one included (its stack is `doc`).
    subtools: Vec<SubTool>,
    active_tool: usize,
    next_tool_id: u64,
    /// `--subtools N`: spheres to append once the window exists.
    initial_subtools: usize,
    /// `--selftest-subtools`: frames left before picking and switching subtools through
    /// the real GPU path, logging the results, then exiting.
    selftest: Option<u32>,
    /// `--selftest-project`: frames left before a save and re-open round trip.
    selftest_project: Option<u32>,
    /// `--selftest-transpose`: frame counter of the scripted move / scale / rotate / undo.
    selftest_transpose: Option<TransposeTest>,
    /// `--selftest-mask`: the scripted masking session.
    selftest_mask: Option<MaskTest>,
    /// Project file shown in the Document palette, used by Save and Open.
    project_path: String,
    /// `--open PATH`: project to open once the window exists.
    open_on_start: Option<String>,
    pending_doc: Option<DocOp>,
    camera: Camera,
    brush: Brush,
    history: History,
    stroke: Option<Stroke>,
    stroke_brush: Brush,

    cursor: Vec2,
    last_cursor: Vec2,
    /// egui points per physical pixel, for placing popups at the cursor.
    scale: f32,
    /// Cursor at the start of the current camera drag, to tell a right-click tap apart.
    nav_press: Vec2,
    modifiers: ModifiersState,
    nav: Option<NavDrag>,
    hover: Option<Vec3>,
    /// The Transpose action line, kept between drags (shown in Move / Scale / Rotate mode).
    action_line: Option<ActionLine>,
    transpose: Option<TransposeDrag>,
    mask_press: Option<MaskPress>,
    mask_stroke: Option<mask::MaskStroke>,

    // UI state.
    workspace: workspace::Workspace,
    canvas_rect: egui::Rect,
    segments: u32,
    export_path: String,
    status: String,
    pending_rebuild: bool,
    pending_export: bool,
    pending_undo: bool,
    pending_redo: bool,
    pending_divide: bool,
    /// Whole-mask operation from a shortcut or the Masking palette.
    pending_mask: Option<MaskOp>,
    pending_level: Option<usize>,
    /// SubTool list operation from the interface, run outside the egui pass.
    pending_subtool: Option<SubToolOp>,
    /// Frames left in `--frames` mode (plain frame timing at the active level) and the
    /// accumulated frame time.
    measure: Option<(u32, f32)>,
    quit: bool,

    // Stats.
    build_ms: f32,
    last_frame: Instant,
    frame_ms: f32,
    fps: f32,
    stroke_ms: f32,
    /// A grab update is waiting for the next frame (grab is applied once per frame).
    stroke_pending: bool,
    last_dab_count: usize,
    demo: bool,
    vsync: bool,
    /// Frames left in `--stress` mode (a large grab moved every frame), plus accumulators.
    stress: Option<(u32, f32, f32)>,
    upload_ms: f32,
    render_ms: f32,
    stress_upload: f32,
    stress_render: f32,
}

impl App {
    fn new(segments: u32, demo: bool, budget: usize) -> Self {
        let t = Instant::now();
        let (p, i) = cube_sphere(segments, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let build_ms = t.elapsed().as_secs_f32() * 1000.0;
        Self {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            subtools: vec![SubTool {
                id: 1,
                name: "Sphere 1".to_string(),
                visible: true,
                bounds: mesh.bounds(),
                parked: None,
            }],
            active_tool: 0,
            next_tool_id: 2,
            initial_subtools: 0,
            selftest: None,
            selftest_project: None,
            selftest_transpose: None,
            selftest_mask: None,
            project_path: "untitled.klay".to_string(),
            open_on_start: None,
            pending_doc: None,
            doc: Multires::new(mesh, budget),
            camera: Camera::new(),
            brush: Brush::default(),
            history: History::new(32),
            stroke: None,
            stroke_brush: Brush::default(),
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            scale: 1.0,
            nav_press: Vec2::ZERO,
            modifiers: ModifiersState::empty(),
            nav: None,
            hover: None,
            action_line: None,
            transpose: None,
            mask_press: None,
            mask_stroke: None,
            workspace: workspace::Workspace::new(),
            canvas_rect: egui::Rect::NOTHING,
            segments,
            export_path: "klay_export.obj".to_string(),
            status: String::new(),
            pending_rebuild: false,
            pending_export: false,
            pending_undo: false,
            pending_redo: false,
            pending_divide: false,
            pending_mask: None,
            pending_level: None,
            pending_subtool: None,
            measure: None,
            quit: false,
            build_ms,
            last_frame: Instant::now(),
            frame_ms: 0.0,
            fps: 0.0,
            stroke_ms: 0.0,
            stroke_pending: false,
            last_dab_count: 0,
            demo,
            vsync: true,
            stress: None,
            upload_ms: 0.0,
            render_ms: 0.0,
            stress_upload: 0.0,
            stress_render: 0.0,
        }
    }

    fn mesh(&self) -> &SculptMesh {
        self.doc.mesh()
    }

    /// A stroke or a Transpose drag is in progress: operations that replace or reshape the
    /// active stack wait for it to end.
    fn busy(&self) -> bool {
        self.stroke.is_some() || self.transpose.is_some() || self.mask_press.is_some() || self.mask_stroke.is_some()
    }

    /// Stands in for `doc` between parking one subtool and activating another.
    fn placeholder() -> Multires {
        let (p, i) = cube_sphere(1, 1.0);
        Multires::new(SculptMesh::from_raw(p, i), 0)
    }

    /// Park the active subtool: its stack is replaced by the packed form, its GPU levels are
    /// freed and its stand-in uploaded. `doc` holds a placeholder until another subtool is
    /// made active. Returns the time taken in ms.
    fn park_active(&mut self) -> f32 {
        let t = Instant::now();
        let mut doc = std::mem::replace(&mut self.doc, Self::placeholder());
        let stand_in = doc.stand_in(STAND_IN_VERTICES);
        let packed = doc.pack();
        let mut history = std::mem::replace(&mut self.history, History::new(32));
        history.drop_redo();
        let tool = &mut self.subtools[self.active_tool];
        tool.bounds = stand_in.bounds;
        let mut stand_in_vertices = stand_in.positions.len();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
            if !gfx.upload_stand_in(tool.id, &stand_in) {
                stand_in_vertices = 0;
            }
        }
        tool.parked = Some(Box::new(Parked {
            packed,
            history,
            stand_in_vertices,
        }));
        t.elapsed().as_secs_f32() * 1000.0
    }

    /// Rebuild the stack of parked subtool `i` and make it the active one. Returns the time
    /// taken in ms.
    fn activate(&mut self, i: usize) -> f32 {
        let t = Instant::now();
        let tool = &mut self.subtools[i];
        let Parked {
            packed, history, ..
        } = *tool.parked.take().expect("only a parked subtool can be activated");
        tool.visible = true;
        let id = tool.id;
        self.doc = packed.unpack();
        self.history = history;
        self.active_tool = i;
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.drop_stand_in(id);
            gfx.clear_levels();
        }
        self.show_active_level();
        ms
    }

    /// Make subtool `i` the active one (SubTool list, or Alt + click on it).
    fn select_subtool(&mut self, i: usize) {
        if i == self.active_tool || i >= self.subtools.len() || self.busy() {
            return;
        }
        let park_ms = self.park_active();
        let unpack_ms = self.activate(i);
        self.status = format!(
            "{}: {} vertices at level {} · parked the previous one in {:.0} ms, rebuilt this one in {:.0} ms",
            self.subtools[i].name,
            fmt_count(self.mesh().vertex_count()),
            self.doc.active(),
            park_ms,
            unpack_ms
        );
        log::info!("{}", self.status);
        self.workspace.notify(format!("Selected {}", self.subtools[i].name));
    }

    fn new_tool_id(&mut self) -> u64 {
        let id = self.next_tool_id;
        self.next_tool_id += 1;
        id
    }

    /// Add a sphere subtool next to the visible model and make it active (ZBrush: Append).
    fn append_sphere(&mut self) {
        if self.busy() {
            return;
        }
        let bounds = self.model_bounds();
        let radius = 0.5;
        let center = Vec3::new(
            bounds.max.x + radius * 1.25,
            (bounds.min.y + bounds.max.y) * 0.5,
            (bounds.min.z + bounds.max.z) * 0.5,
        );
        let budget = self.doc.budget;
        self.park_active();
        let t = Instant::now();
        let (p, i) = cube_sphere(self.segments, radius);
        let p = p.into_iter().map(|q| q + center).collect();
        self.doc = Multires::new(SculptMesh::from_raw(p, i), budget);
        let id = self.new_tool_id();
        self.subtools.push(SubTool {
            id,
            name: format!("Sphere {id}"),
            visible: true,
            bounds: self.doc.bounds(),
            parked: None,
        });
        self.active_tool = self.subtools.len() - 1;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
        self.status = format!(
            "Appended Sphere {id}: {} vertices in {:.0} ms",
            fmt_count(self.mesh().vertex_count()),
            t.elapsed().as_secs_f32() * 1000.0
        );
    }

    /// Insert a parked copy of the active subtool below it (ZBrush: Duplicate).
    fn duplicate_subtool(&mut self) {
        if self.busy() {
            return;
        }
        let packed = self.doc.pack_copy();
        let stand_in = self.doc.stand_in(STAND_IN_VERTICES);
        let id = self.new_tool_id();
        let mut stand_in_vertices = stand_in.positions.len();
        if let Some(gfx) = self.gfx.as_mut() {
            if !gfx.upload_stand_in(id, &stand_in) {
                stand_in_vertices = 0;
            }
        }
        let name = format!("{} copy", self.subtools[self.active_tool].name);
        self.subtools.insert(
            self.active_tool + 1,
            SubTool {
                id,
                name: name.clone(),
                visible: true,
                bounds: stand_in.bounds,
                parked: Some(Box::new(Parked {
                    packed,
                    history: History::new(32),
                    stand_in_vertices,
                })),
            },
        );
        self.workspace.notify(format!("Duplicated as {name}"));
    }

    /// Delete the active subtool and activate the one below it, or above it when it was
    /// the last (ZBrush: Delete). Not undoable.
    fn delete_subtool(&mut self) {
        if self.busy() {
            return;
        }
        if self.subtools.len() < 2 {
            self.workspace.notify("The last subtool cannot be deleted");
            return;
        }
        let removed = self.subtools.remove(self.active_tool);
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.doc = Self::placeholder();
        self.history = History::new(32);
        let next = self.active_tool.min(self.subtools.len() - 1);
        self.activate(next);
        self.workspace.notify(format!("Deleted {}", removed.name));
    }

    /// Move the active subtool up (-1) or down (+1) in the list.
    fn move_subtool(&mut self, by: i32) {
        let i = self.active_tool;
        let j = i as i64 + by as i64;
        if j < 0 || j >= self.subtools.len() as i64 {
            return;
        }
        self.subtools.swap(i, j as usize);
        self.active_tool = j as usize;
    }

    /// Show or hide a parked subtool. The active one is always shown.
    fn toggle_subtool(&mut self, i: usize) {
        if i != self.active_tool {
            if let Some(t) = self.subtools.get_mut(i) {
                t.visible = !t.visible;
            }
        }
    }

    /// Offset or size the active subtool, undoably (ZBrush: Tool > Deformation).
    fn deform(&mut self, kind: Deform, value: f32) {
        if self.busy() || value.abs() < 0.5 {
            return;
        }
        self.apply_transform(subtool::deform_map(kind, value, self.doc.bounds()));
        self.workspace
            .notify(format!("{} {:+.0} applied", kind.label(), value));
    }

    /// Map every level of the active subtool by `map` as one undo step. Returns the time
    /// taken in ms (the GPU copy of the shown level is re-uploaded by the next frame).
    fn apply_transform(&mut self, map: Affine) -> f32 {
        let t = Instant::now();
        if self.doc.mesh().mask.iter().any(|&m| m > 0) {
            // ZBrush: masked vertices stay; partly masked ones move part of the way.
            let level = self.doc.active();
            let mask = self.doc.mesh().mask.clone();
            self.doc.transform_masked(&map, level, &mask);
            self.history.push(UndoStep::MaskedTransform {
                map: map.inverse(),
                level,
                mask,
            });
            self.subtools[self.active_tool].bounds = self.doc.bounds();
            let ms = t.elapsed().as_secs_f32() * 1000.0;
            self.update_hover();
            return ms;
        }
        self.transform_doc(&map, |app| app.doc.transform(&map));
        // History entries hold what undoes them: the inverse map.
        self.history.push(UndoStep::Transform(map.inverse()));
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        self.update_hover();
        ms
    }

    /// Run `f`, which maps every level of the active subtool by `map` on the CPU, and bring
    /// the GPU along: when the shown level had nothing waiting for upload its GPU copy is
    /// mapped in place rather than uploaded again (184 MB at 15M vertices).
    fn transform_doc(&mut self, map: &Affine, f: impl FnOnce(&mut Self)) {
        let clean = self
            .doc
            .mesh()
            .chunks
            .iter()
            .all(|c| !c.dirty_pos && !c.dirty_nrm);
        f(self);
        if let Some(gfx) = self.gfx.as_mut() {
            if clean {
                gfx.transform_level(self.doc.mesh_mut(), map);
            } else {
                gfx.rescale_normals(map.scale);
            }
        }
        self.subtools[self.active_tool].bounds = self.doc.bounds();
    }

    /// The canvas as the camera shows it, for the Transpose maths.
    fn view(&self) -> View<'_> {
        View {
            camera: &self.camera,
            size: self.viewport(),
        }
    }

    /// The cursor is in the margin between the document frame and the canvas edge, where a
    /// left drag always navigates (ZBrush: the border acts as empty canvas).
    fn in_canvas_border(&self) -> bool {
        if !self.canvas_rect.is_positive() {
            return false;
        }
        let inner = self.canvas_rect.shrink(workspace::FRAME_INSET * self.scale);
        !inner.contains(egui::pos2(self.cursor.x, self.cursor.y))
    }

    /// Cursor in canvas pixels.
    fn canvas_cursor(&self) -> Vec2 {
        self.cursor - self.canvas_origin()
    }

    /// Start a Transpose drag for a left press in Move / Scale / Rotate mode: on the action
    /// line's circles or the line itself if the press is there, else a new line when the
    /// press is on the active subtool. Returns false when the press is on neither (the
    /// caller navigates).
    fn begin_transpose(&mut self) -> bool {
        let mode = self.workspace.mode;
        let press = self.canvas_cursor();
        if let Some(line) = self.action_line {
            if let Some(hit) = transform::hit_test(&self.view(), &line, press, self.scale) {
                let act = |anchor| TransposeDrag::Act {
                    mode,
                    anchor,
                    press,
                    line,
                    map: Affine::IDENTITY,
                };
                let drag_line = |ends| TransposeDrag::Line { line, press, ends };
                // ZBrush: the middle circle moves, the end circle scales or rotates about
                // the start (the start circle about the end); the rings and the line move
                // the line itself.
                let drag = match (mode, hit) {
                    (Mode::Move, Hit::Inner(Part::Middle)) => act(line.mid()),
                    (Mode::Scale | Mode::Rotate, Hit::Inner(Part::End)) => act(line.start),
                    (Mode::Scale | Mode::Rotate, Hit::Inner(Part::Start)) => act(line.end),
                    (_, Hit::Inner(Part::Start) | Hit::Ring(Part::Start)) => drag_line([true, false]),
                    (_, Hit::Inner(Part::End) | Hit::Ring(Part::End)) => drag_line([false, true]),
                    _ => drag_line([true, true]),
                };
                log::debug!("transpose: {hit:?} in {} mode", mode.name());
                self.transpose = Some(drag);
                self.hover = None;
                return true;
            }
        }
        let (o, d) = self.cursor_ray();
        let Some(hit) = self.mesh().raycast(o, d) else {
            return false;
        };
        self.transpose = Some(TransposeDrag::Draw { start: hit.point });
        self.action_line = Some(ActionLine {
            start: hit.point,
            end: hit.point,
        });
        self.hover = None;
        true
    }

    /// Follow the cursor during a Transpose drag.
    fn transpose_move(&mut self) {
        let to = self.canvas_cursor();
        let view = View {
            camera: &self.camera,
            size: self.viewport(),
        };
        let Some(drag) = self.transpose.as_mut() else {
            return;
        };
        match drag {
            TransposeDrag::Draw { start } => {
                let from = view.project(*start);
                // ZBrush: Shift while drawing snaps the line to 45° steps.
                let to = if self.modifiers.shift_key() { transform::snap_45(from, to) } else { to };
                self.action_line = Some(ActionLine {
                    start: *start,
                    end: view.unproject_at(to, *start),
                });
            }
            TransposeDrag::Line { line, press, ends } => {
                let follow = |p: Vec3, on: bool| {
                    if on { p + view.unproject_at(to, p) - view.unproject_at(*press, p) } else { p }
                };
                self.action_line = Some(ActionLine {
                    start: follow(line.start, ends[0]),
                    end: follow(line.end, ends[1]),
                });
            }
            TransposeDrag::Act {
                mode,
                anchor,
                press,
                line,
                map,
            } => {
                *map = match mode {
                    Mode::Move => transform::move_map(&view, *anchor, *press, to),
                    Mode::Scale => transform::scale_map(&view, *anchor, *press, to),
                    Mode::Rotate => transform::rotate_map(&view, *anchor, *press, to),
                    Mode::Draw => Affine::IDENTITY,
                };
                self.action_line = Some(line.mapped(map));
            }
        }
    }

    /// Release of a Transpose drag: a move / scale / rotate is applied to every level of
    /// the active subtool as one undo step. Returns the time the CPU transform took in ms.
    fn end_transpose(&mut self) -> Option<f32> {
        match self.transpose.take()? {
            TransposeDrag::Draw { .. } => {
                // A click without a drag leaves no line.
                if let Some(line) = self.action_line {
                    let view = self.view();
                    if (view.project(line.end) - view.project(line.start)).length() < 2.0 {
                        self.action_line = None;
                    }
                }
                None
            }
            TransposeDrag::Line { .. } => None,
            TransposeDrag::Act { mode, line, map, .. } => {
                if map.is_identity() {
                    return None;
                }
                let ms = self.apply_transform(map);
                self.action_line = Some(line.mapped(&map));
                self.status = match mode {
                    Mode::Move => format!("Moved by {:.3} in {ms:.0} ms", map.translate.length()),
                    Mode::Scale => format!("Scaled by {:.3} in {ms:.0} ms", map.scale),
                    _ => format!(
                        "Rotated by {:.1}° in {ms:.0} ms",
                        map.rotation.to_axis_angle().1.to_degrees()
                    ),
                };
                log::info!("{}", self.status);
                Some(ms)
            }
        }
    }

    /// The map the vertex shader applies to the active subtool: the Transpose drag's while
    /// one is in progress.
    fn preview_map(&self) -> Affine {
        match self.transpose {
            Some(TransposeDrag::Act { map, .. }) => map,
            _ => Affine::IDENTITY,
        }
    }

    /// Draw the action line in Move / Scale / Rotate mode: a line with start, middle and
    /// end circles (inner circle acts, outer ring moves that point).
    fn paint_action_line(&self, ctx: &egui::Context) {
        let Some(line) = self.action_line else {
            return;
        };
        if self.workspace.mode == Mode::Draw || !self.canvas_rect.is_positive() {
            return;
        }
        let view = self.view();
        let to_pos = |p: Vec3| {
            let px = view.project(p) + self.canvas_origin();
            egui::pos2(px.x / self.scale, px.y / self.scale)
        };
        let painter = ctx
            .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("action line")))
            .with_clip_rect(self.workspace.canvas);
        let (a, m, b) = (to_pos(line.start), to_pos(line.mid()), to_pos(line.end));
        let white = egui::Color32::from_rgb(232, 232, 232);
        let accent = egui::Color32::from_rgb(227, 131, 79);
        let dark = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(140));
        let len = (b - a).length();
        if len > 2.0 * transform::OUTER {
            let dir = (b - a) / len;
            let (p, q) = (a + dir * transform::OUTER, b - dir * transform::OUTER);
            painter.line_segment([p, q], dark);
            painter.line_segment([p, q], egui::Stroke::new(1.5, white));
        }
        for (c, color) in [(a, accent), (m, white), (b, white)] {
            for r in [transform::OUTER, transform::INNER] {
                painter.circle_stroke(c, r, dark);
                painter.circle_stroke(c, r, egui::Stroke::new(1.5, color));
            }
        }
    }

    /// Parked subtools that are drawn: `(gpu key, list index)`.
    fn drawn_parked(&self) -> Vec<(u64, usize)> {
        if self.workspace.solo {
            return Vec::new();
        }
        self.subtools
            .iter()
            .enumerate()
            .filter(|(i, t)| *i != self.active_tool && t.visible)
            .map(|(i, t)| (t.id, i))
            .collect()
    }

    /// Bounds of what is drawn: the active subtool and the visible parked ones.
    fn model_bounds(&self) -> Aabb {
        self.drawn_parked()
            .iter()
            .fold(self.doc.bounds(), |b, &(_, i)| subtool::union(b, self.subtools[i].bounds))
    }

    /// Every subtool as `(name, visible, packed)` for a project file, with the active one
    /// packed as a copy so it is not disturbed.
    fn with_project_tools<T>(&self, f: impl FnOnce(&[(&str, bool, &multires::Packed)]) -> T) -> T {
        let active = self.doc.pack_copy();
        let tools: Vec<(&str, bool, &multires::Packed)> = self
            .subtools
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let packed = match &t.parked {
                    Some(p) => &p.packed,
                    None => &active,
                };
                (t.name.as_str(), t.visible || i == self.active_tool, packed)
            })
            .collect();
        f(&tools)
    }

    /// The whole model as project bytes, in memory (tests and the self-test).
    fn project_bytes(&self) -> Vec<u8> {
        self.with_project_tools(|tools| project::write_to(Vec::new(), tools, self.active_tool))
            .expect("writing to memory")
    }

    /// Write every subtool to `self.project_path` (ZBrush: Save; Ctrl + S).
    fn save_project(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let path = self.project_path.clone();
        let result = self.with_project_tools(|tools| project::save(Path::new(&path), tools, self.active_tool));
        self.status = match result {
            Ok(bytes) => format!(
                "Saved {path}: {} subtools, {:.1} MB in {:.2} s",
                self.subtools.len(),
                bytes as f64 / 1e6,
                t.elapsed().as_secs_f32()
            ),
            Err(e) => format!("Save failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// Replace the model with the project at `path` (ZBrush: Open; Ctrl + O). Every stack is
    /// rebuilt and checked, one at a time, before the current model is let go; a file that
    /// fails leaves the current model as it was.
    fn open_project(&mut self, path: &str) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        match self.load_project(path) {
            Ok(()) => {
                self.project_path = path.to_string();
                self.status = format!(
                    "Opened {path}: {} subtools in {:.2} s",
                    self.subtools.len(),
                    t.elapsed().as_secs_f32()
                );
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    fn load_project(&mut self, path: &str) -> Result<(), String> {
        let project = project::open(Path::new(path))?;
        let active = project.active;
        let mut staged: Vec<SubTool> = Vec::with_capacity(project.tools.len());
        let mut active_doc = None;
        let mut failure = None;
        for (i, saved) in project.tools.into_iter().enumerate() {
            let mut doc = match saved.packed.try_unpack() {
                Ok(doc) => doc,
                Err(e) => {
                    failure = Some(format!("subtool {:?}: {e}", saved.name));
                    break;
                }
            };
            let id = self.new_tool_id();
            if i == active {
                staged.push(SubTool {
                    id,
                    name: saved.name,
                    visible: true,
                    bounds: doc.bounds(),
                    parked: None,
                });
                active_doc = Some(doc);
            } else {
                let stand_in = doc.stand_in(STAND_IN_VERTICES);
                let mut stand_in_vertices = stand_in.positions.len();
                if let Some(gfx) = self.gfx.as_mut() {
                    if !gfx.upload_stand_in(id, &stand_in) {
                        stand_in_vertices = 0;
                    }
                }
                staged.push(SubTool {
                    id,
                    name: saved.name,
                    visible: saved.visible,
                    bounds: stand_in.bounds,
                    parked: Some(Box::new(Parked {
                        packed: doc.pack(),
                        history: History::new(32),
                        stand_in_vertices,
                    })),
                });
            }
        }
        let (Some(doc), None) = (active_doc, &failure) else {
            if let Some(gfx) = self.gfx.as_mut() {
                for t in &staged {
                    gfx.drop_stand_in(t.id);
                }
            }
            return Err(failure.unwrap_or_else(|| "no active subtool".into()));
        };
        if let Some(gfx) = self.gfx.as_mut() {
            for t in &self.subtools {
                gfx.drop_stand_in(t.id);
            }
            gfx.clear_levels();
        }
        self.subtools = staged;
        self.active_tool = active;
        self.doc = doc;
        self.history = History::new(32);
        self.show_active_level();
        self.frame_camera();
        Ok(())
    }

    /// Start over with one sphere subtool.
    fn new_project(&mut self) {
        if self.busy() {
            return;
        }
        if let Some(gfx) = self.gfx.as_mut() {
            for t in &self.subtools {
                gfx.drop_stand_in(t.id);
            }
            gfx.clear_levels();
        }
        let budget = self.doc.budget;
        let (p, i) = cube_sphere(self.segments, 1.0);
        self.doc = Multires::new(SculptMesh::from_raw(p, i), budget);
        self.history = History::new(32);
        let id = self.new_tool_id();
        self.subtools = vec![SubTool {
            id,
            name: format!("Sphere {id}"),
            visible: true,
            bounds: self.doc.bounds(),
            parked: None,
        }];
        self.active_tool = 0;
        self.project_path = "untitled.klay".to_string();
        self.show_active_level();
        self.home_camera();
        self.status = "New project".to_string();
    }

    /// Export every visible subtool at its shown level as one OBJ with an object per
    /// subtool. Parked subtools are rebuilt one at a time for this, so memory peaks at one
    /// extra stack.
    fn export_obj_all(&mut self) {
        let t = Instant::now();
        let path = self.export_path.clone();
        let result = (|| -> std::io::Result<(usize, usize)> {
            let file = std::fs::File::create(&path)?;
            let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
            use std::io::Write;
            writeln!(w, "# Klay export: {} subtools", self.subtools.len())?;
            let (mut verts, mut count) = (0usize, 0usize);
            for (i, tool) in self.subtools.iter().enumerate() {
                if i == self.active_tool {
                    self.doc.mesh().write_obj(&mut w, &tool.name, verts)?;
                    verts += self.doc.mesh().vertex_count();
                } else if let (true, Some(p)) = (tool.visible, &tool.parked) {
                    let doc = p.packed.clone().unpack();
                    doc.mesh().write_obj(&mut w, &tool.name, verts)?;
                    verts += doc.mesh().vertex_count();
                } else {
                    continue;
                }
                count += 1;
            }
            w.flush()?;
            Ok((count, verts))
        })();
        self.status = match result {
            Ok((count, verts)) => format!(
                "Exported {count} subtools ({} vertices) to {path} in {:.1} s",
                fmt_count(verts),
                t.elapsed().as_secs_f32()
            ),
            Err(e) => format!("Export failed: {e}"),
        };
        log::info!("{}", self.status);
    }

    /// `--selftest-project`: save the model, open it again, and check that every subtool
    /// came back byte for byte. Logs sizes and times; returns whether it passed.
    fn selftest_project(&mut self) -> bool {
        let path = std::env::temp_dir().join(format!("klay-selftest-{}.klay", std::process::id()));
        let path_str = path.to_string_lossy().to_string();
        let before = self.project_bytes();
        let names: Vec<String> = self.subtools.iter().map(|t| t.name.clone()).collect();
        let active = self.active_tool;
        self.project_path = path_str.clone();
        let t = Instant::now();
        self.save_project();
        let save_s = t.elapsed().as_secs_f32();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let t = Instant::now();
        self.open_project(&path_str);
        let open_s = t.elapsed().as_secs_f32();
        let after = self.project_bytes();
        let same_names = self.subtools.iter().map(|t| t.name.clone()).collect::<Vec<_>>() == names;
        let pass = before == after && same_names && self.active_tool == active;
        log::info!(
            "selftest: saved {} subtools in {save_s:.2} s ({:.1} MB), opened in {open_s:.2} s, round trip {}",
            names.len(),
            size as f64 / 1e6,
            if pass { "identical (ok)" } else { "DIFFERS (FAIL)" }
        );
        let _ = std::fs::remove_file(&path);
        pass
    }

    /// Pixel position of a world point on the canvas.
    fn project(&self, p: Vec3) -> Vec2 {
        let clip = self.camera.view_proj() * p.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        let vp = self.viewport();
        self.canvas_origin() + Vec2::new((ndc.x + 1.0) * 0.5 * vp.x, (1.0 - ndc.y) * 0.5 * vp.y)
    }

    /// `--selftest-subtools`: pick every subtool at the centre of its bounds and the
    /// background in a corner through the GPU id pass, then switch to a parked subtool the
    /// way Alt + click does. Logs each check; returns whether all passed.
    fn selftest_subtools(&mut self) -> bool {
        let mut ok = true;
        let saved = self.cursor;
        for i in 0..self.subtools.len() {
            let b = if i == self.active_tool { self.doc.bounds() } else { self.subtools[i].bounds };
            self.cursor = self.project((b.min + b.max) * 0.5);
            let got = self.pick_subtool();
            let pass = got == Some(i);
            ok &= pass;
            log::info!("selftest: pick at the centre of {} -> {:?} ({})", self.subtools[i].name, got.map(|g| &self.subtools[g].name), if pass { "ok" } else { "FAIL" });
        }
        self.cursor = self.canvas_origin() + Vec2::splat(4.0);
        let got = self.pick_subtool();
        ok &= got.is_none();
        log::info!("selftest: pick in the canvas corner -> {got:?} ({})", if got.is_none() { "ok" } else { "FAIL" });
        let target = (self.active_tool + 1) % self.subtools.len();
        let b = self.subtools[target].bounds;
        self.cursor = self.project((b.min + b.max) * 0.5);
        if let Some(i) = self.pick_subtool().filter(|&i| i != self.active_tool) {
            self.select_subtool(i);
        }
        let pass = self.active_tool == target && self.subtools.iter().filter(|t| t.parked.is_none()).count() == 1;
        ok &= pass;
        log::info!("selftest: Alt + click path selected {} ({})", self.subtools[self.active_tool].name, if pass { "ok" } else { "FAIL" });
        log::info!(
            "selftest: active stack {} MB in RAM, parked {} MB in RAM, stand-ins {} MB on GPU, active levels {} MB on GPU",
            self.doc.bytes() / 1_000_000,
            self.subtools.iter().filter_map(|t| t.parked.as_ref()).map(|p| p.packed.bytes() + p.history.bytes()).sum::<usize>() / 1_000_000,
            self.gfx.as_ref().map_or(0, Gfx::stand_in_bytes) / 1_000_000,
            self.gfx.as_ref().map_or(0, Gfx::level_bytes) / 1_000_000,
        );
        self.cursor = saved;
        log::info!("selftest: {}", if ok { "all passed" } else { "FAILED" });
        ok
    }

    /// One frame of `--selftest-transpose` (see [`TransposeTest`]); `dt` is the time since
    /// the previous frame in ms. Returns true when the test is over.
    fn selftest_transpose_frame(&mut self, t: &mut TransposeTest, dt: f32) -> bool {
        use std::f32::consts::FRAC_PI_2;
        const DRAG: u32 = 60;
        let vp = self.viewport();
        let k = t.k;
        t.k += 1;
        let next = |t: &mut TransposeTest| {
            t.phase += 1;
            t.k = 0;
        };
        let worst = |a: &[Vec3], b: &[Vec3]| -> f32 {
            use rayon::prelude::*;
            a.par_iter().zip(b).map(|(p, q)| (*p - *q).length()).reduce(|| 0.0, f32::max)
        };
        match t.phase {
            // Warm up (upload, first normal pass), then time idle frames.
            0 => {
                if k > 30 {
                    t.idle.push(dt);
                }
                if k == 90 {
                    t.ok = true;
                    t.original = self.doc.mesh().positions.clone();
                    // Centre the active subtool, so the action line stays inside the frame
                    // however many subtools there are.
                    let b = self.doc.bounds();
                    self.camera.target = (b.min + b.max) * 0.5;
                    self.camera.distance = (b.max - b.min).length() * 1.6;
                    next(t);
                }
            }
            // W, then draw an action line from the middle of the subtool to the right.
            1 => {
                let b = self.doc.bounds();
                let centre = self.project((b.min + b.max) * 0.5);
                // Short enough that the circles stay inside the document frame (the border
                // around it always navigates) even with several subtools framed.
                let path = Vec2::new(0.12 * vp.y, 0.0);
                if k == 0 {
                    self.on_key(KeyCode::KeyW);
                    self.cursor_moved(centre, false);
                    self.left_press(false, false);
                    let pass = matches!(self.transpose, Some(TransposeDrag::Draw { .. }));
                    t.ok &= pass;
                    log::info!("selftest-transpose: press on the subtool starts a line ({})", if pass { "ok" } else { "FAIL" });
                    t.press = centre;
                } else if k <= 10 {
                    self.cursor_moved(t.press + path * (k as f32 / 10.0), false);
                } else {
                    self.left_release();
                    let pass = self.action_line.is_some_and(|l| (l.end - l.start).length() > 1e-3);
                    t.ok &= pass;
                    log::info!("selftest-transpose: drew the action line ({})", if pass { "ok" } else { "FAIL" });
                    next(t);
                }
            }
            // Move (W): drag the middle circle; Scale (E) and Rotate (R): drag the end circle.
            2..=4 => {
                let mode = [Mode::Move, Mode::Scale, Mode::Rotate][t.phase - 2];
                let line = self.action_line.unwrap();
                if k == 0 {
                    self.on_key([KeyCode::KeyW, KeyCode::KeyE, KeyCode::KeyR][t.phase - 2]);
                    t.before = self.doc.mesh().positions.clone();
                    t.press = self.project(if mode == Mode::Move { line.mid() } else { line.end });
                    t.pivot = self.project(line.start);
                    self.cursor_moved(t.press, false);
                    self.left_press(false, false);
                    let pass = matches!(self.transpose, Some(TransposeDrag::Act { .. }));
                    t.ok &= pass;
                    if !pass {
                        log::info!("selftest-transpose: {} press missed its circle (FAIL)", mode.name());
                        return true;
                    }
                } else if k <= DRAG {
                    if k >= 2 {
                        t.drag.push(dt);
                    }
                    let f = k as f32 / DRAG as f32;
                    let to = match mode {
                        Mode::Move => t.press + Vec2::new(0.08, -0.06) * vp.y * f,
                        Mode::Scale => t.pivot + (t.press - t.pivot) * (1.0 + 0.5 * f),
                        _ => {
                            let r = t.press - t.pivot;
                            t.pivot + Vec2::from_angle(FRAC_PI_2 * f).rotate(r)
                        }
                    };
                    self.cursor_moved(to, false);
                } else if k == DRAG + 1 {
                    // Only the release in this frame, so the next frame time is its cost.
                    let map = self.preview_map();
                    let shown = self.action_line.unwrap();
                    t.release.push(self.end_transpose_timed());
                    t.applied = Some((map, shown));
                } else if k == DRAG + 2 {
                    t.release_frame.push(dt);
                } else if k == DRAG + 3 {
                    // GPU work queued by the release (the in-place map, the normal pass after a
                    // rotation) can finish in the frame after it.
                    t.release_frame.push(dt);
                    let (map, shown) = t.applied.take().unwrap();
                    let ms = *t.release.last().unwrap();
                    // The exact CPU transform is the map the GPU showed during the drag.
                    let want: Vec<Vec3> = {
                        use rayon::prelude::*;
                        t.before.par_iter().map(|&p| map.apply(p)).collect()
                    };
                    let err = worst(&self.doc.mesh().positions, &want);
                    let (detail, pass) = match mode {
                        Mode::Move => {
                            let got = self.project(shown.mid());
                            let want = t.press + Vec2::new(0.08, -0.06) * vp.y;
                            (format!("middle circle {:.2} px from the cursor", (got - want).length()), (got - want).length() < 0.5)
                        }
                        Mode::Scale => (format!("scale {:.4} (want 1.5)", map.scale), (map.scale - 1.5).abs() < 1e-3),
                        _ => {
                            let deg = map.rotation.to_axis_angle().1.to_degrees();
                            (format!("rotation {deg:.2}° (want 90)"), (deg - 90.0).abs() < 0.5)
                        }
                    };
                    let pass = pass && err < 1e-4 && self.history.can_undo();
                    t.ok &= pass;
                    let n = t.release_frame.len();
                    log::info!(
                        "selftest-transpose: {}: {detail}, CPU result vs the previewed map {err:.1e}, CPU transform {ms:.1} ms, release frame {:.1} ms + next frame {:.1} ms ({})",
                        mode.name(),
                        t.release_frame[n - 2],
                        dt,
                        if pass { "ok" } else { "FAIL" }
                    );
                } else if k > DRAG + 4 {
                    next(t);
                }
            }
            // Ctrl + Z three times through the deferred-undo path, then compare.
            _ => {
                if k < 3 {
                    self.pending_undo = true;
                } else if k == 4 {
                    let err = worst(&self.doc.mesh().positions, &t.original);
                    let pass = err < 1e-4 && self.history.can_redo();
                    t.ok &= pass;
                    log::info!("selftest-transpose: three undos restore the subtool to {err:.1e} ({})", if pass { "ok" } else { "FAIL" });
                    // The GPU copy was mapped in place six times, never uploaded: it has to
                    // agree with the CPU.
                    if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                        let err = worst(&gpu, &self.doc.mesh().positions);
                        let pass = err < 1e-4;
                        t.ok &= pass;
                        log::info!("selftest-transpose: GPU copy of the shown level vs CPU {err:.1e} ({})", if pass { "ok" } else { "FAIL" });
                    }
                    let avg = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
                    log::info!(
                        "selftest-transpose: level {} ({} vertices): idle frame {:.2} ms, frame while dragging (GPU preview) {:.2} ms, release frame + next frame: move {:.1} + {:.1}, scale {:.1} + {:.1}, rotate {:.1} + {:.1} ms",
                        self.doc.active(),
                        fmt_count(self.mesh().vertex_count()),
                        avg(&t.idle),
                        avg(&t.drag),
                        t.release_frame[0],
                        t.release_frame[1],
                        t.release_frame[2],
                        t.release_frame[3],
                        t.release_frame[4],
                        t.release_frame[5],
                    );
                    log::info!("selftest-transpose: {}", if t.ok { "all passed" } else { "FAILED" });
                    return true;
                }
            }
        }
        false
    }

    /// One frame of `--selftest-mask` (see [`MaskTest`]). Returns true when it is over.
    fn selftest_mask_frame(&mut self, t: &mut MaskTest, dt: f32) -> bool {
        let vp = self.viewport();
        let k = t.k;
        t.k += 1;
        let next = |t: &mut MaskTest| {
            t.phase += 1;
            t.k = 0;
        };
        let check = |t: &mut MaskTest, what: &str, pass: bool| {
            t.ok &= pass;
            log::info!("selftest-mask: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let masked = |app: &App| app.doc.mesh().mask.iter().filter(|&&m| m > 127).count();
        let centre = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
        // Inside the document frame but off the model.
        let background = self.canvas_origin() + Vec2::splat(50.0 * self.scale);
        // Press, then release on the same frame: a click.
        let click = |app: &mut App, at: Vec2, m: ModifiersState| -> f32 {
            app.modifiers = m;
            app.cursor_moved(at, false);
            let t0 = Instant::now();
            app.left_press(false, false);
            app.left_release();
            app.modifiers = ModifiersState::empty();
            t0.elapsed().as_secs_f32() * 1000.0
        };
        match t.phase {
            0 => {
                if k > 30 {
                    t.idle.push(dt);
                }
                if k == 90 {
                    t.ok = true;
                    t.original = self.doc.mesh().positions.clone();
                    self.on_key(KeyCode::KeyQ);
                    next(t);
                }
            }
            // Ctrl + drag on the model paints the mask.
            1 => {
                if k == 0 {
                    t.press = centre;
                    self.modifiers = ModifiersState::CONTROL;
                    self.cursor_moved(centre, false);
                    self.left_press(false, false);
                } else if k <= 30 {
                    self.cursor_moved(t.press + Vec2::new(0.25 * vp.y, 0.0) * (k as f32 / 30.0), false);
                    if self.mask_stroke.is_some() {
                        t.dabs.push(self.stroke_ms);
                    }
                } else {
                    self.left_release();
                    self.modifiers = ModifiersState::empty();
                    let n = masked(self);
                    check(t, &format!("Ctrl + drag masked {n} vertices"), n > 0 && self.history.can_undo());
                    next(t);
                }
            }
            // Frames with the mask shown.
            2 => {
                if k > 2 {
                    t.shaded.push(dt);
                }
                if k == 60 {
                    let gpu = self.gfx.as_ref().and_then(Gfx::read_mask).unwrap_or_default();
                    let cpu = &self.doc.mesh().mask;
                    check(t, "GPU mask matches the CPU", gpu.len() >= cpu.len() && gpu[..cpu.len()] == cpu[..]);
                    next(t);
                }
            }
            // Ctrl + click on the background inverts (twice), Ctrl + click on the model blurs.
            3 => {
                let n = masked(self);
                let ms = click(self, background, ModifiersState::CONTROL);
                t.costs.push(("invert (Ctrl + click background)", ms));
                let total = self.doc.mesh().vertex_count();
                check(t, "Ctrl + click on the background inverts", masked(self) == total - n);
                click(self, background, ModifiersState::CONTROL);
                let soft = |app: &App| app.doc.mesh().mask.iter().filter(|&&m| m > 10 && m < 245).count();
                let s0 = soft(self);
                let ms = click(self, centre, ModifiersState::CONTROL);
                t.costs.push(("blur (Ctrl + click model)", ms));
                check(t, "Ctrl + click on the model blurs", masked(self) > 0 && soft(self) > s0);
                next(t);
            }
            // The mask follows a level change and comes back unchanged.
            4 => {
                let level = self.doc.active();
                if level > 0 {
                    let mask = self.doc.mesh().mask.clone();
                    let t0 = Instant::now();
                    self.set_level(level - 1);
                    let down = t0.elapsed().as_secs_f32() * 1000.0;
                    let t0 = Instant::now();
                    self.set_level(level);
                    let up = t0.elapsed().as_secs_f32() * 1000.0;
                    t.costs.push(("level down with the mask", down));
                    t.costs.push(("level up with the mask", up));
                    // Blurred values do not survive the round trip exactly: even vertices do.
                    let same = self.doc.mesh().mask.len() == mask.len();
                    check(t, "the mask follows D / Shift + D", same && masked(self) > 0);
                }
                next(t);
            }
            // W: a line from the middle, then drag its middle circle up.
            5 => {
                if k == 0 {
                    self.on_key(KeyCode::KeyW);
                    self.cursor_moved(centre, false);
                    self.left_press(false, false);
                } else if k <= 5 {
                    self.cursor_moved(centre + Vec2::new(0.2 * vp.y, 0.0) * (k as f32 / 5.0), false);
                } else if k == 6 {
                    self.left_release();
                    t.before = self.doc.mesh().positions.clone();
                    t.press = self.project(self.action_line.unwrap().mid());
                    self.cursor_moved(t.press, false);
                    self.left_press(false, false);
                } else if k <= 66 {
                    if k > 8 {
                        t.drag.push(dt);
                    }
                    self.cursor_moved(t.press + Vec2::new(0.0, -0.15 * vp.y) * ((k - 6) as f32 / 60.0), false);
                } else if k == 67 {
                    let t0 = Instant::now();
                    self.left_release();
                    t.costs.push(("masked move release (CPU)", t0.elapsed().as_secs_f32() * 1000.0));
                } else if k == 68 {
                    t.costs.push(("masked move release frame", dt));
                } else {
                    let mesh = self.doc.mesh();
                    let (mut held, mut moved) = (0.0f32, 0.0f32);
                    for ((p, q), &m) in t.before.iter().zip(&mesh.positions).zip(&mesh.mask) {
                        let d = (*p - *q).length();
                        if m == 255 {
                            held = held.max(d);
                        } else if m == 0 {
                            moved = moved.max(d);
                        }
                    }
                    check(t, &format!("masked move: fully masked vertices moved {held:.1e}, free ones up to {moved:.3}"), held < 1e-4 && moved > 0.05);
                    if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                        let err = gpu.iter().zip(&mesh.positions).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max);
                        check(t, &format!("GPU positions match the CPU after the masked move ({err:.1e})"), err < 1e-4);
                    }
                    next(t);
                }
            }
            // Ctrl + drag on the background clears; then undo everything.
            6 => {
                if k == 0 {
                    self.modifiers = ModifiersState::CONTROL;
                    self.cursor_moved(background, false);
                    self.left_press(false, false);
                } else if k == 1 {
                    self.cursor_moved(background + Vec2::new(40.0, 30.0), false);
                } else if k == 2 {
                    self.left_release();
                    self.modifiers = ModifiersState::empty();
                    check(t, "Ctrl + drag on the background clears", self.doc.mesh().mask.is_empty());
                } else if k < 9 {
                    // Clear, move, blur, invert, invert, stroke.
                    self.pending_undo = true;
                } else if k == 10 {
                    let err = self.doc.mesh().positions.iter().zip(&t.original).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max);
                    check(t, &format!("six undos restore positions ({err:.1e}) and the empty mask"), err < 1e-4 && masked(self) == 0);
                    let avg = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
                    log::info!(
                        "selftest-mask: level {} ({} vertices): frame {:.2} ms without a mask, {:.2} ms with it shown, {:.2} ms dragging a masked move; mask dab {:.2} ms avg",
                        self.doc.active(),
                        fmt_count(self.mesh().vertex_count()),
                        avg(&t.idle),
                        avg(&t.shaded),
                        avg(&t.drag),
                        avg(&t.dabs),
                    );
                    for (what, ms) in &t.costs {
                        log::info!("selftest-mask: {what}: {ms:.1} ms");
                    }
                    log::info!("selftest-mask: {}", if t.ok { "all passed" } else { "FAILED" });
                    return true;
                }
            }
            _ => return true,
        }
        false
    }

    /// Release the left button and return how long the CPU side took (ms).
    fn end_transpose_timed(&mut self) -> f32 {
        let t = Instant::now();
        self.left_release();
        t.elapsed().as_secs_f32() * 1000.0
    }

    /// The subtool under the cursor, from the GPU id pass.
    fn pick_subtool(&mut self) -> Option<usize> {
        let parked: Vec<(u64, u32)> = self
            .drawn_parked()
            .into_iter()
            .map(|(key, i)| (key, i as u32 + 1))
            .collect();
        let viewport = [
            self.canvas_rect.min.x,
            self.canvas_rect.min.y,
            self.canvas_rect.width(),
            self.canvas_rect.height(),
        ];
        let (x, y) = (self.cursor.x.max(0.0) as u32, self.cursor.y.max(0.0) as u32);
        let active = self.active_tool as u32 + 1;
        let id = self.gfx.as_mut()?.pick(x, y, viewport, active, &parked);
        (id as usize).checked_sub(1)
    }

    /// Add a subdivision level above the top one and show it (ZBrush: Ctrl + D).
    fn divide(&mut self) {
        if self.busy() {
            return;
        }
        if !self.doc.can_divide() {
            self.workspace.notify(format!(
                "Next level would have {} vertices, over the budget of {}",
                fmt_count(self.doc.next_vertex_count()),
                fmt_count(self.doc.budget)
            ));
            return;
        }
        let t = Instant::now();
        self.doc.divide();
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        self.history.push(UndoStep::Divide(None));
        self.show_active_level();
        self.status = format!(
            "Level {}: {} vertices, divided in {:.0} ms",
            self.doc.active(),
            fmt_count(self.mesh().vertex_count()),
            ms
        );
        log::info!("{}", self.status);
    }

    /// Show another subdivision level (ZBrush: D up, Shift + D down).
    fn set_level(&mut self, level: usize) {
        if self.busy() || level == self.doc.active() || level > self.doc.top() {
            return;
        }
        let t = Instant::now();
        self.doc.set_active(level);
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        self.show_active_level();
        self.status = format!(
            "Level {} of {}: {} vertices ({:.1} ms)",
            self.doc.active(),
            self.doc.top(),
            fmt_count(self.mesh().vertex_count()),
            ms
        );
        log::info!("{}", self.status);
    }

    /// Point the renderer at the active level after it changed. If the level does not fit
    /// in GPU memory, the highest level below it that does is shown instead.
    fn show_active_level(&mut self) {
        let wanted = self.doc.active();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.truncate_levels(self.doc.count());
            if !gfx.show_level(self.doc.mesh_mut(), wanted) {
                let mut level = wanted;
                while level > 0 {
                    level -= 1;
                    self.doc.set_active(level);
                    if gfx.show_level(self.doc.mesh_mut(), level) {
                        break;
                    }
                }
                let msg = format!(
                    "Level {wanted} ({} vertices) does not fit in GPU memory · showing level {}",
                    fmt_count(self.doc.vertex_counts()[wanted]),
                    self.doc.active()
                );
                log::warn!("{msg}");
                self.workspace.notify(msg);
            }
        }
        self.update_hover();
    }

    /// Scripted strokes through the real input path: used by `--demo` and `--bench`.
    fn scripted_strokes(&mut self) -> Vec<(String, f32, u32, usize)> {
        let vp = self.viewport();
        let c = self.canvas_origin() + vp * 0.5;
        let r = vp.y * 0.33;
        let mut report = Vec::new();
        let scripts: Vec<(BrushKind, f32, f32, bool, Vec<Vec2>)> = vec![
            // kind, radius, strength, invert, path (unit circle coords)
            (BrushKind::Draw, 0.18, 0.8, false, arc(0.2, 1.4, 0.55, 60)),
            (BrushKind::Draw, 0.12, 0.8, true, arc(-0.9, -0.1, 0.5, 40)),
            (BrushKind::Inflate, 0.2, 0.7, false, arc(2.0, 2.8, 0.6, 40)),
            (
                BrushKind::Flatten,
                0.35,
                0.9,
                false,
                vec![Vec2::new(0.0, 0.0); 20],
            ),
            (BrushKind::Smooth, 0.2, 0.8, false, arc(0.2, 1.4, 0.55, 40)),
            (
                BrushKind::Grab,
                0.3,
                0.6,
                false,
                line(Vec2::new(-0.5, 0.55), Vec2::new(-0.2, 0.9), 15),
            ),
            // Large-radius strokes: these cover a big share of the mesh.
            (BrushKind::Draw, 0.8, 0.5, false, arc(3.5, 4.5, 0.4, 30)),
            (
                BrushKind::Grab,
                1.2,
                0.5,
                false,
                line(Vec2::new(0.3, -0.3), Vec2::new(0.5, -0.1), 30),
            ),
        ];
        for (kind, radius, strength, invert, path) in scripts {
            self.brush = Brush {
                kind,
                radius,
                strength,
            };
            self.modifiers = if invert {
                ModifiersState::ALT
            } else {
                ModifiersState::empty()
            };
            let t = Instant::now();
            let mut dabs = 0;
            let mut count = 0;
            for (i, p) in path.iter().enumerate() {
                self.cursor = c + *p * r;
                if i == 0 {
                    self.begin_stroke();
                } else {
                    self.apply_stroke();
                }
            }
            if let Some(s) = &self.stroke {
                dabs = s.dabs;
                count = s.last_count;
            }
            self.end_stroke();
            report.push((
                kind.name().to_string(),
                t.elapsed().as_secs_f32() * 1000.0,
                dabs,
                count,
            ));
        }
        self.modifiers = ModifiersState::empty();
        self.brush = Brush::default();
        report
    }

    fn canvas_origin(&self) -> Vec2 {
        if self.canvas_rect.is_positive() {
            Vec2::new(self.canvas_rect.min.x, self.canvas_rect.min.y)
        } else {
            Vec2::ZERO
        }
    }

    fn viewport(&self) -> Vec2 {
        if self.canvas_rect.is_positive() {
            return Vec2::new(self.canvas_rect.width(), self.canvas_rect.height());
        }
        match &self.gfx {
            Some(g) => Vec2::new(g.config.width as f32, g.config.height as f32),
            None => Vec2::new(1600.0, 1000.0),
        }
    }

    fn cursor_ray(&self) -> (Vec3, Vec3) {
        let vp = self.viewport();
        let cursor = self.cursor - self.canvas_origin();
        self.camera.ray(cursor.x, cursor.y, vp.x, vp.y)
    }

    fn effective_brush(&self) -> Brush {
        let mut b = self.brush;
        if self.modifiers.shift_key() {
            b.kind = BrushKind::Smooth;
        }
        b
    }

    fn begin_stroke(&mut self) {
        self.stroke_brush = self.effective_brush();
        let gpu = self.gfx.is_some() && self.stroke_brush.kind == BrushKind::Grab;
        // Alt is ZBrush's alternate (ZSub) modifier.
        let invert = self.modifiers.alt_key() ^ self.workspace.subtract;
        let symmetry = self.workspace.symmetry;
        log::trace!("stroke {:?} invert={invert} symmetry={symmetry}", self.stroke_brush.kind);
        self.stroke = Some(Stroke::begin(invert, gpu, symmetry));
        self.apply_stroke();
    }

    fn apply_stroke(&mut self) {
        let t = Instant::now();
        let ray = self.cursor_ray();
        let fwd = self.camera.forward();
        let Some(stroke) = self.stroke.as_mut() else {
            return;
        };
        if stroke.grabbing() {
            stroke.dab(
                self.doc.mesh_mut(),
                &self.stroke_brush,
                Vec3::ZERO,
                Vec3::Y,
                fwd,
                ray,
            );
        } else if let Some(hit) = self.doc.mesh().raycast(ray.0, ray.1) {
            self.hover = Some(hit.point);
            stroke.dab(
                self.doc.mesh_mut(),
                &self.stroke_brush,
                hit.point,
                hit.normal,
                fwd,
                ray,
            );
        }
        // Hand a GPU grab to the renderer: buffers once, then one delta per frame.
        if let (Some(stroke), Some(gfx)) = (self.stroke.as_mut(), self.gfx.as_mut()) {
            if !gfx.gpu_grab_active() {
                if let Some(setup) = stroke.gpu_grab_setup() {
                    let flags = self.doc.mesh().normal_flags(setup.chunks);
                    gfx.begin_gpu_grab(setup, flags);
                }
            }
            if let Some(delta) = stroke.take_gpu_delta() {
                gfx.gpu_grab_delta(delta);
            }
        }
        self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
        if let Some(s) = &self.stroke {
            self.last_dab_count = s.last_count;
        }
    }

    fn end_stroke(&mut self) {
        if self.stroke_pending {
            self.stroke_pending = false;
            self.apply_stroke();
        }
        if let Some(stroke) = self.stroke.take() {
            let level = self.doc.active();
            if let Some(snapshot) = stroke.end(self.doc.mesh_mut()) {
                let chunks = self.doc.finish_stroke(level, snapshot);
                self.history.push(UndoStep::Stroke { level, chunks });
            }
        }
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.end_gpu_grab();
        }
    }

    fn update_hover(&mut self) {
        let (o, d) = self.cursor_ray();
        self.hover = self.mesh().raycast(o, d).map(|h| h.point);
        log::trace!(
            "hover {:?} cursor={:?} canvas={:?} yaw={} pitch={} roll={} o={o:?} d={d:?}",
            self.hover, self.cursor, self.canvas_rect, self.camera.yaw, self.camera.pitch, self.camera.roll
        );
    }

    fn frame_camera(&mut self) {
        let b = self.model_bounds();
        self.camera.target = (b.min + b.max) * 0.5;
        self.camera.distance = (b.max - b.min).length() * 1.1;
    }

    /// Default view angle plus a frame: `Home`.
    fn home_camera(&mut self) {
        let fresh = Camera::new();
        self.camera.yaw = fresh.yaw;
        self.camera.pitch = fresh.pitch;
        self.camera.roll = fresh.roll;
        self.frame_camera();
    }

    fn begin_nav(&mut self, button: MouseButton) {
        self.nav = Some(NavDrag {
            button,
            alt_used: self.modifiers.alt_key(),
            shift_at_press: self.modifiers.shift_key(),
        });
        self.nav_press = self.cursor;
        // No brush ring while the camera moves.
        self.hover = None;
    }

    fn end_nav(&mut self, button: MouseButton) {
        if self.nav.map(|n| n.button) == Some(button) {
            self.nav = None;
            self.update_hover();
        }
    }

    fn nav_move(&mut self, drag: NavDrag, delta: Vec2) {
        let mode = drag.mode(self.modifiers);
        log::trace!("nav {mode:?} {:?} dx={} dy={}", drag.button, delta.x, delta.y);
        match mode {
            NavMode::Orbit => {
                self.camera.orbit(delta.x, delta.y);
                if self.modifiers.shift_key() {
                    self.camera.snap_to_axis();
                }
            }
            NavMode::Pan => self.camera.pan(delta.x, delta.y, self.viewport().y),
            NavMode::Zoom => self.camera.dolly(delta.x, delta.y),
            NavMode::Roll => {
                // Follow the cursor's angle about the canvas centre.
                let c = self.canvas_origin() + self.viewport() * 0.5;
                let (a, b) = (self.last_cursor - c, self.cursor - c);
                if a.length() > 8.0 && b.length() > 8.0 {
                    let d = b.y.atan2(b.x) - a.y.atan2(a.x);
                    let d = (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                        - std::f32::consts::PI;
                    // Screen y points down: the scene follows the cursor around the centre.
                    self.camera.roll_by(d);
                }
            }
        }
    }

    /// A whole-mask operation as one undo step (Masking palette, Ctrl + I / A / Shift + A,
    /// Ctrl + click). Returns false when it changed nothing.
    fn mask_op(&mut self, op: MaskOp) -> bool {
        if self.busy() {
            return false;
        }
        let level = self.doc.active();
        let mesh = self.doc.mesh_mut();
        let new = mask::apply(mesh, op);
        let unchanged = new == mesh.mask
            || (new.is_empty() && mesh.mask.iter().all(|&m| m == 0))
            || (mesh.mask.is_empty() && new.iter().all(|&m| m == 0));
        if unchanged {
            return false;
        }
        let old = mask::replace(mesh, new);
        self.history.push(UndoStep::Mask {
            level,
            data: MaskData::Whole(old),
        });
        true
    }

    /// Ctrl + left press (ZBrush masking): decided on the first move or on release.
    fn begin_mask_press(&mut self, border: bool) {
        if self.modifiers.shift_key() {
            self.workspace.notify("Ctrl + Shift selection (hide) is not connected yet");
            return;
        }
        let hit = if border {
            None
        } else {
            let (o, d) = self.cursor_ray();
            self.mesh().raycast(o, d).map(|h| h.point)
        };
        self.mask_press = Some(MaskPress {
            at: self.cursor,
            hit,
            alt: self.modifiers.alt_key(),
            moved: false,
        });
        self.hover = None;
    }

    /// One mask dab under the cursor.
    fn mask_dab(&mut self) {
        let t = Instant::now();
        let (o, d) = self.cursor_ray();
        let Some(hit) = self.mesh().raycast(o, d).map(|h| h.point) else {
            return;
        };
        let brush = self.brush;
        if let Some(stroke) = self.mask_stroke.as_mut() {
            stroke.dab(self.doc.mesh_mut(), &brush, hit);
        }
        self.hover = Some(hit);
        self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
    }

    fn mask_move(&mut self) {
        if self.mask_stroke.is_some() {
            self.mask_dab();
            return;
        }
        let Some(press) = self.mask_press.as_mut() else {
            return;
        };
        if press.moved || (self.cursor - press.at).length() < 4.0 {
            return;
        }
        press.moved = true;
        let press = *press;
        let Some(hit) = press.hit else {
            return; // a drag on the background clears the mask on release
        };
        if self.workspace.mode != Mode::Draw {
            self.workspace.notify("Masking along the action line is not connected yet · Q to paint masks");
            return;
        }
        // A drag on the model: paint from the press point on.
        self.mask_press = None;
        let mut stroke = mask::MaskStroke::begin(press.alt, self.workspace.symmetry);
        let brush = self.brush;
        stroke.dab(self.doc.mesh_mut(), &brush, hit);
        self.mask_stroke = Some(stroke);
        self.mask_dab();
    }

    fn end_mask(&mut self) {
        if let Some(stroke) = self.mask_stroke.take() {
            if let Some(chunks) = stroke.end() {
                self.history.push(UndoStep::Mask {
                    level: self.doc.active(),
                    data: MaskData::Chunks(chunks),
                });
            }
        }
        let Some(press) = self.mask_press.take() else {
            return;
        };
        let op = match (press.hit.is_some(), press.moved, press.alt) {
            (true, false, false) => Some(MaskOp::Blur),
            (true, false, true) => Some(MaskOp::Sharpen),
            (false, false, _) => Some(MaskOp::Invert),
            (false, true, _) => Some(MaskOp::Clear),
            (true, true, _) => None,
        };
        if let Some(op) = op {
            self.mask_op(op);
        }
    }

    /// The cursor moved to `pos` (window pixels).
    fn cursor_moved(&mut self, pos: Vec2, over_ui: bool) {
        self.last_cursor = self.cursor;
        self.cursor = pos;
        let delta = self.cursor - self.last_cursor;
        if let Some(drag) = self.nav {
            self.nav_move(drag, delta);
        } else if self.transpose.is_some() {
            self.transpose_move();
        } else if self.mask_press.is_some() || self.mask_stroke.is_some() {
            self.mask_move();
        } else if self.stroke.is_some() {
            if self.stroke_brush.kind == BrushKind::Grab {
                self.stroke_pending = true;
            } else {
                self.apply_stroke();
            }
        } else if !over_ui && !self.in_canvas_border() {
            self.update_hover();
        } else {
            self.hover = None;
        }
    }

    /// Left button pressed. `consumed`: egui took the event; `over_ui`: the cursor is off
    /// the canvas.
    fn left_press(&mut self, consumed: bool, over_ui: bool) {
        let border = self.in_canvas_border();
        let picked = if self.modifiers.alt_key()
            && !self.modifiers.control_key()
            && !border
            && self.subtools.len() > 1
            && !consumed
            && !over_ui
            && !self.busy()
            && self.nav.is_none()
            && self.workspace.popup.is_none()
            && self.workspace.quick_menu.is_none()
        {
            self.pick_subtool().filter(|&i| i != self.active_tool)
        } else {
            None
        };
        if self.workspace.popup.is_some() || self.workspace.quick_menu.is_some() {
            // The press that closes a popup or menu does nothing else.
        } else if let Some(i) = picked {
            // ZBrush: Alt + click on another subtool selects it.
            self.select_subtool(i);
        } else if !consumed && !over_ui && self.nav.is_none() && !self.busy() {
            // ZBrush: a left drag on the model sculpts (Alt = alternate brush, Ctrl =
            // masking), or in Move / Scale / Rotate mode draws and drives the Transpose
            // action line; on empty canvas it rotates, with Alt it moves, and releasing Alt
            // turns it into a zoom. The border outside the document frame is always canvas.
            // Ctrl masks (see `begin_mask_press`).
            if self.modifiers.control_key() {
                self.begin_mask_press(border);
                return;
            }
            if border {
                self.begin_nav(MouseButton::Left);
                return;
            }
            if self.workspace.mode != Mode::Draw
                && !self.modifiers.control_key()
                && self.begin_transpose()
            {
                return;
            }
            let (o, d) = self.cursor_ray();
            let hit = self.mesh().raycast(o, d);
            log::trace!("press cursor={:?} hit={:?}", self.cursor, hit.as_ref().map(|h| h.point));
            if hit.is_none() {
                self.begin_nav(MouseButton::Left);
            } else {
                self.begin_stroke();
            }
        }
    }

    fn left_release(&mut self) {
        self.end_mask();
        self.end_stroke();
        self.end_transpose();
        self.end_nav(MouseButton::Left);
    }

    /// Logical (egui) position of the cursor.
    fn cursor_pos(&self) -> egui::Pos2 {
        egui::pos2(self.cursor.x / self.scale, self.cursor.y / self.scale)
    }

    /// Replace the active subtool's mesh with a fresh sphere of `segments`.
    fn rebuild(&mut self) {
        let t = Instant::now();
        let (p, i) = cube_sphere(self.segments, 1.0);
        self.doc = Multires::new(SculptMesh::from_raw(p, i), self.doc.budget);
        self.build_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.history.clear();
        self.stroke = None;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.show_active_level();
        self.status = format!(
            "Built {} vertices in {:.0} ms",
            fmt_count(self.mesh().vertex_count()),
            self.build_ms
        );
    }

    fn on_key(&mut self, key: KeyCode) {
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        match key {
            KeyCode::Escape if self.workspace.popup.is_some() => self.workspace.popup = None,
            KeyCode::Escape if self.workspace.focus => self.workspace.focus = false,
            KeyCode::KeyS if ctrl => self.pending_doc = Some(DocOp::Save),
            KeyCode::KeyO if ctrl => self.pending_doc = Some(DocOp::Open),
            // ZBrush masking: Ctrl + I invert, Ctrl + A mask all, Ctrl + Shift + A clear,
            // Ctrl + H view mask.
            KeyCode::KeyI if ctrl => self.pending_mask = Some(MaskOp::Invert),
            KeyCode::KeyA if ctrl && shift => self.pending_mask = Some(MaskOp::Clear),
            KeyCode::KeyA if ctrl => self.pending_mask = Some(MaskOp::All),
            KeyCode::KeyH if ctrl => self.workspace.show_mask = !self.workspace.show_mask,
            KeyCode::KeyZ if ctrl && shift => self.pending_redo = true,
            KeyCode::KeyZ if ctrl => self.pending_undo = true,
            KeyCode::KeyY if ctrl => self.pending_redo = true,
            // ZBrush: Ctrl + D divides, D and Shift + D step through the levels.
            KeyCode::KeyD if ctrl => self.pending_divide = true,
            KeyCode::KeyD if shift => self.pending_level = Some(self.doc.active().saturating_sub(1)),
            KeyCode::KeyD => self.pending_level = Some(self.doc.active() + 1),
            KeyCode::BracketLeft => self.brush.radius = (self.brush.radius * 0.8).max(0.01),
            KeyCode::BracketRight => self.brush.radius = (self.brush.radius * 1.25).min(2.0),
            KeyCode::KeyF => self.frame_camera(),
            KeyCode::Home => self.home_camera(),
            KeyCode::KeyP if shift => self.workspace.grid = !self.workspace.grid,
            KeyCode::KeyP => self.workspace.perspective = !self.workspace.perspective,
            KeyCode::Digit1 => self.brush.kind = BrushKind::Draw,
            KeyCode::Digit2 => self.brush.kind = BrushKind::Smooth,
            KeyCode::Digit3 => self.brush.kind = BrushKind::Grab,
            KeyCode::Digit4 => self.brush.kind = BrushKind::Flatten,
            KeyCode::Digit5 => self.brush.kind = BrushKind::Inflate,
            KeyCode::KeyQ => self.workspace.mode = Mode::Draw,
            KeyCode::KeyW => self.workspace.mode = Mode::Move,
            KeyCode::KeyE => self.workspace.mode = Mode::Scale,
            KeyCode::KeyR => self.workspace.mode = Mode::Rotate,
            KeyCode::KeyX => self.workspace.symmetry = !self.workspace.symmetry,
            KeyCode::KeyS => self.workspace.toggle_popup(QuickPopup::Size, self.cursor_pos()),
            KeyCode::KeyU => self.workspace.toggle_popup(QuickPopup::Intensity, self.cursor_pos()),
            KeyCode::KeyI => self.workspace.toggle_popup(QuickPopup::Rgb, self.cursor_pos()),
            KeyCode::KeyO => self.workspace.toggle_popup(QuickPopup::Focal, self.cursor_pos()),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let levels = self.doc.vertex_counts();
        let (vertices, triangles, chunks) = {
            let m = self.mesh();
            (m.vertex_count(), m.triangle_count(), m.chunks.len())
        };
        let level = self.doc.active();
        let next_level_vertices = self.doc.next_vertex_count();
        let levels_bytes = self.doc.bytes();
        let gpu_level_bytes = self.gfx.as_ref().map_or(0, Gfx::level_bytes);
        let stand_in_bytes = self.gfx.as_ref().map_or(0, Gfx::stand_in_bytes);
        let rows: Vec<SubToolRow> = self
            .subtools
            .iter()
            .enumerate()
            .map(|(i, t)| match &t.parked {
                None => SubToolRow {
                    name: t.name.clone(),
                    visible: true,
                    active: i == self.active_tool,
                    vertices,
                    level,
                    top: self.doc.top(),
                    bytes: levels_bytes,
                    drawn: true,
                },
                Some(p) => SubToolRow {
                    name: t.name.clone(),
                    visible: t.visible,
                    active: false,
                    vertices: p.packed.counts()[p.packed.active()],
                    level: p.packed.active(),
                    top: p.packed.counts().len() - 1,
                    bytes: p.packed.bytes() + p.history.bytes(),
                    drawn: p.stand_in_vertices > 0 || self.gfx.is_none(),
                },
            })
            .collect();
        let parked_bytes = rows.iter().filter(|r| !r.active).map(|r| r.bytes).sum();
        let actions = self.workspace.show(
            ui,
            &mut workspace::Model {
                brush: &mut self.brush,
                segments: &mut self.segments,
                export_path: &mut self.export_path,
                status: &self.status,
                can_undo: self.history.can_undo(),
                can_redo: self.history.can_redo(),
                history_bytes: self.history.bytes(),
                vertices,
                triangles,
                chunks,
                level,
                levels: &levels,
                next_level_vertices,
                vertex_budget: &mut self.doc.budget,
                levels_bytes,
                gpu_level_bytes,
                fps: self.fps,
                frame_ms: self.frame_ms,
                stroke_ms: self.stroke_ms,
                upload_ms: self.upload_ms,
                render_ms: self.render_ms,
                adapter: self
                    .gfx
                    .as_ref()
                    .map(|g| g.adapter_name.as_str())
                    .unwrap_or(""),
                axes: {
                    let (r, u, f) = (self.camera.right(), self.camera.up(), self.camera.forward());
                    [Vec3::X, Vec3::Y, Vec3::Z].map(|v| [v.dot(r), v.dot(u), -v.dot(f)])
                },
                subtools: &rows,
                parked_bytes,
                stand_in_bytes,
                project_path: &mut self.project_path,
            },
        );
        if actions.new_project {
            self.pending_doc = Some(DocOp::New);
        }
        if actions.open {
            self.pending_doc = Some(DocOp::Open);
        }
        if actions.save {
            self.pending_doc = Some(DocOp::Save);
        }
        if let Some(i) = actions.select_subtool {
            self.pending_subtool = Some(SubToolOp::Select(i));
        }
        if let Some(i) = actions.toggle_subtool {
            self.toggle_subtool(i);
        }
        if actions.append {
            self.pending_subtool = Some(SubToolOp::Append);
        }
        if actions.duplicate {
            self.pending_subtool = Some(SubToolOp::Duplicate);
        }
        if actions.delete {
            self.pending_subtool = Some(SubToolOp::Delete);
        }
        if actions.move_subtool != 0 {
            self.move_subtool(actions.move_subtool);
        }
        if let Some((kind, value)) = actions.deform {
            self.pending_subtool = Some(SubToolOp::Deform(kind, value));
        }
        self.pending_rebuild |= actions.rebuild;
        self.pending_export |= actions.export;
        self.pending_undo |= actions.undo;
        self.pending_redo |= actions.redo;
        self.pending_divide |= actions.divide;
        if actions.mask.is_some() {
            self.pending_mask = actions.mask;
        }
        if actions.level.is_some() {
            self.pending_level = actions.level;
        }
        if actions.frame {
            self.frame_camera();
        }
        if actions.zoom != 0.0 {
            self.camera.zoom(actions.zoom);
        }
        if let Some((yaw, pitch)) = actions.view {
            self.camera.yaw = yaw;
            self.camera.pitch = pitch;
        }
        self.paint_action_line(ui.ctx());
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.frame_ms = self.frame_ms * 0.9 + dt * 1000.0 * 0.1;
        self.fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };

        let (Some(window), Some(state)) = (self.window.clone(), self.egui_state.as_mut()) else {
            return;
        };
        let raw = state.take_egui_input(&window);
        let ctx = self.egui_ctx.clone();
        let full = ctx.run_ui(raw, |ui| self.ui(ui));
        if let Some(state) = self.egui_state.as_mut() {
            state.handle_platform_output(&window, full.platform_output);
        }
        let scale = full.pixels_per_point;
        self.scale = scale;
        let logical_canvas = self.workspace.canvas;
        self.canvas_rect =
            egui::Rect::from_min_max(logical_canvas.min * scale, logical_canvas.max * scale);
        let jobs = ctx.tessellate(full.shapes, full.pixels_per_point);

        // Deferred UI actions.
        if self.pending_rebuild {
            self.pending_rebuild = false;
            self.rebuild();
        }
        if self.pending_undo {
            self.pending_undo = false;
            if !self.busy() {
                match self.history.next_undo_map() {
                    Some(map) => {
                        self.transform_doc(&map, |app| app.history.undo(&mut app.doc));
                    }
                    None => self.history.undo(&mut self.doc),
                }
                self.show_active_level();
            }
        }
        if self.pending_redo {
            self.pending_redo = false;
            if !self.busy() {
                match self.history.next_redo_map() {
                    Some(map) => {
                        self.transform_doc(&map, |app| app.history.redo(&mut app.doc));
                    }
                    None => self.history.redo(&mut self.doc),
                }
                self.show_active_level();
            }
        }
        if self.pending_divide {
            self.pending_divide = false;
            self.divide();
        }
        if let Some(op) = self.pending_mask.take() {
            self.mask_op(op);
        }
        if let Some(level) = self.pending_level.take() {
            self.set_level(level);
        }
        match self.pending_doc.take() {
            Some(DocOp::New) => self.new_project(),
            Some(DocOp::Open) => {
                let path = self.project_path.clone();
                self.open_project(&path);
            }
            Some(DocOp::Save) => self.save_project(),
            None => {}
        }
        match self.pending_subtool.take() {
            Some(SubToolOp::Select(i)) => self.select_subtool(i),
            Some(SubToolOp::Append) => self.append_sphere(),
            Some(SubToolOp::Duplicate) => self.duplicate_subtool(),
            Some(SubToolOp::Delete) => self.delete_subtool(),
            Some(SubToolOp::Deform(kind, value)) => self.deform(kind, value),
            None => {}
        }
        if self.pending_export {
            self.pending_export = false;
            self.export_obj_all();
        }

        if let Some((left, frame_acc, dab_acc)) = self.stress {
            const TOTAL: u32 = 240;
            let vp = self.viewport();
            let c = self.canvas_origin() + vp * 0.5;
            if left == TOTAL {
                self.brush = Brush {
                    kind: BrushKind::Grab,
                    radius: 1.2,
                    strength: 0.5,
                };
                self.cursor = c + Vec2::new(0.3, -0.3) * vp.y * 0.33;
                self.begin_stroke();
            } else if left == 0 {
                self.end_stroke();
                log::info!(
                    "stress: {} frames of a {} vertex grab, avg frame {:.2} ms, avg dab (CPU) {:.2} ms, upload {:.2} ms, render submit {:.2} ms",
                    TOTAL,
                    fmt_count(self.last_dab_count),
                    frame_acc / TOTAL as f32,
                    dab_acc / TOTAL as f32,
                    self.stress_upload / TOTAL as f32,
                    self.stress_render / TOTAL as f32
                );
            } else {
                let a = (TOTAL - left) as f32 * 0.1;
                self.cursor =
                    c + Vec2::new(0.3 + 0.15 * a.cos(), -0.3 + 0.15 * a.sin()) * vp.y * 0.33;
                self.stroke_pending = true;
            }
            self.stress = if left == 0 {
                None
            } else {
                Some((left - 1, frame_acc + dt * 1000.0, dab_acc + self.stroke_ms))
            };
        }
        if self.stroke_pending {
            self.stroke_pending = false;
            self.apply_stroke();
        }
        if let Some(left) = self.selftest_project {
            if left == 0 {
                self.selftest_project = None;
                self.selftest_project();
                self.quit = true;
            } else {
                self.selftest_project = Some(left - 1);
            }
        }
        if let Some(mut t) = self.selftest_mask.take() {
            if self.selftest_mask_frame(&mut t, dt * 1000.0) {
                self.quit = true;
            } else {
                self.selftest_mask = Some(t);
            }
        }
        if let Some(mut t) = self.selftest_transpose.take() {
            if self.selftest_transpose_frame(&mut t, dt * 1000.0) {
                self.quit = true;
            } else {
                self.selftest_transpose = Some(t);
            }
        }
        if let Some(left) = self.selftest {
            if left == 0 {
                self.selftest = None;
                self.selftest_subtools();
                self.quit = true;
            } else {
                self.selftest = Some(left - 1);
            }
        }
        if let Some((left, acc)) = self.measure {
            const TOTAL: u32 = 240;
            // Skip the first frames: they include the upload and the first normal pass.
            let acc = if left <= TOTAL { acc + dt * 1000.0 } else { acc };
            if left == 0 {
                log::info!(
                    "frames: avg {:.2} ms over {TOTAL} frames at level {} ({} vertices), last upload {:.2} ms, submit {:.2} ms",
                    acc / TOTAL as f32,
                    self.doc.active(),
                    fmt_count(self.mesh().vertex_count()),
                    self.upload_ms,
                    self.render_ms
                );
                self.measure = None;
                self.quit = true;
            } else {
                self.measure = Some((left - 1, acc));
            }
        }

        let vp = self.viewport();
        self.camera.aspect = vp.x / vp.y;
        self.camera.ortho = !self.workspace.perspective;
        let brush_pos = self.hover.unwrap_or(Vec3::ZERO);
        let show_brush = self.hover.is_some()
            && !ctx.is_pointer_over_egui()
            && self.workspace.mode == Mode::Draw;
        let radius = if self.stroke.is_some() {
            self.stroke_brush.radius
        } else {
            self.effective_brush().radius
        };
        let px = self.camera.pixel_size_at(brush_pos, vp.y);
        let eye = self.camera.eye();
        let light = (self.camera.right() * 0.6 + self.camera.up() * 0.8
            - self.camera.forward() * 0.9)
            .normalize();
        let preview = self.preview_map();
        let (preview_axis, preview_angle) = preview.rotation.to_axis_angle();
        let uniforms = Uniforms {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            inv_view_proj: self.camera.view_proj().inverse().to_cols_array_2d(),
            cam_pos: [eye.x, eye.y, eye.z, 1.0],
            light_dir: [light.x, light.y, light.z, 0.0],
            brush: [brush_pos.x, brush_pos.y, brush_pos.z, radius],
            params: [
                if show_brush { 1.0 } else { 0.0 },
                if self.workspace.wireframe { 1.0 } else { 0.0 },
                px,
                self.workspace.material as f32,
            ],
            viewport: [
                self.canvas_rect.min.x,
                self.canvas_rect.min.y,
                self.canvas_rect.width(),
                self.canvas_rect.height(),
            ],
            display: [
                if self.workspace.grid { 1.0 } else { 0.0 },
                if self.workspace.symmetry { 1.0 } else { 0.0 },
                if self.workspace.show_mask { 1.0 } else { 0.0 },
                if self.doc.mesh().mask.is_empty() { 0.0 } else { 1.0 },
            ],
            xf_center: preview.center.extend(if preview.is_identity() { 0.0 } else { 1.0 }).to_array(),
            xf_rotation: preview_axis.extend(preview_angle).to_array(),
            xf_move: preview.translate.extend(preview.scale).to_array(),
        };
        let parked: Vec<u64> = self.drawn_parked().into_iter().map(|(key, _)| key).collect();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let t = Instant::now();
        gfx.upload_dirty(self.doc.mesh_mut());
        self.upload_ms = t.elapsed().as_secs_f32() * 1000.0;
        let t = Instant::now();
        gfx.render(&uniforms, &parked, &jobs, full.textures_delta, full.pixels_per_point);
        self.render_ms = t.elapsed().as_secs_f32() * 1000.0;
        if self.stress.is_some() {
            self.stress_upload += self.upload_ms;
            self.stress_render += self.render_ms;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Klay")
                        .with_min_inner_size(LogicalSize::new(960.0, 640.0))
                        .with_inner_size(PhysicalSize::new(1600, 1000)),
                )
                .expect("create window"),
        );
        workspace::theme(&self.egui_ctx);
        let gfx = Gfx::new(window.clone(), self.vsync);
        let max_tex = gfx.device.limits().max_texture_dimension_2d as usize;
        let state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(max_tex),
        );
        self.egui_state = Some(state);
        self.gfx = Some(gfx);
        self.window = Some(window);
        self.show_active_level();
        self.status = format!(
            "Built {} vertices in {:.0} ms",
            fmt_count(self.mesh().vertex_count()),
            self.build_ms
        );
        if self.demo {
            for (name, ms, dabs, count) in self.scripted_strokes() {
                log::info!("demo stroke {name}: {dabs} dabs over {count} verts in {ms:.1} ms");
            }
        }
        if let Some(path) = self.open_on_start.take() {
            self.open_project(&path);
        }
        for _ in 0..std::mem::take(&mut self.initial_subtools) {
            self.append_sphere();
            log::info!("{}", self.status);
        }
        if self.subtools.len() > 1 {
            self.frame_camera();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let consumed = match self.egui_state.as_mut() {
            Some(state) => state.on_window_event(&window, &event).consumed,
            None => false,
        };
        let over_ui = self.egui_ctx.is_pointer_over_egui()
            || (self.canvas_rect.is_positive()
                && !self
                    .canvas_rect
                    .contains(egui::pos2(self.cursor.x, self.cursor.y)))
            || egui::DragAndDrop::has_any_payload(&self.egui_ctx);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Pressing Alt during a left drag arms the pan/zoom pair for the rest of it.
                if let Some(drag) = self.nav.as_mut() {
                    if drag.button == MouseButton::Left && self.modifiers.alt_key() {
                        drag.alt_used = true;
                    }
                    let drag = *drag;
                    if drag.mode(self.modifiers) == NavMode::Orbit && self.modifiers.shift_key() {
                        self.camera.snap_to_axis();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_moved(Vec2::new(position.x as f32, position.y as f32), over_ui);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => {
                        if pressed {
                            self.left_press(consumed, over_ui);
                        } else {
                            self.left_release();
                        }
                    }
                    MouseButton::Right | MouseButton::Middle => {
                        if pressed {
                            if self.workspace.quick_menu.is_some() {
                                // Closes the menu (handled by the workspace), nothing else.
                            } else if !over_ui && self.stroke.is_none() && self.nav.is_none() {
                                self.begin_nav(button);
                            }
                        } else {
                            let tap = button == MouseButton::Right
                                && self.nav.map(|n| n.button) == Some(button)
                                && (self.cursor - self.nav_press).length() < 4.0;
                            self.end_nav(button);
                            if tap {
                                // ZBrush: a right-click without a drag opens the quick menu.
                                self.workspace.open_quick_menu(self.cursor_pos(), false);
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !consumed && !over_ui {
                    let y = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    if self.modifiers.control_key() {
                        self.brush.radius = (self.brush.radius * (1.0 + y * 0.1)).clamp(0.01, 2.0);
                    } else {
                        // Zoom towards the point under the cursor so it stays put.
                        let ray = self.cursor_ray();
                        self.camera.zoom_towards(y, ray);
                        if self.nav.is_none() {
                            self.update_hover();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let typing = self.egui_ctx.egui_wants_keyboard_input();
                if let PhysicalKey::Code(KeyCode::Space) = event.physical_key {
                    // ZBrush quick menu: shown while Space is held.
                    match event.state {
                        ElementState::Pressed if !event.repeat && !typing => {
                            self.workspace.open_quick_menu(self.cursor_pos(), true);
                        }
                        ElementState::Released => {
                            if self.workspace.quick_menu.is_some_and(|(_, held)| held) {
                                self.workspace.quick_menu = None;
                            }
                        }
                        _ => {}
                    }
                } else if event.state == ElementState::Pressed && !typing {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        self.on_key(code);
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit {
            event_loop.exit();
            return;
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn arc(a0: f32, a1: f32, radius: f32, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / (n - 1) as f32;
            Vec2::new(a.cos(), -a.sin()) * radius
        })
        .collect()
}

fn line(a: Vec2, b: Vec2, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| a.lerp(b, i as f32 / (n - 1) as f32))
        .collect()
}

/// Headless benchmark: build, optional subdivision, scripted strokes, undo/redo, export.
fn bench(segments: u32, divide: u32, budget: usize) {
    let mut app = App::new(segments, false, budget);
    println!(
        "mesh: {} vertices, {} triangles, {} chunks, built in {:.0} ms",
        fmt_count(app.mesh().vertex_count()),
        fmt_count(app.mesh().triangle_count()),
        app.mesh().chunks.len(),
        app.build_ms
    );
    let adj_mb = (app.mesh().adj_verts.len() + app.mesh().vt_tris.len()) as f32 * 4.0 / 1e6;
    let geo_mb = app.mesh().vertex_count() as f32 * 16.0 / 1e6
        + app.mesh().indices.len() as f32 * 4.0 / 1e6;
    println!("memory: geometry {geo_mb:.0} MB, adjacency {adj_mb:.0} MB");
    if divide > 0 {
        bench_multires(&mut app, divide);
    }
    bench_subtool(&mut app);
    let t = Instant::now();
    let hit = app.mesh().raycast(app.camera.eye(), app.camera.forward());
    println!(
        "raycast: {:.3} ms, hit = {}",
        t.elapsed().as_secs_f32() * 1000.0,
        hit.is_some()
    );
    for (name, ms, dabs, count) in app.scripted_strokes() {
        println!(
            "stroke {name:8} {count:>9} verts: {dabs:3} dabs, {ms:7.1} ms total, {:6.2} ms/dab",
            ms / dabs.max(1) as f32
        );
    }
    // Micro-profile of the giant grab's building blocks.
    if let Some(hit) = app.mesh().raycast(app.camera.eye(), app.camera.forward()) {
        let t = Instant::now();
        let sel = app.mesh().select_sphere(hit.point, 1.2);
        println!(
            "select_sphere r=1.2: {} verts in {:.2} ms",
            fmt_count(sel.len()),
            t.elapsed().as_secs_f32() * 1000.0
        );
        let chunks = sel.chunks();
        let orig = {
            let mesh = app.mesh();
            sel.par_map(|_, v| mesh.positions[v as usize])
        };
        let w: Vec<f32> = sel.dist.iter().map(|&d| sculpt::falloff(d, 1.2)).collect();
        let n = 20;
        let t = Instant::now();
        for k in 0..n {
            let delta = Vec3::new(0.001 * k as f32, 0.0, 0.0);
            app.doc
                .mesh_mut()
                .scatter_positions_with(&sel, |i, _| orig[i] + delta * w[i]);
        }
        println!(
            "scatter_positions_with: {:.2} ms",
            t.elapsed().as_secs_f32() * 1000.0 / n as f32
        );
        let t = Instant::now();
        for _ in 0..n {
            app.doc.mesh_mut().finish_edit(&chunks);
        }
        println!(
            "finish_edit ({} chunks): {:.2} ms",
            chunks.len(),
            t.elapsed().as_secs_f32() * 1000.0 / n as f32
        );
    }
    let t = Instant::now();
    let normals = app.mesh().compute_all_normals();
    println!(
        "CPU normals for all {} vertices (export path only): {:.0} ms",
        fmt_count(normals.len()),
        t.elapsed().as_secs_f32() * 1000.0
    );
    let t = Instant::now();
    for _ in 0..3 {
        app.history.undo(&mut app.doc);
    }
    for _ in 0..3 {
        app.history.redo(&mut app.doc);
    }
    println!(
        "3 undo + 3 redo: {:.1} ms, history {:.1} MB",
        t.elapsed().as_secs_f32() * 1000.0,
        app.history.bytes() as f32 / 1e6
    );
}

/// Multires part of the benchmark: divide `times`, then a Draw stroke at the base level
/// and the cost of stepping back up through the levels it made stale.
fn bench_multires(app: &mut App, times: u32) {
    for _ in 0..times {
        if !app.doc.can_divide() {
            println!(
                "divide: next level ({} vertices) is over the budget of {}",
                fmt_count(app.doc.next_vertex_count()),
                fmt_count(app.doc.budget)
            );
            break;
        }
        let t = Instant::now();
        app.doc.divide();
        app.history.push(UndoStep::Divide(None));
        println!(
            "divide to level {}: {} vertices, {} chunks in {:.0} ms",
            app.doc.active(),
            fmt_count(app.mesh().vertex_count()),
            app.mesh().chunks.len(),
            t.elapsed().as_secs_f32() * 1000.0
        );
    }
    let top = app.doc.top();
    if top == 0 {
        return;
    }
    println!(
        "levels: {} MB for {} levels",
        app.doc.bytes() / 1_000_000,
        app.doc.count()
    );
    // Draw at the base with every level above it present.
    app.doc.set_active(0);
    let vp = app.viewport();
    let c = app.canvas_origin() + vp * 0.5;
    let r = vp.y * 0.33;
    app.brush = Brush {
        kind: BrushKind::Draw,
        radius: 0.18,
        strength: 0.8,
    };
    let t = Instant::now();
    for (i, p) in arc(0.2, 1.4, 0.55, 60).iter().enumerate() {
        app.cursor = c + *p * r;
        if i == 0 {
            app.begin_stroke();
        } else {
            app.apply_stroke();
        }
    }
    let (dabs, count) = app.stroke.as_ref().map(|s| (s.dabs, s.last_count)).unwrap_or((0, 0));
    let ms = t.elapsed().as_secs_f32() * 1000.0;
    app.end_stroke();
    println!(
        "draw at level 0 with {top} levels above: {dabs} dabs over {count} verts, {ms:.1} ms, {:.2} ms/dab",
        ms / dabs.max(1) as f32
    );
    for level in 1..=top {
        let t = Instant::now();
        let touched = app.doc.set_active(level);
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        println!(
            "step up to level {level}: {touched} of {} chunks re-smoothed in {ms:.1} ms",
            app.mesh().chunks.len()
        );
    }
    let t = Instant::now();
    app.history.undo(&mut app.doc);
    println!("undo the base stroke from level {top}: {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
    let t = Instant::now();
    app.history.redo(&mut app.doc);
    println!("redo it: {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
    app.brush = Brush::default();
}

/// Parking and re-activating the active subtool: memory before and after, and the times.
fn bench_subtool(app: &mut App) {
    let levels = app.doc.vertex_counts();
    let stack = app.doc.bytes();
    let t = Instant::now();
    let stand_in = app.doc.stand_in(STAND_IN_VERTICES);
    let stand_in_ms = t.elapsed().as_secs_f32() * 1000.0;
    let stand_in_gpu = stand_in.positions.len() * 16 + stand_in.indices.len() * 4;
    let t = Instant::now();
    let doc = std::mem::replace(&mut app.doc, App::placeholder());
    let packed = doc.pack();
    let pack_ms = t.elapsed().as_secs_f32() * 1000.0;
    let packed_bytes = packed.bytes();
    let t = Instant::now();
    app.doc = packed.unpack();
    let unpack_ms = t.elapsed().as_secs_f32() * 1000.0;
    println!(
        "subtool: levels {:?}, active stack {} MB in RAM",
        levels.iter().map(|&n| fmt_count(n)).collect::<Vec<_>>(),
        stack / 1_000_000
    );
    println!(
        "park: stand-in of {} vertices (level {}) in {stand_in_ms:.0} ms, {} MB on GPU; packed in {pack_ms:.1} ms to {} MB in RAM ({:.1}% of the stack)",
        fmt_count(stand_in.positions.len()),
        stand_in.level,
        stand_in_gpu / 1_000_000,
        packed_bytes / 1_000_000,
        packed_bytes as f64 * 100.0 / stack as f64
    );
    println!("activate: stack rebuilt in {unpack_ms:.0} ms");
}

/// Default vertex budget for subdivision: three divides of the default sphere fit (61M
/// vertices: about 8 GB of RAM and 3.2 GB of GPU memory for that level).
const DEFAULT_BUDGET: usize = 64_000_000;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| -> Option<&String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1))
    };
    let divide: u32 = flag("--divide").and_then(|s| s.parse().ok()).unwrap_or(0);
    let budget: usize = flag("--budget").and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_BUDGET);
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        let segments = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(400);
        bench(segments, divide, budget);
        return;
    }
    let demo = args.iter().any(|a| a == "--demo");
    let stress = args.iter().any(|a| a == "--stress");
    let frames = args.iter().any(|a| a == "--frames");
    let vsync = !args.iter().any(|a| a == "--novsync");
    let segments = flag("--segments").and_then(|s| s.parse().ok()).unwrap_or(400);
    let subtools: usize = flag("--subtools").and_then(|s| s.parse().ok()).unwrap_or(1);
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(segments, demo, budget);
    app.vsync = vsync;
    for _ in 0..divide {
        let t = Instant::now();
        if !app.doc.divide() {
            log::warn!("--divide: vertex budget reached at level {}", app.doc.top());
            break;
        }
        app.history.push(UndoStep::Divide(None));
        log::info!(
            "divided to level {}: {} vertices in {:.0} ms",
            app.doc.active(),
            fmt_count(app.mesh().vertex_count()),
            t.elapsed().as_secs_f32() * 1000.0
        );
    }
    app.initial_subtools = subtools.saturating_sub(1);
    if args.iter().any(|a| a == "--selftest-subtools") {
        app.selftest = Some(20);
    }
    if args.iter().any(|a| a == "--selftest-mask") {
        app.selftest_mask = Some(MaskTest::default());
    }
    if args.iter().any(|a| a == "--selftest-transpose") {
        app.selftest_transpose = Some(TransposeTest::default());
    }
    if args.iter().any(|a| a == "--selftest-project") {
        app.selftest_project = Some(20);
    }
    app.open_on_start = flag("--open").cloned();
    if stress {
        app.stress = Some((240, 0.0, 0.0));
    }
    if frames {
        app.measure = Some((240 + 30, 0.0));
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
    }
}

#[cfg(test)]
mod nav_tests {
    use super::*;

    #[test]
    fn alt_click_release_alt_zooms() {
        // ZBrush: Alt + click starts a pan, releasing Alt turns the same drag into a zoom,
        // pressing Alt again goes back to panning.
        let drag = NavDrag {
            button: MouseButton::Left,
            alt_used: true,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Zoom);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
    }

    #[test]
    fn plain_canvas_drag_orbits_until_alt_is_used() {
        let mut drag = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Orbit);
        drag.alt_used = true; // Alt pressed mid-drag
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Zoom);
    }

    #[test]
    fn shift_click_release_shift_rolls() {
        // ZBrush: Shift held at the click snaps while held; releasing it turns the drag
        // into a roll. Alt still wins for the pan/zoom pair.
        let drag = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: true,
        };
        assert_eq!(drag.mode(ModifiersState::SHIFT), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Roll);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        // Shift pressed only mid-drag never rolls.
        let plain = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(plain.mode(ModifiersState::empty()), NavMode::Orbit);
    }

    #[test]
    fn right_button_modifiers() {
        let drag = NavDrag {
            button: MouseButton::Right,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        // Shift is the rotation snap, not a pan.
        assert_eq!(drag.mode(ModifiersState::SHIFT), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::CONTROL), NavMode::Zoom);
    }
}

#[cfg(test)]
mod subtool_tests {
    use super::*;

    fn worst(a: &[Vec3], b: &[Vec3]) -> f32 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(p, q)| (*p - *q).length()).fold(0.0, f32::max)
    }

    #[test]
    fn subtools_park_light_and_come_back_with_their_history() {
        let mut app = App::new(16, false, usize::MAX);
        app.segments = 16;
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        app.deform(Deform::OffsetX, -50.0);
        let shown = app.doc.mesh().positions.clone();
        let stack = app.doc.bytes();

        app.append_sphere();
        assert_eq!(app.subtools.len(), 2);
        assert_eq!(app.active_tool, 1);
        assert_eq!(app.doc.top(), 0, "the appended sphere starts undivided");
        let parked = app.subtools[0].parked.as_ref().expect("first subtool is parked");
        assert!(parked.packed.bytes() * 5 < stack, "parked {} vs stack {stack}", parked.packed.bytes());
        // The new sphere sits to the right of the first one, not inside it.
        assert!(app.doc.bounds().min.x > app.subtools[0].bounds.max.x);

        app.select_subtool(0);
        assert_eq!(app.active_tool, 0);
        assert!(app.subtools[1].parked.is_some());
        assert_eq!(app.doc.active(), 1, "the shown level comes back");
        assert!(worst(&app.doc.mesh().positions, &shown) < 1e-5);

        // The undo list came back with it: undo the offset, then the divide.
        assert!(app.history.can_undo());
        app.history.undo(&mut app.doc);
        let centre = (app.doc.bounds().min + app.doc.bounds().max) * 0.5;
        assert!(centre.length() < 1e-3, "offset undone, centre at {centre:?}");
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.top(), 0);

        app.duplicate_subtool();
        assert_eq!(app.subtools.len(), 3);
        assert!(app.subtools[1].parked.is_some());
        assert_eq!(app.subtools[1].name, "Sphere 1 copy");
        app.delete_subtool();
        assert_eq!(app.subtools.len(), 2);
        assert_eq!(app.subtools[app.active_tool].name, "Sphere 1 copy");
        assert!(app.subtools.iter().filter(|t| t.parked.is_none()).count() == 1);

        app.move_subtool(1);
        assert_eq!(app.active_tool, 1);
        assert_eq!(app.subtools[1].name, "Sphere 1 copy");
        app.toggle_subtool(0);
        assert!(!app.subtools[0].visible);
        assert!(app.drawn_parked().is_empty());
        app.toggle_subtool(1);
        assert!(app.subtools[1].visible, "the active subtool cannot be hidden");
    }

    #[test]
    fn projects_save_open_and_export_every_subtool() {
        let dir = std::env::temp_dir();
        let tag = std::process::id();
        let project = dir.join(format!("klay-test-{tag}.klay"));
        let obj = dir.join(format!("klay-test-{tag}.obj"));

        let mut app = App::new(12, false, usize::MAX);
        app.segments = 8;
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        app.deform(Deform::Size, 40.0);
        app.append_sphere();
        app.toggle_subtool(0);
        app.toggle_subtool(0);
        app.subtools[1].name = "Eye".to_string();
        let before = app.project_bytes();
        app.project_path = project.to_string_lossy().to_string();
        app.save_project();
        assert!(app.status.starts_with("Saved"), "{}", app.status);

        let mut other = App::new(4, false, 1000);
        let path = app.project_path.clone();
        other.open_project(&path);
        assert!(other.status.starts_with("Opened"), "{}", other.status);
        assert_eq!(other.subtools.len(), 2);
        assert_eq!(other.subtools[1].name, "Eye");
        assert_eq!(other.active_tool, 1);
        assert_eq!(other.project_bytes(), before, "the model came back byte for byte");

        // A damaged file is refused and the open model stays as it was.
        let mut bytes = std::fs::read(&project).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&project, &bytes).unwrap();
        other.open_project(&path);
        assert!(other.status.starts_with("Could not open"), "{}", other.status);
        assert_eq!(other.project_bytes(), before);

        // Whole-model OBJ: one object per visible subtool, indices continue across them.
        other.export_path = obj.to_string_lossy().to_string();
        other.export_obj_all();
        let text = std::fs::read_to_string(&obj).unwrap();
        let objects: Vec<&str> = text.lines().filter(|l| l.starts_with("o ")).collect();
        assert_eq!(objects, ["o Sphere 1", "o Eye"]);
        let verts = text.lines().filter(|l| l.starts_with("v ")).count();
        let first = other.subtools[0].parked.as_ref().unwrap().packed.counts()[1];
        assert_eq!(verts, first + other.doc.mesh().vertex_count());
        let max_index = text
            .lines()
            .filter(|l| l.starts_with("f "))
            .flat_map(|l| l[2..].split(' ').map(|c| c.split("//").next().unwrap().parse::<usize>().unwrap()))
            .max()
            .unwrap();
        assert_eq!(max_index, verts);
        let _ = std::fs::remove_file(&project);
        let _ = std::fs::remove_file(&obj);
    }
}

#[cfg(test)]
mod transpose_tests {
    use super::*;

    fn worst(a: &[Vec3], b: &[Vec3]) -> f32 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(p, q)| (*p - *q).length()).fold(0.0, f32::max)
    }

    /// Press, drag in `steps` moves to `to`, release, through the window-event handlers.
    fn drag(app: &mut App, from: Vec2, to: Vec2) {
        app.cursor_moved(from, false);
        app.left_press(false, false);
        for k in 1..=8 {
            app.cursor_moved(from + (to - from) * (k as f32 / 8.0), false);
        }
        app.left_release();
    }

    fn centre(app: &App) -> Vec3 {
        let b = app.doc.bounds();
        (b.min + b.max) * 0.5
    }

    #[test]
    fn transpose_moves_scales_rotates_and_undoes() {
        let mut app = App::new(24, false, usize::MAX);
        app.camera.aspect = 1.6;
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        // A stroke at level 1, so the stack carries offsets that have to turn with it.
        let c = app.project(Vec3::ZERO);
        let plain = app.doc.mesh().positions.clone();
        drag(&mut app, c, c + Vec2::new(40.0, 20.0));
        assert!(worst(&app.doc.mesh().positions, &plain) > 1e-3, "the stroke sculpted");
        let original = app.doc.mesh().positions.clone();

        // W: a drag on the model draws the line and leaves the model alone.
        app.on_key(KeyCode::KeyW);
        drag(&mut app, c, c + Vec2::new(200.0, 0.0));
        let line = app.action_line.expect("a line was drawn");
        assert!((app.project(line.start) - c).length() < 0.5);
        assert!((app.project(line.end) - (c + Vec2::new(200.0, 0.0))).length() < 0.5);
        assert_eq!(app.doc.mesh().positions, original);

        // Middle circle: during the drag only the preview map changes; release applies it.
        let mid = app.project(line.mid());
        let target = mid + Vec2::new(150.0, -80.0);
        app.cursor_moved(mid, false);
        app.left_press(false, false);
        app.cursor_moved(target, false);
        let map = app.preview_map();
        assert!(map.is_translation() && !map.is_identity());
        assert_eq!(app.doc.mesh().positions, original, "nothing moves on the CPU mid-drag");
        let c0 = centre(&app);
        app.left_release();
        assert!(app.preview_map().is_identity());
        assert!((centre(&app) - (c0 + map.translate)).length() < 1e-4);
        assert!((app.project(app.action_line.unwrap().mid()) - target).length() < 0.5);
        let moved = app.doc.mesh().positions.clone();

        // E: end circle out to twice its distance from the start doubles the size.
        app.on_key(KeyCode::KeyE);
        let line = app.action_line.unwrap();
        let (s, e) = (app.project(line.start), app.project(line.end));
        let size0 = app.doc.bounds().max - app.doc.bounds().min;
        drag(&mut app, e, s + (e - s) * 2.0);
        let size1 = app.doc.bounds().max - app.doc.bounds().min;
        assert!((size1 / size0 - Vec3::splat(2.0)).abs().max_element() < 1e-3, "{size1} vs {size0}");
        // The pivot (line start) stays put.
        assert!((app.action_line.unwrap().start - line.start).length() < 1e-5);

        // R: end circle a quarter turn about the start.
        app.on_key(KeyCode::KeyR);
        let line = app.action_line.unwrap();
        let (s, e) = (app.project(line.start), app.project(line.end));
        let before = app.doc.mesh().positions.clone();
        app.cursor_moved(e, false);
        app.left_press(false, false);
        for k in 1..=8 {
            let a = std::f32::consts::FRAC_PI_2 * k as f32 / 8.0;
            app.cursor_moved(s + Vec2::from_angle(a).rotate(e - s), false);
        }
        let map = app.preview_map();
        app.left_release();
        assert!((map.rotation.to_axis_angle().1.to_degrees() - 90.0).abs() < 0.5);
        let pivot = line.start;
        for (p, q) in before.iter().zip(&app.doc.mesh().positions).step_by(97) {
            assert!(((*p - pivot).length() - (*q - pivot).length()).abs() < 1e-4, "rigid about the pivot");
        }
        let rotated = app.doc.mesh().positions.clone();

        // Undo the three transforms, redo them.
        for _ in 0..3 {
            app.history.undo(&mut app.doc);
        }
        assert!(worst(&app.doc.mesh().positions, &original) < 1e-4);
        app.history.redo(&mut app.doc);
        assert!(worst(&app.doc.mesh().positions, &moved) < 1e-4);
        app.history.redo(&mut app.doc);
        app.history.redo(&mut app.doc);
        assert!(worst(&app.doc.mesh().positions, &rotated) < 1e-4);
        // Lower levels followed: level 0 under the same maps subdivides to the shown level.
        app.set_level(0);
        app.set_level(1);
        assert!(worst(&app.doc.mesh().positions, &rotated) < 1e-4);
    }

    #[test]
    fn transpose_mode_keeps_navigation_and_draw_keeps_sculpting() {
        let mut app = App::new(16, false, usize::MAX);
        app.camera.aspect = 1.6;
        let c = app.project(Vec3::ZERO);
        app.on_key(KeyCode::KeyW);
        // Off the model (and away from any line) a left drag still rotates the view.
        app.cursor_moved(Vec2::new(20.0, 20.0), false);
        app.left_press(false, false);
        assert!(app.nav.is_some() && app.transpose.is_none());
        app.left_release();
        // A click without a drag leaves no line.
        drag(&mut app, c, c);
        assert!(app.action_line.is_none());
        // Ctrl stays reserved for masking: no line, no stroke.
        app.modifiers = ModifiersState::CONTROL;
        drag(&mut app, c, c + Vec2::new(100.0, 0.0));
        assert!(app.action_line.is_none() && !app.history.can_undo());
        app.modifiers = ModifiersState::empty();
        // The line's ring moves an end without touching the model.
        drag(&mut app, c, c + Vec2::new(100.0, 0.0));
        let line = app.action_line.unwrap();
        let ring = app.project(line.end) + Vec2::new(transform::INNER + 3.0, 0.0);
        drag(&mut app, ring, ring + Vec2::new(0.0, 50.0));
        let moved = app.action_line.unwrap();
        assert_eq!(moved.start, line.start);
        assert!((app.project(moved.end) - app.project(line.end) - Vec2::new(0.0, 50.0)).length() < 0.5);
        assert!(!app.history.can_undo(), "moving the line is not an edit");
        // Back in Draw mode the same drag sculpts.
        app.on_key(KeyCode::KeyQ);
        drag(&mut app, c, c + Vec2::new(30.0, 0.0));
        assert!(app.history.can_undo());
    }

    #[test]
    fn the_canvas_border_navigates_even_over_the_model() {
        let mut app = App::new(16, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        // Zoomed in so far that the model fills the whole canvas.
        app.camera.distance = 1.05;
        let corner = Vec2::new(6.0, 6.0);
        let (o, d) = app.camera.ray(corner.x, corner.y, 1600.0, 1000.0);
        assert!(app.mesh().raycast(o, d).is_some(), "the model is under the border");
        for key in [KeyCode::KeyQ, KeyCode::KeyW] {
            app.on_key(key);
            app.cursor_moved(corner, false);
            assert!(app.hover.is_none(), "no brush ring in the border");
            app.left_press(false, false);
            assert!(app.nav.is_some() && app.stroke.is_none() && app.transpose.is_none());
            // Alt + click, release Alt, drag: zoom back out.
            app.modifiers = ModifiersState::ALT;
            app.left_release();
            app.left_press(false, false);
            app.modifiers = ModifiersState::empty();
            let before = app.camera.distance;
            app.cursor_moved(corner - Vec2::new(0.0, 5.0), false);
            assert!(app.camera.distance > before, "zoomed out");
            app.left_release();
            app.camera.distance = 1.05;
        }
        // Inside the frame the same press sculpts.
        app.on_key(KeyCode::KeyQ);
        drag(&mut app, Vec2::new(40.0, 40.0), Vec2::new(60.0, 40.0));
        assert!(app.history.can_undo());
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new(30, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app
    }

    fn drag(app: &mut App, from: Vec2, to: Vec2, m: ModifiersState) {
        app.modifiers = m;
        app.cursor_moved(from, false);
        app.left_press(false, false);
        for k in 1..=10 {
            app.cursor_moved(from + (to - from) * (k as f32 / 10.0), false);
        }
        app.left_release();
        app.modifiers = ModifiersState::empty();
    }

    fn masked(app: &App) -> usize {
        app.doc.mesh().mask.iter().filter(|&&m| m > 127).count()
    }

    #[test]
    fn ctrl_paints_erases_inverts_clears_and_blurs() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        let background = Vec2::new(200.0, 200.0);
        let ctrl = ModifiersState::CONTROL;
        drag(&mut app, c, c + Vec2::new(80.0, 0.0), ctrl);
        let painted = masked(&app);
        assert!(painted > 0 && painted < app.doc.mesh().vertex_count() / 2, "{painted}");
        assert!(app.history.can_undo());
        // Ctrl + click on the background inverts, again inverts back.
        drag(&mut app, background, background, ctrl);
        assert_eq!(masked(&app), app.doc.mesh().vertex_count() - painted);
        drag(&mut app, background, background, ctrl);
        assert_eq!(masked(&app), painted);
        // Ctrl + click on the model blurs: the soft band widens.
        let soft = |app: &App| app.doc.mesh().mask.iter().filter(|&&m| m > 10 && m < 245).count();
        let s0 = soft(&app);
        drag(&mut app, c, c, ctrl);
        assert!(soft(&app) > s0);
        // Ctrl + Alt + drag erases.
        drag(&mut app, c, c + Vec2::new(80.0, 0.0), ctrl | ModifiersState::ALT);
        assert!(masked(&app) < painted / 2);
        // Ctrl + drag on the background clears.
        drag(&mut app, background, background + Vec2::new(60.0, 40.0), ctrl);
        assert!(app.doc.mesh().mask.is_empty());
        // Undo walks back through the five steps after the first stroke.
        for _ in 0..5 {
            app.history.undo(&mut app.doc);
        }
        assert_eq!(masked(&app), painted);
        app.history.undo(&mut app.doc);
        assert_eq!(masked(&app), 0);
        // Shortcuts: Ctrl + A masks all, Ctrl + Shift + A clears.
        assert!(app.mask_op(MaskOp::All));
        assert_eq!(masked(&app), app.doc.mesh().vertex_count());
        assert!(app.mask_op(MaskOp::Clear));
        assert!(!app.mask_op(MaskOp::Clear), "clearing nothing is not an undo step");
    }

    #[test]
    fn brushes_and_transpose_respect_the_mask() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        app.mask_op(MaskOp::All);
        let before = app.doc.mesh().positions.clone();
        // A Draw stroke on a fully masked model moves nothing.
        drag(&mut app, c, c + Vec2::new(60.0, 0.0), ModifiersState::empty());
        assert_eq!(app.doc.mesh().positions, before);
        // Free the front, then move with Transpose: only the front moves.
        app.brush.strength = 1.0;
        drag(&mut app, c, c + Vec2::new(1.0, 5.0), ModifiersState::CONTROL | ModifiersState::ALT);
        let front = app.doc.mesh().positions.iter().enumerate()
            .max_by(|a, b| a.1.dot(app.camera.eye()).total_cmp(&b.1.dot(app.camera.eye()))).unwrap().0;
        let back = app.doc.mesh().positions.iter().enumerate()
            .min_by(|a, b| a.1.dot(app.camera.eye()).total_cmp(&b.1.dot(app.camera.eye()))).unwrap().0;
        assert!(app.doc.mesh().mask[front] < 64 && app.doc.mesh().mask[back] == 255);
        app.on_key(KeyCode::KeyW);
        drag(&mut app, c, c + Vec2::new(200.0, 0.0), ModifiersState::empty());
        let mid = app.project(app.action_line.unwrap().mid());
        drag(&mut app, mid, mid + Vec2::new(0.0, -120.0), ModifiersState::empty());
        let after = app.doc.mesh().positions.clone();
        assert!((after[front] - before[front]).length() > 0.1, "the free front moved");
        assert!((after[back] - before[back]).length() < 1e-5, "the masked back stayed");
        // One undo step brings it back.
        app.history.undo(&mut app.doc);
        let worst = app.doc.mesh().positions.iter().zip(&before).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn projects_keep_the_mask() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        drag(&mut app, c, c + Vec2::new(80.0, 0.0), ModifiersState::CONTROL);
        let mask = app.doc.mesh().mask.clone();
        let path = std::env::temp_dir().join(format!("klay-mask-{}.klay", std::process::id()));
        app.project_path = path.to_string_lossy().to_string();
        app.save_project();
        let mut other = App::new(4, false, 1000);
        other.open_project(&app.project_path.clone());
        assert!(other.status.starts_with("Opened"), "{}", other.status);
        assert_eq!(other.doc.mesh().mask, mask);
        let _ = std::fs::remove_file(&path);
    }
}
