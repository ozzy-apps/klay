mod branding;
mod brush_asset;
mod asset_library;
mod alpha_library;
mod brush_preview;
mod paint;
mod camera;
mod mask;
mod matcap;
mod surface_tools;
mod symmetry;
mod detail_brush;
mod curve;
mod curve_tube;
mod cut;
mod deform;
mod file_dialog;
mod gizmo;
mod insert_mesh;
mod sculptris;
mod fx;
mod boolean;
mod zremesh;
mod zmodeler;
mod layers;
mod obj;
mod pressure;
mod print;
mod stl;
mod subtool_ops;
mod native_tests;
mod mesh;
mod multires;
mod polygroup;
mod primitive;
mod project;
mod render;
mod sculpt;
mod subtool;
mod tablet;
mod update;
mod transform;
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

/// How close (physical pixels at scale 1) a press must be to an applied curve to bend it.
const CURVE_GRAB_PX: f32 = 10.0;

/// The brush along `curve`: detail reliefs as one pattern in the curve's frame, other
/// brushes as dabs every `step` of arc length.
fn apply_curve(stroke: &mut Stroke, mesh: &mut SculptMesh, brush: &Brush, curve: &curve::Curve, view_dir: Vec3, step: f32) {
    stroke.set_view(view_dir);
    if brush.kind.is_detail() {
        stroke.detail_curve(mesh, brush, curve);
    } else {
        stroke.dabs_along(mesh, brush, curve, view_dir, step);
    }
}

/// State of `--selftest-curve` between its phases.
#[derive(Default)]
struct CurveTest {
    ok: bool,
    original: Vec<Vec3>,
    applied: Vec<Vec3>,
    bent: Vec<Vec3>,
}

/// A curve stroke being drawn (ZBrush: Stroke › Curve): the cursor path in canvas pixels
/// and the brush as it was at the press. It is applied on release.
struct CurveDraw {
    path: Vec<Vec2>,
    brush: Brush,
    invert: bool,
}

/// An applied curve that can still be bent (see `curve.rs`), with what it was applied
/// with and where.
struct CurveTool {
    curve: curve::Curve,
    brush: Brush,
    invert: bool,
    symmetry: symmetry::Symmetry,
    assets: sculpt::StampSettings,
    level: usize,
    tool: usize,
    vertices: usize,
    /// `History::edits` right after it was applied: any other edit lets it go.
    edits: u64,
    /// Positions and colours of every chunk it has touched, from before it was first
    /// applied: a bent curve is re-applied from these.
    base: std::collections::HashMap<u32, Vec<Vec3>>,
    base_paint: std::collections::HashMap<u32, Vec<paint::Color>>,
    bend: Option<CurveBend>,
    /// CurveTube: the tube it added instead of a brush result.
    tube: Option<TubePart>,
}

/// The tube a CurveTube curve added to the active subtool: everything after `keep` in its
/// base mesh, in polygroup `group`, built with `shape`. Bending or resizing swaps it.
#[derive(Clone, Copy, Debug)]
struct TubePart {
    keep: mesh::Extent,
    group: u16,
    shape: curve_tube::Tube,
}

/// A drag on an applied curve: its points on the canvas when the drag began (`None`
/// behind the camera), their arc length along the whole curve, the grabbed arc length and
/// how far along the curve the pull reaches. `stroke` records the bend for undo.
struct CurveBend {
    stroke: Stroke,
    /// The curve's pieces when the drag began (`screen` and `along` follow them).
    pieces: Vec<std::ops::Range<usize>>,
    screen: Vec<Option<Vec2>>,
    along: Vec<f32>,
    grab: f32,
    reach: f32,
    press: Vec2,
    /// Moved since the last frame's re-application.
    pending: bool,
    /// CurveTube: the curve and the tube when the drag began (the undo step of the bend).
    tube_from: Option<(curve::Curve, Option<Box<SculptMesh>>)>,
}

/// A Ctrl + Shift press in progress (ZBrush: SelectRect). A click acts on the polygroup
/// under it (or shows everything on the background); a drag draws a rectangle.
#[derive(Clone, Copy, Debug)]
struct SelectDrag {
    start: Vec2,
    /// Triangle of the active subtool under the press; `None` on the background.
    hit: Option<u32>,
}

/// A press that moves less than this (physical pixels) before release is a click.
const CLICK_SLOP: f32 = 4.0;

/// Document operations queued by the interface or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocOp {
    New,
    Open,
    Save,
    /// SubTool: save the active subtool on its own, add the subtools of a file, export the
    /// active subtool as OBJ (all at `App::subtool_path`, set by the file dialog).
    SaveSubtool,
    LoadSubtools,
    ExportSubtool,
    /// Document › Import: the OBJ or STL at `App::import_path` as a new subtool.
    ImportMesh,
    /// Export STL: the visible subtools to `App::stl_path`.
    ExportStl,
}

/// Mesh › Layers operations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayerOp {
    New,
    Delete(usize),
    Bake(usize),
    BakeAll,
    /// Start or stop recording into the layer.
    Rec(usize),
    /// Show or hide the layer.
    Eye(usize),
    /// Intensity while the slider moves, and on release with the value it started from.
    Intensity(usize, f32),
    IntensityDone(usize, f32),
}

/// Mesh › Morph Target operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorphOp {
    Store,
    Switch,
    Delete,
}

/// Polygroup and visibility operations queued by the interface or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisOp {
    GroupVisible,
    ShowAll,
    Invert,
    /// Visibility › Grow / Shrink (one ring of polygons).
    Grow,
    Shrink,
    /// Polygroups › Auto Groups, Group By Normals (max angle, degrees).
    AutoGroups,
    ByNormals(u8),
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
    /// Geometry › Del Hidden, Close Holes; Deformation › Mirror And Weld.
    DelHidden,
    CloseHoles,
    MirrorWeld,
    /// Geometry › ZRemesher.
    ZRemesh,
}

/// SubTool operations queued by the interface.
#[derive(Clone, Copy, Debug)]
enum SubToolOp {
    Select(usize),
    Append,
    Duplicate,
    Delete,
    Deform(Deform, f32),
    /// SubTool › Merge Down, Split Hidden, Groups Split, Extract.
    MergeDown,
    SplitHidden,
    GroupsSplit,
    Extract,
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
    kind: BrushKind,
    direct: bool,
    normal: Vec3,
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

#[derive(Default)]
struct PaintTest {
    frame: u32,
    original: Vec<Vec3>,
    painted: Vec<[u8; 4]>,
    ok: bool,
}

#[derive(Default)]
struct BrushTest {
    frame: u32,
    index: usize,
    /// The model has been rippled for SmoothValleys / SmoothPeaks (a smooth sphere has no
    /// valleys or peaks to smooth).
    rippled: bool,
    original: Vec<Vec3>,
    edited: Vec<Vec3>,
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

/// An Insert Mesh drag (IMM Primitives, see `insert_mesh.rs`): the mesh goes in at the
/// press, at Draw Size, and is put in again at the drag's size until the release.
#[derive(Clone, Debug)]
struct InsertDrag {
    /// The subtool as it was; everything after it is the inserted mesh.
    keep: mesh::Extent,
    at: Vec3,
    normal: Vec3,
    press: Vec2,
    radius: f32,
    group: u16,
    /// The model's mean edge length, for the inserted mesh's density.
    edge: f32,
    shape: primitive::Primitive,
    symmetry: symmetry::Symmetry,
    /// The mask before (for undo).
    mask: Vec<u8>,
}

/// The polygon view of the active subtool ZModeler works on, kept while the subtool's
/// topology is the one it made (`generation`): its polygons, the polygon of each mesh
/// triangle and the mesh vertex of each polygon vertex.
struct ZModel {
    poly: zmodeler::PolyMesh,
    face_of: Vec<u32>,
    vmap: Vec<u32>,
    generation: u64,
}

/// A ZModeler click or drag: what it started on, the polygons before it and the subtool
/// before it (the undo step).
struct ZDrag {
    hover: zmodeler::Hover,
    base: zmodeler::PolyMesh,
    press: Vec2,
    before: Box<multires::Packed>,
    /// The polygons a polygon action takes, and where the drag is measured from.
    faces: Vec<u32>,
    anchor: Vec3,
    normal: Vec3,
    label: String,
    changed: bool,
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
    /// Moving, scaling or rotating the active subtool about `anchor` (`squash`: Scale's
    /// middle circle, along the line). `map` is only shown (the vertex shader applies it to
    /// the shown level) until the release applies it to every level as one undo step.
    Act {
        mode: Mode,
        squash: bool,
        anchor: Vec3,
        press: Vec2,
        line: ActionLine,
        map: Affine,
    },
    /// Dragging a Gizmo 3D handle; `place` is where the gizmo stood at the press. `alone`
    /// (Alt): the drag moves or turns the gizmo, not the model.
    Gizmo {
        handle: gizmo::Handle,
        place: gizmo::Place,
        press: Vec2,
        map: Affine,
        alone: bool,
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
    /// `--selftest-groups`: `(phase, frames left)` of the polygroup / visibility self-test,
    /// which runs its steps a few frames apart so the GPU state can be checked in between.
    selftest_groups: Option<(u8, u32)>,
    /// What the polygroup self-test expects to find, whether every check passed, and the
    /// polygroups the model started with.
    selftest_groups_state: (usize, bool, usize),
    /// `--selftest-transpose`: frame counter of the scripted move / scale / rotate / undo.
    selftest_transpose: Option<TransposeTest>,
    /// `--selftest-mask`: the scripted masking session.
    selftest_mask: Option<MaskTest>,
    selftest_paint: Option<PaintTest>,
    selftest_brushes: Option<BrushTest>,
    selftest_surface: Option<native_tests::SurfaceTest>,
    /// `--selftest-materials`: every material drawn and read back, then polypaint under a MatCap.
    selftest_materials: Option<native_tests::MaterialTest>,
    selftest_boolean: Option<native_tests::BooleanTest>,
    /// `--selftest-document`: Back / Border on the GPU, the empty border, Snapshot.
    selftest_document: Option<native_tests::DocumentTest>,
    /// `--selftest-cull`: culled and whole frames compared after camera moves and edits.
    selftest_cull: Option<native_tests::CullTest>,
    /// `--selftest-splat`: points against triangles after paint, a mask and a Grab.
    selftest_splat: Option<native_tests::SplatTest>,
    /// `--snapshot FILE`: frames left, then the canvas is saved to FILE and the app quits.
    snapshot_to: Option<(String, u32)>,
    /// `--snapshot-ui`: the snapshot is the whole window, interface included.
    snapshot_ui: bool,
    /// `--selftest-symmetry`: brushes with Y and radial symmetry on the GPU.
    selftest_symmetry: Option<native_tests::SymmetryTest>,
    /// Project file shown in the Document palette, used by Save and Open.
    project_path: String,
    /// SubTool › Save / Load / Export OBJ: a file holding single subtools.
    subtool_path: String,
    /// Document › Import: OBJ or STL file to import.
    import_path: String,
    /// Export STL (Document and 3D Print): where the STL goes.
    stl_path: String,
    /// 3D Print › Check: the last result, and the model it was for (see `print_stamp`).
    print_report: Option<print::Report>,
    /// `--print-check`: run the check once the model is built.
    print_check_on_start: bool,
    /// The desktop's Open / Save dialog while it is open, the folder of the last file picked
    /// in one, and whether `project_path` is a file this project was opened from or saved to
    /// (Save asks for a file until it is).
    file_dialog: Option<file_dialog::Pending>,
    last_dir: Option<std::path::PathBuf>,
    project_has_file: bool,
    /// QuickSave: when the last input came, when the model was last QuickSaved, and the
    /// (subtool id, edit count) it was saved at, so an unchanged model is not saved again.
    last_input: Instant,
    last_quicksave: Instant,
    quicksaved_at: (u64, u64),
    /// A self-test or scripted run: no QuickSave into the user's folder.
    automated: bool,
    /// Mesh › Morph Target per subtool id: the level it was stored at and its positions.
    morph_targets: std::collections::HashMap<u64, (usize, std::sync::Arc<Vec<Vec3>>)>,
    /// `--open PATH`: project to open once the window exists.
    open_on_start: Option<String>,
    pending_doc: Option<DocOp>,
    camera: Camera,
    brush: Brush,
    history: History,
    stroke: Option<Stroke>,
    stroke_brush: Brush,
    /// Pen pressure from the tablet (X11), read on every pointer event.
    tablet: Option<tablet::Tablet>,
    /// Pressure of the pen moving the pointer, 0..=1; `None` for a mouse (full strength).
    pressure: Option<f32>,
    /// Why there is no tablet, for the interface.
    tablet_error: Option<String>,
    /// `--selftest-pressure`: frames left before the scripted pressure strokes.
    selftest_pressure: Option<u32>,
    /// Curve mode: the curve being drawn, and the applied one that can be bent.
    curve_draw: Option<CurveDraw>,
    curve: Option<CurveTool>,
    /// `--selftest-curve`: phase and frames left before it.
    selftest_curve: Option<(u8, u32)>,
    selftest_curve_state: CurveTest,
    /// `--selftest-tube`: phase and frames left before it (shares `selftest_curve_state`).
    selftest_tube: Option<(u8, u32)>,
    /// `--selftest-insert`: likewise, for Insert Mesh.
    selftest_insert: Option<(u8, u32)>,
    /// `--selftest-zmodeler`: likewise, for ZModeler.
    selftest_zmodeler: Option<(u8, u32)>,
    /// `--selftest-sculptris`: likewise, for Sculptris Pro.
    selftest_sculptris: Option<(u8, u32)>,
    /// `--hold`: a self-test leaves the window open when it is done.
    selftest_hold: bool,
    /// `--selftest-clip`: frames left before the crossing Clip Line cuts.
    selftest_clip: Option<u32>,

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
    /// Wedge settings (ZBrush: Tool > Geometry > DynaMesh).
    wedge: wedge::Settings,
    /// For the Wedge section: whether the shown mesh is closed and its area, kept for the
    /// stack generation and level they were measured on (both cost a pass over the mesh).
    wedge_info: Option<(u64, usize, bool, f32)>,
    hover: Option<Vec3>,
    /// The Transpose action line, kept between drags (shown in Move / Scale / Rotate mode).
    action_line: Option<ActionLine>,
    transpose: Option<TransposeDrag>,
    insert: Option<InsertDrag>,
    zmodel: Option<ZModel>,
    zm_hover: Option<zmodeler::Hover>,
    zm_drag: Option<ZDrag>,
    /// Bridge: the polygon picked first (its vertices), waiting for the second.
    zm_bridge: Option<Vec<u32>>,
    /// Sculptris Pro during a stroke: the subtool as it was before (the stroke's undo step
    /// once the mesh changed), whether it changed, and where the last pass was.
    sculptris_before: Option<Box<sculptris::Before>>,
    /// Through the stroke: for each chunk the chunk it was at the start (`None`: rebuilt),
    /// and for each vertex its number at the start (`u32::MAX`: new).
    sculptris_track: (Vec<Option<u32>>, Vec<u32>),
    /// The vertex cap was reached this stroke (told once).
    sculptris_capped: bool,
    sculptris_changed: bool,
    sculptris_at: Vec<Vec3>,
    /// Live Boolean: each subtool's distance volume and the version it was made from.
    bool_volumes: std::collections::HashMap<u64, (u64, wedge::Volume)>,
    /// Live Boolean was refused for too many subtools (told once).
    bool_refused: bool,
    /// Where Gizmo 3D was put (Alt + click, Alt + drag) or carried by its drags, for the
    /// subtool with this id; otherwise it stands at the subtool's centre, world-aligned.
    gizmo_place: Option<(u64, gizmo::Place)>,
    mask_press: Option<MaskPress>,
    mask_stroke: Option<mask::MaskStroke>,
    gesture: Option<surface_tools::Gesture>,

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
    /// Whole-mask operation from a shortcut or the Masking palette.
    pending_mask: Option<MaskOp>,
    pending_level: Option<usize>,
    /// Polygroup / visibility operation from the interface or a key, run outside the egui pass.
    pending_vis: Option<VisOp>,
    /// Geometry operation (Wedge, Del Higher / Lower), run outside the egui pass.
    pending_geo: Option<GeoOp>,
    /// `--selftest-wedge`: `(phase, frames left)`, like the polygroup self-test.
    selftest_wedge: Option<(u8, u32)>,
    /// SubTool list operation from the interface, run outside the egui pass.
    pending_subtool: Option<SubToolOp>,
    /// `--render triangles`: draw the active subtool whole, without GPU culling (A/B checks).
    render_triangles: bool,
    /// `--camera-distance K`: the camera distance times K once the model is framed
    /// (`--frames`, `--snapshot`).
    camera_distance: Option<f32>,
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
    /// Screen point (window pixels) the stroke has reached, and the LazyMouse brush point.
    stroke_from: Vec2,
    lazy_point: Option<Vec2>,
    /// DragRect (true) / DragDot (false) stroke in progress: the press point (window
    /// pixels) and the surface under it.
    drag_stamp: Option<(bool, Vec2, Option<(Vec3, Vec3)>)>,
    last_dab_count: usize,
    demo: bool,
    /// `--demo-brushes`: stroke with the advanced brushes at start.
    demo_brushes: bool,
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
        let (p, i, g) = settings.build(1.0);
        let mesh = SculptMesh::from_raw_with_groups(p, i, g);
        let build_ms = t.elapsed().as_secs_f32() * 1000.0;
        Self {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            subtools: vec![SubTool {
            boolean: Default::default(),
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
            selftest_groups_state: (0, true, 1),
            selftest_transpose: None,
            selftest_mask: None,
            selftest_paint: None,
            selftest_brushes: None,
            selftest_surface: None,
            selftest_materials: None,
            selftest_boolean: None,
            selftest_document: None,
            selftest_cull: None,
            selftest_splat: None,
            selftest_symmetry: None,
            snapshot_to: None,
            snapshot_ui: false,
            project_path: "untitled.klay".to_string(),
            subtool_path: "subtool.klay".to_string(),
            import_path: "model.obj".to_string(),
            stl_path: "klay_export.stl".to_string(),
            print_report: None,
            print_check_on_start: false,
            file_dialog: None,
            last_dir: None,
            project_has_file: false,
            morph_targets: std::collections::HashMap::new(),
            last_input: Instant::now(),
            last_quicksave: Instant::now(),
            quicksaved_at: (0, 0),
            automated: false,
            open_on_start: None,
            pending_doc: None,
            doc: Multires::new(mesh, budget),
            camera: Camera::new(),
            brush: Brush::default(),
            history: History::new(32),
            stroke: None,
            stroke_brush: Brush::default(),
            tablet: None,
            pressure: None,
            tablet_error: None,
            selftest_pressure: None,
            curve_draw: None,
            curve: None,
            selftest_curve: None,
            selftest_curve_state: CurveTest::default(),
            selftest_tube: None,
            selftest_insert: None,
            selftest_sculptris: None,
            selftest_zmodeler: None,
            selftest_hold: false,
            selftest_clip: None,
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            scale: 1.0,
            nav_press: Vec2::ZERO,
            modifiers: ModifiersState::empty(),
            nav: None,
            select: None,
            wedge: wedge::Settings::default(),
            wedge_info: None,
            hover: None,
            action_line: None,
            transpose: None,
            insert: None,
            zmodel: None,
            zm_hover: None,
            zm_drag: None,
            zm_bridge: None,
            sculptris_before: None,
            sculptris_track: (Vec::new(), Vec::new()),
            sculptris_capped: false,
            sculptris_changed: false,
            sculptris_at: Vec::new(),
            bool_volumes: std::collections::HashMap::new(),
            bool_refused: false,
            gizmo_place: None,
            mask_press: None,
            mask_stroke: None,
            gesture: None,
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
            pending_mask: None,
            pending_level: None,
            pending_vis: None,
            pending_geo: None,
            selftest_wedge: None,
            pending_subtool: None,
            measure: None,
            render_triangles: false,
            camera_distance: None,
            quit: false,
            build_ms,
            last_frame: Instant::now(),
            frame_ms: 0.0,
            fps: 0.0,
            stroke_ms: 0.0,
            stroke_pending: false,
            stroke_from: Vec2::ZERO,
            lazy_point: None,
            drag_stamp: None,
            last_dab_count: 0,
            demo,
            demo_brushes: false,
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
        self.stroke.is_some()
            || self.transpose.is_some()
            || self.mask_press.is_some()
            || self.mask_stroke.is_some()
            || self.gesture.is_some()
            || self.select.is_some()
            || self.curve_draw.is_some()
            || self.curve.as_ref().is_some_and(|c| c.bend.is_some())
            || self.insert.is_some()
            || self.zm_drag.is_some()
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
        self.doc.freeze_layers();
        let mut doc = std::mem::replace(&mut self.doc, Self::placeholder());
        let mut doc_layers = std::mem::take(&mut doc.layers);
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
            layers: std::mem::take(&mut doc_layers),
        }));
        t.elapsed().as_secs_f32() * 1000.0
    }

    /// Rebuild the stack of parked subtool `i` and make it the active one. Returns the time
    /// taken in ms.
    fn activate(&mut self, i: usize) -> f32 {
        let t = Instant::now();
        let tool = &mut self.subtools[i];
        let Parked {
            packed, history, layers, ..
        } = *tool.parked.take().expect("only a parked subtool can be activated");
        tool.visible = true;
        let id = tool.id;
        self.doc = packed.unpack();
        self.doc.layers = layers;
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

    /// Add a subtool made from the current primitive next to the visible model and make it
    /// active (ZBrush: Append). It is half the size of a new project's first subtool and
    /// sits to the right of the model's bounds with a small gap.
    fn append_primitive(&mut self) {
        if self.busy() {
            return;
        }
        let bounds = self.model_bounds();
        let budget = self.doc.budget;
        self.park_active();
        let t = Instant::now();
        let size = 0.5;
        let (p, i, g) = self.primitive.build(size);
        let half_x = p.iter().fold(0.0f32, |m, q| m.max(q.x.abs()));
        let center = Vec3::new(
            bounds.max.x + half_x + size * 0.25,
            (bounds.min.y + bounds.max.y) * 0.5,
            (bounds.min.z + bounds.max.z) * 0.5,
        );
        let p = p.into_iter().map(|q| q + center).collect();
        self.doc = Multires::new(SculptMesh::from_raw_with_groups(p, i, g), budget);
        let id = self.new_tool_id();
        let name = format!("{} {id}", self.primitive.kind.short());
        self.subtools.push(SubTool {
            boolean: Default::default(),
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
        let wedge = self.subtools[self.active_tool].wedge;
        self.subtools.insert(
            self.active_tool + 1,
            SubTool {
            boolean: Default::default(),
                id,
                name: name.clone(),
                visible: true,
                bounds: stand_in.bounds,
                wedge,
                parked: Some(Box::new(Parked {
                    packed,
                    history: History::new(32),
                    stand_in_vertices,
                    layers: Default::default(),
                })),
            },
        );
        self.workspace.notify(format!("Duplicated as {name}"));
    }

    /// Whether the active subtool has no subdivision levels, as SubTool › Split and Merge
    /// need (it says so when it has).
    fn without_levels(&mut self, what: &str) -> bool {
        if self.doc.count() > 1 {
            self.workspace.notify(format!("{what} needs a subtool without subdivision levels · Geometry: Del Higher / Del Lower"));
            return false;
        }
        true
    }

    /// A change of topology on a subtool without levels, as one undo step (the old stack is
    /// kept packed, as for Remesh). `f` gives the new mesh, or `None` when there is nothing
    /// to do.
    fn topology_op(&mut self, what: &str, f: impl FnOnce(&SculptMesh) -> Option<SculptMesh>) {
        if self.busy() || !self.without_levels(what) {
            return;
        }
        if !self.doc.layers.list.is_empty() {
            self.workspace.notify(format!("{what}: bake or delete the layers first · Mesh › Layers"));
            return;
        }
        let Some(mesh) = f(self.doc.mesh()) else {
            self.workspace.notify(format!("{what}: nothing to do on this subtool"));
            return;
        };
        let (before, after) = (self.doc.mesh().triangle_count(), mesh.triangle_count());
        let budget = self.doc.budget;
        let old = std::mem::replace(&mut self.doc, Multires::new(mesh, budget));
        self.history.push(UndoStep::Replace(Box::new(old.pack())));
        self.history.name_last(what);
        self.morph_targets.remove(&self.subtools[self.active_tool].id);
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
        self.workspace.notify(format!("{what}: {} → {} triangles", fmt_count(before), fmt_count(after)));
    }

    /// Geometry › ZRemesher: the active subtool rebuilt as quads (one undo step).
    fn zremesh_now(&mut self) {
        if self.busy() || !self.without_levels("ZRemesher") {
            return;
        }
        let settings = self.workspace.zremesh;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| zremesh::zremesh(self.doc.mesh(), &settings)));
        let (mesh, stats) = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                self.workspace.notify(format!("ZRemesher: {e}"));
                return;
            }
            Err(_) => {
                self.workspace.notify("ZRemesher failed".to_string());
                return;
            }
        };
        let mut mesh = Some(mesh);
        self.topology_op("ZRemesher", |_| mesh.take());
        self.status = format!("ZRemesher: {} quads in {:.0} ms", fmt_count(stats.quads), stats.ms);
        log::info!("{} ({stats:?})", self.status);
    }

    /// The active subtool becomes `mesh` (a new topology: undo history and its Morph Target
    /// go).
    fn replace_active_mesh(&mut self, mesh: SculptMesh) {
        let budget = self.doc.budget;
        self.doc = Multires::new(mesh, budget);
        self.history = History::new(32);
        let tool = &mut self.subtools[self.active_tool];
        self.morph_targets.remove(&tool.id);
        tool.bounds = self.doc.bounds();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
    }

    /// SubTool › Merge Down: the subtool below joins the active one, which keeps its name;
    /// both keep their polygroups. Not undoable.
    fn merge_down(&mut self) {
        if self.busy() || !self.without_levels("Merge Down") {
            return;
        }
        let below = self.active_tool + 1;
        let Some(parked) = self.subtools.get(below).and_then(|t| t.parked.as_ref()) else {
            self.workspace.notify("Merge Down: there is no subtool below this one");
            return;
        };
        let other = parked.packed.clone().unpack();
        if other.count() > 1 {
            self.workspace.notify(format!("Merge Down: {} has subdivision levels · Del Higher / Del Lower first", self.subtools[below].name));
            return;
        }
        let mesh = subtool_ops::merged(self.doc.mesh(), other.mesh());
        let removed = self.subtools.remove(below);
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.drop_stand_in(removed.id);
        }
        self.morph_targets.remove(&removed.id);
        self.replace_active_mesh(mesh);
        self.workspace.notify(format!("Merged {} into {}", removed.name, self.subtools[self.active_tool].name));
    }

    /// Put `meshes` (name, mesh) in as parked subtools right after the active one.
    fn insert_after_active(&mut self, meshes: Vec<(String, SculptMesh)>) {
        let budget = self.doc.budget;
        for (k, (name, mesh)) in meshes.into_iter().enumerate() {
            let tool = self.parked_subtool(name, true, Multires::new(mesh, budget));
            self.subtools.insert(self.active_tool + 1 + k, tool);
        }
    }

    /// SubTool › Boolean › Make Boolean Mesh: the boolean of every visible subtool (shown
    /// levels, top to bottom) as a new subtool at the end, selected; the subtools it came
    /// from are hidden and Live Boolean goes off. Not undoable (delete the new subtool).
    fn make_boolean_mesh(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let mut unpacked: Vec<(usize, Multires)> = Vec::new();
        for (i, tool) in self.subtools.iter().enumerate() {
            if i == self.active_tool || !tool.visible {
                continue;
            }
            if let Some(p) = &tool.parked {
                match p.packed.clone().try_unpack() {
                    Ok(doc) => unpacked.push((i, doc)),
                    Err(e) => {
                        self.workspace.notify(format!("Make Boolean Mesh: {}: {e}", tool.name));
                        return;
                    }
                }
            }
        }
        let mut parts: Vec<(&SculptMesh, boolean::BoolOp)> = Vec::new();
        let mut used = Vec::new();
        for (i, tool) in self.subtools.iter().enumerate() {
            let mesh = if i == self.active_tool {
                Some(self.doc.mesh())
            } else {
                unpacked.iter().find(|(j, _)| *j == i).map(|(_, d)| d.mesh())
            };
            if let Some(mesh) = mesh {
                parts.push((mesh, tool.boolean));
                used.push(tool.id);
            }
        }
        let resolution = self.workspace.boolean_resolution.round() as u32;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boolean::make_mesh(&parts, resolution)));
        drop(parts);
        let (mesh, _) = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                self.workspace.notify(format!("Make Boolean Mesh: {e}"));
                return;
            }
            Err(_) => {
                self.workspace.notify("Make Boolean Mesh failed".to_string());
                return;
            }
        };
        let tris = mesh.triangle_count();
        let n = self.subtools.iter().filter(|t| t.name.starts_with("Boolean")).count() + 1;
        let name = format!("Boolean {n}");
        let budget = self.doc.budget;
        let tool = self.parked_subtool(name.clone(), true, Multires::new(mesh, budget));
        self.subtools.push(tool);
        self.select_subtool(self.subtools.len() - 1);
        for tool in &mut self.subtools {
            if used.contains(&tool.id) {
                tool.visible = false;
            }
        }
        self.workspace.live_boolean = false;
        self.status = format!("{name}: {} triangles from {} subtools in {:.0} ms", fmt_count(tris), used.len(), t.elapsed().as_secs_f32() * 1000.0);
        self.workspace.notify(format!("{} · the subtools it came from are hidden", self.status));
    }

    /// SubTool › Split Hidden: the hidden polygons become a subtool of their own. Not undoable.
    fn split_hidden(&mut self) {
        if self.busy() || !self.without_levels("Split Hidden") {
            return;
        }
        let mesh = self.doc.mesh();
        if mesh.hidden.is_empty() {
            self.workspace.notify("Split Hidden: nothing is hidden");
            return;
        }
        let (Some(shown), Some(hidden)) = (subtool_ops::part(mesh, |t| mesh.visible(t)), subtool_ops::part(mesh, |t| !mesh.visible(t))) else {
            self.workspace.notify("Split Hidden: everything is hidden");
            return;
        };
        let name = format!("{} hidden", self.subtools[self.active_tool].name);
        self.replace_active_mesh(shown);
        self.insert_after_active(vec![(name.clone(), hidden)]);
        self.workspace.notify(format!("Split the hidden polygons into {name}"));
    }

    /// SubTool › Groups Split: each polygroup becomes a subtool; the first stays active. Not
    /// undoable.
    fn groups_split(&mut self) {
        if self.busy() || !self.without_levels("Groups Split") {
            return;
        }
        let mesh = self.doc.mesh();
        let mut ids: Vec<u16> = (0..mesh.triangle_count() as u32).map(|t| mesh.group(t)).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() < 2 {
            self.workspace.notify("Groups Split: this subtool is one polygroup");
            return;
        }
        let base = self.subtools[self.active_tool].name.clone();
        let mut parts: Vec<(String, SculptMesh)> = ids
            .iter()
            .filter_map(|&g| subtool_ops::part(mesh, |t| mesh.group(t) == g).map(|m| (format!("{base} {g}"), m)))
            .collect();
        let first = parts.remove(0).1;
        let count = parts.len() + 1;
        self.replace_active_mesh(first);
        self.insert_after_active(parts);
        self.workspace.notify(format!("Split into {count} subtools, one per polygroup"));
    }

    /// SubTool › Extract: the masked surface of the shown level as a new shell subtool,
    /// Thick (a share of the subtool's size) thick.
    fn extract(&mut self) {
        if self.busy() {
            return;
        }
        let b = self.doc.bounds();
        let thickness = (b.max - b.min).max_element() * self.workspace.extract_thick / 100.0;
        let Some(shell) = subtool_ops::extracted(self.doc.mesh(), thickness) else {
            self.workspace.notify("Extract: mask the part to extract first (Ctrl + drag)");
            return;
        };
        let name = format!("{} extract", self.subtools[self.active_tool].name);
        self.insert_after_active(vec![(name.clone(), shell)]);
        self.workspace.notify(format!("Extracted {name}"));
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
        if !kind.is_map() {
            // Reshape the shown level as one stroke (other levels follow, undo as usual).
            let Some(new) = deform::shaped(self.doc.mesh(), kind, value) else {
                self.workspace.notify(format!("{}: nothing to move", kind.label()));
                return;
            };
            let level = self.doc.active();
            let mesh = self.doc.mesh_mut();
            let all: Vec<u32> = (0..mesh.chunks.len() as u32).collect();
            let mut stroke = Stroke::begin(false, false, false);
            stroke.snapshot_now(mesh, &all);
            mesh.positions.copy_from_slice(&new);
            mesh.finish_edit(&all);
            self.commit_stroke_named(stroke, level, kind.label());
            self.subtools[self.active_tool].bounds = self.doc.bounds();
            self.update_hover();
            self.workspace.notify(format!("{} {:+.0} applied", kind.label(), value));
            return;
        }
        self.apply_transform(subtool::deform_map(kind, value, self.doc.bounds(), self.workspace.size_axes));
        self.workspace
            .notify(format!("{} {:+.0} applied", kind.label(), value));
    }

    /// Map every level of the active subtool by `map` as one undo step. Returns the time
    /// taken in ms (the GPU copy of the shown level is re-uploaded by the next frame).
    fn apply_transform(&mut self, map: Affine) -> f32 {
        let t = Instant::now();
        let mesh = self.doc.mesh();
        let hidden = !mesh.shown.is_empty();
        if hidden || mesh.mask.iter().any(|&m| m > 0) {
            // ZBrush: masked vertices stay; partly masked ones move part of the way. So do
            // the vertices of hidden polygons only: Transpose moves what is shown.
            let level = self.doc.active();
            let mask: Vec<u8> = if hidden {
                (0..mesh.vertex_count())
                    .into_par_iter()
                    .map(|v| if mesh.shown[v] == 0 { 255 } else { mesh.mask.get(v).copied().unwrap_or(0) })
                    .collect()
            } else {
                mesh.mask.clone()
            };
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
                gfx.rescale_normals(map.area_factor());
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
        let inner = self.canvas_rect.shrink(self.workspace.layout.document.inset * self.scale);
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
                    squash: mode == Mode::Scale && hit == Hit::Inner(Part::Middle),
                    anchor,
                    press,
                    line,
                    map: Affine::IDENTITY,
                };
                let drag_line = |ends| TransposeDrag::Line { line, press, ends };
                // ZBrush: the middle circle moves (Scale: squashes or stretches along the
                // line), the end circle scales or rotates about the start (the start circle
                // about the end); the rings and the line move the line itself.
                let drag = match (mode, hit) {
                    (Mode::Move | Mode::Scale, Hit::Inner(Part::Middle)) => act(line.mid()),
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
            TransposeDrag::Gizmo { handle, place, press, map, alone } => {
                *map = gizmo::drag_map(&view, place, *handle, *press, to);
                if *alone {
                    // The gizmo alone only moves and turns, about its own centre.
                    map.scale = 1.0;
                    map.stretch = 1.0;
                }
            }
            TransposeDrag::Act {
                mode,
                squash,
                anchor,
                press,
                line,
                map,
            } => {
                *map = match mode {
                    Mode::Move => transform::move_map(&view, *anchor, *press, to),
                    Mode::Scale if *squash => transform::stretch_map(line, *press, to),
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
            TransposeDrag::Gizmo { handle, place, map, alone, .. } => {
                if map.is_identity() {
                    return None;
                }
                let id = self.subtools[self.active_tool].id;
                if alone {
                    self.gizmo_place = Some((id, place.mapped(&map)));
                    self.status = "Gizmo moved".into();
                    return None;
                }
                // The gizmo stays on the model: carried along and turned with it.
                if self.gizmo_place.is_some() || map.rotation != glam::Quat::IDENTITY {
                    self.gizmo_place = Some((id, place.mapped(&map)));
                }
                let ms = self.apply_transform(map);
                self.status = format!("Gizmo {handle:?} applied in {ms:.0} ms");
                log::info!("{}", self.status);
                Some(ms)
            }
            TransposeDrag::Act { mode, line, map, .. } => {
                if map.is_identity() {
                    return None;
                }
                let ms = self.apply_transform(map);
                self.action_line = Some(line.mapped(&map));
                self.status = match mode {
                    Mode::Move => format!("Moved by {:.3} in {ms:.0} ms", map.translate.length()),
                    Mode::Scale if map.stretch != 1.0 => format!("Squashed along the line to {:.3} in {ms:.0} ms", map.stretch),
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
            Some(TransposeDrag::Act { map, .. }) | Some(TransposeDrag::Gizmo { map, alone: false, .. }) => map,
            _ => Affine::IDENTITY,
        }
    }

    /// Where Gizmo 3D stands: where it was put for the active subtool, else its centre.
    fn gizmo_place(&self) -> gizmo::Place {
        match self.gizmo_place {
            Some((id, place)) if id == self.subtools[self.active_tool].id => place,
            _ => {
                let b = self.doc.bounds();
                gizmo::Place::at((b.min + b.max) * 0.5)
            }
        }
    }

    #[cfg(test)]
    fn gizmo_centre(&self) -> Vec3 {
        self.gizmo_place().centre
    }

    /// Gizmo 3D's buttons: to the centre of what is unmasked and shown (keeping its turn),
    /// or back to the subtool's centre, world-aligned.
    fn gizmo_pivot(&mut self, to_unmasked: bool) {
        if !to_unmasked {
            self.gizmo_place = None;
            self.workspace.notify("Gizmo reset: subtool centre, world axes".to_string());
            return;
        }
        let mesh = self.doc.mesh();
        let free = |v: usize| mesh.mask.get(v).is_none_or(|&m| m < 128) && mesh.shown.get(v).is_none_or(|&s| s != 0);
        let (sum, n) = (0..mesh.vertex_count()).filter(|&v| free(v)).fold((Vec3::ZERO, 0usize), |(s, n), v| (s + mesh.positions[v], n + 1));
        if n == 0 {
            self.workspace.notify("Everything is masked".to_string());
            return;
        }
        let place = gizmo::Place { centre: sum / n as f32, rotation: self.gizmo_place().rotation };
        self.gizmo_place = Some((self.subtools[self.active_tool].id, place));
        self.workspace.notify("Gizmo at the unmasked centre".to_string());
    }

    /// A press in Move / Scale / Rotate mode with Gizmo 3D on: start dragging the handle
    /// under it (with Alt, the gizmo alone). Alt + click on the model puts the gizmo there,
    /// turned to the surface. Returns false when it is on neither (the caller navigates).
    fn begin_gizmo(&mut self) -> bool {
        let place = self.gizmo_place();
        let press = self.canvas_cursor();
        let alone = self.modifiers.alt_key();
        let Some(handle) = gizmo::hit(&self.view(), &place, press) else {
            if alone {
                let (o, d) = self.cursor_ray();
                if let Some(hit) = self.mesh().raycast(o, d) {
                    let normal = if hit.normal.dot(d) > 0.0 { -hit.normal } else { hit.normal };
                    self.gizmo_place = Some((self.subtools[self.active_tool].id, gizmo::Place::on_surface(hit.point, normal)));
                    self.status = "Gizmo put on the surface".into();
                    return true;
                }
            }
            return false;
        };
        self.transpose = Some(TransposeDrag::Gizmo { handle, place, press, map: Affine::IDENTITY, alone });
        self.hover = None;
        true
    }

    /// ZModeler: what the press would act on, outlined (ZBrush lights the target the
    /// same way): the polygons, the edge (and the loop Insert would cut or Delete would
    /// take), or the point.
    fn paint_zmodel(&self, ctx: &egui::Context) {
        if !self.brush.kind.is_zmodeler() || self.zm_drag.is_some() || self.workspace.mode != Mode::Draw {
            return;
        }
        let (Some(hover), Some(z)) = (self.zm_hover, self.zmodel.as_ref()) else { return };
        // Bridge's first polygon stays lit.
        let first = self.zm_bridge.as_ref().and_then(|v| z.poly.face_with(v));
        let view = self.view();
        let to_pos = |p: Vec3| {
            let px = view.project(p) + self.canvas_origin();
            egui::pos2(px.x / self.scale, px.y / self.scale)
        };
        let painter = ctx
            .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("zmodeler")))
            .with_clip_rect(self.workspace.canvas);
        let lit = egui::Color32::from_rgb(255, 214, 92);
        let stroke = egui::Stroke::new(2.0, lit);
        let pm = &z.poly;
        let outline = |f: u32| {
            let fv = &pm.faces[f as usize];
            let mut pts: Vec<egui::Pos2> = fv.iter().map(|&v| to_pos(pm.pos[v as usize])).collect();
            pts.push(pts[0]);
            painter.add(egui::Shape::line(pts, stroke));
        };
        if let Some(f) = first {
            outline(f);
        }
        match hover {
            zmodeler::Hover::Poly(f) => {
                let at = self.canvas_cursor();
                let face = &pm.faces[f as usize];
                let n = face.len();
                let k = (0..n)
                    .min_by(|&a, &b| {
                        let mid = |k: usize| view.project((pm.pos[face[k] as usize] + pm.pos[face[(k + 1) % n] as usize]) * 0.5);
                        mid(a).distance(at).total_cmp(&mid(b).distance(at))
                    })
                    .unwrap_or(0);
                let faces = pm.target(f, self.workspace.zm_target, Some((face[(k + 1) % n], face[(k + 2) % n])));
                for g in faces.into_iter().take(4000) {
                    outline(g);
                }
            }
            zmodeler::Hover::Edge(a, b) => {
                let wide = egui::Stroke::new(3.0, lit);
                match self.workspace.zm_edge {
                    zmodeler::EdgeAction::Insert => {
                        // The loop to be cut: across the ring's quads, through the middles.
                        for (f, k) in pm.ring(a, b) {
                            let fv = &pm.faces[f as usize];
                            let p = |i: usize| pm.pos[fv[(k + i) % 4] as usize];
                            painter.line_segment([to_pos((p(0) + p(1)) * 0.5), to_pos((p(2) + p(3)) * 0.5)], stroke);
                        }
                    }
                    zmodeler::EdgeAction::Spin => {}
                    zmodeler::EdgeAction::Delete | zmodeler::EdgeAction::Bevel => {
                        let whole = self.workspace.zm_edge == zmodeler::EdgeAction::Bevel || self.workspace.zm_edge_loop;
                        let edges = if whole { pm.edge_loop(a, b) } else { vec![(a, b)] };
                        for (x, y) in edges {
                            painter.line_segment([to_pos(pm.pos[x as usize]), to_pos(pm.pos[y as usize])], wide);
                        }
                    }
                }
                painter.line_segment([to_pos(pm.pos[a as usize]), to_pos(pm.pos[b as usize])], wide);
            }
            zmodeler::Hover::Point(v) => {
                painter.circle_stroke(to_pos(pm.pos[v as usize]), 6.0, stroke);
            }
        }
    }

    /// Draw Gizmo 3D: X red, Y green, Z blue; the handle under the cursor (or being dragged)
    /// is lit.
    fn paint_gizmo(&self, ctx: &egui::Context) {
        if !self.workspace.gizmo || self.workspace.mode == Mode::Draw || !self.canvas_rect.is_positive() {
            return;
        }
        let view = self.view();
        let (place, active) = match self.transpose {
            Some(TransposeDrag::Gizmo { handle, place, map, .. }) => (place.mapped(&map), Some(handle)),
            _ => (self.gizmo_place(), None),
        };
        let g = &place;
        let centre = place.centre;
        let lit = active.or_else(|| gizmo::hit(&view, g, self.canvas_cursor()));
        let to_pos = |p: Vec2| {
            let px = p + self.canvas_origin();
            egui::pos2(px.x / self.scale, px.y / self.scale)
        };
        let painter = ctx
            .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("gizmo")))
            .with_clip_rect(self.workspace.canvas);
        let colors = [egui::Color32::from_rgb(232, 72, 64), egui::Color32::from_rgb(110, 205, 90), egui::Color32::from_rgb(70, 140, 240)];
        let width = |h: gizmo::Handle| if lit == Some(h) { 3.0 } else { 1.6 };
        let shade = |c: egui::Color32, h: gizmo::Handle| if lit == Some(h) { egui::Color32::from_rgb(255, 230, 120) } else { c };
        let line = |pts: Vec<Vec2>, stroke: egui::Stroke| {
            painter.add(egui::Shape::line(pts.into_iter().map(to_pos).collect(), stroke));
        };
        let white = egui::Color32::from_rgb(235, 235, 235);
        line(gizmo::ring(&view, g, None, gizmo::VIEW_RING), egui::Stroke::new(width(gizmo::Handle::RotateView), shade(white, gizmo::Handle::RotateView)));
        for a in 0..3 {
            let h = gizmo::Handle::Rotate(a);
            line(gizmo::ring(&view, g, Some(a), gizmo::RING), egui::Stroke::new(width(h), shade(colors[a], h)));
        }
        let c = view.project(centre);
        for a in 0..3 {
            let h = gizmo::Handle::Move(a);
            let tip = gizmo::arrow_tip(&view, g, a);
            let color = shade(colors[a], h);
            line(vec![c, tip], egui::Stroke::new(width(h) + 0.6, color));
            painter.circle_filled(to_pos(tip), 4.5, color);
            let s = gizmo::Handle::Stretch(a);
            let half = gizmo::AXIS_BOX / self.scale;
            let at = to_pos(gizmo::axis_box(&view, g, a));
            painter.rect_filled(egui::Rect::from_center_size(at, egui::vec2(half * 2.0, half * 2.0)), 1, shade(colors[a], s));
            painter.rect_stroke(egui::Rect::from_center_size(at, egui::vec2(half * 2.0, half * 2.0)), 1, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)), egui::StrokeKind::Outside);
        }
        let free = gizmo::Handle::Free;
        painter.circle_stroke(to_pos(c), gizmo::FREE_RING / self.scale, egui::Stroke::new(width(free), shade(white, free)));
        let scale = gizmo::Handle::Scale;
        let half = gizmo::SCALE_BOX / self.scale;
        painter.rect_filled(egui::Rect::from_center_size(to_pos(c), egui::vec2(half * 2.0, half * 2.0)), 1, shade(egui::Color32::from_rgb(240, 200, 60), scale));
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
    /// The shader's `material` uniform: a MatCap's unpainted colour and index, or w = -1
    /// for the procedural clays.
    fn material_uniform(&self) -> [f32; 4] {
        let material = self.workspace.material;
        match matcap::of_material(material) {
            Some(look) => look.base().extend((material - matcap::FIRST) as f32).to_array(),
            None => [0.0, 0.0, 0.0, -1.0],
        }
    }

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

    /// Live Boolean: bring each operand's volume up to date and hand them to the renderer.
    /// Returns (subtool, operand slot) for every drawn subtool; empty when the viewport
    /// draws everything as it is (Live Boolean off, Solo, or nothing but Add).
    fn update_live_boolean(&mut self) -> Vec<(usize, usize)> {
        let operands: Vec<usize> = (0..self.subtools.len()).filter(|&i| i == self.active_tool || self.subtools[i].visible).collect();
        let wanted = self.workspace.live_boolean && !self.workspace.solo && operands.iter().any(|&i| self.subtools[i].boolean != boolean::BoolOp::Add);
        if !wanted || operands.len() > render::BOOL_MAX {
            if wanted && !self.bool_refused {
                self.bool_refused = true;
                self.workspace.notify(format!("Live Boolean shows at most {} visible subtools · hide some", render::BOOL_MAX));
            }
            if let Some(gfx) = self.gfx.as_mut() {
                gfx.clear_boolean();
            }
            return Vec::new();
        }
        self.bool_refused = false;
        // The active subtool's version: its stack and every edit to it.
        let active_id = self.subtools[self.active_tool].id;
        let version = self.doc.generation.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.history.edits() ^ (self.doc.active() as u64) << 56;
        let stale = self.bool_volumes.get(&active_id).is_none_or(|(v, _)| *v != version);
        // Not while a stroke or drag is under way: the old volume stands in until it ends.
        if stale && (!self.busy() || !self.bool_volumes.contains_key(&active_id)) {
            let vol = wedge::distance_volume(self.doc.mesh(), render::BOOL_VOLUME);
            self.bool_volumes.insert(active_id, (version, vol));
        }
        for &i in &operands {
            let tool = &self.subtools[i];
            if i == self.active_tool || self.bool_volumes.contains_key(&tool.id) {
                continue;
            }
            // A parked subtool never seen: its shown level, once.
            if let Some(Ok(doc)) = tool.parked.as_ref().map(|p| p.packed.clone().try_unpack()) {
                let vol = wedge::distance_volume(doc.mesh(), render::BOOL_VOLUME);
                self.bool_volumes.insert(tool.id, (0, vol));
            }
        }
        let inv = self.preview_map().inverse().matrix();
        let mut list = Vec::new();
        let mut slots = Vec::new();
        let mut active_slot = None;
        for &i in &operands {
            let tool = &self.subtools[i];
            if let Some((v, vol)) = self.bool_volumes.get(&tool.id) {
                if i == self.active_tool {
                    active_slot = Some(list.len());
                }
                slots.push((i, list.len()));
                list.push((tool.id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ *v, vol, tool.boolean));
            }
        }
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.set_boolean(&list, active_slot, inv);
        }
        slots
    }

    /// Bounds of what is drawn: the active subtool and the visible parked ones.
    fn model_bounds(&self) -> Aabb {
        self.drawn_parked()
            .iter()
            .fold(self.doc.bounds(), |b, &(_, i)| subtool::union(b, self.subtools[i].bounds))
    }

    /// Every subtool as `(name, visible, packed)` for a project file, with the active one
    /// packed as a copy so it is not disturbed.
    fn with_project_tools<T>(&self, f: impl FnOnce(&[(&str, bool, &multires::Packed)], &[project::Extras]) -> T) -> T {
        let active = self.doc.pack_copy();
        let extras: Vec<project::Extras> = self
            .subtools
            .iter()
            .map(|t| project::Extras {
                layers: Some(match &t.parked {
                    Some(p) => &p.layers,
                    None => &self.doc.layers,
                }),
                morph: self.morph_targets.get(&t.id).map(|(level, pts)| (*level, pts.as_slice())),
                boolean: t.boolean,
            })
            .collect();
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
        f(&tools, &extras)
    }

    /// Put a loaded subtool's layers and Morph Target on `doc` when they fit its levels
    /// (layers: the highest level's vertex count; target: its level's). Returns the target.
    fn attach_saved(doc: &mut Multires, layers: layers::Layers, morph: Option<(usize, Vec<Vec3>)>) -> Option<(usize, std::sync::Arc<Vec<Vec3>>)> {
        let counts = doc.vertex_counts();
        let top = counts.last().copied().unwrap_or(0);
        if layers.list.iter().all(|l| l.delta.len() == top) {
            doc.layers = layers;
        } else {
            log::warn!("layers left out: they do not fit the highest level");
        }
        morph.filter(|(level, pts)| counts.get(*level) == Some(&pts.len())).map(|(l, p)| (l, std::sync::Arc::new(p)))
    }

    /// The whole model as project bytes, in memory (tests and the self-test).
    fn project_bytes(&self) -> Vec<u8> {
        self.with_project_tools(|tools, extras| project::write_with(Vec::new(), tools, extras, self.active_tool))
            .expect("writing to memory")
    }

    /// Save (Ctrl + S): to the project's file, or ask for one when it has none yet.
    fn save_or_ask(&mut self) {
        if self.project_has_file {
            self.pending_doc = Some(DocOp::Save);
        } else {
            self.ask_file(file_dialog::Ask::SaveProject);
        }
    }

    /// Open the desktop's file dialog for `ask`, starting in the folder of the file it is
    /// about; [`answer_file_dialog`](Self::answer_file_dialog) acts on the file picked.
    /// Scripted runs and tests have no dialogs and use the stored paths.
    fn ask_file(&mut self, ask: file_dialog::Ask) {
        use file_dialog::Ask;
        if self.automated || self.window.is_none() {
            self.pending_doc = Some(match ask {
                Ask::OpenProject => DocOp::Open,
                Ask::SaveProject => DocOp::Save,
                Ask::SaveSubtool => DocOp::SaveSubtool,
                Ask::LoadSubtools => DocOp::LoadSubtools,
                Ask::ExportSubtoolObj => DocOp::ExportSubtool,
                Ask::ImportMesh => DocOp::ImportMesh,
                Ask::ExportStl => DocOp::ExportStl,
                Ask::ExportObj => {
                    self.pending_export = true;
                    return;
                }
            });
            return;
        }
        if self.file_dialog.is_some() {
            self.workspace.notify("A file dialog is already open");
            return;
        }
        let file_of = |path: &str| Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned());
        let project_stem = Path::new(&self.project_path).file_stem().map_or_else(|| "untitled".into(), |s| s.to_string_lossy().into_owned());
        let tool = self.subtools[self.active_tool].name.clone();
        let (near, name) = match ask {
            Ask::OpenProject => (&self.project_path, String::new()),
            Ask::SaveProject => (&self.project_path, file_of(&self.project_path).unwrap_or_else(|| "untitled.klay".into())),
            Ask::SaveSubtool | Ask::LoadSubtools => (&self.subtool_path, file_dialog::file_name(&tool, "klay")),
            Ask::ExportSubtoolObj => (&self.subtool_path, file_dialog::file_name(&tool, "obj")),
            Ask::ExportObj if self.project_has_file => (&self.export_path, file_dialog::file_name(&project_stem, "obj")),
            Ask::ExportObj => (&self.export_path, file_of(&self.export_path).unwrap_or_else(|| "export.obj".into())),
            Ask::ExportStl if self.project_has_file => (&self.stl_path, file_dialog::file_name(&project_stem, "stl")),
            Ask::ExportStl => (&self.stl_path, file_of(&self.stl_path).unwrap_or_else(|| "export.stl".into())),
            Ask::ImportMesh => (&self.import_path, String::new()),
        };
        // A path nothing was picked into yet (`untitled.klay`) starts where the last file was.
        let dir = file_dialog::start_dir(near, self.last_dir.as_deref());
        self.file_dialog = Some(file_dialog::open(ask, &dir, &name, self.window.as_deref()));
    }

    /// Act on the file picked in the open dialog once it closes (nothing when it was
    /// cancelled). Waits while a stroke or gesture is under way.
    fn answer_file_dialog(&mut self) {
        use file_dialog::Ask;
        if self.busy() {
            return;
        }
        let Some(paths) = self.file_dialog.as_ref().and_then(file_dialog::Pending::answer) else {
            return;
        };
        let ask = self.file_dialog.take().expect("an open dialog").ask;
        let Some(first) = paths.first() else {
            return;
        };
        self.last_dir = first.parent().map(Path::to_path_buf);
        let text = |p: &std::path::PathBuf| p.to_string_lossy().into_owned();
        match ask {
            Ask::OpenProject => self.open_project(&text(first)),
            Ask::SaveProject => {
                self.project_path = text(first);
                self.save_project();
            }
            Ask::SaveSubtool => {
                self.subtool_path = text(first);
                self.save_subtool();
            }
            Ask::LoadSubtools => {
                for path in &paths {
                    self.subtool_path = text(path);
                    self.load_subtools();
                }
            }
            Ask::ExportSubtoolObj => {
                self.subtool_path = text(first);
                self.export_subtool_obj();
            }
            Ask::ExportObj => {
                self.export_path = text(first);
                self.export_obj_all();
            }
            Ask::ExportStl => {
                self.stl_path = text(first);
                self.export_stl();
            }
            Ask::ImportMesh => {
                for path in &paths {
                    self.import_path = text(path);
                    self.import_mesh(&text(path));
                }
            }
        }
    }

    /// Write every subtool to `self.project_path` (ZBrush: Save; Ctrl + S).
    fn save_project(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let path = self.project_path.clone();
        self.doc.freeze_layers();
        let result = self.with_project_tools(|tools, extras| project::save_with(Path::new(&path), tools, extras, self.active_tool));
        self.project_has_file |= result.is_ok();
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

    /// Document › Snapshot: ask the renderer for the canvas inside the document frame of
    /// the next frame; `save_snapshot` writes it once drawn.
    fn request_snapshot(&mut self) {
        let inset = self.workspace.layout.document.inset * self.scale;
        let mut r = self.canvas_rect.shrink(inset);
        if self.gfx.as_ref().is_some_and(|g| g.snapshot_ui) {
            let (w, h) = self.gfx.as_ref().map(|g| (g.config.width, g.config.height)).unwrap_or((1, 1));
            r = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32));
        }
        log::debug!("snapshot requested: {r:?}");
        if let Some(gfx) = self.gfx.as_mut() {
            if !gfx.config.usage.contains(wgpu::TextureUsages::COPY_SRC) {
                self.workspace.notify("Snapshot: this GPU's window surface cannot be read back");
                return;
            }
            gfx.snapshot = Some([r.min.x.round() as u32, r.min.y.round() as u32, r.width().round() as u32, r.height().round() as u32]);
        }
    }

    /// Write a drawn snapshot as `<project> snapshot N.png` next to the project file.
    fn save_snapshot(&mut self) {
        // `--selftest-cull` and `--selftest-splat` read their snapshots themselves.
        if self.selftest_cull.is_some() || self.selftest_splat.is_some() {
            return;
        }
        let Some((w, h, rgba)) = self.gfx.as_mut().and_then(|g| g.snapshot_image.take()) else {
            return;
        };
        if let Some((path, _)) = self.snapshot_to.take() {
            match image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8) {
                Ok(()) => log::info!("snapshot {w}×{h} saved to {path}"),
                Err(e) => log::error!("snapshot to {path} failed: {e}"),
            }
            self.quit = true;
            return;
        }
        let project = Path::new(&self.project_path);
        let stem = project.file_stem().map_or_else(|| "untitled".into(), |s| s.to_string_lossy().to_string());
        let dir = project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let path = (1..)
            .map(|n| dir.join(if n == 1 { format!("{stem} snapshot.png") } else { format!("{stem} snapshot {n}.png") }))
            .find(|p| !p.exists())
            .unwrap();
        self.status = match image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8) {
            Ok(()) => format!("Snapshot {w}×{h} saved to {}", path.display()),
            Err(e) => format!("Snapshot failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// The QuickSave folder: `$XDG_DATA_HOME/klay/quicksave`, else
    /// `~/.local/share/klay/quicksave`.
    fn quicksave_dir() -> std::path::PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("klay/quicksave")
    }

    /// QuickSave (ZBrush): every subtool to the oldest of `QuickSave_1.klay` ..
    /// `QuickSave_5.klay` in the QuickSave folder, leaving the project's own file alone.
    fn quicksave(&mut self) {
        self.quicksave_in(&Self::quicksave_dir());
    }

    /// [`quicksave`](Self::quicksave) into `dir`.
    fn quicksave_in(&mut self, dir: &Path) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        self.doc.freeze_layers();
        let result = std::fs::create_dir_all(dir).and_then(|()| {
            let path = (1..=5)
                .map(|n| dir.join(format!("QuickSave_{n}.klay")))
                .min_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
                .unwrap();
            self.with_project_tools(|tools, extras| project::save_with(&path, tools, extras, self.active_tool)).map(|_| path)
        });
        self.last_quicksave = Instant::now();
        self.quicksaved_at = (self.subtools[self.active_tool].id, self.history.edits());
        self.status = match result {
            Ok(path) => format!("QuickSaved to {} in {:.1} s", path.display(), t.elapsed().as_secs_f32()),
            Err(e) => format!("QuickSave failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// QuickSave when it is on, its interval has passed, something changed since the last
    /// one, and nothing has been touched for a few seconds (so it never interrupts a stroke).
    fn quicksave_when_due(&mut self) {
        let ws = &self.workspace;
        let due = ws.quicksave && self.last_quicksave.elapsed().as_secs_f32() >= ws.quicksave_minutes * 60.0;
        let changed = self.quicksaved_at != (self.subtools[self.active_tool].id, self.history.edits()) && self.history.edits() > 0;
        if due && changed && self.last_input.elapsed().as_secs_f32() > 3.0 && !self.busy() && !self.automated {
            self.quicksave();
        }
    }

    /// Write the active subtool on its own, with all its levels, as a one-subtool project
    /// (SubTool › Save). It opens with Open, or joins another model with Load.
    fn save_subtool(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let path = self.subtool_path.clone();
        let name = self.subtools[self.active_tool].name.clone();
        let packed = self.doc.pack_copy();
        self.doc.freeze_layers();
        let extras = [project::Extras {
            layers: Some(&self.doc.layers),
            morph: self.morph_targets.get(&self.subtools[self.active_tool].id).map(|(l, p)| (*l, p.as_slice())),
            boolean: self.subtools[self.active_tool].boolean,
        }];
        self.status = match project::save_with(Path::new(&path), &[(name.as_str(), true, &packed)], &extras, 0) {
            Ok(bytes) => format!(
                "Saved subtool {name} to {path}: {} levels, {:.1} MB in {:.2} s",
                self.doc.count(),
                bytes as f64 / 1e6,
                t.elapsed().as_secs_f32()
            ),
            Err(e) => format!("Save failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// Add every subtool of the project at `subtool_path` to the end of the list, parked,
    /// with its levels (SubTool › Load; ZBrush: Append a saved tool). The active subtool
    /// stays active. Every stack is rebuilt and checked first; a file that fails adds
    /// nothing.
    fn load_subtools(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let path = self.subtool_path.clone();
        #[allow(clippy::type_complexity)]
        let result = (|| -> Result<Vec<(String, bool, Multires, Option<(usize, std::sync::Arc<Vec<Vec3>>)>)>, String> {
            let project = project::open(Path::new(&path))?;
            project
                .tools
                .into_iter()
                .map(|saved| {
                    let mut doc = saved.packed.try_unpack().map_err(|e| format!("subtool {:?}: {e}", saved.name))?;
                    let morph = Self::attach_saved(&mut doc, saved.layers, saved.morph);
                    Ok((saved.name, saved.visible, doc, morph))
                })
                .collect()
        })();
        self.status = match result {
            Ok(docs) => {
                let count = docs.len();
                for (name, visible, doc, morph) in docs {
                    let tool = self.parked_subtool(name, visible, doc);
                    if let Some(m) = morph {
                        self.morph_targets.insert(tool.id, m);
                    }
                    self.subtools.push(tool);
                }
                format!("Loaded {count} subtool{} from {path} in {:.2} s", if count == 1 { "" } else { "s" }, t.elapsed().as_secs_f32())
            }
            Err(e) => format!("Could not load {path}: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// A parked subtool holding `doc`, its stand-in uploaded.
    fn parked_subtool(&mut self, name: String, visible: bool, mut doc: Multires) -> SubTool {
        let id = self.new_tool_id();
        let stand_in = doc.stand_in(STAND_IN_VERTICES);
        let mut stand_in_vertices = stand_in.positions.len();
        if let Some(gfx) = self.gfx.as_mut() {
            if !gfx.upload_stand_in(id, &stand_in) {
                stand_in_vertices = 0;
            }
        }
        SubTool {
            boolean: Default::default(),
            id,
            name,
            visible,
            bounds: stand_in.bounds,
            wedge: false,
            parked: Some(Box::new(Parked {
                layers: std::mem::take(&mut doc.layers),
                packed: doc.pack(),
                history: History::new(32),
                stand_in_vertices,
            })),
        }
    }

    /// The active subtool's Morph Target when it fits the shown level (stored there, same
    /// vertex count): what the Morph brush brushes toward.
    fn morph_target(&self) -> Option<std::sync::Arc<Vec<Vec3>>> {
        let (level, t) = self.morph_targets.get(&self.subtools[self.active_tool].id)?;
        (*level == self.doc.active() && t.len() == self.doc.mesh().vertex_count()).then(|| t.clone())
    }

    /// Mesh › Layers (ZBrush: Tool › Layers). Every change is one undo step; the layers
    /// live on the highest subdivision level, which has to be the one shown.
    fn layer_op(&mut self, op: LayerOp) {
        if self.busy() {
            return;
        }
        if self.doc.active() != self.doc.top() {
            self.workspace.notify("Layers work at the highest subdivision level · D or the SDiv slider");
            return;
        }
        self.doc.freeze_layers();
        let current = self.doc.layers.clone();
        let mut target = current.clone();
        let count = target.list.len();
        let valid = |i: usize| i < count;
        // Removing layer `i` shifts the recording index.
        let without = |t: &mut layers::Layers, i: usize| {
            t.list.remove(i);
            t.rec = match t.rec.take() {
                Some((r, _)) if r == i => None,
                Some((r, b)) if r > i => Some((r - 1, b)),
                other => other,
            };
        };
        let (reshape, name) = match op {
            LayerOp::New => {
                let n = self.doc.mesh().vertex_count();
                let number = (1..).find(|k| !target.list.iter().any(|l| l.name == format!("Layer {k}"))).unwrap();
                target.list.push(layers::SculptLayer { name: format!("Layer {number}"), delta: vec![Vec3::ZERO; n], intensity: 1.0, visible: true });
                target.rec = Some((target.list.len() - 1, Vec::new()));
                self.workspace.layer_selected = target.list.len() - 1;
                (true, "New layer")
            }
            LayerOp::Delete(i) if valid(i) => {
                without(&mut target, i);
                (true, "Delete layer")
            }
            LayerOp::Bake(i) if valid(i) => {
                without(&mut target, i);
                (false, "Bake layer")
            }
            LayerOp::BakeAll if count > 0 => {
                target = layers::Layers::default();
                (false, "Bake all layers")
            }
            LayerOp::Rec(i) if valid(i) => {
                target.rec = if target.rec.as_ref().is_some_and(|(r, _)| *r == i) { None } else { Some((i, Vec::new())) };
                (true, "Record")
            }
            LayerOp::Eye(i) if valid(i) => {
                target.list[i].visible = !target.list[i].visible;
                (true, "Show / hide layer")
            }
            LayerOp::Intensity(i, v) if valid(i) => {
                self.doc.set_layer_intensity(i, v.clamp(-1.0, 2.0));
                self.show_active_level();
                return;
            }
            LayerOp::IntensityDone(i, start) if valid(i) => {
                if (self.doc.layers.list[i].intensity - start).abs() > 1e-6 {
                    self.history.push(UndoStep::LayerIntensity { index: i, intensity: start });
                }
                return;
            }
            _ => return,
        };
        let old = self.doc.swap_layers(target, reshape);
        self.history.push(UndoStep::Layers { state: Box::new(old), reshape });
        self.history.name_last(name);
        let selected = &mut self.workspace.layer_selected;
        *selected = (*selected).min(self.doc.layers.list.len().saturating_sub(1));
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.show_active_level();
    }

    /// Mesh › Morph Target › StoreMT: keep the shown level's shape (ZBrush: StoreMT).
    fn store_morph_target(&mut self) {
        let id = self.subtools[self.active_tool].id;
        let level = self.doc.active();
        self.morph_targets.insert(id, (level, std::sync::Arc::new(self.doc.mesh().positions.clone())));
        self.workspace.notify(format!("Morph Target stored at level {}", level + 1));
    }

    /// Mesh › Morph Target › Switch: swap the shape and the stored Morph Target, undoably.
    fn switch_morph_target(&mut self) {
        if self.busy() {
            return;
        }
        let Some(target) = self.morph_target() else {
            self.workspace.notify("No Morph Target for this level · StoreMT first");
            return;
        };
        let id = self.subtools[self.active_tool].id;
        let level = self.doc.active();
        let mesh = self.doc.mesh_mut();
        let all: Vec<u32> = (0..mesh.chunks.len() as u32).collect();
        let mut stroke = Stroke::begin(false, false, false);
        stroke.snapshot_now(mesh, &all);
        let now = std::mem::replace(&mut mesh.positions, (*target).clone());
        mesh.finish_edit(&all);
        self.morph_targets.insert(id, (level, std::sync::Arc::new(now)));
        self.commit_stroke_named(stroke, level, "Switch MT");
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.update_hover();
        self.workspace.notify("Switched with the Morph Target");
    }

    /// Import the OBJ or STL at `path` (by its extension) as a new subtool after the others
    /// and make it active (Document › Import; ZBrush: Tool › Import into a new SubTool).
    /// STL corners are welded into shared vertices, and with 3D Print › Z Up on the file is
    /// turned from Z up to Klay's Y up. With Fit on it is centred and scaled to the size of a
    /// new primitive, so Draw Size suits it.
    fn import_mesh(&mut self, path: &str) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let is_stl = Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("stl"));
        let parsed = if is_stl {
            let z_up = self.workspace.layout.print.z_up;
            std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}")).and_then(|bytes| stl::parse(&bytes)).map(|mut m| {
                if z_up {
                    for p in &mut m.positions {
                        *p = Vec3::new(p.x, p.z, -p.y);
                    }
                }
                m
            })
        } else {
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}")).and_then(|text| obj::parse(&text))
        };
        let mut m = match parsed {
            Ok(m) => m,
            Err(e) => {
                self.status = format!("Import failed: {e}");
                self.workspace.notify(self.status.clone());
                return;
            }
        };
        let scale = self.workspace.import_fit.then(|| obj::fit(&mut m.positions, 2.0));
        let (verts, tris) = (m.positions.len(), m.indices.len() / 3);
        let mesh = std::panic::catch_unwind(|| SculptMesh::from_raw_painted(m.positions, m.indices, m.groups, m.colors));
        let Ok(mesh) = mesh else {
            self.status = format!("Import failed: {path} is not a mesh Klay can sculpt");
            self.workspace.notify(self.status.clone());
            return;
        };
        let budget = self.doc.budget;
        self.park_active();
        self.doc = Multires::new(mesh, budget);
        let id = self.new_tool_id();
        let name = Path::new(path).file_stem().map_or_else(|| format!("Import {id}"), |s| s.to_string_lossy().to_string());
        self.subtools.push(SubTool { boolean: Default::default(), id, name: name.clone(), visible: true, bounds: self.doc.bounds(), wedge: false, parked: None });
        self.active_tool = self.subtools.len() - 1;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
        self.frame_camera();
        self.status = format!(
            "Imported {name}: {} vertices, {} triangles{}{} in {:.1} s",
            fmt_count(verts),
            fmt_count(tris),
            scale.map_or(String::new(), |s| format!(", scaled ×{s:.3} to fit")),
            match (m.skipped, is_stl) {
                (0, _) => String::new(),
                (n, true) => format!(", {n} flat or repeated triangles left out"),
                (n, false) => format!(", {n} bad faces left out"),
            },
            t.elapsed().as_secs_f32()
        );
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// The active subtool at its shown level as an OBJ, next to `subtool_path` with an
    /// `.obj` extension (SubTool › Export OBJ).
    fn export_subtool_obj(&mut self) {
        let t = Instant::now();
        let path = Path::new(&self.subtool_path).with_extension("obj");
        let name = self.subtools[self.active_tool].name.clone();
        let result = (|| -> std::io::Result<()> {
            let file = std::fs::File::create(&path)?;
            let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
            use std::io::Write;
            writeln!(w, "# Klay export: subtool {name}")?;
            self.doc.mesh().write_obj(&mut w, &name, 0)?;
            w.flush()
        })();
        self.status = match result {
            Ok(()) => format!(
                "Exported {name} ({} vertices) to {} in {:.1} s",
                fmt_count(self.mesh().vertex_count()),
                path.display(),
                t.elapsed().as_secs_f32()
            ),
            Err(e) => format!("Export failed: {e}"),
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
                self.project_has_file = true;
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
        let mut morphs = Vec::new();
        for (i, saved) in project.tools.into_iter().enumerate() {
            let mut doc = match saved.packed.try_unpack() {
                Ok(doc) => doc,
                Err(e) => {
                    failure = Some(format!("subtool {:?}: {e}", saved.name));
                    break;
                }
            };
            let morph = Self::attach_saved(&mut doc, saved.layers, saved.morph);
            if i == active {
                let id = self.new_tool_id();
                staged.push(SubTool {
                    boolean: saved.boolean,
                    id,
                    name: saved.name,
                    visible: true,
                    bounds: doc.bounds(),
                    wedge: false,
                    parked: None,
                });
                active_doc = Some(doc);
            } else {
                let mut tool = self.parked_subtool(saved.name, saved.visible, doc);
                tool.boolean = saved.boolean;
                staged.push(tool);
            }
            if let Some(m) = morph {
                morphs.push((staged.last().unwrap().id, m));
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
        self.morph_targets = morphs.into_iter().collect();
        self.active_tool = active;
        self.doc = doc;
        self.history = History::new(32);
        self.show_active_level();
        self.frame_camera();
        Ok(())
    }

    /// Start over with one subtool made from the current primitive.
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
        let (p, i, g) = self.primitive.build(1.0);
        self.doc = Multires::new(SculptMesh::from_raw_with_groups(p, i, g), budget);
        self.history = History::new(32);
        let id = self.new_tool_id();
        self.subtools = vec![SubTool {
            boolean: Default::default(),
            id,
            name: format!("{} {id}", self.primitive.kind.short()),
            visible: true,
            bounds: self.doc.bounds(),
            wedge: false,
            parked: None,
        }];
        self.active_tool = 0;
        self.project_path = "untitled.klay".to_string();
        self.project_has_file = false;
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

    /// Every visible subtool at its shown level as one binary STL at `path`, placed by the
    /// 3D Print settings (millimetres, Z up). Parked subtools are rebuilt one at a time, as
    /// for OBJ. Returns the subtools and triangles written.
    fn write_stl(&self, path: &Path) -> std::io::Result<(usize, u64)> {
        let settings = &self.workspace.layout.print;
        let place = |p: Vec3| settings.place(p);
        let file = std::fs::File::create(path)?;
        let buf = std::io::BufWriter::with_capacity(8 << 20, file);
        let stem = Path::new(&self.project_path).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let mut out = stl::BinaryWriter::new(buf, &format!("Klay {} {stem} (mm)", env!("CARGO_PKG_VERSION")), 0)?;
        let mut count = 0;
        for (i, tool) in self.subtools.iter().enumerate() {
            if i == self.active_tool {
                let m = self.doc.mesh();
                out.mesh(&m.positions, &m.indices, place)?;
            } else if let (true, Some(p)) = (tool.visible, &tool.parked) {
                let doc = p.packed.clone().unpack();
                out.mesh(&doc.mesh().positions, &doc.mesh().indices, place)?;
            } else {
                continue;
            }
            count += 1;
        }
        let (buf, tris) = out.finish()?;
        let mut file = buf.into_inner().map_err(|e| e.into_error())?;
        stl::patch_count(&mut file, tris)?;
        Ok((count, tris))
    }

    /// Export STL (Document › Export, 3D Print): the visible subtools to `stl_path`.
    fn export_stl(&mut self) {
        let t = Instant::now();
        let path = self.stl_path.clone();
        self.status = match self.write_stl(Path::new(&path)) {
            Ok((count, tris)) => format!(
                "Exported {count} subtools ({} triangles) to {path} in {:.1} s",
                fmt_count(tris as usize),
                t.elapsed().as_secs_f32()
            ),
            Err(e) => format!("Export failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// 3D Print › Send: write the visible subtools as an STL in [`print::send_dir`] and open
    /// it in the chosen slicer (the first one found when none is chosen yet).
    fn send_to_slicer(&mut self) {
        self.send_to_slicer_in(&print::send_dir());
    }

    fn send_to_slicer_in(&mut self, dir: &Path) {
        if self.busy() {
            return;
        }
        let (name, exec) = match self.workspace.chosen_slicer() {
            Some(s) => s,
            None => {
                self.status = "Send: no slicer found; pick Custom and type its command".into();
                self.workspace.notify(self.status.clone());
                return;
            }
        };
        let stem = Path::new(&self.project_path).file_stem().map_or_else(|| "klay".into(), |s| s.to_string_lossy().into_owned());
        let path = dir.join(file_dialog::file_name(&stem, "stl"));
        let result = std::fs::create_dir_all(dir)
            .and_then(|()| self.write_stl(&path))
            .and_then(|(_, tris)| print::launch(&print::command_for(&exec, &path)).map(|()| tris));
        self.status = match result {
            Ok(tris) => format!("Sent {} triangles to {name}: {}", fmt_count(tris as usize), path.display()),
            Err(e) => format!("Send to {name} failed: {e}"),
        };
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// The size of the box around the visible subtools (what Export STL writes), `None`
    /// when nothing is shown.
    fn print_extent(&self) -> Option<Vec3> {
        let mut b = Aabb::empty();
        for (i, t) in self.subtools.iter().enumerate() {
            let tb = if i == self.active_tool { self.doc.bounds() } else if t.visible { t.bounds } else { continue };
            b.grow(tb.min);
            b.grow(tb.max);
        }
        (b.max.cmpge(b.min).all()).then(|| b.max - b.min)
    }

    /// What the 3D Print check result is for: the subtools, which are shown, the active one,
    /// its level and its edits.
    fn print_stamp(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in &self.subtools {
            (t.id, t.visible).hash(&mut h);
        }
        (self.active_tool, self.doc.active(), self.history.edits()).hash(&mut h);
        h.finish()
    }

    /// 3D Print › Check: holes, non-manifold and flipped edges and shells of every visible
    /// subtool at its shown level (what Export STL writes).
    fn print_check(&mut self) {
        let t = Instant::now();
        let mut report = print::Report { stamp: self.print_stamp(), ..Default::default() };
        for (i, tool) in self.subtools.iter().enumerate() {
            let check = if i == self.active_tool {
                print::check(&self.doc.mesh().indices)
            } else if let (true, Some(p)) = (tool.visible, &tool.parked) {
                print::check(&p.packed.clone().unpack().mesh().indices)
            } else {
                continue;
            };
            report.total.add(&check);
            report.booleans += (tool.boolean != boolean::BoolOp::Add) as usize;
            report.rows.push((tool.name.clone(), check));
        }
        let c = &report.total;
        self.status = if c.watertight() {
            format!("Check: watertight, {} triangles, {} shells ({:.2} s)", fmt_count(c.triangles), c.shells, t.elapsed().as_secs_f32())
        } else {
            format!(
                "Check: {} holes, {} open, {} non-manifold, {} flipped edges ({:.2} s)",
                c.holes,
                c.open_edges,
                c.non_manifold,
                c.flipped,
                t.elapsed().as_secs_f32()
            )
        };
        log::info!("{}", self.status);
        self.print_report = Some(report);
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

    /// Preferences › Config › Store Config: every subtool to `path`, which Klay opens at
    /// start; the project's own path stays as it is.
    fn save_startup_doc(&mut self, path: &Path) {
        if self.busy() {
            self.workspace.notify("Startup document: wait for the current task to finish");
            return;
        }
        self.doc.freeze_layers();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let result = self.with_project_tools(|tools, extras| project::save_with(path, tools, extras, self.active_tool));
        self.workspace.notify(match result {
            Ok(bytes) => format!("Config stored with the model as the startup document ({:.1} MB)", bytes as f64 / 1e6),
            Err(e) => format!("Startup document not saved: {e}"),
        });
    }

    /// F11: the window fills the screen (borderless), or goes back to a window.
    fn toggle_fullscreen(&mut self) {
        if let Some(window) = self.window.as_ref() {
            let full = window.fullscreen().is_some();
            window.set_fullscreen((!full).then_some(winit::window::Fullscreen::Borderless(None)));
            self.workspace.notify(if full { "Window" } else { "Fullscreen · F11 to leave" });
        }
    }

    /// Select Rect / Select Lasso: keep the triangles marked in `inside` shown and hide the
    /// rest, or with `hide` (Alt) hide them. An outline around no visible polygon inverts
    /// visibility, as Ctrl + Shift does.
    fn select_tris(&mut self, inside: Vec<u8>, hide: bool, what: &str) {
        let mesh = self.doc.mesh();
        if inside.len() != mesh.triangle_count() {
            return;
        }
        let any = inside.par_iter().enumerate().any(|(t, &i)| i != 0 && mesh.visible(t as u32));
        if !any {
            self.invert_visibility();
            return;
        }
        let hidden: Vec<u8> = inside
            .par_iter()
            .enumerate()
            .map(|(t, &i)| (!mesh.visible(t as u32) || (i != 0) == hide) as u8)
            .collect();
        self.apply_hidden(hidden, what);
    }

    /// Trim Rect / Circle / Lasso: keep what the outline covers through the whole model (or,
    /// with Alt, cut it away), by the boolean remesher at Make Boolean Mesh's resolution. With
    /// Alt and X symmetry the mirrored outline cuts too.
    fn trim_shape(&mut self, g: &surface_tools::Gesture) {
        if g.tiny() {
            return;
        }
        let screen = self.surface_screen();
        let mesh = self.doc.mesh();
        let (lo, hi) = mesh
            .positions
            .par_iter()
            .filter_map(|&p| screen.ndc_z(p))
            .fold(|| (f32::MAX, f32::MIN), |(a, b), z| (a.min(z), b.max(z)))
            .reduce(|| (f32::MAX, f32::MIN), |(a, b), (c, d)| (a.min(c), b.max(d)));
        if lo > hi {
            return;
        }
        // A little in front of the model and behind it (depth runs 0 near to 1 far).
        let margin = ((hi - lo) * 0.25).max(1e-4);
        let (near, far) = ((lo - margin).max(lo * 0.5), (hi + margin).min(0.999_999).max(hi));
        let Some(prism) = cut::prism(&g.outline(), |s, z| screen.unproject(s, z), near, far) else {
            return;
        };
        let op = if g.alternate { boolean::BoolOp::Subtract } else { boolean::BoolOp::Intersect };
        let mirror = (g.alternate && self.workspace.symmetry).then(|| {
            let positions: Vec<Vec3> = prism.positions.iter().map(|p| Vec3::new(-p.x, p.y, p.z)).collect();
            let mut indices = prism.indices.clone();
            for t in indices.chunks_mut(3) {
                t.swap(1, 2);
            }
            SculptMesh::from_raw(positions, indices)
        });
        let resolution = self.workspace.boolean_resolution.round() as u32;
        let name = g.kind.name();
        self.topology_op(name, |m| {
            let mut parts = vec![(m, boolean::BoolOp::Add), (&prism, op)];
            if let Some(mirror) = mirror.as_ref() {
                parts.push((mirror, boolean::BoolOp::Subtract));
            }
            boolean::make_mesh(&parts, resolution).ok().map(|r| r.0)
        });
    }

    /// Set the polygroups of the shown level (and through it every level) to `groups`,
    /// undoably.
    fn apply_groups(&mut self, groups: Vec<u16>, what: &str) {
        let t = Instant::now();
        let before = self.doc.group_runs();
        let level = self.doc.active();
        self.doc.set_groups(level, groups);
        self.history.push(UndoStep::Groups(before));
        self.status = format!(
            "{what}: {} polygroups ({:.0} ms over {} levels)",
            self.doc.mesh().group_count(),
            t.elapsed().as_secs_f32() * 1000.0,
            self.doc.count()
        );
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// Make one new polygroup of the visible polygons (ZBrush: Ctrl + W, Group Visible).
    /// With a mask, make one new polygroup of the visible masked polygons and clear the
    /// mask (ZBrush: Ctrl + W, Group Masked); without one, of the visible polygons (Group
    /// Visible). One undo step either way.
    fn group_visible(&mut self) {
        let t = Instant::now();
        let mesh = self.doc.mesh();
        let masked = mesh.mask.par_iter().any(|&m| m > 127);
        let next = mesh.groups.par_iter().copied().max().map_or(1, |m| m.wrapping_add(1));
        let takes = |t: u32| {
            if !mesh.visible(t) {
                return false;
            }
            if !masked {
                return true;
            }
            // A polygon is masked when its corners are on average.
            let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
            tri.iter().map(|&v| mesh.mask[v as usize] as u32).sum::<u32>() > 3 * 127
        };
        let groups: Vec<u16> = (0..mesh.triangle_count() as u32)
            .into_par_iter()
            .map(|t| if takes(t) { next } else { mesh.group(t) })
            .collect();
        let before = self.doc.group_runs();
        let level = self.doc.active();
        self.doc.set_groups(level, groups);
        if masked {
            let old = mask::replace(self.doc.mesh_mut(), Vec::new());
            // Undone in this order: the mask comes back, then the groups.
            self.history.push(UndoStep::Steps(vec![
                UndoStep::Mask { level, data: MaskData::Whole(old) },
                UndoStep::Groups(before),
            ]));
        } else {
            self.history.push(UndoStep::Groups(before));
        }
        self.status = format!(
            "Group {}: {} polygroups ({:.0} ms over {} levels)",
            if masked { "masked" } else { "visible" },
            self.doc.mesh().group_count(),
            t.elapsed().as_secs_f32() * 1000.0,
            self.doc.count()
        );
        log::info!("{}", self.status);
        self.workspace.notify(self.status.clone());
    }

    /// Run a queued polygroup / visibility operation (not during a stroke or a selection).
    fn run_pending_vis(&mut self) {
        if self.busy() {
            return;
        }
        match self.pending_vis.take() {
            Some(VisOp::GroupVisible) => self.group_visible(),
            Some(VisOp::ShowAll) => self.show_all(),
            Some(VisOp::Invert) => self.invert_visibility(),
            Some(VisOp::Grow) => {
                if self.doc.mesh().hidden.is_empty() {
                    self.workspace.notify("Nothing is hidden · nothing to grow");
                } else {
                    let hidden = polygroup::grown_visibility(self.doc.mesh());
                    self.apply_hidden(hidden, "Grow visibility");
                }
            }
            Some(VisOp::Shrink) => {
                if self.doc.mesh().hidden.is_empty() {
                    self.workspace.notify("Nothing is hidden · hide something first to shrink from it");
                } else {
                    let hidden = polygroup::shrunk_visibility(self.doc.mesh());
                    self.apply_hidden(hidden, "Shrink visibility");
                }
            }
            Some(VisOp::AutoGroups) => {
                let groups = polygroup::auto_groups(self.doc.mesh());
                self.apply_groups(groups, "Auto Groups");
            }
            Some(VisOp::ByNormals(angle)) => {
                let groups = polygroup::groups_by_normals(self.doc.mesh(), angle as f32);
                self.apply_groups(groups, "Group By Normals");
            }
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

    /// The selection rectangle for the workspace to draw, in logical pixels.
    fn show_select_rect(&mut self) {
        self.workspace.select_rect = self.select.and_then(|s| {
            ((self.cursor - s.start).length() >= CLICK_SLOP).then(|| {
                let (a, b) = (s.start / self.scale, self.cursor / self.scale);
                (egui::Rect::from_two_pos(egui::pos2(a.x, a.y), egui::pos2(b.x, b.y)), self.modifiers.alt_key())
            })
        });
    }

    // ------------------------------------------------------------ Wedge, Del Higher / Lower

    /// Run a queued geometry operation (not during a stroke or a selection).
    fn run_pending_geo(&mut self) {
        if self.busy() {
            return;
        }
        match self.pending_geo.take() {
            Some(GeoOp::WedgeToggle) => {
                let tool = &mut self.subtools[self.active_tool];
                if tool.wedge {
                    tool.wedge = false;
                    self.workspace.notify("Remesh off");
                } else if self.wedge_now() {
                    self.subtools[self.active_tool].wedge = true;
                }
            }
            Some(GeoOp::Wedge) => {
                self.wedge_now();
            }
            Some(GeoOp::DelHidden) => self.topology_op("Del Hidden", |m| {
                if m.hidden.is_empty() { None } else { subtool_ops::part(m, |t| m.visible(t)) }
            }),
            Some(GeoOp::CloseHoles) => self.topology_op("Close Holes", subtool_ops::holes_closed),
            Some(GeoOp::MirrorWeld) => self.topology_op("Mirror And Weld", subtool_ops::mirrored_and_welded),
            Some(GeoOp::ZRemesh) => self.zremesh_now(),
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
            return Some("Remesh needs a subtool without subdivision levels · Geometry: Del Higher / Del Lower".into());
        }
        if !self.wedge_info().0 {
            return Some("Remesh needs a closed surface (this one has a border)".into());
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
                self.status = format!("Remesh: {e}");
                self.workspace.notify(self.status.clone());
                return false;
            }
        };
        let remesh_ms = t.elapsed().as_secs_f32() * 1000.0;
        let before = self.doc.mesh().vertex_count();
        let t2 = Instant::now();
        let mesh = SculptMesh::from_raw_painted(out.positions, out.indices, out.groups, out.colors);
        let budget = self.doc.budget;
        let old = std::mem::replace(&mut self.doc, Multires::new(mesh, budget));
        self.history.push(UndoStep::Replace(Box::new(old.pack())));
        let build_ms = t2.elapsed().as_secs_f32() * 1000.0;
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.show_active_level();
        let st = &out.stats;
        self.status = format!(
            "Remeshed at {}: {} → {} vertices in {:.0} ms (remesh {remesh_ms:.0}, chunks {build_ms:.0}; grid {}×{}×{}, {} corners turned)",
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
        if higher && !self.doc.layers.list.is_empty() {
            self.workspace.notify("Bake or delete the layers before Del Higher · Mesh › Layers");
            return;
        }
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
                self.selftest_groups_state = (self.mesh().vertex_count(), true, 1);
                let before = self.mesh().vertex_count();
                let t = Instant::now();
                self.pending_geo = Some(GeoOp::WedgeToggle);
                self.run_pending_geo();
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                let on = self.subtools[self.active_tool].wedge;
                let after = self.mesh().vertex_count();
                check(self, &format!("Remesh on at {}: {} -> {} vertices in {ms:.0} ms", self.wedge.resolution, fmt_count(before), fmt_count(after)), on && after > 0);
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
                check(self, &format!("Ctrl + drag on the background remeshed again at {}: {} -> {} vertices in {ms:.0} ms", self.wedge.resolution, fmt_count(before), fmt_count(after)), after > before);
                let ok = solid(self);
                check(self, "still closed and manifold", ok);
                Some(2)
            }
            2 => {
                let (d, t) = (drawn(self), self.mesh().triangle_count());
                check(self, &format!("GPU draws the {} remeshed triangles", fmt_count(d)), d == t);
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
                // A Cube or Cylinder starts with several polygroups.
                let before = self.mesh().group_count();
                self.selftest_groups_state.2 = before;
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
                check(self, &format!("Ctrl + W made {groups} polygroups in {group_ms:.0} ms"), groups == before + 1);
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
                let before = self.selftest_groups_state.2;
                let clean = self.mesh().hidden_count() == 0 && self.mesh().group_count() == before;
                check(self, &format!("three undos back to {before} polygroup(s), nothing hidden, in {undo_ms:.0} ms"), clean);
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

    /// `--selftest-pressure`: the tablet connection, then Draw strokes through the window's
    /// own handlers at set pen pressures (a mouse, 100 %, 50 %, 25 %, and 50 % with each of
    /// Size and Z Intensity off), each measured and undone. Logs every check and exits.
    fn selftest_pressure(&mut self) {
        let mut ok = true;
        let mut check = |what: &str, pass: bool| {
            ok &= pass;
            log::info!("selftest-pressure: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        match (&self.tablet, &self.tablet_error) {
            (Some(t), _) => check(&format!("XInput2 connection open; pens: {}", t.describe()), true),
            (None, e) => check(&format!("no tablet connection: {}", e.as_deref().unwrap_or("?")), false),
        }
        // Real raw events reach the listener: an XTest move comes from a device without
        // pressure, so it reads as a mouse.
        if let Some(t) = &self.tablet {
            let before = t.state().events;
            let sent = tablet::fake_motion(300, 300);
            let t0 = Instant::now();
            while t.state().events == before && t0.elapsed().as_millis() < 500 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let (seen, source) = {
                let s = t.state();
                (s.events > before, s.last_source)
            };
            check(
                &format!("an XTest pointer move reached the listener in {:.0} ms from device {source:?}, read as a mouse", t0.elapsed().as_secs_f32() * 1000.0),
                sent.is_ok() && seen && t.pressure().is_none(),
            );
        }
        // The scripted pressures stand in for the pen: keep the tablet from overwriting them.
        let tablet = self.tablet.take();
        let centre = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
        let step = Vec2::new(self.viewport().y * 0.01, 0.0);
        // (vertices moved, deepest move, ms per stroke); every stroke is undone.
        let mut undone = true;
        let mut stroke = |app: &mut App, p: Option<f32>, size: bool, intensity: bool| -> (usize, f32, f32) {
            app.pressure = p;
            app.workspace.pressure_size = size;
            app.workspace.pressure_intensity = intensity;
            let before = app.doc.mesh().positions.clone();
            let t = Instant::now();
            app.cursor_moved(centre, false);
            app.left_press(false, false);
            for k in 1..=10 {
                app.cursor_moved(centre + step * k as f32, false);
            }
            app.left_release();
            let ms = t.elapsed().as_secs_f32() * 1000.0;
            let moves: Vec<f32> = app.doc.mesh().positions.par_iter().zip(&before).map(|(a, b)| (*a - *b).length()).collect();
            app.history.undo(&mut app.doc);
            undone &= app.doc.mesh().positions.par_iter().zip(&before).all(|(a, b)| (*a - *b).length() < 1e-6);
            let moved = moves.iter().filter(|&&d| d > 1e-7).count();
            (moved, moves.iter().copied().fold(0.0, f32::max), ms)
        };
        let mouse = stroke(self, None, true, true);
        let full = stroke(self, Some(1.0), true, true);
        let half = stroke(self, Some(0.5), true, true);
        let quarter = stroke(self, Some(0.25), true, true);
        let half_depth_only = stroke(self, Some(0.5), false, true);
        let half_size_only = stroke(self, Some(0.5), true, false);
        let none = stroke(self, Some(0.0), true, true);
        for (name, r) in [("mouse", mouse), ("100 %", full), ("50 %", half), ("25 %", quarter), ("50 %, Z Intensity only", half_depth_only), ("50 %, Size only", half_size_only), ("0 %", none)] {
            log::info!("selftest-pressure: {name}: {} vertices moved, deepest {:.5}, stroke {:.1} ms", fmt_count(r.0), r.1, r.2);
        }
        check("a mouse strokes like full pressure", mouse.0 == full.0 && (mouse.1 - full.1).abs() < 1e-6);
        check("half pressure: a narrower, shallower stroke", half.0 < full.0 && half.1 < full.1 * 0.5);
        check("quarter pressure: narrower and shallower still", quarter.0 < half.0 && quarter.1 < half.1);
        let depth_ratio = half_depth_only.1 / full.1;
        check(
            &format!("Size off: the same width ({} vs {}), half the depth ({depth_ratio:.2})", fmt_count(half_depth_only.0), fmt_count(full.0)),
            half_depth_only.0 as f32 > full.0 as f32 * 0.95 && (depth_ratio - 0.5).abs() < 0.05,
        );
        check("Z Intensity off: narrower, deeper than with both", half_size_only.0 < full.0 && half_size_only.1 > half.1);
        check("no pressure moves nothing", none.0 == 0);
        check("undo restores every stroke", undone);
        self.pressure = None;
        self.workspace.pressure_size = true;
        self.workspace.pressure_intensity = true;
        self.tablet = tablet;
        log::info!("selftest-pressure: {}", if ok { "all passed" } else { "FAILED" });
    }

    /// Test new brushes through canvas input, GPU upload/compute, undo and redo.
    fn selftest_brushes_frame(&mut self, t: &mut BrushTest) -> bool {
        let kinds = [
            BrushKind::Blob,
            BrushKind::Layer,
            BrushKind::Fill,
            BrushKind::Scrape,
            BrushKind::Nudge,
            BrushKind::Sharpen,
            BrushKind::Elastic,
            BrushKind::SnakeHook,
            BrushKind::FormSoft,
            BrushKind::FormHard,
            BrushKind::Zipper,
            BrushKind::Stitch,
            BrushKind::Rake,
            BrushKind::ClayTubes,
            BrushKind::HPolish,
            BrushKind::TrimAdaptive,
            BrushKind::Planar,
            BrushKind::TrimSmoothBorder,
            BrushKind::MoveTopological,
            BrushKind::InflateBalloon,
            BrushKind::Magnify,
            BrushKind::OrbCracks,
            BrushKind::MalletFast,
            BrushKind::SmoothValleys,
            BrushKind::SmoothPeaks,
        ];
        if t.index >= kinds.len() {
            return self.selftest_tiny_drag_frame(t, t.index - kinds.len());
        }
        let kind = kinds[t.index];
        let frame = t.frame;
        t.frame += 1;
        let centre = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
        let check_gpu = |app: &App| {
            let gpu = app
                .gfx
                .as_ref()
                .and_then(Gfx::read_positions)
                .unwrap_or_default();
            let cpu = &app.mesh().positions;
            assert_eq!(gpu.len(), cpu.len(), "{kind:?} GPU vertex count");
            let error = gpu
                .iter()
                .zip(cpu)
                .map(|(a, b)| a.distance(*b))
                .fold(0.0f32, f32::max);
            assert!(error < 1e-5, "{kind:?} CPU/GPU error {error}");
        };
        match frame {
            19 if matches!(kind, BrushKind::SmoothValleys | BrushKind::SmoothPeaks) && !t.rippled => {
                t.rippled = true;
                let mesh = self.doc.mesh_mut();
                let sel = mesh.select_sphere(Vec3::ZERO, f32::MAX.sqrt());
                let rippled = sel.par_map(|_, v| {
                    let p = mesh.positions[v as usize];
                    p * (1.0 + 0.02 * (p.x * 23.0).sin() * (p.y * 19.0 + p.z * 7.0).sin())
                });
                mesh.scatter_positions(&sel, &rippled);
                mesh.finish_edit(&sel.chunks());
            }
            20 => {
                t.original = self.mesh().positions.clone();
                self.workspace.select_brush(&mut self.brush, kind);
                self.brush.radius = 0.4;
                self.brush.strength = 0.7;
                self.on_key(KeyCode::KeyQ);
                self.cursor_moved(centre, false);
                self.left_press(false, false);
            }
            21..=27 => self.cursor_moved(centre + Vec2::new((frame - 20) as f32 * 8.0, 0.0), false),
            28 => self.left_release(),
            34 => {
                t.edited = self.mesh().positions.clone();
                assert!(
                    t.original != t.edited,
                    "{kind:?} must sculpt through canvas input"
                );
                assert!(t.edited.iter().all(|p| p.is_finite()));
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
            }
            40 => {
                assert!(self.mesh().positions == t.original, "{kind:?} undo");
                check_gpu(self);
                self.history.redo(&mut self.doc);
                self.show_active_level();
            }
            46 => {
                assert!(
                    self.mesh()
                        .positions
                        .iter()
                        .zip(&t.edited)
                        .all(|(a, b)| a.distance(*b) < 1e-6),
                    "{kind:?} redo"
                );
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
                log::info!(
                    "selftest-brushes: {} canvas, GPU, undo and redo (ok)",
                    kind.name()
                );
                t.index += 1;
                t.frame = 18;
            }
            _ => {}
        }
        false
    }

    /// End of `--selftest-brushes`: Grab, Snake Hook and Move Topological (`which` 0..3) with a brush smaller
    /// than the polygons under it, so the press selects no vertex. The stroke must do
    /// nothing, add no undo step and keep the GPU copy in step, not crash.
    fn selftest_tiny_drag_frame(&mut self, t: &mut BrushTest, which: usize) -> bool {
        let kinds = [BrushKind::Grab, BrushKind::SnakeHook, BrushKind::MoveTopological];
        let Some(&kind) = kinds.get(which) else {
            log::info!("selftest-brushes: all passed");
            return true;
        };
        let frame = t.frame;
        t.frame += 1;
        let centre = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
        match frame {
            20 => {
                t.original = self.mesh().positions.clone();
                t.edited = vec![Vec3::splat(self.history.edits() as f32)];
                self.workspace.select_brush(&mut self.brush, kind);
                self.brush.radius = 1e-5;
                self.brush.strength = 1.0;
                self.on_key(KeyCode::KeyQ);
                self.cursor_moved(centre, false);
                self.left_press(false, false);
            }
            21..=27 => self.cursor_moved(centre + Vec2::new((frame - 20) as f32 * 12.0, 0.0), false),
            28 => self.left_release(),
            34 => {
                assert!(self.mesh().positions == t.original, "{kind:?} smaller than a polygon moved the mesh");
                let gpu = self.gfx.as_ref().and_then(Gfx::read_positions).unwrap_or_default();
                assert!(gpu.len() == t.original.len() && gpu.iter().zip(&t.original).all(|(a, b)| a.distance(*b) < 1e-6), "{kind:?} GPU copy");
                log::info!("selftest-brushes: {} smaller than a polygon does nothing, no crash (ok)", kind.name());
                t.index += 1;
                t.frame = 18;
            }
            _ => {}
        }
        false
    }

    /// Real input handlers, rendered frames and GPU readback for RGB texture painting.
    fn selftest_paint_frame(&mut self, t: &mut PaintTest) -> bool {
        let k=t.frame; t.frame+=1;
        let check=|t:&mut PaintTest,what:&str,pass:bool| {
            t.ok &= pass;
            log::info!("selftest-paint: {what} ({})",if pass{"ok"}else{"FAIL"});
        };
        let centre=self.project((self.doc.bounds().min+self.doc.bounds().max)*0.5);
        match k {
            20=>{
                t.ok=true;t.original=self.mesh().positions.clone();
                self.workspace.select_brush(&mut self.brush,BrushKind::Paint);
                self.brush.color=[215,72,39];self.brush.radius=0.4;
                self.workspace.assets.texture=brush_asset::Pattern::Marble;
                self.on_key(KeyCode::KeyQ);
                self.cursor_moved(centre,false);self.left_press(false,false);
            },
            21..=27=>self.cursor_moved(centre+Vec2::new((k-20) as f32*8.0,0.0),false),
            28=>self.left_release(),
            35=>{
                t.painted=self.mesh().colors.clone();
                check(t,"texture painted through canvas input",t.painted.iter().any(|c|c[3]>0));
                check(t,"RGB mode preserves positions",self.mesh().positions==t.original);
                let gpu=self.gfx.as_ref().and_then(Gfx::read_colors).unwrap_or_default();
                check(t,"GPU paint matches CPU",gpu==t.painted);
                self.history.undo(&mut self.doc);self.show_active_level();
            },
            42=>{
                let gpu=self.gfx.as_ref().and_then(Gfx::read_colors).unwrap_or_default();
                check(t,"undo clears CPU and GPU paint",self.mesh().colors.iter().all(|c|c[3]==0)&&gpu.iter().all(|c|c[3]==0)&&gpu.len()==t.original.len());
                self.history.redo(&mut self.doc);self.show_active_level();
            },
            49=>{
                let gpu=self.gfx.as_ref().and_then(Gfx::read_colors).unwrap_or_default();
                check(t,"redo restores exact CPU and GPU paint",self.mesh().colors==t.painted&&gpu==t.painted);
                assert!(t.ok,"native RGB painting self-test failed");
                log::info!("selftest-paint: all passed");return true;
            },
            _=>{},
        }
        false
    }

    /// `--selftest-curve`, one phase per call a few frames apart. Phase 0 draws the same
    /// Zipper line four ways through the window's input handlers (a shaky and a steady hand,
    /// each in Curve and Freehand mode) and measures how far the shaky result strays from
    /// the steady one, then applies a curve and bends it. Phase 1 checks the GPU copy of the
    /// bent result, undo and redo of the bend and the curve, and Esc. Logs and exits.
    fn selftest_curve_phase(&mut self, phase: u8) -> Option<u8> {
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_curve_state.ok &= pass;
            log::info!("selftest-curve: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let max_diff = |a: &[Vec3], b: &[Vec3]| a.par_iter().zip(b).map(|(x, y)| x.distance(*y)).reduce(|| 0.0, f32::max);
        match phase {
            0 => {
                self.selftest_curve_state = CurveTest { ok: true, ..CurveTest::default() };
                self.workspace.select_brush(&mut self.brush, BrushKind::Zipper);
                self.workspace.assets = sculpt::StampSettings::default();
                self.brush.radius = 0.15;
                self.brush.strength = 1.0;
                self.pressure = None;
                let original = self.mesh().positions.clone();
                let vp = self.viewport();
                let c = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
                let len = vp.y * 0.5;
                // A gentle arc, drawn every 2 px, with or without a few pixels of hand shake.
                let path = |shake: f32| -> Vec<Vec2> {
                    (0..=(len / 2.0) as usize)
                        .map(|k| {
                            let x = k as f32 * 2.0;
                            let arc = (std::f32::consts::PI * x / len).sin() * len * 0.12;
                            let jitter = ((x * 0.9).sin() + 0.6 * (x * 0.47 + 1.0).sin()) * shake;
                            c + Vec2::new(x - len * 0.5, -arc + jitter)
                        })
                        .collect()
                };
                let draw = |app: &mut Self, path: &[Vec2], curve: bool| -> (Vec<Vec3>, bool, f32) {
                    app.workspace.set_curve_mode(BrushKind::Zipper, curve);
                    app.cursor_moved(path[0], false);
                    app.left_press(false, false);
                    for &p in &path[1..] {
                        app.cursor_moved(p, false);
                    }
                    let untouched = app.mesh().positions == original;
                    let t = Instant::now();
                    app.left_release();
                    let ms = t.elapsed().as_secs_f32() * 1000.0;
                    (app.mesh().positions.clone(), untouched, ms)
                };
                let undo = |app: &mut Self| {
                    app.history.undo(&mut app.doc);
                    app.show_active_level();
                };
                let (shaky, steady) = (path(3.0), path(0.0));
                let (a, waited, apply_ms) = draw(self, &shaky, true);
                let shaky_curve = self.curve.as_ref().map(|c| c.curve.clone()).unwrap_or_default();
                undo(self);
                let back = max_diff(&self.mesh().positions, &original) < 1e-5;
                let (b, _, _) = draw(self, &steady, true);
                let steady_curve = self.curve.as_ref().map(|c| c.curve.clone()).unwrap_or_default();
                undo(self);
                // How far the curve laid from the shaky stroke strays from the steady one's,
                // in Draw Size: in the middle, and at the ends.
                let gap = |p: Vec3| {
                    steady_curve
                        .points
                        .windows(2)
                        .map(|w| {
                            let ab = w[1] - w[0];
                            let t = ((p - w[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
                            (w[0] + ab * t).distance(p)
                        })
                        .fold(f32::INFINITY, f32::min)
                };
                let n = shaky_curve.points.len();
                let (mut middle, mut ends) = (0.0f32, 0.0f32);
                for (i, &p) in shaky_curve.points.iter().enumerate() {
                    let d = gap(p) / self.brush.radius;
                    if i * 10 < n || i * 10 > n * 9 { ends = ends.max(d) } else { middle = middle.max(d) }
                }
                let lengths = (shaky_curve.along.last().copied().unwrap_or(0.0), steady_curve.along.last().copied().unwrap_or(0.0));
                let curve_ok = middle < 0.01 && ends < 0.03 && (lengths.0 - lengths.1).abs() < 0.01 * lengths.1;
                let (fc, _, _) = draw(self, &shaky, false);
                undo(self);
                let (fd, _, _) = draw(self, &steady, false);
                undo(self);
                // Stray of a result from the steady line's: RMS vertex difference over the
                // relief's height.
                let stray = |x: &[Vec3], y: &[Vec3]| {
                    let height = y.par_iter().zip(&original).map(|(p, q)| p.distance(*q)).reduce(|| 0.0, f32::max).max(1e-9);
                    let sum: f64 = x.par_iter().zip(y).map(|(p, q)| p.distance_squared(*q) as f64).sum();
                    let moved = y.par_iter().zip(&original).filter(|(p, q)| p.distance(**q) > 1e-7).count().max(1);
                    ((sum / moved as f64).sqrt() as f32) / height
                };
                let moved = a.par_iter().zip(&original).filter(|(p, q)| p.distance(**q) > 1e-7).count();
                let (curve_stray, free_stray) = (stray(&a, &b), stray(&fc, &fd));
                check(self, &format!("Curve mode changes nothing until release, then applies along the curve in {apply_ms:.0} ms ({} vertices)", fmt_count(moved)), waited && moved > 0);
                check(self, "undo restores the curve stroke", back);
                check(
                    self,
                    &format!(
                        "a shaky stroke lays the steady stroke's curve: {:.2} % of Draw Size apart in the middle, {:.2} % at the ends, lengths {:.4} / {:.4}",
                        middle * 100.0, ends * 100.0, lengths.0, lengths.1
                    ),
                    curve_ok,
                );
                // Vertex by vertex the relief's sharp tooth edges magnify even sub-percent
                // differences, so this compares the two modes rather than expecting zero.
                check(
                    self,
                    &format!("its Zipper strays {:.1} % from the steady line's (RMS over tooth height); freehand strays {:.1} %", curve_stray * 100.0, free_stray * 100.0),
                    curve_stray * 3.0 < free_stray,
                );
                // Apply once more and bend it: grab the middle and pull it up.
                let (applied, _, _) = draw(self, &shaky, true);
                let live = self.curve.is_some();
                let mid = self.curve.as_ref().map(|c| c.curve.points[c.curve.points.len() / 2]).unwrap_or(Vec3::ZERO);
                let grab = self.project(mid);
                self.cursor_moved(grab, false);
                self.left_press(false, false);
                let bending = self.curve.as_ref().is_some_and(|c| c.bend.is_some());
                let t = Instant::now();
                for k in 1..=10 {
                    self.cursor_moved(grab + Vec2::new(0.0, -vp.y * 0.008 * k as f32), false);
                    self.bend_curve();
                }
                let bend_ms = t.elapsed().as_secs_f32() * 1000.0 / 10.0;
                self.left_release();
                let bent = self.mesh().positions.clone();
                let still = self.live_curve().is_some();
                check(self, &format!("the applied curve stays and a press on it bends it ({bend_ms:.0} ms per re-application)"), live && bending);
                check(self, "the bend re-applies along the new shape and the curve can be bent again", max_diff(&bent, &applied) > 1e-4 && still);
                self.selftest_curve_state.original = original;
                self.selftest_curve_state.applied = applied;
                self.selftest_curve_state.bent = bent;
                Some(1)
            }
            _ => {
                let t = std::mem::take(&mut self.selftest_curve_state);
                self.selftest_curve_state.ok = t.ok;
                if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                    let d = max_diff(&gpu[..t.bent.len()], &t.bent);
                    check(self, &format!("GPU copy of the bent result matches the CPU ({d:.1e})"), d < 1e-5);
                }
                self.history.undo(&mut self.doc);
                self.show_active_level();
                let d1 = max_diff(&self.mesh().positions, &t.applied);
                let gone = self.live_curve().is_none();
                self.history.undo(&mut self.doc);
                self.show_active_level();
                let d0 = max_diff(&self.mesh().positions, &t.original);
                check(self, &format!("undo takes back the bend ({d1:.1e}), then the curve ({d0:.1e}); the curve is let go"), d1 < 1e-5 && d0 < 1e-5 && gone);
                self.history.redo(&mut self.doc);
                self.history.redo(&mut self.doc);
                self.show_active_level();
                let d2 = max_diff(&self.mesh().positions, &t.bent);
                check(self, &format!("redo brings both back ({d2:.1e})"), d2 < 1e-5);
                // Esc lets an applied curve go and keeps its result.
                let c = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
                self.cursor_moved(c, false);
                self.left_press(false, false);
                self.cursor_moved(c + Vec2::new(60.0, 0.0), false);
                self.left_release();
                let before = self.mesh().positions.clone();
                let had = self.curve.is_some();
                self.on_key(KeyCode::Escape);
                check(self, "Esc lets the curve go and keeps its result", had && self.curve.is_none() && self.mesh().positions == before);
                let ok = self.selftest_curve_state.ok;
                log::info!("selftest-curve: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
    }

    /// `--selftest-clip`: a pie slice cut out of the model with three crossing Clip Line
    /// strokes through the real handlers (the cuts meet at wide angles), then the view is
    /// turned. No cut may push an earlier one back out, and the GPU copy must match.
    fn selftest_clip_cuts(&mut self) -> bool {
        self.workspace.select_brush(&mut self.brush, BrushKind::ClipLine);
        let b = self.doc.bounds();
        let c = self.project((b.min + b.max) * 0.5);
        let r = (self.project((b.min + b.max) * 0.5 + Vec3::X * (b.max - b.min).max_element() * 0.5) - c).length();
        // Two cuts meeting at a point near the top, each taking the outside of the slice,
        // then one across the bottom (a cut removes the side to the left of its drag).
        let cuts = [
            (Vec2::new(-1.2, 1.2), Vec2::new(0.05, -0.85)),
            (Vec2::new(0.05, -0.85), Vec2::new(1.2, 1.2)),
            (Vec2::new(1.5, 0.6), Vec2::new(-1.5, 0.6)),
        ];
        let mut planes = Vec::new();
        let mut ok = true;
        for (k, (a, e)) in cuts.into_iter().enumerate() {
            let (a, e) = (c + a * r, c + e * r);
            planes.push(self.surface_screen().plane(a - self.canvas_origin(), e - self.canvas_origin()).unwrap());
            let t = Instant::now();
            self.cursor_moved(a, false);
            self.left_press(false, false);
            self.cursor_moved(e, false);
            self.left_release();
            let ms = t.elapsed().as_secs_f32() * 1000.0;
            let out = planes
                .iter()
                .map(|q| self.mesh().positions.iter().map(|p| q.truncate().dot(*p) + q.w).fold(f32::MIN, f32::max))
                .fold(f32::MIN, f32::max);
            let pass = out < 1e-4;
            ok &= pass;
            log::info!("selftest-clip: cut {} in {ms:.0} ms, worst vertex past any cut so far {out:.1e} ({})", k + 1, if pass { "ok" } else { "FAIL" });
        }
        self.camera.yaw += 0.9;
        self.camera.pitch -= 0.35;
        self.wedge_info = None;
        log::info!("selftest-clip: {}", if ok { "all passed" } else { "FAILED" });
        ok
    }

    /// `--selftest-tube`: CurveTube through the real input handlers on the GPU. Phase 0
    /// draws a fringe of tubes over the model (each one let go with Esc), the last one off
    /// the side of the model, and bends it; phase 1 checks the GPU copy, undo and redo.
    fn selftest_tube_phase(&mut self, phase: u8) -> Option<u8> {
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_curve_state.ok &= pass;
            log::info!("selftest-tube: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let max_diff = |a: &[Vec3], b: &[Vec3]| a.par_iter().zip(b).map(|(x, y)| x.distance(*y)).reduce(|| 0.0, f32::max);
        let draw = |app: &mut Self, path: &[Vec2]| -> f32 {
            app.cursor_moved(path[0], false);
            app.left_press(false, false);
            for &p in &path[1..] {
                app.cursor_moved(p, false);
            }
            let t = Instant::now();
            app.left_release();
            t.elapsed().as_secs_f32() * 1000.0
        };
        match phase {
            0 => {
                self.selftest_curve_state = CurveTest { ok: true, ..CurveTest::default() };
                self.workspace.select_brush(&mut self.brush, BrushKind::CurveTube);
                let b = self.doc.bounds();
                let (centre, size) = ((b.min + b.max) * 0.5, (b.max - b.min).max_element());
                self.brush.radius = size * 0.022;
                self.workspace.tube_taper = 0.35;
                self.pressure = None;
                let original = self.mesh().positions.clone();
                let groups = self.mesh().group_count();
                let c = self.project(centre);
                let r = (self.project(centre + Vec3::X * size * 0.5) - c).length();
                // Locks from the crown falling down over the front and sides, with a
                // little wave, the last one running off the side.
                let mut times = Vec::new();
                for k in 0..9 {
                    let a = (k as f32 / 8.0 - 0.5) * 2.4;
                    let start = c + Vec2::new(a.sin() * r * 0.25, -r * 0.8);
                    let end = c + Vec2::new(a.sin() * r * (if k == 8 { 1.5 } else { 0.95 }), r * (0.1 + 0.25 * a.cos()));
                    let path: Vec<Vec2> = (0..=120)
                        .map(|i| {
                            let f = i as f32 / 120.0;
                            let p = start.lerp(end, f);
                            let n = (end - start).perp().normalize_or_zero();
                            p + n * (f * 18.0).sin() * r * 0.03
                        })
                        .collect();
                    times.push(draw(self, &path));
                    if k < 8 {
                        self.on_key(KeyCode::Escape);
                    }
                }
                let n = original.len();
                let added = self.mesh().vertex_count() - n;
                let kept = max_diff(&self.mesh().positions[..n], &original) == 0.0;
                let avg = times.iter().sum::<f32>() / times.len() as f32;
                check(self, &format!("nine tubes added {} vertices in {avg:.0} ms each, the model untouched", fmt_count(added)), added > 0 && kept);
                check(self, &format!("each tube is a polygroup of its own ({} → {})", groups, self.mesh().group_count()), self.mesh().group_count() == groups + 9);
                let applied = self.mesh().positions.clone();
                // Bend the last one: grab its middle and pull it up.
                let mid = self.curve.as_ref().map(|c| c.curve.points[c.curve.points.len() / 2]).unwrap_or(centre);
                let grab = self.project(mid);
                self.cursor_moved(grab, false);
                self.left_press(false, false);
                let bending = self.curve.as_ref().is_some_and(|c| c.bend.is_some());
                let t = Instant::now();
                for k in 1..=10 {
                    self.cursor_moved(grab + Vec2::new(r * 0.02 * k as f32, -r * 0.03 * k as f32), false);
                    self.bend_curve();
                }
                let bend_ms = t.elapsed().as_secs_f32() * 1000.0 / 10.0;
                self.left_release();
                let bent = self.mesh().positions.clone();
                let n_last = self.curve.as_ref().and_then(|c| c.tube).map_or(0, |t| t.keep.verts);
                check(
                    self,
                    &format!("a press on the last curve bends its tube ({bend_ms:.0} ms per rebuild at {} vertices)", fmt_count(bent.len())),
                    bending && max_diff(&bent[..n_last], &applied[..n_last]) == 0.0 && bent[n_last..] != applied[n_last..],
                );
                self.selftest_curve_state.original = original;
                self.selftest_curve_state.applied = applied;
                self.selftest_curve_state.bent = bent;
                Some(1)
            }
            _ => {
                let t = std::mem::take(&mut self.selftest_curve_state);
                self.selftest_curve_state.ok = t.ok;
                if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                    let d = if gpu.len() >= t.bent.len() { max_diff(&gpu[..t.bent.len()], &t.bent) } else { f32::INFINITY };
                    check(self, &format!("GPU copy of the model with its tubes matches the CPU ({d:.1e})"), d < 1e-6);
                }
                let undo = |app: &mut Self| {
                    app.history.undo(&mut app.doc);
                    app.show_active_level();
                };
                undo(self);
                let d1 = max_diff(&self.mesh().positions, &t.applied);
                let same = self.mesh().vertex_count() == t.applied.len();
                for _ in 0..9 {
                    undo(self);
                }
                let back = self.mesh().positions == t.original;
                check(self, &format!("undo takes back the bend ({d1:.1e}), then every tube"), same && d1 == 0.0 && back);
                for _ in 0..10 {
                    self.history.redo(&mut self.doc);
                }
                self.show_active_level();
                check(self, "redo brings them all back", self.mesh().positions == t.bent);
                let ok = self.selftest_curve_state.ok;
                log::info!("selftest-tube: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
    }

    /// `--selftest-insert`: Insert Mesh through the real input handlers on the GPU. Phase 0
    /// drags one of each shape onto the model; phase 1 checks the GPU copy, undo and redo.
    fn selftest_insert_phase(&mut self, phase: u8) -> Option<u8> {
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_curve_state.ok &= pass;
            log::info!("selftest-insert: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let max_diff = |a: &[Vec3], b: &[Vec3]| a.par_iter().zip(b).map(|(x, y)| x.distance(*y)).reduce(|| 0.0, f32::max);
        match phase {
            0 => {
                self.selftest_curve_state = CurveTest { ok: true, ..CurveTest::default() };
                self.workspace.select_brush(&mut self.brush, BrushKind::InsertMesh);
                self.pressure = None;
                let b = self.doc.bounds();
                let (centre, size) = ((b.min + b.max) * 0.5, (b.max - b.min).max_element());
                let original = self.mesh().positions.clone();
                let groups = self.mesh().group_count();
                let c = self.project(centre);
                let r = (self.project(centre + Vec3::X * size * 0.5) - c).length();
                let mut times = Vec::new();
                for (k, shape) in insert_mesh::SHAPES.into_iter().enumerate() {
                    self.workspace.insert_shape = shape;
                    let at = c + Vec2::new((k as f32 - 1.5) * r * 0.35, -r * 0.1);
                    self.cursor_moved(at, false);
                    let t = Instant::now();
                    self.left_press(false, false);
                    for i in 1..=12 {
                        self.cursor_moved(at + Vec2::new(r * 0.12 * i as f32 / 12.0, 0.0), false);
                    }
                    self.left_release();
                    times.push(t.elapsed().as_secs_f32() * 1000.0);
                }
                let n = original.len();
                let added = self.mesh().vertex_count() - n;
                let kept = max_diff(&self.mesh().positions[..n], &original) == 0.0;
                let avg = times.iter().sum::<f32>() / times.len() as f32;
                check(self, &format!("four shapes added {} vertices, {avg:.0} ms per drag of 12 rebuilds, the model untouched", fmt_count(added)), added > 0 && kept);
                check(self, &format!("each is a polygroup of its own ({} → {})", groups, self.mesh().group_count()), self.mesh().group_count() == groups + 4);
                let masked = self.mesh().mask.len() == self.mesh().vertex_count() && self.mesh().mask[..n].iter().all(|&m| m == 255);
                check(self, "the model is masked, the last insert is not", masked && *self.mesh().mask.last().unwrap() == 0);
                self.selftest_curve_state.original = original;
                self.selftest_curve_state.bent = self.mesh().positions.clone();
                Some(1)
            }
            _ => {
                let t = std::mem::take(&mut self.selftest_curve_state);
                self.selftest_curve_state.ok = t.ok;
                if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                    let d = if gpu.len() >= t.bent.len() { max_diff(&gpu[..t.bent.len()], &t.bent) } else { f32::INFINITY };
                    check(self, &format!("GPU copy of the model with its inserts matches the CPU ({d:.1e})"), d < 1e-6);
                }
                for _ in 0..4 {
                    self.history.undo(&mut self.doc);
                }
                self.show_active_level();
                check(self, "undo takes every insert back", self.mesh().positions == t.original);
                for _ in 0..4 {
                    self.history.redo(&mut self.doc);
                }
                self.show_active_level();
                check(self, "redo brings them all back", self.mesh().positions == t.bent);
                let ok = self.selftest_curve_state.ok;
                log::info!("selftest-insert: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
    }

    /// `--selftest-sculptris`: Sculptris Pro strokes through the real input handlers on the
    /// GPU. Phase 0 strokes across the model at three sizes, timing every pointer move;
    /// phase 1 checks the surface is closed, the GPU copy, undo and redo.
    fn selftest_sculptris_phase(&mut self, phase: u8) -> Option<u8> {
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_curve_state.ok &= pass;
            log::info!("selftest-sculptris: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let max_diff = |a: &[Vec3], b: &[Vec3]| a.par_iter().zip(b).map(|(x, y)| x.distance(*y)).reduce(|| 0.0, f32::max);
        match phase {
            0 => {
                self.selftest_curve_state = CurveTest { ok: true, ..CurveTest::default() };
                self.workspace.sculptris = true;
                self.workspace.select_brush(&mut self.brush, BrushKind::ClayBuildup);
                self.pressure = None;
                let b = self.doc.bounds();
                let (centre, size) = ((b.min + b.max) * 0.5, (b.max - b.min).max_element());
                let original = self.mesh().positions.clone();
                let tris = self.mesh().triangle_count();
                let c = self.project(centre);
                let r = (self.project(centre + Vec3::X * size * 0.5) - c).length();
                let mut moves = Vec::new();
                for (k, share) in [0.08f32, 0.04, 0.02].into_iter().enumerate() {
                    self.brush.radius = size * share;
                    let y = (k as f32 - 1.0) * r * 0.3;
                    let (from, to) = (c + Vec2::new(-r * 0.6, y), c + Vec2::new(r * 0.6, y + r * 0.1));
                    self.cursor_moved(from, false);
                    self.left_press(false, false);
                    for i in 1..=60 {
                        let t = Instant::now();
                        self.cursor_moved(from + (to - from) * (i as f32 / 60.0), false);
                        moves.push(t.elapsed().as_secs_f32() * 1000.0);
                    }
                    self.left_release();
                }
                let after = self.mesh().triangle_count();
                let mean = moves.iter().sum::<f32>() / moves.len() as f32;
                let worst = moves.iter().copied().fold(0.0, f32::max);
                check(self, &format!("three strokes took {} → {} triangles; pointer moves {mean:.1} ms mean, {worst:.1} ms worst", fmt_count(tris), fmt_count(after)), after != tris);
                let m = self.mesh();
                let closed = (0..m.vertex_count() as u32).into_par_iter().all(|v| {
                    m.neighbors(v).iter().all(|&w| m.incident(v).iter().filter(|&&t| m.indices[3 * t as usize..3 * t as usize + 3].contains(&w)).count() == 2)
                });
                check(self, "the surface is still closed: every edge has two triangles", closed);
                self.selftest_curve_state.original = original;
                self.selftest_curve_state.bent = self.mesh().positions.clone();
                Some(1)
            }
            _ => {
                let t = std::mem::take(&mut self.selftest_curve_state);
                self.selftest_curve_state.ok = t.ok;
                if let Some(gpu) = self.gfx.as_ref().and_then(Gfx::read_positions) {
                    let d = if gpu.len() >= t.bent.len() { max_diff(&gpu[..t.bent.len()], &t.bent) } else { f32::INFINITY };
                    check(self, &format!("GPU copy matches the CPU ({d:.1e})"), d < 1e-6);
                }
                self.history_jump(-3);
                let sorted = |p: &[Vec3]| {
                    let mut v: Vec<[u32; 3]> = p.iter().map(|q| q.to_array().map(f32::to_bits)).collect();
                    v.par_sort_unstable();
                    v
                };
                check(self, "three undos put the subtool back", sorted(&self.mesh().positions) == sorted(&t.original));
                self.history_jump(3);
                check(self, "redo brings the strokes back", sorted(&self.mesh().positions) == sorted(&t.bent));
                let ok = self.selftest_curve_state.ok;
                log::info!("selftest-sculptris: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
    }

    /// `--selftest-zmodeler`: on a cube of 2 x 2 quads a side, through the real input
    /// handlers: QMesh, Inset, Insert, Delete edge loop, Polygroup and point Move, each
    /// checked on the polygons and against the GPU copy; then every step undone.
    fn selftest_zmodeler_phase(&mut self, phase: u8) -> Option<u8> {
        use zmodeler::{EdgeAction, PolyAction};
        let check = |app: &mut Self, what: &str, pass: bool| {
            app.selftest_curve_state.ok &= pass;
            log::info!("selftest-zmodeler: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let drag = |app: &mut Self, from: Vec2, to: Vec2| {
            app.cursor_moved(from, false);
            app.left_press(false, false);
            for k in 1..=6 {
                app.cursor_moved(from + (to - from) * (k as f32 / 6.0), false);
            }
            app.left_release();
        };
        let faces = |app: &Self| app.zmodel.as_ref().map_or(0, |z| z.poly.faces.len());
        let gpu_ok = |app: &Self| {
            app.gfx.as_ref().and_then(Gfx::read_positions).is_none_or(|g| {
                let m = app.doc.mesh();
                g.len() >= m.vertex_count() && g.iter().zip(&m.positions).all(|(a, b)| a.distance(*b) < 1e-6)
            })
        };
        let at = |app: &Self, p: Vec3| app.project(p);
        match phase {
            0 => {
                self.selftest_curve_state = CurveTest { ok: true, ..CurveTest::default() };
                let (p, i) = Primitive::Cube.build(2, 1.0);
                self.replace_active_mesh(SculptMesh::from_raw(p, i));
                self.frame_camera();
                // Seen from the front, a little from the right and above: +Z, +X and +Y show.
                self.camera.yaw = 0.6;
                self.camera.pitch = 0.45;
                if self.camera.forward().z > 0.0 {
                    self.camera.yaw = std::f32::consts::PI - self.camera.yaw;
                }
                if self.camera.forward().x > 0.0 {
                    self.camera.yaw = -self.camera.yaw;
                }
                if self.camera.forward().y > 0.0 {
                    self.camera.pitch = -self.camera.pitch;
                }
                self.workspace.select_brush(&mut self.brush, BrushKind::ZModeler);
                self.selftest_curve_state.original = self.doc.mesh().positions.clone();
                Some(1)
            }
            1 => {
                self.workspace.zm_poly = PolyAction::QMesh;
                let c = at(self, Vec3::new(0.5, 0.5, 1.0));
                let up = at(self, Vec3::new(0.5, 0.5, 1.6));
                drag(self, c, up);
                check(self, &format!("QMesh pulls a quad out: {} polygons, front now at z {:.2}", faces(self), self.doc.bounds().max.z), faces(self) == 28 && self.doc.bounds().max.z > 1.3);
                check(self, "GPU copy matches", gpu_ok(self));
                Some(2)
            }
            2 => {
                self.workspace.zm_poly = PolyAction::Inset;
                let c = at(self, Vec3::new(-0.5, 0.5, 1.0));
                drag(self, c, c + Vec2::new(40.0, 0.0));
                check(self, &format!("Inset: {} polygons", faces(self)), faces(self) == 32);
                Some(3)
            }
            3 => {
                self.workspace.zm_edge = EdgeAction::Insert;
                let e = at(self, Vec3::new(0.0, -0.5, 1.0));
                drag(self, e, e + Vec2::new(4.0, 0.0));
                let quads = self.zmodel.as_ref().is_some_and(|z| z.poly.faces.iter().all(|f| f.len() == 4));
                check(self, &format!("Insert cuts a loop: {} polygons, all quads {quads}", faces(self)), faces(self) > 32 && quads);
                check(self, "GPU copy matches", gpu_ok(self));
                Some(4)
            }
            4 => {
                self.workspace.zm_poly = PolyAction::Polygroup;
                let groups = self.doc.mesh().group_count();
                let c = at(self, Vec3::new(-0.5, -0.5, 1.0));
                drag(self, c, c);
                check(self, &format!("Polygroup makes a new group ({groups} → {})", self.doc.mesh().group_count()), self.doc.mesh().group_count() == groups + 1);
                Some(5)
            }
            5 => {
                // Move the middle point of the top.
                let p = Vec3::new(0.0, 1.0, 0.0);
                let c = at(self, p);
                let before = self.doc.mesh().positions.iter().map(|q| q.distance(p)).fold(f32::MAX, f32::min);
                drag(self, c, c + Vec2::new(30.0, -30.0));
                let moved = !self.doc.mesh().positions.iter().any(|q| q.distance(p) < 1e-4);
                check(self, "a point moves", before < 1e-4 && moved);
                check(self, "GPU copy matches", gpu_ok(self));
                Some(6)
            }
            _ => {
                let steps = self.history.names().0.iter().filter(|n| n.starts_with("ZModeler")).count();
                self.history_jump(-(steps as i64));
                let back = self.doc.mesh().positions.len() == self.selftest_curve_state.original.len();
                check(self, &format!("{steps} undo steps put the cube back"), steps == 5 && back && self.doc.mesh().triangle_count() == 48);
                let ok = self.selftest_curve_state.ok;
                log::info!("selftest-zmodeler: {}", if ok { "all passed" } else { "FAILED" });
                None
            }
        }
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
        if !self.doc.layers.list.is_empty() {
            self.workspace.notify("Bake or delete the layers before Divide · Mesh › Layers");
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
        if self.doc.layers.rec.is_some() {
            self.workspace.notify("A layer is recording · stop REC to change the level (layers live on the highest one)");
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

    /// `--demo-brushes`: a stroke with each of the advanced brushes added in 0.7, for
    /// looking at them (through the same input path as `--demo`).
    fn advanced_strokes(&mut self) -> Vec<(String, f32, u32, usize)> {
        let still = |p: Vec2, n: usize| vec![p; n];
        self.run_scripts(vec![
            (BrushKind::ClayTubes, 0.13, 0.9, false, line(Vec2::new(-0.75, -0.55), Vec2::new(-0.1, -0.68), 30)),
            (BrushKind::ClayTubes, 0.13, 0.9, false, line(Vec2::new(-0.78, -0.36), Vec2::new(-0.12, -0.47), 30)),
            (BrushKind::OrbCracks, 0.09, 1.0, false, line(Vec2::new(0.15, -0.75), Vec2::new(0.72, -0.25), 40)),
            (BrushKind::MalletFast, 0.12, 1.0, false, still(Vec2::new(0.42, 0.12), 6)),
            (BrushKind::MalletFast, 0.09, 1.0, false, still(Vec2::new(0.62, 0.36), 6)),
            (BrushKind::InflateBalloon, 0.2, 0.9, false, arc(0.0, 6.2, 0.05, 30).into_iter().map(|p| p + Vec2::new(-0.5, 0.32)).collect()),
            (BrushKind::Planar, 0.3, 1.0, false, line(Vec2::new(-0.05, -0.1), Vec2::new(0.1, 0.0), 12)),
            (BrushKind::Magnify, 0.13, 0.8, false, still(Vec2::new(0.1, 0.62), 10)),
            (BrushKind::MoveTopological, 0.25, 0.6, false, line(Vec2::new(0.92, -0.1), Vec2::new(1.12, -0.25), 15)),
        ])
    }

    /// Scripted strokes through the real input path: used by `--demo` and `--bench`.
    fn scripted_strokes(&mut self) -> Vec<(String, f32, u32, usize)> {
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
        self.run_scripts(scripts)
    }

    /// Strokes of `(kind, radius, strength, invert, path in units of a third of the canvas
    /// height from its centre)`, each through the real input path.
    fn run_scripts(&mut self, scripts: Vec<(BrushKind, f32, f32, bool, Vec<Vec2>)>) -> Vec<(String, f32, u32, usize)> {
        let vp = self.viewport();
        let c = self.canvas_origin() + vp * 0.5;
        let r = vp.y * 0.33;
        let mut report = Vec::new();
        for (kind, radius, strength, invert, path) in scripts {
            self.brush = Brush {
                kind,
                radius,
                strength, ..Brush::default() };
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
        self.ray_at(self.cursor)
    }

    /// The camera ray through window point `p`.
    fn ray_at(&self, p: Vec2) -> (Vec3, Vec3) {
        let vp = self.viewport();
        let p = p - self.canvas_origin();
        self.camera.ray(p.x, p.y, vp.x, vp.y)
    }

    /// Where the stroke goes next, in window pixels: the cursor, or with LazyMouse the
    /// brush point pulled along behind it on a string of LazyRadius (it stays put while the
    /// cursor moves inside the string). The points from the last one reached to it, a few
    /// pixels apart, so a fast stroke leaves no gaps (Stroke › Spacing thins the dabs).
    fn stroke_path(&mut self) -> Vec<Vec2> {
        let target = match self.lazy_point.as_mut() {
            Some(lazy) => {
                let d = self.cursor - *lazy;
                let r = self.workspace.lazy_radius * self.scale;
                if d.length() > r {
                    *lazy += d * ((d.length() - r) / d.length());
                }
                *lazy
            }
            None => self.cursor,
        };
        let from = std::mem::replace(&mut self.stroke_from, target);
        let steps = ((target.distance(from) / 3.0).ceil() as usize).clamp(1, 96);
        (1..=steps).map(|k| from.lerp(target, k as f32 / steps as f32)).collect()
    }

    fn effective_brush(&self) -> Brush {
        let mut b = self.brush;
        if !b.sculpt || b.kind==BrushKind::Paint {
            b.kind=BrushKind::Paint;b.sculpt=false;
        }
        if self.modifiers.shift_key() {
            // Smooth with its own Z Intensity (ZBrush: Shift + Z Intensity sets it).
            b = self.workspace.brush_for(&b, BrushKind::Smooth);
            b.sculpt=true;b.paint=false;
        }
        b
    }

    fn begin_stroke(&mut self) {
        self.stroke_brush = self.effective_brush();
        let gpu = self.gfx.is_some() && self.stroke_brush.kind.gpu_drag();
        // Alt is ZBrush's alternate (ZSub) modifier.
        let invert = self.modifiers.alt_key() ^ self.workspace.subtract;
        let symmetry = self.workspace.sym();
        log::trace!("stroke {:?} invert={invert} symmetry={}", self.stroke_brush.kind, symmetry.label());
        self.stroke = Some(
            Stroke::begin(invert, gpu, symmetry)
                .with_assets(self.workspace.assets.clone())
                .with_spacing(self.workspace.spacing)
                .with_morph(self.morph_target()),
        );
        if self.stroke_brush.kind == BrushKind::Morph && self.morph_target().is_none() {
            self.workspace.notify("Morph brush: no Morph Target for this level · Mesh › Morph Target › StoreMT");
        }
        self.stroke_from = self.cursor;
        // LazyMouse trails sculpt and paint brushes; drag brushes follow the cursor.
        self.lazy_point = (self.workspace.lazy_mouse && !self.stroke_brush.kind.is_drag()).then_some(self.cursor);
        // DragRect / DragDot (Stroke): one stamp, placed and sized (or slid) by the drag.
        let kind = self.brush.kind;
        self.drag_stamp = match self.workspace.stroke_mode(kind) {
            "DragRect" | "DragDot" if self.stroke_brush.kind == kind => {
                let (o, d) = self.cursor_ray();
                let under = self.doc.mesh().raycast(o, d).map(|h| (h.point, h.normal));
                Some((self.workspace.stroke_mode(kind) == "DragRect", self.cursor, under))
            }
            _ => None,
        };
        if self.drag_stamp.is_some() {
            self.lazy_point = None;
        }
        self.begin_sculptris();
        self.apply_stroke();
    }

    /// Sculptris Pro, at the start of a stroke that can use it: keep the subtool for undo.
    fn begin_sculptris(&mut self) {
        self.sculptris_before = None;
        self.sculptris_changed = false;
        self.sculptris_at.clear();
        let kind = self.stroke_brush.kind;
        if !self.workspace.sculptris || !Stroke::retessellates(kind) || self.drag_stamp.is_some() || !self.stroke_brush.sculpt {
            return;
        }
        if self.doc.count() > 1 {
            self.workspace.notify("Sculptris Pro needs a subtool without subdivision levels · Del Lower first; this stroke sculpts as usual".to_string());
            return;
        }
        if !self.doc.layers.list.is_empty() {
            self.workspace.notify("Sculptris Pro: bake or delete the layers first · Mesh › Layers; this stroke sculpts as usual".to_string());
            return;
        }
        self.sculptris_capped = false;
        self.sculptris_before = Some(Box::new(sculptris::Before::of(self.doc.mesh())));
        let mesh = self.doc.mesh();
        self.sculptris_track = ((0..mesh.chunks.len() as u32).map(Some).collect(), (0..mesh.vertex_count() as u32).collect());
    }

    /// Sculptris Pro before the dabs at `path`: retessellate around where they land (and
    /// their symmetry images) when the brush has moved on from the last pass.
    fn sculptris_pass(&mut self, path: &[Vec2], radius: f32) {
        // Past this many vertices Sculptris Pro stops adding (sculpting goes on), so a long
        // session at the highest Detail cannot outgrow memory.
        const MAX_VERTICES: usize = 8_000_000;
        if self.doc.mesh().vertex_count() > MAX_VERTICES {
            if !self.sculptris_capped {
                self.sculptris_capped = true;
                self.workspace.notify(format!("Sculptris Pro stops adding past {} vertices · lower Detail or ZRemesher first", fmt_count(MAX_VERTICES)));
            }
            return;
        }
        let hits: Vec<Vec3> = path
            .iter()
            .filter_map(|&p| {
                let (o, d) = self.ray_at(p);
                self.doc.mesh().raycast(o, d).map(|h| h.point)
            })
            .collect();
        let fresh = hits.iter().any(|h| self.sculptris_at.iter().all(|c| c.distance(*h) > radius * 0.3));
        if !fresh {
            return;
        }
        let images = self.workspace.sym().images();
        let centres: Vec<Vec3> = hits.iter().flat_map(|&h| images.of(h)).collect();
        let target = sculptris::target_edge(radius, self.workspace.sculptris_detail);
        self.sculptris_at = hits;
        let t = Instant::now();
        let Some((mesh, stats, origin, carried)) = sculptris::retessellated_tracked(self.doc.mesh(), &centres, radius * 1.15, target, &self.sculptris_track.1) else {
            return;
        };
        let chunks: Vec<Option<u32>> = origin.iter().map(|o| o.and_then(|c| self.sculptris_track.0[c as usize])).collect();
        self.sculptris_track = (chunks, carried);
        if let Some(s) = self.stroke.as_mut() {
            s.rebase();
        }
        // What the GPU copy must take again: vertices from the first chunk that changed,
        // triangles from the first that differs.
        let old = self.doc.mesh();
        let verts = old.chunks.iter().zip(&mesh.chunks).position(|(a, b)| a.vert_end != b.vert_end).map_or(old.vertex_count().min(mesh.vertex_count()), |c| old.chunks[c].vert_start as usize);
        let verts = (0..verts).into_par_iter().find_first(|&v| old.positions[v] != mesh.positions[v]).unwrap_or(verts);
        let shared = old.indices.len().min(mesh.indices.len());
        let tris = (0..shared).into_par_iter().find_first(|&i| old.indices[i] != mesh.indices[i]).unwrap_or(shared) / 3;
        let generation = self.doc.generation;
        let budget = self.doc.budget;
        self.doc = Multires::new(mesh, budget);
        self.sculptris_changed = true;
        let written = self.gfx.as_mut().is_some_and(|gfx| gfx.generation == generation && gfx.rewrite_from(self.doc.mesh_mut(), verts, tris, true));
        if let Some(gfx) = self.gfx.as_mut() {
            if written {
                gfx.generation = self.doc.generation;
            } else {
                // Afresh, with room to grow so the next passes write in place.
                gfx.clear_levels();
                gfx.generation = self.doc.generation;
                let mesh = self.doc.mesh_mut();
                let (n, t, c) = (mesh.vertex_count(), mesh.triangle_count(), mesh.chunks.len());
                let room = render::Room { verts: n + (n / 4).max(1 << 16), tris: t + (t / 4).max(1 << 17), chunks: c + (c / 4).max(64) };
                if !gfx.upload_level_with_room(mesh, 0, room) {
                    self.show_active_level();
                }
            }
        }
        self.update_hover();
        log::debug!("sculptris: {stats:?} in {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
    }

    /// Sculptris Pro at the end of a stroke that changed the mesh: one undo step that puts
    /// the whole subtool back. Returns true when it took the stroke.
    fn end_sculptris(&mut self) -> bool {
        let Some(before) = self.sculptris_before.take() else {
            return false;
        };
        let changed = std::mem::take(&mut self.sculptris_changed);
        self.sculptris_at.clear();
        if !changed {
            return false;
        }
        if let Some(stroke) = self.stroke.take() {
            let _ = stroke.end(self.doc.mesh_mut());
        }
        let tris = (before.idx.len() / 3, self.doc.mesh().triangle_count());
        // Only the region the stroke changed is kept for undo.
        // Only the chunks the stroke rebuilt (and the vertices it moved elsewhere) are kept
        // for undo, and undo puts the layout back exactly.
        let t = Instant::now();
        let (origin, orig) = std::mem::take(&mut self.sculptris_track);
        let step = match sculptris::stroke_undo(&before, self.doc.mesh(), &origin, &orig) {
            Some(undo) => {
                log::info!(
                    "sculptris: undo keeps {:.1} MB (a full copy: {:.1} MB), {:.0} ms",
                    undo.bytes() as f32 / 1e6,
                    (before.pos.len() * 12 + before.idx.len() * 4) as f32 / 1e6,
                    t.elapsed().as_secs_f32() * 1000.0
                );
                UndoStep::Region(Box::new(undo))
            }
            // Should not happen; a whole copy is always right.
            None => {
                log::warn!("sculptris: the stroke's undo is kept as a whole copy");
                let mesh = SculptMesh::from_chunked(before.pos.clone(), before.idx.clone(), &before.ranges);
                let mut doc = Multires::new(mesh, self.doc.budget);
                {
                    let m = doc.mesh_mut();
                    m.mask = before.mask.clone();
                    m.colors = before.colors.clone();
                    if !before.groups.is_empty() {
                        m.set_groups(before.groups.clone());
                    }
                    if !before.hidden.is_empty() {
                        m.set_hidden(before.hidden.clone());
                    }
                }
                UndoStep::Replace(Box::new(doc.pack()))
            }
        };
        self.history.push(step);
        self.history.name_last(self.stroke_brush.kind.name());
        self.morph_targets.remove(&self.subtools[self.active_tool].id);
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        self.status = format!("Sculptris Pro: {} → {} triangles", fmt_count(tris.0), fmt_count(tris.1));
        true
    }

    /// The pressure of the pen in use, or `None` once a mouse moves the pointer (kept up to
    /// date by the tablet's thread). A lost connection turns pressure off.
    fn poll_tablet(&mut self) {
        let Some(t) = self.tablet.as_mut() else {
            return;
        };
        match t.poll() {
            Ok(()) => self.pressure = t.pressure(),
            Err(e) => {
                log::warn!("tablet: {e}; pen pressure is off");
                self.tablet_error = Some(format!("lost the X connection: {e}"));
                self.tablet = None;
                self.pressure = None;
            }
        }
    }

    /// `brush` for one dab at the current pen pressure (Brush > Tablet pressure).
    /// `brush` at the pen's pressure, through the Tablet Pressure curves (Size for Draw
    /// Size, Intensity for Z and RGB Intensity).
    fn at_pressure(&self, brush: Brush) -> Brush {
        let Some(p) = self.pressure else { return brush };
        let t = self.workspace.tablet(brush.kind);
        brush
            .at_pressure(Some(t.size_curve.eval(p)), t.size, false)
            .at_pressure(Some(t.intensity_curve.eval(p)), false, t.intensity)
    }

    fn apply_stroke(&mut self) {
        let t = Instant::now();
        let ray = self.cursor_ray();
        let fwd = self.camera.forward();
        // Pen pressure per dab (drag brushes keep what they picked up with).
        let brush = self.at_pressure(self.stroke_brush);
        let Some(stroke) = self.stroke.as_mut() else {
            return;
        };
        if let Some((rect, anchor, under)) = self.drag_stamp {
            let mut brush = brush;
            let (stamp, axis, ray) = if rect {
                // DragRect: centred on the press, as wide as the drag, turned along it.
                let Some((hit, normal)) = under else { return };
                let drag = self.cursor - anchor;
                if drag.length() < 2.0 {
                    return;
                }
                brush.radius = drag.length() * self.camera.pixel_size_at(hit, self.viewport().y);
                let axis = (self.camera.right() * drag.x - self.camera.up() * drag.y).normalize_or(Vec3::Y);
                ((hit, normal), Some(axis), self.ray_at(anchor))
            } else {
                // DragDot: the stamp follows the pointer.
                let ray = self.cursor_ray();
                let Some(h) = self.doc.mesh().raycast(ray.0, ray.1) else { return };
                ((h.point, h.normal), None, ray)
            };
            self.stroke_brush.radius = brush.radius;
            self.hover = Some(stamp.0);
            if let Some(stroke) = self.stroke.as_mut() {
                stroke.restamp(self.doc.mesh_mut(), &brush, stamp.0, stamp.1, fwd, ray, axis);
            }
            self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
            return;
        }
        if stroke.grabbing() {
            stroke.dab(
                self.doc.mesh_mut(),
                &self.stroke_brush,
                Vec3::ZERO,
                Vec3::Y,
                fwd,
                ray,
            );
        } else {
            let path = self.stroke_path();
            if self.sculptris_before.is_some() {
                self.sculptris_pass(&path, brush.radius);
            }
            for p in path {
                let ray = self.ray_at(p);
                let Some(hit) = self.doc.mesh().raycast(ray.0, ray.1) else {
                    continue;
                };
                self.hover = Some(hit.point);
                if let Some(stroke) = self.stroke.as_mut() {
                    stroke.dab(self.doc.mesh_mut(), &brush, hit.point, hit.normal, fwd, ray);
                }
            }
            self.workspace.lazy_line = self.lazy_point.map(|l| {
                let (a, b) = (l / self.scale, self.cursor / self.scale);
                (egui::pos2(a.x, a.y), egui::pos2(b.x, b.y))
            });
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
        self.end_sculptris();
        if let Some(stroke) = self.stroke.take() {
            let level = self.doc.active();
            self.commit_stroke(stroke, level);
        }
        self.lazy_point = None;
        self.workspace.lazy_line = None;
        self.drag_stamp = None;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.end_gpu_grab();
        }
    }

    /// One undo step for a finished stroke at `level`: its positions and its paint, named
    /// after the brush in Edit › History.
    fn commit_stroke(&mut self, stroke: Stroke, level: usize) {
        let name = self.stroke_brush.kind.name();
        self.commit_stroke_named(stroke, level, name);
    }

    /// [`commit_stroke`](Self::commit_stroke) under `name` in Edit › History.
    fn commit_stroke_named(&mut self, mut stroke: Stroke, level: usize, name: &str) {
        let paint=stroke.take_paint();
        let mut steps=Vec::new();
        if let Some(snapshot) = stroke.end(self.doc.mesh_mut()) {
            let chunks = self.doc.finish_stroke(level, snapshot);
            steps.push(UndoStep::Stroke { level, chunks });
        }
        if !paint.is_empty() {steps.push(UndoStep::Paint(self.doc.finish_paint(level,paint)));}
        match steps.len() {
            0=>return,1=>self.history.push(steps.pop().unwrap()),
            _=>self.history.push(UndoStep::Steps(steps.into_iter().rev().collect())),
        }
        self.history.name_last(name);
    }

    /// Undo (`by` < 0) or redo (`by` > 0) that many steps (Ctrl + Z / Ctrl + Shift + Z, or a
    /// click in Edit › History). Transforms go through the GPU map, as one step does.
    fn history_jump(&mut self, by: i64) {
        if self.busy() || by == 0 {
            return;
        }
        for _ in 0..by.unsigned_abs() {
            let (can, map) = if by < 0 {
                (self.history.can_undo(), self.history.next_undo_map())
            } else {
                (self.history.can_redo(), self.history.next_redo_map())
            };
            if !can {
                break;
            }
            // A placed gizmo goes back and forth with the model.
            if let (Some(m), Some((_, place))) = (map, self.gizmo_place.as_mut()) {
                *place = place.mapped(&m);
            }
            match (map, by < 0) {
                (Some(map), true) => self.transform_doc(&map, |app| app.history.undo(&mut app.doc)),
                (Some(map), false) => self.transform_doc(&map, |app| app.history.redo(&mut app.doc)),
                (None, true) => self.history.undo(&mut self.doc),
                (None, false) => self.history.redo(&mut self.doc),
            };
        }
        self.show_active_level();
        self.subtools[self.active_tool].bounds = self.doc.bounds();
    }

    /// Whether `kind` strokes along a curve (Stroke › Curve).
    fn curve_mode(&self, kind: BrushKind) -> bool {
        curve::always(kind) || (curve::capable(kind) && self.workspace.curve_kinds.contains(&kind))
    }

    /// CurveTube's shape at brush radius `radius` (Stroke › Curve: Tube taper, Tube sides).
    fn tube_shape(&self, radius: f32) -> curve_tube::Tube {
        curve_tube::Tube { radius, taper: self.workspace.tube_taper, sides: self.workspace.tube_sides }
    }

    /// Why CurveTube cannot add geometry to the active subtool now, if it cannot.
    fn tube_refusal(&self) -> Option<&'static str> {
        (self.doc.count() > 1).then_some(
            "CurveTube adds geometry, which needs a subtool without subdivision levels · Del Lower first, or Append a subtool for it",
        )
    }

    /// The point under canvas pixel `p` at the depth of `at` (on the plane through `at`
    /// facing the camera): where a curve continues off the surface.
    fn at_depth(cam: &Camera, vp: Vec2, p: Vec2, at: Vec3) -> Vec3 {
        let (o, d) = cam.ray(p.x, p.y, vp.x, vp.y);
        let f = cam.forward();
        o + d * ((at - o).dot(f) / d.dot(f).max(1e-6))
    }

    /// The applied curve, if it can still be bent: nothing edited since, and the same level
    /// of the same subtool shown. Otherwise it is let go.
    fn live_curve(&mut self) -> Option<&mut CurveTool> {
        let edits = self.history.edits();
        let (level, tool, vertices) = (self.doc.active(), self.active_tool, self.doc.mesh().vertex_count());
        let live = self
            .curve
            .as_ref()
            .is_some_and(|c| c.edits == edits && c.level == level && c.tool == tool && c.vertices == vertices);
        if !live {
            self.curve = None;
        }
        self.curve.as_mut()
    }

    /// Window pixels of a world point in front of the camera.
    fn project_front(&self, p: Vec3) -> Option<Vec2> {
        let clip = self.camera.view_proj() * p.extend(1.0);
        (clip.w > 1e-6).then(|| self.project(p))
    }

    /// Pixels on screen of `r` world units at `at`.
    fn screen_radius(&self, at: Vec3, r: f32) -> f32 {
        let right = self.camera.forward().cross(Vec3::Y).normalize_or(Vec3::X);
        (self.project(at + right * r) - self.project(at)).length()
    }

    /// Lay a screen path (canvas pixels) onto the surface: smoothed by Curve smooth times
    /// the brush's screen radius (when `smooth`), raycast every tenth of that radius, then
    /// resampled every 4 % of `radius` in the world.
    ///
    /// With `free` (CurveTube) the curve does not stop where the path leaves the surface: it
    /// goes on at the depth of the nearest point on it, so a tube can hang off the model.
    fn lay_curve(&self, path: &[Vec2], radius: f32, smooth: bool, free: bool) -> curve::Curve {
        let vp = self.viewport();
        let (cam, mesh) = (&self.camera, self.doc.mesh());
        let hit = |p: &Vec2| {
            let (o, d) = cam.ray(p.x, p.y, vp.x, vp.y);
            mesh.raycast(o, d).map(|h| (h.point, h.normal))
        };
        let Some((at, _)) = path.iter().find_map(hit) else {
            return curve::Curve::default();
        };
        let screen_r = self.screen_radius(at, radius).max(1.0);
        let step = (screen_r * 0.1).clamp(1.0, 6.0);
        let pts = if smooth {
            curve::smooth_path(path, (self.workspace.curve_smooth * screen_r).clamp(0.0, 150.0), step)
        } else {
            curve::resample(path, step)
        };
        let mut hits: Vec<_> = pts.par_iter().map(hit).collect();
        if free {
            // Before the first hit at its depth, after it at the last one's.
            let mut last = None;
            for k in 0..hits.len() {
                match hits[k] {
                    Some(h) => last = Some(h),
                    None => {
                        if let Some((p, n)) = last.or_else(|| hits[k..].iter().flatten().next().copied()) {
                            hits[k] = Some((Self::at_depth(cam, vp, pts[k], p), n));
                            last = hits[k];
                        }
                    }
                }
            }
        }
        curve::Curve::from_hits(&hits, radius * 0.04)
    }

    /// Press on the model with a brush in Curve mode: start drawing a new curve (the
    /// previous one keeps its result and can no longer be bent).
    fn begin_curve(&mut self) {
        let brush = self.effective_brush();
        if brush.kind.is_insert() {
            if let Some(why) = self.tube_refusal() {
                self.workspace.notify(why);
                self.status = why.to_string();
                return;
            }
        }
        self.curve = None;
        self.curve_draw = Some(CurveDraw {
            path: vec![self.cursor - self.canvas_origin()],
            brush,
            invert: self.modifiers.alt_key() ^ self.workspace.subtract,
        });
        self.hover = None;
    }

    /// Release after drawing: lay the path onto the surface as a smooth curve and apply the
    /// brush along it, as one undo step. The curve stays so it can be bent.
    fn end_curve_draw(&mut self) {
        let Some(d) = self.curve_draw.take() else {
            return;
        };
        let t = Instant::now();
        let curve = self.lay_curve(&d.path, d.brush.radius, true, d.brush.kind.is_insert());
        self.workspace.curve_preview = None;
        if curve.is_empty() {
            return;
        }
        if d.brush.kind.is_insert() {
            self.add_tube(curve, d, t);
            return;
        }
        let (symmetry, assets) = (self.workspace.sym(), self.workspace.assets.clone());
        let mut stroke = Stroke::begin(d.invert, false, symmetry).with_assets(assets.clone()).with_morph(self.morph_target());
        let (fwd, step) = (self.camera.forward(), d.brush.radius * self.workspace.curve_step);
        apply_curve(&mut stroke, self.doc.mesh_mut(), &d.brush, &curve, fwd, step);
        let base = stroke.position_snapshots().iter().cloned().collect();
        let base_paint = stroke.paint_snapshots().iter().cloned().collect();
        self.last_dab_count = stroke.last_count;
        let level = self.doc.active();
        self.commit_stroke(stroke, level);
        self.curve = Some(CurveTool {
            curve,
            brush: d.brush,
            invert: d.invert,
            symmetry,
            assets,
            level,
            tool: self.active_tool,
            vertices: self.doc.mesh().vertex_count(),
            edits: self.history.edits(),
            base,
            base_paint,
            bend: None,
            tube: None,
        });
        self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.update_hover();
    }

    /// The polygon view of the active subtool, made afresh when its topology changed since
    /// ZModeler last made it, and with the mesh's positions (sculpting may have moved them).
    fn zmodel_view(&mut self) -> &ZModel {
        let mesh = self.doc.mesh();
        let fresh = self.zmodel.as_ref().is_some_and(|z| z.generation == self.doc.generation && z.face_of.len() == mesh.triangle_count());
        if !fresh {
            let (poly, face_of) = zmodeler::PolyMesh::from_mesh(mesh);
            let vmap = (0..mesh.vertex_count() as u32).collect();
            self.zmodel = Some(ZModel { poly, face_of, vmap, generation: self.doc.generation });
        }
        let z = self.zmodel.as_mut().unwrap();
        for (i, &v) in z.vmap.iter().enumerate() {
            if v != u32::MAX {
                z.poly.pos[i] = mesh.positions[v as usize];
            }
        }
        self.zmodel.as_ref().unwrap()
    }

    /// What ZModeler would act on under the cursor: a point within 9 pixels, else an edge
    /// within 7, else the polygon.
    fn zmodel_hover(&mut self) -> Option<zmodeler::Hover> {
        if self.doc.count() > 1 {
            return None;
        }
        let (o, d) = self.cursor_ray();
        let hit = self.doc.mesh().raycast(o, d)?;
        let at = self.canvas_cursor();
        let view_size = self.viewport();
        let camera = self.camera.clone();
        let z = self.zmodel_view();
        let f = *z.face_of.get(hit.tri as usize)?;
        if f == u32::MAX {
            return None;
        }
        let view = View { camera: &camera, size: view_size };
        let face = &z.poly.faces[f as usize];
        let px: Vec<Vec2> = face.iter().map(|&v| view.project(z.poly.pos[v as usize])).collect();
        let near = (0..face.len()).map(|k| (px[k].distance(at), k)).min_by(|a, b| a.0.total_cmp(&b.0))?;
        if near.0 < 9.0 {
            return Some(zmodeler::Hover::Point(face[near.1]));
        }
        let seg = |a: Vec2, b: Vec2| {
            let ab = b - a;
            let t = ((at - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
            at.distance(a + ab * t)
        };
        let n = face.len();
        let edge = (0..n).map(|k| (seg(px[k], px[(k + 1) % n]), k)).min_by(|a, b| a.0.total_cmp(&b.0))?;
        if edge.0 < 7.0 {
            return Some(zmodeler::Hover::Edge(face[edge.1], face[(edge.1 + 1) % n]));
        }
        Some(zmodeler::Hover::Poly(f))
    }

    /// ZModeler press: start the action of what is under the cursor.
    fn begin_zmodel(&mut self) {
        if self.doc.count() > 1 {
            self.workspace.notify("ZModeler works on a subtool without subdivision levels · Del Lower first".to_string());
            return;
        }
        let Some(hover) = self.zmodel_hover() else { return };
        let before = Box::new(self.doc.pack_copy());
        let at = self.canvas_cursor();
        let view_size = self.viewport();
        let camera = self.camera.clone();
        let ws = (self.workspace.zm_poly, self.workspace.zm_target, self.workspace.zm_edge, self.workspace.zm_edge_loop);
        let base = self.zmodel_view().poly.clone();
        let (faces, anchor, normal, label) = match hover {
            zmodeler::Hover::Poly(f) => {
                // A loop runs across the polygon's edge nearest the cursor.
                let view = View { camera: &camera, size: view_size };
                let face = &base.faces[f as usize];
                let n = face.len();
                let k = (0..n)
                    .min_by(|&a, &b| {
                        let mid = |k: usize| view.project((base.pos[face[k] as usize] + base.pos[face[(k + 1) % n] as usize]) * 0.5);
                        mid(a).distance(at).total_cmp(&mid(b).distance(at))
                    })
                    .unwrap_or(0);
                // Across that edge means through the edge next to it.
                let across = (face[(k + 1) % n], face[(k + 2) % n]);
                let faces = base.target(f, ws.1, Some(across));
                let normal = faces.iter().map(|&g| base.normal(g)).sum::<Vec3>().normalize_or(Vec3::Z);
                (faces, base.centre(f), normal, format!("ZModeler {} {}", ws.0.name(), ws.1.name()))
            }
            zmodeler::Hover::Edge(a, b) => (Vec::new(), (base.pos[a as usize] + base.pos[b as usize]) * 0.5, Vec3::Z, format!("ZModeler {} Edge{}", ws.2.name(), if ws.2 == zmodeler::EdgeAction::Delete && ws.3 { "Loop" } else { "" })),
            zmodeler::Hover::Point(v) => (Vec::new(), base.pos[v as usize], Vec3::Z, "ZModeler Move Point".to_string()),
        };
        self.zm_drag = Some(ZDrag { hover, base, press: self.cursor, before, faces, anchor, normal, label, changed: false });
        self.hover = None;
        // Insert puts the loop where the press is at once.
        if matches!(hover, zmodeler::Hover::Edge(..)) && ws.2 == zmodeler::EdgeAction::Insert {
            self.zmodel_move();
        }
    }

    /// Show the polygons `poly` as the active subtool (during a ZModeler drag).
    fn show_zmodel(&mut self, poly: zmodeler::PolyMesh) {
        let (mesh, face_of, vmap) = poly.to_mesh();
        let budget = self.doc.budget;
        self.doc = Multires::new(mesh, budget);
        self.zmodel = Some(ZModel { poly, face_of, vmap, generation: self.doc.generation });
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.clear_levels();
        }
        self.show_active_level();
        self.subtools[self.active_tool].bounds = self.doc.bounds();
    }

    /// The action at the drag's current amount.
    fn zmodel_result(&self, d: &ZDrag) -> Option<zmodeler::PolyMesh> {
        use zmodeler::{EdgeAction, Hover, PolyAction};
        let view = self.view();
        let delta = self.cursor - d.press;
        let ws = &self.workspace;
        let images = ws.sym().images();
        let b = &d.base;
        // The vertex of `b` at the mirror image `k` of vertex `v`, if there is one.
        let size = d.base.pos.iter().fold(0.0f32, |m, p| m.max(p.abs().max_element())).max(1e-6);
        let mirror = |k: usize, v: u32| -> Option<u32> {
            let want = images.map(k, b.pos[v as usize]);
            let (best, dist) = b.pos.iter().enumerate().map(|(i, p)| (i as u32, p.distance(want))).min_by(|x, y| x.1.total_cmp(&y.1))?;
            (dist < size * 1e-4).then_some(best)
        };
        // Every copy of the target with symmetry (the target first), and whether the copy
        // is mirrored (an odd number of mirrors turns polygons round).
        let copies = |verts: &[u32]| -> Vec<(Vec<u32>, bool)> {
            let mut out: Vec<(Vec<u32>, bool)> = vec![(verts.to_vec(), false)];
            for k in 1..images.len() {
                let Some(m) = verts.iter().map(|&v| mirror(k, v)).collect::<Option<Vec<u32>>>() else { continue };
                let mut sorted = m.clone();
                sorted.sort_unstable();
                if out.iter().any(|(o, _)| {
                    let mut s2 = o.clone();
                    s2.sort_unstable();
                    s2 == sorted
                }) {
                    continue;
                }
                out.push((m, images.mirrors(k)));
            }
            out
        };
        Some(match d.hover {
            Hover::Poly(_) => {
                // The target's polygons and their mirror images.
                let mut faces = d.faces.clone();
                for &f in &d.faces {
                    for (verts, _) in copies(&b.faces[f as usize]).into_iter().skip(1) {
                        if let Some(g) = b.face_with(&verts) {
                            if !faces.contains(&g) {
                                faces.push(g);
                            }
                        }
                    }
                }
                match ws.zm_poly {
                    PolyAction::QMesh | PolyAction::Extrude => {
                        // Along the normal's screen direction, at its screen speed.
                        let len = self.camera.pixel_size_at(d.anchor, self.viewport().y) * 100.0;
                        let s = (view.project(d.anchor + d.normal * len) - view.project(d.anchor)) / len;
                        // Seen nearly head on the normal barely moves on screen: up pulls out.
                        let px = self.camera.pixel_size_at(d.anchor, self.viewport().y);
                        let dist = if s.length() * px < 0.35 { -delta.y * px } else { delta.dot(s) / s.length_squared() };
                        if delta.length() < 2.0 {
                            return None;
                        }
                        b.extrude(&faces, dist)
                    }
                    PolyAction::Inset => {
                        if delta.length() < 2.0 {
                            return None;
                        }
                        b.inset(&faces, (delta.length() / 150.0).min(0.9))
                    }
                    PolyAction::Delete => b.delete(&faces),
                    PolyAction::Polygroup => b.regroup(&faces),
                    PolyAction::Bridge => {
                        // The second click: from the polygon picked first to this one.
                        let first = self.zm_bridge.as_ref()?;
                        let Hover::Poly(g) = d.hover else { return None };
                        let mut out = b.clone();
                        let pairs: Vec<(Vec<u32>, Vec<u32>)> = copies(first)
                            .into_iter()
                            .zip(copies(&b.faces[g as usize]))
                            .map(|((x, _), (y, _))| (x, y))
                            .collect();
                        for (x, y) in pairs {
                            let (Some(f1), Some(f2)) = (out.face_with(&x), out.face_with(&y)) else { continue };
                            out = out.bridge(f1, f2)?;
                        }
                        out
                    }
                }
            }
            Hover::Edge(a0, b0) => {
                let edges: Vec<((u32, u32), bool)> = copies(&[a0, b0]).into_iter().map(|(v, m)| ((v[0], v[1]), m)).collect();
                match ws.zm_edge {
                    EdgeAction::Insert => {
                        let (pa, pb) = (view.project(b.pos[a0 as usize]), view.project(b.pos[b0 as usize]));
                        let at = self.canvas_cursor();
                        let ab = pb - pa;
                        let t = ((at - pa).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.05, 0.95);
                        let mut out = b.clone();
                        let mut cut: Vec<u32> = Vec::new();
                        for ((x, y), _) in edges {
                            // A copy whose ring the loop already went round gets no second one.
                            let ring: Vec<u32> = out.ring(x, y).iter().map(|r| r.0).collect();
                            if ring.iter().any(|f| cut.contains(f)) {
                                continue;
                            }
                            cut.extend(ring);
                            out = out.insert_loop(x, y, t);
                        }
                        out
                    }
                    EdgeAction::Delete => {
                        let mut all = Vec::new();
                        for ((x, y), _) in edges {
                            if ws.zm_edge_loop {
                                all.extend(b.edge_loop(x, y));
                            } else {
                                all.push((x, y));
                            }
                        }
                        b.dissolve(&all)
                    }
                    EdgeAction::Bevel => {
                        if delta.length() < 2.0 {
                            return None;
                        }
                        let t = (delta.length() / 300.0).clamp(0.02, 0.45);
                        let mut out = b.clone();
                        let mut done: Vec<(u32, u32)> = Vec::new();
                        for ((x, y), _) in edges {
                            if done.iter().any(|&(p, q)| (p, q) == (x, y) || (p, q) == (y, x)) {
                                continue;
                            }
                            done.extend(out.edge_loop(x, y));
                            out = out.bevel(x, y, t);
                        }
                        out
                    }
                    EdgeAction::Spin => {
                        let mut out = b.clone();
                        for ((x, y), mirrored) in edges {
                            out = out.spin(x, y, !mirrored);
                        }
                        out
                    }
                }
            }
            Hover::Point(v) => {
                if delta.length() < 1.0 {
                    return None;
                }
                let m = transform::move_map(&view, d.anchor, d.press - self.canvas_origin(), self.canvas_cursor());
                let mut out = b.clone();
                out = out.moved(&[v], m.translate);
                for k in 1..images.len() {
                    if let Some(w) = mirror(k, v) {
                        if w != v {
                            out = out.moved(&[w], images.map(k, m.translate));
                        }
                    }
                }
                out
            }
        })
    }

    /// Whether the ZModeler action on `hover` happens on release, without a drag.
    fn zmodel_click(&self, hover: zmodeler::Hover) -> bool {
        use zmodeler::{EdgeAction, Hover, PolyAction};
        match hover {
            Hover::Poly(_) => matches!(self.workspace.zm_poly, PolyAction::Delete | PolyAction::Polygroup | PolyAction::Bridge),
            Hover::Edge(..) => matches!(self.workspace.zm_edge, EdgeAction::Delete | EdgeAction::Spin),
            Hover::Point(_) => false,
        }
    }

    /// Follow the cursor during a ZModeler drag.
    fn zmodel_move(&mut self) {
        let Some(d) = self.zm_drag.as_ref() else { return };
        if self.zmodel_click(d.hover) {
            return;
        }
        if let Some(poly) = self.zmodel_result(d) {
            self.show_zmodel(poly);
            self.zm_drag.as_mut().unwrap().changed = true;
        }
    }

    /// ZModeler release: click actions happen now; the action is one undo step.
    fn end_zmodel(&mut self) {
        let Some(d) = self.zm_drag.take() else { return };
        let mut changed = d.changed;
        // Bridge: the first click picks a polygon, the second bridges to it.
        if let (zmodeler::Hover::Poly(f), zmodeler::PolyAction::Bridge, None) = (d.hover, self.workspace.zm_poly, &self.zm_bridge) {
            self.zm_bridge = Some(d.base.faces[f as usize].clone());
            self.workspace.notify("Bridge: click the polygon to join it to".to_string());
            return;
        }
        let click = self.zmodel_click(d.hover);
        if click {
            if let Some(poly) = self.zmodel_result(&d) {
                self.show_zmodel(poly);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.zm_bridge = None;
        self.history.push(UndoStep::Replace(d.before));
        self.history.name_last(&d.label);
        self.morph_targets.remove(&self.subtools[self.active_tool].id);
        self.status = format!("{}: {} polygons", d.label, fmt_count(self.zmodel.as_ref().map_or(0, |z| z.poly.faces.len())));
        self.update_zmodel_hover();
    }

    /// Keep what ZModeler would act on under the cursor up to date.
    fn update_zmodel_hover(&mut self) {
        let on = self.brush.kind.is_zmodeler() && self.workspace.mode == Mode::Draw && !self.busy() && !self.in_canvas_border();
        self.zm_hover = if on { self.zmodel_hover() } else { None };
    }

    /// Insert Mesh press on the model: the mesh goes in there at Draw Size. With
    /// subdivision levels it says why it cannot.
    fn begin_insert(&mut self) {
        if self.doc.count() > 1 {
            self.workspace.notify("Insert Mesh adds geometry, which needs a subtool without subdivision levels · Del Lower first, or Append a subtool for it".to_string());
            return;
        }
        let (o, d) = self.cursor_ray();
        let Some(hit) = self.mesh().raycast(o, d) else {
            return;
        };
        let normal = if hit.normal.dot(d) > 0.0 { -hit.normal } else { hit.normal };
        let mesh = self.mesh();
        self.insert = Some(InsertDrag {
            keep: mesh.extent(),
            at: hit.point,
            normal,
            press: self.cursor,
            radius: self.brush.radius,
            group: mesh.max_group().saturating_add(1),
            edge: insert_mesh::mean_edge(mesh),
            shape: self.workspace.insert_shape,
            symmetry: self.workspace.sym(),
            mask: mesh.mask.clone(),
        });
        self.hover = None;
        self.place_insert();
    }

    /// Put the dragged mesh in again at its current size.
    fn place_insert(&mut self) {
        let Some(d) = self.insert.as_ref() else {
            return;
        };
        let segments = insert_mesh::segments_for(d.shape, d.radius, d.edge);
        let part = insert_mesh::inserted(d.shape, segments, d.at, d.normal, d.radius, d.symmetry, d.group);
        let keep = d.keep;
        self.doc.swap_part_in_place(keep, Some(&part));
        self.show_swapped_part(keep);
    }

    /// The drag sizes the mesh: its radius reaches from the press point to the cursor (in
    /// the view plane through it). A click keeps Draw Size.
    fn insert_move(&mut self) {
        let to = self.canvas_cursor();
        let view = self.view();
        let Some(d) = self.insert.as_ref() else {
            return;
        };
        if (self.cursor - d.press).length() < 4.0 {
            return;
        }
        let radius = (view.unproject_at(to, d.at) - d.at).length().max(d.edge);
        if (radius / d.radius - 1.0).abs() > 0.01 {
            self.insert.as_mut().unwrap().radius = radius;
            self.place_insert();
        }
    }

    /// Release: the inserted mesh stays, the rest of the subtool is masked (ZBrush), as one
    /// undo step.
    fn end_insert(&mut self) {
        let Some(d) = self.insert.take() else {
            return;
        };
        let n = self.mesh().vertex_count();
        let mut mask = vec![255u8; d.keep.verts];
        mask.resize(n, 0);
        crate::mask::replace(self.doc.mesh_mut(), mask);
        self.history.push(UndoStep::Steps(vec![
            UndoStep::Part { keep: d.keep, part: None },
            UndoStep::Mask { level: 0, data: crate::mask::MaskData::Whole(d.mask) },
        ]));
        self.history.name_last(&format!("Insert {}", d.shape.short()));
        // A live CurveTube curve lets go: its part is no longer the last.
        self.curve = None;
        self.update_hover();
        self.status = format!("Inserted {} (radius {:.3}): {} vertices as polygroup {}", d.shape.name(), d.radius, fmt_count(n - d.keep.verts), d.group);
        log::info!("{}", self.status);
    }

    /// CurveTube release: a tube along `curve` (and its mirror image with symmetry) added
    /// to the active subtool as a new polygroup, as one undo step. The curve stays so the
    /// tube can be bent and resized.
    fn add_tube(&mut self, curve: curve::Curve, d: CurveDraw, t: Instant) {
        if let Some(why) = self.tube_refusal() {
            self.workspace.notify(why);
            return;
        }
        let (symmetry, shape) = (self.workspace.sym(), self.tube_shape(d.brush.radius));
        let keep = self.mesh().extent();
        let group = self.mesh().max_group().saturating_add(1);
        let Some(part) = curve_tube::tubes(&curve, shape, symmetry, group) else {
            return;
        };
        let added = part.vertex_count();
        self.doc.swap_part_in_place(keep, Some(&part));
        self.history.push(UndoStep::Part { keep, part: None });
        self.show_swapped_part(keep);
        self.curve = Some(CurveTool {
            curve,
            brush: d.brush,
            invert: d.invert,
            symmetry,
            assets: self.workspace.assets.clone(),
            level: 0,
            tool: self.active_tool,
            vertices: self.mesh().vertex_count(),
            edits: self.history.edits(),
            base: Default::default(),
            base_paint: Default::default(),
            bend: None,
            tube: Some(TubePart { keep, group, shape }),
        });
        self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.status = format!("CurveTube: added {} vertices as polygroup {group} in {:.0} ms", fmt_count(added), self.stroke_ms);
        log::info!("{}", self.status);
    }

    /// Put a new tube along the live CurveTube curve `curve` with `shape`, in place of the
    /// one it made before (a bend, or a new Draw Size, taper or sides).
    fn rebuild_tube(&mut self, curve: Option<curve::Curve>, shape: curve_tube::Tube) {
        let Some(c) = self.curve.as_mut() else {
            return;
        };
        let Some(tube) = c.tube.as_mut() else {
            return;
        };
        if let Some(curve) = curve {
            c.curve = curve;
        }
        tube.shape = shape;
        let part = curve_tube::tubes(&c.curve, shape, c.symmetry, tube.group);
        let keep = tube.keep;
        self.doc.swap_part_in_place(keep, part.as_ref());
        let vertices = self.doc.mesh().vertex_count();
        self.curve.as_mut().unwrap().vertices = vertices;
        self.show_swapped_part(keep);
    }

    /// Show the active subtool after everything after `keep` changed in place: only that
    /// part is written to the GPU while its buffers have room; otherwise the level is
    /// uploaded afresh with room for more (so the next tubes and bends go in place).
    fn show_swapped_part(&mut self, keep: mesh::Extent) {
        self.subtools[self.active_tool].bounds = self.doc.bounds();
        let Some(gfx) = self.gfx.as_mut() else {
            return self.update_hover();
        };
        let generation = self.doc.generation;
        let mesh = self.doc.mesh_mut();
        if gfx.generation == generation && gfx.swap_tail(mesh, keep) {
            return self.update_hover();
        }
        let (n, t, c) = (mesh.vertex_count(), mesh.triangle_count(), mesh.chunks.len());
        let (pn, pt, pc) = (n - keep.verts, t - keep.tris, c - keep.chunks);
        let room = render::Room {
            verts: n + (n / 16).max(4 * pn).max(1 << 16),
            tris: t + (t / 16).max(4 * pt).max(1 << 17),
            chunks: c + (c / 16).max(4 * pc).max(64),
        };
        gfx.clear_levels();
        gfx.generation = generation;
        if !gfx.upload_level_with_room(mesh, 0, room) {
            self.show_active_level();
            return;
        }
        self.update_hover();
    }

    /// A live CurveTube follows Draw Size, Tube taper and Tube sides while CurveTube is the
    /// brush (ZBrush: change the size to update the curve's mesh).
    fn follow_tube_settings(&mut self) {
        if self.busy() || !self.brush.kind.is_insert() {
            return;
        }
        let Some(tube) = self.live_curve().and_then(|c| c.tube) else {
            return;
        };
        let shape = self.tube_shape(self.brush.radius);
        if shape != tube.shape {
            self.rebuild_tube(None, shape);
        }
    }

    /// Bend a CurveTube curve: points pulled with the falloff snap to the model where it is
    /// under them (the tube itself left out) and stay at their own depth where it is not, so
    /// a tube can be pulled off the surface; points the pull does not reach stay put.
    fn bend_tube(&mut self) {
        let (vp, cursor) = (self.viewport(), self.cursor);
        let Some(c) = self.curve.as_mut() else {
            return;
        };
        let (Some(b), Some(tube)) = (c.bend.as_mut(), c.tube) else {
            return;
        };
        b.pending = false;
        let Some((from, _)) = b.tube_from.as_ref() else {
            return;
        };
        let delta = cursor - b.press;
        let (grab, reach) = (b.grab, b.reach);
        let along = &b.along;
        let pull = |i: usize| {
            let x = ((along[i] - grab) / reach).abs();
            if x >= 1.0 { 0.0 } else { (1.0 - x * x).powi(2) }
        };
        let (cam, mesh) = (&self.camera, self.doc.mesh());
        let mut slots: Vec<Option<usize>> = Vec::new();
        for piece in &b.pieces {
            slots.extend(piece.clone().map(Some));
            slots.push(None);
        }
        // Ok: kept or snapped to the model; Err: pulled off it, placed below.
        let found: Vec<Option<Result<(Vec3, Vec3), (Vec2, Vec3)>>> = slots
            .par_iter()
            .map(|slot| {
                let i = (*slot)?;
                let (p, n) = (from.points[i], from.normals[i]);
                let w = pull(i);
                let Some(s) = b.screen[i].filter(|_| w > 1e-4) else {
                    return Some(Ok((p, n)));
                };
                let q = s + delta * w;
                let (o, d) = cam.ray(q.x, q.y, vp.x, vp.y);
                Some(mesh.raycast_in(o, d, tube.keep.chunks).map(|h| (h.point, h.normal)).ok_or((q, n)))
            })
            .collect();
        // Off the model a point goes on at the depth of the point before it on the curve
        // (the one after it at the start of a piece), as when the curve was drawn, so the
        // tube does not jump in depth where it leaves the surface.
        let mut hits: Vec<Option<(Vec3, Vec3)>> = vec![None; found.len()];
        let mut k = 0;
        while k < found.len() {
            let end = (k..found.len()).find(|&j| found[j].is_none()).unwrap_or(found.len());
            let mut last: Option<Vec3> = None;
            for j in k..end {
                hits[j] = match found[j] {
                    Some(Ok(h)) => Some(h),
                    Some(Err((q, n))) => {
                        let near = last.or_else(|| found[j..end].iter().find_map(|f| f.and_then(Result::ok)).map(|h| h.0));
                        let at = near.unwrap_or(from.points[slots[j].unwrap()]);
                        Some((Self::at_depth(cam, vp, q, at), n))
                    }
                    None => None,
                };
                last = hits[j].map(|h| h.0);
            }
            k = end + 1;
        }
        let curve = curve::Curve::from_hits(&hits, c.brush.radius * 0.04);
        if !curve.is_empty() {
            self.rebuild_tube(Some(curve), tube.shape);
        }
    }

    /// Press within reach of the applied curve: start bending it (ZBrush: drag a curve to
    /// reshape it; the brush is re-applied along the new shape).
    fn begin_curve_bend(&mut self) -> bool {
        if self.live_curve().is_none() {
            return false;
        }
        let c = self.curve.as_ref().unwrap();
        let origin = self.canvas_origin();
        let screen: Vec<Option<Vec2>> = c.curve.points.iter().map(|&p| self.project_front(p).map(|q| q - origin)).collect();
        let mut along = vec![0.0; screen.len()];
        let mut total = 0.0;
        for piece in &c.curve.pieces {
            for i in piece.clone() {
                along[i] = total + c.curve.along[i];
            }
            total += c.curve.along[piece.end - 1];
        }
        let cursor = self.cursor - origin;
        let nearest = screen
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|s| (s.distance(cursor), i)))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((d, i)) = nearest else {
            return false;
        };
        if d > CURVE_GRAB_PX / self.scale {
            return false;
        }
        let reach = (total * 0.35).max(c.brush.radius * 2.0);
        let stroke = Stroke::begin(c.invert, false, c.symmetry).with_assets(c.assets.clone()).with_morph(self.morph_target());
        let grab = along[i];
        let press = self.cursor;
        let pieces = c.curve.pieces.clone();
        let tube_from = c.tube.map(|t| (c.curve.clone(), self.doc.mesh().tail(t.keep).map(Box::new)));
        self.curve.as_mut().unwrap().bend = Some(CurveBend { stroke, pieces, screen, along, grab, reach, press, pending: false, tube_from });
        self.hover = None;
        true
    }

    /// Re-apply a bent curve: put back the surface it was first applied to, move its points
    /// on the screen with a smooth falloff from the grabbed point, lay them onto the surface
    /// again and apply the brush along the new shape.
    fn bend_curve(&mut self) {
        if self.curve.as_ref().is_some_and(|c| c.tube.is_some()) {
            self.bend_tube();
            return;
        }
        let vp = self.viewport();
        let (fwd, cursor) = (self.camera.forward(), self.cursor);
        let step_share = self.workspace.curve_step;
        let Some(c) = self.curve.as_mut() else {
            return;
        };
        let Some(b) = c.bend.as_mut() else {
            return;
        };
        b.pending = false;
        let delta = cursor - b.press;
        let pull = |i: usize| {
            let x = ((b.along[i] - b.grab) / b.reach).abs();
            if x >= 1.0 { 0.0 } else { (1.0 - x * x).powi(2) }
        };
        // At most ~200 raycasts: the curve is resampled finely again in the world.
        let stride = (b.screen.len() / 200).max(1);
        let mut pts: Vec<Option<Vec2>> = Vec::new();
        for piece in &b.pieces {
            for i in piece.clone() {
                if (i - piece.start) % stride == 0 || i + 1 == piece.end {
                    pts.push(b.screen[i].map(|s| s + delta * pull(i)));
                }
            }
            pts.push(None);
        }
        let mesh = self.doc.mesh_mut();
        // Record for undo what the curve covers now, then restore its base.
        let chunks: Vec<u32> = c.base.keys().copied().collect();
        b.stroke.snapshot_now(mesh, &chunks);
        for (&k, values) in &c.base {
            let (a, e) = mesh.chunks[k as usize].range();
            mesh.positions[a..e].copy_from_slice(values);
        }
        if !c.base_paint.is_empty() && !mesh.colors.is_empty() {
            for (&k, values) in &c.base_paint {
                let (a, e) = mesh.chunks[k as usize].range();
                mesh.colors[a..e].copy_from_slice(values);
                mesh.chunks[k as usize].dirty_color = true;
            }
        }
        mesh.finish_edit(&chunks);
        let cam = &self.camera;
        let mesh_ref: &SculptMesh = mesh;
        let hits: Vec<_> = pts
            .par_iter()
            .map(|p| {
                p.and_then(|p| {
                    let (o, d) = cam.ray(p.x, p.y, vp.x, vp.y);
                    mesh_ref.raycast(o, d).map(|h| (h.point, h.normal))
                })
            })
            .collect();
        let curve = curve::Curve::from_hits(&hits, c.brush.radius * 0.04);
        apply_curve(&mut b.stroke, mesh, &c.brush, &curve, fwd, c.brush.radius * step_share);
        for (k, v) in b.stroke.position_snapshots() {
            c.base.entry(*k).or_insert_with(|| v.clone());
        }
        for (k, v) in b.stroke.paint_snapshots() {
            c.base_paint.entry(*k).or_insert_with(|| v.clone());
        }
        if !curve.is_empty() {
            c.curve = curve;
        }
    }

    /// Release after bending: one undo step for the bend; the curve stays bendable.
    fn end_curve_bend(&mut self) {
        if self.curve.as_ref().is_some_and(|c| c.bend.as_ref().is_some_and(|b| b.pending)) {
            self.bend_curve();
        }
        let Some(c) = self.curve.as_mut() else {
            return;
        };
        let Some(b) = c.bend.take() else {
            return;
        };
        let level = c.level;
        match (c.tube, b.tube_from) {
            (Some(tube), Some((_, before))) => self.history.push(UndoStep::Part { keep: tube.keep, part: before }),
            _ => self.commit_stroke(b.stroke, level),
        }
        let edits = self.history.edits();
        if let Some(c) = self.curve.as_mut() {
            c.edits = edits;
        }
        self.update_hover();
    }

    /// The curve on the canvas for the overlay: the path being drawn, or the applied curve
    /// (highlighted when the cursor is on it or it is being bent).
    fn curve_preview(&mut self) -> Option<workspace::CurvePreview> {
        let origin = self.canvas_origin();
        let scale = self.scale;
        let logical = |q: Vec2| egui::pos2(q.x / scale, q.y / scale);
        if let Some(d) = &self.curve_draw {
            let pieces = vec![d.path.iter().map(|&p| logical(p + origin)).collect()];
            return Some(workspace::CurvePreview { pieces, drawing: true, hot: false });
        }
        self.live_curve()?;
        let c = self.curve.as_ref()?;
        let mut pieces = Vec::new();
        let mut nearest = f32::INFINITY;
        for piece in &c.curve.pieces {
            let mut run = Vec::new();
            for i in piece.clone() {
                match self.project_front(c.curve.points[i]) {
                    Some(q) => {
                        nearest = nearest.min(q.distance(self.cursor));
                        run.push(logical(q));
                    }
                    None => {
                        if run.len() > 1 {
                            pieces.push(std::mem::take(&mut run));
                        }
                        run.clear();
                    }
                }
            }
            if run.len() > 1 {
                pieces.push(run);
            }
        }
        let hot = c.bend.is_some() || nearest <= CURVE_GRAB_PX / scale;
        Some(workspace::CurvePreview { pieces, drawing: false, hot })
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

    /// A whole-mask operation as one undo step (Masking palette, Ctrl + I / A / Shift + A,
    /// Ctrl + click). Returns false when it changed nothing.
    fn mask_op(&mut self, op: MaskOp) -> bool {
        if self.busy() {
            return false;
        }
        let level = self.doc.active();
        let mesh = self.doc.mesh_mut();
        let new = mask::apply(mesh, op);
        // Invert and Mask All act on what is shown: vertices of hidden polygons only keep
        // their mask.
        let new = if matches!(op, MaskOp::Invert | MaskOp::All) && !mesh.shown.is_empty() && !new.is_empty() {
            let (old, shown) = (&mesh.mask, &mesh.shown);
            new.par_iter()
                .enumerate()
                .map(|(v, &m)| if shown[v] != 0 { m } else { old.get(v).copied().unwrap_or(0) })
                .collect()
        } else {
            new
        };
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

    /// The brush a held modifier strokes with, when it is not the brush itself (ZBrush
    /// shows it in the brush panels while the key is down): Ctrl the remembered mask brush,
    /// Shift Smooth for sculpt and paint brushes (masks and shape tools keep their own:
    /// Shift snaps or smooths with them). Ctrl + Shift (selection) and Alt (the alternate
    /// side of the same brush) show the brush itself.
    fn held_brush(&self) -> Option<BrushKind> {
        if self.egui_ctx.egui_wants_keyboard_input() {
            return None;
        }
        let (ctrl, shift) = (self.modifiers.control_key(), self.modifiers.shift_key());
        let kind = self.brush.kind;
        let shown = match (ctrl, shift) {
            (true, false) => self.workspace.mask_brush,
            (false, true) if !kind.is_mask() && !kind.is_shape() => BrushKind::Smooth,
            _ => return None,
        };
        (shown != kind).then_some(shown)
    }

    /// The half (+1 or -1 along X) a Clip Line was drawn on, for X symmetry: the side of
    /// most of the surface under the guide line, or, with no surface under it or as much on
    /// each side, the side the camera is on.
    fn clip_side(&self, g: &surface_tools::Gesture) -> f32 {
        let (vp, cam, mesh) = (self.viewport(), &self.camera, self.mesh());
        let sum: f32 = (0..=64)
            .into_par_iter()
            .filter_map(|k| {
                let p = g.start.lerp(g.end, k as f32 / 64.0);
                let (o, d) = cam.ray(p.x, p.y, vp.x, vp.y);
                mesh.raycast(o, d).map(|h| h.point.x.signum())
            })
            .sum();
        if sum != 0.0 {
            sum.signum()
        } else if self.camera.eye().x < 0.0 {
            -1.0
        } else {
            1.0
        }
    }

    fn surface_screen(&self) -> surface_tools::Screen {
        surface_tools::Screen {matrix:self.camera.view_proj(),size:self.viewport()}
    }

    fn begin_gesture(&mut self,kind:BrushKind) {
        self.gesture=Some(surface_tools::Gesture::new(kind,self.cursor-self.canvas_origin(),self.modifiers.alt_key()));
        self.gesture.as_mut().unwrap().ctrl=self.modifiers.control_key();
        self.update_gesture();self.hover=None;
    }

    fn update_gesture(&mut self) {
        let origin=self.canvas_origin();
        if let Some(g)=self.gesture.as_mut() {
            g.update(self.cursor-origin,self.modifiers.shift_key(),self.modifiers.alt_key());
            let mut preview=g.clone();
            preview.start=(preview.start+origin)/self.scale;
            preview.end=(preview.end+origin)/self.scale;
            for p in &mut preview.points {*p=(*p+origin)/self.scale;}
            self.workspace.gesture_preview=Some(preview);
        }
    }

    fn end_gesture(&mut self)->bool {
        let Some(g)=self.gesture.take() else{return false;};
        self.workspace.gesture_preview=None;
        if g.ctrl && g.kind.is_mask() && g.start.distance(g.end)<CLICK_SLOP && g.points.len()<3 {
            let surface=!self.in_canvas_border() && self.mesh().raycast(self.cursor_ray().0,self.cursor_ray().1).is_some();
            self.mask_op(if surface {if g.alternate{MaskOp::Sharpen}else{MaskOp::Blur}}else{MaskOp::Invert});
            return true;
        }
        let screen=self.surface_screen();let level=self.doc.active();
        match g.kind {
            BrushKind::ClipLine => {
                let symmetry=self.workspace.symmetry.then(||self.clip_side(&g));
                if let Some(before)=surface_tools::clip_line(self.doc.mesh_mut(),screen,&g,symmetry) {
                    let chunks=self.doc.finish_stroke(level,before);
                    self.history.push(UndoStep::Stroke {level,chunks});
                    self.subtools[self.active_tool].bounds=self.doc.bounds();
                }
            }
            BrushKind::ClipRect | BrushKind::ClipCircle => {
                if let Some(before)=surface_tools::clip_shape(self.doc.mesh_mut(),screen,&g,self.workspace.sym()) {
                    let chunks=self.doc.finish_stroke(level,before);
                    self.history.push(UndoStep::Stroke {level,chunks});
                    self.history.name_last(g.kind.name());
                    self.subtools[self.active_tool].bounds=self.doc.bounds();
                }
            }
            BrushKind::SelectRect | BrushKind::SelectLasso => {
                if g.tiny() {
                    // A click: isolate the polygroup under it, or on the background show all.
                    let hit=if self.in_canvas_border() {None} else {let (o,d)=self.cursor_ray();self.mesh().raycast(o,d).map(|h|h.tri)};
                    match hit {
                        Some(tri) => self.isolate_group(tri, g.alternate),
                        None => self.show_all(),
                    }
                } else {
                    let inside=surface_tools::tris_in_gesture(self.doc.mesh(),screen,&g);
                    self.select_tris(inside,g.alternate,g.kind.name());
                }
            }
            BrushKind::TrimCurve | BrushKind::SliceCurve | BrushKind::KnifeCurve => {
                if !g.tiny() {
                    let side=self.workspace.symmetry.then(||self.clip_side(&g));
                    let what=match g.kind {
                        BrushKind::TrimCurve=>cut::LineCut::Trim,
                        BrushKind::SliceCurve=>cut::LineCut::Slice,
                        _=>cut::LineCut::Knife,
                    };
                    if let Some(d)=surface_tools::line_distance(self.doc.mesh(),screen,&g,side) {
                        self.topology_op(g.kind.name(),|m|cut::line_cut(m,&d,what));
                    }
                }
            }
            BrushKind::TrimRect | BrushKind::TrimCircle | BrushKind::TrimLasso => self.trim_shape(&g),
            _ => {
                if let Some(new)=surface_tools::mask_shape(self.doc.mesh(),screen,&g,self.workspace.settings_of(&self.brush,g.kind).strength,self.workspace.sym()) {
                    let old=mask::replace(self.doc.mesh_mut(),new);
                    self.history.push(UndoStep::Mask {level,data:MaskData::Whole(old)});
                }
            }
        }
        self.update_hover();true
    }

    /// Ctrl + left press (ZBrush masking): decided on the first move or on release.
    fn begin_mask_press(&mut self, border: bool) {
        let surface = if border {
            None
        } else {
            let (o, d) = self.cursor_ray();
            self.mesh().raycast(o, d)
        };
        self.mask_press = Some(MaskPress {
            at: self.cursor,
            hit: surface.as_ref().map(|h|h.point),
            alt: self.modifiers.alt_key(),
            moved: false,
            kind: self.workspace.mask_brush,
            direct: self.brush.kind.is_mask() && !self.modifiers.control_key(),
            normal: surface.map_or(Vec3::Z,|h|h.normal),
        });
        self.hover = None;
    }

    /// One mask dab under the cursor.
    fn mask_dab(&mut self) {
        let t = Instant::now();
        let (o, d) = self.cursor_ray();
        let Some(hit) = self.mesh().raycast(o, d) else {
            return;
        };
        let brush = self.at_pressure(self.workspace.brush_for(&self.brush, self.workspace.mask_brush));
        if let Some(stroke) = self.mask_stroke.as_mut() {
            stroke.dab_normal(self.doc.mesh_mut(), &brush, hit.point, hit.normal);
        }
        self.hover = Some(hit.point);
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
        let mut stroke = mask::MaskStroke::begin(press.alt, self.workspace.sym())
            .with_brush(press.kind,self.workspace.assets.clone());
        let brush = self.at_pressure(self.workspace.brush_for(&self.brush, press.kind));
        stroke.dab_normal(self.doc.mesh_mut(), &brush, hit, press.normal);
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
        if press.direct && !press.moved {
            if let Some(hit)=press.hit {
                let mut stroke=mask::MaskStroke::begin(press.alt,self.workspace.sym())
                    .with_brush(press.kind,self.workspace.assets.clone());
                stroke.dab_normal(self.doc.mesh_mut(),&self.brush,hit,press.normal);
                if let Some(chunks)=stroke.end() {self.history.push(UndoStep::Mask {level:self.doc.active(),data:MaskData::Chunks(chunks)});}
            }
            return;
        }
        if press.hit.is_none() && press.moved && self.subtools[self.active_tool].wedge {
            // ZBrush: while DynaMesh is on, Ctrl + drag on the background remeshes; the new
            // surface starts without a mask (undo brings back the old one with its mask).
            self.pending_geo = Some(GeoOp::Wedge);
            self.run_pending_geo();
            return;
        }
        let op = match (press.hit.is_some(), press.moved, press.alt) {
            (true, false, false) => Some(MaskOp::Blur),
            (true, false, true) => Some(MaskOp::Sharpen),
            (false, false, _) => Some(MaskOp::Invert),
            (false, true, _) => Some(MaskOp::Clear),
            (true, true, _) => None,
        };
        if let Some(op) = op {
            let changed = self.mask_op(op);
            if op == MaskOp::Clear && !changed {
                // Nothing to clear: say how this gesture remeshes (ZBrush: re-DynaMesh).
                self.workspace.notify("Ctrl + drag on the background remeshes while Remesh is on · Mesh › Remesh");
            }
        }
    }

    /// The cursor moved to `pos` (window pixels).
    fn cursor_moved(&mut self, pos: Vec2, over_ui: bool) {
        self.last_cursor = self.cursor;
        self.cursor = pos;
        if self.workspace.modal_open() || self.workspace.brush_catalog_open() {
            self.hover = None;
            return;
        }
        let delta = self.cursor - self.last_cursor;
        if self.gesture.is_some() {
            self.update_gesture();
        } else if let Some(drag) = self.nav {
            self.nav_move(drag, delta);
        } else if self.select.is_some() {
            self.show_select_rect();
        } else if self.transpose.is_some() {
            self.transpose_move();
        } else if self.insert.is_some() {
            self.insert_move();
        } else if self.zm_drag.is_some() {
            self.zmodel_move();
        } else if self.mask_press.is_some() || self.mask_stroke.is_some() {
            self.mask_move();
        } else if self.curve_draw.is_some() {
            let p = self.cursor - self.canvas_origin();
            let d = self.curve_draw.as_mut().unwrap();
            if d.path.last().is_none_or(|q| q.distance(p) >= 1.0) {
                d.path.push(p);
            }
        } else if let Some(b) = self.curve.as_mut().and_then(|c| c.bend.as_mut()) {
            // Re-applied once per frame.
            b.pending = true;
        } else if self.stroke.is_some() {
            if self.stroke_brush.kind.is_drag() {
                self.stroke_pending = true;
            } else {
                self.apply_stroke();
            }
        } else if !over_ui && !self.in_canvas_border() {
            self.update_hover();
            if self.brush.kind.is_zmodeler() {
                self.update_zmodel_hover();
            }
        } else {
            self.hover = None;
            self.zm_hover = None;
        }
    }

    /// Left button pressed. `consumed`: egui took the event; `over_ui`: the cursor is off
    /// the canvas.
    fn left_press(&mut self, consumed: bool, over_ui: bool) {
        if self.workspace.modal_open() || self.workspace.brush_catalog_open() {
            return;
        }
        let border = self.in_canvas_border();
        let free = !consumed && !over_ui && self.nav.is_none() && !self.busy();
        if self.workspace.popup.is_some() || self.workspace.quick_menu.is_some() {
            // The press that closes a popup or menu does nothing else.
            return;
        }
        // ZBrush: Ctrl + Shift uses the Ctrl + Shift tool (the last Select, Clip, Trim, Slice
        // or Knife brush picked), Ctrl the mask brush, a plain drag the brush itself.
        let (ctrl, shift) = (self.modifiers.control_key(), self.modifiers.shift_key());
        let tool = if ctrl && shift {
            self.workspace.gesture_tool
        } else if ctrl {
            self.workspace.mask_brush
        } else {
            self.brush.kind
        };
        // Select Rect on Ctrl + Shift keeps its own press (a click on a polygroup isolates it).
        if free && tool.is_shape() && !(ctrl && shift && tool == BrushKind::SelectRect) {
            self.begin_gesture(tool);return;
        }
        if self.modifiers.control_key() && self.modifiers.shift_key() {
            // ZBrush: Ctrl + Shift selects (SelectRect) on the model or on the background;
            // the border outside the document frame counts as background.
            if free {
                let hit = if border {
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
        let picked = if self.modifiers.alt_key() && !self.modifiers.control_key() && !border && self.subtools.len() > 1 && free {
            self.pick_subtool().filter(|&i| i != self.active_tool)
        } else {
            None
        };
        if let Some(i) = picked {
            // ZBrush: Alt + click on another subtool selects it.
            self.select_subtool(i);
        } else if free {
            // ZBrush: a left drag on the model sculpts (Alt = alternate brush, Ctrl =
            // masking), or in Move / Scale / Rotate mode draws and drives the Transpose
            // action line; on empty canvas it rotates, with Alt it moves, and releasing Alt
            // turns it into a zoom. The border outside the document frame is always canvas.
            // Ctrl masks (see `begin_mask_press`).
            if self.modifiers.control_key() || self.brush.kind.is_mask() {
                if !self.modifiers.control_key() && (border || self.mesh().raycast(self.cursor_ray().0,self.cursor_ray().1).is_none()) {
                    self.begin_nav(MouseButton::Left);return;
                }
                self.begin_mask_press(border);
                return;
            }
            if border {
                self.begin_nav(MouseButton::Left);
                return;
            }
            if self.workspace.mode != Mode::Draw {
                // Gizmo 3D (Y) replaces the action line; a press off its handles navigates.
                let started = if self.workspace.gizmo { self.begin_gizmo() } else { self.begin_transpose() };
                if started {
                    return;
                }
            }
            // ZBrush Curve mode: a press on the applied curve bends it.
            if self.workspace.mode == Mode::Draw && self.begin_curve_bend() {
                return;
            }
            let (o, d) = self.cursor_ray();
            let hit = self.mesh().raycast(o, d);
            log::trace!("press cursor={:?} hit={:?}", self.cursor, hit.as_ref().map(|h| h.point));
            if hit.is_none() {
                self.begin_nav(MouseButton::Left);
            } else if self.effective_brush().kind.is_imm() {
                self.begin_insert();
            } else if self.effective_brush().kind.is_zmodeler() {
                self.begin_zmodel();
            } else if self.curve_mode(self.effective_brush().kind) {
                self.begin_curve();
            } else {
                self.begin_stroke();
            }
        }
    }

    /// Left button released.
    fn left_release(&mut self) {
        if self.end_gesture() {return;}
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
        self.end_mask();
        self.end_curve_draw();
        self.end_curve_bend();
        self.end_stroke();
        self.end_transpose();
        self.end_insert();
        self.end_zmodel();
        self.end_nav(MouseButton::Left);
    }

    /// Logical (egui) position of the cursor.
    fn cursor_pos(&self) -> egui::Pos2 {
        egui::pos2(self.cursor.x / self.scale, self.cursor.y / self.scale)
    }

    /// Replace the active subtool's mesh with the current primitive, in the same place and
    /// at the same size. Its levels and undo history go.
    fn rebuild(&mut self) {
        if self.busy() {
            return;
        }
        let t = Instant::now();
        let b = self.doc.bounds();
        let center = (b.min + b.max) * 0.5;
        let size = ((b.max - b.min).max_element() * 0.5).max(1e-3);
        let (p, i, g) = self.primitive.build(size);
        let p = p.into_iter().map(|q| q + center).collect();
        self.doc = Multires::new(SculptMesh::from_raw_with_groups(p, i, g), self.doc.budget);
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
        if key==KeyCode::Escape && self.gesture.is_some() {
            self.gesture=None;self.workspace.gesture_preview=None;return;
        }
        if self.gesture.is_some() {return;}
        if self.workspace.modal_open() || self.workspace.brush_catalog_open() {
            return;
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        match key {
            KeyCode::Escape if self.workspace.popup.is_some() => self.workspace.popup = None,
            KeyCode::Escape if self.workspace.focus => self.workspace.focus = false,
            // Esc drops the curve being drawn, or lets go of the applied one (its result
            // stays), unless it is being bent.
            KeyCode::Escape if self.curve_draw.is_some() || self.curve.as_ref().is_some_and(|c| c.bend.is_none()) => {
                self.curve_draw = None;
                self.curve = None;
                self.workspace.curve_preview = None;
            }
            KeyCode::KeyS if ctrl => self.save_or_ask(),
            KeyCode::KeyO if ctrl => self.ask_file(file_dialog::Ask::OpenProject),
            // ZBrush masking: Ctrl + I invert, Ctrl + A mask all, Ctrl + Shift + A clear,
            // Ctrl + H view mask.
            KeyCode::KeyI if ctrl => self.pending_mask = Some(MaskOp::Invert),
            KeyCode::KeyA if ctrl && shift => self.pending_mask = Some(MaskOp::Clear),
            KeyCode::KeyA if ctrl => self.pending_mask = Some(MaskOp::All),
            KeyCode::KeyH if ctrl => self.workspace.show_mask = !self.workspace.show_mask,
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
            KeyCode::F11 => self.toggle_fullscreen(),
            KeyCode::KeyP if shift => self.workspace.grid = !self.workspace.grid,
            KeyCode::KeyP => self.workspace.perspective = !self.workspace.perspective,
            KeyCode::KeyB => self.workspace.open_brush_catalog(self.cursor_pos()),
            KeyCode::Digit1 => self.workspace.select_brush(&mut self.brush,BrushKind::Draw),
            KeyCode::Digit2 => self.workspace.select_brush(&mut self.brush,BrushKind::Smooth),
            KeyCode::Digit3 => self.workspace.select_brush(&mut self.brush,BrushKind::Grab),
            KeyCode::Digit4 => self.workspace.select_brush(&mut self.brush,BrushKind::Flatten),
            KeyCode::Digit5 => self.workspace.select_brush(&mut self.brush,BrushKind::Inflate),
            KeyCode::Digit6 => self.workspace.select_brush(&mut self.brush,BrushKind::DamStandard),
            KeyCode::Digit7 => self.workspace.select_brush(&mut self.brush,BrushKind::Pinch),
            KeyCode::Digit8 => self.workspace.select_brush(&mut self.brush,BrushKind::Clay),
            KeyCode::Digit9 => self.workspace.select_brush(&mut self.brush,BrushKind::ClayBuildup),
            KeyCode::KeyQ => self.workspace.mode = Mode::Draw,
            KeyCode::KeyW => self.workspace.mode = Mode::Move,
            // ZBrush: Y switches Move / Scale / Rotate between the action line and Gizmo 3D.
            KeyCode::KeyY => {
                self.workspace.gizmo = !self.workspace.gizmo;
                if self.workspace.mode == Mode::Draw {
                    self.workspace.mode = Mode::Move;
                }
            }
            KeyCode::KeyE => self.workspace.mode = Mode::Scale,
            KeyCode::KeyR => self.workspace.mode = Mode::Rotate,
            KeyCode::KeyX => self.workspace.symmetry = !self.workspace.symmetry,
            // ZBrush: L toggles LazyMouse.
            KeyCode::KeyL => {
                self.workspace.lazy_mouse = !self.workspace.lazy_mouse;
                self.workspace.notify(if self.workspace.lazy_mouse { "LazyMouse on" } else { "LazyMouse off" });
            }
            KeyCode::KeyS => self.workspace.toggle_popup(QuickPopup::Size, self.cursor_pos()),
            KeyCode::KeyU => self.workspace.toggle_popup(QuickPopup::Intensity, self.cursor_pos()),
            KeyCode::KeyI => self.workspace.toggle_popup(QuickPopup::Rgb, self.cursor_pos()),
            KeyCode::KeyO => self.workspace.toggle_popup(QuickPopup::Focal, self.cursor_pos()),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        // ZBrush: over the model the brush is the cursor. The system pointer hides while the
        // brush ring is drawn there or a stroke is under way; over the interface, the empty
        // canvas and its border it shows, so navigating stays visible.
        // Shape gestures (mask shapes, Clip Line) keep the pointer: it marks their corners.
        let sculpting = self.stroke.is_some() || self.mask_stroke.is_some() || self.curve_draw.is_some();
        let ring = self.hover.is_some() && self.workspace.mode == Mode::Draw && !self.in_canvas_border() && self.gesture.is_none() && !self.brush.kind.is_shape() && !self.brush.kind.is_zmodeler();
        if (sculpting || ring) && !ui.ctx().is_pointer_over_egui() && !self.workspace.modal_open() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }
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
                    boolean: t.boolean,
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
                    boolean: t.boolean,
                },
            })
            .collect();
        let parked_bytes = rows.iter().filter(|r| !r.active).map(|r| r.bytes).sum();
        let (groups, hidden) = (self.doc.mesh().group_count(), self.doc.mesh().hidden_count());
        let wedge_refusal = self.wedge_refusal();
        let area = self.wedge_info().1;
        let wedge_estimate = self.wedge.estimate(area);
        let wedge_on = self.subtools[self.active_tool].wedge;
        let tablet = match (&self.tablet, &self.tablet_error) {
            (Some(t), _) => t.describe(),
            (None, Some(e)) => e.clone(),
            (None, None) => String::new(),
        };
        self.workspace.curve_preview = self.curve_preview();
        self.workspace.held_brush = self.held_brush();
        let print_extent = self.print_extent();
        let print_stale = self.print_report.as_ref().is_some_and(|r| r.stamp != self.print_stamp());
        let actions = self.workspace.show(
            ui,
            &mut workspace::Model {
                brush: &mut self.brush,
                primitive: &mut self.primitive,
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
                drawn: self.gfx.as_ref().and_then(|g| g.drawn),
                points_supported: self.gfx.as_ref().is_some_and(Gfx::points_supported),
                axes: {
                    let (r, u, f) = (self.camera.right(), self.camera.up(), self.camera.forward());
                    [Vec3::X, Vec3::Y, Vec3::Z].map(|v| [v.dot(r), v.dot(u), -v.dot(f)])
                },
                subtools: &rows,
                parked_bytes,
                stand_in_bytes,
                project_path: &mut self.project_path,
                morph_level: self.morph_targets.get(&self.subtools[self.active_tool].id).map(|(l, _)| *l),
                layers: self.doc.layers.list.iter().enumerate().map(|(i, l)| (l.name.clone(), l.visible, self.doc.layers.rec.as_ref().is_some_and(|(r, _)| *r == i), l.intensity)).collect(),
                undo_names: self.history.names().0,
                redo_names: self.history.names().1.into_iter().cloned().collect(),
                groups,
                hidden,
                polyframe_supported: self.gfx.as_ref().is_none_or(Gfx::group_overlay_supported),
                wedge: &mut self.wedge,
                wedge_on,
                wedge_refusal: wedge_refusal.as_deref(),
                wedge_estimate,
                print_extent,
                print_report: self.print_report.as_ref(),
                print_stale,
                tablet: &tablet,
                tablet_on: self.tablet.is_some(),
                pressure: self.pressure,
            },
        );
        if actions.new_project {
            self.pending_doc = Some(DocOp::New);
        }
        if actions.open {
            self.ask_file(file_dialog::Ask::OpenProject);
        }
        if actions.save {
            self.save_or_ask();
        }
        if actions.save_as {
            self.ask_file(file_dialog::Ask::SaveProject);
        }
        if actions.revert {
            self.pending_doc = Some(DocOp::Open);
        }
        if actions.snapshot {
            self.request_snapshot();
        }
        if actions.save_subtool {
            self.ask_file(file_dialog::Ask::SaveSubtool);
        }
        if actions.load_subtools {
            self.ask_file(file_dialog::Ask::LoadSubtools);
        }
        if actions.export_subtool {
            self.ask_file(file_dialog::Ask::ExportSubtoolObj);
        }
        if actions.import_mesh {
            self.ask_file(file_dialog::Ask::ImportMesh);
        }
        if actions.export_stl {
            self.ask_file(file_dialog::Ask::ExportStl);
        }
        if actions.send_slicer {
            self.send_to_slicer();
        }
        if actions.print_check {
            self.print_check();
        }
        if actions.export {
            self.ask_file(file_dialog::Ask::ExportObj);
        }
        if let Some(by) = actions.history_jump {
            self.history_jump(by);
        }
        if actions.quicksave {
            self.quicksave();
        }
        if actions.fullscreen {
            self.toggle_fullscreen();
        }
        if let Some(path) = actions.store_startup_doc {
            self.save_startup_doc(&path);
        }
        if let Some(path) = actions.delete_startup_doc {
            let _ = std::fs::remove_file(path);
        }
        if let Some(op) = actions.layer {
            self.layer_op(op);
        }
        match actions.morph {
            Some(MorphOp::Store) => self.store_morph_target(),
            Some(MorphOp::Delete) => {
                let id = self.subtools[self.active_tool].id;
                if self.morph_targets.remove(&id).is_some() {
                    self.workspace.notify("Morph Target deleted");
                }
            }
            Some(MorphOp::Switch) => self.switch_morph_target(),
            None => {}
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
        for (on, op) in [
            (actions.merge_down, SubToolOp::MergeDown),
            (actions.split_hidden, SubToolOp::SplitHidden),
            (actions.groups_split, SubToolOp::GroupsSplit),
            (actions.extract, SubToolOp::Extract),
        ] {
            if on {
                self.pending_subtool = Some(op);
            }
        }
        if actions.delete {
            self.pending_subtool = Some(SubToolOp::Delete);
        }
        if actions.group_visible {
            self.pending_vis = Some(VisOp::GroupVisible);
        }
        if let Some(op) = actions.boolean_op {
            self.subtools[self.active_tool].boolean = op;
        }
        if actions.make_boolean {
            self.make_boolean_mesh();
        }
        if actions.gizmo_center || actions.gizmo_reset {
            self.gizmo_pivot(actions.gizmo_center);
        }
        for (on, op) in [(actions.del_hidden, GeoOp::DelHidden), (actions.close_holes, GeoOp::CloseHoles), (actions.mirror_weld, GeoOp::MirrorWeld), (actions.zremesh, GeoOp::ZRemesh)] {
            if on {
                self.pending_geo = Some(op);
            }
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
        if let Some(grow) = actions.grow_visibility {
            self.pending_vis = Some(if grow { VisOp::Grow } else { VisOp::Shrink });
        }
        if actions.auto_groups {
            self.pending_vis = Some(VisOp::AutoGroups);
        }
        if let Some(angle) = actions.group_by_normals {
            self.pending_vis = Some(VisOp::ByNormals(angle));
        }
        if actions.move_subtool != 0 {
            self.move_subtool(actions.move_subtool);
        }
        if let Some((kind, value)) = actions.deform {
            self.pending_subtool = Some(SubToolOp::Deform(kind, value));
        }
        self.pending_rebuild |= actions.rebuild;
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
        self.paint_zmodel(ui.ctx());
        if self.workspace.gizmo {
            self.paint_gizmo(ui.ctx());
        } else {
            self.paint_action_line(ui.ctx());
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
        // Preferences › UI scale; egui's own Ctrl + plus / minus zoom is kept in step.
        let zoom = self.egui_ctx.zoom_factor();
        if (zoom - self.workspace.applied_scale).abs() > 1e-3 {
            self.workspace.layout.ui_scale = zoom.clamp(workspace::UI_SCALES[0], workspace::UI_SCALES[workspace::UI_SCALES.len() - 1]);
        }
        if (zoom - self.workspace.layout.ui_scale).abs() > 1e-3 {
            self.egui_ctx.set_zoom_factor(self.workspace.layout.ui_scale);
        }
        self.workspace.applied_scale = self.workspace.layout.ui_scale;
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
            self.history_jump(-1);
        }
        if self.pending_redo {
            self.pending_redo = false;
            self.history_jump(1);
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
        self.run_pending_vis();
        self.run_pending_geo();
        self.answer_file_dialog();
        match self.pending_doc.take() {
            Some(DocOp::New) => self.new_project(),
            Some(DocOp::Open) => {
                let path = self.project_path.clone();
                self.open_project(&path);
            }
            Some(DocOp::Save) => self.save_project(),
            Some(DocOp::SaveSubtool) => self.save_subtool(),
            Some(DocOp::LoadSubtools) => self.load_subtools(),
            Some(DocOp::ExportSubtool) => self.export_subtool_obj(),
            Some(DocOp::ImportMesh) => {
                let path = self.import_path.clone();
                self.import_mesh(&path);
            }
            Some(DocOp::ExportStl) => self.export_stl(),
            None => {}
        }
        match self.pending_subtool.take() {
            Some(SubToolOp::Select(i)) => self.select_subtool(i),
            Some(SubToolOp::Append) => self.append_primitive(),
            Some(SubToolOp::Duplicate) => self.duplicate_subtool(),
            Some(SubToolOp::Delete) => self.delete_subtool(),
            Some(SubToolOp::Deform(kind, value)) => self.deform(kind, value),
            Some(SubToolOp::MergeDown) => self.merge_down(),
            Some(SubToolOp::SplitHidden) => self.split_hidden(),
            Some(SubToolOp::GroupsSplit) => self.groups_split(),
            Some(SubToolOp::Extract) => self.extract(),
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
                    strength: 0.5, ..Brush::default() };
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
        if self.curve.as_ref().is_some_and(|c| c.bend.as_ref().is_some_and(|b| b.pending)) {
            let t = Instant::now();
            self.bend_curve();
            self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
        }
        self.follow_tube_settings();
        if let Some(left) = self.selftest_clip {
            if left == 0 {
                self.selftest_clip = None;
                self.selftest_clip_cuts();
                if !self.selftest_hold {
                    self.quit = true;
                }
            } else {
                self.selftest_clip = Some(left - 1);
            }
        }
        if let Some((phase, left)) = self.selftest_tube {
            if left == 0 {
                self.selftest_tube = self.selftest_tube_phase(phase).map(|next| (next, 10));
                if self.selftest_tube.is_none() && !self.selftest_hold {
                    self.quit = true;
                }
            } else {
                self.selftest_tube = Some((phase, left - 1));
            }
        }
        if let Some((phase, left)) = self.selftest_zmodeler {
            if left == 0 {
                self.selftest_zmodeler = self.selftest_zmodeler_phase(phase).map(|next| (next, 6));
                if self.selftest_zmodeler.is_none() && !self.selftest_hold {
                    self.quit = true;
                }
            } else {
                self.selftest_zmodeler = Some((phase, left - 1));
            }
        }
        if let Some((phase, left)) = self.selftest_sculptris {
            if left == 0 {
                self.selftest_sculptris = self.selftest_sculptris_phase(phase).map(|next| (next, 10));
                if self.selftest_sculptris.is_none() && !self.selftest_hold {
                    self.quit = true;
                }
            } else {
                self.selftest_sculptris = Some((phase, left - 1));
            }
        }
        if let Some((phase, left)) = self.selftest_insert {
            if left == 0 {
                self.selftest_insert = self.selftest_insert_phase(phase).map(|next| (next, 10));
                if self.selftest_insert.is_none() && !self.selftest_hold {
                    self.quit = true;
                }
            } else {
                self.selftest_insert = Some((phase, left - 1));
            }
        }
        if let Some((phase, left)) = self.selftest_curve {
            if left == 0 {
                self.selftest_curve = self.selftest_curve_phase(phase).map(|next| (next, 10));
                if self.selftest_curve.is_none() {
                    self.quit = true;
                }
            } else {
                self.selftest_curve = Some((phase, left - 1));
            }
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
        if let Some(left) = self.selftest_pressure {
            if left == 0 {
                self.selftest_pressure = None;
                self.selftest_pressure();
                self.quit = true;
            } else {
                self.selftest_pressure = Some(left - 1);
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
        let snapshot_now = match self.snapshot_to.as_mut() {
            Some((_, frames)) => {
                *frames = frames.saturating_sub(1);
                *frames == 0
            }
            None => false,
        };
        if snapshot_now {
            self.request_snapshot();
        }
        if let Some(mut t) = self.selftest_symmetry.take() {
            if self.selftest_symmetry_frame(&mut t) { self.quit = true; }
            else { self.selftest_symmetry = Some(t); }
        }
        if let Some(mut t) = self.selftest_document.take() {
            if self.selftest_document_frame(&mut t) { self.quit = true; }
            else { self.selftest_document = Some(t); }
        }
        if let Some(mut t) = self.selftest_cull.take() {
            if self.selftest_cull_frame(&mut t) { self.quit = true; }
            else { self.selftest_cull = Some(t); }
        }
        if let Some(mut t) = self.selftest_splat.take() {
            if self.selftest_splat_frame(&mut t) { self.quit = true; }
            else { self.selftest_splat = Some(t); }
        }
        if let Some(mut t) = self.selftest_boolean.take() {
            if self.selftest_boolean_frame(&mut t) { self.quit = !self.selftest_hold; }
            else { self.selftest_boolean = Some(t); }
        }
        if let Some(mut t) = self.selftest_materials.take() {
            if self.selftest_materials_frame(&mut t) { self.quit = true; }
            else { self.selftest_materials = Some(t); }
        }
        if let Some(mut t) = self.selftest_surface.take() {
            if self.selftest_surface_frame(&mut t) {self.quit=true;}
            else{self.selftest_surface=Some(t);}
        }
        if let Some(mut t) = self.selftest_brushes.take() {
            if self.selftest_brushes_frame(&mut t) { self.quit = true; }
            else { self.selftest_brushes = Some(t); }
        }
        if let Some(mut t) = self.selftest_paint.take() {
            if self.selftest_paint_frame(&mut t) { self.quit=true; }
            else { self.selftest_paint=Some(t); }
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
                let culled = match self.gfx.as_ref().and_then(Gfx::read_cull_counts) {
                    Some(c) => format!(
                        ", culling drew {} of {} chunks as triangles ({} of {} triangles) and {} as points ({} vertices)",
                        c.chunks,
                        c.chunk_total,
                        fmt_count(c.triangles as usize),
                        fmt_count(c.triangle_total as usize),
                        c.point_chunks,
                        fmt_count(c.points as usize)
                    ),
                    None => ", no culling".to_string(),
                };
                log::info!(
                    "frames: avg {:.2} ms over {TOTAL} frames at level {} ({} vertices), last upload {:.2} ms, submit {:.2} ms{culled}",
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
        self.camera.fov_y = self.workspace.fov.clamp(10.0, 120.0).to_radians();
        let brush_pos = self.hover.unwrap_or(Vec3::ZERO);
        let show_brush = self.hover.is_some()
            && !ctx.is_pointer_over_egui()
            && self.workspace.mode == Mode::Draw
            && !self.brush.kind.is_zmodeler();
        let radius = if self.stroke.is_some() {
            self.stroke_brush.radius
        } else {
            self.effective_brush().radius
        };
        let px = self.camera.pixel_size_at(brush_pos, vp.y);
        let eye = self.camera.eye();
        // Light palette: placed on the front of a sphere facing the camera.
        let place = self.workspace.light;
        let toward = (1.0 - place.length_squared()).max(0.0).sqrt();
        let light = (self.camera.right() * place.x + self.camera.up() * place.y - self.camera.forward() * toward).normalize_or(-self.camera.forward());
        let preview = self.preview_map();
        let (preview_axis, preview_angle) = preview.rotation.to_axis_angle();
        let uniforms = Uniforms {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            inv_view_proj: self.camera.view_proj().inverse().to_cols_array_2d(),
            cam_pos: [eye.x, eye.y, eye.z, 1.0],
            light_dir: [light.x, light.y, light.z, self.workspace.light_intensity],
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
                if self.workspace.sym().is_on() { 1.0 } else { 0.0 },
                if self.workspace.show_mask { 1.0 } else { 0.0 },
                if self.doc.mesh().mask.is_empty() { 0.0 } else { 1.0 },
            ],
            xf_center: preview.center.extend(if preview.is_identity() { 0.0 } else { 1.0 }).to_array(),
            xf_rotation: preview_axis.extend(preview_angle).to_array(),
            xf_move: preview.translate.extend(preview.scale).to_array(),
            xf_stretch: preview.axis.extend(preview.stretch.ln()).to_array(),
            material: self.material_uniform(),
            cam_up: self.camera.up().extend(self.workspace.ambient).to_array(),
            back: {
                let d = &self.workspace.layout.document;
                let c = d.back.map(|c| c as f32 / 255.0);
                [c[0], c[1], c[2], d.range]
            },
            back_shape: {
                let d = &self.workspace.layout.document;
                [d.center, d.rate, self.workspace.layout.document.inset * self.scale, 0.0]
            },
            border: {
                let c = self.workspace.layout.document.border.map(|c| c as f32 / 255.0);
                [c[0], c[1], c[2], 1.0]
            },
            sym: {
                let s = self.workspace.sym();
                let bits = s.x as u32 | (s.y as u32) << 1 | (s.z as u32) << 2;
                let (axis, count) = s.radial.map_or((-1.0, 1.0), |(a, n)| (symmetry::Axis::ALL.iter().position(|x| *x == a).unwrap() as f32, n as f32));
                [bits as f32, axis, count, 0.0]
            },
        };
        if Gfx::take_gpu_error() {
            self.workspace.notify("The GPU reported an error (likely out of memory) · the view may be incomplete; save, and lower the polygon count".to_string());
        }
        let slots = self.update_live_boolean();
        let parked: Vec<(u64, u32)> = self
            .drawn_parked()
            .into_iter()
            .map(|(key, i)| (key, slots.iter().find(|s| s.0 == i).map_or(1, |s| 1 + s.1 as u32)))
            .collect();
        let Some(gfx) = self.gfx.as_mut() else { return };
        // Performance › Drawing (self-tests switch points themselves).
        if self.selftest_cull.is_none() && self.selftest_splat.is_none() {
            gfx.splatting = self.workspace.layout.dense_points && !self.render_triangles;
        }
        let t = Instant::now();
        gfx.upload_dirty(self.doc.mesh_mut(), self.workspace.polyframe);
        self.upload_ms = t.elapsed().as_secs_f32() * 1000.0;
        let t = Instant::now();
        gfx.render(&uniforms, &parked, self.workspace.polyframe, &jobs, full.textures_delta, full.pixels_per_point);
        self.render_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.save_snapshot();
        self.quicksave_when_due();
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
        let attributes=Window::default_attributes()
                        .with_title("Klay")
                        .with_window_icon(Some(branding::window_icon()))
                        .with_min_inner_size(LogicalSize::new(960.0, 640.0))
                        .with_inner_size(PhysicalSize::new(1600, 1000));
        #[cfg(target_os="linux")]
        let attributes=winit::platform::x11::WindowAttributesExtX11::with_name(attributes,"klay","Klay");
        #[cfg(target_os="linux")]
        let attributes=winit::platform::wayland::WindowAttributesExtWayland::with_name(attributes,"klay","Klay");
        let window = Arc::new(
            event_loop
                .create_window(attributes)
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
        let mut gfx = gfx;
        gfx.snapshot_ui = self.snapshot_ui;
        gfx.culling = !self.render_triangles;
        self.gfx = Some(gfx);
        match tablet::Tablet::for_window(&window) {
            Ok(t) => {
                log::info!("tablet: listening for pen pressure; {}", t.describe());
                self.tablet = Some(t);
            }
            Err(e) => {
                log::info!("tablet: {e}");
                self.tablet_error = Some(e);
            }
        }
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
        if self.demo_brushes {
            for (name, ms, dabs, count) in self.advanced_strokes() {
                log::info!("demo stroke {name}: {dabs} dabs over {count} verts in {ms:.1} ms");
            }
        }
        if let Some(path) = self.open_on_start.take() {
            self.open_project(&path);
        } else if !self.automated && self.workspace.layout.start_from_stored {
            // Preferences › Config: the stored startup document, opened as a new, unsaved
            // project (Save asks for a path of its own).
            let doc = self.workspace.startup_doc_path();
            if doc.exists() {
                let keep = self.project_path.clone();
                self.open_project(&doc.to_string_lossy());
                self.project_path = keep;
                self.project_has_file = false;
            }
        }
        for _ in 0..std::mem::take(&mut self.initial_subtools) {
            self.append_primitive();
            log::info!("{}", self.status);
        }
        if self.subtools.len() > 1 {
            self.frame_camera();
        }
        if let Some(k) = self.camera_distance {
            self.camera.distance *= k;
        }
        if std::mem::take(&mut self.print_check_on_start) {
            self.print_check();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::KeyboardInput { .. } | WindowEvent::MouseWheel { .. }
        ) || (matches!(event, WindowEvent::CursorMoved { .. }) && self.stroke.is_some())
        {
            self.last_input = Instant::now();
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } | WindowEvent::RedrawRequested
        ) {
            self.poll_tablet();
        }
        let consumed = match self.egui_state.as_mut() {
            Some(state) => state.on_window_event(&window, &event).consumed,
            None => false,
        };
        let over_ui = self.workspace.modal_open() || self.workspace.brush_catalog_open()
            || self.egui_ctx.is_pointer_over_egui()
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
                if self.gesture.is_some() {self.update_gesture();}
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
                let typing = self.egui_ctx.egui_wants_keyboard_input() || self.workspace.modal_open() || self.workspace.brush_catalog_open();
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
        strength: 0.8, ..Brush::default() };
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
                println!("remesh: {e}");
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
    if args.iter().any(|a|a=="--version"||a=="-V") {
        println!("Klay {}",branding::VERSION);return;
    }
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
    if args.iter().any(|a| a == "--selftest-remesh" || a == "--selftest-wedge") {
        app.selftest_wedge = Some((0, 20));
    }
    if let Some(r) = flag("--remesh-resolution").or_else(|| flag("--wedge-resolution")).and_then(|s| s.parse().ok()) {
        app.wedge.resolution = r;
    }
    if args.iter().any(|a| a == "--selftest-groups") {
        app.selftest_groups = Some((0, 20));
    }
    if args.iter().any(|a| a == "--selftest-mask") {
        app.selftest_mask = Some(MaskTest::default());
    }
    if args.iter().any(|a| a == "--selftest-paint") {
        app.selftest_paint = Some(PaintTest::default());
    }
    if args.iter().any(|a| a == "--selftest-brushes") {
        app.selftest_brushes = Some(BrushTest::default());
    }
    if args.iter().any(|a| a == "--selftest-surface") {
        app.selftest_surface = Some(native_tests::SurfaceTest::default());
    }
    if args.iter().any(|a| a == "--selftest-symmetry") {
        app.selftest_symmetry = Some(native_tests::SymmetryTest::default());
    }
    if args.iter().any(|a| a == "--selftest-document") {
        app.selftest_document = Some(native_tests::DocumentTest::default());
        // Snapshots go next to the project: keep them out of the working directory.
        app.project_path = std::env::temp_dir().join(format!("klay-document-{}.klay", std::process::id())).to_string_lossy().into_owned();
    }
    if args.iter().any(|a| a == "--selftest-cull") {
        app.selftest_cull = Some(native_tests::CullTest::default());
    }
    if args.iter().any(|a| a == "--selftest-splat") {
        app.selftest_splat = Some(native_tests::SplatTest::default());
    }
    if args.iter().any(|a| a == "--selftest-boolean") {
        app.selftest_boolean = Some(native_tests::BooleanTest::default());
    }
    if args.iter().any(|a| a == "--selftest-materials") {
        app.selftest_materials = Some(native_tests::MaterialTest::default());
    }
    if args.iter().any(|a| a == "--selftest-transpose") {
        app.selftest_transpose = Some(TransposeTest::default());
    }
    if args.iter().any(|a| a == "--selftest-project") {
        app.selftest_project = Some(20);
    }
    if args.iter().any(|a| a == "--selftest-pressure") {
        app.selftest_pressure = Some(20);
    }
    if args.iter().any(|a| a == "--selftest-curve") {
        app.selftest_curve = Some((0, 20));
    }
    if args.iter().any(|a| a == "--selftest-tube") {
        app.selftest_tube = Some((0, 20));
    }
    if args.iter().any(|a| a == "--selftest-zmodeler") {
        app.selftest_zmodeler = Some((0, 20));
    }
    if args.iter().any(|a| a == "--selftest-sculptris") {
        app.selftest_sculptris = Some((0, 20));
    }
    if args.iter().any(|a| a == "--selftest-insert") {
        app.selftest_insert = Some((0, 20));
    }
    app.selftest_hold = args.iter().any(|a| a == "--hold");
    app.demo_brushes = args.iter().any(|a| a == "--demo-brushes");
    app.automated = args.iter().any(|a| {
        a.starts_with("--selftest") || matches!(a.as_str(), "--demo" | "--demo-brushes" | "--stress" | "--frames" | "--snapshot")
    });
    // Look for a newer release only in a normal run (not self-tests, demos, snapshots).
    app.workspace.updates_allowed = !app.automated && !args.iter().any(|a| a == "--no-update-check");
    // `--symmetry x,y,z,rz8`: start with these mirrors and radial symmetry (axis, count).
    if let Some(spec) = flag("--symmetry") {
        for part in spec.split(',').map(str::trim) {
            match part {
                "x" => app.workspace.symmetry = true,
                "y" => app.workspace.symmetry_y = true,
                "z" => app.workspace.symmetry_z = true,
                p if p.len() > 2 && p.starts_with('r') => {
                    let axis = symmetry::Axis::ALL.into_iter().find(|a| a.name().eq_ignore_ascii_case(&p[1..2]));
                    match (axis, p[2..].parse::<f32>()) {
                        (Some(axis), Ok(n)) => {
                            app.workspace.radial = true;
                            app.workspace.radial_axis = axis;
                            app.workspace.radial_count = n.clamp(2.0, 64.0);
                        }
                        _ => eprintln!("--symmetry: unknown part {p} (x, y, z, or r<axis><count> like rz8)"),
                    }
                }
                p => eprintln!("--symmetry: unknown part {p} (x, y, z, or r<axis><count> like rz8)"),
            }
        }
    }
    // `--snapshot FILE`: after start-up (and `--demo-brushes`), save the canvas and quit.
    if let Some(path) = flag("--snapshot") {
        // `--snapshot-frames N`: after N frames instead of 30.
        let frames = flag("--snapshot-frames").and_then(|n| n.parse().ok()).unwrap_or(30);
        app.snapshot_to = Some((path.clone(), frames));
        // `--snapshot-ui`: the whole window with the interface (for UI reviews).
        app.snapshot_ui = args.iter().any(|a| a == "--snapshot-ui");
    }
    app.render_triangles = flag("--render").is_some_and(|r| r == "triangles");
    app.camera_distance = flag("--camera-distance").and_then(|s| s.parse().ok());
    // `--customize`: start in Customize shelves mode (for snapshots of it).
    app.workspace.customize = args.iter().any(|a| a == "--customize");
    // `--fake-update X`: show the update prompt for a release X (snapshots, no network).
    if let Some(v) = flag("--fake-update") {
        app.workspace.pretend_update(update::Release {
            version: v.clone(),
            page: format!("{}/releases/tag/v{v}", branding::REPOSITORY),
            asset: None,
            notes: "Every shelf item can be dragged anywhere · default config · update checks".into(),
        });
    }
    // `--about`: start with About Klay open (for snapshots of it).
    app.workspace.about_open = args.iter().any(|a| a == "--about");
    // `--print-check`: run 3D Print › Check Mesh once the model is built (snapshots of
    // its result).
    app.print_check_on_start = args.iter().any(|a| a == "--print-check");
    // `--menu NAME`: start with that palette's menu-bar dropdown open (snapshots).
    if let Some(name) = flag("--menu") {
        app.workspace.open_menu(&name);
    }
    // `--ui-scale X`: start at this interface scale.
    if let Some(x) = flag("--ui-scale").and_then(|v| v.parse::<f32>().ok()) {
        app.workspace.layout.ui_scale = x;
    }
    // `--theme NAME`: start in this interface theme (Preferences › Theme).
    if let Some(name) = flag("--theme") {
        app.workspace.layout.theme = name.clone();
    }
    // `--space-menu`: start with the Space menu open (for snapshots of it).
    if args.iter().any(|a| a == "--space-menu") {
        app.workspace.open_quick_menu(egui::pos2(900.0, 360.0), false);
    }
    // `--brush-catalog`: start with the B catalog open (for snapshots of it).
    if args.iter().any(|a| a == "--brush-catalog") {
        app.workspace.open_brush_catalog(egui::pos2(320.0, 140.0));
    }
    // `--gizmo`: start in Move mode with Gizmo 3D (Y).
    if args.iter().any(|a| a == "--gizmo") {
        app.workspace.gizmo = true;
        app.workspace.mode = Mode::Move;
    }
    // `--polyf`: start with polygroup colours and the polygon frame shown (Shift + F).
    app.workspace.polyframe = args.iter().any(|a| a == "--polyf");
    // `--zremesh K`: ZRemesher to K thousand quads at startup.
    if let Some(k) = flag("--zremesh").and_then(|v| v.parse::<f32>().ok()) {
        app.workspace.zremesh.target_k = k;
        app.pending_geo = Some(GeoOp::ZRemesh);
    }
    // `--material <name or number>`: start in that material (e.g. "MatCap Red Wax").
    if let Some(m) = flag("--material") {
        let names = workspace::MATERIALS.iter().map(|(_, n)| n.to_lowercase()).collect::<Vec<_>>();
        match m.parse::<usize>().ok().or_else(|| names.iter().position(|n| *n == m.to_lowercase())) {
            Some(i) if i < names.len() => app.workspace.material = i,
            _ => eprintln!("unknown material {m}; one of: {}", workspace::MATERIALS.map(|(_, n)| n).join(", ")),
        }
    }
    if args.iter().any(|a| a == "--selftest-clip") {
        app.selftest_clip = Some(20);
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
    fn about_modal_blocks_editing_and_canvas_shortcuts() {
        let mut app = App::new(4, false, 1000);
        app.workspace.about_open = true;
        let radius = app.brush.radius;
        let brush = app.brush.kind;
        let perspective = app.workspace.perspective;
        let symmetry = app.workspace.symmetry;
        app.modifiers = ModifiersState::CONTROL;
        app.on_key(KeyCode::KeyD);
        app.on_key(KeyCode::KeyZ);
        assert!(!app.pending_divide);
        assert!(!app.pending_undo);
        app.modifiers = ModifiersState::empty();
        for key in [
            KeyCode::Digit3,
            KeyCode::BracketRight,
            KeyCode::KeyP,
            KeyCode::KeyX,
        ] {
            app.on_key(key);
        }
        app.left_press(false, false);
        assert!(app.stroke.is_none());
        assert!(app.nav.is_none());
        assert_eq!(app.brush.radius, radius);
        assert_eq!(app.brush.kind, brush);
        assert_eq!(app.workspace.perspective, perspective);
        assert_eq!(app.workspace.symmetry, symmetry);
        let (yaw, pitch) = (app.camera.yaw, app.camera.pitch);
        app.nav = Some(NavDrag {
            button: MouseButton::Right,
            alt_used: false,
            shift_at_press: false,
        });
        app.cursor_moved(app.cursor + Vec2::new(40.0, 20.0), false);
        assert_eq!((app.camera.yaw, app.camera.pitch), (yaw, pitch));
    }

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

    /// Low poly, highest Detail, many strokes and undos (a crash report): every undo must
    /// fit, and undoing everything gives the start back.
    #[test]
    fn sculptris_pro_survives_strokes_and_undo_on_a_low_poly_model() {
        let mut app = App::new(4, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app.workspace.sculptris_detail = 100.0;
        // No mask and a mask of zeros are the same.
        let state = |app: &App| {
            let m = app.doc.mesh();
            let mask = if m.mask.is_empty() { vec![0; m.vertex_count()] } else { m.mask.clone() };
            (m.positions.clone(), m.indices.clone(), mask)
        };
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10000) as f32 / 10000.0
        };
        let c = app.project(Vec3::ZERO);
        let r = (app.project(Vec3::X) - c).length();
        let mut states = vec![state(&app)];
        for round in 0..60 {
            // Sculptris strokes, plain strokes (Sculptris off) and mask strokes, mixed: the
            // plain and mask steps name vertices by chunk, so Sculptris undo must put the
            // layout back exactly (the crash: it did not).
            let pick = rnd();
            app.workspace.sculptris = pick < 0.6;
            let mask = (0.6..0.75).contains(&pick);
            let kind = [BrushKind::Draw, BrushKind::ClayBuildup, BrushKind::Smooth, BrushKind::Inflate][round % 4];
            app.workspace.select_brush(&mut app.brush, kind);
            app.brush.radius = 0.04 + rnd() * 0.3;
            app.workspace.symmetry = rnd() < 0.3;
            let from = c + Vec2::new(rnd() - 0.5, rnd() - 0.5) * r * 1.2;
            let to = c + Vec2::new(rnd() - 0.5, rnd() - 0.5) * r * 1.2;
            let m = if mask { ModifiersState::CONTROL } else { ModifiersState::empty() };
            app.modifiers = m;
            let edits = app.history.edits();
            app.cursor_moved(from, false);
            app.left_press(false, false);
            for k in 1..=8 {
                app.cursor_moved(from + (to - from) * (k as f32 / 8.0), false);
            }
            app.left_release();
            app.modifiers = ModifiersState::empty();
            // A stroke that missed records nothing.
            if app.history.edits() != edits {
                states.push(state(&app));
            }
            // Undo goes back 32 steps at most: keep the states it can reach.
            if states.len() > 33 {
                states.remove(0);
            }
            if rnd() < 0.35 {
                let back = (1 + (rnd() * 4.0) as usize).min(states.len() - 1);
                let names: Vec<String> = app.history.names().0.iter().rev().take(back).cloned().collect();
                app.history_jump(-(back as i64));
                let (now, want) = (state(&app), &states[states.len() - 1 - back]);
                if now != *want {
                    let diff_pos = now.0.iter().zip(&want.0).filter(|(a, b)| a != b).count();
                    let diff_idx = now.1.iter().zip(&want.1).filter(|(a, b)| a != b).count();
                    let diff_mask = now.2.iter().zip(&want.2).filter(|(a, b)| a != b).count();
                    panic!(
                        "round {round}: undo {back} ({names:?}) gave another mesh: vertices {} vs {}, tris {} vs {}, positions differ {diff_pos}, indices {diff_idx}, mask {diff_mask}",
                        now.0.len(),
                        want.0.len(),
                        now.1.len() / 3,
                        want.1.len() / 3
                    );
                }
                let fwd = (rnd() * (back + 1) as f32) as usize;
                app.history_jump(fwd as i64);
                let keep = states.len() - back + fwd;
                states.truncate(keep);
                assert!(state(&app) == states[keep - 1], "round {round}: redo {fwd} gave another mesh");
            }
        }
        let steps = (states.len() - 1).min(app.history.names().0.len());
        let states = states[states.len() - 1 - steps..].to_vec();
        app.history_jump(-(steps as i64));
        assert!(state(&app) == states[0], "undoing everything gives the start back, vertex for vertex");
        app.history_jump(steps as i64);
        assert!(state(&app) == *states.last().unwrap(), "and redoing everything the end");
    }

    #[test]
    fn zmodeler_extrudes_a_polygon_and_inserts_an_edge_loop_through_the_input_path() {
        let mut app = App::new(4, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        let (p, i) = Primitive::Cube.build(2, 1.0);
        app.replace_active_mesh(SculptMesh::from_raw(p, i));
        app.workspace.select_brush(&mut app.brush, BrushKind::ZModeler);
        let drag = |app: &mut App, from: Vec2, to: Vec2| {
            app.cursor_moved(from, false);
            app.left_press(false, false);
            for k in 1..=5 {
                app.cursor_moved(from + (to - from) * (k as f32 / 5.0), false);
            }
            app.left_release();
        };
        // QMesh on the front quad at (0.5, 0.5): pulled toward the viewer.
        let c = app.project(Vec3::new(0.5, 0.5, 1.0));
        app.cursor_moved(c, false);
        assert!(matches!(app.zm_hover, Some(zmodeler::Hover::Poly(_))), "{:?}", app.zm_hover);
        let size0 = app.doc.bounds().max.z;
        drag(&mut app, c, c - Vec2::new(0.0, 60.0));
        let pm = &app.zmodel.as_ref().unwrap().poly;
        assert_eq!(pm.faces.len(), 28, "the quad and four walls");
        // The quad left its plane (along its normal: in or out, by the drag's sense).
        let off = app.doc.mesh().positions.iter().map(|p| (p.z - size0).abs()).filter(|d| *d < 0.9).fold(0.0f32, f32::max);
        assert!(off > 0.05, "moved off the front by {off}");
        assert!(app.history.names().0.last().unwrap().starts_with("ZModeler QMesh"));
        app.history_jump(-1);
        assert_eq!(app.doc.mesh().triangle_count(), 48);
        // Insert on the edge from (0, 0, 1) to (1, 0, 1): a loop round the cube.
        let e = app.project(Vec3::new(0.5, 0.0, 1.0));
        app.cursor_moved(e, false);
        assert!(matches!(app.zm_hover, Some(zmodeler::Hover::Edge(..))), "{:?}", app.zm_hover);
        drag(&mut app, e, e + Vec2::new(10.0, 0.0));
        assert_eq!(app.zmodel.as_ref().unwrap().poly.faces.len(), 32);
        assert!(app.zmodel.as_ref().unwrap().poly.faces.iter().all(|f| f.len() == 4));
        // The pairing is kept: the triangle mesh pairs back into the same 32 quads.
        let (again, _) = zmodeler::PolyMesh::from_mesh(app.doc.mesh());
        assert_eq!(again.faces.len(), 32);
        app.history_jump(-1);
        // X symmetry: QMesh on one quad pulls its mirror image out too.
        app.workspace.symmetry = true;
        let c = app.project(Vec3::new(0.5, 0.5, 1.0));
        app.cursor_moved(c, false);
        drag(&mut app, c, c - Vec2::new(0.0, 60.0));
        // The two quads share an edge, so they go out as one block: six walls.
        assert_eq!(app.zmodel.as_ref().unwrap().poly.faces.len(), 30, "both quads and their walls");
        let b = app.doc.bounds();
        assert!((b.max.x + b.min.x).abs() < 1e-4, "still symmetric: {:?}", b);
        app.history_jump(-1);
        // Spin, then Bridge two polygons with two clicks.
        app.workspace.symmetry = false;
        app.workspace.zm_edge = zmodeler::EdgeAction::Spin;
        let e = app.project(Vec3::new(0.5, 0.0, 1.0));
        app.cursor_moved(e, false);
        drag(&mut app, e, e);
        assert_eq!(app.history.names().0.last().map(String::as_str), Some("ZModeler Spin Edge"));
        app.history_jump(-1);
        app.workspace.zm_poly = zmodeler::PolyAction::Bridge;
        // Turn so the +X side shows too, then bridge a front quad to a side quad (no
        // corner in common).
        app.camera.yaw = 0.6;
        if app.camera.forward().x > 0.0 {
            app.camera.yaw = -0.6;
        }
        if app.camera.forward().z > 0.0 {
            app.camera.yaw = std::f32::consts::PI - app.camera.yaw;
        }
        let (p1, p2) = (app.project(Vec3::new(0.5, 0.5, 1.0)), app.project(Vec3::new(1.0, -0.5, -0.5)));
        app.cursor_moved(p1, false);
        drag(&mut app, p1, p1);
        assert!(app.zm_bridge.is_some(), "the first polygon is picked");
        app.cursor_moved(p2, false);
        drag(&mut app, p2, p2);
        assert!(app.zm_bridge.is_none());
        assert_eq!(app.zmodel.as_ref().unwrap().poly.faces.len(), 24 - 2 + 4);
    }

    #[test]
    fn zremesher_rebuilds_the_subtool_in_quads_as_one_undo_step() {
        let mut app = App::new(40, false, usize::MAX);
        let before = app.doc.mesh().positions.clone();
        app.workspace.zremesh.target_k = 0.5;
        app.zremesh_now();
        let quads = app.doc.mesh().triangle_count() / 2;
        assert!((250..800).contains(&quads), "{quads} quads");
        assert_eq!(app.history.names().0.last().map(String::as_str), Some("ZRemesher"));
        app.history_jump(-1);
        assert_eq!(app.doc.mesh().positions, before);
        // With subdivision levels it refuses.
        assert!(app.doc.divide());
        let n = app.doc.mesh().vertex_count();
        app.zremesh_now();
        assert_eq!(app.doc.mesh().vertex_count(), n);
    }

    #[test]
    fn make_boolean_mesh_carves_the_subtract_subtool_out() {
        let mut app = App::new(24, false, usize::MAX);
        let (p, i) = cube_sphere(24, 0.6);
        let bite = SculptMesh::from_raw(p.into_iter().map(|q| q + Vec3::new(0.0, 0.0, 0.9)).collect(), i);
        app.insert_after_active(vec![("Bite".into(), bite)]);
        app.subtools[1].boolean = boolean::BoolOp::Subtract;
        app.workspace.boolean_resolution = 160.0;
        app.workspace.live_boolean = true;
        app.make_boolean_mesh();
        assert_eq!(app.subtools.len(), 3);
        assert_eq!(app.active_tool, 2);
        assert_eq!(app.subtools[2].name, "Boolean 1");
        assert!(!app.subtools[0].visible && !app.subtools[1].visible, "the operands are hidden");
        assert!(!app.workspace.live_boolean);
        let m = app.doc.mesh();
        assert!(m.triangle_count() > 1000);
        assert!(m.positions.iter().all(|p| p.distance(Vec3::new(0.0, 0.0, 0.9)) > 0.55), "the bite is gone");
        assert!(m.positions.iter().any(|p| p.z < -0.95), "the far side stays");
    }

    #[test]
    fn sculptris_pro_adds_triangles_under_the_stroke_as_one_undo_step() {
        let mut app = App::new(8, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app.workspace.sculptris = true;
        app.workspace.select_brush(&mut app.brush, BrushKind::Draw);
        app.brush.radius = 0.3;
        app.brush.strength = 0.5;
        let before = app.doc.mesh().positions.clone();
        let tris = app.doc.mesh().triangle_count();
        let c = app.project(Vec3::new(0.0, 0.0, 1.0));
        let stroke = |app: &mut App, from: Vec2, to: Vec2| {
            app.cursor_moved(from, false);
            app.left_press(false, false);
            for k in 1..=10 {
                app.cursor_moved(from + (to - from) * (k as f32 / 10.0), false);
            }
            app.left_release();
        };
        stroke(&mut app, c - Vec2::new(60.0, 0.0), c + Vec2::new(60.0, 0.0));
        let after = app.doc.mesh().triangle_count();
        assert!(after > tris + 200, "{tris} → {after}");
        // The stroke still raised the surface where it went.
        let top = app.doc.mesh().positions.iter().map(|p| p.z).fold(f32::MIN, f32::max);
        assert!(top > 1.0, "{top}");
        assert_eq!(app.history.names().0.last().map(String::as_str), Some("Draw"));
        // Undo keeps only the region the stroke changed, and puts the same surface back
        // (its vertices may come back in another order).
        let sorted = |p: &[Vec3]| {
            let mut v: Vec<[u32; 3]> = p.iter().map(|q| q.to_array().map(f32::to_bits)).collect();
            v.sort_unstable();
            v
        };
        let sculpted = sorted(&app.doc.mesh().positions);
        let kept = app.history.bytes_last();
        assert!(kept < 48 * after, "undo holds {kept} bytes");
        app.history_jump(-1);
        assert_eq!(sorted(&app.doc.mesh().positions), sorted(&before));
        assert_eq!(app.doc.mesh().triangle_count(), tris);
        app.history_jump(1);
        assert_eq!(app.doc.mesh().triangle_count(), after);
        assert_eq!(sorted(&app.doc.mesh().positions), sculpted);
        // With subdivision levels it sculpts as usual.
        app.history_jump(-1);
        assert!(app.doc.divide());
        let n = app.doc.mesh().triangle_count();
        stroke(&mut app, c - Vec2::new(60.0, 0.0), c + Vec2::new(60.0, 0.0));
        assert_eq!(app.doc.mesh().triangle_count(), n);
    }

    #[test]
    fn insert_mesh_adds_a_masked_off_polygroup_sized_by_the_drag() {
        let mut app = App::new(16, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app.workspace.select_brush(&mut app.brush, BrushKind::InsertMesh);
        app.workspace.insert_shape = Primitive::Cube;
        let before = app.doc.mesh().vertex_count();
        let groups = app.doc.mesh().max_group();
        let c = app.project(Vec3::new(0.0, 0.0, 1.0));
        let press = |app: &mut App, to: Vec2| {
            app.cursor_moved(c, false);
            app.left_press(false, false);
            for k in 1..=5 {
                app.cursor_moved(c + (to - c) * (k as f32 / 5.0), false);
            }
            app.left_release();
        };
        press(&mut app, c + Vec2::new(60.0, 0.0));
        let mesh = app.doc.mesh();
        let n = mesh.vertex_count();
        assert!(n > before, "the cube went in");
        assert_eq!(mesh.max_group(), groups + 1);
        // The old part is masked, the cube is not, and the cube sits on the front of the
        // sphere, about as wide as the drag.
        assert!(mesh.mask[..before].iter().all(|&m| m == 255) && mesh.mask[before..].iter().all(|&m| m == 0));
        let (lo, hi) = mesh.positions[before..].iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let mid = (lo + hi) * 0.5;
        assert!((mid - Vec3::new(0.0, 0.0, 1.0)).length() < 0.05, "{mid}");
        let world = (app.view().unproject_at(app.canvas_cursor(), mid) - mid).length();
        assert!(((hi.x - lo.x) * 0.5 / world - 1.0).abs() < 0.1, "{} vs {world}", (hi.x - lo.x) * 0.5);
        assert_eq!(app.history.names().0.last().map(String::as_str), Some("Insert Cube"));
        // One undo takes the cube and the mask away; redo puts both back.
        app.history_jump(-1);
        assert_eq!(app.doc.mesh().vertex_count(), before);
        assert!(app.doc.mesh().mask.iter().all(|&m| m == 0));
        app.history_jump(1);
        assert_eq!(app.doc.mesh().vertex_count(), n);
        assert_eq!(app.doc.mesh().mask[0], 255);
        // With subdivision levels it refuses.
        app.history_jump(-1);
        assert!(app.doc.divide());
        let levels = app.doc.mesh().vertex_count();
        press(&mut app, c + Vec2::new(60.0, 0.0));
        assert_eq!(app.doc.mesh().vertex_count(), levels);
    }

    fn worst(a: &[Vec3], b: &[Vec3]) -> f32 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(p, q)| (*p - *q).length()).fold(0.0, f32::max)
    }

    #[test]
    fn a_subtool_saves_alone_and_loads_into_another_model() {
        let dir = std::env::temp_dir().join(format!("klay-subtool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("part.klay");
        // Two subtools; the active one divided once and sculpted at the top level.
        let mut app = App::new(12, false, usize::MAX);
        app.append_primitive();
        assert!(app.doc.divide());
        let hit = app.doc.mesh().positions[0];
        let mut stroke = Stroke::begin(false, false, false);
        stroke.dab(app.doc.mesh_mut(), &Brush { radius: 0.3, strength: 1.0, ..Brush::default() }, hit, hit.normalize(), -Vec3::Z, (hit * 3.0, -hit));
        let snapshot = stroke.end(app.doc.mesh_mut()).unwrap();
        app.doc.finish_stroke(1, snapshot);
        let name = app.subtools[app.active_tool].name.clone();
        let sculpted = app.doc.pack_copy().unpack().mesh().positions.clone();
        app.subtool_path = path.to_string_lossy().into_owned();
        app.save_subtool();
        // The file holds that subtool alone, with both levels; Open reads it.
        let saved = project::open(&path).unwrap();
        assert_eq!(saved.tools.len(), 1);
        assert_eq!(saved.tools[0].name, name);
        assert_eq!(saved.tools[0].packed.clone().unpack().count(), 2);
        // Load adds it to another model, parked at the end; the active subtool stays.
        let mut other = App::new(10, false, usize::MAX);
        other.subtool_path = app.subtool_path.clone();
        let before = other.doc.mesh().positions.clone();
        other.load_subtools();
        assert_eq!(other.subtools.len(), 2);
        assert_eq!((other.active_tool, other.subtools[1].name.as_str()), (0, name.as_str()));
        assert_eq!(other.doc.mesh().positions, before);
        other.activate(1);
        assert_eq!(other.doc.count(), 2);
        assert_eq!(worst(&other.doc.mesh().positions, &sculpted), 0.0, "levels and sculpting came back exactly");
        // A file that is not a project adds nothing.
        std::fs::write(&path, b"not a project").unwrap();
        other.load_subtools();
        assert_eq!(other.subtools.len(), 2);
        // Export OBJ writes the active subtool alone, next to the path.
        app.subtool_path = path.to_string_lossy().into_owned();
        app.export_subtool_obj();
        let obj = std::fs::read_to_string(path.with_extension("obj")).unwrap();
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), app.doc.mesh().vertex_count());
        assert_eq!(obj.lines().filter(|l| l.starts_with("o ")).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quicksave_rotates_five_files_that_open_as_projects() {
        let dir = std::env::temp_dir().join(format!("klay-quicksave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = App::new(10, false, usize::MAX);
        app.append_primitive();
        for _ in 0..7 {
            app.quicksave_in(&dir);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(files.len(), 5, "{files:?}");
        assert!(files.iter().all(|f| f.starts_with("QuickSave_") && f.ends_with(".klay")));
        let project = project::open(&dir.join("QuickSave_1.klay")).unwrap();
        assert_eq!(project.tools.len(), 2);
        assert_eq!(app.quicksaved_at.1, app.history.edits());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_exported_obj_imports_as_a_new_active_subtool() {
        let dir = std::env::temp_dir().join(format!("klay-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(12, false, usize::MAX);
        app.primitive.kind = Primitive::Cube;
        app.append_primitive();
        let (verts, groups) = (app.doc.mesh().vertex_count(), app.doc.mesh().group_count());
        app.subtool_path = dir.join("cube.klay").to_string_lossy().into_owned();
        app.export_subtool_obj();
        app.import_path = dir.join("cube.obj").to_string_lossy().into_owned();
        app.import_mesh(&app.import_path.clone());
        assert_eq!(app.subtools.len(), 3);
        assert_eq!(app.active_tool, 2);
        assert_eq!(app.subtools[2].name, "cube");
        assert_eq!(app.doc.mesh().vertex_count(), verts);
        assert_eq!(app.doc.mesh().group_count(), groups);
        // Fitted: the largest extent is 2.
        let b = app.doc.bounds();
        assert!(((b.max - b.min).max_element() - 2.0).abs() < 1e-4);
        // A missing file adds nothing.
        app.import_mesh("/nonexistent/x.obj");
        assert_eq!(app.subtools.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stl_exports_checks_sends_and_imports_back_at_print_size() {
        let dir = std::env::temp_dir().join(format!("klay-stl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(12, false, usize::MAX);
        app.primitive.kind = Primitive::Cube;
        app.append_primitive();
        app.workspace.layout.print.scale = 10.0;
        let extent = app.print_extent().unwrap();
        let tris = app.doc.mesh().triangle_count() + app.subtools[0].parked.as_ref().unwrap().packed.clone().unpack().mesh().triangle_count();
        // Export: both subtools in one binary STL, in mm, Z up.
        app.stl_path = dir.join("model.stl").to_string_lossy().into_owned();
        app.export_stl();
        let bytes = std::fs::read(&app.stl_path).unwrap();
        assert_eq!(bytes.len(), 84 + 50 * tris, "{}", app.status);
        let file = stl::parse(&bytes).unwrap();
        let (lo, hi) = file.positions.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(a, b), &p| (a.min(p), b.max(p)));
        let printed = hi - lo;
        assert!((printed - Vec3::new(extent.x, extent.z, extent.y) * 10.0).abs().max_element() < 1e-3, "{printed} vs {extent}");
        let c = print::check(&file.indices);
        assert!(c.watertight() && c.shells == 2, "{c:?}");
        // Check: two closed subtools; it goes stale when the model changes.
        app.print_check();
        let report = app.print_report.clone().unwrap();
        assert_eq!(report.rows.len(), 2);
        assert!(report.total.watertight() && report.total.triangles == tris);
        assert_eq!(report.stamp, app.print_stamp());
        app.subtools[0].visible = false;
        assert_ne!(report.stamp, app.print_stamp());
        app.subtools[0].visible = true;
        // Send: a custom command gets the STL.
        let copied = dir.join("copied.stl");
        app.workspace.layout.print.slicer = print::CUSTOM.into();
        app.workspace.layout.print.command = format!("sh -c 'cp \"$1\" \"$2\"' sh {{file}} '{}'", copied.display());
        app.send_to_slicer_in(&dir.join("send"));
        assert!(app.status.starts_with("Sent"), "{}", app.status);
        let t = Instant::now();
        while !copied.exists() && t.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(std::fs::read(&copied).unwrap(), bytes);
        // Import without Fit: back in Klay's frame at print size, welded and closed.
        app.workspace.import_fit = false;
        app.import_mesh(&app.stl_path.clone());
        assert_eq!((app.subtools.len(), app.active_tool), (3, 2));
        assert_eq!(app.subtools[2].name, "model");
        let b = app.doc.bounds();
        assert!(((b.max - b.min) - extent * 10.0).abs().max_element() < 1e-3);
        assert!(print::check(&app.doc.mesh().indices).watertight());
        assert_eq!(app.doc.mesh().triangle_count(), tris);
        // Without a window the dialogs fall back to the stored paths.
        app.ask_file(file_dialog::Ask::ExportStl);
        assert!(matches!(app.pending_doc, Some(DocOp::ExportStl)));
        app.ask_file(file_dialog::Ask::ImportMesh);
        assert!(matches!(app.pending_doc, Some(DocOp::ImportMesh)));
        app.pending_doc = None;
        // A broken STL adds nothing.
        std::fs::write(dir.join("bad.stl"), b"solid nothing\nendsolid\n").unwrap();
        app.import_mesh(&dir.join("bad.stl").to_string_lossy());
        assert_eq!(app.subtools.len(), 3);
        assert!(app.status.starts_with("Import failed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subtools_merge_split_and_extract() {
        let mut app = App::new(12, false, usize::MAX);
        app.primitive.kind = Primitive::Cube;
        app.append_primitive();
        app.select_subtool(0);
        let (a, b) = (app.doc.mesh().triangle_count(), {
            let p = app.subtools[1].parked.as_ref().unwrap().packed.clone().unpack();
            p.mesh().triangle_count()
        });
        // Merge Down: one subtool with both meshes, the cube's six groups after the sphere's.
        app.merge_down();
        assert_eq!(app.subtools.len(), 1);
        assert_eq!(app.doc.mesh().triangle_count(), a + b);
        assert_eq!(app.doc.mesh().group_count(), 7);
        // Groups Split: seven subtools again, the first one active.
        app.groups_split();
        assert_eq!(app.subtools.len(), 7);
        assert_eq!(app.active_tool, 0);
        assert_eq!(app.doc.mesh().triangle_count(), a);
        // Split Hidden: hide half the sphere, split it off.
        let mesh = app.doc.mesh();
        let hidden: Vec<u8> = (0..mesh.triangle_count()).map(|t| (t % 2) as u8).collect();
        let n = mesh.triangle_count();
        app.doc.set_hidden(0, hidden);
        app.split_hidden();
        assert_eq!(app.subtools.len(), 8);
        assert_eq!(app.doc.mesh().triangle_count(), n / 2);
        assert!(app.doc.mesh().hidden.is_empty());
        // Extract: nothing masked does nothing; a mask makes a shell subtool.
        app.extract();
        assert_eq!(app.subtools.len(), 8);
        app.mask_op(MaskOp::All);
        app.extract();
        assert_eq!(app.subtools.len(), 9);
        assert!(app.subtools[1].name.ends_with("extract"));
        // Subdivided subtools are refused.
        assert!(app.doc.divide());
        app.groups_split();
        assert_eq!(app.subtools.len(), 9);
    }

    #[test]
    fn del_hidden_close_holes_and_mirror_weld_are_one_undo_step_each() {
        let mut app = App::new(12, false, usize::MAX);
        let tris = app.doc.mesh().triangle_count();
        // Del Hidden: hide the top half, delete it, undo.
        let mesh = app.doc.mesh();
        let hidden: Vec<u8> = (0..mesh.triangle_count()).map(|t| {
            let i = 3 * t;
            (mesh.indices[i..i + 3].iter().map(|&v| mesh.positions[v as usize].y).sum::<f32>() > 0.0) as u8
        }).collect();
        app.doc.set_hidden(0, hidden);
        app.topology_op("Del Hidden", |m| subtool_ops::part(m, |t| m.visible(t)));
        let open = app.doc.mesh().triangle_count();
        assert!(open < tris && app.doc.mesh().hidden.is_empty());
        // Close Holes caps the cut; the history has both steps.
        app.topology_op("Close Holes", subtool_ops::holes_closed);
        assert!(app.doc.mesh().triangle_count() > open);
        assert_eq!(app.history.names().0, ["Del Hidden", "Close Holes"]);
        app.history_jump(-2);
        assert_eq!(app.doc.mesh().triangle_count(), tris);
        // Mirror And Weld keeps a closed sphere closed; refused with subdivision levels.
        app.topology_op("Mirror And Weld", subtool_ops::mirrored_and_welded);
        assert!(app.doc.mesh().triangle_count() >= tris);
        assert!(app.doc.divide());
        let n = app.history.names().0.len();
        app.topology_op("Close Holes", subtool_ops::holes_closed);
        assert_eq!(app.history.names().0.len(), n);
    }

    #[test]
    fn shape_deformations_reshape_the_shown_level_and_undo() {
        let mut app = App::new(12, false, usize::MAX);
        assert!(app.doc.divide());
        let before = app.doc.mesh().positions.clone();
        app.deform(Deform::Inflate, 60.0);
        let after = app.doc.mesh().positions.clone();
        let grew = after.iter().map(|p| p.length()).sum::<f32>() > before.iter().map(|p| p.length()).sum::<f32>();
        assert!(grew, "Inflate grows the sphere");
        // One undo step, exact.
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().positions, before);
        app.history.redo(&mut app.doc);
        assert!(app.doc.mesh().positions.iter().zip(&after).all(|(a, b)| a.distance(*b) < 1e-6));
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
            // Cube: a polygroup per face; Cylinder: the wall and each cap; the rest: one.
            let groups = match kind {
                Primitive::Cube => 6,
                Primitive::Cylinder => 3,
                _ => 1,
            };
            assert_eq!(app.doc.mesh().group_count(), groups, "{kind:?} appended");
            // Beside the model like the old sphere append: to its right with a gap of a
            // quarter of its size, centred on the model in Y and Z, half the size.
            let b = app.doc.bounds();
            let gap = b.min.x - first.max.x;
            assert!((gap - 0.125).abs() < 1e-3, "{kind:?} gap {gap}");
            let (c, fc) = ((b.min + b.max) * 0.5, (first.min + first.max) * 0.5);
            assert!((c.y - fc.y).abs() < 1e-3 && (c.z - fc.z).abs() < 1e-3, "{kind:?} centre {c:?}");
            assert!(((b.max - b.min).max_element() - 1.0).abs() < 1e-3, "{kind:?} is half size");
            // It divides like any subtool, keeping its polygroups.
            app.divide();
            assert_eq!(app.doc.top(), 1);
            assert_eq!(app.doc.mesh().group_count(), groups, "{kind:?} divided");

            // Rebuild keeps the place and size, renames an automatic name, keeps a chosen one.
            app.primitive.kind = Primitive::Sphere;
            app.rebuild();
            let r = app.doc.bounds();
            assert_eq!(app.doc.top(), 0);
            assert_eq!(app.subtools[1].name, "Sphere 2");
            assert_eq!(app.doc.mesh().group_count(), 1, "a rebuilt Sphere is one polygroup");
            assert!((((r.min + r.max) * 0.5) - c).length() < 1e-3, "{kind:?} rebuilt in place");
            app.subtools[1].name = "Head".into();
            app.rebuild();
            assert_eq!(app.subtools[1].name, "Head");

            app.primitive.kind = kind;
            app.new_project();
            assert_eq!(app.subtools.len(), 1);
            assert_eq!(app.doc.mesh().vertex_count(), kind.vertex_count(kind.default_segments()));
            assert!(app.subtools[0].name.starts_with(kind.short()));
            assert_eq!(app.doc.mesh().group_count(), groups, "{kind:?} new project");
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
        app.brush = Brush { kind: BrushKind::Draw, radius: 0.8, strength: 1.0, ..Brush::default() };
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

        // Ctrl + drag on the background only clears the mask while Wedge is off (it never
        // rotates).
        let yaw = app.camera.yaw;
        drag(&mut app, ModifiersState::CONTROL, corner, corner + Vec2::splat(60.0));
        assert_eq!(app.doc.mesh().positions, original);
        assert_eq!(app.camera.yaw, yaw);
        // With no mask to clear, it says how to remesh with it.
        assert!(app.workspace.pending_notice.take().is_some_and(|n| n.contains("Remesh is on")));

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
        // A Ctrl click on the background inverts the mask (ZBrush), it does not re-wedge.
        drag(&mut app, ModifiersState::CONTROL, corner, corner);
        assert_eq!(app.doc.mesh().vertex_count(), second);
        assert!(app.doc.mesh().mask.iter().all(|&m| m == 255));
        // With a mask, Ctrl + drag on the background still re-wedges; the new surface starts
        // without a mask, and undo brings the masked one back.
        app.wedge.resolution = 56;
        drag(&mut app, ModifiersState::CONTROL, corner, corner + Vec2::splat(60.0));
        let third = app.doc.mesh().vertex_count();
        assert!(third != second && app.doc.mesh().mask.is_empty());
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().vertex_count(), second);
        assert!(app.doc.mesh().mask.iter().all(|&m| m == 255), "the mask came back");
        app.history.undo(&mut app.doc);
        assert!(app.doc.mesh().mask.iter().all(|&m| m == 0));

        // Undo twice more: the sphere exactly as it was, with its groups.
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
    fn scale_middle_circle_squashes_along_the_line() {
        let mut app = App::new(24, false, usize::MAX);
        app.camera.aspect = 1.6;
        let start = app.doc.mesh().positions.clone();
        app.on_key(KeyCode::KeyE);
        // A vertical line down the front of the sphere, then its middle circle dragged left.
        let top = app.project(Vec3::new(0.0, 0.5, 0.9));
        drag(&mut app, top, top + Vec2::new(0.0, 200.0));
        let line = app.action_line.expect("a line");
        // Extent along the line and across it.
        let axis = (line.end - line.start).normalize();
        let across = axis.cross(app.camera.forward()).normalize();
        let extent = |app: &App, d: Vec3| {
            let ts = app.doc.mesh().positions.iter().map(|p| p.dot(d));
            let (lo, hi) = ts.fold((f32::MAX, f32::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
            hi - lo
        };
        let (along0, across0) = (extent(&app, axis), extent(&app, across));
        let mid = app.project(line.mid());
        drag(&mut app, mid, mid - Vec2::new(150.0, 0.0));
        assert!((extent(&app, axis) / along0 - 0.5).abs() < 0.01, "{} vs {along0}", extent(&app, axis));
        assert!((extent(&app, across) / across0 - 1.0).abs() < 1e-3);
        app.history_jump(-1);
        assert!(worst(&app.doc.mesh().positions, &start) < 1e-4);
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
        app.workspace.assets = sculpt::StampSettings::default();
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
    fn ctrl_masks_and_shift_smooths_with_their_own_z_intensity() {
        // ZBrush: the Ctrl mask brush and Shift's Smooth keep their own Z Intensity; the
        // sculpt brush's does not leak into them.
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        let peak = |app: &App| app.doc.mesh().mask.iter().copied().max().unwrap_or(0);
        app.workspace.select_brush(&mut app.brush, BrushKind::ClayBuildup);
        app.brush.strength = 0.05;
        drag(&mut app, c, c + Vec2::new(30.0, 0.0), ModifiersState::CONTROL);
        assert_eq!(peak(&app), 255, "Mask Pen starts at full intensity");
        assert_eq!(app.brush.strength, 0.05, "the sculpt brush keeps its own");
        // Turn the mask brush down: Ctrl masks lightly, whatever the sculpt brush has.
        app.mask_op(MaskOp::Clear);
        app.workspace.select_brush(&mut app.brush, BrushKind::MaskPen);
        app.brush.strength = 0.1;
        app.workspace.select_brush(&mut app.brush, BrushKind::ClayBuildup);
        app.brush.strength = 1.0;
        drag(&mut app, c, c + Vec2::new(30.0, 0.0), ModifiersState::CONTROL);
        let light = peak(&app);
        assert!(light > 0 && light < 128, "{light}");
        // Shift smooths with Smooth's intensity, at the shared Draw Size.
        app.brush.radius = 0.31;
        app.modifiers = ModifiersState::SHIFT;
        let smooth = app.effective_brush();
        app.modifiers = ModifiersState::empty();
        assert_eq!((smooth.kind, smooth.strength, smooth.radius), (BrushKind::Smooth, 1.0, 0.31));
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
    fn masking_polygroups_and_visibility_work_together() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        let n = app.doc.mesh().vertex_count();
        // Paint a mask, then Ctrl + W: the masked polygons become a group and the mask goes.
        drag(&mut app, c, c + Vec2::new(80.0, 0.0), ModifiersState::CONTROL);
        let painted = masked(&app);
        assert!(painted > 0);
        app.modifiers = ModifiersState::CONTROL;
        app.on_key(KeyCode::KeyW);
        app.modifiers = ModifiersState::empty();
        app.run_pending_vis();
        assert_eq!(app.doc.mesh().group_count(), 2);
        assert!(app.doc.mesh().mask.is_empty(), "Group Masked clears the mask");
        // The new group sits where the mask was.
        let mesh = app.doc.mesh();
        let in_group = (0..mesh.triangle_count() as u32).filter(|&t| mesh.group(t) == 1).count();
        assert!(in_group > 0 && in_group < mesh.triangle_count() / 2);
        // One undo brings back both the mask and the single group.
        app.history.undo(&mut app.doc);
        assert_eq!(masked(&app), painted);
        assert_eq!(app.doc.mesh().group_count(), 1);
        app.history.redo(&mut app.doc);
        assert_eq!(app.doc.mesh().group_count(), 2);
        assert!(app.doc.mesh().mask.is_empty());

        // Hide everything but the new group (Ctrl + Shift + click on it).
        drag(&mut app, c, c, ModifiersState::CONTROL | ModifiersState::SHIFT);
        let shown = app.doc.mesh().shown.clone();
        assert!(!shown.is_empty());
        let hidden_only: Vec<usize> = (0..n).filter(|&v| shown[v] == 0).collect();
        assert!(!hidden_only.is_empty());
        // Ctrl + A masks what is shown only; inverting leaves hidden vertices alone too.
        assert!(app.mask_op(MaskOp::All));
        assert!(hidden_only.iter().all(|&v| app.doc.mesh().mask[v] == 0));
        assert!((0..n).filter(|&v| shown[v] != 0).all(|v| app.doc.mesh().mask[v] == 255));
        app.mask_op(MaskOp::Clear);
        // A mask stroke across the border does not reach hidden vertices.
        drag(&mut app, c, c + Vec2::new(300.0, 0.0), ModifiersState::CONTROL);
        assert!(hidden_only.iter().all(|&v| app.doc.mesh().mask.get(v).copied().unwrap_or(0) == 0));
        app.mask_op(MaskOp::Clear);
        // Transpose moves what is shown; hidden vertices stay.
        let before = app.doc.mesh().positions.clone();
        app.on_key(KeyCode::KeyW);
        drag(&mut app, c, c + Vec2::new(150.0, 0.0), ModifiersState::empty());
        let mid = app.project(app.action_line.unwrap().mid());
        drag(&mut app, mid, mid + Vec2::new(0.0, -100.0), ModifiersState::empty());
        let after = &app.doc.mesh().positions;
        assert!(hidden_only.iter().all(|&v| after[v] == before[v]), "a hidden vertex moved");
        assert!((0..n).any(|v| shown[v] != 0 && (after[v] - before[v]).length() > 0.05), "the shown part moved");
        app.history.undo(&mut app.doc);
        let worst = app.doc.mesh().positions.iter().zip(&before).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4);
    }

    #[test]
    fn project_versions_1_2_and_3_all_open() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        drag(&mut app, c, c + Vec2::new(80.0, 0.0), ModifiersState::CONTROL);
        app.group_visible();
        let centre = app.viewport() * 0.5;
        drag(&mut app, centre - Vec2::splat(150.0), centre + Vec2::splat(150.0), ModifiersState::CONTROL | ModifiersState::SHIFT);
        drag(&mut app, c, c + Vec2::new(60.0, 30.0), ModifiersState::CONTROL);
        let mask = app.doc.mesh().mask.clone();
        assert!(!mask.is_empty() && app.doc.mesh().hidden_count() > 0);
        let marks = app.doc.marks();
        let packed = app.doc.pack_copy();
        for version in [1u32, 2, 3] {
            let bytes = project::write_version(Vec::new(), &[("A", true, &packed)], 0, version).unwrap();
            let p = project::read_from(&bytes[..], bytes.len() as u64).unwrap_or_else(|e| panic!("version {version}: {e}"));
            let back = p.tools.into_iter().next().unwrap().packed.try_unpack().unwrap();
            assert_eq!(back.mesh().mask.is_empty(), version < 2, "version {version} mask");
            if version >= 2 {
                assert_eq!(back.mesh().mask, mask);
            }
            if version >= 3 {
                assert_eq!(back.marks(), marks);
            } else {
                assert!(back.marks().iter().all(|m| m.hidden.is_empty() && m.groups.lens.is_empty()));
            }
        }
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

#[cfg(test)]
mod pressure_tests {
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

    /// Largest move of any vertex since `before`, and how many moved.
    fn moved(app: &App, before: &[Vec3]) -> (f32, usize) {
        let d: Vec<f32> = app.doc.mesh().positions.iter().zip(before).map(|(a, b)| (*a - *b).length()).collect();
        (d.iter().copied().fold(0.0, f32::max), d.iter().filter(|&&x| x > 1e-7).count())
    }

    #[test]
    fn pressure_goes_through_the_tablet_pressure_curves() {
        let mut app = App::new(8, false, usize::MAX);
        let brush = Brush { kind: BrushKind::Draw, radius: 0.4, strength: 0.8, ..Brush::default() };
        app.pressure = Some(0.5);
        let linear = app.at_pressure(brush);
        assert!((linear.radius - 0.2).abs() < 1e-5 && (linear.strength - 0.4).abs() < 1e-5);
        app.workspace.size_curve = pressure::PressureCurve::preset("Soft");
        app.workspace.intensity_curve = pressure::PressureCurve::preset("Firm");
        let curved = app.at_pressure(brush);
        assert!(curved.radius < linear.radius, "Soft: a half press gives less than half the size");
        assert!(curved.strength > linear.strength, "Firm: a half press gives more than half the intensity");
        // Smooth with Tablet Pressure of its own: no size pressure, linear intensity; Draw
        // keeps the global curves.
        let smooth = Brush { kind: BrushKind::Smooth, ..brush };
        app.workspace.brush_pressure.insert(
            BrushKind::Smooth,
            workspace::TabletPressure { size: false, intensity: true, size_curve: pressure::PressureCurve::linear(), intensity_curve: pressure::PressureCurve::linear() },
        );
        let own = app.at_pressure(smooth);
        assert!(own.radius == 0.4 && (own.strength - 0.4).abs() < 1e-5, "{own:?}");
        assert_eq!(app.at_pressure(brush).radius, curved.radius);
        app.pressure = None;
        assert_eq!(app.at_pressure(brush).radius, 0.4, "a mouse ignores the curves");
    }

    #[test]
    fn mask_strokes_follow_pen_pressure() {
        let total = |p: Option<f32>| -> u64 {
            let mut app = app();
            app.pressure = p;
            let c = app.project(Vec3::ZERO);
            drag(&mut app, c, c + Vec2::new(80.0, 0.0), ModifiersState::CONTROL);
            app.doc.mesh().mask.iter().map(|&m| m as u64).sum()
        };
        let (mouse, full, half) = (total(None), total(Some(1.0)), total(Some(0.5)));
        assert!(mouse > 0);
        assert_eq!(mouse, full, "a mouse masks like full pressure");
        assert!(half < full / 2, "half pressure: a smaller, lighter mask ({half} vs {full})");
    }

    #[test]
    fn grab_keeps_its_size_and_strength_under_any_pressure() {
        let pull = |p: Option<f32>| -> (f32, usize) {
            let mut app = app();
            app.brush.kind = BrushKind::Grab;
            app.pressure = p;
            let before = app.doc.mesh().positions.clone();
            let c = app.project(Vec3::ZERO);
            drag(&mut app, c, c + Vec2::new(60.0, 0.0), ModifiersState::empty());
            moved(&app, &before)
        };
        let (full, light) = (pull(None), pull(Some(0.1)));
        assert!(full.1 > 0);
        assert_eq!(full.1, light.1);
        assert!((full.0 - light.0).abs() < 1e-6);
    }

    #[test]
    fn draw_follows_pen_pressure_and_the_toggles() {
        let stroke = |p: Option<f32>, size: bool, intensity: bool| -> (f32, usize) {
            let mut app = app();
            app.pressure = p;
            app.workspace.pressure_size = size;
            app.workspace.pressure_intensity = intensity;
            let before = app.doc.mesh().positions.clone();
            let c = app.project(Vec3::ZERO);
            drag(&mut app, c, c + Vec2::new(60.0, 0.0), ModifiersState::empty());
            moved(&app, &before)
        };
        let full = stroke(None, true, true);
        assert_eq!(full, stroke(Some(1.0), true, true));
        let half = stroke(Some(0.5), true, true);
        assert!(half.1 < full.1 && half.0 < full.0 * 0.5);
        // Both off: pressure changes nothing.
        assert_eq!(stroke(Some(0.3), false, false), full);
    }
}

#[cfg(test)]
mod surface_input_tests {
    use super::*;
    fn app()->App {
        let mut app=App::new(30,false,usize::MAX);app.camera.aspect=1.6;
        app.canvas_rect=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1600.0,1000.0));
        app.workspace.assets=sculpt::StampSettings::default();app.brush.strength=1.0;app
    }
    fn drag(app:&mut App,kind:BrushKind,alt:bool) {
        let c=app.project(Vec3::ZERO);app.workspace.select_brush(&mut app.brush,kind);
        app.modifiers=if alt {ModifiersState::ALT}else{ModifiersState::empty()};
        let a=c+Vec2::new(-80.0,-60.0);let b=c+Vec2::new(80.0,60.0);
        app.cursor_moved(a,false);app.left_press(false,false);
        if kind.shape()==Some(sculpt::Shape::Lasso) {
            for p in [Vec2::new(b.x,a.y),b,Vec2::new(a.x,b.y),a] {app.cursor_moved(p,false);}
        } else {app.cursor_moved(b,false);}
        app.left_release();app.modifiers=ModifiersState::empty();
    }
    /// Drag a straight line across the model, `dy` pixels above its middle.
    fn line(app:&mut App,kind:BrushKind,dy:f32,mods:ModifiersState) {
        let c=app.project(Vec3::ZERO);
        if mods.is_empty() {app.workspace.select_brush(&mut app.brush,kind);}
        app.modifiers=mods;
        app.cursor_moved(c+Vec2::new(-500.0,dy),false);app.left_press(false,false);
        app.cursor_moved(c+Vec2::new(500.0,dy),false);app.left_release();
        app.modifiers=ModifiersState::empty();
    }
    fn closed(m:&SculptMesh)->bool {
        let mut count:std::collections::HashMap<(u32,u32),i32>=std::collections::HashMap::new();
        for t in m.indices.chunks(3) {for k in 0..3 {*count.entry((t[k],t[(k+1)%3])).or_insert(0)+=1;}}
        count.iter().all(|(&(a,b),&c)|c==1&&count.get(&(b,a))==Some(&1))
    }
    #[test]
    fn select_rect_and_lasso_keep_what_they_cover_and_alt_hides_it() {
        for kind in [BrushKind::SelectRect,BrushKind::SelectLasso] {
            let mut a=app();let n=a.mesh().triangle_count();
            drag(&mut a,kind,false);
            let shown=(0..n as u32).filter(|&t|a.mesh().visible(t)).count();
            assert!(shown>0 && shown<n,"{kind:?}: {shown} of {n}");
            a.history.undo(&mut a.doc);
            assert_eq!((0..n as u32).filter(|&t|a.mesh().visible(t)).count(),n);
            drag(&mut a,kind,true);
            let hidden=(0..n as u32).filter(|&t|!a.mesh().visible(t)).count();
            assert!(hidden>0 && hidden<n && hidden+shown<=n+n/10,"{kind:?} alt: {hidden}");
        }
    }
    #[test]
    fn clip_rect_and_circle_press_the_model_into_the_outline() {
        for kind in [BrushKind::ClipRect,BrushKind::ClipCircle] {
            let mut a=app();let before=a.mesh().positions.clone();
            drag(&mut a,kind,false);
            let c=a.project(Vec3::ZERO);let (lo,hi)=(c+Vec2::new(-80.0,-60.0),c+Vec2::new(80.0,60.0));
            let screen=a.surface_screen();
            for p in &a.mesh().positions {
                let s=screen.project(*p).unwrap()+a.canvas_origin();
                assert!(s.x>lo.x-1.5&&s.x<hi.x+1.5&&s.y>lo.y-1.5&&s.y<hi.y+1.5,"{kind:?} {s:?}");
                if kind==BrushKind::ClipCircle {
                    let q=(s-c)/Vec2::new(80.0,60.0);assert!(q.length()<1.02,"{q:?}");
                }
            }
            assert_ne!(a.mesh().positions,before);
            a.history.undo(&mut a.doc);assert_eq!(a.mesh().positions,before,"{kind:?} undo");
        }
    }
    #[test]
    fn trim_slice_and_knife_curves_cut_along_the_line() {
        // Trim: the side the arrow points to is gone and the cut is closed flat.
        let mut a=app();let before=(a.mesh().vertex_count(),a.mesh().triangle_count());
        let c=a.project(Vec3::ZERO);
        let (s,e)=(c+Vec2::new(-500.0,-100.0),c+Vec2::new(500.0,-100.0));
        let plane=a.surface_screen().plane(s-a.canvas_origin(),e-a.canvas_origin()).unwrap();
        line(&mut a,BrushKind::TrimCurve,-100.0,ModifiersState::empty());
        let m=a.mesh();
        assert!(m.positions.iter().all(|p|plane.truncate().dot(*p)+plane.w<1e-4),"something is left past the cut");
        assert!(closed(m),"the trimmed model is closed");
        assert!(m.group_count()>=2,"the cap has its own polygroup");
        a.history.undo(&mut a.doc);
        assert_eq!((a.mesh().vertex_count(),a.mesh().triangle_count()),before);
        // Slice: same surface, one more polygroup, no triangle across the line.
        let mut a=app();let groups=a.mesh().group_count();
        line(&mut a,BrushKind::SliceCurve,-100.0,ModifiersState::empty());
        assert_eq!(a.mesh().group_count(),groups+1);
        assert!(closed(a.mesh()));
        // Knife: two closed pieces with polygroups of their own.
        let mut a=app();
        line(&mut a,BrushKind::KnifeCurve,-100.0,ModifiersState::empty());
        assert!(closed(a.mesh()));
        assert!(a.mesh().group_count()>=groups+2);
    }
    #[test]
    fn trim_rect_keeps_only_what_is_inside_through_the_model() {
        let mut a=app();a.workspace.boolean_resolution=96.0;
        drag(&mut a,BrushKind::TrimRect,false);
        let c=a.project(Vec3::ZERO);let (lo,hi)=(c+Vec2::new(-80.0,-60.0),c+Vec2::new(80.0,60.0));
        let screen=a.surface_screen();let slack=12.0;
        assert!(a.mesh().vertex_count()>100);
        for p in &a.mesh().positions {
            let s=screen.project(*p).unwrap()+a.canvas_origin();
            assert!(s.x>lo.x-slack&&s.x<hi.x+slack&&s.y>lo.y-slack&&s.y<hi.y+slack,"{s:?}");
        }
        assert!(closed(a.mesh()));
        a.history.undo(&mut a.doc);
        assert!(a.mesh().positions.iter().all(|p|(p.length()-1.0).abs()<0.05),"undo brings the sphere back");
    }
    #[test]
    fn ctrl_shift_uses_the_last_picked_gesture_tool() {
        let mut a=app();
        a.workspace.select_brush(&mut a.brush,BrushKind::SliceCurve);
        a.workspace.select_brush(&mut a.brush,BrushKind::Draw);
        assert_eq!(a.workspace.gesture_tool,BrushKind::SliceCurve);
        let (groups,before)=(a.mesh().group_count(),a.mesh().positions.len());
        line(&mut a,BrushKind::Draw,-100.0,ModifiersState::CONTROL|ModifiersState::SHIFT);
        assert_eq!(a.brush.kind,BrushKind::Draw);
        assert_eq!(a.mesh().group_count(),groups+1);
        assert!(a.mesh().positions.len()>before);
    }
    #[test]
    fn direct_mask_shapes_commit_and_undo_without_sculpting() {
        for kind in [BrushKind::MaskRect,BrushKind::MaskLasso,BrushKind::MaskCircle,BrushKind::MaskLine] {
            let mut a=app();let before=a.mesh().positions.clone();drag(&mut a,kind,false);
            let after=a.mesh().mask.clone();assert!(after.iter().any(|&v|v>0),"{kind:?}");
            assert_eq!(a.mesh().positions,before);assert!(!a.busy());
            a.history.undo(&mut a.doc);assert!(a.mesh().mask.iter().all(|&v|v==0));
            a.history.redo(&mut a.doc);assert_eq!(a.mesh().mask,after);
            drag(&mut a,kind,true);assert!(a.mesh().mask.iter().all(|&v|v==0),"{kind:?} erase");
        }
    }
    #[test]
    fn direct_mask_click_and_remembered_ctrl_brush_work() {
        let mut a=app();let c=a.project(Vec3::ZERO);let original=a.mesh().positions.clone();
        a.workspace.select_brush(&mut a.brush,BrushKind::MaskPen);
        a.cursor_moved(c,false);a.left_press(false,false);a.left_release();
        assert!(a.mesh().mask.iter().any(|&v|v>0));assert_eq!(a.mesh().positions,original);
        a.mask_op(MaskOp::Clear);
        a.workspace.select_brush(&mut a.brush,BrushKind::MaskCircle);
        a.workspace.select_brush(&mut a.brush,BrushKind::Draw);a.modifiers=ModifiersState::CONTROL;
        a.cursor_moved(c-Vec2::splat(70.0),false);a.left_press(false,false);a.cursor_moved(c+Vec2::splat(70.0),false);a.left_release();
        assert_eq!(a.brush.kind,BrushKind::Draw);assert!(a.mesh().mask.iter().any(|&v|v>0));
        assert_eq!(a.mesh().positions,original);
        a.mask_op(MaskOp::Clear);a.cursor_moved(Vec2::splat(90.0),false);a.left_press(false,false);a.left_release();
        assert!(a.mesh().mask.iter().all(|&v|v==255),"Ctrl background click still inverts with a shape brush");
    }
    #[test]
    fn a_symmetric_clip_in_a_turned_view_cuts_no_deeper_than_the_guide() {
        let mut a=app();a.workspace.symmetry=true;a.camera.yaw+=0.6;a.camera.pitch+=0.25;
        a.workspace.select_brush(&mut a.brush,BrushKind::ClipLine);
        let c=a.project(Vec3::ZERO);
        let before=a.mesh().positions.clone();
        let (s,e)=(c+Vec2::new(-400.0,-150.0),c+Vec2::new(400.0,-150.0));
        let plane=a.surface_screen().plane(s,e).unwrap();
        let side=a.clip_side(&surface_tools::Gesture{end:e,..surface_tools::Gesture::new(BrushKind::ClipLine,s,false)});
        a.cursor_moved(s,false);a.left_press(false,false);a.cursor_moved(e,false);a.left_release();
        // On the half under the line nothing below the guide moves (before the fix the
        // mirrored plane cut this half too, 0.56 deep).
        for (p,q) in before.iter().zip(&a.mesh().positions) {
            if p.x*side>0.0 && plane.truncate().dot(*p)+plane.w<=0.0 {assert_eq!(p,q);}
        }
        // And the top comes down to the guide on both halves alike.
        let top=|v:&[Vec3],s:f32|v.iter().filter(|p|p.x*s>0.0).map(|p|p.y).fold(f32::MIN,f32::max);
        let after=&a.mesh().positions;
        assert!((top(after,1.0)-top(after,-1.0)).abs()<0.02,"{} {}",top(after,1.0),top(after,-1.0));
        assert!(top(after,side)>0.7,"cut deeper than the guide: {}",top(after,side));
    }
    #[test]
    fn a_pie_slice_cut_from_crossing_clips_stays_inside_every_cut() {
        let mut a=app();
        assert!(a.selftest_clip_cuts());
        // Finer meshes have far more outline segments and coincident points.
        for segments in [100, 400] {
            for (origin,size) in [(egui::Pos2::ZERO,egui::vec2(1600.0,1000.0)),(egui::pos2(369.0,90.0),egui::vec2(908.0,864.0))] {
                let mut a=App::new(segments,false,usize::MAX);a.camera.aspect=size.x/size.y;
                a.canvas_rect=egui::Rect::from_min_size(origin,size);
                assert!(a.selftest_clip_cuts(),"{segments} segments, canvas {size:?}");
            }
        }
        // Turned and tilted views.
        for (yaw,pitch) in [(0.4,0.0),(-0.7,0.3),(1.2,-0.4),(2.5,0.6)] {
            let mut a=App::new(150,false,usize::MAX);a.camera.aspect=1.6;
            a.canvas_rect=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1600.0,1000.0));
            a.camera.yaw+=yaw;a.camera.pitch+=pitch;
            assert!(a.selftest_clip_cuts(),"view {yaw} {pitch}");
        }
    }
    #[test]
    fn repeated_clips_leave_the_kept_side_alone() {
        for levels in 0..=2 {
            let mut a=app();
            for _ in 0..levels {a.divide();}
            a.workspace.select_brush(&mut a.brush,BrushKind::ClipLine);
            let c=a.project(Vec3::ZERO);
            // The last two cross the earlier cuts at wide angles, the way a pie slice is cut.
            let cuts=[(Vec2::new(150.0,-400.0),Vec2::new(150.0,400.0)),(Vec2::new(-400.0,-120.0),Vec2::new(400.0,-140.0)),(Vec2::new(-300.0,300.0),Vec2::new(200.0,-300.0)),(Vec2::new(-120.0,400.0),Vec2::new(300.0,-400.0))];
            let mut planes=Vec::new();
            for (k,(s,e)) in cuts.into_iter().enumerate() {
                let before=a.mesh().positions.clone();
                let plane=a.surface_screen().plane(c+s,c+e).unwrap();
                planes.push(plane);
                a.cursor_moved(c+s,false);a.left_press(false,false);a.cursor_moved(c+e,false);a.left_release();
                let after=&a.mesh().positions;
                let mut worst_kept=0.0f32;let mut worst_cut=0.0f32;let mut moved=0;
                for (p,q) in before.iter().zip(after) {
                    let d=plane.truncate().dot(*p)+plane.w;
                    if d<=0.0 {worst_kept=worst_kept.max(p.distance(*q));} else {moved+=1;worst_cut=worst_cut.max((plane.truncate().dot(*q)+plane.w).abs());}
                }
                assert!(moved>0 && worst_kept==0.0 && worst_cut<1e-5,"levels {levels} clip {k}: kept side moved {worst_kept}, cut side off the plane by {worst_cut}");
                // No cut pushes an earlier one back out (the model turned shows it).
                for (j,q) in planes.iter().enumerate() {
                    let out=after.iter().map(|p|q.truncate().dot(*p)+q.w).fold(f32::MIN,f32::max);
                    assert!(out<1e-4,"levels {levels}: clip {k} pushed clip {j} back out by {out}");
                }
                // Step down and up again: the levels must not drift.
                if levels>0 {
                    let top=a.doc.active();let at_top=a.mesh().positions.clone();
                    a.set_level(0);a.set_level(top);
                    let drift=a.mesh().positions.iter().zip(&at_top).map(|(p,q)|p.distance(*q)).fold(0.0f32,f32::max);
                    assert_eq!(drift,0.0,"levels {levels} clip {k}: level round trip drift");
                }
            }
        }
    }
    #[test]
    fn clip_can_start_off_mesh_cancel_and_undo() {
        let mut a=app();let c=a.project(Vec3::ZERO);let original=a.mesh().positions.clone();
        a.workspace.select_brush(&mut a.brush,BrushKind::ClipLine);
        let start=c+Vec2::new(80.0,-400.0);let end=c+Vec2::new(80.0,400.0);
        a.cursor_moved(start,false);a.left_press(false,false);a.cursor_moved(end,false);
        assert!(a.gesture.is_some()&&a.nav.is_none());assert!(a.workspace.gesture_preview.is_some());
        a.on_key(KeyCode::Escape);a.left_release();assert_eq!(a.mesh().positions,original);assert!(!a.history.can_undo());
        a.cursor_moved(start,false);a.left_press(false,false);a.cursor_moved(end,false);a.left_release();
        assert_ne!(a.mesh().positions,original);a.history.undo(&mut a.doc);assert_eq!(a.mesh().positions,original);
    }
}

#[cfg(test)]
mod curve_input_tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new(40, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app.workspace.assets = sculpt::StampSettings::default();
        app
    }

    /// Press, a shaky drag along +X, and optionally the release.
    fn drag(app: &mut App, from: Vec2, len: f32, release: bool) {
        app.cursor_moved(from, false);
        app.left_press(false, false);
        for k in 1..=40 {
            let x = len * k as f32 / 40.0;
            app.cursor_moved(from + Vec2::new(x, (x * 0.9).sin() * 3.0), false);
        }
        if release {
            app.left_release();
        }
    }

    #[test]
    fn zipper_strokes_along_a_curve_by_default_and_bends() {
        let mut a = app();
        a.workspace.select_brush(&mut a.brush, BrushKind::Zipper);
        a.brush.radius = 0.2;
        assert!(a.curve_mode(BrushKind::Zipper) && !a.curve_mode(BrushKind::Draw));
        let original = a.mesh().positions.clone();
        let c = a.project(Vec3::ZERO) - Vec2::new(150.0, 0.0);
        drag(&mut a, c, 300.0, false);
        assert_eq!(a.mesh().positions, original, "nothing until release");
        assert!(a.curve_preview().is_some_and(|p| p.drawing));
        a.left_release();
        let applied = a.mesh().positions.clone();
        assert_ne!(applied, original);
        assert!(a.history.can_undo() && a.live_curve().is_some());
        assert!(a.curve_preview().is_some_and(|p| !p.drawing && !p.pieces.is_empty()));
        // Grab the middle of the curve and pull it.
        let mid = { let c = &a.curve.as_ref().unwrap().curve; c.points[c.points.len() / 2] };
        let at = a.project(mid);
        a.cursor_moved(at, false);
        a.left_press(false, false);
        assert!(a.curve.as_ref().unwrap().bend.is_some());
        a.cursor_moved(at + Vec2::new(0.0, -60.0), false);
        a.left_release();
        let bent = a.mesh().positions.clone();
        assert_ne!(bent, applied);
        assert!(a.live_curve().is_some(), "still bendable");
        // Undo the bend, then the curve.
        a.history.undo(&mut a.doc);
        let d = a.mesh().positions.iter().zip(&applied).map(|(p, q)| p.distance(*q)).fold(0.0f32, f32::max);
        assert!(d < 1e-6, "{d}");
        assert!(a.live_curve().is_none(), "an undo lets the curve go");
        a.history.undo(&mut a.doc);
        assert_eq!(a.mesh().positions, original);
    }

    #[test]
    fn another_edit_or_esc_lets_the_curve_go_and_freehand_stays_the_default() {
        let mut a = app();
        a.workspace.select_brush(&mut a.brush, BrushKind::Zipper);
        let c = a.project(Vec3::ZERO) - Vec2::new(100.0, 0.0);
        drag(&mut a, c, 200.0, true);
        assert!(a.live_curve().is_some());
        // A freehand Draw stroke elsewhere: the curve can no longer be bent.
        a.workspace.select_brush(&mut a.brush, BrushKind::Draw);
        drag(&mut a, c + Vec2::new(0.0, 120.0), 100.0, true);
        assert!(a.live_curve().is_none());
        // Esc lets a curve go and keeps its result.
        a.workspace.select_brush(&mut a.brush, BrushKind::Zipper);
        drag(&mut a, c - Vec2::new(0.0, 120.0), 200.0, true);
        let result = a.mesh().positions.clone();
        assert!(a.curve.is_some());
        a.on_key(KeyCode::Escape);
        assert!(a.curve.is_none() && a.mesh().positions == result);
        // Esc while drawing drops the stroke.
        drag(&mut a, c, 200.0, false);
        a.on_key(KeyCode::Escape);
        a.left_release();
        assert_eq!(a.mesh().positions, result);
        // Any dab brush can stroke along a curve; Freehand turns it off again.
        let undo_steps = |a: &App| a.history.edits();
        a.workspace.set_curve_mode(BrushKind::DamStandard, true);
        a.workspace.select_brush(&mut a.brush, BrushKind::DamStandard);
        let before = (a.mesh().positions.clone(), undo_steps(&a));
        drag(&mut a, c + Vec2::new(0.0, 60.0), 200.0, false);
        assert_eq!(a.mesh().positions, before.0);
        a.left_release();
        assert_ne!(a.mesh().positions, before.0);
        assert_eq!(undo_steps(&a), before.1 + 1, "one undo step");
        a.workspace.set_curve_mode(BrushKind::DamStandard, false);
        assert!(!a.curve_mode(BrushKind::DamStandard));
        // Drag brushes never stroke along a curve.
        a.workspace.set_curve_mode(BrushKind::Grab, true);
        assert!(!a.curve_mode(BrushKind::Grab));
    }

    fn tube_app() -> App {
        let mut a = app();
        a.workspace.select_brush(&mut a.brush, BrushKind::CurveTube);
        a.brush.radius = 0.05;
        a
    }

    #[test]
    fn curve_tube_adds_a_polygroup_that_bends_and_undoes() {
        let mut a = tube_app();
        a.workspace.set_curve_mode(BrushKind::CurveTube, false);
        assert!(a.curve_mode(BrushKind::CurveTube), "CurveTube always draws a curve");
        let (positions, groups, verts) = (a.mesh().positions.clone(), a.mesh().group_count(), a.mesh().vertex_count());
        let c = a.project(Vec3::ZERO) - Vec2::new(150.0, 0.0);
        drag(&mut a, c, 300.0, false);
        assert_eq!(a.mesh().vertex_count(), verts, "nothing until release");
        a.left_release();
        let added = a.mesh().vertex_count();
        assert!(added > verts + 100, "{added}");
        assert_eq!(a.mesh().positions[..verts], positions[..], "the model itself is untouched");
        assert_eq!(a.mesh().group_count(), groups + 1);
        // The tube runs along the front of the unit sphere, its centre on the surface.
        let tube = a.mesh().positions[verts..].to_vec();
        assert!(tube.iter().all(|p| (p.length() - 1.0).abs() < 0.06 && p.z > 0.0), "off the surface");
        assert!(a.live_curve().is_some());
        // Bend it: the model stays, the tube moves.
        let mid = { let c = &a.curve.as_ref().unwrap().curve; c.points[c.points.len() / 2] };
        let at = a.project(mid);
        a.cursor_moved(at, false);
        a.left_press(false, false);
        assert!(a.curve.as_ref().unwrap().bend.is_some());
        a.cursor_moved(at + Vec2::new(0.0, -80.0), false);
        a.left_release();
        assert!(a.live_curve().is_some(), "still bendable");
        let bent = a.mesh().positions.clone();
        assert_eq!(bent[..verts], positions[..]);
        assert_ne!(bent[verts..], tube[..]);
        let lift = |p: &[Vec3]| p.iter().map(|q| q.y).fold(f32::MIN, f32::max);
        assert!(lift(&bent[verts..]) > lift(&tube) + 0.05, "pulled up the screen");
        // Undo the bend, then the tube; redo both.
        a.history.undo(&mut a.doc);
        assert_eq!(a.mesh().positions[verts..], tube[..]);
        a.history.undo(&mut a.doc);
        assert_eq!(a.mesh().positions, positions);
        assert_eq!(a.mesh().group_count(), groups);
        a.history.redo(&mut a.doc);
        a.history.redo(&mut a.doc);
        assert_eq!(a.mesh().positions, bent);
        assert_eq!(a.mesh().group_count(), groups + 1);
        // Saved in a project and opened again, the model and its tube come back exactly.
        let back = a.doc.pack_copy().try_unpack().unwrap();
        assert_eq!((&back.mesh().positions, &back.mesh().indices, &back.mesh().groups), (&a.mesh().positions, &a.mesh().indices, &a.mesh().groups));
        // A tube can be divided like the rest of the model (closed, two triangles per edge).
        assert!(!a.mesh().edges_over_two_triangles());
        a.divide();
        assert_eq!(a.doc.count(), 2);
    }

    #[test]
    fn curve_tube_follows_draw_size_hangs_off_the_model_and_mirrors() {
        let mut a = tube_app();
        let verts = a.mesh().vertex_count();
        let centre = a.project(Vec3::ZERO);
        drag(&mut a, centre - Vec2::new(100.0, 0.0), 200.0, true);
        let width = |a: &App| {
            let t = &a.mesh().positions[verts..];
            t.iter().map(|p| p.y).fold(f32::MIN, f32::max) - t.iter().map(|p| p.y).fold(f32::MAX, f32::min)
        };
        let (thin, edits) = (width(&a), a.history.edits());
        a.brush.radius = 0.1;
        a.follow_tube_settings();
        assert!(width(&a) > thin * 1.6, "{} vs {thin}", width(&a));
        assert_eq!(a.history.edits(), edits, "resizing is part of the same tube");
        assert!(a.live_curve().is_some());
        a.workspace.tube_sides = 5;
        a.follow_tube_settings();
        assert!(a.mesh().vertex_count() < verts + 400);
        a.on_key(KeyCode::Escape);
        // A stroke that runs off the side of the model goes on at the depth it left.
        let start = a.mesh().vertex_count();
        a.brush.radius = 0.05;
        drag(&mut a, centre + Vec2::new(0.0, 150.0), 700.0, true);
        let far = a.mesh().positions[start..].iter().map(|p| p.x).fold(f32::MIN, f32::max);
        assert!(far > 1.3, "the tube stops at the silhouette: {far}");
        // With symmetry a second tube on the other side, in the same polygroup.
        a.on_key(KeyCode::Escape);
        a.workspace.symmetry = true;
        let (start, groups) = (a.mesh().vertex_count(), a.mesh().group_count());
        drag(&mut a, centre + Vec2::new(60.0, -150.0), 120.0, true);
        let part = &a.mesh().positions[start..];
        assert!(part.iter().any(|p| p.x > 0.1) && part.iter().any(|p| p.x < -0.1));
        assert_eq!(a.mesh().group_count(), groups + 1);
    }

    #[test]
    fn curve_tube_needs_a_subtool_without_levels() {
        let mut a = tube_app();
        a.divide();
        let (verts, edits) = (a.mesh().vertex_count(), a.history.edits());
        let c = a.project(Vec3::ZERO) - Vec2::new(100.0, 0.0);
        drag(&mut a, c, 200.0, true);
        assert_eq!((a.mesh().vertex_count(), a.history.edits()), (verts, edits));
        assert!(a.curve.is_none() && a.curve_draw.is_none());
        assert!(a.status.contains("subdivision levels"));
    }
}

#[cfg(test)]
mod stroke_modifier_tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new(60, false, usize::MAX);
        app.workspace.assets = sculpt::StampSettings::default();
        app.camera.aspect = 1.6;
        app.canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 1000.0));
        app.workspace.select_brush(&mut app.brush, BrushKind::Draw);
        app.brush.radius = 0.06;
        app
    }

    /// Press at `from`, move through `path`, release; returns the stroke's dab count.
    fn stroke(app: &mut App, from: Vec2, path: &[Vec2]) -> u32 {
        app.cursor_moved(from, false);
        app.left_press(false, false);
        for &p in path {
            app.cursor_moved(p, false);
        }
        let dabs = app.stroke.as_ref().map_or(0, |s| s.dabs);
        app.left_release();
        dabs
    }

    /// How far each of `n` points along the screen line from `a` to `b` was raised.
    fn raised_along(app: &App, before: &[Vec3], a: Vec2, b: Vec2, n: usize) -> Vec<f32> {
        (0..n)
            .map(|k| {
                let p = a.lerp(b, (k as f32 + 0.5) / n as f32);
                let (o, d) = app.ray_at(p);
                let hit = app.doc.mesh().raycast(o, d).unwrap();
                let v = (0..before.len()).min_by(|&x, &y| before[x].distance(hit.point).total_cmp(&before[y].distance(hit.point))).unwrap();
                (app.doc.mesh().positions[v] - before[v]).length()
            })
            .collect()
    }

    #[test]
    fn a_fast_stroke_leaves_no_gaps_and_spacing_sets_the_dab_count() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        let (a, b) = (c - Vec2::new(150.0, 0.0), c + Vec2::new(150.0, 0.0));
        let before = app.doc.mesh().positions.clone();
        // One big jump: before, a single dab at each end.
        let dabs = stroke(&mut app, a, &[b]);
        assert!(dabs > 10, "{dabs} dabs along the jump");
        let raised = raised_along(&app, &before, a, b, 12);
        assert!(raised.iter().all(|&h| h > 1e-4), "a continuous ridge: {raised:?}");
        // Wider spacing, fewer dabs over the same path.
        app.history.undo(&mut app.doc);
        app.workspace.spacing = 0.6;
        let sparse = stroke(&mut app, a, &[b]);
        assert!(sparse * 3 < dabs, "{sparse} vs {dabs}");
    }

    #[test]
    fn the_morph_brush_brings_back_the_stored_shape_and_switch_swaps_it() {
        let mut app = app();
        app.brush.radius = 0.3;
        let c = app.project(Vec3::ZERO);
        let original = app.doc.mesh().positions.clone();
        // Without a target the Morph brush moves nothing.
        app.workspace.select_brush(&mut app.brush, BrushKind::Morph);
        stroke(&mut app, c, &[c + Vec2::new(20.0, 0.0)]);
        assert_eq!(app.doc.mesh().positions, original);
        // Store, sculpt, then brush back.
        app.store_morph_target();
        app.workspace.select_brush(&mut app.brush, BrushKind::Inflate);
        app.brush.strength = 1.0;
        for _ in 0..3 {
            stroke(&mut app, c, &[c + Vec2::new(30.0, 0.0), c - Vec2::new(30.0, 0.0)]);
        }
        let off = |app: &App| app.doc.mesh().positions.iter().zip(&original).map(|(a, b)| a.distance(*b)).sum::<f32>();
        let sculpted = off(&app);
        assert!(sculpted > 0.1);
        let shaped = app.doc.mesh().positions.clone();
        app.workspace.select_brush(&mut app.brush, BrushKind::Morph);
        app.brush.strength = 1.0;
        for _ in 0..4 {
            stroke(&mut app, c, &[c + Vec2::new(30.0, 0.0), c - Vec2::new(30.0, 0.0)]);
        }
        assert!(off(&app) < sculpted * 0.3, "{} of {sculpted} left", off(&app));
        // Switch: the stored shape comes back exactly, the sculpt becomes the target; undo.
        app.history.undo(&mut app.doc);
        let before_switch = app.doc.mesh().positions.clone();
        app.switch_morph_target();
        assert_eq!(app.doc.mesh().positions, original);
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().positions, before_switch);
        assert_ne!(shaped, original);
    }

    #[test]
    fn drag_rect_sizes_one_stamp_and_drag_dot_slides_it() {
        let mut app = app();
        app.brush.strength = 1.0;
        app.workspace.set_stroke_mode(BrushKind::Draw, "DragRect");
        assert_eq!(app.workspace.stroke_mode(BrushKind::Draw), "DragRect");
        let c = app.project(Vec3::ZERO);
        let original = app.doc.mesh().positions.clone();
        let raised_at = |app: &App, p: Vec2| {
            let (o, d) = app.ray_at(p);
            let hit = app.doc.mesh().raycast(o, d).unwrap().point;
            let v = (0..original.len()).min_by(|&a, &b| original[a].distance(hit).total_cmp(&original[b].distance(hit))).unwrap();
            app.doc.mesh().positions[v].distance(original[v])
        };
        // Press, drag wide: the stamp reaches 60 px out; drag back in: only the middle stays.
        app.cursor_moved(c, false);
        app.left_press(false, false);
        assert_eq!(app.doc.mesh().positions, original, "no stamp before the drag");
        app.cursor_moved(c + Vec2::new(120.0, 0.0), false);
        assert!(raised_at(&app, c + Vec2::new(0.0, 60.0)) > 1e-3);
        app.cursor_moved(c + Vec2::new(30.0, 0.0), false);
        assert!(raised_at(&app, c + Vec2::new(0.0, 60.0)) < 1e-6, "made again smaller, from the surface before");
        assert!(raised_at(&app, c) > 1e-4);
        app.left_release();
        app.history.undo(&mut app.doc);
        assert_eq!(app.doc.mesh().positions, original, "one undo step");
        // DragDot: the stamp slides with the pointer and sets where it is released.
        app.workspace.set_stroke_mode(BrushKind::Draw, "DragDot");
        app.brush.radius = 0.15;
        app.cursor_moved(c, false);
        app.left_press(false, false);
        assert!(raised_at(&app, c) > 1e-4);
        app.cursor_moved(c + Vec2::new(150.0, 40.0), false);
        app.left_release();
        assert!(raised_at(&app, c) < 1e-6 && raised_at(&app, c + Vec2::new(150.0, 40.0)) > 1e-4);
        // Shelf and Stroke palette offer every mode a dab brush has.
        assert_eq!(workspace::Workspace::stroke_modes(BrushKind::Draw), vec!["Freehand", "Curve", "DragRect", "DragDot"]);
        assert_eq!(workspace::Workspace::stroke_modes(BrushKind::Grab), vec!["Freehand"]);
    }

    #[test]
    fn edit_history_names_steps_and_jumps_back_and_forth() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        let mut states = vec![app.doc.mesh().positions.clone()];
        for kind in [BrushKind::Draw, BrushKind::ClayBuildup, BrushKind::Inflate] {
            app.workspace.select_brush(&mut app.brush, kind);
            stroke(&mut app, c, &[c + Vec2::new(40.0, 10.0)]);
            states.push(app.doc.mesh().positions.clone());
        }
        app.deform(Deform::Smooth, 30.0);
        states.push(app.doc.mesh().positions.clone());
        let (done, ahead) = app.history.names();
        assert_eq!(done, ["Draw", "Clay Buildup", "Inflate", "Smooth"]);
        assert!(ahead.is_empty());
        // Back to just after the first stroke, then forward two.
        app.history_jump(-3);
        assert_eq!(app.doc.mesh().positions, states[1]);
        let (done, ahead) = app.history.names();
        assert_eq!((done.len(), ahead.iter().map(|s| s.as_str()).collect::<Vec<_>>()), (1, vec!["Clay Buildup", "Inflate", "Smooth"]));
        app.history_jump(2);
        assert!(app.doc.mesh().positions.iter().zip(&states[3]).all(|(a, b)| a.distance(*b) < 1e-6));
        // Past either end it stops there.
        app.history_jump(-10);
        assert_eq!(app.doc.mesh().positions, states[0]);
        // A new step drops the redo names.
        stroke(&mut app, c, &[c + Vec2::new(30.0, 0.0)]);
        assert_eq!(app.history.names().0.len(), 1);
        assert!(app.history.names().1.is_empty());
    }

    #[test]
    fn gizmo_arrows_move_rings_turn_and_presses_elsewhere_do_nothing() {
        let mut app = app();
        app.on_key(KeyCode::KeyY);
        assert!(app.workspace.gizmo && app.workspace.mode == Mode::Move);
        let before = app.doc.mesh().positions.clone();
        let same = |app: &App| app.doc.mesh().positions.iter().zip(&before).all(|(p, q)| p.distance(*q) < 1e-5);
        let drag = |app: &mut App, from: Vec2, to: Vec2| {
            app.cursor_moved(from, false);
            app.left_press(false, false);
            for k in 1..=6 {
                app.cursor_moved(from + (to - from) * (k as f32 / 6.0), false);
            }
            app.left_release();
        };
        let origin = app.canvas_origin();
        let view = app.view();
        let centre = app.gizmo_centre();
        let g = app.gizmo_place();
        let tip = gizmo::arrow_tip(&view, &g, 0) + origin;
        let c = view.project(centre) + origin;
        // X arrow: the whole subtool moves along X, nothing else.
        drag(&mut app, c + (tip - c) * 0.8, c + (tip - c) * 1.3);
        let d = app.doc.mesh().positions[0] - before[0];
        assert!(d.x > 0.05 && d.y.abs() < 1e-4 && d.z.abs() < 1e-4, "{d:?}");
        assert!(app.doc.mesh().positions.iter().zip(&before).all(|(p, q)| (*p - *q - d).length() < 1e-4));
        app.history_jump(-1);
        assert!(same(&app));
        // Y ring: turns about Y through the centre.
        let view = app.view();
        let ring = gizmo::ring(&view, &g, Some(1), gizmo::RING);
        let at = ring.iter().copied().find(|&p| gizmo::hit(&view, &g, p) == Some(gizmo::Handle::Rotate(1))).unwrap() + origin;
        drag(&mut app, at, at + Vec2::new(40.0, 0.0));
        let ys: Vec<f32> = app.doc.mesh().positions.iter().map(|p| p.y).collect();
        assert!(ys.iter().zip(&before).all(|(y, q)| (y - q.y).abs() < 1e-4), "Y stays");
        assert_ne!(app.doc.mesh().positions, before);
        app.history_jump(-1);
        // A press away from every handle leaves the model alone.
        let undone = app.doc.mesh().positions.clone();
        drag(&mut app, c + Vec2::new(400.0, 300.0), c + Vec2::new(450.0, 320.0));
        assert_eq!(app.doc.mesh().positions, undone);
        assert!(same(&app));
        // The Y box dragged in squashes along Y only: a flattened sphere. (The press away
        // from the handles turned the view.)
        let view = app.view();
        let g = app.gizmo_place();
        let c = view.project(centre) + origin;
        let tip = gizmo::arrow_tip(&view, &g, 0) + origin;
        let b = gizmo::axis_box(&view, &g, 1) + origin;
        let size0 = app.doc.bounds().max - app.doc.bounds().min;
        drag(&mut app, b, b + (c - b).normalize() * 100.0);
        let size1 = app.doc.bounds().max - app.doc.bounds().min;
        assert!((size1.y / size0.y - 0.5).abs() < 0.02 && (size1.x / size0.x - 1.0).abs() < 1e-3, "{size1} vs {size0}");
        app.history_jump(-1);
        assert!(same(&app));
        // Alt + drag the X arrow: the gizmo moves, the model stays.
        app.modifiers = ModifiersState::ALT;
        drag(&mut app, c + (tip - c) * 0.8, c + (tip - c) * 1.3);
        assert!(same(&app));
        assert!(app.gizmo_centre().x > centre.x + 0.05, "{:?}", app.gizmo_centre());
        // Alt + click on the model: the gizmo goes there, its Y along the surface normal.
        let on = app.project(Vec3::new(0.0, 0.0, 1.0)) ;
        app.cursor_moved(on, false);
        app.left_press(false, false);
        app.left_release();
        app.modifiers = ModifiersState::empty();
        let place = app.gizmo_place();
        assert!(place.centre.z > 0.9 && place.axis(1).z > 0.95, "{place:?}");
        assert!(same(&app));
        app.gizmo_pivot(false);
        assert_eq!(app.gizmo_centre(), centre);
    }

    #[test]
    fn layers_record_scale_hide_delete_bake_and_undo() {
        let mut app = app();
        assert!(app.doc.divide());
        app.show_active_level();
        let c = app.project(Vec3::ZERO);
        let sculpt = |app: &mut App, kind: BrushKind| {
            app.workspace.select_brush(&mut app.brush, kind);
            app.brush.radius = 0.3;
            app.brush.strength = 1.0;
            stroke(app, c, &[c + Vec2::new(40.0, 0.0), c - Vec2::new(40.0, 0.0)]);
        };
        let start = app.doc.mesh().positions.clone();
        let near = |a: &[Vec3], b: &[Vec3]| a.iter().zip(b).all(|(p, q)| p.distance(*q) < 1e-4);
        // A new layer records what is sculpted.
        app.layer_op(LayerOp::New);
        assert!(app.doc.layers.rec.is_some());
        sculpt(&mut app, BrushKind::Inflate);
        let with_layer = app.doc.mesh().positions.clone();
        assert!(!near(&with_layer, &start));
        // Recording keeps the level; Divide waits for a bake.
        app.set_level(0);
        assert_eq!(app.doc.active(), 1);
        app.divide();
        assert_eq!(app.doc.count(), 2);
        // Intensity 0 takes it out, 0.5 halfway, then back to 1 as one undo step.
        app.layer_op(LayerOp::Intensity(0, 0.0));
        assert!(near(&app.doc.mesh().positions, &start));
        app.layer_op(LayerOp::Intensity(0, 1.0));
        app.layer_op(LayerOp::IntensityDone(0, 1.0));
        assert!(near(&app.doc.mesh().positions, &with_layer));
        // Stop recording; a stroke now is not in the layer.
        app.layer_op(LayerOp::Rec(0));
        assert!(app.doc.layers.rec.is_none());
        sculpt(&mut app, BrushKind::Draw);
        let both = app.doc.mesh().positions.clone();
        // Hide: only the layer's part goes.
        app.layer_op(LayerOp::Eye(0));
        let hidden = app.doc.mesh().positions.clone();
        assert!(!near(&hidden, &both) && !near(&hidden, &start));
        app.layer_op(LayerOp::Eye(0));
        assert!(near(&app.doc.mesh().positions, &both));
        // Delete takes it out; undo brings it back.
        app.layer_op(LayerOp::Delete(0));
        assert!(near(&app.doc.mesh().positions, &hidden) && app.doc.layers.list.is_empty());
        app.history_jump(-1);
        assert!(near(&app.doc.mesh().positions, &both) && app.doc.layers.list.len() == 1);
        // Bake keeps the shape and removes the layer; undo, then hiding still works.
        app.layer_op(LayerOp::Bake(0));
        assert!(near(&app.doc.mesh().positions, &both) && app.doc.layers.list.is_empty());
        app.history_jump(-1);
        app.layer_op(LayerOp::Eye(0));
        assert!(near(&app.doc.mesh().positions, &hidden));
        app.layer_op(LayerOp::Eye(0));
        // Parking the subtool keeps its layers.
        app.append_primitive();
        app.select_subtool(0);
        assert_eq!(app.doc.layers.list.len(), 1);
        assert!(near(&app.doc.mesh().positions, &both));
        // Edit › History names the steps.
        assert!(app.history.names().0.iter().any(|n| n == "New layer"));
    }

    #[test]
    fn projects_keep_layers_and_morph_targets() {
        let mut app = app();
        let c = app.project(Vec3::ZERO);
        app.store_morph_target();
        app.layer_op(LayerOp::New);
        app.workspace.select_brush(&mut app.brush, BrushKind::Inflate);
        app.brush.radius = 0.3;
        app.brush.strength = 1.0;
        stroke(&mut app, c, &[c + Vec2::new(40.0, 0.0)]);
        app.layer_op(LayerOp::Intensity(0, 0.7));
        let shaped = app.doc.mesh().positions.clone();
        // Save while still recording: what was sculpted is folded in first.
        let bytes = { app.doc.freeze_layers(); app.project_bytes() };
        let dir = std::env::temp_dir().join(format!("klay-v5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v5.klay");
        std::fs::write(&path, &bytes).unwrap();
        let mut other = App::new(10, false, usize::MAX);
        other.open_project(path.to_str().unwrap());
        assert_eq!(other.doc.mesh().positions, shaped);
        assert_eq!(other.doc.layers.list.len(), 1);
        assert!(other.doc.layers.rec.is_none(), "recording is not saved");
        assert!((other.doc.layers.list[0].intensity - 0.7).abs() < 1e-6);
        assert!(other.morph_target().is_some());
        // The loaded layer still works: hiding it takes its part out.
        other.layer_op(LayerOp::Eye(0));
        assert_ne!(other.doc.mesh().positions, shaped);
        // Version 6: boolean operations, the active subtool's and a parked one's.
        let (p, i) = cube_sphere(6, 0.3);
        app.insert_after_active(vec![("Cutter".into(), SculptMesh::from_raw(p, i))]);
        app.subtools[0].boolean = boolean::BoolOp::Intersect;
        app.subtools[1].boolean = boolean::BoolOp::Subtract;
        std::fs::write(&path, app.project_bytes()).unwrap();
        let mut third = App::new(10, false, usize::MAX);
        third.open_project(path.to_str().unwrap());
        let ops: Vec<boolean::BoolOp> = third.subtools.iter().map(|t| t.boolean).collect();
        assert_eq!(ops, [boolean::BoolOp::Intersect, boolean::BoolOp::Subtract]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lazy_mouse_trails_the_cursor_by_its_radius() {
        let mut app = app();
        app.workspace.lazy_mouse = true;
        app.workspace.lazy_radius = 60.0;
        let c = app.project(Vec3::ZERO);
        // Inside the string the brush stays put: only the press dabs.
        app.cursor_moved(c, false);
        app.left_press(false, false);
        app.cursor_moved(c + Vec2::new(40.0, 20.0), false);
        assert_eq!(app.stroke.as_ref().unwrap().dabs, 1);
        assert!(app.workspace.lazy_line.is_some(), "the string is drawn");
        // Pulled 200 px, it follows to 60 px behind the cursor.
        app.cursor_moved(c + Vec2::new(200.0, 0.0), false);
        let lazy = app.lazy_point.unwrap();
        assert!((app.cursor.distance(lazy) - 60.0).abs() < 1e-3);
        let brush = app.project(app.hover.unwrap());
        assert!(brush.distance(lazy) < 2.0, "the brush is at the lazy point: {brush:?} {lazy:?}");
        assert!(app.stroke.as_ref().unwrap().dabs > 5);
        app.left_release();
        assert!(app.workspace.lazy_line.is_none() && app.lazy_point.is_none());
        // L toggles it (ZBrush).
        app.on_key(KeyCode::KeyL);
        assert!(!app.workspace.lazy_mouse);
    }
}
