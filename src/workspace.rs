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
use crate::curve_tube::Tube;
use egui::ecolor::Hsva;
use egui::{
    Align, Align2, Color32, ColorImage, Context, CornerRadius, FontId, Frame, Id, Layout, Margin,
    Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextureHandle, TextureId,
    TextureOptions, Ui, UiBuilder, Vec2, pos2, vec2,
};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
#[path = "brush_ui.rs"]
mod brush_ui;

// ---------------------------------------------------------------- theme tokens
pub const BACKGROUND: Color32 = Color32::from_rgb(14, 15, 16); // canvas
/// Inset of the document frame drawn inside the canvas, in points. Like ZBrush, the margin
/// between the frame and the canvas edge always navigates, even over the model.
pub const FRAME_INSET: f32 = 16.0;
const BG0: Color32 = Color32::from_rgb(20, 23, 26); // value fields, text fields
const BG1: Color32 = Color32::from_rgb(26, 29, 32); // trays, palette bodies
const BG2: Color32 = Color32::from_rgb(29, 32, 35); // menubar, status bar, shelves
const BG3: Color32 = Color32::from_rgb(42, 47, 52); // buttons, slider bars
const BG4: Color32 = Color32::from_rgb(54, 60, 66); // hover
const HEAD: Color32 = Color32::from_rgb(38, 42, 46); // palette title bars
const TILE: Color32 = Color32::from_rgb(34, 38, 42); // thumbnails, brush tiles, subtool rows
const SHELF: Color32 = Color32::from_rgb(22, 25, 27); // left and right shelves
const LINE: Color32 = Color32::from_rgb(11, 12, 13);
const LINE2: Color32 = Color32::from_rgb(47, 53, 58); // dividers inside trays
const FG: Color32 = Color32::from_rgb(230, 232, 234);
const FG2: Color32 = Color32::from_rgb(180, 186, 193);
const FG3: Color32 = Color32::from_rgb(138, 145, 153);
const ACC: Color32 = Color32::from_rgb(232, 134, 90);
const ACC_FG: Color32 = Color32::from_rgb(26, 16, 10);
/// Accent as text on dark surfaces (docked menus, tags that are on).
const ACC_TEXT: Color32 = Color32::from_rgb(240, 162, 125);
/// Selected tiles, rows and toggles: a dark clay fill under an accent border.
const ACC_DIM: Color32 = Color32::from_rgb(58, 42, 34);
/// Slider fill under the label.
const ACC_FILL: Color32 = Color32::from_rgb(74, 58, 49);
const ACC_LINE: Color32 = ACC;

const MENUBAR_H: f32 = 30.0;
const SHELF_H: f32 = 70.0;
const STATUS_H: f32 = 26.0;
const LEFT_SHELF_W: f32 = 100.0;
const RIGHT_SHELF_W: f32 = 64.0;
const DIVIDER_W: f32 = 7.0;
const HEADER_H: f32 = 34.0;
const SECTION_H: f32 = 30.0;
const SECTION_W: f32 = 250.0; // section width in horizontal trays
/// Height of sliders, segmented buttons and plain buttons.
const CONTROL_H: f32 = 26.0;
const DROPDOWN_W: f32 = 330.0;
const NC: &str = "Not connected";

/// Inter at a weight: the interface font (bundled, OFL).
fn font(size: f32, weight: Weight) -> FontId {
    match weight {
        Weight::Regular => FontId::proportional(size),
        Weight::Medium => FontId::new(size, egui::FontFamily::Name("medium".into())),
        Weight::SemiBold => FontId::new(size, egui::FontFamily::Name("semibold".into())),
    }
}

#[derive(Clone, Copy)]
enum Weight {
    Regular,
    Medium,
    SemiBold,
}

/// Small capitals for section titles and captions: upper case with a little tracking.
fn caps(text: &str, size: f32, color: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: font(size, Weight::SemiBold),
            color,
            extra_letter_spacing: size * 0.06,
            ..Default::default()
        },
    );
    job
}

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
    Light,
    Masking,
    Material,
    Mesh,
    Performance,
    Preferences,
    Stroke,
    Symmetry,
    Texture,
    Transform,
    View,
    Workspace,
}

impl PaletteId {
    /// Alphabetical, which is also the menu bar order.
    pub const ALL: [Self; 16] = [
        Self::Alpha,
        Self::Brush,
        Self::Document,
        Self::Edit,
        Self::Light,
        Self::Masking,
        Self::Material,
        Self::Mesh,
        Self::Performance,
        Self::Preferences,
        Self::Stroke,
        Self::Symmetry,
        Self::Texture,
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
            Self::Light => "Light",
            Self::Masking => "Masking",
            Self::Material => "Material",
            Self::Mesh => "Mesh",
            Self::Performance => "Performance",
            Self::Preferences => "Preferences",
            Self::Stroke => "Stroke",
            Self::Symmetry => "Symmetry",
            Self::Texture => "Texture",
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

/// Document › Back and Border (ZBrush): the canvas background colour with a vertical
/// gradient, and the colour of the border around the document frame. Colours are sRGB.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DocumentLook {
    pub back: [u8; 3],
    /// Gradient strength: 0 is flat, 1 the strongest light-to-dark ramp.
    pub range: f32,
    /// Height (0 top, 1 bottom) where the gradient crosses the back colour.
    pub center: f32,
    /// Softness of the gradient: 0 is a hard edge, 1 a ramp over the whole height.
    pub rate: f32,
    pub border: [u8; 3],
}

impl Default for DocumentLook {
    fn default() -> Self {
        Self { back: [24, 25, 28], range: 0.2, center: 0.5, rate: 1.0, border: [12, 13, 14] }
    }
}

impl DocumentLook {
    /// The background at `v` (0 top to 1 bottom of the document), 0..1 sRGB; the shader
    /// (`fs_background`) does the same.
    pub fn back_at(&self, v: f32) -> [f32; 3] {
        let half = (self.rate * 0.5).max(1e-3);
        let t = ((v - (self.center - half)) / (2.0 * half)).clamp(0.0, 1.0);
        let light = 1.0 - t * t * (3.0 - 2.0 * t);
        self.back.map(|c| (c as f32 / 255.0 + (light - 0.5) * self.range * 0.5).clamp(0.0, 1.0))
    }

    fn encode(&self) -> String {
        let rgb = |c: [u8; 3]| format!("{},{},{}", c[0], c[1], c[2]);
        format!("document|{}|{:.3}|{:.3}|{:.3}|{}\n", rgb(self.back), self.range, self.center, self.rate, rgb(self.border))
    }

    fn decode(parts: &[&str]) -> Option<Self> {
        let rgb = |s: &str| -> Option<[u8; 3]> {
            let v: Vec<u8> = s.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
            v.try_into().ok()
        };
        let unit = |s: &str| s.parse::<f32>().ok().map(|x| x.clamp(0.0, 1.0));
        match parts {
            [back, range, center, rate, border] => Some(Self {
                back: rgb(back)?,
                range: unit(range)?,
                center: unit(center)?,
                rate: unit(rate)?,
                border: rgb(border)?,
            }),
            _ => None,
        }
    }
}

/// Document dialogs (see `Workspace::doc_dialog`).
#[derive(Clone, Debug, PartialEq)]
pub enum DocDialog {
    /// Save As, with the path being typed.
    SaveAs(String),
    Revert,
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
    /// Document › Back / Border, saved with the workspace (ZBrush: Save As Startup Doc).
    pub document: DocumentLook,
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
            tray_width: 284.0,
            document: DocumentLook::default(),
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
        if self.document != DocumentLook::default() {
            text.push_str(&self.document.encode());
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
                        // 262 was the default before the readability pass widened trays.
                        result.tray_width = if (w - 262.0).abs() < 0.01 { 284.0 } else { w.clamp(200.0, 420.0) };
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
                ["document", rest @ ..] => {
                    if let Some(d) = DocumentLook::decode(rest) {
                        result.document = d;
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
    /// File for SubTool › Save / Load (and Export OBJ, as `.obj`).
    pub subtool_path: &'a mut String,
    /// Document › Import: OBJ path.
    pub import_path: &'a mut String,
    /// Mesh › Morph Target: the level (0-based) of the active subtool's stored target.
    pub morph_level: Option<usize>,
    /// Mesh › Layers of the active subtool: name, visible, recording, intensity.
    pub layers: Vec<(String, bool, bool, f32)>,
    /// Edit › History: steps that can be undone (oldest first) and redone (next first).
    pub undo_names: &'a [String],
    pub redo_names: Vec<String>,
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
    /// How it joins the subtools above it.
    pub boolean: crate::boolean::BoolOp,
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
    /// SubTool › Merge Down, Split Hidden, Groups Split, Extract.
    pub merge_down: bool,
    pub split_hidden: bool,
    pub groups_split: bool,
    pub extract: bool,
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
    /// Document › Save As (to this path, which becomes the project's), Revert, Snapshot.
    pub save_as: Option<String>,
    pub revert: bool,
    pub snapshot: bool,
    /// SubTool: save the active subtool alone, add the subtools of a file, export the
    /// active subtool as OBJ.
    pub save_subtool: bool,
    pub load_subtools: bool,
    pub export_subtool: bool,
    /// Document › Import OBJ.
    pub import_obj: bool,
    /// Mesh › Morph Target: StoreMT, DelMT, Switch.
    pub morph: Option<crate::MorphOp>,
    /// Edit › History: undo (negative) or redo (positive) this many steps.
    pub history_jump: Option<i64>,
    /// Preferences › QuickSave › QuickSave now.
    pub quicksave: bool,
    /// Mesh › Layers.
    pub layer: Option<crate::LayerOp>,
    /// Polygroups: one new group of the visible polygons (Ctrl + W).
    pub group_visible: bool,
    /// Visibility: show everything / swap hidden and visible.
    pub show_all: bool,
    pub invert_visibility: bool,
    /// Visibility › Grow (true) / Shrink (false).
    pub grow_visibility: Option<bool>,
    /// Polygroups › Auto Groups / Group By Normals (max angle, degrees).
    pub auto_groups: bool,
    pub group_by_normals: Option<u8>,
    /// Wedge: turn on (remeshing) or off / remesh now.
    pub wedge_toggle: bool,
    pub wedge_now: bool,
    /// Geometry: Del Higher / Del Lower.
    pub del_higher: bool,
    pub del_lower: bool,
    /// Geometry › Del Hidden, Close Holes; Deformation › Mirror And Weld.
    pub del_hidden: bool,
    pub close_holes: bool,
    pub mirror_weld: bool,
    /// Gizmo 3D: to the unmasked centre, or back to the subtool centre and world axes.
    pub gizmo_center: bool,
    pub gizmo_reset: bool,
    /// SubTool › Boolean: the active subtool's operation; Make Boolean Mesh.
    pub boolean_op: Option<crate::boolean::BoolOp>,
    pub make_boolean: bool,
    /// Geometry › ZRemesher.
    pub zremesh: bool,
}

/// Brush › Tablet Pressure: what pen pressure scales and through which curves.
#[derive(Clone, Debug, PartialEq)]
pub struct TabletPressure {
    pub size: bool,
    pub intensity: bool,
    pub size_curve: crate::pressure::PressureCurve,
    pub intensity_curve: crate::pressure::PressureCurve,
}

/// Materials: Klay's procedural clays, then the MatCaps from `matcap::FIRST` on (in
/// `matcap::Look::ALL` order; the colour is each one's unpainted colour, sRGB).
pub const MATERIALS: [(Color32, &str); 16] = [
    (Color32::from_rgb(199, 158, 128), "Warm clay"),
    (Color32::from_rgb(156, 163, 171), "Cool clay"),
    (Color32::from_rgb(182, 112, 90), "Terracotta"),
    (Color32::from_rgb(184, 174, 152), "Polished bronze"),
    (Color32::from_rgb(199, 85, 71), "MatCap Red Wax"),
    (Color32::from_rgb(230, 186, 168), "MatCap Skin"),
    (Color32::from_rgb(186, 186, 188), "MatCap Gray"),
    (Color32::from_rgb(241, 239, 237), "MatCap White"),
    (Color32::from_rgb(211, 208, 205), "MatCap Form"),
    (Color32::from_rgb(246, 247, 249), "MatCap Chrome"),
    (Color32::from_rgb(71, 133, 228), "MatCap Toy Plastic"),
    (Color32::from_rgb(196, 196, 198), "MatCap Studio Gray"),
    (Color32::from_rgb(238, 237, 234), "MatCap Cavity White"),
    (Color32::from_rgb(148, 160, 176), "MatCap Slate"),
    (Color32::from_rgb(101, 101, 104), "MatCap Rim Dark"),
    (Color32::from_rgb(255, 255, 255), "MatCap Normal"),
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
            Self::Rgb => "RGB Intensity",
            Self::Focal => "Focal Shift",
        }
    }
}

/// A curve stroke on the canvas (see `crate::curve`): its pieces in logical pixels,
/// whether it is still being drawn, and whether the cursor is on it (it can be bent).
#[derive(Clone, Debug, Default)]
pub struct CurvePreview {
    pub pieces: Vec<Vec<Pos2>>,
    pub drawing: bool,
    pub hot: bool,
}

pub struct Workspace {
    pub layout: DockLayout,
    pub canvas: Rect,
    pub focus: bool,
    /// A true modal: the app also uses this to suspend canvas input and shortcuts.
    pub about_open: bool,
    /// Document › Save As or Revert waiting for an answer.
    pub doc_dialog: Option<DocDialog>,
    logo: Option<TextureHandle>,
    pub subtract: bool,
    pub material: usize,
    /// Surface detail shading (the shader's reserved toggle).
    pub wireframe: bool,
    pub grid: bool,
    /// Perspective (true) or orthographic projection: `P`.
    pub perspective: bool,
    /// Mirror strokes across X: `X`.
    pub symmetry: bool,
    /// Symmetry › Y and Z mirrors, and Radial (axis, count).
    pub symmetry_y: bool,
    pub symmetry_z: bool,
    pub radial: bool,
    pub radial_axis: crate::symmetry::Axis,
    pub radial_count: f32,
    /// Brushes in Curve mode (ZBrush: Stroke › Curve), how much a curve is smoothed
    /// (times the brush's screen radius) and the dab step along it (times Draw Size).
    pub curve_kinds: Vec<BrushKind>,
    /// Brushes set to DragRect (true) or DragDot (false) (ZBrush: Stroke › DragRect,
    /// DragDot).
    pub drag_kinds: Vec<(BrushKind, bool)>,
    pub curve_smooth: f32,
    pub curve_step: f32,
    /// Stroke › Spacing (share of Draw Size between dabs), LazyMouse and LazyRadius
    /// (pixels the brush trails the cursor by).
    pub spacing: f32,
    /// View › Camera: perspective angle of view, degrees (vertical).
    pub fov: f32,
    /// Document › Import: centre and scale the import to a new primitive's size.
    pub import_fit: bool,
    /// SubTool › Extract thickness, % of the subtool's size.
    pub extract_thick: f32,
    /// Gizmo 3D instead of the action line in Move / Scale / Rotate (Y).
    pub gizmo: bool,
    /// Light palette: where the key light is, as a point on the front of a sphere seen from
    /// the camera (x right, y up), its intensity and the ambient light.
    pub light: glam::Vec2,
    pub light_intensity: f32,
    pub ambient: f32,
    /// Mesh › Layers: the selected layer, and the Intensity slider's value while dragged
    /// with the value it started from.
    pub layer_selected: usize,
    layer_drag: Option<(f32, f32)>,
    /// Preferences › QuickSave: on, and every how many minutes.
    pub quicksave: bool,
    pub quicksave_minutes: f32,
    /// Masking › Mask By Cavity sensitivity (1..=100) and Mask By Polygroups border width
    /// (rings).
    pub cavity_sensitivity: f32,
    pub group_border_width: f32,
    /// Polygroups › Group By Normals: Max Angle Tolerance, degrees.
    pub normals_angle: f32,
    pub lazy_mouse: bool,
    pub lazy_radius: f32,
    /// LazyMouse string while stroking: brush point to cursor, logical pixels.
    pub lazy_line: Option<(Pos2, Pos2)>,
    /// CurveTube shape (Stroke › Curve): taper toward the end, vertices around each ring.
    pub tube_taper: f32,
    pub tube_sides: u32,
    /// The curve being drawn or the one that can be bent, in logical pixels, per piece.
    pub curve_preview: Option<CurvePreview>,
    /// Pen pressure scales Draw Size and Z Intensity (ZBrush: Brush > Tablet Pressure).
    pub pressure_size: bool,
    pub pressure_intensity: bool,
    /// Brush › Tablet Pressure curves for Draw Size and for Z / RGB Intensity.
    pub size_curve: crate::pressure::PressureCurve,
    pub intensity_curve: crate::pressure::PressureCurve,
    /// The curve point being dragged: (which curve, point index).
    curve_drag: Option<(bool, usize)>,
    /// Brushes with Tablet Pressure of their own (ZBrush: per brush); the others use the
    /// settings above.
    pub brush_pressure: HashMap<BrushKind, TabletPressure>,
    /// The mesh IMM Primitives inserts.
    pub insert_shape: crate::primitive::Primitive,
    /// Sculptris Pro (ZBrush): strokes retessellate the mesh under the brush; Detail sets
    /// how small its triangles are for the brush size (see `sculptris::target_edge`).
    pub sculptris: bool,
    pub sculptris_detail: f32,
    /// Live Boolean (ZBrush): the viewport shows the subtools' boolean result; Make
    /// Boolean Mesh builds it at this resolution (Remesh voxels across two units).
    pub live_boolean: bool,
    pub boolean_resolution: f32,
    /// Geometry › ZRemesher settings.
    pub zremesh: crate::zremesh::Settings,
    /// Brush › ZModeler: the polygon action and target, the edge action and whether it
    /// takes the whole edge loop.
    pub zm_poly: crate::zmodeler::PolyAction,
    pub zm_target: crate::zmodeler::PolyTarget,
    pub zm_edge: crate::zmodeler::EdgeAction,
    pub zm_edge_loop: bool,
    /// Shade the mask: Ctrl + H (ZBrush: View Mask).
    pub show_mask: bool,
    /// Draw only the active subtool (ZBrush: Solo).
    pub solo: bool,
    /// Polygroup colours on the active subtool (ZBrush: PolyF, Shift + F).
    pub polyframe: bool,
    /// Ctrl + Shift selection rectangle being dragged (logical pixels), and whether it
    /// hides (Alt, red) rather than keeps (green) what it covers.
    pub select_rect: Option<(Rect, bool)>,
    pub gesture_preview: Option<crate::surface_tools::Gesture>,
    pub mask_brush: BrushKind,
    /// The brush a held modifier strokes with, shown in the panels this frame instead of
    /// the brush (ZBrush: holding Ctrl shows the mask brush, Shift shows Smooth).
    pub held_brush: Option<BrushKind>,
    /// Each brush's own Draw settings (ZBrush: everything in the Draw palette but Draw
    /// Size follows the brush), for every brush but the one in use, whose values are live
    /// in `Brush` and `subtract`. See `select_brush`.
    pub(super) draw_settings: HashMap<BrushKind, brush_ui::DrawSettings>,
    /// Deformation slider being dragged, and its value.
    deform_drag: Option<(Deform, f32)>,
    /// Deformation › Size's X, Y and Z toggles: the axes it scales along (ZBrush).
    pub size_axes: [bool; 3],
    pub mode: Mode,
    /// Open hotkey popup and the logical position it was opened at.
    pub popup: Option<(QuickPopup, Pos2)>,
    /// ZBrush quick menu (Space held, or a right-click tap): where it opened and whether
    /// it closes when Space is released rather than on a click elsewhere.
    pub quick_menu: Option<(Pos2, bool)>,
    pub color: Hsva,
    pub color2: Hsva,
    pub assets: crate::sculpt::StampSettings,
    brush_ui: brush_ui::BrushUi,
    dynamic: bool,
    dropdown: Option<PaletteId>,
    dropdown_anchor: Rect,
    menubar_rect: Rect,
    pending_move: Option<(PaletteId, Place, Option<PaletteId>)>,
    scroll_to: Option<PaletteId>,
    textures: HashMap<(usize, usize, bool), TextureHandle>,
    message: Option<(String, f64)>,
    /// Text queued by the app for the status bar; shown on the next frame.
    pub(crate) pending_notice: Option<String>,
    saved: String,
    persistence_path: PathBuf,
    pub persistence_error: Option<String>,
}

impl Workspace {
    /// Tablet Pressure for strokes with `kind`: its own, else the global settings.
    pub fn tablet(&self, kind: BrushKind) -> TabletPressure {
        self.brush_pressure.get(&kind).cloned().unwrap_or_else(|| TabletPressure {
            size: self.pressure_size,
            intensity: self.pressure_intensity,
            size_curve: self.size_curve.clone(),
            intensity_curve: self.intensity_curve.clone(),
        })
    }

    /// Store `t` where [`tablet`](Self::tablet) reads it for `kind`.
    fn set_tablet(&mut self, kind: BrushKind, t: TabletPressure) {
        if let Some(own) = self.brush_pressure.get_mut(&kind) {
            *own = t;
        } else {
            self.pressure_size = t.size;
            self.pressure_intensity = t.intensity;
            self.size_curve = t.size_curve;
            self.intensity_curve = t.intensity_curve;
        }
    }

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
        let brush_ui=brush_ui::BrushUi::new(root.join("klay/brush-assets.v1"));
        let assets=brush_ui.settings();
        Self {
            layout,
            canvas: Rect::NOTHING,
            focus: false,
            about_open: false,
            doc_dialog: None,
            logo: None,
            subtract: false,
            material: 0,
            wireframe: false,
            grid: true,
            perspective: true,
            symmetry: false,
            symmetry_y: false,
            symmetry_z: false,
            radial: false,
            radial_axis: crate::symmetry::Axis::Y,
            radial_count: 8.0,
            curve_kinds: crate::curve::CURVE_BY_DEFAULT.to_vec(),
            drag_kinds: Vec::new(),
            curve_smooth: 0.5,
            curve_step: 0.16,
            spacing: crate::sculpt::SPACING,
            fov: 45.0,
            import_fit: true,
            extract_thick: 2.0,
            quicksave: true,
            quicksave_minutes: 10.0,
            gizmo: false,
            light: LIGHT_DEFAULT,
            light_intensity: 1.0,
            ambient: 1.0,
            layer_selected: 0,
            layer_drag: None,
            cavity_sensitivity: 50.0,
            normals_angle: 20.0,
            group_border_width: 2.0,
            lazy_mouse: false,
            lazy_radius: 40.0,
            lazy_line: None,
            tube_taper: 0.0,
            tube_sides: 12,
            curve_preview: None,
            pressure_size: true,
            pressure_intensity: true,
            size_curve: crate::pressure::PressureCurve::linear(),
            intensity_curve: crate::pressure::PressureCurve::linear(),
            curve_drag: None,
            brush_pressure: HashMap::new(),
            insert_shape: crate::primitive::Primitive::Sphere,
            sculptris: false,
            sculptris_detail: 50.0,
            live_boolean: false,
            boolean_resolution: 400.0,
            zremesh: crate::zremesh::Settings::default(),
            zm_poly: Default::default(),
            zm_target: Default::default(),
            zm_edge: Default::default(),
            zm_edge_loop: true,
            show_mask: true,
            solo: false,
            polyframe: false,
            select_rect: None,
            gesture_preview: None,
            mask_brush: BrushKind::MaskPen,
            held_brush: None,
            draw_settings: HashMap::new(),
            deform_drag: None,
            size_axes: [true; 3],
            mode: Mode::Draw,
            popup: None,
            quick_menu: None,
            color: Hsva::new(0.06, 0.55, 0.88, 1.0),
            color2: Hsva::new(0.0, 0.0, 1.0, 1.0),
            assets,
            brush_ui,
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
        let held = self.begin_held_brush(m.brush);
        let a = self.show_panels(ui, m);
        self.end_held_brush(m.brush, held);
        a
    }

    fn show_panels(&mut self, ui: &mut Ui, m: &mut Model<'_>) -> Actions {
        let mut a = Actions::default();
        self.brush_assets_frame(ui.ctx());
        if let Some(text) = self.pending_notice.take() {
            self.flash(ui.ctx(), text);
        }
        if !self.modal_open() && !self.brush_catalog_open()
            && !ui.ctx().egui_wants_keyboard_input()
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
        self.brush_catalog_ui(ui,m);
        self.import_path_ui(ui);
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
        self.about_dialog(ui.ctx());
        self.doc_dialog(ui.ctx(), m, &mut a);
        m.brush.color=self.color.to_srgb();
        self.save_brush_assets();
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
        self.draw_quick_menu(ui, m);
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
                            draw_size_slider(ui, kind.label(), &mut m.brush.radius, true);
                        }
                        QuickPopup::Intensity => {
                            zslider(ui, kind.label(), &mut m.brush.strength, 0.0..=1.0, false, true, 2);
                        }
                        QuickPopup::Rgb => {zslider(ui,kind.label(),&mut m.brush.rgb_intensity,0.0..=1.0,false,true,2);}
                        QuickPopup::Focal => {zslider(ui,kind.label(),&mut m.brush.focal_shift,-100.0..=100.0,false,true,0);}
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
                    let logo = self.logo_texture(ui.ctx());
                    let brand = ui
                        .scope(|ui| {
                            ui.spacing_mut().button_padding = vec2(3.0, 1.0);
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.add(
                                egui::Button::new((
                                    egui::Image::new((logo, vec2(22.0, 22.0))),
                                    RichText::new("Klay").font(font(14.0, Weight::SemiBold)),
                                ))
                                .frame_when_inactive(false)
                                .corner_radius(5),
                            )
                            .on_hover_text("About Klay")
                        })
                        .inner;
                    if brand.clicked() {
                        self.about_open = true;
                        self.dropdown = None;
                        self.popup = None;
                        self.quick_menu = None;
                    }
                    ui.add_space(8.0);
                    for id in PaletteId::ALL {
                        let docked = self.layout.palette(id).place != Place::Menu;
                        let on = self.dropdown == Some(id);
                        let galley = ui.painter().layout_no_wrap(
                            id.name().to_string(),
                            font(13.0, if on { Weight::SemiBold } else { Weight::Regular }),
                            FG,
                        );
                        let padding=if ui.max_rect().width()<1200.0 {8.0}else{16.0};
                        let width = galley.size().x + padding + if docked { 9.0 } else { 0.0 };
                        let (r, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
                        if on {
                            ui.painter().rect_filled(r, 3, ACC);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, 3, BG3);
                        }
                        // Docked palettes keep a dot: they are in a tray.
                        let color = if on {
                            ACC_FG
                        } else {
                            Color32::from_rgb(218, 222, 226)
                        };
                        ui.painter().galley_with_override_text_color(
                            r.left_center() + vec2(padding*0.5, -galley.size().y * 0.5),
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
                        ui.add(egui::Label::new(RichText::new(title).font(font(12.5, Weight::Medium)).color(FG2)).truncate());
                    });
                });
            })
            .response;
        self.menubar_rect = response.rect;
    }

    fn logo_texture(&mut self, ctx: &Context) -> TextureId {
        self.logo
            .get_or_insert_with(|| crate::branding::texture(ctx))
            .id()
    }

    /// The symmetry every brush strokes with (Symmetry palette, X key).
    pub fn sym(&self) -> crate::symmetry::Symmetry {
        crate::symmetry::Symmetry {
            x: self.symmetry,
            y: self.symmetry_y,
            z: self.symmetry_z,
            radial: self.radial.then(|| (self.radial_axis, self.radial_count.round().clamp(2.0, 64.0) as u8)),
        }
    }

    /// A dialog is up: canvas input waits (About, Save As, Revert).
    pub fn modal_open(&self) -> bool {
        self.about_open || self.doc_dialog.is_some()
    }

    /// Document › Save As (a path, prefilled) and Revert (asks first: unsaved work is lost).
    fn doc_dialog(&mut self, ctx: &Context, m: &mut Model<'_>, a: &mut Actions) {
        let Some(dialog) = self.doc_dialog.as_mut() else {
            return;
        };
        let mut close = false;
        let response = egui::Modal::new(Id::unique("document-dialog"))
            .backdrop_color(Color32::from_black_alpha(120))
            .frame(Frame::NONE.fill(BG1).stroke(Stroke::new(1.0, BG4)).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                match dialog {
                    DocDialog::SaveAs(path) => {
                        ui.label(RichText::new("Save As").size(14.0).strong());
                        let field = ui.add(
                            egui::TextEdit::singleline(path).desired_width(f32::INFINITY).font(FontId::monospace(12.0)),
                        );
                        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!path.trim().is_empty(), egui::Button::new("Save")).clicked() || (enter && !path.trim().is_empty()) {
                                a.save_as = Some(path.trim().to_string());
                                close = true;
                            }
                            close |= ui.button("Cancel").clicked();
                        });
                        note(ui, "Saves every subtool there; later saves go to the new file.");
                    }
                    DocDialog::Revert => {
                        ui.label(RichText::new("Revert").size(14.0).strong());
                        ui.label(format!("Open {} again as it was last saved? Changes since then are lost.", m.project_path));
                        ui.horizontal(|ui| {
                            if ui.button("Revert").clicked() {
                                a.revert = true;
                                close = true;
                            }
                            close |= ui.button("Cancel").clicked();
                        });
                    }
                }
            });
        if close || response.should_close() {
            self.doc_dialog = None;
        }
    }

    fn about_dialog(&mut self, ctx: &Context) {
        if !self.about_open {
            return;
        }
        let logo = self.logo_texture(ctx);
        let response = egui::Modal::new(Id::unique("about-klay"))
            .backdrop_color(Color32::from_black_alpha(150))
            .frame(
                Frame::NONE
                    .fill(BG1)
                    .stroke(Stroke::new(1.0, BG4))
                    .corner_radius(16)
                    .inner_margin(28),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("About Klay").size(18.0).strong());
                    ui.add_space(6.0);
                    ui.add(egui::Image::new((logo, vec2(160.0, 160.0))));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Version {}", crate::branding::VERSION)).size(14.0),
                    );
                    ui.label(
                        RichText::new(format!("Built with {}", crate::branding::LANGUAGE)).color(FG2),
                    );
                    ui.hyperlink_to("GitHub repository", crate::branding::REPOSITORY)
                        .on_hover_text(crate::branding::REPOSITORY);
                    ui.add_space(10.0);
                    ui.add_sized(
                        vec2(104.0, 32.0),
                        egui::Button::new("Close").corner_radius(8),
                    )
                    .clicked()
                })
                .inner
            });
        if response.inner || response.should_close() {
            self.about_open = false;
        }
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
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter()
                        .circle_filled(r.center(), 4.0, Color32::from_rgb(127, 211, 106));
                    ui.label(RichText::new(message).font(font(12.5, Weight::Medium)).color(FG));
                    for text in [
                        format!("{} vertices", crate::fmt_count(m.vertices)),
                        format!("{} triangles", crate::fmt_count(m.triangles)),
                        format!("{} chunks", m.chunks),
                    ] {
                        ui.label(RichText::new(text).font(font(12.5, Weight::Regular)).color(FG2));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.1} ms · {:.0} fps", m.frame_ms, m.fps))
                                .font(font(12.5, Weight::Regular))
                                .color(FG2),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new("Drag tray space to scroll · Tab hides the interface")
                                    .font(font(12.5, Weight::Regular))
                                    .color(FG3),
                            )
                            .truncate(),
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
            .frame(Frame::NONE.fill(BG1).inner_margin(Margin::symmetric(12, 0)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter()
                    .hline(rect.x_range(), rect.bottom() - 0.5, (1.0, LINE));
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    // mode buttons (Q / W / E / R)
                    for (glyph, label, mode, hint) in [
                        ("sculpt", "Sculpt", Mode::Draw, "Draw · Q"),
                        ("move", "Move", Mode::Move, "Move · W · drag the model for an action line, its middle circle moves"),
                        ("scale", "Scale", Mode::Scale, "Scale · E · drag the action line's end circle; its middle circle squashes along the line"),
                        ("rotate", "Rotate", Mode::Rotate, "Rotate · R · drag the action line's end circle"),
                    ] {
                        if big_button(ui, glyph, label, vec2(56.0, 56.0), self.mode == mode, true)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            self.mode = mode;
                        }
                    }
                    shelf_separator(ui);
                    // ZBrush keeps Sculptris Pro and Live Boolean on the top shelf.
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.add_space(6.0);
                        if small_toggle(ui, "Sculptris Pro", self.sculptris, vec2(120.0, CONTROL_H))
                            .on_hover_text("Strokes add and remove triangles under the brush · Detail in Brush › Sculptris Pro")
                            .clicked()
                        {
                            self.sculptris = !self.sculptris;
                        }
                        if small_toggle(ui, "Live Boolean", self.live_boolean, vec2(120.0, CONTROL_H))
                            .on_hover_text("Show the subtools' Add / Subtract / Intersect result · Mesh › Boolean")
                            .clicked()
                        {
                            self.live_boolean = !self.live_boolean;
                        }
                    });
                    shelf_separator(ui);
                    // Sculpt / polypaint modes and live RGB intensity.
                    ui.vertical(|ui| {
                        ui.set_width(200.0);
                        ui.add_space(6.0);
                        self.paint_modes(ui,m);
                        zslider(ui,"RGB Intensity",&mut m.brush.rgb_intensity,0.0..=1.0,false,true,2);
                    });
                    shelf_separator(ui);
                    // Zadd / Zsub + intensity
                    ui.vertical(|ui| {
                        ui.set_width(200.0);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.set_width(200.0);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let w = ui.available_width() - 54.0;
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
                                egui::Button::new("Zcut").min_size(vec2(50.0, CONTROL_H)),
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
                        ui.set_width(236.0);
                        ui.add_space(6.0);
                        zslider(ui,"Focal Shift",&mut m.brush.focal_shift,-100.0..=100.0,false,true,0);
                        ui.horizontal(|ui| {
                            ui.set_width(236.0);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let w = ui.available_width() - 44.0;
                            ui.scope(|ui| {
                                ui.set_width(w);
                                draw_size_slider(ui, "Draw Size", &mut m.brush.radius, true);
                            });
                            if small_toggle(ui, "Dyn", self.dynamic, vec2(40.0, CONTROL_H))
                                .on_hover_text("Radius follows zoom")
                                .clicked()
                            {
                                self.dynamic = !self.dynamic;
                            }
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        a.redo |= ui
                            .add_enabled(m.can_redo, egui::Button::new("Redo").min_size(vec2(64.0, 32.0)))
                            .on_hover_text("Ctrl+Shift+Z")
                            .clicked();
                        a.undo |= ui
                            .add_enabled(m.can_undo, egui::Button::new("Undo").min_size(vec2(64.0, 32.0)))
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
            .frame(Frame::NONE.fill(SHELF).inner_margin(Margin::symmetric(7, 10)))
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
                        ui.spacing_mut().item_spacing.y = 8.0;
                        // brush
                        let tex = self.texture(ui.ctx(), Some(m.brush.kind), self.subtract);
                        let resp = thumb(ui, "Brush", Thumb::Texture(tex), m.brush.kind.name(), true, false);
                        if resp.clicked() {self.open_brush_catalog(resp.rect.right_top()+vec2(6.0,0.0));}
                        let kind = m.brush.kind;
                        let mode = self.stroke_mode(kind);
                        let modes = Self::stroke_modes(kind);
                        let resp = thumb(ui, "Stroke", Thumb::Glyph("stroke"), mode, false, false);
                        if modes.len() < 2 {
                            resp.on_hover_text(format!("{} strokes {} only", kind.name(), mode));
                        } else if resp.on_hover_text(format!("Click for the next stroke: {}", modes.join(" / "))).clicked() {
                            let at = modes.iter().position(|m| *m == mode).unwrap_or(0);
                            self.set_stroke_mode(kind, modes[(at + 1) % modes.len()]);
                        }
                        self.asset_shelf(ui,false);
                        self.asset_shelf(ui,true);
                        // material
                        let tex = self.texture(ui.ctx(), None, false);
                        let resp = thumb(
                            ui,
                            "Material",
                            Thumb::Texture(tex),
                            MATERIALS[self.material].1.trim_start_matches("MatCap "),
                            false,
                            false,
                        );
                        egui::Popup::from_toggle_button_response(&resp)
                            .align(egui::RectAlign::RIGHT_START)
                            .gap(6.0)
                            .frame(popup_frame())
                            .show(|ui| {
                                ui.label(RichText::new("MATERIAL").size(11.0).color(FG3));
                                self.material_swatches(ui);
                            });
                        // colour
                        ui.add_space(3.0);
                        color_picker(ui, &mut self.color);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(36.0, 32.0), Sense::click());
                            let p = ui.painter();
                            let back =
                                Rect::from_min_size(r.min + vec2(14.0, 10.0), vec2(22.0, 22.0));
                            let front = Rect::from_min_size(r.min, vec2(22.0, 22.0));
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
                                RichText::new(format!("#{cr:02X}{cg:02X}{cb:02X}"))
                                    .font(font(12.0, Weight::Medium))
                                    .color(FG2),
                            );
                        });
                        ui.add_enabled_ui(false, |ui| {
                            let _ = ui.add(
                                egui::Button::new(RichText::new("Alternate").color(ACC_FG))
                                    .fill(ACC)
                                    .min_size(vec2(86.0, CONTROL_H)),
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
            .frame(Frame::NONE.fill(SHELF).inner_margin(Margin::symmetric(6, 10)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter()
                    .vline(rect.left() + 0.5, rect.y_range(), (1.0, LINE));
                ui.spacing_mut().item_spacing.y = 6.0;
                let size = vec2(52.0, 50.0);
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
                if big_button(ui, "sym", if self.symmetry { "Sym X" } else { "Sym" }, size, self.symmetry, true)
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
                .min(((screen.width() - 440.0) * 0.5).max(160.0));
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
                                .size(12.0)
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
                    p.rect_filled(hr, 0, if hovered { BG4 } else { HEAD });
                    p.hline(hr.x_range(), hr.top() + 0.5, (1.0, LINE));
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
                    // Accent tick, then the palette name in the heaviest weight of the tray.
                    let x = hr.left() + 26.0;
                    ui.painter().rect_filled(
                        Rect::from_center_size(pos2(x, hr.center().y), vec2(3.0, 16.0)),
                        1,
                        ACC,
                    );
                    if !floating {
                        chevron(ui.painter(), hr.right_center() - vec2(14.0, 0.0), open, FG2);
                    }
                    ui.painter().text(
                        pos2(x + 8.0, hr.center().y),
                        Align2::LEFT_CENTER,
                        id.name(),
                        font(14.0, Weight::SemiBold),
                        FG,
                    );
                    if floating {
                        if let Place::Tray(side) = state.place {
                            tag(
                                ui.painter(),
                                hr.right_center() - vec2(10.0, 0.0),
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
                        .size(11.0)
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
        // Section titles: small capitals on the tray, a divider above, the chevron and any
        // state on the right. Palette titles stay the bolder level above them.
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(r, 0, BG3);
        }
        p.hline(r.x_range(), r.top() + 0.5, (1.0, LINE2));
        let color = if resp.hovered() || open { FG2 } else { FG3 };
        let title_job = caps(title, 11.5, if open { FG2 } else { Color32::from_rgb(160, 167, 175) });
        let galley = p.layout_job(title_job);
        p.galley(pos2(r.left() + 12.0, r.center().y - galley.size().y * 0.5), galley, FG2);
        chevron(p, r.right_center() - vec2(14.0, 0.0), open, color);
        if let Some(t) = tag_text {
            tag(p, r.right_center() - vec2(28.0, 0.0), t);
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
                        left: 12,
                        right: 12,
                        top: 4,
                        bottom: 12,
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
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
                self.sec(ui,h,id,"alpha","Alphas",None,true,true,&mut |ws,ui|ws.asset_grid(ui,false));
            }
            PaletteId::Texture => {
                self.sec(ui,h,id,"textures","Textures",None,true,true,&mut |ws,ui|ws.asset_grid(ui,true));
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
                        ws.brush_palette(ui,m);
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
                        draw_size_slider(ui, "Radius", &mut m.brush.radius, false);
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
                        zslider(ui,"Focal Shift",&mut m.brush.focal_shift,-100.0..=100.0,false,false,0);
                    },
                );
                self.sec(ui, h, id, "sculptris", "Sculptris Pro", None, true, true, &mut |ws, ui| {
                    ui.checkbox(&mut ws.sculptris, "Sculptris Pro")
                        .on_hover_text("Strokes add and remove triangles under the brush as you sculpt (subtools without subdivision levels)");
                    ui.add_enabled_ui(ws.sculptris, |ui| {
                        zslider(ui, "Detail", &mut ws.sculptris_detail, 0.0..=100.0, false, false, 0);
                    });
                    note(ui, "Triangles follow Draw Size: a smaller brush makes smaller ones · Smooth (Shift) coarsens to its own size · masked surface stays as it is");
                });
                if m.brush.kind.is_zmodeler() {
                    self.sec(ui, h, id, "zmodeler", "ZModeler", None, true, true, &mut |ws, ui| {
                        use crate::zmodeler::{EdgeAction, PolyAction, PolyTarget};
                        ui.label(RichText::new("Polygon").size(12.0).color(FG2));
                        let names: Vec<&str> = PolyAction::ALL.iter().map(|x| x.name()).collect();
                        let at = PolyAction::ALL.iter().position(|x| *x == ws.zm_poly).unwrap_or(0);
                        if let Some(i) = seg(ui, &names[..3], at.min(2), false) {
                            ws.zm_poly = PolyAction::ALL[i];
                        }
                        if let Some(i) = seg(ui, &names[3..], at.saturating_sub(3).min(2), false) {
                            ws.zm_poly = PolyAction::ALL[3 + i];
                        }
                        let targets: Vec<&str> = PolyTarget::ALL.iter().map(|x| x.name()).collect();
                        let at = PolyTarget::ALL.iter().position(|x| *x == ws.zm_target).unwrap_or(0);
                        if let Some(i) = seg(ui, &targets, at, false) {
                            ws.zm_target = PolyTarget::ALL[i];
                        }
                        ui.label(RichText::new("Edge").size(12.0).color(FG2));
                        let names: Vec<&str> = EdgeAction::ALL.iter().map(|x| x.name()).collect();
                        let at = EdgeAction::ALL.iter().position(|x| *x == ws.zm_edge).unwrap_or(0);
                        if let Some(i) = seg(ui, &names, at, false) {
                            ws.zm_edge = EdgeAction::ALL[i];
                        }
                        if ws.zm_edge == EdgeAction::Delete {
                            ui.checkbox(&mut ws.zm_edge_loop, "EdgeLoop").on_hover_text("Delete the whole loop, not just the edge");
                        }
                        note(ui, "Hover a polygon, edge or point · QMesh and Extrude: drag out · Inset: drag in · Insert: click where the loop goes, drag to slide it · points: drag to move");
                    });
                }
                if m.brush.kind.is_imm() {
                    self.sec(ui, h, id, "imm", "Insert Mesh", None, true, true, &mut |ws, ui| {
                        let shapes = crate::insert_mesh::SHAPES;
                        let names: Vec<&str> = shapes.iter().map(|p| p.short()).collect();
                        let at = shapes.iter().position(|p| *p == ws.insert_shape).unwrap_or(0);
                        if let Some(i) = seg(ui, &names, at, false) {
                            ws.insert_shape = shapes[i];
                        }
                        note(ui, "Drag on the model: the mesh stands on the surface, the drag sets its size · new polygroup, the rest masked · Shift smooths");
                    });
                }
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
                        // ZBrush keeps Tablet Pressure per brush; Global is shared by every
                        // brush without its own.
                        let kind = ws.held_brush.unwrap_or(m.brush.kind);
                        let own = ws.brush_pressure.contains_key(&kind);
                        if let Some(i) = seg(ui, &["Global", kind.name()], own as usize, false) {
                            if i == 1 && !own {
                                let t = ws.tablet(kind);
                                ws.brush_pressure.insert(kind, t);
                            } else if i == 0 {
                                ws.brush_pressure.remove(&kind);
                            }
                        }
                        let mut t = ws.tablet(kind);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            ui.checkbox(&mut t.size, "Size")
                                .on_hover_text("Pen pressure scales Draw Size");
                            ui.checkbox(&mut t.intensity, "Z Intensity")
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
                        let live = m.pressure.filter(|_| m.tablet_on);
                        for size in [true, false] {
                            ui.label(RichText::new(if size { "Size curve" } else { "Intensity curve" }).size(12.0).color(FG2));
                            let mut drag = ws.curve_drag.filter(|d| d.0 == size).map(|d| d.1);
                            let curve = if size { &mut t.size_curve } else { &mut t.intensity_curve };
                            pressure_curve_editor(ui, curve, &mut drag, live);
                            let presets = crate::pressure::PressureCurve::PRESETS;
                            let at = presets
                                .iter()
                                .position(|name| *curve == crate::pressure::PressureCurve::preset(name))
                                .unwrap_or(usize::MAX);
                            if let Some(i) = seg(ui, &presets, at, false) {
                                *curve = crate::pressure::PressureCurve::preset(presets[i]);
                            }
                            if drag.is_some() {
                                ws.curve_drag = drag.map(|i| (size, i));
                            } else if ws.curve_drag.is_some_and(|d| d.0 == size) {
                                ws.curve_drag = None;
                            }
                        }
                        ws.set_tablet(kind, t);
                        note(ui, "Drag a point · click to add one · double-click to remove it · Grab keeps its size");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "auto",
                    "Auto Masking",
                    None,
                    false,
                    true,
                    &mut |ws, ui| {
                        let auto = &mut ws.assets.auto;
                        ui.checkbox(&mut auto.backface, "BackfaceMask")
                            .on_hover_text("Leave surface turned away from you alone: the far side of thin parts");
                        ui.checkbox(&mut auto.topological, "Topological")
                            .on_hover_text("Only the surface joined to the point under the brush, inside the brush: the next finger or the other lip stays");
                        note(ui, "Every sculpt and paint brush, with symmetry too.");
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
                    &mut |ws, ui| {
                        let name = std::path::Path::new(m.project_path.as_str())
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().to_string());
                        kv(ui, "Name", &name);
                        kv(ui, "Subtools", &m.subtools.len().to_string());
                        ui.add(
                            egui::TextEdit::singleline(m.project_path)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(12.0)),
                        );
                        let saved = std::path::Path::new(m.project_path.as_str()).is_file();
                        let mut clicked = [false; 6];
                        for (row, cells) in [
                            [
                                ("New", true, "Start over with one subtool from Mesh › Initialize"),
                                ("Open", true, "Open the project at this path · Ctrl + O"),
                                ("Save", true, "Save every subtool to this path · Ctrl + S"),
                            ],
                            [
                                ("Save As", true, "Save every subtool to a new path and keep working there"),
                                ("Revert", saved, "Open this project again as it was last saved"),
                                ("Snapshot", true, "Save the canvas inside the document frame as a PNG next to the project"),
                            ],
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                            let w = (r.width() - 6.0) / 3.0;
                            for (col, (label, on, hint)) in cells.into_iter().enumerate() {
                                let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                                child.spacing_mut().button_padding = vec2(2.0, 1.0);
                                if !on {
                                    child.disable();
                                }
                                clicked[row * 3 + col] = child
                                    .put(cell, egui::Button::new(RichText::new(label).size(12.0)).truncate())
                                    .on_hover_text(hint)
                                    .clicked();
                            }
                        }
                        a.new_project |= clicked[0];
                        a.open |= clicked[1];
                        a.save |= clicked[2];
                        if clicked[3] {
                            ws.doc_dialog = Some(DocDialog::SaveAs(m.project_path.clone()));
                        }
                        if clicked[4] {
                            ws.doc_dialog = Some(DocDialog::Revert);
                        }
                        a.snapshot |= clicked[5];
                        note(ui, "Saves every subtool with its levels and sculpting; undo history is not saved.");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "back",
                    "Back",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        let d = &mut ws.layout.document;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut d.back)
                                .on_hover_text("Canvas background colour");
                            ui.label("Back");
                            ui.add_space(12.0);
                            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut d.border)
                                .on_hover_text("Colour of the border around the document frame");
                            ui.label("Border");
                        });
                        zslider(ui, "Range", &mut d.range, 0.0..=1.0, false, false, 2);
                        zslider(ui, "Center", &mut d.center, 0.0..=1.0, false, false, 2);
                        zslider(ui, "Rate", &mut d.rate, 0.0..=1.0, false, false, 2);
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Reset"))
                            .on_hover_text("Klay's default background and border")
                            .clicked()
                        {
                            *d = DocumentLook::default();
                        }
                        note(ui, "Range: gradient strength · Center: where it crosses the Back colour · Rate: its softness. Kept with the workspace.");
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
                                .font(FontId::monospace(12.0)),
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
                    None,
                    false,
                    true,
                    &mut |ws, ui| {
                        ui.add(
                            egui::TextEdit::singleline(m.import_path)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(12.0)),
                        );
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Import OBJ"))
                            .on_hover_text("Add the OBJ at this path as a new subtool")
                            .clicked()
                        {
                            a.import_obj = true;
                        }
                        ui.checkbox(&mut ws.import_fit, "Fit")
                            .on_hover_text("Centre it and scale it to a new primitive's size, so Draw Size suits it");
                        note(ui, "OBJ groups become polygroups, vertex colours polypaint.");
                    },
                );
            }
            PaletteId::Edit => {
                self.sec(ui, h, id, "undo", "Undo", None, true, true, &mut |_, ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 3.0) * 0.5;
                        a.undo |= ui
                            .add_enabled_ui(m.can_undo, |ui| ui.add_sized([w, CONTROL_H], egui::Button::new("Undo").truncate())).inner
                            .clicked();
                        a.redo |= ui
                            .add_enabled_ui(m.can_redo, |ui| ui.add_sized([w, CONTROL_H], egui::Button::new("Redo").truncate())).inner
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
                        // ZBrush: Edit › Undo History. Click a step to go back (or forward)
                        // to just after it; the first row is the subtool before any step.
                        let done = m.undo_names.len();
                        if done == 0 && m.redo_names.is_empty() {
                            note(ui, "No steps yet.");
                            return;
                        }
                        egui::ScrollArea::vertical().max_height(240.0).stick_to_bottom(true).show(ui, |ui| {
                            let rows = std::iter::once("Start".to_string())
                                .chain(m.undo_names.iter().cloned())
                                .chain(m.redo_names.iter().cloned());
                            for (k, name) in rows.enumerate() {
                                let here = k == done;
                                let future = k > done;
                                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
                                let p = ui.painter();
                                if here {
                                    p.rect_filled(r, 2, ACC_DIM);
                                } else if resp.hovered() {
                                    p.rect_filled(r, 2, BG4);
                                }
                                let label = if k == 0 { name } else { format!("{k}. {name}") };
                                p.text(r.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.5), if future { FG3 } else { FG });
                                if resp.on_hover_text(if future { "Redo to here" } else { "Undo to here" }).clicked() && !here {
                                    a.history_jump = Some(k as i64 - done as i64);
                                }
                            }
                        });
                    },
                );
            }
            PaletteId::Light => {
                self.sec(
                    ui,
                    h,
                    id,
                    "light",
                    "Light",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        light_placer(ui, &mut ws.light);
                        zslider(ui, "Intensity", &mut ws.light_intensity, 0.0..=2.0, false, false, 2);
                        zslider(ui, "Ambient", &mut ws.ambient, 0.0..=2.0, false, false, 2);
                        if ui.add_sized([ui.available_width(), 20.0], egui::Button::new("Reset")).clicked() {
                            ws.light = LIGHT_DEFAULT;
                            ws.light_intensity = 1.0;
                            ws.ambient = 1.0;
                        }
                        note(ui, "Drag on the sphere to place the light; it turns with the view · lights the clays, MatCaps carry their own light");
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
                        ui.horizontal_wrapped(|ui| {
                            for (label, op, hint) in [
                                ("Blur", MaskOp::Blur, "Ctrl + click the model"),
                                ("Sharpen", MaskOp::Sharpen, "Ctrl + Alt + click the model"),
                                ("Grow", MaskOp::Grow, "Spread the mask one ring of polygons"),
                                ("Shrink", MaskOp::Shrink, "Pull the mask back one ring of polygons"),
                            ] {
                                if ui.button(label).on_hover_text(hint).clicked() {
                                    a.mask = Some(op);
                                }
                            }
                        });
                        let full = |ui: &mut Ui, label: &str, hint: &str| {
                            ui.add_sized([ui.available_width(), 20.0], egui::Button::new(label)).on_hover_text(hint).clicked()
                        };
                        if full(ui, "Mask By Cavity", "Mask the recessed surface, more where it is deeper") {
                            a.mask = Some(MaskOp::Cavity(ws.cavity_sensitivity.round() as u8));
                        }
                        zslider(ui, "Cavity", &mut ws.cavity_sensitivity, 1.0..=100.0, false, false, 0);
                        if full(ui, "Mask By Polygroups", "Mask the borders between polygroups · Invert to mask their insides") {
                            a.mask = Some(MaskOp::GroupBorders(ws.group_border_width.round() as u8));
                        }
                        zslider(ui, "Border width", &mut ws.group_border_width, 1.0..=12.0, false, false, 0);
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
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ws.mask_picker(ui,m);
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
                                        egui::Button::new(RichText::new(label).size(12.0))
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
                        // Split, merge and extract (ZBrush: SubTool › Split, Merge, Extract).
                        let mut clicked = [false; 3];
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                        let w = (r.width() - 6.0) / 3.0;
                        for (col, (label, on, hint)) in [
                            ("Merge Down", active + 1 < count, "Join the subtool below into this one · not undoable"),
                            ("Split Hidden", m.hidden > 0, "The hidden polygons become their own subtool · not undoable"),
                            ("Groups Split", m.groups > 1, "Each polygroup becomes its own subtool · not undoable"),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                            child.spacing_mut().button_padding = vec2(2.0, 1.0);
                            if !on {
                                child.disable();
                            }
                            clicked[col] = child.put(cell, egui::Button::new(RichText::new(label).size(12.0)).truncate()).on_hover_text(hint).clicked();
                        }
                        a.merge_down |= clicked[0];
                        a.split_hidden |= clicked[1];
                        a.groups_split |= clicked[2];
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.38;
                            if ui
                                .add_sized([w, 20.0], egui::Button::new("Extract"))
                                .on_hover_text("The masked surface becomes a new subtool, a shell Thick thick")
                                .clicked()
                            {
                                a.extract = true;
                            }
                            zslider(ui, "Thick %", &mut ws.extract_thick, 0.2..=20.0, true, false, 1);
                        });
                        // Single subtools to and from files (ZBrush: Tool › Export, Append).
                        ui.add_space(2.0);
                        ui.add(
                            egui::TextEdit::singleline(m.subtool_path)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(12.0)),
                        );
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                        let w = (r.width() - 6.0) / 3.0;
                        for (col, (label, hint)) in [
                            ("Save", "Save the active subtool alone, with its levels, to this path"),
                            ("Load", "Add the subtools saved at this path to the list"),
                            ("Export OBJ", "The active subtool at its shown level, as an OBJ next to this path"),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                            child.spacing_mut().button_padding = vec2(2.0, 1.0);
                            if child
                                .put(cell, egui::Button::new(RichText::new(label).size(12.0)).truncate())
                                .on_hover_text(hint)
                                .clicked()
                            {
                                match col {
                                    0 => a.save_subtool = true,
                                    1 => a.load_subtools = true,
                                    _ => a.export_subtool = true,
                                }
                            }
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
                self.sec(ui, h, id, "boolean", "Boolean", None, false, true, &mut |ws, ui| {
                    ui.checkbox(&mut ws.live_boolean, "Live Boolean")
                        .on_hover_text("The viewport shows the result of every visible subtool, top to bottom");
                    let active = m.subtools.iter().find(|r| r.active).map_or(Default::default(), |r| r.boolean);
                    let ops = crate::boolean::BoolOp::ALL;
                    let names: Vec<&str> = ops.iter().map(|o| o.name()).collect();
                    let at = ops.iter().position(|o| *o == active).unwrap_or(0);
                    if let Some(i) = seg(ui, &names, at, false) {
                        a.boolean_op = Some(ops[i]);
                    }
                    zslider(ui, "Resolution", &mut ws.boolean_resolution, 64.0..=1024.0, false, false, 0);
                    if ui
                        .add_sized([ui.available_width(), 20.0], egui::Button::new("Make Boolean Mesh"))
                        .on_hover_text("Build the result as a new closed subtool; the visible ones are hidden")
                        .clicked()
                    {
                        a.make_boolean = true;
                    }
                    note(ui, "Add joins, Subtract carves, Intersect keeps the overlap · each subtool acts on the visible ones above it · Resolution: Remesh voxels across two units");
                });
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
                            let released = if kind == Deform::Size {
                                // ZBrush's x y z toggles: Size scales along the lit axes only.
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    let w = ui.available_width() - 3.0 * 20.0;
                                    let v = ui.allocate_ui(vec2(w, 22.0), |ui| spring_slider(ui, kind, &mut ws.deform_drag)).inner;
                                    for (i, name) in ["X", "Y", "Z"].into_iter().enumerate() {
                                        let on = ws.size_axes[i];
                                        if ui
                                            .add(egui::Button::new(RichText::new(name).size(11.0)).selected(on).min_size(vec2(18.0, 22.0)))
                                            .on_hover_text(format!("Size scales along {name}"))
                                            .clicked()
                                        {
                                            ws.size_axes[i] = !on;
                                        }
                                    }
                                    v
                                })
                                .inner
                            } else {
                                spring_slider(ui, kind, &mut ws.deform_drag)
                            };
                            if let Some(v) = released {
                                a.deform = Some((kind, v));
                            }
                        }
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Mirror And Weld"))
                            .on_hover_text("Make the subtool symmetric: the +X half mirrored onto −X and welded (no subdivision levels)")
                            .clicked()
                        {
                            a.mirror_weld = true;
                        }
                        note(ui, "Drag and release · Offset and Size move every level; the others reshape the shown level (masks hold, one undo step) · Size's X Y Z pick its axes: one axis flattens or stretches · Twist and Taper about Y");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "morph",
                    "Morph Target",
                    None,
                    false,
                    true,
                    &mut |_, ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                        let w = (r.width() - 6.0) / 3.0;
                        for (col, (label, on, hint, op)) in [
                            ("StoreMT", true, "Keep the shown level's shape as the Morph Target", crate::MorphOp::Store),
                            ("Switch", m.morph_level.is_some(), "Swap the shape and the Morph Target", crate::MorphOp::Switch),
                            ("DelMT", m.morph_level.is_some(), "Forget the Morph Target", crate::MorphOp::Delete),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let cell = Rect::from_min_size(r.min + vec2(col as f32 * (w + 3.0), 0.0), vec2(w, 20.0));
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
                            child.spacing_mut().button_padding = vec2(2.0, 1.0);
                            if !on {
                                child.disable();
                            }
                            if child.put(cell, egui::Button::new(RichText::new(label).size(12.0)).truncate()).on_hover_text(hint).clicked() {
                                a.morph = Some(op);
                            }
                        }
                        match m.morph_level {
                            Some(l) => kv(ui, "Stored at level", &(l + 1).to_string()),
                            None => note(ui, "Store a target, sculpt, then brush back to it with the Morph brush or Switch."),
                        }
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "layers",
                    "Layers",
                    None,
                    false,
                    true,
                    &mut |ws, ui| {
                        use crate::LayerOp;
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            if ui.add_sized([w, CONTROL_H], egui::Button::new("New").truncate()).on_hover_text("A new layer, recording").clicked() {
                                a.layer = Some(LayerOp::New);
                            }
                            if ui
                                .add_enabled_ui(!m.layers.is_empty(), |ui| ui.add_sized([w, CONTROL_H], egui::Button::new("Bake All").truncate())).inner
                                .on_hover_text("Keep every layer's effect in the sculpt and remove the layers")
                                .clicked()
                            {
                                a.layer = Some(LayerOp::BakeAll);
                            }
                        });
                        // Newest on top, as in ZBrush.
                        for (i, (name, visible, rec, _)) in m.layers.iter().enumerate().rev() {
                            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                            let p = ui.painter();
                            p.rect_filled(r, 2, if ws.layer_selected == i { ACC_DIM } else if resp.hovered() { BG4 } else { BG2 });
                            let eye = Rect::from_min_size(r.min + vec2(4.0, 3.0), vec2(16.0, 16.0));
                            let recr = Rect::from_min_size(r.min + vec2(24.0, 3.0), vec2(30.0, 16.0));
                            p.circle(eye.center(), 4.5, if *visible { FG } else { Color32::TRANSPARENT }, Stroke::new(1.2, FG2));
                            p.rect_filled(recr, 2, if *rec { Color32::from_rgb(200, 50, 45) } else { BG3 });
                            p.text(recr.center(), Align2::CENTER_CENTER, "REC", FontId::proportional(10.5), if *rec { Color32::WHITE } else { FG3 });
                            p.text(r.left_center() + vec2(60.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(12.5), FG);
                            if resp.clicked() {
                                let at = resp.interact_pointer_pos().unwrap_or(r.center());
                                if eye.expand(2.0).contains(at) {
                                    a.layer = Some(LayerOp::Eye(i));
                                } else if recr.expand(2.0).contains(at) {
                                    a.layer = Some(LayerOp::Rec(i));
                                } else {
                                    ws.layer_selected = i;
                                }
                            }
                        }
                        if let Some(&(_, _, _, intensity)) = m.layers.get(ws.layer_selected) {
                            let i = ws.layer_selected;
                            let mut value = ws.layer_drag.map_or(intensity, |d| d.0);
                            if zslider(ui, "Intensity", &mut value, -1.0..=2.0, false, false, 2) {
                                let start = ws.layer_drag.map_or(intensity, |d| d.1);
                                ws.layer_drag = Some((value, start));
                                a.layer = Some(LayerOp::Intensity(i, value));
                            } else if let Some((_, start)) = ws.layer_drag.filter(|_| !ui.input(|inp| inp.pointer.any_down())) {
                                ws.layer_drag = None;
                                a.layer = Some(LayerOp::IntensityDone(i, start));
                            }
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 4.0) * 0.5;
                                if ui.add_sized([w, CONTROL_H], egui::Button::new("Delete").truncate()).on_hover_text("Remove the layer and its effect").clicked() {
                                    a.layer = Some(LayerOp::Delete(i));
                                }
                                if ui.add_sized([w, CONTROL_H], egui::Button::new("Bake").truncate()).on_hover_text("Keep its effect in the sculpt and remove the layer").clicked() {
                                    a.layer = Some(LayerOp::Bake(i));
                                }
                            });
                        }
                        note(ui, "Layers live on the highest subdivision level · REC sculpts into a layer · the eye shows or hides it · Divide waits until they are baked · not saved in projects yet");
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
                                    .add_sized([w, CONTROL_H], egui::Button::new("Del Lower").truncate())
                                    .on_hover_text("Make this level the base, deleting the ones below · clears undo")
                                    .clicked()
                                {
                                    a.del_lower = true;
                                }
                            });
                            ui.add_enabled_ui(m.level < top, |ui| {
                                if ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Del Higher").truncate())
                                    .on_hover_text("Delete the levels above this one · clears undo")
                                    .clicked()
                                {
                                    a.del_higher = true;
                                }
                            });
                        });
                        note(ui, "D / Shift + D: higher / lower level");
                        // ZBrush: Geometry › Modify Topology.
                        ui.add_enabled_ui(top == 0, |ui| {
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 4.0) * 0.5;
                                if ui
                                    .add_enabled_ui(m.hidden > 0, |ui| ui.add_sized([w, CONTROL_H], egui::Button::new("Del Hidden").truncate())).inner
                                    .on_hover_text("Delete the hidden polygons")
                                    .clicked()
                                {
                                    a.del_hidden = true;
                                }
                                if ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Close Holes").truncate())
                                    .on_hover_text("Cap every open border, each cap a new polygroup")
                                    .clicked()
                                {
                                    a.close_holes = true;
                                }
                            });
                        })
                        .response
                        .on_disabled_hover_text("Needs a subtool without subdivision levels");
                    },
                );
                self.sec(
                    ui,
                    h,
                    id,
                    "wedge",
                    "Remesh",
                    None,
                    true,
                    true,
                    &mut |_, ui| {
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 4.0) * 0.5;
                            let can = m.wedge_refusal.is_none() || m.wedge_on;
                            ui.add_enabled_ui(can, |ui| {
                                let r = ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Remesh").selected(m.wedge_on).truncate())
                                    .on_hover_text("Remesh into an even, closed surface and keep Remesh on, so Ctrl + drag on the background remeshes again (ZBrush: DynaMesh)");
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
                                    .add_sized([w, CONTROL_H], egui::Button::new("Update").truncate())
                                    .on_hover_text("Remesh again now · Ctrl + drag on the background")
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
                            note(ui, "Overlapping parts merge. While Remesh is on, Ctrl + drag on the background remeshes again.");
                        }
                    },
                );
                self.sec(ui, h, id, "zremesher", "ZRemesher", None, false, true, &mut |ws, ui| {
                    let can = m.wedge_refusal.is_none() || m.wedge_on;
                    ui.add_enabled_ui(can, |ui| {
                        let r = ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("ZRemesher"))
                            .on_hover_text("Rebuild the subtool as clean quads whose edge loops follow its form · one undo step");
                        let r = match m.wedge_refusal {
                            Some(why) => r.on_disabled_hover_text(why),
                            None => r,
                        };
                        if r.clicked() {
                            a.zremesh = true;
                        }
                    });
                    zslider(ui, "Target Polygons Count", &mut ws.zremesh.target_k, 0.1..=100.0, true, false, 1);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let w = (ui.available_width() - 6.0) / 3.0;
                        // ZBrush: half, the same or double the subtool's polygons now.
                        let now = m.triangles as f32 / 2000.0;
                        for (label, k) in [("Half", 0.5), ("Same", 1.0), ("Double", 2.0)] {
                            if ui.add_sized([w, CONTROL_H], egui::Button::new(RichText::new(label).size(12.0)).truncate()).on_hover_text(format!("Target {label}: of the quads the subtool has now")).clicked() {
                                ws.zremesh.target_k = (now * k).clamp(0.1, 100.0);
                            }
                        }
                    });
                    zslider(ui, "AdaptiveSize", &mut ws.zremesh.adaptive, 0.0..=100.0, false, false, 0);
                    ui.checkbox(&mut ws.zremesh.keep_groups, "KeepGroups")
                        .on_hover_text("Edge loops run along polygroup borders");
                    note(ui, "Target in thousands of quads · all quads, loops following the form · needs a subtool without subdivision levels");
                });
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
                                    .add_sized([w, CONTROL_H], egui::Button::new("Show all").truncate())
                                    .on_hover_text("Ctrl + Shift + click on the background")
                                    .clicked()
                                {
                                    a.show_all = true;
                                }
                                if ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Invert").truncate())
                                    .on_hover_text("Swap hidden and visible · Ctrl + Shift + drag on the background")
                                    .clicked()
                                {
                                    a.invert_visibility = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 4.0) * 0.5;
                                if ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Grow").truncate())
                                    .on_hover_text("Show one more ring of polygons around the visible ones")
                                    .clicked()
                                {
                                    a.grow_visibility = Some(true);
                                }
                                if ui
                                    .add_sized([w, CONTROL_H], egui::Button::new("Shrink").truncate())
                                    .on_hover_text("Hide the outer ring of the visible polygons")
                                    .clicked()
                                {
                                    a.grow_visibility = Some(false);
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
                            let width = ui.available_width() - ui.spacing().item_spacing.x;
                            let group_width = width * 0.6;
                            if ui
                                .add_sized(
                                    [group_width, 22.0],
                                    egui::Button::new("Group Visible").truncate(),
                                )
                                .on_hover_text("One new polygroup of the visible polygons · Ctrl + W")
                                .clicked()
                            {
                                a.group_visible = true;
                            }
                            ui.add_enabled_ui(m.polyframe_supported, |ui| {
                                if ui
                                    .add_sized(
                                        [width - group_width, 22.0],
                                        egui::Button::new("PolyF").selected(ws.polyframe).truncate(),
                                    )
                                    .on_hover_text("Polygroup colours · Shift + F")
                                    .on_disabled_hover_text("Needs a GPU with primitive-index support")
                                    .clicked()
                                {
                                    ws.polyframe = !ws.polyframe;
                                }
                            });
                        });
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Auto Groups"))
                            .on_hover_text("One polygroup per connected piece of the visible polygons")
                            .clicked()
                        {
                            a.auto_groups = true;
                        }
                        if ui
                            .add_sized([ui.available_width(), 20.0], egui::Button::new("Group By Normals"))
                            .on_hover_text("New polygroups where the surface turns by more than Max Angle")
                            .clicked()
                        {
                            a.group_by_normals = Some(ws.normals_angle.round() as u8);
                        }
                        zslider(ui, "Max Angle", &mut ws.normals_angle, 1.0..=90.0, false, false, 0);
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
                                    .add_sized([w, CONTROL_H], egui::Button::new(label).selected(on).truncate())
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
                    "quicksave",
                    "QuickSave",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        ui.checkbox(&mut ws.quicksave, "QuickSave")
                            .on_hover_text("Save every subtool now and then to a rotating set of five files");
                        ui.add_enabled_ui(ws.quicksave, |ui| {
                            zslider(ui, "Every (min)", &mut ws.quicksave_minutes, 1.0..=60.0, false, false, 0);
                        });
                        if ui.add_sized([ui.available_width(), 20.0], egui::Button::new("QuickSave now")).clicked() {
                            a.quicksave = true;
                        }
                        note(ui, "When something changed and you pause for a few seconds · ~/.local/share/klay/quicksave, QuickSave_1–5.klay · open one with Document › Open");
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
                            ("Ctrl + drag bg, Remesh on", "remesh again"),
                            ("Ctrl + S / Ctrl + O", "save / open project"),
                            ("S / U", "draw size / Z intensity popup"),
                            ("I / O", "RGB intensity / focal shift"),
                            ("[ ]", "brush radius"),
                            ("1 – 5", "brushes"),
                            ("Q W E R", "draw / move / scale / rotate"),
                            ("Y", "Gizmo 3D / action line"),
                            ("W E R + drag model", "action line · Shift: 45°"),
                            ("Drag circle", "middle: move, Scale squashes · end: scale / rotate"),
                            ("Drag ring / line", "move the action line"),
                            ("X", "symmetry"),
                            ("L", "LazyMouse"),
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
                    "curve",
                    "Curve",
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        let kind = m.brush.kind;
                        let modes = Self::stroke_modes(kind);
                        if modes.len() > 1 {
                            let at = modes.iter().position(|x| *x == ws.stroke_mode(kind)).unwrap_or(0);
                            if let Some(i) = seg(ui, &modes, at, false) {
                                ws.set_stroke_mode(kind, modes[i]);
                            }
                        } else {
                            note(ui, &format!("{} strokes {} only", kind.name(), modes[0]));
                        }
                        match ws.stroke_mode(kind) {
                            "DragRect" => note(ui, "Press where the stamp goes, drag to size and turn it"),
                            "DragDot" => note(ui, "Press and drag to slide one stamp; it sets on release"),
                            _ => {}
                        }
                        zslider(ui, "Curve smooth", &mut ws.curve_smooth, 0.0..=2.0, false, false, 2);
                        zslider(ui, "Curve step", &mut ws.curve_step, 0.16..=1.0, false, false, 2);
                        if kind.is_insert() {
                            zslider(ui, "Tube taper", &mut ws.tube_taper, 0.0..=1.0, false, false, 2);
                            let mut sides = ws.tube_sides as f32;
                            zslider(ui, "Tube sides", &mut sides, Tube::MIN_SIDES as f32..=Tube::MAX_SIDES as f32, false, false, 0);
                            ws.tube_sides = sides.round() as u32;
                            note(ui, "Release adds the tube · drag the curve to bend it · Draw Size, taper and sides update it · Esc lets it go");
                        } else {
                            note(ui, "Release applies along the curve · drag the curve to bend it · Esc lets it go");
                        }
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
                        zslider(ui, "Spacing", &mut ws.spacing, 0.03..=1.0, false, false, 2);
                        // How each dab's alpha and texture are turned (ZBrush: Roll, spray strokes).
                        ui.label(RichText::new("Alpha rotation").size(12.0).color(FG2));
                        let turns = crate::sculpt::AlphaTurn::ALL;
                        let names: Vec<&str> = turns.iter().map(|t| t.name()).collect();
                        let at = turns.iter().position(|t| *t == ws.assets.turn).unwrap_or(0);
                        if let Some(i) = seg(ui, &names, at, false) {
                            ws.assets.turn = turns[i];
                        }
                        zslider(ui, "Angle", &mut ws.assets.angle, -180.0..=180.0, false, false, 0);
                        ui.checkbox(&mut ws.lazy_mouse, "LazyMouse")
                            .on_hover_text("The brush trails the cursor on a string, for steady lines · L");
                        ui.add_enabled_ui(ws.lazy_mouse, |ui| {
                            zslider(ui, "LazyRadius", &mut ws.lazy_radius, 5.0..=300.0, true, false, 0);
                        });
                        note(ui, "Spacing: dabs per Draw Size along the stroke (ZBrush: LazyStep) · LazyRadius in pixels · pen pressure: Brush › Tablet pressure");
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
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            ui.checkbox(&mut ws.symmetry, "X").on_hover_text("Mirror across X · X");
                            ui.checkbox(&mut ws.symmetry_y, "Y").on_hover_text("Mirror across Y");
                            ui.checkbox(&mut ws.symmetry_z, "Z").on_hover_text("Mirror across Z");
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut ws.radial, "Radial").on_hover_text("Copies turned evenly about an axis");
                            ui.add_enabled_ui(ws.radial, |ui| {
                                let axes = crate::symmetry::Axis::ALL;
                                let names: Vec<&str> = axes.iter().map(|a| a.name()).collect();
                                let at = axes.iter().position(|a| *a == ws.radial_axis).unwrap_or(1);
                                if let Some(i) = seg(ui, &names, at, false) {
                                    ws.radial_axis = axes[i];
                                }
                            });
                        });
                        ui.add_enabled_ui(ws.radial, |ui| {
                            zslider(ui, "RadialCount", &mut ws.radial_count, 2.0..=64.0, false, false, 0);
                        });
                        let sym = ws.sym();
                        kv(ui, "Copies", &sym.images().len().to_string());
                        note(ui, "Brushes, masks and curves follow every copy · Clip Line mirrors across X only");
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
                    None,
                    true,
                    true,
                    &mut |ws, ui| {
                        let modes = [Mode::Draw, Mode::Move, Mode::Scale, Mode::Rotate];
                        let at = modes.iter().position(|m| *m == ws.mode).unwrap_or(0);
                        if let Some(i) = seg(ui, &["Draw", "Move", "Scale", "Rotate"], at, false) {
                            ws.mode = modes[i];
                        }
                        ui.checkbox(&mut ws.gizmo, "Gizmo 3D")
                            .on_hover_text("A manipulator instead of the action line in Move / Scale / Rotate · Y");
                        if ws.gizmo {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                let w = (ui.available_width() - 3.0) / 2.0;
                                if ui.add_sized([w, 20.0], egui::Button::new("Unmasked Center")).on_hover_text("Put the gizmo at the centre of what is unmasked and shown").clicked() {
                                    a.gizmo_center = true;
                                }
                                if ui.add_sized([w, 20.0], egui::Button::new("Reset")).on_hover_text("Gizmo back to the subtool centre, turned to the world axes").clicked() {
                                    a.gizmo_reset = true;
                                }
                            });
                        }
                        note(ui, if ws.gizmo {
                            "Arrows move along X / Y / Z, boxes squash or stretch along them, rings turn about them, the outer ring turns in the view, the circle moves freely, the square scales · Alt + click the model: gizmo there, on the surface · Alt + drag a handle: move or turn the gizmo alone"
                        } else {
                            "Drag on the model to draw the action line · Y for Gizmo 3D"
                        });
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
                        ui.add_enabled_ui(ws.perspective, |ui| {
                            zslider(ui, "Angle of view", &mut ws.fov, 10.0..=120.0, false, false, 0);
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
                                    .add_sized([w, CONTROL_H], egui::Button::new(label).truncate())
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
                                .add_sized([w, CONTROL_H], egui::Button::new("Reset").truncate())
                                .clicked()
                            {
                                // The document colours are not layout: they stay.
                                let document = ws.layout.document;
                                ws.layout = DockLayout::default();
                                ws.layout.document = document;
                                ws.flash(ui.ctx(), "Layout reset".to_string());
                            }
                            if ui
                                .add_sized([w, CONTROL_H], egui::Button::new("Collapse all").truncate())
                                .clicked()
                            {
                                for p in &mut ws.layout.palettes {
                                    p.open = false;
                                }
                            }
                            if ui
                                .add_sized([w, CONTROL_H], egui::Button::new("Expand all").truncate())
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
                                FontId::proportional(13.0),
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

    /// Turn Curve mode on or off for one brush (Stroke › Curve).
    pub fn set_curve_mode(&mut self, kind: BrushKind, on: bool) {
        self.curve_kinds.retain(|&k| k != kind);
        self.drag_kinds.retain(|&(k, _)| k != kind);
        if on {
            self.curve_kinds.push(kind);
        }
    }

    /// The stroke `kind` uses: "Freehand", "Curve", "DragRect" or "DragDot".
    pub fn stroke_mode(&self, kind: BrushKind) -> &'static str {
        if crate::curve::always(kind) || (crate::curve::capable(kind) && self.curve_kinds.contains(&kind)) {
            return "Curve";
        }
        match self.drag_kinds.iter().find(|(k, _)| *k == kind) {
            Some((_, true)) if kind.stamps() => "DragRect",
            Some((_, false)) if kind.stamps() => "DragDot",
            _ => "Freehand",
        }
    }

    /// The strokes `kind` can use, in the order the stroke thumbnail cycles them.
    pub fn stroke_modes(kind: BrushKind) -> Vec<&'static str> {
        if crate::curve::always(kind) {
            return vec!["Curve"];
        }
        let mut modes = vec!["Freehand"];
        if crate::curve::capable(kind) {
            modes.push("Curve");
        }
        if kind.stamps() {
            modes.extend(["DragRect", "DragDot"]);
        }
        modes
    }

    /// Set the stroke `kind` uses (one of [`Workspace::stroke_modes`]).
    pub fn set_stroke_mode(&mut self, kind: BrushKind, mode: &str) {
        self.set_curve_mode(kind, mode == "Curve");
        match mode {
            "DragRect" => self.drag_kinds.push((kind, true)),
            "DragDot" => self.drag_kinds.push((kind, false)),
            _ => {}
        }
    }

    // ------------------------------------------------------------ canvas
    fn canvas_overlays(&mut self, ui: &mut Ui, m: &Model<'_>, a: &mut Actions) {
        if let Some(c) = &self.curve_preview {
            // ZBrush draws a curve as a dotted line on the surface; it lights up under the
            // cursor, where a drag bends it.
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("curve"))).with_clip_rect(self.canvas);
            let (color, width) = if c.hot || c.drawing { (ACC, 2.0) } else { (Color32::from_rgb(236, 226, 205), 1.5) };
            for piece in &c.pieces {
                if piece.len() < 2 {
                    continue;
                }
                p.extend(egui::Shape::dashed_line(piece, Stroke::new(width, color), 6.0, 4.0));
                if !c.drawing {
                    for end in [piece[0], *piece.last().unwrap()] {
                        p.circle_filled(end, 3.0, color);
                    }
                }
            }
        }
        if let Some(g)=&self.gesture_preview {
            let p=ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground,Id::unique("brush-gesture"))).with_clip_rect(self.canvas);
            let start=pos2(g.start.x,g.start.y);let end=pos2(g.end.x,g.end.y);
            let color=if g.alternate {Color32::from_rgb(235,95,75)}else{ACC};
            let stroke=Stroke::new(1.5,color);let rect=Rect::from_two_pos(start,end);
            match g.kind {
                BrushKind::MaskRect=>{p.rect_filled(rect,0,color.gamma_multiply(0.12));p.rect_stroke(rect,0,stroke,StrokeKind::Inside);},
                BrushKind::MaskCircle=>{
                    let points:Vec<_>=(0..64).map(|i|{let angle=i as f32/64.0*std::f32::consts::TAU;rect.center()+vec2(angle.cos(),angle.sin())*rect.size()*0.5}).collect();
                    p.add(egui::Shape::convex_polygon(points,color.gamma_multiply(0.12),stroke));
                },
                BrushKind::MaskLasso=>{p.add(egui::Shape::closed_line(g.points.iter().map(|p|pos2(p.x,p.y)).collect(),stroke));},
                _=>{
                    p.line_segment([start,end],stroke);let d=(end-start).normalized();
                    let normal=vec2(d.y,-d.x)*if g.kind==BrushKind::ClipLine&&g.alternate{-1.0}else{1.0};
                    if start.distance(end)>4.0 {p.arrow(start.lerp(end,0.5),normal*22.0,stroke);}
                },
            }
            p.text(end+vec2(8.0,8.0),Align2::LEFT_TOP,g.kind.name(),FontId::proportional(12.0),color);
        }
        if let Some((brush, cursor)) = self.lazy_line {
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("lazy-line")));
            let color = Color32::from_rgb(235, 80, 55);
            p.line_segment([brush, cursor], Stroke::new(1.0, color));
            p.circle_stroke(brush, 3.0, Stroke::new(1.2, color));
        }
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
                        egui::Button::new(RichText::new(projection).font(font(12.5, Weight::Medium)).color(FG))
                            .fill(Color32::from_rgba_unmultiplied(20, 22, 24, 220))
                            .min_size(vec2(0.0, 24.0)),
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
                    Frame::NONE
                        .fill(Color32::from_rgba_unmultiplied(20, 22, 24, 200))
                        .corner_radius(4)
                        .inner_margin(Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new(active).font(font(12.5, Weight::Regular)).color(FG2));
                        });
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
                        ("X", Color32::from_rgb(240, 127, 110), m.axes[0], (std::f32::consts::FRAC_PI_2, 0.0)),
                        ("Y", Color32::from_rgb(127, 211, 106), m.axes[1], (0.0, std::f32::consts::FRAC_PI_2)),
                        ("Z", Color32::from_rgb(120, 168, 240), m.axes[2], (0.0, 0.0)),
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
                            font(11.5, Weight::SemiBold),
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
        {
            let text = format!(
                "{} · Size {:.2} · Z {:.0} · {}",
                m.brush.kind.name(),
                m.brush.radius,
                m.brush.strength * 100.0,
                if self.subtract { "Zsub" } else { "Zadd" }
            );
            let galley = painter.layout_no_wrap(text, font(12.5, Weight::Regular), FG2);
            let chip = Rect::from_min_size(
                pos2(canvas.left() + 8.0, canvas.bottom() - 8.0 - galley.size().y - 10.0),
                galley.size() + vec2(20.0, 10.0),
            );
            painter.rect_filled(chip, 4, Color32::from_rgba_unmultiplied(20, 22, 24, 220));
            painter.rect_stroke(chip, 4, Stroke::new(1.0, LINE2), StrokeKind::Inside);
            painter.galley(chip.min + vec2(10.0, 5.0), galley, FG2);
        }
        if self.focus {
            painter.text(
                pos2(canvas.right() - 10.0, canvas.top() + 14.0),
                Align2::RIGHT_CENTER,
                "Tab or Esc restores the interface",
                FontId::proportional(12.0),
                FG3,
            );
        }
        if let Some(error) = &self.persistence_error {
            painter.text(
                canvas.center_top() + vec2(0.0, 40.0),
                Align2::CENTER_TOP,
                error,
                FontId::proportional(12.0),
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
                FontId::proportional(11.5),
                ACC,
            );
            if hovered && ui.input(|i| i.pointer.any_released()) {
                egui::DragAndDrop::take_payload::<PaletteId>(ui.ctx());
                self.pending_move = Some((*id, Place::Tray(side), None));
            }
        }
        if let Some(pos) = pointer {
            let galley =
                painter.layout_no_wrap(id.name().to_string(), FontId::proportional(13.0), FG);
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
        let flips = matches!(kind, Some(BrushKind::Draw | BrushKind::Inflate | BrushKind::DamStandard | BrushKind::Pinch | BrushKind::Crease | BrushKind::Clay | BrushKind::ClayBuildup | BrushKind::Blob | BrushKind::Layer | BrushKind::Nudge | BrushKind::Sharpen | BrushKind::FormSoft | BrushKind::FormHard | BrushKind::Zipper | BrushKind::Stitch | BrushKind::Rake | BrushKind::ClayTubes | BrushKind::InflateBalloon | BrushKind::Magnify | BrushKind::OrbCracks | BrushKind::MalletFast));
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
    crate::brush_preview::image(kind, material, sub)
}

// ---------------------------------------------------------------- widgets
/// Where the key light starts (Light › Reset): upper left, in front.
pub const LIGHT_DEFAULT: glam::Vec2 = glam::Vec2::new(0.44, 0.59);

/// Light placement (ZBrush: the Light palette's sphere): a lit sphere; drag on it to put the
/// light there.
fn light_placer(ui: &mut Ui, light: &mut glam::Vec2) {
    let size = 96.0;
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size + 6.0), Sense::click_and_drag());
    let c = r.center();
    let radius = size * 0.5;
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        let mut v = glam::Vec2::new((p.x - c.x) / radius, (c.y - p.y) / radius);
        if v.length() > 0.98 {
            v = v.normalize() * 0.98;
        }
        *light = v;
    }
    let p = ui.painter();
    // A few rings shaded by how much each faces the light.
    let l = glam::Vec3::new(light.x, light.y, (1.0 - light.length_squared()).max(0.0).sqrt());
    for k in (1..=24).rev() {
        let f = k as f32 / 24.0;
        let rr = radius * f;
        // Shade the disc toward the light: an offset gradient approximates a lit sphere.
        let centre = c + vec2(light.x, -light.y) * radius * (1.0 - f) * 0.55;
        let lit = (0.25 + 0.75 * (1.0 - f).powf(0.7) * l.z.max(0.2)).clamp(0.0, 1.0);
        let g = (40.0 + lit * 190.0) as u8;
        p.circle_filled(centre, rr, Color32::from_rgb(g, g, g));
    }
    p.circle_stroke(c, radius, Stroke::new(1.0, LINE));
    let at = c + vec2(light.x, -light.y) * radius;
    p.circle_filled(at, 4.0, Color32::from_rgb(255, 214, 120));
    p.circle_stroke(at, 6.0, Stroke::new(1.0, Color32::from_rgb(255, 214, 120)));
}

/// A Tablet Pressure curve: pressure across, output up. Drag a point, click empty space to
/// add one, double-click a point to remove it. `live` marks the pen's pressure now.
fn pressure_curve_editor(ui: &mut Ui, curve: &mut crate::pressure::PressureCurve, drag: &mut Option<usize>, live: Option<f32>) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::click_and_drag());
    let to_screen = |p: glam::Vec2| pos2(r.left() + p.x * r.width(), r.bottom() - p.y * r.height());
    let to_curve = |q: Pos2| glam::Vec2::new((q.x - r.left()) / r.width(), (r.bottom() - q.y) / r.height());
    let near = |q: Pos2| curve.points.iter().position(|&p| to_screen(p).distance(q) < 7.0);
    if let Some(q) = resp.interact_pointer_pos() {
        if resp.double_clicked() {
            if let Some(i) = near(q) {
                curve.remove(i);
            }
            *drag = None;
        } else if resp.drag_started() || (resp.clicked() && drag.is_none()) {
            *drag = near(q).or_else(|| curve.insert(to_curve(q)));
        }
        if resp.dragged() {
            if let Some(i) = *drag {
                curve.drag(i, to_curve(q));
            }
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        *drag = None;
    }
    let p = ui.painter();
    p.rect_filled(r, 2, BG2);
    p.rect_stroke(r, 2, Stroke::new(1.0, LINE), StrokeKind::Inside);
    for k in 1..4 {
        let f = k as f32 / 4.0;
        p.vline(r.left() + f * r.width(), r.y_range(), (1.0, LINE2));
        p.hline(r.x_range(), r.bottom() - f * r.height(), (1.0, LINE2));
    }
    let line: Vec<Pos2> = (0..=48).map(|k| {
        let x = k as f32 / 48.0;
        to_screen(glam::Vec2::new(x, curve.eval(x)))
    }).collect();
    p.add(egui::Shape::line(line, Stroke::new(1.5, ACC)));
    for (i, &q) in curve.points.iter().enumerate() {
        p.circle_filled(to_screen(q), if *drag == Some(i) { 4.5 } else { 3.5 }, FG);
    }
    if let Some(x) = live {
        let at = to_screen(glam::Vec2::new(x, curve.eval(x)));
        p.vline(at.x, r.y_range(), (1.0, ACC_LINE));
        p.circle_stroke(at, 4.0, Stroke::new(1.2, ACC_LINE));
    }
}

/// ZBrush-style slider: label inside the bar, value on a neutral field at the
/// right. `line` draws the shelf variant with a thin value line underneath.
/// How a slider's position maps to its value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SliderScale {
    Linear,
    Logarithmic,
    /// value = min + (max - min) · position^k: fine steps at the low end, rising steadily
    /// instead of shooting up at the top like a logarithmic scale.
    Power(f32),
}

impl SliderScale {
    /// The value at slider position `f` (0..=1).
    pub fn value(self, f: f32, min: f32, max: f32) -> f32 {
        match self {
            Self::Linear => min + f * (max - min),
            Self::Logarithmic => min * (max / min).powf(f),
            Self::Power(k) => min + f.powf(k) * (max - min),
        }
    }

    /// The slider position (0..=1) of `v`.
    pub fn position(self, v: f32, min: f32, max: f32) -> f32 {
        let f = match self {
            Self::Linear => (v - min) / (max - min),
            Self::Logarithmic => (v / min).ln() / (max / min).ln(),
            Self::Power(k) => ((v - min) / (max - min)).max(0.0).powf(1.0 / k),
        };
        f.clamp(0.0, 1.0)
    }
}

/// Draw Size: the range of every Draw Size slider, and its scale.
pub const DRAW_SIZE: std::ops::RangeInclusive<f32> = 0.01..=2.0;
pub const DRAW_SIZE_SCALE: SliderScale = SliderScale::Power(2.0);

/// A Draw Size slider (top shelf, Brush palette, quick menu).
pub fn draw_size_slider(ui: &mut Ui, label: &str, value: &mut f32, line: bool) -> bool {
    zslider_scaled(ui, label, value, DRAW_SIZE, DRAW_SIZE_SCALE, line, 2)
}

pub fn zslider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
    line: bool,
    decimals: usize,
) -> bool {
    let scale = if logarithmic { SliderScale::Logarithmic } else { SliderScale::Linear };
    zslider_scaled(ui, label, value, range, scale, line, decimals)
}

pub fn zslider_scaled(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    scale: SliderScale,
    line: bool,
    decimals: usize,
) -> bool {
    let _ = line;
    let (min, max) = (*range.start(), *range.end());
    let (r, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_H), Sense::click_and_drag());
    // ZBrush keeps the name on the bar; the value gets a field of its own on the right so
    // it never sits on the moving fill.
    let value_text = format!("{:.*}", decimals, *value);
    let value_galley = ui.painter().layout_no_wrap(value_text, font(13.0, Weight::SemiBold), FG);
    let value_w = (value_galley.size().x + 16.0).max(48.0).min(r.width() * 0.4);
    let field = Rect::from_min_max(pos2(r.right() - value_w, r.top()), r.max);
    let bar = Rect::from_min_max(r.min, pos2(field.left() - 4.0, r.bottom()));
    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
            let v = scale.value(f, min, max);
            if v != *value {
                *value = v;
                changed = true;
            }
        }
    }
    let frac = scale.position(*value, min, max);
    let p = ui.painter();
    // Disabled UIs already apply the theme opacity to their painter.
    p.rect_filled(bar, 4, if resp.hovered() { BG4 } else { BG3 });
    // Clay fill up to the value with an accent rule along its bottom edge: the position
    // reads at a glance without a marker crossing the name.
    let x = bar.left() + 1.0 + frac * (bar.width() - 2.0);
    if frac > 0.0 {
        let fill = Rect::from_min_max(bar.min + vec2(1.0, 1.0), pos2(x, bar.bottom() - 1.0));
        p.rect_filled(fill, CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, ACC_FILL);
        p.rect_filled(
            Rect::from_min_max(pos2(fill.left(), fill.bottom() - 2.0), fill.max),
            CornerRadius { nw: 0, sw: 3, ne: 0, se: 0 },
            ACC,
        );
    }
    p.rect_stroke(bar, 4, Stroke::new(1.0, LINE), StrokeKind::Inside);
    p.with_clip_rect(bar.shrink(1.0).intersect(p.clip_rect())).text(
        pos2(bar.left() + 9.0, bar.center().y),
        Align2::LEFT_CENTER,
        label,
        font(13.0, Weight::Medium),
        FG,
    );
    p.rect_filled(field, 4, BG0);
    p.rect_stroke(field, 4, Stroke::new(1.0, LINE), StrokeKind::Inside);
    p.galley(
        pos2(field.right() - 8.0 - value_galley.size().x, field.center().y - value_galley.size().y * 0.5),
        value_galley,
        FG,
    );
    changed
}

/// Segmented buttons. Returns the index clicked this frame.
fn seg(ui: &mut Ui, labels: &[&str], selected: usize, tall: bool) -> Option<usize> {
    let h = if tall { 32.0 } else { CONTROL_H };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let enabled = ui.is_enabled();
    let w = r.width() / labels.len() as f32;
    let mut clicked = None;
    ui.painter().rect_filled(r, 4, BG3);
    for (i, label) in labels.iter().enumerate() {
        let cell = Rect::from_min_size(r.min + vec2(w * i as f32, 0.0), vec2(w, h));
        let resp = ui.interact(cell, ui.scope_id().with(("seg", label, i)), Sense::click());
        let on = i == selected;
        let p = ui.painter();
        let corner = |left: bool, right: bool| CornerRadius {
            nw: if left { 4 } else { 0 },
            sw: if left { 4 } else { 0 },
            ne: if right { 4 } else { 0 },
            se: if right { 4 } else { 0 },
        };
        let round = corner(i == 0, i + 1 == labels.len());
        if on {
            p.rect_filled(cell, round, if enabled { ACC } else { BG4 });
        } else if resp.hovered() {
            p.rect_filled(cell, round, BG4);
        }
        p.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            font(13.0, if on { Weight::SemiBold } else { Weight::Medium }),
            if on && enabled { ACC_FG } else if on { FG } else { FG2 },
        );
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    let p = ui.painter();
    p.rect_stroke(r, 4, Stroke::new(1.0, LINE), StrokeKind::Inside);
    for i in 1..labels.len() {
        p.vline(r.left() + w * i as f32, r.y_range(), (1.0, LINE));
    }
    clicked
}

fn small_toggle(ui: &mut Ui, label: &str, on: bool, size: Vec2) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(r, 4, if on { ACC } else if resp.hovered() { BG4 } else { BG3 });
    p.rect_stroke(r, 4, Stroke::new(1.0, if on { ACC } else { LINE }), StrokeKind::Inside);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        font(12.5, if on { Weight::SemiBold } else { Weight::Medium }),
        if on { ACC_FG } else { FG },
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
    // Shelf modes (tall) fill with the accent when on; view toggles get a clay tint and an
    // accent border so several can be on at once without shouting.
    let mode = size.y > 52.0;
    let alpha = if enabled { 1.0 } else { 0.55 };
    let p = ui.painter();
    let fill = if on && mode {
        ACC
    } else if on {
        ACC_DIM
    } else if resp.hovered() && enabled {
        BG4
    } else if mode {
        BG3
    } else {
        TILE
    };
    p.rect_filled(r, 4, fill.gamma_multiply(alpha));
    let border = if on { ACC } else { LINE };
    p.rect_stroke(r, 4, Stroke::new(1.0, border.gamma_multiply(alpha)), StrokeKind::Inside);
    let color = if !enabled {
        FG3
    } else if on && mode {
        ACC_FG
    } else if on {
        Color32::from_rgb(255, 210, 189)
    } else if resp.hovered() {
        FG
    } else {
        FG2
    };
    glyph(p, r.center() - vec2(0.0, if mode { 8.0 } else { 7.0 }), icon, color);
    p.text(
        r.center_bottom() - vec2(0.0, if mode { 11.0 } else { 9.5 }),
        Align2::CENTER_CENTER,
        label,
        font(if mode { 12.0 } else { 11.5 }, Weight::Medium),
        color,
    );
    resp
}

enum Thumb {
    Texture(TextureId),
    Glyph(&'static str),
}

/// Left-shelf thumbnail: a caption (what it is), the image, and its current name.
fn thumb(ui: &mut Ui, caption: &str, content: Thumb, label: &str, on: bool, dim: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(86.0, 106.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(r, 4, if resp.hovered() { BG3 } else { TILE });
    p.rect_stroke(r, 4, Stroke::new(1.0, if on { ACC } else { LINE }), StrokeKind::Inside);
    let cap = p.layout_job(caps(caption, 10.5, FG3));
    p.galley(pos2(r.center().x - cap.size().x * 0.5, r.top() + 6.0), cap, FG3);
    let im = Rect::from_center_size(pos2(r.center().x, r.top() + 52.0), vec2(60.0, 60.0));
    match content {
        Thumb::Texture(tex) => {
            p.image(tex, im, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        Thumb::Glyph(name) => glyph_large(p, im.center(), name, if dim { FG3 } else { FG2 }),
    }
    let color = if dim { FG3 } else { FG };
    let mut job = egui::text::LayoutJob::simple(label.to_owned(), font(12.0, Weight::Medium), color, 80.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = p.layout_job(job);
    p.galley(pos2(r.center().x - galley.size().x * 0.5, r.bottom() - 10.0 - galley.size().y * 0.5), galley, color);
    resp.on_hover_text(label)
}

/// A catalog tile. `code` is the ZBrush brush code drawn in the corner, lit when its
/// first letter has been typed.
fn brush_tile(
    ui: &mut Ui, tex: TextureId, name: &str, key: usize, code: (&str, Option<char>), on: bool, width: f32,
) -> egui::Response {
    let color = if on { Color32::WHITE } else { FG2 };
    let label = ui.painter().layout(
        name.to_owned(),
        font(12.0, if on { Weight::SemiBold } else { Weight::Regular }),
        color,
        width - 6.0,
    );
    let height = width + label.size().y.max(15.0) + 4.0;
    let (r, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let p = ui.painter();
    p.rect_filled(r, 4, if on { ACC_DIM } else if resp.hovered() { BG4 } else { TILE });
    p.rect_stroke(r, 4, Stroke::new(1.0, if on { ACC } else { LINE }), StrokeKind::Inside);
    let im = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(width - 8.0, width - 8.0));
    p.image(tex, im, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    if (1..=9).contains(&key) {
        p.text(im.right_top() + vec2(-3.0, 2.0), Align2::RIGHT_TOP, key.to_string(), font(11.0, Weight::SemiBold), FG);
    }
    if !code.0.is_empty() {
        let lit = code.1.is_some_and(|c| code.0.starts_with(c));
        let galley = p.layout_no_wrap(code.0.to_owned(), font(11.5, Weight::SemiBold), if lit { ACC_FG } else { FG });
        let chip = Rect::from_min_size(r.min + vec2(4.0, 4.0), galley.size() + vec2(8.0, 2.0));
        p.rect_filled(chip, 3, if lit { ACC } else { Color32::from_rgba_unmultiplied(14, 16, 18, 210) });
        p.galley(chip.min + vec2(4.0, 1.0), galley, FG);
    }
    p.galley(pos2(r.center().x - label.size().x * 0.5, r.top() + width - 2.0), label, color);
    let keys = match (code.0.is_empty(), (1..=9).contains(&key)) {
        (false, true) => format!("{name} · B {} · {key}", spaced(code.0)),
        (false, false) => format!("{name} · B {}", spaced(code.0)),
        (true, true) => format!("{name} · {key}"),
        (true, false) => name.to_owned(),
    };
    resp.on_hover_text(keys)
}

/// "DS" → "D S" for shortcut hints.
fn spaced(code: &str) -> String {
    code.chars().map(String::from).collect::<Vec<_>>().join(" ")
}

/// HSV picker: saturation/value square plus a hue strip on its right (left shelf).
fn color_picker(ui: &mut Ui, color: &mut Hsva) {
    let (r, _) = ui.allocate_exact_size(vec2(86.0, 72.0), Sense::hover());
    let sv = Rect::from_min_max(r.min, pos2(r.right() - 15.0, r.bottom()));
    let hue = Rect::from_min_max(pos2(r.right() - 12.0, r.top()), r.max);
    hsv_picker(ui, color, sv, hue, false);
}

/// A large square picker with the hue bar underneath it (Space menu).
fn color_picker_stacked(ui: &mut Ui, color: &mut Hsva, width: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(width, width + 22.0), Sense::hover());
    let sv = Rect::from_min_size(r.min, vec2(width, width));
    let hue = Rect::from_min_max(pos2(r.left(), r.bottom() - 14.0), r.max);
    hsv_picker(ui, color, sv, hue, true);
}

fn hsv_picker(ui: &mut Ui, color: &mut Hsva, sv: Rect, hue: Rect, hue_across: bool) {
    let sv_resp = ui.interact(sv, ui.scope_id().with("picker-sv"), Sense::click_and_drag());
    if sv_resp.dragged() || sv_resp.clicked() {
        if let Some(p) = sv_resp.interact_pointer_pos() {
            color.s = ((p.x - sv.left()) / sv.width()).clamp(0.0, 1.0);
            color.v = 1.0 - ((p.y - sv.top()) / sv.height()).clamp(0.0, 1.0);
        }
    }
    let hue_resp = ui.interact(hue, ui.scope_id().with("picker-hue"), Sense::click_and_drag());
    if hue_resp.dragged() || hue_resp.clicked() {
        if let Some(p) = hue_resp.interact_pointer_pos() {
            let t = if hue_across {
                (p.x - hue.left()) / hue.width()
            } else {
                (p.y - hue.top()) / hue.height()
            };
            color.h = t.clamp(0.0, 0.999);
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
    let marker = pos2(sv.left() + color.s * sv.width(), sv.top() + (1.0 - color.v) * sv.height());
    let big = sv.width() > 100.0;
    p.circle_stroke(marker, if big { 6.5 } else { 4.5 }, Stroke::new(1.0, Color32::BLACK));
    p.circle_stroke(marker, if big { 5.5 } else { 3.5 }, Stroke::new(1.5, Color32::WHITE));
    // hue strip
    let mut strip = egui::Mesh::default();
    let steps = 12;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let c = Color32::from(Hsva::new(t.min(0.999), 1.0, 1.0, 1.0));
        if hue_across {
            let x = hue.left() + t * hue.width();
            strip.colored_vertex(pos2(x, hue.top()), c);
            strip.colored_vertex(pos2(x, hue.bottom()), c);
        } else {
            let y = hue.top() + t * hue.height();
            strip.colored_vertex(pos2(hue.left(), y), c);
            strip.colored_vertex(pos2(hue.right(), y), c);
        }
        if i > 0 {
            let b = (i * 2) as u32;
            strip.add_triangle(b - 2, b - 1, b);
            strip.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(egui::Shape::mesh(strip));
    p.rect_stroke(hue, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let hm = if hue_across {
        let hx = hue.left() + color.h * hue.width();
        Rect::from_min_max(pos2(hx - 2.0, hue.top() - 2.0), pos2(hx + 2.0, hue.bottom() + 2.0))
    } else {
        let hy = hue.top() + color.h * hue.height();
        Rect::from_min_max(pos2(hue.left() - 2.0, hy - 2.0), pos2(hue.right() + 2.0, hy + 2.0))
    };
    p.rect_stroke(hm.expand(1.0), 0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Outside);
    p.rect_stroke(hm, 0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
}

fn menu_item(ui: &mut Ui, label: &str, marked: bool) -> egui::Response {
    let (r, resp) =
        ui.allocate_exact_size(vec2(ui.available_width().max(150.0), 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 3, ACC);
    }
    let color = if resp.hovered() { ACC_FG } else { FG };
    if marked {
        p.circle_filled(r.left_center() + vec2(10.0, 0.0), 2.5, color);
    }
    p.text(
        r.left_center() + vec2(22.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font(13.0, Weight::Regular),
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
    ui.add_space(6.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, SHELF_H - 18.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), (1.0, LINE));
    ui.add_space(6.0);
}

fn nav_separator(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(40.0, 3.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, (1.0, LINE));
}

fn tag(p: &egui::Painter, right_center: Pos2, text: &str) {
    tag_colored(p, right_center, text, false);
}

fn tag_colored(p: &egui::Painter, right_center: Pos2, text: &str, accent: bool) {
    p.text(
        right_center,
        Align2::RIGHT_CENTER,
        text,
        font(12.0, Weight::Medium),
        if accent { ACC_TEXT } else { FG3 },
    );
}

fn note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(font(12.0, Weight::Regular)).color(FG3)).wrap());
}

fn kv(ui: &mut Ui, key: &str, value: &str) {
    let key_font = font(13.0, Weight::Regular);
    let value_font = font(13.0, Weight::Medium);
    let key_width = ui
        .painter()
        .layout_no_wrap(key.to_owned(), key_font.clone(), FG2)
        .size()
        .x;
    let value_width = ui
        .painter()
        .layout_no_wrap(value.to_owned(), value_font.clone(), FG)
        .size()
        .x;
    let key_text = RichText::new(key).font(key_font).color(FG2);
    let value_text = RichText::new(value).font(value_font).color(FG);
    if key_width + value_width + ui.spacing().item_spacing.x > ui.available_width() {
        // Long counts and units wrap within the saved tray width.
        ui.label(key_text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(value_text).wrap());
        });
    } else {
        ui.horizontal(|ui| {
            ui.set_height(22.0);
            ui.label(key_text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(value_text);
            });
        });
    }
}

/// A SubTool list line: name and size, highlighted when active, with an eye toggle on the
/// right. Returns (row clicked, eye clicked).
fn subtool_row(ui: &mut Ui, index: usize, row: &SubToolRow) -> (bool, bool) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let eye_rect = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(24.0, 24.0));
    let eye = ui.interact(eye_rect, Id::unique(("subtool-eye", index)), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        4,
        if row.active {
            ACC_DIM
        } else if resp.hovered() {
            BG4
        } else {
            TILE
        },
    );
    p.rect_stroke(
        r,
        4,
        Stroke::new(1.0, if row.active { ACC } else { LINE }),
        StrokeKind::Inside,
    );
    let text = if row.visible || row.active { FG } else { FG3 };
    p.text(
        pos2(r.left() + 10.0, r.top() + 13.0),
        Align2::LEFT_CENTER,
        &row.name,
        font(13.0, if row.active { Weight::SemiBold } else { Weight::Medium }),
        text,
    );
    if row.boolean != crate::boolean::BoolOp::Add {
        p.text(
            pos2(eye_rect.left() - 6.0, r.top() + 13.0),
            Align2::RIGHT_CENTER,
            row.boolean.glyph(),
            FontId::proportional(14.0),
            ACC,
        );
    }
    let detail = if !row.drawn {
        "stand-in did not fit on the GPU".to_string()
    } else if row.top > 0 {
        format!(
            "{} vertices · SDiv {} of {} · {} MB",
            crate::fmt_count(row.vertices),
            row.level,
            row.top,
            row.bytes / 1_000_000
        )
    } else {
        format!("{} vertices · {} MB", crate::fmt_count(row.vertices), row.bytes / 1_000_000)
    };
    p.text(
        pos2(r.left() + 10.0, r.bottom() - 12.0),
        Align2::LEFT_CENTER,
        detail,
        font(12.0, Weight::Regular),
        FG2,
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
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_H), Sense::drag());
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
    p.rect_filled(r, 4, if resp.hovered() || value != 0.0 { BG4 } else { BG3 });
    p.rect_stroke(r, 4, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let cx = r.center().x;
    let x = cx + value / 100.0 * r.width() * 0.5;
    if value != 0.0 {
        p.rect_filled(
            Rect::from_x_y_ranges(cx.min(x)..=cx.max(x), r.y_range()),
            2,
            ACC_FILL,
        );
    }
    p.vline(cx, r.y_range().shrink(4.0), Stroke::new(1.0, FG3));
    p.text(
        pos2(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        kind.label(),
        font(13.0, Weight::Medium),
        FG,
    );
    p.text(
        pos2(r.right() - 6.0, r.center().y),
        Align2::RIGHT_CENTER,
        format!("{value:+.0}"),
        font(13.0, Weight::SemiBold),
        if value != 0.0 { ACC_TEXT } else { FG3 },
    );
    released
}

fn full_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), CONTROL_H], egui::Button::new(label))
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
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("inter", &include_bytes!("../assets/fonts/Inter-Regular.ttf")[..]),
        ("inter-medium", &include_bytes!("../assets/fonts/Inter-Medium.ttf")[..]),
        ("inter-semibold", &include_bytes!("../assets/fonts/Inter-SemiBold.ttf")[..]),
    ] {
        fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }
    let fallback = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    for (family, first) in [
        (egui::FontFamily::Proportional, "inter"),
        (egui::FontFamily::Name("medium".into()), "inter-medium"),
        (egui::FontFamily::Name("semibold".into()), "inter-semibold"),
    ] {
        let mut list = vec![first.to_string()];
        list.extend(fallback.iter().cloned());
        fonts.families.insert(family, list);
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG1;
    style.visuals.window_fill = BG2;
    style.visuals.extreme_bg_color = BG0;
    style.visuals.faint_bg_color = BG2;
    style.visuals.override_text_color = Some(FG);
    style.visuals.weak_text_color = Some(FG2);
    style.visuals.disabled_alpha = 0.6;
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
        widget.corner_radius = CornerRadius::same(4);
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
    style.spacing.button_padding = vec2(10.0, 4.0);
    style.spacing.interact_size = vec2(24.0, 24.0);
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 10.0;
    style.spacing.icon_spacing = 7.0;
    style.spacing.menu_margin = Margin::same(3);
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, font(13.0, Weight::Medium));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(12.0));
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
            boolean: Default::default(),
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
                        subtool_path: &mut String::from("subtool.klay"),
                        import_path: &mut String::from("model.obj"),
                        morph_level: None,
                        layers: Vec::new(),
                        undo_names: &[],
                        redo_names: Vec::new(),
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
                egui::Shape::Text(shape) if same_text(&shape.galley.job.text, text) => Some(shape.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("text {text:?} was not rendered"))
    }

    /// Section titles render in capitals; tests name them as written.
    pub(crate) fn same_text(shown: &str, text: &str) -> bool {
        shown == text || shown == text.to_uppercase()
    }

    fn text_count(output: &egui::FullOutput, text: &str) -> usize {
        output
            .shapes
            .iter()
            .filter(|shape| matches!(&shape.shape, egui::Shape::Text(s) if same_text(&s.galley.job.text, text)))
            .count()
    }

    fn click(workspace: &mut Workspace, ctx: &Context, size: Vec2, pos: Pos2) -> egui::FullOutput {
        frame(workspace, ctx, size, vec![egui::Event::PointerMoved(pos)]);
        frame(workspace, ctx, size, vec![pointer(pos, true)]);
        frame(workspace, ctx, size, vec![pointer(pos, false)])
    }

    #[test]
    fn both_logo_and_wordmark_open_the_same_about_modal() {
        for (tag, wordmark) in [("about-logo", false), ("about-wordmark", true)] {
            let (mut workspace, ctx) = test_workspace(tag);
            let size = vec2(1600.0, 1000.0);
            let output = frame(&mut workspace, &ctx, size, vec![]);
            let pos = if wordmark {
                text_position(&output, "Klay") + vec2(3.0, 3.0)
            } else {
                pos2(18.0, MENUBAR_H * 0.5)
            };
            click(&mut workspace, &ctx, size, pos);
            assert!(workspace.about_open);
            frame(&mut workspace, &ctx, size, vec![]);
            let output = frame(&mut workspace, &ctx, size, vec![]);
            assert_eq!(text_count(&output, "About Klay"), 1);
            assert_eq!(text_count(&output, &format!("Version {}", crate::branding::VERSION)), 1);
            assert_eq!(text_count(&output, "Built with Rust"), 1);
            assert_eq!(text_count(&output, "GitHub repository"), 1);
            assert_eq!(text_count(&output, "Close"), 1);
        }
    }

    #[test]
    fn about_closes_with_button_escape_or_backdrop_without_activating_a_palette() {
        let size = vec2(960.0, 640.0);
        for method in ["close", "escape", "backdrop"] {
            let (mut workspace, ctx) = test_workspace(method);
            workspace.about_open = true;
            frame(&mut workspace, &ctx, size, vec![]);
            frame(&mut workspace, &ctx, size, vec![]);
            let output = frame(&mut workspace, &ctx, size, vec![]);
            match method {
                "close" => {
                    let pos = text_position(&output, "Close") + vec2(3.0, 3.0);
                    click(&mut workspace, &ctx, size, pos);
                }
                "escape" => {
                    frame(&mut workspace, &ctx, size, vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                }
                _ => {
                    let pos = text_position(&output, "Alpha") + vec2(3.0, 3.0);
                    click(&mut workspace, &ctx, size, pos);
                    assert_eq!(workspace.dropdown, None);
                }
            }
            assert!(!workspace.about_open, "{method} did not close the modal");
        }
    }

    #[test]
    fn about_repository_link_opens_the_package_repository() {
        let (mut workspace, ctx) = test_workspace("about-repository");
        let size = vec2(1600.0, 1000.0);
        workspace.about_open = true;
        frame(&mut workspace, &ctx, size, vec![]);
        frame(&mut workspace, &ctx, size, vec![]);
        let output = frame(&mut workspace, &ctx, size, vec![]);
        let pos = text_position(&output, "GitHub repository") + vec2(3.0, 3.0);
        let output = click(&mut workspace, &ctx, size, pos);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::OpenUrl(url) if url.url == "https://github.com/ozzy-apps/klay")
        }));
        assert!(workspace.about_open);
    }

    #[test]
    fn shelf_and_stroke_palette_switch_a_brush_between_freehand_and_curve() {
        let (mut workspace, ctx) = test_workspace("curve-mode");
        let size = vec2(1600.0, 1000.0);
        // Zipper, Stitch and Rake start in Curve mode; the test model's brush is Draw.
        assert!(workspace.curve_kinds.contains(&BrushKind::Zipper));
        let output = frame(&mut workspace, &ctx, size, vec![]);
        // The left shelf names the current brush's stroke; a click switches it.
        let shelf = text_position(&output, "Freehand");
        click(&mut workspace, &ctx, size, shelf + vec2(6.0, 4.0));
        assert!(workspace.curve_kinds.contains(&BrushKind::Draw));
        // Stroke › Curve switches it back.
        let output = frame(&mut workspace, &ctx, size, vec![]);
        click(&mut workspace, &ctx, size, text_position(&output, "Stroke") + vec2(6.0, 4.0));
        frame(&mut workspace, &ctx, size, vec![]);
        let output = frame(&mut workspace, &ctx, size, vec![]);
        assert!(text_count(&output, "Curve smooth") == 1, "the Curve section is open");
        click(&mut workspace, &ctx, size, text_position(&output, "Freehand") + vec2(6.0, 4.0));
        assert!(!workspace.curve_kinds.contains(&BrushKind::Draw));
        assert!(workspace.curve_kinds.contains(&BrushKind::Zipper));
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
            assert_eq!(image.size, [160, 160]);
            assert_eq!(image[(0, 0)].a(), 0, "corner should be transparent");
            assert_eq!(image[(80, 80)].a(), 255, "centre should be opaque");
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
    fn the_document_look_saves_with_the_layout_and_ramps_light_to_dark() {
        let mut layout = DockLayout::default();
        // Klay's default look adds nothing to the file.
        assert!(!layout.encode().contains("document|"));
        layout.document = DocumentLook { back: [90, 60, 40], range: 0.8, center: 0.3, rate: 0.5, border: [1, 2, 3] };
        let back = DockLayout::decode(&layout.encode());
        assert_eq!(back.document, layout.document);
        // A broken line keeps the default.
        assert_eq!(DockLayout::decode("document|1,2|x\n").document, DocumentLook::default());
        // Light at the top, dark at the bottom, the Back colour at Center; flat without Range.
        let d = layout.document;
        let (top, mid, bottom) = (d.back_at(0.0), d.back_at(d.center), d.back_at(1.0));
        assert!(top[0] > mid[0] && mid[0] > bottom[0]);
        assert!((mid[0] - 90.0 / 255.0).abs() < 1e-3);
        let flat = DocumentLook { range: 0.0, ..d };
        assert_eq!(flat.back_at(0.0), flat.back_at(1.0));
    }

    #[test]
    fn draw_size_rises_steadily_across_its_slider() {
        let (min, max) = (*DRAW_SIZE.start(), *DRAW_SIZE.end());
        let size = |f: f32| DRAW_SIZE_SCALE.value(f, min, max);
        for f in [0.0, 0.1, 0.37, 0.5, 0.9, 1.0] {
            assert!((DRAW_SIZE_SCALE.position(size(f), min, max) - f).abs() < 1e-5);
        }
        assert_eq!((size(0.0), size(1.0)), (min, max), "the full range is kept");
        // The last tenth of the slider used to cover 40 % of the range (logarithmic); now
        // under a fifth, and the middle of the slider is a mid-sized brush.
        let log = |f: f32| SliderScale::Logarithmic.value(f, min, max);
        assert!((log(1.0) - log(0.9)) / (max - min) > 0.4);
        assert!((size(1.0) - size(0.9)) / (max - min) < 0.2);
        assert!(size(0.5) > 0.4 && size(0.5) < 0.6);
        // Small brushes keep fine steps at the low end.
        assert!(size(0.1) < 0.04);
    }

    #[test]
    fn remesh_section_turns_remesh_on_and_shows_the_estimate() {
        let (mut workspace, ctx) = test_workspace("wedge");
        let size = vec2(1600.0, 3000.0);
        let rows = [row("Sphere 1", true)];
        let mut settings = crate::primitive::Settings::default();
        let (out, _) = frame_full(&mut workspace, &ctx, size, vec![], &rows, &mut settings);
        assert_eq!(text_count(&out, "75 000 vertices"), 1, "the estimate is shown");
        // The section title reads "Remesh" too; the button is the one just above the slider.
        let slider = text_position(&out, "Resolution").y;
        let at = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == "Remesh" => Some(t.pos),
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
