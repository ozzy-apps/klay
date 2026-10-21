//! ZBrush-style workspace shell.
//!
//! Every palette lives in the menu bar and opens there as a dropdown; any palette
//! can also be docked into one of four trays (left, right, top, bottom), where
//! it collapses, scrolls by pen drag, and can be dragged between trays by its ◉
//! button. Fixed shelves hold the live sculpt controls: the top shelf (mode,
//! add/subtract, intensity, size), the left shelf (brush / material thumbnails
//! and the colour picker) and the right shelf (navigation toggles).
//!
//! Palette placement is data. Adding a palette means one `PaletteId` variant, its
//! defaults, and a `palette_body` arm built from `sec` sections and the widget
//! helpers; the shell supplies docking, collapsing, scrolling and persistence.

use crate::sculpt::{Brush, BrushKind};
use crate::subtool::Deform;
use egui::ecolor::Hsva;
use egui::{
    Align, Align2, Color32, ColorImage, Context, CornerRadius, FontId, Frame, Id, Layout, Margin,
    Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextureHandle, TextureId,
    TextureOptions, Ui, UiBuilder, Vec2, pos2, vec2,
};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

// ---------------------------------------------------------------- theme tokens
pub const BACKGROUND: Color32 = Color32::from_rgb(14, 15, 16); // canvas
/// Inset of the document frame drawn inside the canvas, in points. Like ZBrush, the margin
/// between the frame and the canvas edge always navigates, even over the model.
pub const FRAME_INSET: f32 = 16.0;
const BG0: Color32 = Color32::from_rgb(18, 19, 20);
const BG1: Color32 = Color32::from_rgb(28, 30, 32); // trays, palette bodies
const BG2: Color32 = Color32::from_rgb(36, 39, 41); // menubar, headers, fields
const BG3: Color32 = Color32::from_rgb(45, 49, 52); // buttons
const BG4: Color32 = Color32::from_rgb(56, 61, 65); // hover
const LINE: Color32 = Color32::from_rgb(11, 12, 13);
const LINE2: Color32 = Color32::from_rgb(42, 46, 49);
const HI: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);
const FG: Color32 = Color32::from_rgb(216, 218, 221);
const FG2: Color32 = Color32::from_rgb(155, 160, 166);
const FG3: Color32 = Color32::from_rgb(103, 109, 116);
const ACC: Color32 = Color32::from_rgb(227, 131, 79);
const ACC_FG: Color32 = Color32::from_rgb(27, 18, 12);
const ACC_DIM: Color32 = Color32::from_rgba_premultiplied(54, 31, 19, 61);
const ACC_LINE: Color32 = Color32::from_rgba_premultiplied(159, 92, 55, 179);

const MENUBAR_H: f32 = 26.0;
const SHELF_H: f32 = 58.0;
const STATUS_H: f32 = 20.0;
const LEFT_SHELF_W: f32 = 100.0;
const RIGHT_SHELF_W: f32 = 54.0;
const DIVIDER_W: f32 = 7.0;
const HEADER_H: f32 = 26.0;
const SECTION_H: f32 = 22.0;
const SECTION_W: f32 = 230.0; // section width in horizontal trays
const DROPDOWN_W: f32 = 330.0;
const NC: &str = "Not connected";

// ---------------------------------------------------------------- layout model
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Left,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Self; 4] = [Self::Top, Self::Left, Self::Right, Self::Bottom];
    pub fn name(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Bottom => "Bottom",
        }
    }
    fn horizontal(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == s)
    }
}

/// Where a palette lives: only in the menu bar, or docked in a tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Menu,
    Tray(Side),
}

impl Place {
    fn name(self) -> &'static str {
        match self {
            Self::Menu => "Menu",
            Self::Tray(side) => side.name(),
        }
    }
    fn parse(s: &str) -> Option<Self> {
        if s == "Menu" {
            Some(Self::Menu)
        } else {
            Side::parse(s).map(Self::Tray)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaletteId {
    Alpha,
    Brush,
    Document,
    Edit,
    Masking,
    Material,
    Mesh,
    Performance,
    Preferences,
    Stroke,
    Symmetry,
    Transform,
    View,
    Workspace,
}

impl PaletteId {
    /// Alphabetical, which is also the menu bar order.
    pub const ALL: [Self; 14] = [
        Self::Alpha,
        Self::Brush,
        Self::Document,
        Self::Edit,
        Self::Masking,
        Self::Material,
        Self::Mesh,
        Self::Performance,
        Self::Preferences,
        Self::Stroke,
        Self::Symmetry,
        Self::Transform,
        Self::View,
        Self::Workspace,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Alpha => "Alpha",
            Self::Brush => "Brush",
            Self::Document => "Document",
            Self::Edit => "Edit",
            Self::Masking => "Masking",
            Self::Material => "Material",
            Self::Mesh => "Mesh",
            Self::Performance => "Performance",
            Self::Preferences => "Preferences",
            Self::Stroke => "Stroke",
            Self::Symmetry => "Symmetry",
            Self::Transform => "Transform",
            Self::View => "View",
            Self::Workspace => "Workspace",
        }
    }
    fn defaults(self) -> (Place, bool) {
        match self {
            Self::Brush | Self::Symmetry => (Place::Tray(Side::Left), true),
            Self::Edit | Self::Material | Self::Mesh => (Place::Tray(Side::Right), true),
            Self::Performance => (Place::Tray(Side::Bottom), false),
            _ => (Place::Menu, true),
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteState {
    pub id: PaletteId,
    pub place: Place,
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DockLayout {
    /// Order within a tray is the order in this list.
    pub palettes: Vec<PaletteState>,
    /// Sections whose state differs from their default, keyed "palette/section".
    pub sections: BTreeMap<String, bool>,
    pub left_tray: bool,
    pub right_tray: bool,
    pub tray_width: f32,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            palettes: PaletteId::ALL
                .into_iter()
                .map(|id| {
                    let (place, open) = id.defaults();
                    PaletteState { id, place, open }
                })
                .collect(),
            sections: BTreeMap::new(),
            left_tray: true,
            right_tray: true,
            tray_width: 262.0,
        }
    }
}

impl DockLayout {
    pub fn palette(&self, id: PaletteId) -> &PaletteState {
        self.palettes.iter().find(|p| p.id == id).unwrap()
    }
    fn palette_mut(&mut self, id: PaletteId) -> &mut PaletteState {
        self.palettes.iter_mut().find(|p| p.id == id).unwrap()
    }
    pub fn in_tray(&self, side: Side) -> Vec<PaletteState> {
        self.palettes
            .iter()
            .filter(|p| p.place == Place::Tray(side))
            .cloned()
            .collect()
    }
    pub fn tray_open(&self, side: Side) -> bool {
        match side {
            Side::Left => self.left_tray,
            Side::Right => self.right_tray,
            _ => true,
        }
    }
    fn set_tray_open(&mut self, side: Side, open: bool) {
        match side {
            Side::Left => self.left_tray = open,
            Side::Right => self.right_tray = open,
            _ => {}
        }
    }

    /// Move a palette to `place`, before `before` when that palette is already there,
    /// else to the end. Docking into a closed tray opens it.
    pub fn move_palette(&mut self, id: PaletteId, place: Place, before: Option<PaletteId>) {
        if before == Some(id) {
            return;
        }
        let Some(index) = self.palettes.iter().position(|p| p.id == id) else {
            return;
        };
        let mut palette = self.palettes.remove(index);
        palette.place = place;
        let index = before
            .and_then(|target| {
                self.palettes
                    .iter()
                    .position(|p| p.id == target && p.place == place)
            })
            .unwrap_or(self.palettes.len());
        self.palettes.insert(index, palette);
        if let Place::Tray(side) = place {
            self.set_tray_open(side, true);
        }
    }

    fn encode(&self) -> String {
        let mut text = String::from("# Klay workspace v2\n");
        text.push_str(&format!(
            "trays|{}|{}|{:.0}\n",
            self.left_tray, self.right_tray, self.tray_width
        ));
        for p in &self.palettes {
            text.push_str(&format!(
                "palette|{}|{}|{}\n",
                p.id.name(),
                p.place.name(),
                p.open
            ));
        }
        for (key, open) in &self.sections {
            text.push_str(&format!("section|{key}|{open}\n"));
        }
        text
    }

    fn decode(text: &str) -> Self {
        let defaults = Self::default();
        let mut result = Self {
            palettes: Vec::new(),
            sections: BTreeMap::new(),
            ..defaults.clone()
        };
        for line in text.lines() {
            let parts: Vec<_> = line.split('|').collect();
            match parts.as_slice() {
                ["trays", left, right, width] => {
                    if let (Ok(l), Ok(r)) = (left.parse(), right.parse()) {
                        result.left_tray = l;
                        result.right_tray = r;
                    }
                    if let Ok(w) = width.parse::<f32>() {
                        result.tray_width = w.clamp(200.0, 420.0);
                    }
                }
                ["palette", id, place, open] => {
                    let (Some(id), Some(place), Ok(open)) =
                        (PaletteId::parse(id), Place::parse(place), open.parse())
                    else {
                        continue;
                    };
                    if !result.palettes.iter().any(|p| p.id == id) {
                        result.palettes.push(PaletteState { id, place, open });
                    }
                }
                ["section", key, open] => {
                    if let Ok(open) = open.parse() {
                        result.sections.insert((*key).to_string(), open);
                    }
                }
                _ => {}
            }
        }
        // New palettes appear automatically when an older workspace is loaded.
        for palette in defaults.palettes {
            if !result.palettes.iter().any(|p| p.id == palette.id) {
                result.palettes.push(palette);
            }
        }
        result
    }
}

// ---------------------------------------------------------------- app interface
pub struct Model<'a> {
    pub brush: &'a mut Brush,
    /// Primitive and resolution used by New, Append and Rebuild.
    pub primitive: &'a mut crate::primitive::Settings,
    pub export_path: &'a mut String,
    pub status: &'a str,
    pub can_undo: bool,
    pub can_redo: bool,
    pub history_bytes: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub chunks: usize,
    /// Active subdivision level.
    pub level: usize,
    /// Vertex count of every level, lowest first.
    pub levels: &'a [usize],
    /// Vertices the next Divide would produce.
    pub next_level_vertices: usize,
    /// Divide is refused above this many vertices.
    pub vertex_budget: &'a mut usize,
    /// Memory held by all levels.
    pub levels_bytes: usize,
    /// GPU memory held by the levels that are shown or cached.
    pub gpu_level_bytes: u64,
    pub fps: f32,
    pub frame_ms: f32,
    pub stroke_ms: f32,
    pub upload_ms: f32,
    pub render_ms: f32,
    pub adapter: &'a str,
    /// World X, Y, Z axes in view space as (right, up, towards-camera) components, for
    /// the axis gizmo.
    pub axes: [[f32; 3]; 3],
    /// Every subtool, in list order.
    pub subtools: &'a [SubToolRow],
    /// RAM held by parked subtools (packed stacks and their undo lists).
    pub parked_bytes: usize,
    /// GPU memory held by the stand-ins of parked subtools.
    pub stand_in_bytes: u64,
    /// Project file used by Save and Open.
    pub project_path: &'a mut String,
    /// Polygroups and hidden triangles at the shown level of the active subtool.
    pub groups: usize,
    pub hidden: usize,
    /// The GPU can show polygroup colours.
    pub polyframe_supported: bool,
    /// Wedge settings, whether Wedge is on for the active subtool, why it cannot run (if
    /// it cannot), and about how many vertices it would make.
    pub wedge: &'a mut crate::wedge::Settings,
    pub wedge_on: bool,
    pub wedge_refusal: Option<&'a str>,
    pub wedge_estimate: usize,
    /// The pen in use or found, or why pen pressure is unavailable.
    pub tablet: &'a str,
    /// Whether pen pressure is being read.
    pub tablet_on: bool,
    /// Pressure of the pen moving the pointer; `None` for a mouse.
    pub pressure: Option<f32>,
}

/// One line of the SubTool list.
pub struct SubToolRow {
    pub name: String,
    pub visible: bool,
    pub active: bool,
    /// Vertices at the level it is shown at.
    pub vertices: usize,
    pub level: usize,
    pub top: usize,
    /// RAM it holds: the full stack when active, the packed form when parked.
    pub bytes: usize,
    /// False for a parked subtool whose stand-in did not fit on the GPU.
    pub drawn: bool,
}

#[derive(Default)]
pub struct Actions {
    pub rebuild: bool,
    pub export: bool,
    pub undo: bool,
    pub redo: bool,
    /// Add a subdivision level (ZBrush: Divide).
    pub divide: bool,
    /// Show this subdivision level (ZBrush: SDiv).
    pub level: Option<usize>,
    pub frame: bool,
    pub zoom: f32,
    pub view: Option<(f32, f32)>,
    /// SubTool list: make this one active.
    pub select_subtool: Option<usize>,
    /// SubTool list: flip the visibility of this one.
    pub toggle_subtool: Option<usize>,
    pub append: bool,
    pub duplicate: bool,
    pub delete: bool,
    /// Move the active subtool up (-1) or down (+1) in the list.
    pub move_subtool: i32,
    /// A Deformation slider was released at this value.
    pub deform: Option<(Deform, f32)>,
    /// A Masking palette button (ZBrush: Masking > Invert, Clear, Mask All, Blur, Sharpen).
    pub mask: Option<crate::mask::MaskOp>,
    pub new_project: bool,
    pub open: bool,
    pub save: bool,
    /// Polygroups: one new group of the visible polygons (Ctrl + W).
    pub group_visible: bool,
    /// Visibility: show everything / swap hidden and visible.
    pub show_all: bool,
    pub invert_visibility: bool,
    /// Wedge: turn on (remeshing) or off / remesh now.
    pub wedge_toggle: bool,
    pub wedge_now: bool,
    /// Geometry: Del Higher / Del Lower.
    pub del_higher: bool,
    pub del_lower: bool,
}

pub const MATERIALS: [(Color32, &str); 4] = [
    (Color32::from_rgb(199, 158, 128), "Warm clay"),
    (Color32::from_rgb(156, 163, 171), "Cool clay"),
    (Color32::from_rgb(182, 112, 90), "Terracotta"),
    (Color32::from_rgb(184, 174, 152), "Polished bronze"),
];

/// Top-shelf edit mode (ZBrush: Q / W / E / R). Draw sculpts; Move, Scale and Rotate drive the
/// Transpose action line (see `transform.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Draw,
    Move,
    Scale,
    Rotate,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Draw => "Draw",
            Self::Move => "Move",
            Self::Scale => "Scale",
            Self::Rotate => "Rotate",
        }
    }
}

/// Slider popup opened at the cursor by a hotkey (ZBrush: S / U / I / O).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuickPopup {
    Size,
    Intensity,
    Rgb,
    Focal,
}

impl QuickPopup {
    fn label(self) -> &'static str {
        match self {
            Self::Size => "Draw Size",
            Self::Intensity => "Z Intensity",
            Self::Rgb => "Rgb Intensity",
            Self::Focal => "Focal Shift",
        }
    }
}

pub struct Workspace {
    pub layout: DockLayout,
    pub canvas: Rect,
    pub focus: bool,
    pub subtract: bool,
    pub material: usize,
    /// Surface detail shading (the shader's reserved toggle).
    pub wireframe: bool,
    pub grid: bool,
    /// Perspective (true) or orthographic projection: `P`.
    pub perspective: bool,
    /// Mirror strokes across X: `X`.
    pub symmetry: bool,
    /// Pen pressure scales Draw Size and Z Intensity (ZBrush: Brush > Tablet Pressure).
    pub pressure_size: bool,
    pub pressure_intensity: bool,
    /// Shade the mask: Ctrl + H (ZBrush: View Mask).
    pub show_mask: bool,
    /// Draw only the active subtool (ZBrush: Solo).
    pub solo: bool,
    /// Polygroup colours on the active subtool (ZBrush: PolyF, Shift + F).
    pub polyframe: bool,
    /// Ctrl + Shift selection rectangle being dragged (logical pixels), and whether it
    /// hides (Alt, red) rather than keeps (green) what it covers.
    pub select_rect: Option<(Rect, bool)>,
    /// Deformation slider being dragged, and its value.
    deform_drag: Option<(Deform, f32)>,
    pub mode: Mode,
    /// Open hotkey popup and the logical position it was opened at.
    pub popup: Option<(QuickPopup, Pos2)>,
    /// ZBrush quick menu (Space held, or a right-click tap): where it opened and whether
    /// it closes when Space is released rather than on a click elsewhere.
    pub quick_menu: Option<(Pos2, bool)>,
    pub color: Hsva,
    pub color2: Hsva,
    dynamic: bool,
    dropdown: Option<PaletteId>,
    dropdown_anchor: Rect,
    menubar_rect: Rect,
    pending_move: Option<(PaletteId, Place, Option<PaletteId>)>,
    scroll_to: Option<PaletteId>,
    textures: HashMap<(usize, usize, bool), TextureHandle>,
    message: Option<(String, f64)>,
    /// Text queued by the app for the status bar; shown on the next frame.
    pending_notice: Option<String>,
    saved: String,
    persistence_path: PathBuf,
    pub persistence_error: Option<String>,
}

impl Workspace {
    pub fn new() -> Self {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        let persistence_path = root.join("klay/workspace.layout");
        let layout = std::fs::read_to_string(&persistence_path)
            .map(|s| DockLayout::decode(&s))
            .unwrap_or_default();
        let saved = layout.encode();
        Self {
            layout,
            canvas: Rect::NOTHING,
            focus: false,
            subtract: false,
            material: 0,
            wireframe: false,
            grid: true,
            perspective: true,
            symmetry: false,
            pressure_size: true,
            pressure_intensity: true,
            show_mask: true,
            solo: false,
            polyframe: false,
            select_rect: None,
            deform_drag: None,
            mode: Mode::Draw,
            popup: None,
            quick_menu: None,
            color: Hsva::new(0.06, 0.55, 0.88, 1.0),
            color2: Hsva::new(0.0, 0.0, 1.0, 1.0),
            dynamic: true,
            dropdown: None,
            dropdown_anchor: Rect::NOTHING,
            menubar_rect: Rect::NOTHING,
            pending_move: None,
            scroll_to: None,
            textures: HashMap::new(),
            message: None,
            pending_notice: None,
            saved,
            persistence_path,
            persistence_error: None,
        }
    }

    pub fn show(&mut self, ui: &mut Ui, m: &mut Model<'_>) -> Actions {
        let mut a = Actions::default();
        if let Some(text) = self.pending_notice.take() {
            self.flash(ui.ctx(), text);
        }
        if !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
        {
            self.set_focus(!self.focus);
        }
        if self.dropdown.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dropdown = None;
        }
        let title = {
            let file = std::path::Path::new(m.project_path.as_str())
                .file_stem()
                .map_or_else(|| "Untitled".to_string(), |n| n.to_string_lossy().to_string());
            let active = m.subtools.iter().find(|r| r.active).map_or("", |r| r.name.as_str());
            format!("{file} · {active}")
        };
        self.menubar(ui, &title);
        self.status_bar(ui, m);
        if !self.focus {
            self.shelf_top(ui, m, &mut a);
            self.tray(ui, Side::Left, m, &mut a);
            self.divider(ui, Side::Left);
            self.tray(ui, Side::Right, m, &mut a);
            self.divider(ui, Side::Right);
            self.tray(ui, Side::Top, m, &mut a);
            self.tray(ui, Side::Bottom, m, &mut a);
            self.shelf_left(ui, m, &mut a);
            self.shelf_right(ui, &mut a);
        }
        self.canvas = ui.available_rect_before_wrap();
        self.canvas_overlays(ui, m, &mut a);
        self.quick_popup(ui, m);
        self.quick_menu_ui(ui, m);
        self.dropdown(ui, m, &mut a);
        self.drop_targets(ui);
        if let Some((id, place, before)) = self.pending_move.take() {
            self.layout.move_palette(id, place, before);
            self.scroll_to = Some(id);
            self.dropdown = None;
            self.flash(
                ui.ctx(),
                match place {
                    Place::Menu => format!("{} undocked", id.name()),
                    Place::Tray(side) => format!("{} → {} tray", id.name(), side.name()),
                },
            );
        }
        let encoded = self.layout.encode();
        if encoded != self.saved {
            let result = self
                .persistence_path
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| std::fs::write(&self.persistence_path, &encoded));
            self.persistence_error = result
                .err()
                .map(|e| format!("Workspace could not be saved: {e}"));
            self.saved = encoded;
        }
        a
    }

    /// Show `text` in the status bar for a moment (from outside a frame).
    pub fn notify(&mut self, text: impl Into<String>) {
        self.pending_notice = Some(text.into());
    }

    /// Open the quick menu at `pos`. `held` = it belongs to a held Space key.
    pub fn open_quick_menu(&mut self, pos: Pos2, held: bool) {
        self.popup = None;
        self.quick_menu = Some((pos, held));
    }

    /// The brush row, Zadd/Zsub, the two live sliders and the mode row, like ZBrush's
    /// Space menu. Sliders start under the cursor so a small move adjusts them.
    fn quick_menu_ui(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        let Some((pos, held)) = self.quick_menu else { return };
        let ctx = ui.ctx().clone();
        if !held && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.quick_menu = None;
            return;
        }
        let width = 300.0;
        let area = egui::Area::new(Id::unique("quick-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos - vec2(30.0, 20.0))
            .show(&ctx, |ui| {
                popup_frame().show(ui, |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    let cols = BrushKind::ALL.len();
                    let tile = (width - 4.0 * (cols as f32 - 1.0)) / cols as f32;
                    ui.horizontal(|ui| {
                        for (i, kind) in BrushKind::ALL.iter().enumerate() {
                            let tex = self.texture(ui.ctx(), Some(*kind), self.subtract);
                            if brush_tile(ui, tex, kind.name(), i + 1, m.brush.kind == *kind, tile)
                                .clicked()
                            {
                                m.brush.kind = *kind;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.scope(|ui| {
                            ui.set_width(96.0);
                            if let Some(i) = seg(ui, &["Zadd", "Zsub"], self.subtract as usize, false) {
                                self.subtract = i == 1;
                            }
                        });
                        let modes = [Mode::Draw, Mode::Move, Mode::Scale, Mode::Rotate];
                        let current = modes.iter().position(|x| *x == self.mode).unwrap_or(0);
                        if let Some(i) = seg(ui, &["Draw", "Move", "Scale", "Rotate"], current, false) {
                            self.mode = modes[i];
                        }
                    });
                    zslider(ui, "Z Intensity", &mut m.brush.strength, 0.0..=1.0, false, true, 2);
                    zslider(ui, "Draw Size", &mut m.brush.radius, 0.01..=2.0, true, true, 2);
                    ui.add_enabled_ui(false, |ui| {
                        let (mut rgb, mut focal) = (1.0, 0.5);
                        zslider(ui, "Rgb Intensity", &mut rgb, 0.0..=1.0, false, true, 2);
                        zslider(ui, "Focal Shift", &mut focal, 0.0..=1.0, false, true, 2);
                    });
                });
            });
        if !held {
            let outside = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.latest_pos().is_none_or(|p| !area.response.rect.contains(p))
            });
            if outside {
                self.quick_menu = None;
            }
        }
    }

    /// Open a hotkey popup at `pos`; the same key again closes it.
    pub fn toggle_popup(&mut self, kind: QuickPopup, pos: Pos2) {
        self.popup = match self.popup {
            Some((open, _)) if open == kind => None,
            _ => Some((kind, pos)),
        };
    }

    fn quick_popup(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        let Some((kind, pos)) = self.popup else { return };
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.popup = None;
            return;
        }
        let width = 200.0;
        // Centre the 20 px slider on the cursor; the frame adds a 4 px margin.
        let area = egui::Area::new(Id::unique("quick-popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos - vec2(width * 0.5 + 4.0, 14.0))
            .show(&ctx, |ui| {
                popup_frame().show(ui, |ui| {
                    ui.set_width(width);
                    match kind {
                        QuickPopup::Size => {
                            zslider(ui, kind.label(), &mut m.brush.radius, 0.01..=2.0, true, true, 2);
                        }
                        QuickPopup::Intensity => {
                            zslider(ui, kind.label(), &mut m.brush.strength, 0.0..=1.0, false, true, 2);
                        }
                        QuickPopup::Rgb | QuickPopup::Focal => {
                            ui.add_enabled_ui(false, |ui| {
                                let mut v = if kind == QuickPopup::Rgb { 1.0 } else { 0.5 };
                                zslider(ui, kind.label(), &mut v, 0.0..=1.0, false, true, 2);
                            });
                            note(ui, NC);
                        }
                    }
                });
            });
        // A press anywhere outside closes it (the canvas swallows that press).
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.latest_pos().is_none_or(|p| !area.response.rect.contains(p))
        });
        if outside {
            self.popup = None;
        }
    }

    fn set_focus(&mut self, on: bool) {
        self.focus = on;
        self.dropdown = None;
    }

    fn flash(&mut self, ctx: &Context, text: String) {
        self.message = Some((text, ctx.input(|i| i.time)));
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }

    // ------------------------------------------------------------ menu bar
    fn menubar(&mut self, ui: &mut Ui, title: &str) {
        let response = egui::Panel::top("menubar")
            .exact_size(MENUBAR_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG2).inner_margin(Margin::symmetric(6, 0)))
            .show(ui, |ui| {
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().bottom() - 0.5,
                    (1.0, LINE),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (logo, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    ui.painter().rect_filled(logo, 2, ACC);
                    ui.painter().text(
                        logo.center(),
                        Align2::CENTER_CENTER,
                        "k",
                        FontId::monospace(11.0),
                        ACC_FG,
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("klay").size(12.5).strong());
                    ui.add_space(12.0);
                    for id in PaletteId::ALL {
                        let docked = self.layout.palette(id).place != Place::Menu;
                        let on = self.dropdown == Some(id);
                        let galley = ui.painter().layout_no_wrap(
                            id.name().to_string(),
                            FontId::proportional(11.5),
                            FG,
                        );
                        let width = galley.size().x + 14.0 + if docked { 9.0 } else { 0.0 };
                        let (r, resp) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
                        if on {
                            ui.painter().rect_filled(r, 2, ACC);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, 2, BG3);
                        }
                        let color = if on {
                            ACC_FG
                        } else if docked {
                            FG2
                        } else {
                            FG
                        };
                        ui.painter().galley(
                            r.left_center() + vec2(7.0, -galley.size().y * 0.5),
                            galley,
                            color,
                        );
                        if docked {
                            ui.painter().circle_filled(
                                r.right_center() - vec2(8.0, 0.0),
                                2.0,
                                if on { ACC_FG } else { ACC },
                            );
                        }
                        if resp.clicked() {
                            if on {
                                self.dropdown = None;
                            } else {
                                self.dropdown = Some(id);
                                self.dropdown_anchor = r;
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(title).size(11.0).color(FG3));
                    });
                });
            })
            .response;
        self.menubar_rect = response.rect;
    }

    fn dropdown(&mut self, ui: &mut Ui, m: &mut Model<'_>, a: &mut Actions) {
        let Some(id) = self.dropdown else {
            return;
        };
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let x = self
            .dropdown_anchor
            .left()
            .min(screen.right() - DROPDOWN_W - 6.0)
            .max(screen.left());
        let pos = pos2(x, self.menubar_rect.bottom() + 2.0);
        let max_height = (screen.bottom() - pos.y - 30.0).max(120.0);
        let state = self.layout.palette(id).clone();
        let area = egui::Area::new(Id::unique("palette-dropdown"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(&ctx, |ui| {
                Frame::NONE
                    .fill(BG1)
                    .stroke(Stroke::new(1.0, LINE))
                    .corner_radius(3)
                    .shadow(egui::Shadow {
                        offset: [0, 12],
                        blur: 36,
                        spread: 0,
                        color: Color32::from_black_alpha(150),
                    })
                    .show(ui, |ui| {
                        ui.set_width(DROPDOWN_W);
                        ScrollArea::vertical()
                            .id_salt("dropdown-scroll")
                            .scroll_source(egui::scroll_area::ScrollSource::ALL)
                            .max_height(max_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.palette(ui, &state, true, m, a));
                    });
            });
        // Click anywhere else (except the menu bar, which toggles) closes it.
        if ui.input(|i| i.pointer.any_pressed()) && !egui::Popup::is_any_open(&ctx) {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                if !area.response.rect.contains(p) && !self.menubar_rect.contains(p) {
                    self.dropdown = None;
                }
            }
        }
    }

    // ------------------------------------------------------------ status bar
    fn status_bar(&mut self, ui: &mut Ui, m: &Model<'_>) {
        let now = ui.input(|i| i.time);
        let message = match &self.message {
            Some((text, at)) if now - at < 1.8 => text.clone(),
            _ => {
                self.message = None;
                "Ready".to_string()
            }
        };
        egui::Panel::bottom("status")
            .exact_size(STATUS_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG2).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                ui.painter()
                    .hline(ui.max_rect().x_range(), ui.max_rect().top() + 0.5, (1.0, LINE));
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let (r, _) = ui.allocate_exact_size(vec2(7.0, 7.0), Sense::hover());
                    ui.painter()
                        .circle_filled(r.center(), 3.5, Color32::from_rgb(127, 191, 95));
                    ui.label(RichText::new(message).size(11.0).color(FG2));
                    ui.label(
                        RichText::new(format!(
                            "{} vertices · {} triangles · {} chunks",
                            crate::fmt_count(m.vertices),
                            crate::fmt_count(m.triangles),
                            m.chunks
                        ))
                        .font(FontId::monospace(10.5))
                        .color(FG2),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.1} ms · {:.0} fps", m.frame_ms, m.fps))
                                .font(FontId::monospace(10.5))
                                .color(FG2),
                        );
                        ui.label(
                            RichText::new(
                                "Click a menu name to open its palette · its dock button moves it to a tray · drag empty tray space to scroll · Tab hides the interface",
                            )
                            .size(11.0)
                            .color(FG3),
                        );
                    });
                });
            });
    }

    // ------------------------------------------------------------ shelves
    fn shelf_top(&mut self, ui: &mut Ui, m: &mut Model<'_>, a: &mut Actions) {
        egui::Panel::top("shelf")
            .exact_size(SHELF_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG1).inner_margin(Margin::symmetric(8, 0)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter()
                    .hline(rect.x_range(), rect.top() + 0.5, (1.0, HI));
                ui.painter()
                    .hline(rect.x_range(), rect.bottom() - 0.5, (1.0, LINE));
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    // mode buttons (Q / W / E / R)
                    for (glyph, label, mode, hint) in [
                        ("sculpt", "Sculpt", Mode::Draw, "Draw · Q"),
                        ("move", "Move", Mode::Move, "Move · W · drag the model for an action line, its middle circle moves"),
                        ("scale", "Scale", Mode::Scale, "Scale · E · drag the action line's end circle"),
                        ("rotate", "Rotate", Mode::Rotate, "Rotate · R · drag the action line's end circle"),
                    ] {
                        if big_button(ui, glyph, label, vec2(46.0, 46.0), self.mode == mode, true)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            self.mode = mode;
                        }
                    }
                    shelf_separator(ui);
                    // paint (not connected)
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        ui.add_space(6.0);
                        ui.add_enabled_ui(false, |ui| {
                            seg(ui, &["Mrgb", "Rgb", "M"], 2, false);
                            let mut rgb = 1.0;
                            zslider(ui, "Rgb Intensity", &mut rgb, 0.0..=1.0, false, true, 2);
                        });
                    });
                    shelf_separator(ui);
                    // Zadd / Zsub + intensity
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.set_width(170.0);
                            let w = ui.available_width() - 48.0;
                            ui.scope(|ui| {
                                ui.set_width(w);
                                if let Some(i) =
                                    seg(ui, &["Zadd", "Zsub"], self.subtract as usize, false)
                                {
                                    self.subtract = i == 1;
                                }
                            });
                            ui.add_enabled(
                                false,
                                egui::Button::new("Zcut").min_size(vec2(44.0, 20.0)),
                            );
                        });
                        zslider(
                            ui,
                            "Z Intensity",
                            &mut m.brush.strength,
                            0.0..=1.0,
                            false,
                            true,
                            2,
                        );
                    });
                    shelf_separator(ui);
                    // focal shift (not connected) + draw size
                    ui.vertical(|ui| {
                        ui.set_width(190.0);
                        ui.add_space(6.0);
                        ui.add_enabled_ui(false, |ui| {
                            let mut focal = 0.5;
                            zslider(ui, "Focal Shift", &mut focal, 0.0..=1.0, false, true, 2);
                        });
                        ui.horizontal(|ui| {
                            ui.set_width(190.0);
                            let w = ui.available_width() - 40.0;
                            ui.scope(|ui| {
                                ui.set_width(w);
                                zslider(
                                    ui,
                                    "Draw Size",
                                    &mut m.brush.radius,
                                    0.01..=2.0,
                                    true,
                                    true,
                                    2,
                                );
                            });
                            if small_toggle(ui, "Dyn", self.dynamic, vec2(36.0, 20.0))
                                .on_hover_text("Radius follows zoom")
                                .clicked()
                            {
                                self.dynamic = !self.dynamic;
                            }
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        a.redo |= ui
                            .add_enabled(m.can_redo, egui::Button::new("Redo"))
                            .on_hover_text("Ctrl+Shift+Z")
                            .clicked();
                        a.undo |= ui
                            .add_enabled(m.can_undo, egui::Button::new("Undo"))
                            .on_hover_text("Ctrl+Z")
                            .clicked();
                    });
                });
            });
    }

    fn shelf_left(&mut self, ui: &mut Ui, m: &mut Model<'_>, a: &mut Actions) {
        let _ = a;
        egui::Panel::left("shelf-left")
            .exact_size(LEFT_SHELF_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG1).inner_margin(Margin::symmetric(6, 6)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter()
                    .vline(rect.right() - 0.5, rect.y_range(), (1.0, LINE));
                ScrollArea::vertical()
                    .id_salt("shelf-left-scroll")
                    .scroll_source(egui::scroll_area::ScrollSource::ALL)
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        // brush
                        let tex = self.texture(ui.ctx(), Some(m.brush.kind), self.subtract);
                        let resp = thumb(ui, Thumb::Texture(tex), m.brush.kind.name(), true, false);
                        let picker = egui::Popup::from_toggle_button_response(&resp)
                            .align(egui::RectAlign::RIGHT_START)
                            .gap(6.0)
                            .frame(popup_frame());
                        picker.show(|ui| {
                            ui.label(RichText::new("BRUSH · 1–5").size(10.0).color(FG3));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for (i, kind) in BrushKind::ALL.into_iter().enumerate() {
                                    let tex = self.texture(ui.ctx(), Some(kind), self.subtract);
                                    if brush_tile(
                                        ui,
                                        tex,
                                        kind.name(),
                                        i + 1,
                                        m.brush.kind == kind,
                                        88.0,
                                    )
                                    .clicked()
                                    {
                                        m.brush.kind = kind;
                                    }
                                }
                            });
                            ui.add_enabled_ui(false, |ui| {
                                ui.horizontal(|ui| {
                                    for label in ["Load Brush", "Save As", "Clone", "Reset All"] {
                                        let _ = ui.add(
                                            egui::Button::new(label).min_size(vec2(108.0, 20.0)),
                                        );
                                    }
                                });
                            });
                        });
                        thumb(ui, Thumb::Glyph("stroke"), "Freehand", false, false)
                            .on_hover_text("Stroke types are not connected yet");
                        thumb(ui, Thumb::Glyph("alpha"), "Alpha Off", false, true)
                            .on_hover_text("Alphas are not connected yet");
                        thumb(ui, Thumb::Glyph("texture"), "Texture Off", false, true)
                            .on_hover_text("Textures are not connected yet");
                        // material
                        let tex = self.texture(ui.ctx(), None, false);
                        let resp = thumb(
                            ui,
                            Thumb::Texture(tex),
                            MATERIALS[self.material].1,
                            false,
                            false,
                        );
                        egui::Popup::from_toggle_button_response(&resp)
                            .align(egui::RectAlign::RIGHT_START)
                            .gap(6.0)
                            .frame(popup_frame())
                            .show(|ui| {
                                ui.label(RichText::new("MATERIAL").size(10.0).color(FG3));
                                self.material_swatches(ui);
                            });
                        // colour
                        ui.add_space(3.0);
                        color_picker(ui, &mut self.color);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(40.0, 32.0), Sense::click());
                            let p = ui.painter();
                            let back =
                                Rect::from_min_size(r.min + vec2(16.0, 10.0), vec2(24.0, 22.0));
                            let front = Rect::from_min_size(r.min, vec2(24.0, 22.0));
                            for (rect, c) in [(back, self.color2), (front, self.color)] {
                                p.rect_filled(rect, 0, Color32::from(c));
                                p.rect_stroke(rect, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
                            }
                            glyph(
                                p,
                                r.right_top() + vec2(-6.0, 6.0),
                                "swap",
                                if resp.hovered() { FG } else { FG2 },
                            );
                            if resp.on_hover_text("Switch colours").clicked() {
                                std::mem::swap(&mut self.color, &mut self.color2);
                            }
                            let [cr, cg, cb] = self.color.to_srgb();
                            ui.label(
                                RichText::new(format!("#{cr:02x}{cg:02x}{cb:02x}"))
                                    .font(FontId::monospace(10.0))
                                    .color(FG2),
                            );
                        });
                        ui.add_enabled_ui(false, |ui| {
                            let _ = ui.add(
                                egui::Button::new(RichText::new("Alternate").color(ACC_FG))
                                    .fill(ACC)
                                    .min_size(vec2(86.0, 20.0)),
                            );
                        });
                        ui.add_space(6.0);
                    });
            });
    }

    fn shelf_right(&mut self, ui: &mut Ui, a: &mut Actions) {
        egui::Panel::right("shelf-right")
            .exact_size(RIGHT_SHELF_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG1).inner_margin(Margin::symmetric(4, 6)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter()
                    .vline(rect.left() + 0.5, rect.y_range(), (1.0, LINE));
                ui.spacing_mut().item_spacing.y = 4.0;
                let size = vec2(46.0, 40.0);
                if big_button(ui, "frame", "Frame", size, false, true)
                    .on_hover_text("Frame mesh · F")
                    .clicked()
                {
                    a.frame = true;
                }
                if big_button(ui, "persp", "Persp", size, self.perspective, true)
                    .on_hover_text("Perspective · P")
                    .clicked()
                {
                    self.perspective = !self.perspective;
                }
                if big_button(ui, "floor", "Floor", size, self.grid, true)
                    .on_hover_text("Floor grid · Shift + P")
                    .clicked()
                {
                    self.grid = !self.grid;
                }
                if big_button(ui, "sym", "Sym", size, self.symmetry, true)
                    .on_hover_text("Symmetry across X · X")
                    .clicked()
                {
                    self.symmetry = !self.symmetry;
                }
                nav_separator(ui);
                if big_button(ui, "polyf", "PolyF", size, self.polyframe, true)
                    .on_hover_text("Polygroup colours · Shift + F")
                    .clicked()
                {
                    self.polyframe = !self.polyframe;
                }
                for (g, label) in [("local", "Local"), ("transp", "Transp"), ("ghost", "Ghost")] {
                    big_button(ui, g, label, size, false, false).on_hover_text(NC);
                }
                if big_button(ui, "solo", "Solo", size, self.solo, true)
                    .on_hover_text("Show only the active subtool")
                    .clicked()
                {
                    self.solo = !self.solo;
                }
                nav_separator(ui);
                if big_button(ui, "detail", "Detail", size, self.wireframe, true)
                    .on_hover_text("Surface detail shading")
                    .clicked()
                {
                    self.wireframe = !self.wireframe;
                }
                if big_button(ui, "hide", "Hide", size, false, true)
                    .on_hover_text("Hide interface · Tab")
                    .clicked()
                {
                    self.set_focus(true);
                }
            });
    }

    // ------------------------------------------------------------ trays
    fn divider(&mut self, ui: &mut Ui, side: Side) {
        let panel = match side {
            Side::Left => egui::Panel::left("divider-left"),
            _ => egui::Panel::right("divider-right"),
        };
        let open = self.layout.tray_open(side);
        panel
            .exact_size(DIVIDER_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG2))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let resp = ui.interact(rect, Id::unique(("divider", side)), Sense::click());
                let p = ui.painter();
                if resp.hovered() {
                    p.rect_filled(rect, 0, BG3);
                }
                p.vline(rect.left() + 0.5, rect.y_range(), (1.0, LINE));
                p.vline(rect.right() - 0.5, rect.y_range(), (1.0, LINE));
                // caret points towards where the tray would go / come from
                let points_left = (side == Side::Left) == open;
                let c = rect.center();
                let tri = if points_left {
                    vec![c + vec2(2.5, -4.5), c + vec2(-2.5, 0.0), c + vec2(2.5, 4.5)]
                } else {
                    vec![
                        c + vec2(-2.5, -4.5),
                        c + vec2(2.5, 0.0),
                        c + vec2(-2.5, 4.5),
                    ]
                };
                p.add(egui::Shape::convex_polygon(
                    tri,
                    if resp.hovered() { ACC } else { FG3 },
                    Stroke::NONE,
                ));
                if resp
                    .on_hover_text(format!("Toggle {} tray", side.name().to_lowercase()))
                    .clicked()
                {
                    self.layout.set_tray_open(side, !open);
                }
            });
    }

    fn tray(&mut self, ui: &mut Ui, side: Side, m: &mut Model<'_>, a: &mut Actions) {
        let palettes = self.layout.in_tray(side);
        let horizontal = side.horizontal();
        if horizontal && palettes.is_empty() {
            return;
        }
        if !horizontal && !self.layout.tray_open(side) {
            return;
        }
        let screen = ui.ctx().content_rect();
        let panel = match side {
            Side::Top => egui::Panel::top("tray-top"),
            Side::Bottom => egui::Panel::bottom("tray-bottom"),
            Side::Left => egui::Panel::left("tray-left"),
            Side::Right => egui::Panel::right("tray-right"),
        };
        let panel = if horizontal {
            let all_closed = palettes.iter().all(|p| !p.open);
            let size = if all_closed {
                HEADER_H + 1.0
            } else {
                (ui.available_height() * 0.42)
                    .min(280.0)
                    .max(HEADER_H + 1.0)
            };
            panel.exact_size(size)
        } else {
            let width = self
                .layout
                .tray_width
                .min(((screen.width() - 420.0) * 0.5).max(160.0));
            panel.exact_size(width)
        };
        let response = panel
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG1))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let p = ui.painter();
                match side {
                    Side::Left => p.vline(rect.right() - 0.5, rect.y_range(), (1.0, LINE)),
                    Side::Right => p.vline(rect.left() + 0.5, rect.y_range(), (1.0, LINE)),
                    Side::Top => p.hline(rect.x_range(), rect.bottom() - 0.5, (1.0, LINE)),
                    Side::Bottom => p.hline(rect.x_range(), rect.top() + 0.5, (1.0, LINE)),
                };
                if palettes.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Drop a palette here,\nor dock one from its dock button")
                                .size(11.0)
                                .color(FG3),
                        );
                    });
                    return;
                }
                if horizontal {
                    let height = ui.available_height();
                    ScrollArea::horizontal()
                        .id_salt(("tray-scroll", side))
                        .scroll_source(egui::scroll_area::ScrollSource::ALL)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for state in &palettes {
                                    ui.scope_builder(
                                        UiBuilder::new()
                                            .max_rect(Rect::from_min_size(
                                                ui.cursor().min,
                                                vec2(f32::INFINITY, height),
                                            ))
                                            .layout(Layout::top_down(Align::Min)),
                                        |ui| {
                                            self.palette(ui, state, false, m, a);
                                        },
                                    );
                                    let x = ui.cursor().min.x;
                                    ui.painter().vline(x - 0.5, rect.y_range(), (1.0, LINE));
                                }
                            });
                        });
                } else {
                    ScrollArea::vertical()
                        .id_salt(("tray-scroll", side))
                        .scroll_source(egui::scroll_area::ScrollSource::ALL)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for state in &palettes {
                                self.palette(ui, state, false, m, a);
                            }
                            ui.add_space(24.0);
                        });
                }
            })
            .response;
        // Empty space in a tray accepts a drop at the end.
        if response.contains_pointer()
            && ui.input(|i| i.pointer.any_released())
            && self.pending_move.is_none()
        {
            if let Some(id) = egui::DragAndDrop::take_payload::<PaletteId>(ui.ctx()) {
                self.pending_move = Some((*id, Place::Tray(side), None));
            }
        }
    }

    // ------------------------------------------------------------ palette shell
    fn palette(
        &mut self,
        ui: &mut Ui,
        state: &PaletteState,
        floating: bool,
        m: &mut Model<'_>,
        a: &mut Actions,
    ) {
        let id = state.id;
        let horizontal = matches!(state.place, Place::Tray(s) if s.horizontal()) && !floating;
        let open = state.open || floating;
        ui.push_id(("palette", id), |ui| {
            let response = Frame::NONE
                .fill(BG1)
                .show(ui, |ui| {
                    if horizontal {
                        ui.set_min_width(if open { SECTION_W } else { 150.0 });
                    } else {
                        ui.set_width(ui.available_width());
                    }
                    // header
                    let width = ui.available_width();
                    let (hr, _) = ui.allocate_exact_size(vec2(width, HEADER_H), Sense::hover());
                    let hovered = ui.rect_contains_pointer(hr);
                    let p = ui.painter();
                    p.rect_filled(hr, 0, if hovered { BG3 } else { BG2 });
                    p.hline(hr.x_range(), hr.top() + 0.5, (1.0, HI));
                    p.hline(hr.x_range(), hr.bottom() - 0.5, (1.0, LINE));
                    // dock button: click = menu, drag = move
                    let dock_rect = Rect::from_center_size(
                        hr.left_center() + vec2(12.0, 0.0),
                        vec2(18.0, 18.0),
                    );
                    let dock =
                        ui.interact(dock_rect, Id::unique(("dock", id)), Sense::click_and_drag());
                    dock.dnd_set_drag_payload(id);
                    let dock_color = if dock.hovered() || dock.dragged() {
                        ACC
                    } else {
                        FG3
                    };
                    if dock.hovered() {
                        ui.painter().circle_filled(dock_rect.center(), 9.0, BG4);
                    }
                    ui.painter().circle_stroke(
                        dock_rect.center(),
                        4.6,
                        Stroke::new(1.2, dock_color),
                    );
                    ui.painter()
                        .circle_filled(dock_rect.center(), 1.8, dock_color);
                    self.dock_menu(&dock, id);
                    let mut x = hr.left() + 24.0;
                    if !floating {
                        chevron(ui.painter(), pos2(x + 4.0, hr.center().y), open, FG2);
                        x += 14.0;
                    }
                    ui.painter().text(
                        pos2(x + 2.0, hr.center().y),
                        Align2::LEFT_CENTER,
                        id.name(),
                        FontId::proportional(12.0),
                        FG,
                    );
                    if floating {
                        if let Place::Tray(side) = state.place {
                            tag(
                                ui.painter(),
                                hr.right_center() - vec2(6.0, 0.0),
                                &format!("{} tray", side.name().to_lowercase()),
                            );
                        }
                    } else {
                        let title_rect =
                            Rect::from_min_max(pos2(hr.left() + 22.0, hr.top()), hr.max);
                        if ui
                            .interact(title_rect, Id::unique(("title", id)), Sense::click())
                            .clicked()
                        {
                            self.layout.palette_mut(id).open = !state.open;
                        }
                    }
                    // body
                    if open {
                        if horizontal {
                            let max_h = (ui.available_height() - 2.0).max(40.0);
                            ScrollArea::vertical()
                                .id_salt("palette-body")
                                .scroll_source(egui::scroll_area::ScrollSource::ALL)
                                .max_height(max_h)
                                .auto_shrink([true, true])
                                .show(ui, |ui| {
                                    ui.horizontal_top(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        self.palette_body(ui, id, true, m, a);
                                    });
                                });
                        } else {
                            self.palette_body(ui, id, false, m, a);
                        }
                    }
                })
                .response;
            if !floating {
                if self.scroll_to == Some(id) {
                    ui.scroll_to_rect(response.rect, Some(Align::Min));
                    self.scroll_to = None;
                }
                // drop before this palette
                if let Some(payload) = response.dnd_hover_payload::<PaletteId>() {
                    if *payload != id {
                        let r = response.rect;
                        if horizontal {
                            ui.painter()
                                .vline(r.left(), r.y_range(), Stroke::new(2.0, ACC));
                        } else {
                            ui.painter()
                                .hline(r.x_range(), r.top(), Stroke::new(2.0, ACC));
                        }
                        if ui.input(|i| i.pointer.any_released()) {
                            egui::DragAndDrop::take_payload::<PaletteId>(ui.ctx());
                            self.pending_move = Some((*payload, state.place, Some(id)));
                        }
                    }
                }
            }
        });
    }

    fn dock_menu(&mut self, anchor: &egui::Response, id: PaletteId) {
        let current = self.layout.palette(id).place;
        let open = self.layout.palette(id).open;
        egui::Popup::menu(anchor)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .frame(popup_frame())
            .show(|ui| {
                ui.set_min_width(150.0);
                ui.label(
                    RichText::new(id.name().to_uppercase())
                        .size(10.0)
                        .color(FG3),
                );
                for side in Side::ALL {
                    let label = format!("{} tray", side.name());
                    if menu_item(ui, &label, current == Place::Tray(side)).clicked() {
                        self.pending_move = Some((id, Place::Tray(side), None));
                    }
                }
                ui.separator();
                if menu_item(ui, "Menu only (undock)", current == Place::Menu).clicked() {
                    self.pending_move = Some((id, Place::Menu, None));
                }
                if current != Place::Menu
                    && menu_item(ui, if open { "Collapse" } else { "Expand" }, false).clicked()
                {
                    self.layout.palette_mut(id).open = !open;
                }
            });
    }

    /// Collapsible sub-palette. Returns whether its body should be drawn.
    fn section(
        &mut self,
        ui: &mut Ui,
        pal: PaletteId,
        sid: &str,
        title: &str,
        tag_text: Option<&str>,
        default_open: bool,
    ) -> bool {
        let key = format!("{}/{}", pal.name(), sid);
        let open = *self.layout.sections.get(&key).unwrap_or(&default_open);
        let (r, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), SECTION_H), Sense::click());
        let p = ui.painter();
        p.rect_filled(r, 0, if resp.hovered() { BG3 } else { BG2 });
        p.hline(r.x_range(), r.top() + 0.5, (1.0, LINE));
        chevron(p, r.left_center() + vec2(12.0, 0.0), open, FG2);
        p.text(
            r.left_center() + vec2(22.0, 0.0),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(11.5),
            FG,
        );
        if let Some(t) = tag_text {
            tag(p, r.right_center() - vec2(6.0, 0.0), t);
        }
        if resp.clicked() {
            if open == default_open {
                self.layout.sections.insert(key, !open);
            } else {
                self.layout.sections.remove(&key);
            }
            return !open;
        }
        open
    }

    /// Section header plus body, laid out side by side in horizontal trays.
    #[allow(clippy::too_many_arguments)]
    fn sec(
        &mut self,
        ui: &mut Ui,
        horizontal: bool,
        pal: PaletteId,
        sid: &str,
        title: &str,
        tag_text: Option<&str>,
        default_open: bool,
        enabled: bool,
        body: &mut dyn FnMut(&mut Self, &mut Ui),
    ) {
        let mut draw = |ws: &mut Self, ui: &mut Ui| {
            if ws.section(ui, pal, sid, title, tag_text, default_open) {
                Frame::NONE
                    .inner_margin(Margin {
                        left: 7,
                        right: 7,
                        top: 5,
                        bottom: 7,
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.set_width(ui.available_width());
                        ui.add_enabled_ui(enabled, |ui| body(ws, ui));
                    });
            }
        };
        if horizontal {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        ui.cursor().min,
                        vec2(SECTION_W, f32::INFINITY),
                    ))
                    .layout(Layout::top_down(Align::Min)),
                |ui| {
                    ui.set_width(SECTION_W);
                    draw(self, ui);
                },
            );
        } else {
            draw(self, ui);
        }
    }

    // ------------------------------------------------------------ palette contents
    fn palette_body(
        &mut self,
        ui: &mut Ui,
        id: PaletteId,
        h: bool,
        m: &mut Model<'_>,
        a: &mut Actions,
    ) {
        match id {
            PaletteId::Alpha => {
                self.sec(
                    ui,
                    h,
                    id,
                    "alpha",
                    "Alpha",
                    Some(NC),
                    true,
                    false,
                    &mut |_, ui| {
                        note(ui, "No alpha selected.");
                        full_button(ui, "Load alpha…");
                    },
                );
            }
            PaletteId::Brush => {
                self.sec(
                    ui,
                    h,
                    id,
                    "brushes",
                    "Brushes",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        let cols = ((ui.available_width() + 4.0) / 78.0).floor().max(1.0) as usize;
                        let tile = (ui.available_width() - 4.0 * (cols as f32 - 1.0)) / cols as f32;
                        for row in BrushKind::ALL.chunks(cols) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for kind in row {
                                    let i =
                                        BrushKind::ALL.iter().position(|k| k == kind).unwrap() + 1;
                                    let tex = ws.texture(ui.ctx(), Some(*kind), ws.subtract);
                                    if brush_tile(
                                        ui,
                                        tex,
                                        kind.name(),
                                        i,
                                        m.brush.kind == *kind,
                                        tile,
                                    )
                                    .clicked()
                                    {
                                        m.brush.kind = *kind;
                                    }
                                }
                            });
                        }
                        note(ui, "Alt inverts · Shift smooths · 1–5 selects");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "mods",
                    "Modifiers",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        zslider(
                            ui,
                            "Radius",
                            &mut m.brush.radius,
                            0.01..=2.0,
                            true,
                            false,
                            2,
                        );
                        zslider(
                            ui,
                            "Strength",
                            &mut m.brush.strength,
                            0.0..=1.0,
                            false,
                            false,
                            2,
                        );
                        if let Some(i) = seg(ui, &["Add", "Subtract"], ws.subtract as usize, false)
                        {
                            ws.subtract = i == 1;
                        }
                        ui.add_enabled_ui(false, |ui| {
                            let mut falloff = 0.5;
                            zslider(ui, "Falloff", &mut falloff, 0.0..=1.0, false, false, 2);
                        });
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "tablet",
                    "Tablet pressure",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            ui.checkbox(&mut ws.pressure_size, "Size")
                                .on_hover_text("Pen pressure scales Draw Size");
                            ui.checkbox(&mut ws.pressure_intensity, "Z Intensity")
                                .on_hover_text("Pen pressure scales Z Intensity (strength)");
                        });
                        let reading = match (m.tablet_on, m.pressure) {
                            (false, _) => "off".to_string(),
                            (true, Some(p)) => format!("{:.0} %", p * 100.0),
                            (true, None) => "mouse".to_string(),
                        };
                        kv(ui, "Pressure", &reading);
                        // Device names and reasons are long: a wrapping line, not a value.
                        if !m.tablet.is_empty() {
                            note(ui, &format!("Pen: {}", m.tablet));
                        }
                        note(ui, "Linear, like ZBrush's default curves · Grab keeps its size");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "auto",
                    "Auto masking",
                    Some(NC),
                    false,
                    false,
                    &mut |_, ui| {
                        let (mut bf, mut topo) = (false, false);
                        ui.checkbox(&mut bf, "Backface mask");
                        ui.checkbox(&mut topo, "Topological");
                    },
                );
            }
            PaletteId::Document => {
                self.sec(
                    ui,
                    h,
                    id,
                    "doc",
                    "Document",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        let name = std::path::Path::new(m.project_path.as_str())
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().to_string());
                        kv(ui, "Name", &name);
                        kv(ui, "Subtools", &m.subtools.len().to_string());
                        ui.add(
                            egui::TextEdit::singleline(m.project_path)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(11.0)),
                        );
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                        let w = (r.width() - 6.0) / 3.0;
                        for (col, (label, hint)) in [
                            ("New", "Start over with one subtool from Mesh › Initialize"),
                            ("Open", "Open the project at this path · Ctrl + O"),
                            ("Save", "Save every subtool to this path · Ctrl + S"),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                            if ui.put(cell, egui::Button::new(label)).on_hover_text(hint).clicked() {
                                match col {
                                    0 => a.new_project = true,
                                    1 => a.open = true,
                                    _ => a.save = true,
                                }
                            }
                        }
                        note(ui, "Saves every subtool with its levels and sculpting; undo history is not saved.");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "export",
                    "Export",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        ui.add(
                            egui::TextEdit::singleline(m.export_path)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(11.0)),
                        );
                        if ui
                            .add_sized(
                                [ui.available_width(), 20.0],
                                egui::Button::new("Export OBJ"),
                            )
                            .on_hover_text("Every visible subtool at its shown level, one OBJ object each")
                            .clicked()
                        {
                            a.export = true;
                        }
                        if m.status.is_empty() {
                            note(
                                ui,
                                "Every visible subtool at its shown level, as one object each.",
                            );
                        } else {
                            note(ui, m.status);
                        }
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "import",
                    "Import",
                    Some(NC),
                    false,
                    false,
                    &mut |_, ui| {
                        full_button(ui, "Import OBJ…");
                    },
                );
            }
            PaletteId::Edit => {
                self.sec(ui, h, id, "undo", "Undo", None, true, true, &mut |_, ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 3.0) * 0.5;
                        a.undo |= ui
                            .add_enabled(
                                m.can_undo,
                                egui::Button::new("Undo").min_size(vec2(w, 20.0)),
                            )
                            .clicked();
                        a.redo |= ui
                            .add_enabled(
                                m.can_redo,
                                egui::Button::new("Redo").min_size(vec2(w, 20.0)),
                            )
                            .clicked();
                    });
                    kv(
                        ui,
                        "Snapshots",
                        &format!("{:.1} MB", m.history_bytes as f32 / 1e6),
                    );
                });
                self.sec(
                    ui,
                    h,
                    id,
                    "history",
                    "History",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        if m.can_undo || m.can_redo {
                            kv(ui, "Undo available", if m.can_undo { "yes" } else { "no" });
                            kv(ui, "Redo available", if m.can_redo { "yes" } else { "no" });
                        } else {
                            note(ui, "No strokes yet.");
                        }
                    },
                );
            }
            PaletteId::Masking => {
                self.sec(
                    ui,
                    h,
                    id,
                    "mask",
                    "Mask",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        use crate::mask::MaskOp;
                        ui.horizontal(|ui| {
                            for (label, op, hint) in [
                                ("Invert", MaskOp::Invert, "Ctrl + I · Ctrl + click the background"),
                                ("Clear", MaskOp::Clear, "Ctrl + Shift + A · Ctrl + drag the background"),
                                ("Mask all", MaskOp::All, "Ctrl + A"),
                            ] {
                                if ui.button(label).on_hover_text(hint).clicked() {
                                    a.mask = Some(op);
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            for (label, op, hint) in [
                                ("Blur", MaskOp::Blur, "Ctrl + click the model"),
                                ("Sharpen", MaskOp::Sharpen, "Ctrl + Alt + click the model"),
                            ] {
                                if ui.button(label).on_hover_text(hint).clicked() {
                                    a.mask = Some(op);
                                }
                            }
                        });
                        ui.checkbox(&mut ws.show_mask, "View mask").on_hover_text("Ctrl + H");
                        note(ui, "Ctrl + drag paints · Ctrl + Alt + drag erases");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "masktype",
                    "Mask type",
                    Some(NC),
                    true,
                    false,
                    &mut |_, ui| {
                        seg(ui, &["Paint", "Rect", "Lasso"], 0, false);
                    },
                );
            }
            PaletteId::Material => {
                self.sec(
                    ui,
                    h,
                    id,
                    "mats",
                    "Materials",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ws.material_swatches(ui);
                        note(ui, MATERIALS[ws.material].1);
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "shade",
                    "Shading",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.checkbox(&mut ws.wireframe, "Surface detail shading");
                        ui.checkbox(&mut ws.grid, "Floor grid");
                    },
                );
            }
            PaletteId::Mesh => {
                self.sec(
                    ui,
                    h,
                    id,
                    "subtool",
                    "SubTool",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        for (i, row) in m.subtools.iter().enumerate() {
                            let (pick, eye) = subtool_row(ui, i, row);
                            if pick {
                                a.select_subtool = Some(i);
                            }
                            if eye {
                                a.toggle_subtool = Some(i);
                            }
                        }
                        ui.add_space(2.0);
                        let active = m.subtools.iter().position(|r| r.active).unwrap_or(0);
                        let count = m.subtools.len();
                        // Three fixed cells per row: a label wider than its cell is clipped
                        // instead of widening the tray.
                        let mut clicked = [false; 6];
                        let labels = [
                            ("Append", true, "Add the Initialize primitive next to the model and select it"),
                            ("Duplicate", true, "Copy the active subtool below it"),
                            ("Delete", count > 1, "Delete the active subtool · not undoable"),
                            ("Up", active > 0, "Move the active subtool up the list"),
                            ("Down", active + 1 < count, "Move the active subtool down the list"),
                            ("Solo", true, "Show only the active subtool"),
                        ];
                        for row in 0..2 {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                            let w = (r.width() - 6.0) / 3.0;
                            for col in 0..3 {
                                let k = row * 3 + col;
                                let (label, on, hint) = labels[k];
                                let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                                child.spacing_mut().button_padding = vec2(2.0, 1.0);
                                if !on {
                                    child.disable();
                                }
                                let selected = k == 5 && ws.solo;
                                clicked[k] = child
                                    .put(
                                        cell,
                                        egui::Button::new(RichText::new(label).size(11.0))
                                            .truncate()
                                            .selected(selected),
                                    )
                                    .on_hover_text(hint)
                                    .clicked();
                            }
                        }
                        a.append |= clicked[0];
                        a.duplicate |= clicked[1];
                        a.delete |= clicked[2];
                        if clicked[3] {
                            a.move_subtool = -1;
                        }
                        if clicked[4] {
                            a.move_subtool = 1;
                        }
                        if clicked[5] {
                            ws.solo = !ws.solo;
                        }
                        kv(
                            ui,
                            "Active in RAM",
                            &format!("{} MB", m.subtools.iter().find(|r| r.active).map_or(0, |r| r.bytes) / 1_000_000),
                        );
                        if m.subtools.len() > 1 {
                            kv(ui, "Parked in RAM", &format!("{} MB", m.parked_bytes / 1_000_000));
                            kv(ui, "Parked on GPU", &format!("{} MB", m.stand_in_bytes / 1_000_000));
                        }
                        note(ui, "Alt + click a subtool to select it");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "deformation",
                    "Deformation",
                    None,
                    false,
                    true,
                    &mut |ws, ui| {
                        for kind in Deform::ALL {
                            if let Some(v) = spring_slider(ui, kind, &mut ws.deform_drag) {
                                a.deform = Some((kind, v));
                            }
                        }
                        note(ui, "Drag and release: applies to every level of the active subtool");
                    },
                );
                self.sec(ui, h, id, "info", "Mesh", None, true, true, &mut |_, ui| {
                    kv(
                        ui,
                        "Object",
                        m.subtools.iter().find(|r| r.active).map_or("", |r| r.name.as_str()),
                    );
                    kv(ui, "Vertices", &crate::fmt_count(m.vertices));
                    kv(ui, "Triangles", &crate::fmt_count(m.triangles));
                    kv(ui, "Chunks", &crate::fmt_count(m.chunks));
                });
                self.sec(
                    ui,
                    h,
                    id,
                    "geometry",
                    "Geometry",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        let top = m.levels.len().saturating_sub(1);
                        let fits = m.next_level_vertices <= *m.vertex_budget;
                        if ui
                            .add_enabled_ui(fits, |ui| full_button(ui, "Divide"))
                            .inner
                            .on_hover_text(format!(
                                "Ctrl + D · next level ≈ {} vertices",
                                crate::fmt_count(m.next_level_vertices)
                            ))
                            .on_disabled_hover_text(format!(
                                "Next level ({} vertices) is over the budget",
                                crate::fmt_count(m.next_level_vertices)
                            ))
                            .clicked()
                        {
                            a.divide = true;
                        }
                        let mut level = m.level as f32;
                        ui.add_enabled_ui(top > 0, |ui| {
                            if zslider(ui, "SDiv", &mut level, 0.0..=top.max(1) as f32, false, false, 0) {
                                let l = level.round() as usize;
                                if l != m.level {
                                    a.level = Some(l);
                                }
                            }
                        });
                        kv(ui, "Level", &format!("{} of {}", m.level, top));
                        kv(
                            ui,
                            "Vertices at level",
                            &crate::fmt_count(m.levels.get(m.level).copied().unwrap_or(0)),
                        );
                        kv(ui, "Next level", &crate::fmt_count(m.next_level_vertices));
                        let mut budget = *m.vertex_budget as f32 / 1e6;
                        if zslider(ui, "Vertex budget (M)", &mut budget, 1.0..=120.0, true, false, 0) {
                            *m.vertex_budget = (budget.round() * 1e6) as usize;
                        }
                        if top > 0 {
                            kv(ui, "Levels in memory", &format!("{} MB", m.levels_bytes / 1_000_000));
                        }
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            ui.add_enabled_ui(m.level > 0, |ui| {
                                if ui
                                    .add(egui::Button::new("Del Lower").min_size(vec2(w, 20.0)))
                                    .on_hover_text("Make this level the base, deleting the ones below · clears undo")
                                    .clicked()
                                {
                                    a.del_lower = true;
                                }
                            });
                            ui.add_enabled_ui(m.level < top, |ui| {
                                if ui
                                    .add(egui::Button::new("Del Higher").min_size(vec2(w, 20.0)))
                                    .on_hover_text("Delete the levels above this one · clears undo")
                                    .clicked()
                                {
                                    a.del_higher = true;
                                }
                            });
                        });
                        note(ui, "D / Shift + D: higher / lower level");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "wedge",
                    "Wedge",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            let can = m.wedge_refusal.is_none() || m.wedge_on;
                            ui.add_enabled_ui(can, |ui| {
                                let r = ui
                                    .add(egui::Button::new("Wedge").selected(m.wedge_on).min_size(vec2(w, 20.0)))
                                    .on_hover_text("Remesh into an even, closed surface and keep Wedge on (ZBrush: DynaMesh)");
                                let r = match m.wedge_refusal {
                                    Some(why) => r.on_disabled_hover_text(why),
                                    None => r,
                                };
                                if r.clicked() {
                                    a.wedge_toggle = true;
                                }
                            });
                            ui.add_enabled_ui(m.wedge_on && m.wedge_refusal.is_none(), |ui| {
                                if ui
                                    .add(egui::Button::new("Remesh").min_size(vec2(w, 20.0)))
                                    .on_hover_text("Wedge again now · Ctrl + drag on the background")
                                    .clicked()
                                {
                                    a.wedge_now = true;
                                }
                            });
                        });
                        let mut r = m.wedge.resolution as f32;
                        if zslider(ui, "Resolution", &mut r, 8.0..=crate::wedge::MAX_RESOLUTION as f32, true, false, 0) {
                            m.wedge.resolution = r.round() as u32;
                        }
                        let mut b = m.wedge.blur as f32;
                        ui.add_enabled_ui(m.wedge.project, |ui| {
                            if zslider(ui, "Blur", &mut b, 0.0..=20.0, false, false, 0) {
                                m.wedge.blur = b.round() as u32;
                            }
                        });
                        ui.checkbox(&mut m.wedge.project, "Project")
                            .on_hover_text("Put the new surface on the old one: keeps detail down to the voxel size");
                        kv(ui, "About", &format!("{} vertices", crate::fmt_count(m.wedge_estimate)));
                        if let Some(why) = m.wedge_refusal {
                            note(ui, why);
                        } else {
                            note(ui, "Overlapping parts merge. Ctrl + drag on the background re-wedges while on.");
                        }
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "visibility",
                    "Visibility",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        kv(
                            ui,
                            "Hidden",
                            &format!("{} of {} triangles", crate::fmt_count(m.hidden), crate::fmt_count(m.triangles)),
                        );
                        ui.add_enabled_ui(m.hidden > 0, |ui| {
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 4.0) * 0.5;
                                if ui
                                    .add(egui::Button::new("Show all").min_size(vec2(w, 20.0)))
                                    .on_hover_text("Ctrl + Shift + click on the background")
                                    .clicked()
                                {
                                    a.show_all = true;
                                }
                                if ui
                                    .add(egui::Button::new("Invert").min_size(vec2(w, 20.0)))
                                    .on_hover_text("Swap hidden and visible · Ctrl + Shift + drag on the background")
                                    .clicked()
                                {
                                    a.invert_visibility = true;
                                }
                            });
                        });
                        note(ui, "Ctrl + Shift + drag: keep the rectangle (Alt: hide it) · click a group to isolate it (Alt: hide it)");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "polygroups",
                    "Polygroups",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        kv(ui, "Polygroups", &crate::fmt_count(m.groups));
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            if ui
                                .add(egui::Button::new("Group Visible").min_size(vec2(w, 20.0)))
                                .on_hover_text("One new polygroup of the visible polygons · Ctrl + W")
                                .clicked()
                            {
                                a.group_visible = true;
                            }
                            ui.add_enabled_ui(m.polyframe_supported, |ui| {
                                if ui
                                    .add(egui::Button::new("PolyF").selected(ws.polyframe).min_size(vec2(w, 20.0)))
                                    .on_hover_text("Polygroup colours · Shift + F")
                                    .on_disabled_hover_text("Needs a GPU with primitive-index support")
                                    .clicked()
                                {
                                    ws.polyframe = !ws.polyframe;
                                }
                            });
                        });
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "initialize",
                    "Initialize",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        use crate::primitive::Primitive;
                        // Three fixed cells per row, like the SubTool buttons.
                        for row in Primitive::ALL.chunks(3) {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                            let w = (r.width() - 6.0) / 3.0;
                            for (col, &kind) in row.iter().enumerate() {
                                let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                                child.spacing_mut().button_padding = vec2(2.0, 1.0);
                                let hint = format!(
                                    "{} vertices at {} segments",
                                    crate::fmt_count(kind.vertex_count(m.primitive.segments[kind.index()])),
                                    m.primitive.segments[kind.index()]
                                );
                                if child
                                    .put(
                                        cell,
                                        egui::Button::new(RichText::new(kind.name()).size(11.0))
                                            .truncate()
                                            .selected(m.primitive.kind == kind),
                                    )
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    m.primitive.kind = kind;
                                }
                            }
                        }
                        let kind = m.primitive.kind;
                        let (lo, hi) = kind.segment_range();
                        let mut s = m.primitive.current() as f32;
                        if zslider(ui, "Segments", &mut s, lo as f32..=hi as f32, true, false, 0) {
                            m.primitive.set_current(s.round() as u32);
                        }
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            for (label, segments, hint) in [
                                ("Low", kind.default_segments(), "About 4 000 vertices, ZBrush's starting resolution"),
                                (
                                    "Dense",
                                    kind.dense_segments(),
                                    "About a million vertices (Sphere3D: the 960k sphere, --segments 400)",
                                ),
                            ] {
                                let on = m.primitive.current() == segments;
                                if ui
                                    .add(egui::Button::new(label).selected(on).min_size(vec2(w, 20.0)))
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    m.primitive.set_current(segments);
                                }
                            }
                        });
                        kv(ui, "Vertices", &crate::fmt_count(m.primitive.vertex_count()));
                        if !kind.closed() {
                            note(ui, "Open surface: its border stays put when divided; seen from the front only.");
                        }
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Rebuild"))
                            .on_hover_text("Replace the active subtool with this primitive, in place · clears its levels and undo history")
                            .clicked()
                        {
                            a.rebuild = true;
                        }
                        note(ui, "New (Document) and Append (SubTool) also start from this primitive.");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "chunks",
                    "Chunks",
                    None,
                    false,
                    true,
                    &mut |_, ui| {
                        kv(ui, "Chunks", &crate::fmt_count(m.chunks));
                        kv(ui, "Target size", "4 096 v");
                    },
                );
            }
            PaletteId::Performance => {
                self.sec(
                    ui,
                    h,
                    id,
                    "frame",
                    "Frame",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        kv(
                            ui,
                            "Frame",
                            &format!("{:.1} ms · {:.0} fps", m.frame_ms, m.fps),
                        );
                        kv(ui, "Stroke", &format!("{:.2} ms", m.stroke_ms));
                        kv(
                            ui,
                            "Upload / submit",
                            &format!("{:.2} / {:.2} ms", m.upload_ms, m.render_ms),
                        );
                    },
                );
                self.sec(ui, h, id, "gpu", "GPU", None, true, true, &mut |_, ui| {
                    kv(ui, "Device", m.adapter);
                    kv(ui, "Chunks", &crate::fmt_count(m.chunks));
                    kv(ui, "Levels on GPU", &format!("{} MB", m.gpu_level_bytes / 1_000_000));
                });
            }
            PaletteId::Preferences => {
                self.sec(
                    ui,
                    h,
                    id,
                    "ui",
                    "Interface",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        zslider(
                            ui,
                            "Tray width",
                            &mut ws.layout.tray_width,
                            200.0..=420.0,
                            false,
                            false,
                            0,
                        );
                        ui.add_enabled_ui(false, |ui| {
                            let mut scale = 1.0;
                            zslider(ui, "UI scale", &mut scale, 0.8..=2.0, false, false, 1);
                        });
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "keys",
                    "Shortcuts",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        for (key, what) in [
                            ("Drag model", "sculpt · Alt: alternate · Shift: smooth"),
                            ("Drag canvas or border", "rotate · Shift: snap to axis"),
                            ("Alt + drag canvas", "move"),
                            ("Alt + click, release Alt", "zoom"),
                            ("Shift + click, release Shift", "roll"),
                            ("RMB / MMB", "rotate · Alt: move · Ctrl: zoom"),
                            ("Scroll", "zoom to cursor · Ctrl: radius"),
                            ("Space (hold)", "quick menu · also: right-click tap"),
                            ("Alt + click subtool", "select subtool"),
                            ("Ctrl + drag model", "mask · Alt: unmask"),
                            ("Ctrl + click model", "blur mask · Alt: sharpen"),
                            ("Ctrl + click / drag bg", "invert / clear mask"),
                            ("Ctrl + I / A / Shift+A", "invert / mask all / clear"),
                            ("Ctrl + H", "view mask"),
                            ("Ctrl + Shift + drag", "keep the rectangle visible · Alt: hide it"),
                            ("Ctrl + Shift + click group", "isolate the polygroup · Alt: hide it"),
                            ("Ctrl + Shift + click bg", "show all"),
                            ("Ctrl + Shift + drag bg", "invert visibility (a rectangle around nothing)"),
                            ("Ctrl + W", "group masked, else visible: a new polygroup"),
                            ("Shift + F", "polygroup colours (PolyF)"),
                            ("Ctrl + drag bg, Wedge on", "re-wedge"),
                            ("Ctrl + S / Ctrl + O", "save / open project"),
                            ("S / U", "draw size / Z intensity popup"),
                            ("I / O", "Rgb intensity / focal shift"),
                            ("[ ]", "brush radius"),
                            ("1 – 5", "brushes"),
                            ("Q W E R", "draw / move / scale / rotate"),
                            ("W E R + drag model", "action line · Shift: 45°"),
                            ("Drag circle", "middle: move · end: scale / rotate"),
                            ("Drag ring / line", "move the action line"),
                            ("X", "symmetry"),
                            ("Ctrl + D", "divide (add a subdivision level)"),
                            ("D / Shift + D", "higher / lower subdivision level"),
                            ("F / Home", "frame mesh / reset view"),
                            ("P / Shift + P", "perspective / floor grid"),
                            ("Tab", "hide interface"),
                        ] {
                            kv(ui, key, what);
                        }
                    },
                );
            }
            PaletteId::Stroke => {
                self.sec(
                    ui,
                    h,
                    id,
                    "type",
                    "Stroke type",
                    Some(NC),
                    true,
                    false,
                    &mut |_, ui| {
                        seg(ui, &["Freehand", "Dots", "Drag rect"], 0, false);
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "mods",
                    "Modifiers",
                    Some(NC),
                    true,
                    false,
                    &mut |_, ui| {
                        let (mut spacing, mut stab) = (0.12, 0.0);
                        zslider(ui, "Spacing", &mut spacing, 0.0..=1.0, false, false, 2);
                        zslider(ui, "Stabilizer", &mut stab, 0.0..=1.0, false, false, 2);
                        note(ui, "Pen pressure: Brush › Tablet pressure");
                    },
                );
            }
            PaletteId::Symmetry => {
                self.sec(
                    ui,
                    h,
                    id,
                    "axes",
                    "Axes",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        let (mut y, mut z, mut radial, mut count) = (false, false, false, 8.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            ui.checkbox(&mut ws.symmetry, "X").on_hover_text("X");
                            ui.add_enabled_ui(false, |ui| {
                                ui.checkbox(&mut y, "Y");
                                ui.checkbox(&mut z, "Z");
                            });
                        });
                        ui.add_enabled_ui(false, |ui| {
                            ui.checkbox(&mut radial, "Radial");
                            zslider(ui, "Radial count", &mut count, 2.0..=24.0, false, false, 0);
                        });
                        note(ui, "Y, Z and radial are not connected");
                    },
                );
            }
            PaletteId::Transform => {
                self.sec(
                    ui,
                    h,
                    id,
                    "xf",
                    "Transform",
                    Some(NC),
                    true,
                    false,
                    &mut |_, ui| {
                        seg(ui, &["Move", "Rotate", "Scale"], 0, false);
                        full_button(ui, "Reset transform");
                    },
                );
            }
            PaletteId::View => {
                self.sec(
                    ui,
                    h,
                    id,
                    "cam",
                    "Camera",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.checkbox(&mut ws.perspective, "Perspective");
                        ui.add_enabled_ui(false, |ui| {
                            let mut fov = 45.0;
                            zslider(ui, "Field of view", &mut fov, 20.0..=90.0, false, false, 0);
                        });
                        if ui
                            .add_sized(
                                [ui.available_width(), 20.0],
                                egui::Button::new("Frame mesh"),
                            )
                            .clicked()
                        {
                            a.frame = true;
                        }
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "disp",
                    "Display",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.checkbox(&mut ws.grid, "Floor grid");
                        ui.checkbox(&mut ws.wireframe, "Surface detail shading");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "views",
                    "Views",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 9.0) * 0.25;
                            for (label, view) in [
                                ("Front", (0.0, 0.0)),
                                ("Side", (std::f32::consts::FRAC_PI_2, 0.0)),
                                ("Top", (0.0, 1.55)),
                                ("Iso", (0.6, 0.35)),
                            ] {
                                if ui
                                    .add(egui::Button::new(label).min_size(vec2(w, 20.0)))
                                    .clicked()
                                {
                                    a.view = Some(view);
                                }
                            }
                        });
                    },
                );
            }
            PaletteId::Workspace => {
                self.sec(
                    ui,
                    h,
                    id,
                    "layout",
                    "Layout",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 6.0) / 3.0;
                            if ui
                                .add(egui::Button::new("Reset").min_size(vec2(w, 20.0)))
                                .clicked()
                            {
                                ws.layout = DockLayout::default();
                                ws.flash(ui.ctx(), "Layout reset".to_string());
                            }
                            if ui
                                .add(egui::Button::new("Collapse all").min_size(vec2(w, 20.0)))
                                .clicked()
                            {
                                for p in &mut ws.layout.palettes {
                                    p.open = false;
                                }
                            }
                            if ui
                                .add(egui::Button::new("Expand all").min_size(vec2(w, 20.0)))
                                .clicked()
                            {
                                for p in &mut ws.layout.palettes {
                                    p.open = true;
                                }
                                ws.layout.sections.clear();
                            }
                        });
                        if ui
                            .add_sized(
                                [ui.available_width(), 20.0],
                                egui::Button::new("Hide interface (Tab)"),
                            )
                            .clicked()
                        {
                            ws.set_focus(true);
                        }
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "trays",
                    "Trays",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut ws.layout.left_tray, "Left tray");
                            ui.checkbox(&mut ws.layout.right_tray, "Right tray");
                        });
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "docked",
                    "Palettes",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        for pid in PaletteId::ALL {
                            let place = ws.layout.palette(pid).place;
                            let (r, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 22.0),
                                Sense::click(),
                            );
                            let p = ui.painter();
                            if resp.hovered() {
                                p.rect_filled(r, 2, BG2);
                            }
                            p.hline(r.x_range(), r.bottom() - 0.5, (1.0, LINE2));
                            p.circle_stroke(
                                r.left_center() + vec2(8.0, 0.0),
                                4.0,
                                Stroke::new(1.1, FG3),
                            );
                            p.text(
                                r.left_center() + vec2(20.0, 0.0),
                                Align2::LEFT_CENTER,
                                pid.name(),
                                FontId::proportional(12.0),
                                FG,
                            );
                            tag_colored(
                                p,
                                r.right_center() - vec2(2.0, 0.0),
                                place.name(),
                                place != Place::Menu,
                            );
                            ws.dock_menu(&resp, pid);
                        }
                        note(ui, "Click a palette to choose its tray.");
                    },
                );
            }
        }
    }

    fn material_swatches(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
            for (i, (_, name)) in MATERIALS.iter().enumerate() {
                let tex = self.material_texture(ui.ctx(), i);
                let (r, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                let p = ui.painter();
                let on = self.material == i;
                p.rect_filled(
                    r,
                    3,
                    if on {
                        ACC_DIM
                    } else if resp.hovered() {
                        BG4
                    } else {
                        BG2
                    },
                );
                p.rect_stroke(
                    r,
                    3,
                    Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
                    StrokeKind::Inside,
                );
                p.image(
                    tex,
                    r.shrink(3.0),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                if resp.on_hover_text(*name).clicked() {
                    self.material = i;
                }
            }
        });
    }

    // ------------------------------------------------------------ canvas
    fn canvas_overlays(&mut self, ui: &mut Ui, m: &Model<'_>, a: &mut Actions) {
        if let Some((r, hide)) = self.select_rect {
            // ZBrush SelectRect: green keeps what it covers, red (Alt) hides it.
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("select-rect")));
            let (fill, line) = if hide {
                (Color32::from_rgba_unmultiplied(220, 60, 50, 40), Color32::from_rgb(235, 80, 65))
            } else {
                (Color32::from_rgba_unmultiplied(70, 200, 90, 40), Color32::from_rgb(90, 220, 110))
            };
            p.rect_filled(r, 0, fill);
            p.rect_stroke(r, 0, Stroke::new(1.0, line), StrokeKind::Inside);
        }
        let canvas = self.canvas;
        let ctx = ui.ctx().clone();
        let painter = ui.painter().with_clip_rect(canvas);
        // document frame
        painter.rect_stroke(
            canvas.shrink(FRAME_INSET),
            0,
            Stroke::new(1.0, Color32::from_white_alpha(28)),
            StrokeKind::Inside,
        );
        egui::Area::new(Id::unique("canvas-view"))
            .fixed_pos(canvas.min + vec2(8.0, 8.0))
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let projection = if self.perspective {
                        "Perspective"
                    } else {
                        "Orthographic"
                    };
                    let resp = ui.add(
                        egui::Button::new(RichText::new(projection).size(11.0).color(FG2))
                            .min_size(vec2(0.0, 18.0)),
                    );
                    egui::Popup::menu(&resp).frame(popup_frame()).show(|ui| {
                        for (label, yaw, pitch) in [
                            ("Default view", 0.6, 0.35),
                            ("Front", 0.0, 0.0),
                            ("Side", std::f32::consts::FRAC_PI_2, 0.0),
                            ("Top", 0.0, std::f32::consts::FRAC_PI_2),
                        ] {
                            if menu_item(ui, label, false).clicked() {
                                a.view = Some((yaw, pitch));
                            }
                        }
                    });
                    let active = m.subtools.iter().find(|r| r.active).map_or("", |r| r.name.as_str());
                    ui.label(RichText::new(format!("/  {active}")).size(11.0).color(FG3));
                });
            });
        if !self.focus {
            egui::Area::new(Id::unique("view-gizmo"))
                .fixed_pos(pos2(canvas.right() - 76.0, canvas.top() + 10.0))
                .show(&ctx, |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(60.0, 60.0), Sense::hover());
                    let center = r.center();
                    // Project the world axes with the live camera; draw back-facing ones
                    // first and dimmer.
                    let mut axes = [
                        ("X", ACC, m.axes[0], (std::f32::consts::FRAC_PI_2, 0.0)),
                        ("Y", FG2, m.axes[1], (0.0, std::f32::consts::FRAC_PI_2)),
                        ("Z", Color32::from_rgb(122, 169, 214), m.axes[2], (0.0, 0.0)),
                    ];
                    axes.sort_by(|a, b| a.2[2].total_cmp(&b.2[2]));
                    for (label, color, [x, y, depth], view) in axes {
                        let offset = vec2(x, -y) * 24.0;
                        let end = center + offset;
                        let dim = if depth < 0.0 { 0.45 } else { 1.0 };
                        ui.painter()
                            .line_segment([center, end], Stroke::new(1.2, FG2.gamma_multiply(dim)));
                        let label_pos = if offset.length() > 1.0 {
                            end + offset.normalized() * 7.0
                        } else {
                            end
                        };
                        ui.painter().text(
                            label_pos,
                            Align2::CENTER_CENTER,
                            label,
                            FontId::monospace(10.0),
                            color.gamma_multiply(dim),
                        );
                        if ui
                            .interact(
                                Rect::from_center_size(label_pos, vec2(22.0, 22.0)),
                                Id::unique(("axis", label)),
                                Sense::click(),
                            )
                            .clicked()
                        {
                            a.view = Some(view);
                        }
                    }
                });
        }
        painter.text(
            pos2(canvas.left() + 10.0, canvas.bottom() - 12.0),
            Align2::LEFT_CENTER,
            format!(
                "Brush {} · r {:.2} · s {:.2}",
                m.brush.kind.name(),
                m.brush.radius,
                m.brush.strength
            ),
            FontId::proportional(11.0),
            FG3,
        );
        if self.focus {
            painter.text(
                pos2(canvas.right() - 10.0, canvas.top() + 14.0),
                Align2::RIGHT_CENTER,
                "Tab or Esc restores the interface",
                FontId::proportional(11.0),
                FG3,
            );
        }
        if let Some(error) = &self.persistence_error {
            painter.text(
                canvas.center_top() + vec2(0.0, 40.0),
                Align2::CENTER_TOP,
                error,
                FontId::proportional(11.0),
                ACC,
            );
        }
    }

    fn drop_targets(&mut self, ui: &mut Ui) {
        let Some(id) = egui::DragAndDrop::payload::<PaletteId>(ui.ctx()) else {
            return;
        };
        let c = self.canvas;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            Id::unique("dock-targets"),
        ));
        for side in Side::ALL {
            let zone = match side {
                Side::Top => Rect::from_min_max(c.min, pos2(c.right(), c.top() + 56.0)),
                Side::Bottom => Rect::from_min_max(pos2(c.left(), c.bottom() - 56.0), c.max),
                Side::Left => Rect::from_min_max(
                    pos2(c.left(), c.top() + 56.0),
                    pos2(c.left() + 56.0, c.bottom() - 56.0),
                ),
                Side::Right => Rect::from_min_max(
                    pos2(c.right() - 56.0, c.top() + 56.0),
                    pos2(c.right(), c.bottom() - 56.0),
                ),
            };
            let hovered = pointer.is_some_and(|p| zone.contains(p));
            painter.rect_filled(
                zone,
                0,
                if hovered {
                    Color32::from_rgba_unmultiplied(227, 131, 79, 90)
                } else {
                    Color32::from_rgba_unmultiplied(227, 131, 79, 30)
                },
            );
            painter.rect_stroke(zone, 0, Stroke::new(1.0, ACC_LINE), StrokeKind::Inside);
            painter.text(
                zone.center(),
                Align2::CENTER_CENTER,
                format!("DOCK {}", side.name().to_uppercase()),
                FontId::proportional(10.5),
                ACC,
            );
            if hovered && ui.input(|i| i.pointer.any_released()) {
                egui::DragAndDrop::take_payload::<PaletteId>(ui.ctx());
                self.pending_move = Some((*id, Place::Tray(side), None));
            }
        }
        if let Some(pos) = pointer {
            let galley =
                painter.layout_no_wrap(id.name().to_string(), FontId::proportional(12.0), FG);
            let rect =
                Rect::from_min_size(pos + vec2(10.0, -12.0), galley.size() + vec2(16.0, 8.0));
            painter.rect_filled(rect, 2, BG3);
            painter.rect_stroke(rect, 2, Stroke::new(1.0, ACC_LINE), StrokeKind::Inside);
            painter.galley(rect.min + vec2(8.0, 4.0), galley, FG);
        }
    }

    // ------------------------------------------------------------ previews
    fn texture(&mut self, ctx: &Context, kind: Option<BrushKind>, sub: bool) -> TextureId {
        let kind_index = kind
            .map(|k| BrushKind::ALL.iter().position(|x| *x == k).unwrap())
            .unwrap_or(usize::MAX);
        let flips = matches!(kind, Some(BrushKind::Draw | BrushKind::Inflate));
        let key = (kind_index, self.material, sub && flips);
        let material = self.material;
        self.textures
            .entry(key)
            .or_insert_with(|| {
                ctx.load_texture(
                    format!("preview-{kind_index}-{material}-{}", sub && flips),
                    sphere_image(kind, material, sub && flips),
                    TextureOptions::LINEAR,
                )
            })
            .id()
    }

    fn material_texture(&mut self, ctx: &Context, material: usize) -> TextureId {
        let key = (usize::MAX, material, false);
        self.textures
            .entry(key)
            .or_insert_with(|| {
                ctx.load_texture(
                    format!("material-{material}"),
                    sphere_image(None, material, false),
                    TextureOptions::LINEAR,
                )
            })
            .id()
    }
}

/// A sphere in `material` with one stroke of `kind` on it (none = plain sphere).
/// Matches the renderer's clay look closely enough to read as "what this brush does".
pub fn sphere_image(kind: Option<BrushKind>, material: usize, sub: bool) -> ColorImage {
    use std::f32::consts::PI;
    let n = 96usize;
    let rs = 0.86f32;
    let (color, _) = MATERIALS[material % MATERIALS.len()];
    let base = [
        color.r() as f32 / 255.0 * 1.02,
        color.g() as f32 / 255.0 * 1.02,
        color.b() as f32 / 255.0 * 1.02,
    ];
    let spec_k = if material == 1 || material == 3 {
        0.9
    } else {
        0.35
    };
    let sgn = if sub { -1.0 } else { 1.0 };
    let path: Vec<(f32, f32)> = (0..=40)
        .map(|i| {
            let t = i as f32 / 40.0;
            (-0.46 + 0.92 * t, 0.26 - 0.5 * t + 0.14 * (t * PI).sin())
        })
        .collect();
    let noise = |x: f32, y: f32| {
        0.55 * ((x * 9.1 + 1.3).sin() * (y * 8.7 + 0.4).sin()
            + 0.6 * (x * 17.0 + y * 13.0 + 2.0).sin() * (y * 15.0 - x * 7.0).sin()
            + 0.4 * ((x + y) * 23.0).sin() * ((x - y) * 19.0).sin())
    };
    let mut d = vec![0f32; n * n];
    if let Some(kind) = kind {
        for py in 0..n {
            for px in 0..n {
                let x = (px as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let y = (py as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let r2 = x * x + y * y;
                if r2 >= rs * rs {
                    continue;
                }
                let nz = (rs * rs - r2).sqrt() / rs;
                let dist = path
                    .iter()
                    .map(|(ax, ay)| ((x - ax).powi(2) + (y - ay).powi(2)).sqrt())
                    .fold(9.0f32, f32::min);
                let rr = if kind == BrushKind::Inflate {
                    0.34
                } else {
                    0.21
                };
                let f = (1.0 - dist / rr).max(0.0);
                let f = f * f * (3.0 - 2.0 * f);
                let h = match kind {
                    BrushKind::Draw => f * 0.16 * sgn,
                    BrushKind::Inflate => f.powf(0.7) * 0.2 * sgn,
                    BrushKind::Smooth => noise(x, y) * 0.11 * (1.0 - f * 0.97),
                    BrushKind::Flatten => noise(x, y) * 0.11 * (1.0 - f) - f * 0.03,
                    BrushKind::Grab => {
                        let (dx, dy) = (x - 0.02, y + 0.02);
                        let a = if dx > 0.0 { 0.5 } else { 0.24 };
                        0.22 * (-1.4 * (dx * dx / (a * a) + dy * dy / (0.26 * 0.26))).exp()
                    }
                };
                d[py * n + px] = h * nz;
            }
        }
    }
    let light = {
        let l = [-0.42f32, -0.5, 0.76];
        let len = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        [l[0] / len, l[1] / len, l[2] / len]
    };
    let half = {
        let h = [light[0], light[1], light[2] + 1.0];
        let len = (h[0] * h[0] + h[1] * h[1] + h[2] * h[2]).sqrt();
        [h[0] / len, h[1] / len, h[2] / len]
    };
    let at = |x: isize, y: isize| {
        d[(y.clamp(0, n as isize - 1) as usize) * n + x.clamp(0, n as isize - 1) as usize]
    };
    let mut rgba = vec![0u8; n * n * 4];
    for py in 0..n {
        for px in 0..n {
            let x = (px as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let y = (py as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let r = (x * x + y * y).sqrt();
            let edge = (rs - r) * n as f32 / 2.0 + 0.5;
            let o = (py * n + px) * 4;
            if edge <= 0.0 {
                continue;
            }
            let rc = r.min(rs - 0.001);
            let z = (rs * rs - rc * rc).sqrt();
            let (ix, iy) = (px as isize, py as isize);
            let gx = (at(ix + 1, iy) - at(ix - 1, iy)) * n as f32 * 0.5;
            let gy = (at(ix, iy + 1) - at(ix, iy - 1)) * n as f32 * 0.5;
            let (mut nx, mut ny, mut nz) = (x / rs - gx * 0.5, y / rs - gy * 0.5, z / rs);
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            nx /= len;
            ny /= len;
            nz /= len;
            let diff = (nx * light[0] + ny * light[1] + nz * light[2]).max(0.0);
            let spec = (nx * half[0] + ny * half[1] + nz * half[2])
                .max(0.0)
                .powf(36.0)
                * spec_k;
            let fill = 0.16 + 0.1 * (-ny).max(0.0);
            for k in 0..3 {
                rgba[o + k] = (255.0 * (base[k] * (fill + 0.9 * diff) + spec)).min(255.0) as u8;
            }
            rgba[o + 3] = (255.0 * edge.min(1.0)).round() as u8;
        }
    }
    ColorImage::from_rgba_unmultiplied([n, n], &rgba)
}

// ---------------------------------------------------------------- widgets
/// ZBrush-style slider: the label and value sit inside the bar. `line` draws the
/// shelf variant (dark field with a thin value line underneath).
pub fn zslider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
    line: bool,
    decimals: usize,
) -> bool {
    let (min, max) = (*range.start(), *range.end());
    let (r, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
            let v = if logarithmic {
                min * (max / min).powf(f)
            } else {
                min + f * (max - min)
            };
            if v != *value {
                *value = v;
                changed = true;
            }
        }
    }
    let frac = if logarithmic {
        ((*value / min).ln() / (max / min).ln()).clamp(0.0, 1.0)
    } else {
        ((*value - min) / (max - min)).clamp(0.0, 1.0)
    };
    let enabled = ui.is_enabled();
    let alpha = if enabled { 1.0 } else { 0.42 };
    let p = ui.painter();
    p.rect_filled(r, 2, (if line { BG2 } else { BG3 }).gamma_multiply(alpha));
    if !line {
        p.hline(r.x_range(), r.top() + 1.5, (1.0, HI));
    }
    p.rect_stroke(r, 2, Stroke::new(1.0, LINE), StrokeKind::Inside);
    if line {
        let fill = Rect::from_min_max(
            pos2(r.left() + 1.0, r.bottom() - 4.0),
            pos2(r.left() + 1.0 + frac * (r.width() - 2.0), r.bottom() - 1.0),
        );
        p.rect_filled(fill, 0, ACC.gamma_multiply(alpha));
    } else {
        let fill = Rect::from_min_max(
            r.min + vec2(1.0, 1.0),
            pos2(r.left() + 1.0 + frac * (r.width() - 2.0), r.bottom() - 1.0),
        );
        p.rect_filled(fill, 0, ACC_DIM.gamma_multiply(alpha));
        p.vline(
            fill.right(),
            fill.y_range(),
            (1.0, ACC_LINE.gamma_multiply(alpha)),
        );
    }
    let ty = if line {
        r.center().y - 2.0
    } else {
        r.center().y
    };
    p.text(
        pos2(r.left() + 6.0, ty),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        FG.gamma_multiply(alpha),
    );
    p.text(
        pos2(r.right() - 6.0, ty),
        Align2::RIGHT_CENTER,
        format!("{:.*}", decimals, *value),
        FontId::monospace(11.0),
        (if line || resp.hovered() { FG } else { FG2 }).gamma_multiply(alpha),
    );
    changed
}

/// Segmented buttons. Returns the index clicked this frame.
fn seg(ui: &mut Ui, labels: &[&str], selected: usize, tall: bool) -> Option<usize> {
    let h = if tall { 30.0 } else { 20.0 };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let enabled = ui.is_enabled();
    let alpha = if enabled { 1.0 } else { 0.42 };
    let w = r.width() / labels.len() as f32;
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let cell = Rect::from_min_size(r.min + vec2(w * i as f32, 0.0), vec2(w, h));
        let resp = ui.interact(cell, ui.scope_id().with(("seg", label, i)), Sense::click());
        let on = i == selected;
        let p = ui.painter();
        let fill = if on {
            ACC
        } else if resp.hovered() {
            BG4
        } else {
            BG3
        };
        p.rect_filled(cell, 0, fill.gamma_multiply(alpha));
        if !on {
            p.hline(cell.x_range(), cell.top() + 0.5, (1.0, HI));
        }
        p.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(12.0),
            (if on && enabled { ACC_FG } else { FG }).gamma_multiply(alpha),
        );
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    let p = ui.painter();
    p.rect_stroke(r, 2, Stroke::new(1.0, LINE), StrokeKind::Inside);
    for i in 1..labels.len() {
        p.vline(r.left() + w * i as f32, r.y_range(), (1.0, LINE));
    }
    clicked
}

fn small_toggle(ui: &mut Ui, label: &str, on: bool, size: Vec2) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        2,
        if on {
            ACC_DIM
        } else if resp.hovered() {
            BG4
        } else {
            BG3
        },
    );
    p.rect_stroke(
        r,
        2,
        Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
        StrokeKind::Inside,
    );
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.0),
        FG,
    );
    resp
}

/// Square icon-over-label button (shelf modes and navigation).
fn big_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    size: Vec2,
    on: bool,
    enabled: bool,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let alpha = if enabled { 1.0 } else { 0.4 };
    let p = ui.painter();
    let fill = if on && size.y > 42.0 {
        ACC
    } else if on {
        ACC_DIM
    } else if resp.hovered() && enabled {
        BG4
    } else {
        BG3
    };
    p.rect_filled(r, 3, fill.gamma_multiply(alpha));
    let border = if on && size.y <= 42.0 { ACC_LINE } else { LINE };
    p.rect_stroke(
        r,
        3,
        Stroke::new(1.0, border.gamma_multiply(alpha)),
        StrokeKind::Inside,
    );
    if !(on && size.y > 42.0) {
        p.hline(r.x_range().shrink(1.0), r.top() + 1.5, (1.0, HI));
    }
    let color = if on && size.y > 42.0 {
        ACC_FG
    } else if on || (resp.hovered() && enabled) {
        FG
    } else {
        FG2
    }
    .gamma_multiply(alpha);
    glyph(p, r.center() - vec2(0.0, 7.0), icon, color);
    p.text(
        r.center_bottom() - vec2(0.0, 9.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(if size.y > 42.0 { 10.0 } else { 9.5 }),
        color,
    );
    resp
}

enum Thumb {
    Texture(TextureId),
    Glyph(&'static str),
}

/// Left-shelf thumbnail: 84px image with a label below.
fn thumb(ui: &mut Ui, content: Thumb, label: &str, on: bool, dim: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(86.0, 100.0), Sense::click());
    let im = Rect::from_min_size(r.min + vec2(1.0, 0.0), vec2(84.0, 84.0));
    let p = ui.painter();
    p.rect_filled(im, 3, BG2);
    p.hline(im.x_range().shrink(1.0), im.top() + 1.5, (1.0, HI));
    match content {
        Thumb::Texture(tex) => {
            p.image(
                tex,
                im.shrink(1.0),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        Thumb::Glyph(name) => glyph_large(p, im.center(), name, if dim { FG3 } else { FG2 }),
    }
    p.rect_stroke(
        im,
        3,
        Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
        StrokeKind::Inside,
    );
    if on {
        p.rect_stroke(
            im.expand(1.0),
            4,
            Stroke::new(1.0, ACC_DIM),
            StrokeKind::Outside,
        );
    }
    p.text(
        pos2(r.center().x, r.bottom() - 7.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        if dim {
            FG3
        } else if resp.hovered() {
            FG
        } else {
            FG2
        },
    );
    resp
}

fn brush_tile(
    ui: &mut Ui,
    tex: TextureId,
    name: &str,
    key: usize,
    on: bool,
    width: f32,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(width, width + 18.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        3,
        if on {
            ACC_DIM
        } else if resp.hovered() {
            BG4
        } else {
            BG3
        },
    );
    p.rect_stroke(
        r,
        3,
        Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
        StrokeKind::Inside,
    );
    let im = Rect::from_min_size(r.min + vec2(3.0, 3.0), vec2(width - 6.0, width - 6.0));
    p.rect_filled(im, 2, BG2);
    p.image(
        tex,
        im,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    p.rect_stroke(im, 2, Stroke::new(1.0, LINE), StrokeKind::Inside);
    p.text(
        im.right_top() + vec2(-5.0, 4.0),
        Align2::RIGHT_TOP,
        key.to_string(),
        FontId::monospace(10.0),
        Color32::WHITE,
    );
    p.text(
        pos2(r.center().x, r.bottom() - 9.0),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(11.0),
        if on || resp.hovered() { FG } else { FG2 },
    );
    resp.on_hover_text(format!("{name} · {key}"))
}

/// HSV picker: saturation/value square plus a hue strip.
fn color_picker(ui: &mut Ui, color: &mut Hsva) {
    let (r, _) = ui.allocate_exact_size(vec2(86.0, 72.0), Sense::hover());
    let sv = Rect::from_min_max(r.min, pos2(r.right() - 15.0, r.bottom()));
    let hue = Rect::from_min_max(pos2(r.right() - 12.0, r.top()), r.max);
    let sv_resp = ui.interact(sv, ui.scope_id().with("picker-sv"), Sense::click_and_drag());
    if sv_resp.dragged() || sv_resp.clicked() {
        if let Some(p) = sv_resp.interact_pointer_pos() {
            color.s = ((p.x - sv.left()) / sv.width()).clamp(0.0, 1.0);
            color.v = 1.0 - ((p.y - sv.top()) / sv.height()).clamp(0.0, 1.0);
        }
    }
    let hue_resp = ui.interact(
        hue,
        ui.scope_id().with("picker-hue"),
        Sense::click_and_drag(),
    );
    if hue_resp.dragged() || hue_resp.clicked() {
        if let Some(p) = hue_resp.interact_pointer_pos() {
            color.h = ((p.y - hue.top()) / hue.height()).clamp(0.0, 0.999);
        }
    }
    let p = ui.painter();
    // saturation/value square: bilinear corner colours
    let pure = Color32::from(Hsva::new(color.h, 1.0, 1.0, 1.0));
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(sv.left_top(), Color32::WHITE);
    mesh.colored_vertex(sv.right_top(), pure);
    mesh.colored_vertex(sv.right_bottom(), Color32::BLACK);
    mesh.colored_vertex(sv.left_bottom(), Color32::BLACK);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(mesh));
    p.rect_stroke(sv, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let marker = pos2(
        sv.left() + color.s * sv.width(),
        sv.top() + (1.0 - color.v) * sv.height(),
    );
    p.circle_stroke(marker, 4.5, Stroke::new(1.0, Color32::BLACK));
    p.circle_stroke(marker, 3.5, Stroke::new(1.5, Color32::WHITE));
    // hue strip
    let mut strip = egui::Mesh::default();
    let steps = 12;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let c = Color32::from(Hsva::new(t.min(0.999), 1.0, 1.0, 1.0));
        let y = hue.top() + t * hue.height();
        strip.colored_vertex(pos2(hue.left(), y), c);
        strip.colored_vertex(pos2(hue.right(), y), c);
        if i > 0 {
            let b = (i * 2) as u32;
            strip.add_triangle(b - 2, b - 1, b);
            strip.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(egui::Shape::mesh(strip));
    p.rect_stroke(hue, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let hy = hue.top() + color.h * hue.height();
    let hm = Rect::from_min_max(
        pos2(hue.left() - 2.0, hy - 2.0),
        pos2(hue.right() + 2.0, hy + 2.0),
    );
    p.rect_stroke(
        hm.expand(1.0),
        0,
        Stroke::new(1.0, Color32::BLACK),
        StrokeKind::Outside,
    );
    p.rect_stroke(hm, 0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
}

fn menu_item(ui: &mut Ui, label: &str, marked: bool) -> egui::Response {
    let (r, resp) =
        ui.allocate_exact_size(vec2(ui.available_width().max(140.0), 22.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 2, ACC);
    }
    let color = if resp.hovered() { ACC_FG } else { FG };
    if marked {
        p.circle_filled(r.left_center() + vec2(10.0, 0.0), 2.5, color);
    }
    p.text(
        r.left_center() + vec2(22.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        color,
    );
    resp
}

fn popup_frame() -> Frame {
    Frame::NONE
        .fill(BG2)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(3)
        .inner_margin(4)
        .shadow(egui::Shadow {
            offset: [0, 10],
            blur: 30,
            spread: 0,
            color: Color32::from_black_alpha(150),
        })
}

fn shelf_separator(ui: &mut Ui) {
    ui.add_space(5.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, SHELF_H), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), (1.0, LINE));
    ui.add_space(5.0);
}

fn nav_separator(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(36.0, 3.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, (1.0, LINE));
}

fn tag(p: &egui::Painter, right_center: Pos2, text: &str) {
    tag_colored(p, right_center, text, false);
}

fn tag_colored(p: &egui::Painter, right_center: Pos2, text: &str, accent: bool) {
    let galley = p.layout_no_wrap(text.to_uppercase(), FontId::proportional(9.5), FG3);
    let rect = Rect::from_min_max(
        right_center - vec2(galley.size().x + 10.0, 8.0),
        right_center + vec2(0.0, 8.0),
    );
    let color = if accent { ACC } else { FG3 };
    p.rect_stroke(
        rect,
        2,
        Stroke::new(1.0, if accent { ACC_LINE } else { LINE2 }),
        StrokeKind::Inside,
    );
    p.galley(
        rect.min + vec2(5.0, (16.0 - galley.size().y) * 0.5),
        galley,
        color,
    );
}

fn note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(FG3)).wrap());
}

fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.set_height(16.0);
        ui.label(RichText::new(key).size(12.0).color(FG2));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).font(FontId::monospace(11.0)).color(FG));
        });
    });
}

/// A SubTool list line: name and size, highlighted when active, with an eye toggle on the
/// right. Returns (row clicked, eye clicked).
fn subtool_row(ui: &mut Ui, index: usize, row: &SubToolRow) -> (bool, bool) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let eye_rect = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(22.0, 22.0));
    let eye = ui.interact(eye_rect, Id::unique(("subtool-eye", index)), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        2,
        if row.active {
            ACC_DIM
        } else if resp.hovered() {
            BG4
        } else {
            BG3
        },
    );
    p.rect_stroke(
        r,
        2,
        Stroke::new(1.0, if row.active { ACC_LINE } else { LINE }),
        StrokeKind::Inside,
    );
    let text = if row.visible || row.active { FG } else { FG3 };
    p.text(
        pos2(r.left() + 8.0, r.top() + 10.0),
        Align2::LEFT_CENTER,
        &row.name,
        FontId::proportional(12.0),
        text,
    );
    let detail = if !row.drawn {
        "stand-in did not fit on the GPU".to_string()
    } else if row.top > 0 {
        format!(
            "{} v · SDiv {} of {} · {} MB",
            crate::fmt_count(row.vertices),
            row.level,
            row.top,
            row.bytes / 1_000_000
        )
    } else {
        format!("{} v · {} MB", crate::fmt_count(row.vertices), row.bytes / 1_000_000)
    };
    p.text(
        pos2(r.left() + 8.0, r.bottom() - 9.0),
        Align2::LEFT_CENTER,
        detail,
        FontId::monospace(10.0),
        FG3,
    );
    // Eye: an outline with a pupil, crossed out when hidden.
    let c = eye_rect.center();
    let color = if row.active {
        FG3
    } else if eye.hovered() {
        FG
    } else {
        FG2
    };
    let pts: Vec<Pos2> = (0..=16)
        .map(|k| {
            let t = k as f32 / 16.0 * std::f32::consts::TAU;
            c + vec2(7.0 * t.cos(), 4.0 * t.sin())
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.2, color)));
    p.circle_filled(c, 1.8, color);
    if !row.visible && !row.active {
        p.line_segment([c + vec2(-7.0, 6.0), c + vec2(7.0, -6.0)], Stroke::new(1.4, color));
    }
    let eye_clicked = eye.clicked() && !row.active;
    let eye = eye.on_hover_text(if row.active {
        "The active subtool is always shown"
    } else if row.visible {
        "Hide"
    } else {
        "Show"
    });
    (resp.clicked() && !eye.hovered(), eye_clicked)
}

/// ZBrush-style deformation slider: drag sets a value in -100..100 from the centre, release
/// returns it and the slider springs back to 0.
fn spring_slider(ui: &mut Ui, kind: Deform, drag: &mut Option<(Deform, f32)>) -> Option<f32> {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::drag());
    let value_at = |p: Pos2| ((p.x - r.center().x) / (r.width() * 0.5) * 100.0).clamp(-100.0, 100.0);
    let mut released = None;
    if resp.drag_started() || resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            *drag = Some((kind, value_at(p).round()));
        }
    }
    if resp.drag_stopped() {
        if let Some((k, v)) = drag.take() {
            if k == kind {
                released = Some(v);
            }
        }
    }
    let value = match drag {
        Some((k, v)) if *k == kind => *v,
        _ => 0.0,
    };
    let p = ui.painter();
    p.rect_filled(r, 2, if resp.hovered() || value != 0.0 { BG4 } else { BG3 });
    let cx = r.center().x;
    let x = cx + value / 100.0 * r.width() * 0.5;
    if value != 0.0 {
        p.rect_filled(
            Rect::from_x_y_ranges(cx.min(x)..=cx.max(x), r.y_range()),
            2,
            ACC_DIM,
        );
    }
    p.vline(cx, r.y_range().shrink(4.0), Stroke::new(1.0, LINE2));
    p.text(
        pos2(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        kind.label(),
        FontId::proportional(11.5),
        FG,
    );
    p.text(
        pos2(r.right() - 6.0, r.center().y),
        Align2::RIGHT_CENTER,
        format!("{value:+.0}"),
        FontId::monospace(11.0),
        if value != 0.0 { ACC } else { FG3 },
    );
    released
}

fn full_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 20.0], egui::Button::new(label))
}

fn chevron(p: &egui::Painter, center: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![
            center + vec2(-3.5, -2.0),
            center + vec2(0.0, 2.5),
            center + vec2(3.5, -2.0),
        ]
    } else {
        vec![
            center + vec2(-2.0, -3.5),
            center + vec2(2.5, 0.0),
            center + vec2(-2.0, 3.5),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// 18px line icons, drawn with the painter so they tint with the button state.
fn glyph(p: &egui::Painter, c: Pos2, name: &str, color: Color32) {
    let s = Stroke::new(1.5, color);
    let line = |pts: &[Vec2]| p.add(egui::Shape::line(pts.iter().map(|v| c + *v).collect(), s));
    match name {
        "sculpt" => {
            line(&[
                vec2(-7.0, 5.0),
                vec2(-4.0, 3.0),
                vec2(-1.5, -4.0),
                vec2(0.0, -6.0),
                vec2(1.5, -4.0),
                vec2(4.0, 3.0),
                vec2(7.0, 5.0),
            ]);
        }
        "move" => {
            line(&[vec2(0.0, -7.0), vec2(0.0, 7.0)]);
            line(&[vec2(-7.0, 0.0), vec2(7.0, 0.0)]);
            line(&[vec2(-2.5, -4.5), vec2(0.0, -7.0), vec2(2.5, -4.5)]);
            line(&[vec2(-2.5, 4.5), vec2(0.0, 7.0), vec2(2.5, 4.5)]);
            line(&[vec2(-4.5, -2.5), vec2(-7.0, 0.0), vec2(-4.5, 2.5)]);
            line(&[vec2(4.5, -2.5), vec2(7.0, 0.0), vec2(4.5, 2.5)]);
        }
        "scale" => {
            p.rect_stroke(
                Rect::from_min_size(c + vec2(-7.0, -2.0), vec2(9.0, 9.0)),
                0,
                s,
                StrokeKind::Middle,
            );
            line(&[vec2(-1.0, -7.0), vec2(7.0, -7.0), vec2(7.0, 1.0)]);
            line(&[vec2(7.0, -7.0), vec2(1.0, -1.0)]);
        }
        "rotate" => {
            let pts: Vec<Vec2> = (0..20)
                .map(|i| {
                    let a = -2.2 + i as f32 / 19.0 * 5.2;
                    vec2(a.cos() * 6.0, a.sin() * 6.0)
                })
                .collect();
            line(&pts);
            line(&[vec2(1.0, -8.0), vec2(4.5, -4.5), vec2(0.5, -3.0)]);
        }
        "frame" => {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                line(&[
                    vec2(sx * 7.0, sy * 3.0),
                    vec2(sx * 7.0, sy * 7.0),
                    vec2(sx * 3.0, sy * 7.0),
                ]);
            }
            p.circle_stroke(c, 2.5, s);
        }
        "persp" => {
            line(&[
                vec2(-7.0, 6.0),
                vec2(-4.0, -6.0),
                vec2(4.0, -6.0),
                vec2(7.0, 6.0),
                vec2(-7.0, 6.0),
            ]);
            line(&[vec2(-2.5, -6.0), vec2(-4.0, 6.0)]);
            line(&[vec2(2.5, -6.0), vec2(4.0, 6.0)]);
        }
        "floor" => {
            line(&[
                vec2(-7.0, 5.0),
                vec2(-3.0, -4.0),
                vec2(3.0, -4.0),
                vec2(7.0, 5.0),
                vec2(-7.0, 5.0),
            ]);
            line(&[vec2(-5.0, 0.5), vec2(5.0, 0.5)]);
            line(&[vec2(0.0, -4.0), vec2(0.0, 5.0)]);
        }
        "sym" => {
            for y in [-7.0, -3.0, 1.0, 5.0] {
                line(&[vec2(0.0, y), vec2(0.0, y + 2.0)]);
            }
            line(&[vec2(-2.0, -4.0), vec2(-6.0, 0.0), vec2(-2.0, 4.0)]);
            line(&[vec2(2.0, -4.0), vec2(6.0, 0.0), vec2(2.0, 4.0)]);
        }
        "local" => {
            p.circle_stroke(c, 6.0, s);
            p.circle_filled(c, 1.5, color);
        }
        "transp" => {
            p.rect_stroke(
                Rect::from_min_size(c + vec2(-7.0, -7.0), vec2(9.0, 9.0)),
                0,
                s,
                StrokeKind::Middle,
            );
            p.rect_stroke(
                Rect::from_min_size(c + vec2(-2.0, -2.0), vec2(9.0, 9.0)),
                0,
                Stroke::new(1.5, color.gamma_multiply(0.5)),
                StrokeKind::Middle,
            );
        }
        "ghost" => {
            let pts: Vec<Vec2> = (0..13)
                .map(|i| {
                    let a = std::f32::consts::PI + i as f32 / 12.0 * std::f32::consts::PI;
                    vec2(a.cos() * 5.0, a.sin() * 5.0 - 1.0)
                })
                .collect();
            line(&pts);
            line(&[
                vec2(-5.0, -1.0),
                vec2(-5.0, 7.0),
                vec2(-2.5, 5.0),
                vec2(0.0, 7.0),
                vec2(2.5, 5.0),
                vec2(5.0, 7.0),
                vec2(5.0, -1.0),
            ]);
        }
        "solo" => {
            p.circle_stroke(c, 4.0, s);
            p.circle_stroke(
                c + vec2(-5.5, -5.0),
                1.2,
                Stroke::new(1.0, color.gamma_multiply(0.4)),
            );
            p.circle_stroke(
                c + vec2(5.5, 5.0),
                1.2,
                Stroke::new(1.0, color.gamma_multiply(0.4)),
            );
        }
        "polyf" => {
            // A quad grid: the polyframe.
            p.rect_stroke(Rect::from_center_size(c, vec2(14.0, 12.0)), 0, s, StrokeKind::Middle);
            line(&[vec2(0.0, -6.0), vec2(0.0, 6.0)]);
            line(&[vec2(-7.0, 0.0), vec2(7.0, 0.0)]);
        }
        "detail" => {
            let wave = |dy: f32, amp: f32, col: Color32| {
                let pts: Vec<Pos2> = (0..15)
                    .map(|i| {
                        let x = i as f32 - 7.0;
                        c + vec2(x, (x / 2.2).sin() * amp + dy)
                    })
                    .collect();
                p.add(egui::Shape::line(pts, Stroke::new(1.5, col)));
            };
            wave(3.0, 2.5, color);
            wave(-3.0, 1.5, color.gamma_multiply(0.5));
        }
        "hide" => {
            let pts: Vec<Vec2> = (0..13)
                .map(|i| {
                    let t = i as f32 / 12.0;
                    let x = -7.0 + 14.0 * t;
                    vec2(x, -((1.0 - (x / 7.0).powi(2)).max(0.0)).sqrt() * 4.0)
                })
                .collect();
            line(&pts);
            let pts: Vec<Vec2> = pts.iter().map(|v| vec2(v.x, -v.y)).collect();
            line(&pts);
            line(&[vec2(-6.0, 6.0), vec2(6.0, -6.0)]);
        }
        "swap" => {
            let s = Stroke::new(1.2, color);
            p.line_segment([c + vec2(-4.0, -2.0), c + vec2(2.0, -2.0)], s);
            p.add(egui::Shape::line(
                vec![c + vec2(0.0, -4.0), c + vec2(2.0, -2.0), c + vec2(0.0, 0.0)],
                s,
            ));
            p.line_segment([c + vec2(4.0, 2.0), c + vec2(-2.0, 2.0)], s);
            p.add(egui::Shape::line(
                vec![c + vec2(0.0, 0.0), c + vec2(-2.0, 2.0), c + vec2(0.0, 4.0)],
                s,
            ));
        }
        _ => {}
    }
}

/// 34px icons for the left-shelf thumbnails that have no image yet.
fn glyph_large(p: &egui::Painter, c: Pos2, name: &str, color: Color32) {
    let s = Stroke::new(2.0, color);
    match name {
        "stroke" => {
            let pts: Vec<Pos2> = (0..40)
                .map(|i| {
                    let t = i as f32 / 39.0;
                    let x = -14.0 + 28.0 * t;
                    c + vec2(x, (t * 9.0).sin() * 6.0 * (1.0 - t * 0.5) + (t - 0.5) * 6.0)
                })
                .collect();
            p.add(egui::Shape::line(pts, s));
        }
        "alpha" => {
            let r = Rect::from_center_size(c, vec2(24.0, 24.0));
            p.rect_stroke(r, 2, Stroke::new(1.4, color), StrokeKind::Middle);
            for i in 0..6 {
                let t = i as f32 / 5.0;
                p.circle_filled(r.left_bottom() + vec2(24.0 * t, -24.0 * t), 1.0, color);
            }
        }
        "texture" => {
            let r = Rect::from_center_size(c, vec2(24.0, 24.0));
            p.rect_stroke(r, 2, Stroke::new(1.4, color), StrokeKind::Middle);
            let faint = Stroke::new(1.0, color.gamma_multiply(0.6));
            p.hline(r.x_range(), c.y - 4.0, faint);
            p.hline(r.x_range(), c.y + 4.0, faint);
            p.vline(c.x - 4.0, r.y_range(), faint);
            p.vline(c.x + 4.0, r.y_range(), faint);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- theme
pub fn theme(ctx: &egui::Context) {
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG1;
    style.visuals.window_fill = BG2;
    style.visuals.extreme_bg_color = BG0;
    style.visuals.faint_bg_color = BG2;
    style.visuals.override_text_color = Some(FG);
    style.visuals.weak_text_color = Some(FG2);
    style.visuals.selection.bg_fill = ACC_DIM;
    style.visuals.selection.stroke = Stroke::new(1.0, ACC);
    style.visuals.window_stroke = Stroke::new(1.0, LINE);
    style.visuals.window_corner_radius = CornerRadius::same(3);
    style.visuals.menu_corner_radius = CornerRadius::same(3);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE2);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(2);
        widget.bg_stroke = Stroke::new(1.0, LINE);
        widget.fg_stroke = Stroke::new(1.0, FG);
        widget.expansion = 0.0;
    }
    style.visuals.widgets.inactive.bg_fill = BG3;
    style.visuals.widgets.inactive.weak_bg_fill = BG3;
    style.visuals.widgets.hovered.bg_fill = BG4;
    style.visuals.widgets.hovered.weak_bg_fill = BG4;
    style.visuals.widgets.active.bg_fill = ACC_DIM;
    style.visuals.widgets.active.weak_bg_fill = ACC_DIM;
    style.visuals.widgets.open.bg_fill = BG4;
    style.visuals.widgets.open.weak_bg_fill = BG4;
    style.spacing.item_spacing = vec2(3.0, 3.0);
    style.spacing.button_padding = vec2(9.0, 3.0);
    style.spacing.interact_size = vec2(20.0, 20.0);
    style.spacing.icon_width = 12.0;
    style.spacing.icon_width_inner = 8.0;
    style.spacing.icon_spacing = 7.0;
    style.spacing.menu_margin = Margin::same(3);
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(10.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(11.0));
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

// ---------------------------------------------------------------- tests
#[cfg(test)]
mod tests {
    use super::*;

    fn test_workspace(name: &str) -> (Workspace, egui::Context) {
        let mut workspace = Workspace::new();
        workspace.layout = DockLayout::default();
        workspace.saved = workspace.layout.encode();
        workspace.persistence_path =
            std::env::temp_dir().join(format!("klay-test-{}-{name}.layout", std::process::id()));
        let ctx = egui::Context::default();
        theme(&ctx);
        (workspace, ctx)
    }

    fn row(name: &str, active: bool) -> SubToolRow {
        SubToolRow {
            name: name.to_string(),
            visible: true,
            active,
            vertices: 240002,
            level: 0,
            top: 0,
            bytes: 23_000_000,
            drawn: true,
        }
    }

    fn frame(
        workspace: &mut Workspace,
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        frame_with(workspace, ctx, size, events, &[row("Sphere 1", true)])
    }

    fn frame_with(
        workspace: &mut Workspace,
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        subtools: &[SubToolRow],
    ) -> egui::FullOutput {
        let mut primitive = crate::primitive::Settings::default();
        frame_full(workspace, ctx, size, events, subtools, &mut primitive).0
    }

    fn frame_full(
        workspace: &mut Workspace,
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        subtools: &[SubToolRow],
        primitive: &mut crate::primitive::Settings,
    ) -> (egui::FullOutput, Actions) {
        let mut brush = Brush::default();
        let mut actions = Actions::default();
        let mut budget = 20_000_000usize;
        let mut path = String::from("test.obj");
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                actions = workspace.show(
                    ui,
                    &mut Model {
                        brush: &mut brush,
                        primitive,
                        export_path: &mut path,
                        status: "",
                        can_undo: false,
                        can_redo: false,
                        history_bytes: 0,
                        vertices: 240002,
                        triangles: 480000,
                        chunks: 128,
                        level: 0,
                        levels: &[240002],
                        next_level_vertices: 960002,
                        vertex_budget: &mut budget,
                        levels_bytes: 0,
                        gpu_level_bytes: 0,
                        fps: 60.0,
                        frame_ms: 16.7,
                        stroke_ms: 0.0,
                        upload_ms: 0.0,
                        render_ms: 0.0,
                        adapter: "Test adapter",
                        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                        subtools,
                        parked_bytes: 0,
                        stand_in_bytes: 0,
                        project_path: &mut String::from("untitled.klay"),
                        groups: 1,
                        hidden: 0,
                        polyframe_supported: true,
                        wedge: &mut crate::wedge::Settings::default(),
                        wedge_on: false,
                        wedge_refusal: None,
                        wedge_estimate: 75_000,
                        tablet: "UGTABLET 15.6 inch PenDisplay Pen (0)",
                        tablet_on: true,
                        pressure: Some(0.42),
                    },
                );
            },
        );
        (out, actions)
    }

    fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn text_position(output: &egui::FullOutput, text: &str) -> Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(shape) if shape.galley.job.text == text => Some(shape.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("text {text:?} was not rendered"))
    }

    fn text_count(output: &egui::FullOutput, text: &str) -> usize {
        output
            .shapes
            .iter()
            .filter(|shape| matches!(&shape.shape, egui::Shape::Text(s) if s.galley.job.text == text))
            .count()
    }

    #[test]
    fn quick_menu_shows_while_open_and_closes_on_a_press_elsewhere() {
        let (mut workspace, ctx) = test_workspace("quick-menu");
        let size = vec2(1600.0, 1000.0);
        let output = frame(&mut workspace, &ctx, size, vec![]);
        // Before: the shelf's (and the Tablet pressure section's Z Intensity toggle).
        let (size_before, z_before) = (text_count(&output, "Draw Size"), text_count(&output, "Z Intensity"));
        workspace.open_quick_menu(pos2(800.0, 500.0), false);
        // A new egui area spends its first frame in an invisible sizing pass.
        frame(&mut workspace, &ctx, size, vec![]);
        let output = frame(&mut workspace, &ctx, size, vec![]);
        assert_eq!(text_count(&output, "Draw Size"), size_before + 1);
        assert_eq!(text_count(&output, "Z Intensity"), z_before + 1);
        // A press far from the menu closes it; a held (Space) menu ignores that.
        frame(&mut workspace, &ctx, size, vec![pointer(pos2(400.0, 900.0), true)]);
        frame(&mut workspace, &ctx, size, vec![pointer(pos2(400.0, 900.0), false)]);
        assert!(workspace.quick_menu.is_none());
        workspace.open_quick_menu(pos2(800.0, 500.0), true);
        frame(&mut workspace, &ctx, size, vec![pointer(pos2(400.0, 900.0), true)]);
        assert!(workspace.quick_menu.is_some());
    }

    /// Centre of the ◉ dock button of the first palette in the left tray.
    const FIRST_DOCK_BUTTON: Pos2 = pos2(12.0, MENUBAR_H + SHELF_H + HEADER_H * 0.5);

    #[test]
    fn pen_style_drag_scrolls_a_tray_without_starting_docking() {
        let (mut workspace, ctx) = test_workspace("scroll");
        let size = vec2(1600.0, 1000.0);
        // Dock everything on the left so the tray overflows.
        for id in PaletteId::ALL {
            workspace
                .layout
                .move_palette(id, Place::Tray(Side::Left), None);
            workspace.layout.palette_mut(id).open = true;
        }
        let output = frame(&mut workspace, &ctx, size, vec![]);
        let before = text_position(&output, "Shortcuts").y;
        let start = pos2(259.0, 700.0);
        frame(
            &mut workspace,
            &ctx,
            size,
            vec![egui::Event::PointerMoved(start)],
        );
        frame(&mut workspace, &ctx, size, vec![pointer(start, true)]);
        for dy in [30.0, 70.0, 120.0, 180.0, 240.0] {
            frame(
                &mut workspace,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(start - vec2(0.0, dy))],
            );
        }
        let end = start - vec2(0.0, 240.0);
        let output = frame(&mut workspace, &ctx, size, vec![pointer(end, false)]);
        assert!(text_position(&output, "Shortcuts").y < before - 100.0);
        assert!(!egui::DragAndDrop::has_any_payload(&ctx));
        let _ = std::fs::remove_file(&workspace.persistence_path);
    }

    #[test]
    fn rendered_workspace_preserves_canvas_at_supported_window_sizes() {
        let (mut workspace, ctx) = test_workspace("sizes");
        for size in [
            vec2(1600.0, 1000.0),
            vec2(1280.0, 800.0),
            vec2(960.0, 640.0),
        ] {
            frame(&mut workspace, &ctx, size, vec![]);
            assert!(
                workspace.canvas.width() >= 250.0,
                "canvas too narrow at {size:?}: {:?}",
                workspace.canvas
            );
            assert!(
                workspace.canvas.height() >= 300.0,
                "canvas too short at {size:?}: {:?}",
                workspace.canvas
            );
            for p in &mut workspace.layout.palettes {
                p.open = true;
            }
            frame(&mut workspace, &ctx, size, vec![]);
            assert!(workspace.canvas.height() >= 180.0);
            workspace.focus = true;
            frame(&mut workspace, &ctx, size, vec![]);
            assert!(workspace.canvas.width() >= size.x - 2.0);
            workspace.focus = false;
            workspace.layout = DockLayout::default();
        }
        let _ = std::fs::remove_file(&workspace.persistence_path);
    }

    #[test]
    fn pointer_click_on_a_palette_title_collapses_it() {
        let (mut workspace, ctx) = test_workspace("collapse");
        let size = vec2(1600.0, 1000.0);
        let pos = pos2(80.0, FIRST_DOCK_BUTTON.y);
        frame(&mut workspace, &ctx, size, vec![]);
        assert!(workspace.layout.palette(PaletteId::Brush).open);
        frame(
            &mut workspace,
            &ctx,
            size,
            vec![egui::Event::PointerMoved(pos)],
        );
        frame(&mut workspace, &ctx, size, vec![pointer(pos, true)]);
        frame(&mut workspace, &ctx, size, vec![pointer(pos, false)]);
        frame(&mut workspace, &ctx, size, vec![]);
        assert!(!workspace.layout.palette(PaletteId::Brush).open);
        let _ = std::fs::remove_file(&workspace.persistence_path);
    }

    #[test]
    fn pointer_drag_of_the_dock_button_docks_to_all_four_canvas_edges() {
        for side in Side::ALL {
            let (mut workspace, ctx) = test_workspace(side.name());
            let size = vec2(1600.0, 1000.0);
            let start = FIRST_DOCK_BUTTON;
            frame(&mut workspace, &ctx, size, vec![]);
            frame(
                &mut workspace,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(start)],
            );
            frame(&mut workspace, &ctx, size, vec![pointer(start, true)]);
            frame(
                &mut workspace,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(start + vec2(40.0, 20.0))],
            );
            assert!(
                egui::DragAndDrop::has_payload_of_type::<PaletteId>(&ctx),
                "no payload for {side:?}"
            );
            let c = workspace.canvas;
            let target = match side {
                Side::Top => c.center_top() + vec2(0.0, 20.0),
                Side::Bottom => c.center_bottom() - vec2(0.0, 20.0),
                Side::Left => c.left_center() + vec2(20.0, 0.0),
                Side::Right => c.right_center() - vec2(20.0, 0.0),
            };
            frame(
                &mut workspace,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(target)],
            );
            frame(&mut workspace, &ctx, size, vec![pointer(target, false)]);
            assert_eq!(
                workspace.layout.palette(PaletteId::Brush).place,
                Place::Tray(side),
                "drop on {side:?}"
            );
            assert!(!egui::DragAndDrop::has_any_payload(&ctx));
            let _ = std::fs::remove_file(&workspace.persistence_path);
        }
    }

    #[test]
    fn palettes_move_to_every_place_and_reorder_without_duplicates() {
        let mut layout = DockLayout::default();
        layout.left_tray = false;
        for side in Side::ALL {
            layout.move_palette(PaletteId::Brush, Place::Tray(side), None);
            assert_eq!(
                layout
                    .palettes
                    .iter()
                    .filter(|p| p.id == PaletteId::Brush)
                    .count(),
                1
            );
            assert_eq!(layout.palette(PaletteId::Brush).place, Place::Tray(side));
        }
        assert!(layout.left_tray, "docking into a closed tray opens it");
        layout.move_palette(
            PaletteId::Brush,
            Place::Tray(Side::Right),
            Some(PaletteId::Mesh),
        );
        let brush = layout
            .palettes
            .iter()
            .position(|p| p.id == PaletteId::Brush)
            .unwrap();
        assert_eq!(layout.palettes[brush + 1].id, PaletteId::Mesh);
        layout.move_palette(PaletteId::Brush, Place::Menu, None);
        assert_eq!(layout.palette(PaletteId::Brush).place, Place::Menu);
        assert!(
            layout
                .in_tray(Side::Right)
                .iter()
                .all(|p| p.id != PaletteId::Brush)
        );
        assert_eq!(layout.palettes.len(), PaletteId::ALL.len());
    }

    #[test]
    fn layout_roundtrip_preserves_order_collapse_sections_and_trays() {
        let mut layout = DockLayout::default();
        layout.move_palette(
            PaletteId::View,
            Place::Tray(Side::Bottom),
            Some(PaletteId::Performance),
        );
        layout.palettes[0].open = false;
        layout.sections.insert("Brush/mods".to_string(), false);
        layout.right_tray = false;
        layout.tray_width = 300.0;
        assert_eq!(DockLayout::decode(&layout.encode()), layout);
    }

    #[test]
    fn old_or_damaged_layouts_recover_and_include_new_palettes() {
        let layout = DockLayout::decode(
            "# Klay workspace v1\nBrushes|Bottom|false|true\npalette|Brush|Bottom|false\npalette|Brush|Top|true\npalette|Unknown|Left|true\npalette|Mesh|invalid|true\nsection|Mesh/info|nope\ntrays|x|y|z\n",
        );
        assert_eq!(layout.palettes.len(), PaletteId::ALL.len());
        assert_eq!(layout.palettes[0].id, PaletteId::Brush);
        assert_eq!(layout.palettes[0].place, Place::Tray(Side::Bottom));
        assert!(!layout.palettes[0].open);
        assert!(layout.sections.is_empty());
        assert!(layout.left_tray && layout.right_tray);
        for id in PaletteId::ALL {
            assert_eq!(layout.palettes.iter().filter(|p| p.id == id).count(), 1);
        }
    }

    #[test]
    fn brush_previews_are_opaque_spheres_with_transparent_corners() {
        for kind in BrushKind::ALL.into_iter().map(Some).chain([None]) {
            let image = sphere_image(kind, 0, false);
            assert_eq!(image.size, [96, 96]);
            assert_eq!(image[(0, 0)].a(), 0, "corner should be transparent");
            assert_eq!(image[(48, 48)].a(), 255, "centre should be opaque");
        }
        let add = sphere_image(Some(BrushKind::Draw), 0, false);
        let sub = sphere_image(Some(BrushKind::Draw), 0, true);
        assert_ne!(add.pixels, sub.pixels, "subtract must flip the stroke");
    }

    #[test]
    fn subtool_section_never_widens_a_narrow_tray() {
        let size = vec2(1600.0, 1000.0);
        let three = [row("Sphere 1", false), row("Sphere 2", false), row("Sphere 3", true)];
        // (Edit palette text, right shelf text) for a tray width and a SubTool list.
        let place = |rows: &[SubToolRow], width: f32| {
            let (mut workspace, ctx) = test_workspace("subtool-rows");
            workspace.layout.tray_width = width;
            frame_with(&mut workspace, &ctx, size, vec![], rows);
            let out = frame_with(&mut workspace, &ctx, size, vec![], rows);
            (text_position(&out, "Snapshots").x, text_position(&out, "Persp").x)
        };
        for width in [262.0, 200.0] {
            assert_eq!(place(&[row("Sphere 1", true)], width), place(&three, width));
        }
        // Narrowing the tray by 62 px moves the tray text and the shelf by exactly 62 px:
        // nothing in the tray asks for more room than the tray has.
        let (wide, narrow) = (place(&three, 262.0), place(&three, 200.0));
        assert!((narrow.0 - wide.0 - 62.0).abs() < 0.5, "{wide:?} {narrow:?}");
        assert!((narrow.1 - wide.1 - 62.0).abs() < 0.5, "{wide:?} {narrow:?}");
    }

    #[test]
    fn initialize_picks_the_primitive_its_resolution_and_rebuilds() {
        use crate::primitive::{Primitive, Settings};
        let (mut workspace, ctx) = test_workspace("initialize");
        let size = vec2(1600.0, 2600.0);
        let mut settings = Settings::default();
        let rows = [row("Sphere 1", true)];
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        assert_eq!(text_count(&out, "4 058"), 1, "the sphere's vertex count is shown");
        let click = |workspace: &mut Workspace, settings: &mut Settings, at: Pos2| {
            frame_full(workspace, &ctx, size, vec![egui::Event::PointerMoved(at)], &rows, settings);
            frame_full(workspace, &ctx, size, vec![pointer(at, true)], &rows, settings);
            frame_full(workspace, &ctx, size, vec![pointer(at, false)], &rows, settings).1
        };
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        click(&mut workspace, &mut settings, text_position(&out, "Ring3D") + vec2(4.0, 4.0));
        assert_eq!(settings.kind, Primitive::Ring);
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        assert_eq!(text_count(&out, "4 107"), 1, "the ring's vertex count follows");
        click(&mut workspace, &mut settings, text_position(&out, "Dense") + vec2(4.0, 4.0));
        assert_eq!(settings.current(), Primitive::Ring.dense_segments());
        assert_eq!(settings.segments[Primitive::Sphere.index()], 26, "each primitive keeps its own resolution");
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        let a = click(&mut workspace, &mut settings, text_position(&out, "Rebuild") + vec2(4.0, 4.0));
        assert!(a.rebuild);
        let _ = std::fs::remove_file(&workspace.persistence_path);
    }

    #[test]
    fn wedge_section_turns_wedge_on_and_shows_the_estimate() {
        let (mut workspace, ctx) = test_workspace("wedge");
        let size = vec2(1600.0, 3000.0);
        let rows = [row("Sphere 1", true)];
        let mut settings = crate::primitive::Settings::default();
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        assert_eq!(text_count(&out, "75 000 vertices"), 1, "the estimate is shown");
        // The section title reads "Wedge" too; the button is the one just above the slider.
        let slider = text_position(&out, "Resolution").y;
        let at = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == "Wedge" => Some(t.pos),
                _ => None,
            })
            .min_by(|a, b| (a.y - slider).abs().total_cmp(&(b.y - slider).abs()))
            .unwrap()
            + vec2(4.0, 4.0);
        frame_full(&mut workspace, &ctx, size, vec![egui::Event::PointerMoved(at)], &rows, &mut settings);
        frame_full(&mut workspace, &ctx, size, vec![pointer(at, true)], &rows, &mut settings);
        let (_, a) = frame_full(&mut workspace, &ctx, size, vec![pointer(at, false)], &rows, &mut settings);
        assert!(a.wedge_toggle);
        let _ = std::fs::remove_file(&workspace.persistence_path);
    }
}
