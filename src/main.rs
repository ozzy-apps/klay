mod camera;
mod mesh;
mod multires;
mod polygroup;
mod primitive;
mod project;
mod render;
mod sculpt;
mod subtool;
mod wedge;
mod workspace;

use camera::Camera;
use glam::{Vec2, Vec3};
use mesh::{SculptMesh, cube_sphere};
use primitive::Primitive;
use multires::Multires;
use rayon::prelude::*;
use render::{Gfx, Uniforms};
use mesh::Aabb;
use sculpt::{Brush, BrushKind, History, Stroke, UndoStep};
use std::path::Path;
use subtool::{Deform, Parked, STAND_IN_VERTICES, SubTool};
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

/// A Ctrl + Shift press in progress (ZBrush: SelectRect). A click acts on the polygroup
/// under it (or shows everything on the background); a drag draws a rectangle.
#[derive(Clone, Copy, Debug)]
struct SelectDrag {
    start: Vec2,
    /// Triangle of the active subtool under the press; `None` on the background.
    hit: Option<u32>,
}

/// Presses this close to the canvas edge count as background even over the model, so the
/// background gestures stay reachable when the model fills the view (logical pixels).
const CANVAS_BORDER: f32 = 12.0;

/// A press that moves less than this (physical pixels) before release is a click.
const CLICK_SLOP: f32 = 4.0;

/// Document operations queued by the interface or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocOp {
    New,
    Open,
    Save,
}

/// Polygroup and visibility operations queued by the interface or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisOp {
    GroupVisible,
    ShowAll,
    Invert,
}

/// Geometry operations queued by the interface or a gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GeoOp {
    /// Turn Wedge on (and remesh) or off.
    WedgeToggle,
    /// Remesh with Wedge (the button while on, or Ctrl + drag on the background).
    Wedge,
    DelHigher,
    DelLower,
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
    /// `--selftest-groups`: `(phase, frames left)` of the polygroup / visibility self-test,
    /// which runs its steps a few frames apart so the GPU state can be checked in between.
    selftest_groups: Option<(u8, u32)>,
    /// What the polygroup self-test expects to find, and whether every check passed.
    selftest_groups_state: (usize, bool),
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
    /// Ctrl + Shift selection in progress.
    select: Option<SelectDrag>,
    /// A Ctrl press on the background in progress (ZBrush: with DynaMesh on, a Ctrl drag
    /// there remeshes), and where it started.
    ctrl_background: Option<Vec2>,
    /// Wedge settings (ZBrush: Tool > Geometry > DynaMesh).
    wedge: wedge::Settings,
    /// For the Wedge section: whether the shown mesh is closed and its area, kept for the
    /// stack generation and level they were measured on (both cost a pass over the mesh).
    wedge_info: Option<(u64, usize, bool, f32)>,
    hover: Option<Vec3>,

    // UI state.
    workspace: workspace::Workspace,
    canvas_rect: egui::Rect,
    /// Primitive and resolution used by New, Append and Rebuild (ZBrush: Tool > Initialize).
    primitive: primitive::Settings,
    export_path: String,
    status: String,
    pending_rebuild: bool,
    pending_export: bool,
    pending_undo: bool,
    pending_redo: bool,
    pending_divide: bool,
    pending_level: Option<usize>,
    /// Polygroup / visibility operation from the interface or a key, run outside the egui pass.
    pending_vis: Option<VisOp>,
    /// Geometry operation (Wedge, Del Higher / Lower), run outside the egui pass.
    pending_geo: Option<GeoOp>,
    /// `--selftest-wedge`: `(phase, frames left)`, like the polygroup self-test.
    selftest_wedge: Option<(u8, u32)>,
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
    /// Start with a sphere of `segments` (tests).
    #[cfg(test)]
    fn new(segments: u32, demo: bool, budget: usize) -> Self {
        let mut settings = primitive::Settings::default();
        settings.set_current(segments);
        Self::with_primitive(settings, demo, budget)
    }

    /// Start with one subtool made from `settings`.
    fn with_primitive(settings: primitive::Settings, demo: bool, budget: usize) -> Self {
        let t = Instant::now();
        let (p, i) = settings.build(1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let build_ms = t.elapsed().as_secs_f32() * 1000.0;
        Self {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            subtools: vec![SubTool {
                id: 1,
                name: format!("{} 1", settings.kind.short()),
                visible: true,
                bounds: mesh.bounds(),
                wedge: false,
                parked: None,
            }],
            active_tool: 0,
            next_tool_id: 2,
            initial_subtools: 0,
            selftest: None,
            selftest_project: None,
            selftest_groups: None,
            selftest_groups_state: (0, true),
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
            select: None,
            ctrl_background: None,
            wedge: wedge::Settings::default(),
            wedge_info: None,
            hover: None,
            workspace: workspace::Workspace::new(),
            canvas_rect: egui::Rect::NOTHING,
            primitive: settings,
            export_path: "klay_export.obj".to_string(),
            status: String::new(),
            pending_rebuild: false,
            pending_export: false,
            pending_undo: false,
            pending_redo: false,
            pending_divide: false,
            pending_level: None,
            pending_vis: None,
            pending_geo: None,
            selftest_wedge: None,
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
        if i == self.active_tool || i >= self.subtools.len() || self.stroke.is_some() {
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

    /// Add a subtool made from the current primitive next to the visible model and make it
    /// active (ZBrush: Append). It is half the size of a new project's first subtool and
    /// sits to the right of the model's bounds with a small gap.
    fn append_primitive(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        let bounds = self.model_bounds();
        let budget = self.doc.budget;
        self.park_active();
        let t = Instant::now();
        let size = 0.5;
        let (p, i) = self.primitive.build(size);
        let half_x = p.iter().fold(0.0f32, |m, q| m.max(q.x.abs()));
        let center = Vec3::new(
            bounds.max.x + half_x + size * 0.25,
            (bounds.min.y + bounds.max.y) * 0.5,
            (bounds.min.z + bounds.max.z) * 0.5,
        );
        let p = p.into_iter().map(|q| q + center).collect();
        self.doc = Multires::new(SculptMesh::from_raw(p, i), budget);
        let id = self.new_tool_id();
        let name = format!("{} {id}", self.primitive.kind.short());
        self.subtools.push(SubTool {
            id,
            name: name.clone(),
            visible: true,
            bounds: self.doc.bounds(),
            wedge: false,
            parked: None,
        });
        self.active_tool = self.subtools.len() - 1;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
        self.status = format!(
            "Appended {name}: {} vertices in {:.0} ms",
            fmt_count(self.mesh().vertex_count()),
            t.elapsed().as_secs_f32() * 1000.0
        );
    }

    /// Insert a parked copy of the active subtool below it (ZBrush: Duplicate).
    fn duplicate_subtool(&mut self) {
        if self.stroke.is_some() {
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
        let wedge = self.subtools[self.active_tool].wedge;
        self.subtools.insert(
            self.active_tool + 1,
            SubTool {
                id,
                name: name.clone(),
                visible: true,
                bounds: stand_in.bounds,
                wedge,
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
        if self.stroke.is_some() {
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
        if self.stroke.is_some() || value.abs() < 0.5 {
            return;
        }
        let (center, scale, translate) = subtool::deform_map(kind, value, self.doc.bounds());
        self.doc.transform(center, scale, translate);
        // History entries hold what undoes them: the inverse map.
        self.history.push(UndoStep::Transform {
            center: center + translate,
            scale: 1.0 / scale,
            translate: -translate,
        });
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.update_hover();
        self.workspace
            .notify(format!("{} {:+.0} applied", kind.label(), value));
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
        if self.stroke.is_some() {
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
        if self.stroke.is_some() {
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
                    wedge: false,
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
                    wedge: false,
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

    /// Start over with one subtool made from the current primitive.
    fn new_project(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        if let Some(gfx) = self.gfx.as_mut() {
            for t in &self.subtools {
                gfx.drop_stand_in(t.id);
            }
            gfx.clear_levels();
        }
        let budget = self.doc.budget;
        let (p, i) = self.primitive.build(1.0);
        self.doc = Multires::new(SculptMesh::from_raw(p, i), budget);
        self.history = History::new(32);
        let id = self.new_tool_id();
        self.subtools = vec![SubTool {
            id,
            name: format!("{} {id}", self.primitive.kind.short()),
            visible: true,
            bounds: self.doc.bounds(),
            wedge: false,
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

    // ------------------------------------------------------------ polygroups and visibility

    /// Replace the hidden triangles of the shown level (carried to every level), undoably.
    /// Hiding everything is refused, as in ZBrush.
    fn apply_hidden(&mut self, hidden: Vec<u8>, what: &str) {
        let mesh = self.doc.mesh();
        let n = mesh.triangle_count();
        if hidden.len() == n && !hidden.par_iter().any(|&h| h == 0) {
            self.workspace.notify("Every polygon would be hidden · nothing changed");
            return;
        }
        if hidden == mesh.hidden || (hidden.is_empty() && mesh.hidden.is_empty()) {
            return;
        }
        let t = Instant::now();
        let before = self.doc.visibility();
        let level = self.doc.active();
        self.doc.set_hidden(level, hidden);
        self.history.push(UndoStep::Visibility(before));
        self.update_hover();
        let mesh = self.doc.mesh();
        self.status = format!(
            "{what}: {} of {} triangles hidden ({:.0} ms over {} levels)",
            fmt_count(mesh.hidden_count()),
            fmt_count(mesh.triangle_count()),
            t.elapsed().as_secs_f32() * 1000.0,
            self.doc.count()
        );
        log::info!("{}", self.status);
    }

    /// Show every polygon (ZBrush: Ctrl + Shift + click on the background).
    fn show_all(&mut self) {
        self.apply_hidden(Vec::new(), "Show all");
    }

    /// Hidden becomes visible and visible hidden (ZBrush: Ctrl + Shift + drag on the
    /// background).
    fn invert_visibility(&mut self) {
        let mesh = self.doc.mesh();
        if mesh.hidden.is_empty() {
            self.workspace.notify("Nothing is hidden · nothing to invert");
            return;
        }
        let inverted = mesh.hidden.par_iter().map(|&h| (h == 0) as u8).collect();
        self.apply_hidden(inverted, "Invert visibility");
    }

    /// Ctrl + Shift + click on the model: show only the polygroup of triangle `tri`, or with
    /// `hide` (Alt) hide it.
    fn isolate_group(&mut self, tri: u32, hide: bool) {
        let mesh = self.doc.mesh();
        let g = mesh.group(tri);
        let hidden: Vec<u8> = (0..mesh.triangle_count() as u32)
            .into_par_iter()
            .map(|t| (!mesh.visible(t) || (mesh.group(t) == g) == hide) as u8)
            .collect();
        self.apply_hidden(hidden, if hide { "Hide group" } else { "Isolate group" });
    }

    /// Ctrl + Shift + drag between `a` and `b` (pixels): keep what lies inside visible, or
    /// with `hide` (Alt) hide it. A rectangle around no visible polygon inverts visibility.
    fn select_rect(&mut self, a: Vec2, b: Vec2, hide: bool) {
        let t = Instant::now();
        let origin = self.canvas_origin();
        let (lo, hi) = (a.min(b) - origin, a.max(b) - origin);
        let mesh = self.doc.mesh();
        let inside = polygroup::tris_in_rect(mesh, self.camera.view_proj(), self.viewport(), lo, hi);
        let any = inside.par_iter().enumerate().any(|(t, &i)| i != 0 && mesh.visible(t as u32));
        log::debug!("select rect: {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
        if !any {
            self.invert_visibility();
            return;
        }
        let hidden: Vec<u8> = inside
            .par_iter()
            .enumerate()
            .map(|(t, &i)| (!mesh.visible(t as u32) || (i != 0) == hide) as u8)
            .collect();
        self.apply_hidden(hidden, if hide { "Hide rectangle" } else { "Select rectangle" });
    }

    /// Make one new polygroup of the visible polygons (ZBrush: Ctrl + W, Group Visible).
    fn group_visible(&mut self) {
        let t = Instant::now();
        let mesh = self.doc.mesh();
        let next = mesh.groups.par_iter().copied().max().map_or(1, |m| m.wrapping_add(1));
        let groups: Vec<u16> = (0..mesh.triangle_count() as u32)
            .into_par_iter()
            .map(|t| if mesh.visible(t) { next } else { mesh.group(t) })
            .collect();
        let before = self.doc.group_runs();
        let level = self.doc.active();
        self.doc.set_groups(level, groups);
        self.history.push(UndoStep::Groups(before));
        self.status = format!(
            "Group visible: {} polygroups ({:.0} ms over {} levels)",
            self.doc.mesh().group_count(),
            t.elapsed().as_secs_f32() * 1000.0,
            self.doc.count()
        );
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// Run a queued polygroup / visibility operation (not during a stroke or a selection).
    fn run_pending_vis(&mut self) {
        if self.stroke.is_some() || self.select.is_some() {
            return;
        }
        match self.pending_vis.take() {
            Some(VisOp::GroupVisible) => self.group_visible(),
            Some(VisOp::ShowAll) => self.show_all(),
            Some(VisOp::Invert) => self.invert_visibility(),
            None => {}
        }
    }

    /// Polygroup colours on the shown subtool (ZBrush: PolyF, Shift + F).
    fn toggle_polyframe(&mut self) {
        self.workspace.polyframe = !self.workspace.polyframe;
        if self.workspace.polyframe && self.gfx.as_ref().is_some_and(|g| !g.group_overlay_supported()) {
            self.workspace.notify("Polygroup colours need a GPU with primitive-index support");
        }
    }

    /// Whether the cursor is in the thin band along the canvas edge that counts as
    /// background.
    fn on_canvas_border(&self) -> bool {
        if !self.canvas_rect.is_positive() {
            return false;
        }
        let m = CANVAS_BORDER * self.scale;
        let r = self.canvas_rect.shrink(m);
        !r.contains(egui::pos2(self.cursor.x, self.cursor.y))
    }

    /// The selection rectangle for the workspace to draw, in logical pixels.
    fn show_select_rect(&mut self) {
        self.workspace.select_rect = self.select.and_then(|s| {
            ((self.cursor - s.start).length() >= CLICK_SLOP).then(|| {
                let (a, b) = (s.start / self.scale, self.cursor / self.scale);
                (egui::Rect::from_two_pos(egui::pos2(a.x, a.y), egui::pos2(b.x, b.y)), self.modifiers.alt_key())
            })
        });
    }

    /// Left button pressed on the window: brushes, navigation, subtool picking and the
    /// Ctrl + Shift selection, ZBrush style. `consumed` / `over_ui`: the interface took it.
    fn left_press(&mut self, consumed: bool, over_ui: bool) {
        let free = !consumed && !over_ui && self.nav.is_none() && self.stroke.is_none() && self.select.is_none();
        if self.workspace.popup.is_some() || self.workspace.quick_menu.is_some() {
            // The press that closes a popup or menu does nothing else.
            return;
        }
        let selecting = self.modifiers.control_key() && self.modifiers.shift_key();
        if selecting {
            if free {
                // ZBrush: Ctrl + Shift starts a selection on the model or the background.
                let hit = if self.on_canvas_border() {
                    None
                } else {
                    let (o, d) = self.cursor_ray();
                    self.mesh().raycast(o, d).map(|h| h.tri)
                };
                self.select = Some(SelectDrag { start: self.cursor, hit });
                self.hover = None;
            }
            return;
        }
        let picked = if self.modifiers.alt_key() && self.subtools.len() > 1 && free {
            self.pick_subtool().filter(|&i| i != self.active_tool)
        } else {
            None
        };
        if let Some(i) = picked {
            // ZBrush: Alt + click on another subtool selects it.
            self.select_subtool(i);
        } else if free {
            // ZBrush: a left drag on the model sculpts (Alt = alternate brush, Ctrl =
            // masking); on empty canvas, or on the canvas border, it rotates, with Alt it
            // moves, and releasing Alt turns it into a zoom.
            let hit = if self.on_canvas_border() {
                None
            } else {
                let (o, d) = self.cursor_ray();
                self.mesh().raycast(o, d)
            };
            log::trace!("press cursor={:?} hit={:?}", self.cursor, hit.as_ref().map(|h| h.point));
            if hit.is_none() && self.modifiers.control_key() {
                // ZBrush: a Ctrl drag on the background does not rotate; with DynaMesh
                // (here Wedge) on it remeshes when released.
                self.ctrl_background = Some(self.cursor);
            } else if hit.is_none() {
                self.begin_nav(MouseButton::Left);
            } else if self.modifiers.control_key() {
                self.workspace.notify("Masking is not connected yet");
            } else if self.workspace.mode != Mode::Draw {
                self.workspace.notify(format!(
                    "{} mode is not connected yet · Q returns to Draw",
                    self.workspace.mode.name()
                ));
            } else {
                self.begin_stroke();
            }
        }
    }

    /// Left button released.
    fn left_release(&mut self) {
        if let Some(start) = self.ctrl_background.take() {
            if (self.cursor - start).length() >= CLICK_SLOP {
                if self.subtools[self.active_tool].wedge {
                    self.pending_geo = Some(GeoOp::Wedge);
                    self.run_pending_geo();
                } else {
                    self.workspace.notify("Ctrl + drag on the background re-wedges while Wedge is on (Mesh: Wedge)");
                }
            }
            return;
        }
        if let Some(sel) = self.select.take() {
            self.workspace.select_rect = None;
            let alt = self.modifiers.alt_key();
            if (self.cursor - sel.start).length() < CLICK_SLOP {
                match sel.hit {
                    Some(tri) => self.isolate_group(tri, alt),
                    None => self.show_all(),
                }
            } else {
                self.select_rect(sel.start, self.cursor, alt);
            }
            self.update_hover();
            return;
        }
        self.end_stroke();
        self.end_nav(MouseButton::Left);
    }

    /// The cursor moved to `pos` (physical pixels).
    fn cursor_moved(&mut self, pos: Vec2, over_ui: bool) {
        self.last_cursor = self.cursor;
        self.cursor = pos;
        let delta = self.cursor - self.last_cursor;
        if let Some(drag) = self.nav {
            self.nav_move(drag, delta);
        } else if self.select.is_some() {
            self.show_select_rect();
        } else if self.stroke.is_some() {
            if self.stroke_brush.kind == BrushKind::Grab {
                self.stroke_pending = true;
            } else {
                self.apply_stroke();
            }
        } else if !over_ui {
            self.update_hover();
        } else {
            self.hover = None;
        }
    }

    // ------------------------------------------------------------ Wedge, Del Higher / Lower

    /// Run a queued geometry operation (not during a stroke or a selection).
    fn run_pending_geo(&mut self) {
        if self.stroke.is_some() || self.select.is_some() {
            return;
        }
        match self.pending_geo.take() {
            Some(GeoOp::WedgeToggle) => {
                let tool = &mut self.subtools[self.active_tool];
                if tool.wedge {
                    tool.wedge = false;
                    self.workspace.notify("Wedge off");
                } else if self.wedge_now() {
                    self.subtools[self.active_tool].wedge = true;
                }
            }
            Some(GeoOp::Wedge) => {
                self.wedge_now();
            }
            Some(GeoOp::DelHigher) => self.delete_levels(true),
            Some(GeoOp::DelLower) => self.delete_levels(false),
            None => {}
        }
    }

    /// Whether the shown mesh is closed, and its area, measured once per stack and level.
    fn wedge_info(&mut self) -> (bool, f32) {
        let key = (self.doc.generation, self.doc.active());
        match self.wedge_info {
            Some((g, l, closed, area)) if (g, l) == key => (closed, area),
            _ => {
                let (closed, area) = (wedge::closed(self.doc.mesh()), wedge::area(self.doc.mesh()));
                self.wedge_info = Some((key.0, key.1, closed, area));
                (closed, area)
            }
        }
    }

    /// Why Wedge cannot run on the active subtool now, if it cannot.
    fn wedge_refusal(&mut self) -> Option<String> {
        if self.doc.count() > 1 {
            return Some("Wedge needs a subtool without subdivision levels · Geometry: Del Higher / Del Lower".into());
        }
        if !self.wedge_info().0 {
            return Some("Wedge needs a closed surface (this one has a border)".into());
        }
        None
    }

    /// Remesh the active subtool with the Wedge settings, undoably. Returns whether it ran.
    fn wedge_now(&mut self) -> bool {
        if let Some(why) = self.wedge_refusal() {
            self.workspace.notify(why.clone());
            self.status = why;
            return false;
        }
        let t = Instant::now();
        // An internal inconsistency is reported, not allowed to take the model down.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wedge::remesh(self.doc.mesh(), &self.wedge)))
            .unwrap_or_else(|_| Err("the remesh failed on this mesh (internal error)".into()));
        let out = match result {
            Ok(out) => out,
            Err(e) => {
                self.status = format!("Wedge: {e}");
                self.workspace.notify(self.status.clone());
                return false;
            }
        };
        let remesh_ms = t.elapsed().as_secs_f32() * 1000.0;
        let before = self.doc.mesh().vertex_count();
        let t2 = Instant::now();
        let mesh = SculptMesh::from_raw_with_groups(out.positions, out.indices, out.groups);
        let budget = self.doc.budget;
        let old = std::mem::replace(&mut self.doc, Multires::new(mesh, budget));
        self.history.push(UndoStep::Replace(Box::new(old.pack())));
        let build_ms = t2.elapsed().as_secs_f32() * 1000.0;
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.show_active_level();
        let st = &out.stats;
        self.status = format!(
            "Wedge at {}: {} → {} vertices in {:.0} ms (remesh {remesh_ms:.0}, chunks {build_ms:.0}; grid {}×{}×{}, {} corners turned)",
            self.wedge.resolution,
            fmt_count(before),
            fmt_count(self.mesh().vertex_count()),
            t.elapsed().as_secs_f32() * 1000.0,
            st.grid[0],
            st.grid[1],
            st.grid[2],
            st.flips
        );
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
        true
    }

    /// Delete the levels above (`higher`) or below the shown one (ZBrush: Del Higher, Del
    /// Lower). Not undoable: the undo list, whose steps name levels, is cleared.
    fn delete_levels(&mut self, higher: bool) {
        let n = if higher { self.doc.delete_higher() } else { self.doc.delete_lower() };
        if n == 0 {
            self.workspace.notify(if higher { "No levels above this one" } else { "No levels below this one" });
            return;
        }
        self.history.clear();
        self.show_active_level();
        self.status = format!(
            "{}: {n} level{} deleted, {} vertices at the base · undo history cleared",
            if higher { "Del Higher" } else { "Del Lower" },
            if n == 1 { "" } else { "s" },
            fmt_count(self.doc.vertex_counts()[0])
        );
        self.workspace.notify(self.status.clone());
    }

    /// Press the left button at `from` with `mods`, drag to `to` and release, through the
    /// handlers the window uses (self-tests and tests).
    fn gesture(&mut self, mods: ModifiersState, from: Vec2, to: Vec2) {
        let saved = self.modifiers;
        self.modifiers = mods;
        self.cursor_moved(from, false);
        self.left_press(false, false);
        self.cursor_moved((from + to) * 0.5, false);
        self.cursor_moved(to, false);
        self.left_release();
        self.modifiers = saved;
    }

    /// `--selftest-wedge`, one phase per call, a few frames apart: Wedge on, then the GPU
    /// draws what it made; Ctrl + drag on the background re-wedges at a finer resolution;
    /// two undos bring back the original mesh, also on the GPU. Logs every check and its
    /// time; returns the next phase, or `None` when done.
    fn selftest_wedge_phase(&mut self, phase: u8) -> Option<u8> {
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_groups_state.1 &= pass;
            log::info!("selftest: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let drawn = |app: &Self| app.gfx.as_ref().map_or(0, Gfx::drawn_triangles);
        let solid = |app: &Self| {
            let m = app.mesh();
            let euler = m.vertex_count() as i64 - m.edge_count() as i64 + m.triangle_count() as i64;
            wedge::closed(m) && !m.edges_over_two_triangles() && euler % 2 == 0
        };
        match phase {
            0 => {
                self.selftest_groups_state = (self.mesh().vertex_count(), true);
                let before = self.mesh().vertex_count();
                let t = Instant::now();
                self.pending_geo = Some(GeoOp::WedgeToggle);
                self.run_pending_geo();
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                let on = self.subtools[self.active_tool].wedge;
                let after = self.mesh().vertex_count();
                check(self, &format!("Wedge on at {}: {} -> {} vertices in {ms:.0} ms", self.wedge.resolution, fmt_count(before), fmt_count(after)), on && after > 0);
                let ok = solid(self);
                check(self, "the result is closed and manifold", ok);
                Some(1)
            }
            1 => {
                let (d, t) = (drawn(self), self.mesh().triangle_count());
                check(self, &format!("GPU draws the {} new triangles", fmt_count(d)), d == t);
                let before = self.mesh().vertex_count();
                self.wedge.resolution = self.wedge.resolution * 3 / 2;
                let corner = self.canvas_origin() + Vec2::splat(30.0);
                let t = Instant::now();
                self.gesture(ModifiersState::CONTROL, corner, corner + Vec2::new(60.0, 40.0));
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                let after = self.mesh().vertex_count();
                check(self, &format!("Ctrl + drag on the background re-wedged at {}: {} -> {} vertices in {ms:.0} ms", self.wedge.resolution, fmt_count(before), fmt_count(after)), after > before);
                let ok = solid(self);
                check(self, "still closed and manifold", ok);
                Some(2)
            }
            2 => {
                let (d, t) = (drawn(self), self.mesh().triangle_count());
                check(self, &format!("GPU draws the {} re-wedged triangles", fmt_count(d)), d == t);
                let t = Instant::now();
                self.history.undo(&mut self.doc);
                self.history.undo(&mut self.doc);
                self.show_active_level();
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                let back = self.mesh().vertex_count() == self.selftest_groups_state.0;
                check(self, &format!("two undos brought back the original {} vertices in {ms:.0} ms", fmt_count(self.mesh().vertex_count())), back);
                Some(3)
            }
            _ => {
                let (d, t) = (drawn(self), self.mesh().triangle_count());
                check(self, &format!("GPU draws the original {} triangles again", fmt_count(d)), d == t);
                let ok = self.selftest_groups_state.1;
                log::info!("selftest: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
    }

    /// `--selftest-groups`, one phase per call, a few frames apart: Ctrl + Shift rectangle
    /// and Ctrl + W, then checks of what the GPU draws and picks, then show all and undo.
    /// Logs every check and its time; returns the next phase, or `None` when done.
    fn selftest_groups(&mut self, phase: u8) -> Option<u8> {
        let centre = self.canvas_origin() + self.viewport() * 0.5;
        let half = Vec2::splat(self.viewport().y * 0.12);
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_groups_state.1 &= pass;
            log::info!("selftest: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        match phase {
            0 => {
                let total = self.mesh().triangle_count();
                let t = Instant::now();
                self.gesture(cs, centre - half, centre + half);
                let rect_ms = t.elapsed().as_secs_f32() * 1000.0;
                let kept = total - self.mesh().hidden_count();
                check(self, &format!("Ctrl + Shift rectangle kept {} of {} triangles in {rect_ms:.0} ms", fmt_count(kept), fmt_count(total)), kept > 0 && kept < total);
                let t = Instant::now();
                self.modifiers = ModifiersState::CONTROL;
                self.on_key(KeyCode::KeyW);
                self.modifiers = ModifiersState::empty();
                self.run_pending_vis();
                let group_ms = t.elapsed().as_secs_f32() * 1000.0;
                let groups = self.mesh().group_count();
                check(self, &format!("Ctrl + W made {groups} polygroups in {group_ms:.0} ms"), groups == 2);
                self.workspace.polyframe = true;
                self.selftest_groups_state.0 = kept;
                Some(1)
            }
            1 => {
                let kept = self.selftest_groups_state.0;
                let drawn = self.gfx.as_ref().map_or(0, Gfx::drawn_triangles);
                check(self, &format!("GPU draws {} visible triangles", fmt_count(drawn)), drawn == kept);
                if let Some(gfx) = &self.gfx {
                    let (draw, groups) = gfx.marks_bytes();
                    let supported = gfx.group_overlay_supported();
                    log::info!(
                        "selftest: GPU extra for this level: visible indices {:.1} MB, polygroups {:.1} MB (PolyF {})",
                        draw as f64 / 1e6,
                        groups as f64 / 1e6,
                        if supported { "on" } else { "not supported" }
                    );
                    check(self, "polygroup buffer is up while PolyF is on", !supported || groups > 0);
                }
                self.cursor = centre;
                let at_centre = self.pick_subtool();
                self.cursor = centre + Vec2::new(half.x * 2.2, 0.0);
                let off_centre = self.pick_subtool();
                check(self, &format!("GPU pick: visible centre -> {at_centre:?}, hidden side -> {off_centre:?}"), at_centre == Some(self.active_tool) && off_centre.is_none());
                let corner = self.canvas_origin() + Vec2::splat(30.0);
                let t = Instant::now();
                self.gesture(cs, corner, corner);
                let show_ms = t.elapsed().as_secs_f32() * 1000.0;
                check(self, &format!("Ctrl + Shift + click on the background showed all in {show_ms:.0} ms"), self.mesh().hidden_count() == 0);
                Some(2)
            }
            _ => {
                let total = self.mesh().triangle_count();
                let drawn = self.gfx.as_ref().map_or(0, Gfx::drawn_triangles);
                check(self, &format!("GPU draws all {} triangles again", fmt_count(drawn)), drawn == total);
                let t = Instant::now();
                self.history.undo(&mut self.doc);
                self.history.undo(&mut self.doc);
                self.history.undo(&mut self.doc);
                self.show_active_level();
                let undo_ms = t.elapsed().as_secs_f32() * 1000.0;
                let clean = self.mesh().hidden_count() == 0 && self.mesh().group_count() == 1;
                check(self, &format!("three undos back to one group, nothing hidden, in {undo_ms:.0} ms"), clean);
                let ok = self.selftest_groups_state.1;
                log::info!("selftest: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
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
            // The centre of the bounds, or for a shape with a hole there (Ring3D) a point
            // three eighths of the way out along X or Z.
            let (c, e) = ((b.min + b.max) * 0.5, (b.max - b.min) * 0.375);
            let mut got = None;
            for p in [c, c + Vec3::X * e.x, c - Vec3::X * e.x, c + Vec3::Z * e.z, c - Vec3::Z * e.z] {
                self.cursor = self.project(p);
                got = self.pick_subtool();
                if got == Some(i) {
                    break;
                }
            }
            let pass = got == Some(i);
            ok &= pass;
            log::info!("selftest: pick on {} -> {:?} ({})", self.subtools[i].name, got.map(|g| &self.subtools[g].name), if pass { "ok" } else { "FAIL" });
        }
        self.cursor = self.canvas_origin() + Vec2::splat(4.0);
        let got = self.pick_subtool();
        ok &= got.is_none();
        log::info!("selftest: pick in the canvas corner -> {got:?} ({})", if got.is_none() { "ok" } else { "FAIL" });
        let target = (self.active_tool + 1) % self.subtools.len();
        let b = self.subtools[target].bounds;
        let (c, e) = ((b.min + b.max) * 0.5, (b.max - b.min) * 0.375);
        for p in [c, c + Vec3::X * e.x, c - Vec3::X * e.x, c + Vec3::Z * e.z, c - Vec3::Z * e.z] {
            self.cursor = self.project(p);
            if self.pick_subtool() == Some(target) {
                break;
            }
        }
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
        if self.stroke.is_some() {
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
        if self.stroke.is_some() || level == self.doc.active() || level > self.doc.top() {
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
            // A stack built afresh (Wedge, its undo, Del Lower) has none of the cached levels.
            if gfx.generation != self.doc.generation {
                gfx.clear_levels();
                gfx.generation = self.doc.generation;
            }
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

    /// Frame the visible model: the shown part of the active subtool and the visible
    /// parked ones (ZBrush: F).
    fn frame_camera(&mut self) {
        let b = self
            .drawn_parked()
            .iter()
            .fold(self.doc.mesh().visible_bounds(), |b, &(_, i)| subtool::union(b, self.subtools[i].bounds));
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

    /// Logical (egui) position of the cursor.
    fn cursor_pos(&self) -> egui::Pos2 {
        egui::pos2(self.cursor.x / self.scale, self.cursor.y / self.scale)
    }

    /// Replace the active subtool's mesh with the current primitive, in the same place and
    /// at the same size. Its levels and undo history go.
    fn rebuild(&mut self) {
        if self.stroke.is_some() {
            return;
        }
        let t = Instant::now();
        let b = self.doc.bounds();
        let center = (b.min + b.max) * 0.5;
        let size = ((b.max - b.min).max_element() * 0.5).max(1e-3);
        let (p, i) = self.primitive.build(size);
        let p = p.into_iter().map(|q| q + center).collect();
        self.doc = Multires::new(SculptMesh::from_raw(p, i), self.doc.budget);
        let tool = &mut self.subtools[self.active_tool];
        if let Some(rest) = auto_name_number(&tool.name) {
            tool.name = format!("{} {rest}", self.primitive.kind.short());
        }
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
            KeyCode::KeyZ if ctrl && shift => self.pending_redo = true,
            KeyCode::KeyZ if ctrl => self.pending_undo = true,
            KeyCode::KeyY if ctrl => self.pending_redo = true,
            // ZBrush: Ctrl + W groups the visible polygons; Shift + F shows polygroups (PolyF).
            KeyCode::KeyW if ctrl => self.pending_vis = Some(VisOp::GroupVisible),
            KeyCode::KeyF if shift => self.toggle_polyframe(),
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
        let (groups, hidden) = (self.doc.mesh().group_count(), self.doc.mesh().hidden_count());
        let wedge_refusal = self.wedge_refusal();
        let area = self.wedge_info().1;
        let wedge_estimate = self.wedge.estimate(area);
        let wedge_on = self.subtools[self.active_tool].wedge;
        let actions = self.workspace.show(
            ui,
            &mut workspace::Model {
                brush: &mut self.brush,
                primitive: &mut self.primitive,
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
                groups,
                hidden,
                polyframe_supported: self.gfx.as_ref().is_none_or(Gfx::group_overlay_supported),
                wedge: &mut self.wedge,
                wedge_on,
                wedge_refusal: wedge_refusal.as_deref(),
                wedge_estimate,
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
        if actions.group_visible {
            self.pending_vis = Some(VisOp::GroupVisible);
        }
        if actions.wedge_toggle {
            self.pending_geo = Some(GeoOp::WedgeToggle);
        }
        if actions.wedge_now {
            self.pending_geo = Some(GeoOp::Wedge);
        }
        if actions.del_higher {
            self.pending_geo = Some(GeoOp::DelHigher);
        }
        if actions.del_lower {
            self.pending_geo = Some(GeoOp::DelLower);
        }
        if actions.show_all {
            self.pending_vis = Some(VisOp::ShowAll);
        }
        if actions.invert_visibility {
            self.pending_vis = Some(VisOp::Invert);
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
            if self.stroke.is_none() {
                self.history.undo(&mut self.doc);
                self.show_active_level();
            }
        }
        if self.pending_redo {
            self.pending_redo = false;
            if self.stroke.is_none() {
                self.history.redo(&mut self.doc);
                self.show_active_level();
            }
        }
        if self.pending_divide {
            self.pending_divide = false;
            self.divide();
        }
        if let Some(level) = self.pending_level.take() {
            self.set_level(level);
        }
        self.run_pending_vis();
        self.run_pending_geo();
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
            Some(SubToolOp::Append) => self.append_primitive(),
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
        if let Some((phase, left)) = self.selftest_wedge {
            if left == 0 {
                self.selftest_wedge = self.selftest_wedge_phase(phase).map(|next| (next, 10));
                if self.selftest_wedge.is_none() {
                    self.quit = true;
                }
            } else {
                self.selftest_wedge = Some((phase, left - 1));
            }
        }
        if let Some((phase, left)) = self.selftest_groups {
            if left == 0 {
                self.selftest_groups = self.selftest_groups(phase).map(|next| (next, 10));
                if self.selftest_groups.is_none() {
                    self.quit = true;
                }
            } else {
                self.selftest_groups = Some((phase, left - 1));
            }
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
        let show_brush = self.hover.is_some() && !ctx.is_pointer_over_egui();
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
                0.0,
                0.0,
            ],
        };
        let parked: Vec<u64> = self.drawn_parked().into_iter().map(|(key, _)| key).collect();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let t = Instant::now();
        gfx.upload_dirty(self.doc.mesh_mut(), self.workspace.polyframe);
        self.upload_ms = t.elapsed().as_secs_f32() * 1000.0;
        let t = Instant::now();
        gfx.render(&uniforms, &parked, self.workspace.polyframe, &jobs, full.textures_delta, full.pixels_per_point);
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
            self.append_primitive();
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
                // Alt during a Ctrl + Shift drag turns the rectangle into a hide (red) one.
                if self.select.is_some() {
                    self.show_select_rect();
                }
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

/// The number of a subtool name made from a primitive ("Cube 3" gives "3"), so Rebuild can
/// rename it to the new primitive; `None` for a name the user chose.
fn auto_name_number(name: &str) -> Option<&str> {
    let (head, number) = name.rsplit_once(' ')?;
    let known = Primitive::ALL.iter().any(|p| p.short() == head);
    (known && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
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
fn bench(settings: primitive::Settings, divide: u32, budget: usize) {
    let mut app = App::with_primitive(settings, false, budget);
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
    bench_groups(&mut app);
    bench_wedge(app.doc.level_mesh(0), "level 0");
    if app.doc.top() > 0 {
        bench_wedge(app.doc.level_mesh(app.doc.top()), "the top level");
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

/// Polygroups and visibility at the shown level through the input handlers: a Ctrl + Shift
/// rectangle, Ctrl + W, isolate, show all, undo, and what they cost in memory.
fn bench_groups(app: &mut App) {
    let level = app.doc.top();
    app.doc.set_active(level);
    let centre = app.canvas_origin() + app.viewport() * 0.5;
    let half = Vec2::splat(app.viewport().y * 0.12);
    let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let total = app.mesh().triangle_count();
    let t = Instant::now();
    app.gesture(cs, centre - half, centre + half);
    let kept = total - app.mesh().hidden_count();
    println!(
        "visibility: Ctrl + Shift rectangle at level {level} kept {} of {} triangles in {:.0} ms (all {} levels updated)",
        fmt_count(kept),
        fmt_count(total),
        t.elapsed().as_secs_f32() * 1000.0,
        app.doc.count()
    );
    let t = Instant::now();
    let visible = app.mesh().visible_indices().map_or(0, |v| v.len() / 3);
    println!(
        "visibility: visible index buffer of {} triangles built in {:.1} ms ({:.1} MB to upload)",
        fmt_count(visible),
        t.elapsed().as_secs_f32() * 1000.0,
        visible as f64 * 12.0 / 1e6
    );
    let t = Instant::now();
    app.group_visible();
    println!("polygroups: Ctrl + W in {:.0} ms", t.elapsed().as_secs_f32() * 1000.0);
    let t = Instant::now();
    let packed = app.mesh().drawn_groups_packed();
    println!(
        "polygroups: PolyF buffer for {} drawn triangles in {:.1} ms ({:.1} MB)",
        fmt_count(visible),
        t.elapsed().as_secs_f32() * 1000.0,
        packed.len() as f64 * 4.0 / 1e6
    );
    let corner = app.canvas_origin() + Vec2::splat(8.0);
    let t = Instant::now();
    app.gesture(cs, corner, corner);
    println!("visibility: show all in {:.0} ms", t.elapsed().as_secs_f32() * 1000.0);
    let t = Instant::now();
    app.gesture(cs, centre, centre);
    println!(
        "visibility: isolate the new group in {:.0} ms, {} triangles shown",
        t.elapsed().as_secs_f32() * 1000.0,
        fmt_count(total - app.mesh().hidden_count())
    );
    let mesh = app.mesh();
    let ram = mesh.groups.len() * 2 + mesh.hidden.len() + mesh.shown.len();
    println!(
        "polygroups + visibility in RAM at this level: {:.0} MB ({:.1} bytes per vertex); history {:.1} MB",
        ram as f64 / 1e6,
        ram as f64 / mesh.vertex_count() as f64,
        app.history.bytes() as f64 / 1e6
    );
    let t = Instant::now();
    for _ in 0..4 {
        app.history.undo(&mut app.doc);
    }
    println!(
        "4 undos (isolate, show all, group, rectangle) in {:.0} ms, back to {} hidden and {} group",
        t.elapsed().as_secs_f32() * 1000.0,
        app.mesh().hidden_count(),
        app.mesh().group_count()
    );
}

/// Wedge of `mesh` at the resolution that gives about a million vertices, then the same
/// with Project; timings per phase and for building the chunked mesh.
fn bench_wedge(mesh: &SculptMesh, what: &str) {
    let area = wedge::area(mesh);
    let resolution = (2.0 / (1.5 * area / 1.0e6).sqrt()).round() as u32;
    for project in [false, true] {
        let s = wedge::Settings { resolution, blur: 2, project };
        let t = Instant::now();
        let out = match wedge::remesh(mesh, &s) {
            Ok(out) => out,
            Err(e) => {
                println!("wedge: {e}");
                return;
            }
        };
        let remesh_ms = t.elapsed().as_secs_f32() * 1000.0;
        let st = out.stats.clone();
        let n = out.positions.len();
        let t = Instant::now();
        let built = SculptMesh::from_raw_with_groups(out.positions, out.indices, out.groups);
        let build_ms = t.elapsed().as_secs_f32() * 1000.0;
        println!(
            "wedge {what} ({} vertices) at resolution {resolution}{}: {} vertices in {remesh_ms:.0} ms + {build_ms:.0} ms to chunk \
             (grid {:?}, {} crossings, {} cells, {} corners turned; scan {:.0}, cells {:.0}, surface {:.0}, blur + project {:.0} ms)",
            fmt_count(mesh.vertex_count()),
            if project { " with Project" } else { "" },
            fmt_count(n),
            st.grid,
            fmt_count(st.crossings),
            fmt_count(st.cells),
            st.flips,
            st.scan_ms,
            st.cells_ms,
            st.surface_ms,
            st.project_ms
        );
        assert!(!built.edges_over_two_triangles());
    }
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
    let demo = args.iter().any(|a| a == "--demo");
    let stress = args.iter().any(|a| a == "--stress");
    let frames = args.iter().any(|a| a == "--frames");
    let vsync = !args.iter().any(|a| a == "--novsync");
    // `--primitive NAME` picks the starting primitive (sphere, cube, cylinder, ring or
    // torus, plane; ZBrush names work too); `--segments N` sets its resolution. Without
    // them the model starts as a 4k-vertex sphere; `--segments 400` is the 960k one.
    let mut settings = primitive::Settings::default();
    if let Some(name) = flag("--primitive") {
        match Primitive::parse(name) {
            Some(kind) => settings.kind = kind,
            None => eprintln!("--primitive {name}: unknown (sphere, cube, cylinder, ring, plane); using a sphere"),
        }
    }
    if let Some(segments) = flag("--segments").and_then(|s| s.parse().ok()) {
        settings.set_current(segments);
    }
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        // `--bench N`: N segments of the primitive, 400 (the 960k sphere) by default.
        settings.set_current(args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(400));
        bench(settings, divide, budget);
        return;
    }
    let subtools: usize = flag("--subtools").and_then(|s| s.parse().ok()).unwrap_or(1);
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::with_primitive(settings, demo, budget);
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
    if args.iter().any(|a| a == "--selftest-wedge") {
        app.selftest_wedge = Some((0, 20));
    }
    if let Some(r) = flag("--wedge-resolution").and_then(|s| s.parse().ok()) {
        app.wedge.resolution = r;
    }
    if args.iter().any(|a| a == "--selftest-groups") {
        app.selftest_groups = Some((0, 20));
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
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        app.deform(Deform::OffsetX, -50.0);
        let shown = app.doc.mesh().positions.clone();
        let stack = app.doc.bytes();

        app.append_primitive();
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
    fn append_new_and_rebuild_use_the_chosen_primitive() {
        for kind in Primitive::ALL {
            let mut app = App::new(8, false, usize::MAX);
            app.primitive.kind = kind;
            app.primitive.set_current(kind.default_segments());
            let first = app.doc.bounds();
            app.append_primitive();
            assert_eq!(app.subtools.len(), 2);
            assert_eq!(app.active_tool, 1);
            assert_eq!(app.subtools[1].name, format!("{} 2", kind.short()));
            assert_eq!(app.doc.mesh().vertex_count(), kind.vertex_count(kind.default_segments()));
            assert_eq!(app.doc.top(), 0, "an appended primitive starts undivided");
            // Beside the model like the old sphere append: to its right with a gap of a
            // quarter of its size, centred on the model in Y and Z, half the size.
            let b = app.doc.bounds();
            let gap = b.min.x - first.max.x;
            assert!((gap - 0.125).abs() < 1e-3, "{kind:?} gap {gap}");
            let (c, fc) = ((b.min + b.max) * 0.5, (first.min + first.max) * 0.5);
            assert!((c.y - fc.y).abs() < 1e-3 && (c.z - fc.z).abs() < 1e-3, "{kind:?} centre {c:?}");
            assert!(((b.max - b.min).max_element() - 1.0).abs() < 1e-3, "{kind:?} is half size");
            // It divides like any subtool.
            app.divide();
            assert_eq!(app.doc.top(), 1);

            // Rebuild keeps the place and size, renames an automatic name, keeps a chosen one.
            app.primitive.kind = Primitive::Sphere;
            app.rebuild();
            let r = app.doc.bounds();
            assert_eq!(app.doc.top(), 0);
            assert_eq!(app.subtools[1].name, "Sphere 2");
            assert!((((r.min + r.max) * 0.5) - c).length() < 1e-3, "{kind:?} rebuilt in place");
            app.subtools[1].name = "Head".into();
            app.rebuild();
            assert_eq!(app.subtools[1].name, "Head");

            app.primitive.kind = kind;
            app.new_project();
            assert_eq!(app.subtools.len(), 1);
            assert_eq!(app.doc.mesh().vertex_count(), kind.vertex_count(kind.default_segments()));
            assert!(app.subtools[0].name.starts_with(kind.short()));
            let b = app.doc.bounds();
            assert!(((b.max - b.min).max_element() - 2.0).abs() < 1e-3, "{kind:?} new project size");
        }
    }

    #[test]
    fn the_default_start_is_a_4k_sphere_and_segments_400_the_old_one() {
        let app = App::with_primitive(primitive::Settings::default(), false, usize::MAX);
        assert_eq!(app.doc.mesh().vertex_count(), 4058);
        assert_eq!(app.subtools[0].name, "Sphere 1");
        let mut dense = primitive::Settings::default();
        dense.set_current(Primitive::Sphere.dense_segments());
        assert_eq!(dense.vertex_count(), 960_002);
        assert_eq!(auto_name_number("Cube 12"), Some("12"));
        assert_eq!(auto_name_number("Cube"), None);
        assert_eq!(auto_name_number("Eye 2"), None);
        assert_eq!(auto_name_number("Sphere 1 copy"), None);
    }

    #[test]
    fn projects_save_open_and_export_every_subtool() {
        let dir = std::env::temp_dir();
        let tag = std::process::id();
        let project = dir.join(format!("klay-test-{tag}.klay"));
        let obj = dir.join(format!("klay-test-{tag}.obj"));

        let mut app = App::new(12, false, usize::MAX);
        app.primitive.set_current(8);
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        app.deform(Deform::Size, 40.0);
        app.append_primitive();
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

    fn drag(app: &mut App, mods: ModifiersState, from: Vec2, to: Vec2) {
        app.gesture(mods, from, to);
    }

    fn ctrl_shift() -> ModifiersState {
        ModifiersState::CONTROL | ModifiersState::SHIFT
    }

    #[test]
    fn ctrl_shift_gestures_hide_isolate_show_and_invert_like_zbrush() {
        let mut app = App::new(24, false, usize::MAX);
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        let total = app.doc.mesh().triangle_count();
        let centre = app.viewport() * 0.5;
        // The sphere covers the middle of the canvas; the corner is background.
        let corner = Vec2::new(8.0, 8.0);

        // Ctrl + Shift + drag a rectangle: what is inside stays, the rest is hidden.
        drag(&mut app, ctrl_shift(), centre - Vec2::splat(80.0), centre + Vec2::splat(80.0));
        let kept = total - app.doc.mesh().hidden_count();
        assert!(kept > 0 && kept < total / 4, "kept {kept} of {total}");
        assert!(app.workspace.select_rect.is_none(), "the rectangle is gone after release");
        // Hidden triangles cannot be hit: off-centre the ray finds nothing to sculpt.
        app.cursor = centre + Vec2::new(250.0, 0.0);
        let (o, d) = app.cursor_ray();
        assert!(app.mesh().raycast(o, d).is_none());
        // The level below follows: its triangles are hidden where all children are.
        assert!(app.doc.level_mesh(0).hidden_count() > 0);

        // A brush stroke over the border only moves vertices that touch a visible triangle.
        let before = app.doc.mesh().positions.clone();
        app.brush = Brush { kind: BrushKind::Draw, radius: 0.8, strength: 1.0 };
        drag(&mut app, ModifiersState::empty(), centre, centre + Vec2::new(30.0, 0.0));
        let mesh = app.doc.mesh();
        let moved: Vec<usize> = (0..before.len()).filter(|&v| mesh.positions[v] != before[v]).collect();
        assert!(!moved.is_empty());
        assert!(moved.iter().all(|&v| mesh.shown[v] != 0), "a hidden vertex moved");

        // Ctrl + W: one new polygroup of the visible part.
        app.modifiers = ModifiersState::CONTROL;
        app.on_key(KeyCode::KeyW);
        app.run_pending_vis();
        app.modifiers = ModifiersState::empty();
        assert_eq!(app.doc.mesh().group_count(), 2);
        assert_eq!(app.doc.level_mesh(0).group_count(), 2, "groups reach the base");

        // Ctrl + Shift + click on the background: show everything.
        drag(&mut app, ctrl_shift(), corner, corner);
        assert_eq!(app.doc.mesh().hidden_count(), 0);
        assert!(app.doc.mesh().hidden.is_empty() && app.doc.mesh().shown.is_empty());

        // Ctrl + Shift + click on the new group: only it stays.
        drag(&mut app, ctrl_shift(), centre, centre);
        assert_eq!(total - app.doc.mesh().hidden_count(), kept);
        // Ctrl + Shift + Alt + click on it would hide everything: refused.
        drag(&mut app, ctrl_shift() | ModifiersState::ALT, centre, centre);
        assert_eq!(total - app.doc.mesh().hidden_count(), kept);
        // Ctrl + Shift + drag on the background around nothing: inverted.
        drag(&mut app, ctrl_shift(), corner, corner + Vec2::splat(30.0));
        assert_eq!(app.doc.mesh().hidden_count(), kept);
        // Show all, then Alt + drag (red rectangle) hides what it covers.
        drag(&mut app, ctrl_shift(), corner, corner);
        drag(&mut app, ctrl_shift() | ModifiersState::ALT, centre - Vec2::splat(80.0), centre + Vec2::splat(80.0));
        let hid = app.doc.mesh().hidden_count();
        // The stroke moved a few triangle centres in or out of the rectangle since.
        assert!(hid.abs_diff(kept) * 20 < kept, "hid {hid}, kept {kept} before");

        // Shift + F toggles polygroup colours.
        app.modifiers = ModifiersState::SHIFT;
        app.on_key(KeyCode::KeyF);
        assert!(app.workspace.polyframe);

        // Save and open: groups and visibility come back.
        let bytes = app.project_bytes();
        let project = project::read_from(&bytes[..], bytes.len() as u64).unwrap();
        let back = project.tools.into_iter().next().unwrap().packed.try_unpack().unwrap();
        assert_eq!(back.marks(), app.doc.marks());

        // Undo every step, back to one group and nothing hidden; the divide stays.
        while app.doc.mesh().hidden_count() > 0 || app.doc.mesh().group_count() > 1 {
            assert!(app.history.can_undo());
            app.history.undo(&mut app.doc);
        }
        assert_eq!(app.doc.top(), 1);
        assert!(app.doc.marks().iter().all(|m| m.groups.lens.is_empty() && m.hidden.is_empty()));
    }

    #[test]
    fn wedge_remeshes_rewedges_on_ctrl_drag_and_undoes() {
        let mut app = App::new(16, false, usize::MAX);
        app.wedge.resolution = 48;
        let original = app.doc.mesh().positions.clone();
        let corner = Vec2::new(8.0, 8.0);

        // Ctrl + drag on the background does nothing while Wedge is off (and never rotates).
        let yaw = app.camera.yaw;
        drag(&mut app, ModifiersState::CONTROL, corner, corner + Vec2::splat(60.0));
        assert_eq!(app.doc.mesh().positions, original);
        assert_eq!(app.camera.yaw, yaw);

        // Polygroups made before Wedge ride along.
        let centre = app.viewport() * 0.5;
        drag(&mut app, ctrl_shift(), centre - Vec2::splat(120.0), centre + Vec2::splat(120.0));
        app.group_visible();
        drag(&mut app, ctrl_shift(), corner, corner);

        // Wedge on: an even, closed mesh replaces the sphere.
        app.pending_geo = Some(GeoOp::WedgeToggle);
        app.run_pending_geo();
        assert!(app.subtools[0].wedge);
        let first = app.doc.mesh().vertex_count();
        assert!(first > 5 * original.len(), "{first} vertices");
        assert!(wedge::closed(app.doc.mesh()));
        assert_eq!(app.doc.mesh().group_count(), 2, "polygroups carried over");
        assert_eq!(app.doc.mesh().hidden_count(), 0);

        // Ctrl + drag on the background re-wedges at the current resolution.
        app.wedge.resolution = 64;
        drag(&mut app, ModifiersState::CONTROL, corner, corner + Vec2::splat(60.0));
        let second = app.doc.mesh().vertex_count();
        assert!(second > first, "{second} after {first}");
        // A Ctrl click (no drag) does not.
        drag(&mut app, ModifiersState::CONTROL, corner, corner);
        assert_eq!(app.doc.mesh().vertex_count(), second);

        // Undo twice: the sphere exactly as it was, with its groups.
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().vertex_count(), first);
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().positions, original);
        assert_eq!(app.doc.mesh().group_count(), 2);
        app.history.redo(&mut app.doc);
        assert_eq!(app.doc.mesh().vertex_count(), first);

        // Wedge off: Ctrl + drag is inert again.
        app.pending_geo = Some(GeoOp::WedgeToggle);
        app.run_pending_geo();
        assert!(!app.subtools[0].wedge);
        drag(&mut app, ModifiersState::CONTROL, corner, corner + Vec2::splat(60.0));
        assert_eq!(app.doc.mesh().vertex_count(), first);
    }

    #[test]
    fn wedge_refuses_levels_and_open_surfaces_and_del_lower_makes_room() {
        let mut app = App::new(12, false, usize::MAX);
        app.wedge.resolution = 40;
        assert!(app.doc.divide());
        app.history.push(UndoStep::Divide(None));
        let shown = app.doc.mesh().positions.clone();
        app.pending_geo = Some(GeoOp::WedgeToggle);
        app.run_pending_geo();
        assert!(!app.subtools[0].wedge, "refused with subdivision levels");
        assert!(app.status.contains("subdivision levels"), "{}", app.status);

        // Del Lower: the shown level becomes the base, sculpting and all.
        app.pending_geo = Some(GeoOp::DelLower);
        app.run_pending_geo();
        assert_eq!(app.doc.count(), 1);
        assert_eq!(app.doc.mesh().positions, shown);
        assert!(!app.history.can_undo(), "Del Lower clears the undo list");
        app.pending_geo = Some(GeoOp::WedgeToggle);
        app.run_pending_geo();
        assert!(app.subtools[0].wedge);

        // Del Higher drops the levels above the shown one.
        assert!(app.doc.divide() && app.doc.divide());
        app.set_level(0);
        app.pending_geo = Some(GeoOp::DelHigher);
        app.run_pending_geo();
        assert_eq!(app.doc.count(), 1);

        // An open surface (Plane3D) is refused.
        app.primitive.kind = Primitive::Plane;
        app.primitive.set_current(16);
        app.rebuild();
        app.subtools[0].wedge = false;
        app.pending_geo = Some(GeoOp::WedgeToggle);
        app.run_pending_geo();
        assert!(!app.subtools[0].wedge);
        assert!(app.status.contains("closed"), "{}", app.status);
    }
}
