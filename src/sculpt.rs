//! Brushes, strokes and undo. Every per-vertex pass is parallel; the mesh is only touched
//! through `Selection`-based bulk operations.

use crate::mesh::{SculptMesh, Selection};
use crate::multires::{ChunkData, Level, Multires};
use crate::mask::MaskData;
use crate::transform::Affine;
use crate::brush_asset::{Pattern, tangent};
use crate::symmetry::{Images, Symmetry};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BrushKind {
    Draw,
    Smooth,
    Grab,
    Flatten,
    Inflate,
    DamStandard,
    Pinch,
    Clay,
    ClayBuildup,
    Crease,
    TrimDynamic,
    Polish,
    Paint,
    Blob,
    Layer,
    Fill,
    Scrape,
    Nudge,
    Sharpen,
    Elastic,
    SnakeHook,
    MaskPen,
    MaskSoft,
    MaskRect,
    MaskLasso,
    MaskCircle,
    MaskLine,
    ClipLine,
    FormSoft,
    FormHard,
    Zipper,
    Stitch,
    Rake,
    CurveTube,
    ClayTubes,
    HPolish,
    TrimAdaptive,
    Planar,
    TrimSmoothBorder,
    MoveTopological,
    InflateBalloon,
    Magnify,
    OrbCracks,
    MalletFast,
    SmoothValleys,
    SmoothPeaks,
}

impl BrushKind {
    pub const ALL: [BrushKind; 46] = [
        BrushKind::Draw,
        BrushKind::Smooth,
        BrushKind::Grab,
        BrushKind::Flatten,
        BrushKind::Inflate,
        BrushKind::DamStandard,
        BrushKind::Pinch,
        BrushKind::Clay,
        BrushKind::ClayBuildup,
        BrushKind::Crease,
        BrushKind::TrimDynamic,
        BrushKind::Polish,
        BrushKind::Paint,
        BrushKind::Blob,
        BrushKind::Layer,
        BrushKind::Fill,
        BrushKind::Scrape,
        BrushKind::Nudge,
        BrushKind::Sharpen,
        BrushKind::Elastic,
        BrushKind::SnakeHook,
        Self::MaskPen, Self::MaskSoft, Self::MaskRect, Self::MaskLasso, Self::MaskCircle,
        Self::MaskLine, Self::ClipLine, Self::FormSoft, Self::FormHard, Self::Zipper,
        Self::Stitch, Self::Rake, Self::CurveTube,
        Self::ClayTubes, Self::HPolish, Self::TrimAdaptive, Self::Planar, Self::TrimSmoothBorder,
        Self::MoveTopological, Self::InflateBalloon, Self::Magnify, Self::OrbCracks,
        Self::MalletFast, Self::SmoothValleys, Self::SmoothPeaks,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BrushKind::Draw => "Draw",
            BrushKind::Smooth => "Smooth",
            BrushKind::Grab => "Grab",
            BrushKind::Flatten => "Flatten",
            BrushKind::Inflate => "Inflate",
            BrushKind::DamStandard => "Dam Standard",
            BrushKind::Pinch => "Pinch",
            BrushKind::Clay => "Clay",
            BrushKind::ClayBuildup => "Clay Buildup",
            BrushKind::Crease => "Crease",
            BrushKind::TrimDynamic => "Trim Dynamic",
            BrushKind::Polish => "Polish",
            BrushKind::Paint => "Paint",
            BrushKind::Blob => "Blob",
            BrushKind::Layer => "Layer",
            BrushKind::Fill => "Fill",
            BrushKind::Scrape => "Scrape",
            BrushKind::Nudge => "Nudge",
            BrushKind::Sharpen => "Sharpen",
            BrushKind::Elastic => "Elastic",
            BrushKind::SnakeHook => "Snake Hook",
            Self::MaskPen => "Mask Pen", Self::MaskSoft => "Mask Soft",
            Self::MaskRect => "Mask Rect", Self::MaskLasso => "Mask Lasso",
            Self::MaskCircle => "Mask Circle", Self::MaskLine => "Mask Line",
            Self::ClipLine => "Clip Line", Self::FormSoft => "FormSoft", Self::FormHard => "FormHard",
            Self::Zipper => "Zipper", Self::Stitch => "Stitch", Self::Rake => "Rake",
            Self::CurveTube => "CurveTube",
            Self::ClayTubes => "ClayTubes", Self::HPolish => "hPolish",
            Self::TrimAdaptive => "TrimAdaptive", Self::Planar => "Planar",
            Self::TrimSmoothBorder => "TrimSmoothBorder", Self::MoveTopological => "Move Topological",
            Self::InflateBalloon => "Inflate Balloon", Self::Magnify => "Magnify",
            Self::OrbCracks => "Orb Cracks", Self::MalletFast => "MalletFast",
            Self::SmoothValleys => "SmoothValleys", Self::SmoothPeaks => "SmoothPeaks",
        }
    }

    pub fn base_type(self) -> &'static str {
        match self {
            Self::Draw|Self::DamStandard|Self::Crease|Self::Inflate|Self::Blob|Self::Layer=>"Standard",
            Self::Clay|Self::ClayBuildup=>"Clay", Self::Pinch=>"Pinch", Self::Grab=>"Move",
            Self::Flatten|Self::TrimDynamic|Self::Polish|Self::Fill|Self::Scrape=>"Planar", Self::Smooth=>"Smooth",Self::Paint=>"Paint",
            Self::Nudge=>"Directional",Self::Sharpen=>"Detail",Self::Elastic|Self::SnakeHook=>"Deform",
            Self::MaskPen|Self::MaskSoft|Self::MaskRect|Self::MaskLasso|Self::MaskCircle|Self::MaskLine=>"Mask",
            Self::ClipLine=>"Clip",Self::FormSoft|Self::FormHard=>"Clay",
            Self::Zipper|Self::Stitch|Self::Rake=>"Detail",
            Self::CurveTube=>"Curve",
            Self::ClayTubes=>"Clay",
            Self::HPolish|Self::TrimAdaptive|Self::Planar|Self::TrimSmoothBorder|Self::MalletFast=>"Planar",
            Self::MoveTopological=>"Move",
            Self::InflateBalloon|Self::OrbCracks=>"Standard",
            Self::Magnify=>"Pinch",
            Self::SmoothValleys|Self::SmoothPeaks=>"Smooth",
        }
    }

    pub fn is_drag(self) -> bool { matches!(self,Self::Grab|Self::Elastic|Self::SnakeHook|Self::MoveTopological) }
    pub fn gpu_drag(self) -> bool { matches!(self,Self::Grab|Self::SnakeHook|Self::MoveTopological) }
    /// A square footprint turned along the stroke (ZBrush: the square alphas of ClayTubes,
    /// hPolish and MalletFast) instead of a round falloff.
    pub fn is_square(self) -> bool { matches!(self,Self::ClayTubes|Self::HPolish|Self::MalletFast) }
    pub fn is_mask(self) -> bool { self.base_type()=="Mask" }
    pub fn is_shape(self) -> bool { matches!(self,Self::MaskRect|Self::MaskLasso|Self::MaskCircle|Self::MaskLine|Self::ClipLine) }
    pub fn is_detail(self) -> bool { matches!(self,Self::Zipper|Self::Stitch|Self::Rake) }
    /// Adds geometry along a curve instead of moving the surface (ZBrush: curve insert brushes).
    pub fn is_insert(self) -> bool { matches!(self,Self::CurveTube) }

    pub fn description(self) -> &'static str {
        match self {
            Self::Draw=>"Build a rounded stroke. Alt carves.",Self::Smooth=>"Relax surface detail.",
            Self::Grab=>"Pull the surface with the cursor.",Self::Flatten=>"Level the surface to a fitted plane.",
            Self::Inflate=>"Expand along surface normals. Alt deflates.",Self::DamStandard=>"Cut a narrow groove. Alt raises a seam.",
            Self::Pinch=>"Contract the surface. Alt spreads.",Self::Clay=>"Deposit a broad clay stroke.",
            Self::ClayBuildup=>"Build up clay with repeated strokes.",Self::Crease=>"Pinch and cut a crease.",
            Self::TrimDynamic=>"Trim raised areas to a local plane.",Self::Polish=>"Polish the local plane.",
            Self::Paint=>"Paint vertex color without moving the surface.",Self::Blob=>"Build a soft dome. Alt indents.",
            Self::Layer=>"Build a capped layer per stroke. Release to add another layer.",
            Self::Fill=>"Fill low areas toward a fitted plane. Alt lowers peaks.",
            Self::Scrape=>"Shave raised clay into a flat facet. Alt fills.",
            Self::Nudge=>"Push clay along the stroke direction. Alt reverses.",
            Self::Sharpen=>"Enhance local surface detail. Alt relaxes.",
            Self::Elastic=>"Pull a broad area with directional surface deformation.",
            Self::SnakeHook=>"Pull a tapered tip. Divide for finer topology before long pulls.",
            Self::MaskPen=>"Paint a mask. Alt erases. Ctrl temporarily uses the selected mask brush.",
            Self::MaskSoft=>"Paint a broad soft mask. Alt erases.",
            Self::MaskRect=>"Drag a rectangular mask through the mesh. Alt erases.",
            Self::MaskLasso=>"Draw a closed mask outline. Alt erases.",
            Self::MaskCircle=>"Drag an elliptical mask through the mesh. Alt erases.",
            Self::MaskLine=>"Mask one side of a line. Reverse the drag to change sides. Alt erases.",
            Self::ClipLine=>"Drag a line to flatten one side onto it. Alt flips sides; Shift snaps the angle.",
            Self::FormSoft=>"Build a broad soft form while relaxing fine surface noise. Alt carves.",
            Self::FormHard=>"Build a firm, flat-topped clay form. Alt carves.",
            Self::Zipper=>"Draw paired zipper teeth and a center seam as relief. Divide for fine detail.",
            Self::Stitch=>"Draw spaced stitches along a seam. Divide for fine detail.",
            Self::Rake=>"Draw parallel grooves in the stroke direction. Alt raises ridges.",
            Self::CurveTube=>"Draw a curve to add a tube along it as a new polygroup. Drag the curve to bend the tube; Draw Size sets its radius. Needs a subtool without subdivision levels.",
            Self::ClayTubes=>"Lay square, flat-topped clay strips along the stroke. Alt carves.",
            Self::HPolish=>"Flatten to the local plane with a square, hard edge.",
            Self::TrimAdaptive=>"Trim raised areas to the plane right under the brush centre.",
            Self::Planar=>"Flatten onto the plane where the stroke started.",
            Self::TrimSmoothBorder=>"Trim raised areas to a local plane and soften the edge of the cut.",
            Self::MoveTopological=>"Pull only the surface connected to the click point; nearby separate surfaces stay still.",
            Self::InflateBalloon=>"Swell the surface round, like a balloon. Alt deflates.",
            Self::Magnify=>"Spread the surface outward and swell it. Alt contracts.",
            Self::OrbCracks=>"Cut a sharp, uneven crack. Alt raises a sharp ridge.",
            Self::MalletFast=>"Hammer a flat-bottomed, square dent. Alt raises a flat plateau.",
            Self::SmoothValleys=>"Smooth only recessed areas; ridges stay sharp.",
            Self::SmoothPeaks=>"Smooth only raised areas; grooves stay sharp.",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Brush {
    pub kind: BrushKind,
    /// World-space radius.
    pub radius: f32,
    /// 0..1
    pub strength: f32,
    /// -100..100: negative tightens the centre, positive widens the plateau.
    pub focal_shift: f32,
    pub rgb_intensity: f32,
    pub color: [u8;3],
    pub paint: bool,
    pub sculpt: bool,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            kind: BrushKind::Draw,
            radius: 0.25,
            strength: 0.5,
            focal_shift: 0.0,
            rgb_intensity: 1.0,
            color: [227,131,79],
            paint: false,
            sculpt: true,
        }
    }
}

/// Smallest share of the radius pen pressure leaves, so the lightest touch still makes a
/// small dab (and the falloff never divides by a zero radius).
pub const MIN_PRESSURE_SIZE: f32 = 0.05;

impl Brush {
    /// This brush for one dab at pen pressure `p` (0..=1; `None` for a mouse): ZBrush's
    /// Tablet Pressure with its default linear curves, scaling Draw Size (`size`) and Z and
    /// RGB Intensity (`intensity`). Drag brushes (Grab, Elastic, Snake Hook) keep what they
    /// picked up with, and detail reliefs (Zipper, Stitch, Rake) keep their size so their
    /// teeth stay even.
    pub fn at_pressure(mut self, p: Option<f32>, size: bool, intensity: bool) -> Self {
        let Some(p) = p else {
            return self;
        };
        if self.kind.is_drag() {
            return self;
        }
        let p = p.clamp(0.0, 1.0);
        if size && !self.kind.is_detail() {
            self.radius *= p.max(MIN_PRESSURE_SIZE);
        }
        if intensity {
            self.strength *= p;
            self.rgb_intensity *= p;
        }
        self
    }
}

/// One undo step.
pub enum UndoStep {
    /// A stroke at `level`: the chunk data it replaced (positions at level 0, offsets
    /// above it, see [`Multires::finish_stroke`]).
    Stroke { level: usize, chunks: ChunkData },
    /// A subdivision level was added. In the undo list the level is live (`None`); the
    /// redo entry keeps the removed level so redo can put it back without recomputing it.
    Divide(Option<Box<Level>>),
    /// Every level was mapped by an [`Affine`] (Deformation: Offset, Size; Transpose Move,
    /// Scale, Rotate). Like every entry it holds what undoes it: the inverse map.
    Transform(Affine),
    /// Every level was mapped by `map` weighted per vertex by how free each vertex is in
    /// `mask`, the mask of `level` when the transform was made (Transpose with a mask).
    MaskedTransform {
        map: Affine,
        level: usize,
        mask: Vec<u8>,
    },
    /// Hidden triangles of every level before a visibility change (run-length coded).
    Visibility(Vec<Vec<u32>>),
    /// Polygroups of every level before a group change (run-length coded).
    Groups(Vec<crate::polygroup::GroupRuns>),
    /// The whole stack before it was replaced (Wedge), packed.
    Replace(Box<crate::multires::Packed>),
    /// What followed `keep` in the base mesh before a part was added or changed there
    /// (CurveTube): `None` for nothing. Only the part is kept, not the whole model.
    Part { keep: crate::mesh::Extent, part: Option<Box<SculptMesh>> },
    /// The mask of `level` changed (Ctrl).
    Mask { level: usize, data: MaskData },
    Paint(crate::paint::Snapshot),
    /// Several steps made by one action (Ctrl + W on a mask: the new group and the cleared
    /// mask), applied in the order listed (the undo of the last change first).
    Steps(Vec<UndoStep>),
}

pub struct History {
    undo: Vec<UndoStep>,
    redo: Vec<UndoStep>,
    pub limit: usize,
    /// Counts every push, undo, redo and clear: a curve stays bendable only while no other
    /// edit has happened since it was applied.
    edits: u64,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit,
            edits: 0,
        }
    }

    /// See [`History::edits`](Self).
    pub fn edits(&self) -> u64 {
        self.edits
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The map the next undo applies, when that step is a transform.
    pub fn next_undo_map(&self) -> Option<Affine> {
        match self.undo.last() {
            Some(UndoStep::Transform(map)) => Some(*map),
            _ => None,
        }
    }

    /// The map the next redo applies, when that step is a transform.
    pub fn next_redo_map(&self) -> Option<Affine> {
        match self.redo.last() {
            Some(UndoStep::Transform(map)) => Some(*map),
            _ => None,
        }
    }

    pub fn push(&mut self, step: UndoStep) {
        self.edits += 1;
        self.redo.clear();
        self.undo.push(step);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self, doc: &mut Multires) {
        self.edits += 1;
        if let Some(step) = self.undo.pop() {
            let inverse = apply_step(doc, step);
            self.redo.push(inverse);
        }
    }

    pub fn redo(&mut self, doc: &mut Multires) {
        self.edits += 1;
        if let Some(step) = self.redo.pop() {
            let inverse = apply_step(doc, step);
            self.undo.push(inverse);
        }
    }

    pub fn clear(&mut self) {
        self.edits += 1;
        self.undo.clear();
        self.redo.clear();
    }

    /// Drop the redo list, whose divide entries can hold whole levels. Used when a subtool
    /// is parked so its history stays light; the undo list stays valid because a parked
    /// stack comes back with the same chunks.
    pub fn drop_redo(&mut self) {
        self.redo.clear();
    }

    /// Approximate memory held by history, in bytes.
    pub fn bytes(&self) -> usize {
        self.undo.iter().chain(self.redo.iter()).map(step_bytes).sum()
    }
}

fn step_bytes(s: &UndoStep) -> usize {
    match s {
        UndoStep::Stroke { chunks, .. } => chunks.iter().map(|(_, v)| v.len() * 12).sum::<usize>(),
        UndoStep::Divide(level) => level.as_ref().map_or(0, |l| l.bytes()),
        UndoStep::Transform(_) => 0,
        UndoStep::MaskedTransform { mask, .. } => mask.len(),
        UndoStep::Mask { data, .. } => data.bytes(),
        UndoStep::Paint(data) => data.bytes(),
        UndoStep::Visibility(runs) => runs.iter().map(|r| r.len() * 4).sum(),
        UndoStep::Groups(runs) => runs.iter().map(|r| r.bytes()).sum(),
        UndoStep::Replace(packed) => packed.bytes(),
        UndoStep::Part { part, .. } => part.as_ref().map_or(0, |m| m.vertex_count() * 40 + m.triangle_count() * 14),
        UndoStep::Steps(steps) => steps.iter().map(step_bytes).sum(),
    }
}

/// Apply a step and return its inverse.
fn apply_step(doc: &mut Multires, step: UndoStep) -> UndoStep {
    match step {
        UndoStep::Stroke { level, chunks } => UndoStep::Stroke {
            level,
            chunks: doc.restore(level, chunks),
        },
        UndoStep::Divide(None) => UndoStep::Divide(doc.pop_level().map(Box::new)),
        UndoStep::Divide(Some(level)) => {
            doc.push_level(*level);
            UndoStep::Divide(None)
        }
        UndoStep::Transform(map) => {
            doc.transform(&map);
            UndoStep::Transform(map.inverse())
        }
        UndoStep::MaskedTransform { map, level, mask } => {
            doc.transform_masked(&map, level, &mask);
            UndoStep::MaskedTransform {
                map: map.inverse(),
                level,
                mask,
            }
        }
        UndoStep::Mask { level, data } => UndoStep::Mask {
            level,
            data: doc.restore_mask(level, data),
        },
        UndoStep::Paint(data) => UndoStep::Paint(doc.restore_paint(data)),
        UndoStep::Visibility(runs) => UndoStep::Visibility(doc.restore_visibility(runs)),
        UndoStep::Groups(runs) => UndoStep::Groups(doc.restore_groups(runs)),
        UndoStep::Replace(packed) => {
            let budget = doc.budget;
            let now = std::mem::replace(doc, packed.unpack());
            doc.budget = budget;
            UndoStep::Replace(Box::new(now.pack()))
        }
        UndoStep::Part { keep, part } => UndoStep::Part {
            keep,
            part: doc.swap_part(keep, part.as_deref()).map(Box::new),
        },
        UndoStep::Steps(steps) => {
            // Applied in order; their inverses run the other way.
            let mut inverse: Vec<UndoStep> = steps.into_iter().map(|s| apply_step(doc, s)).collect();
            inverse.reverse();
            UndoStep::Steps(inverse)
        }
    }
}

struct GrabState {
    sel: Selection,
    chunks: Vec<u32>,
    weights: Vec<f32>,
    orig: Vec<Vec3>,
    anchor: Vec3,
    /// Positions are moved on the GPU during the stroke; the CPU applies the final delta at the end.
    gpu: bool,
    delta: Vec3,
    kind: BrushKind,
    radius: f32,
    /// Vertices several symmetry copies reach: index into `overlaps` per selected vertex
    /// (`u32::MAX`: one copy only, its image and weight), each list holding every copy's
    /// image and weight. Empty when no vertex is shared. Such a grab runs on the CPU.
    overlap_of: Vec<u32>,
    overlaps: Vec<Vec<(u8, f32)>>,
}

/// Data the renderer needs to run a grab on the GPU.
pub struct GpuGrabSetup<'a> {
    pub verts: &'a [u32],
    pub orig: &'a [Vec3],
    pub weights: &'a [f32],
    /// Symmetry image of each vertex (empty: all the brush's own); the delta of each image
    /// comes with [`Stroke::take_gpu_delta`].
    pub images: &'a [u8],
    pub chunks: &'a [u32],
}

/// State of the stroke currently being painted.
pub struct Stroke {
    pub invert: bool,
    snapshots: Vec<(u32, Vec<Vec3>)>,
    snapshotted: HashSet<u32>,
    snapshot_index: HashMap<u32, usize>,
    layer_normal: Option<Vec3>,
    /// Planar: the plane (centre, normal) set by the stroke's first dab.
    plane: Option<(Vec3, Vec3)>,
    /// Stroke direction in the surface, for square footprints.
    direction: Option<Vec3>,
    detail_distance: f32,
    last_dab: Option<Vec3>,
    grab: Option<GrabState>,
    pub dabs: u32,
    /// Vertices touched by the last dab.
    pub last_count: usize,
    /// Reused per dab for the two-pass brushes.
    scratch: Vec<Vec3>,
    weights: Vec<f32>,
    /// Grab runs on the GPU when the renderer is available.
    gpu_grab: bool,
    /// The grab's move for every symmetry image, waiting for the GPU.
    pending_delta: Option<Vec<Vec3>>,
    /// Every dab is applied at each of these images of it (symmetry).
    images: Images,
    assets: StampSettings,
    paint_snapshots: crate::paint::Chunks,
    paint_snapshotted: HashSet<u32>,
    /// Camera forward of the latest dab (Auto Masking's BackfaceMask).
    view: Vec3,
    /// Least distance between dabs, as a share of the radius (Stroke › Spacing).
    spacing: f32,
}

/// Default Stroke › Spacing: a dab every 0.15 radius.
pub const SPACING: f32 = 0.15;

#[derive(Clone, Debug)]
pub struct StampSettings {
    pub alpha: Pattern,
    pub texture: Pattern,
    pub texture_strength: f32,
    pub texture_scale: f32,
    /// Brush › Auto Masking.
    pub auto: AutoMask,
}

impl Default for StampSettings {
    fn default()->Self {Self {alpha:Pattern::Off,texture:Pattern::Off,texture_strength:0.5,texture_scale:2.0,auto:AutoMask::default()}}
}

/// Brush › Auto Masking (ZBrush): surface the stroke leaves alone without a mask.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoMask {
    /// BackfaceMask: surface turned away from the viewer (the far side of a thin part).
    pub backface: bool,
    /// Topological: surface not joined to the point under the brush inside the brush
    /// (a finger next to the one being sculpted, the other lip).
    pub topological: bool,
}

/// Every symmetry copy that moves grabbed vertex `i`, with its weight: one, or all of
/// them where copies overlap (their moves add up, as in ZBrush).
fn grab_copies(g: &GrabState, i: usize) -> impl Iterator<Item = (usize, f32)> + '_ {
    let shared = g.overlap_of.get(i).copied().filter(|&o| o != u32::MAX);
    let one = shared.is_none().then(|| (g.sel.image(i), g.weights[i]));
    one.into_iter().chain(shared.into_iter().flat_map(move |o| g.overlaps[o as usize].iter().map(|&(k, w)| (k as usize, w))))
}

/// Offset of grabbed vertex `i`: each copy's image of the move times its weight (as
/// grab.wgsl does for a vertex one copy reaches).
fn grab_offset(images: &Images, g: &GrabState, i: usize) -> Vec3 {
    grab_copies(g, i).map(|(k, w)| images.map(k, g.delta) * w).sum()
}

fn drag_offset(images: &Images, g: &GrabState, i: usize) -> Vec3 {
    if g.kind != BrushKind::Elastic { return grab_offset(images, g, i); }
    grab_copies(g, i)
        .map(|(k, w)| {
            let anchor = images.map(k, g.anchor);
            let relative = (g.orig[i] - anchor) / g.radius;
            let direction = images.map(k, g.delta);
            // Directional coupling gives broad surrounding clay a softer, lateral response.
            direction * w + relative * (relative.dot(direction) * 0.35 * w)
        })
        .sum()
}

impl Stroke {
    pub fn begin(invert: bool, gpu_grab: bool, symmetry: impl Into<Symmetry>) -> Self {
        Self {
            invert,
            snapshots: Vec::new(),
            snapshotted: HashSet::new(),
            snapshot_index: HashMap::new(),
            layer_normal: None,
            plane: None,
            direction: None,
            detail_distance: 0.0,
            last_dab: None,
            grab: None,
            dabs: 0,
            last_count: 0,
            scratch: Vec::new(),
            weights: Vec::new(),
            gpu_grab,
            pending_delta: None,
            images: symmetry.into().images(),
            assets: StampSettings::default(),
            paint_snapshots: Vec::new(),
            paint_snapshotted: HashSet::new(),
            view: -Vec3::Z,
            spacing: SPACING,
        }
    }

    pub fn with_spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing.max(0.01);
        self
    }

    /// The camera forward a curve stroke is seen along (dabs set it themselves).
    pub fn set_view(&mut self, view: Vec3) {
        self.view = view;
    }

    /// Auto Masking's factor for every selected vertex, or `None` when it is off. The
    /// mirrored half is judged against the mirrored view.
    fn auto_weights(&self, mesh: &SculptMesh, sel: &Selection) -> Option<Vec<f32>> {
        let a = self.assets.auto;
        if !a.backface && !a.topological {
            return None;
        }
        let joined = a.topological.then(|| connected(mesh, sel));
        let view = self.view;
        Some(
            sel.verts
                .par_iter()
                .enumerate()
                .map(|(i, &v)| {
                    if joined.as_ref().is_some_and(|j| !j[i]) {
                        return 0.0;
                    }
                    if !a.backface {
                        return 1.0;
                    }
                    let view = self.images.map(sel.image(i), view);
                    let facing = -mesh.normal_at(v).dot(view);
                    let t = ((facing + 0.05) / 0.25).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                })
                .collect(),
        )
    }

    pub fn with_assets(mut self,assets:StampSettings)->Self {self.assets=assets;self}
    pub fn take_paint(&mut self)->crate::paint::Chunks {std::mem::take(&mut self.paint_snapshots)}

    /// Alpha and texture at `p`, a vertex of symmetry copy `k` of `images`, for a dab at
    /// `hit` with `normal` in the brush's own frame (so every copy shows the same stamp).
    fn stamp_weight(&self,images:&Images,k:usize,p:Vec3,hit:Vec3,normal:Vec3,r:f32)->f32 {
        if self.assets.alpha==Pattern::Off && self.assets.texture==Pattern::Off {return 1.0;}
        let p=images.unmap(k,p);
        let (u,v)=tangent(normal);
        let delta=p-hit;
        let uv=glam::Vec2::new(delta.dot(u),-delta.dot(v))/(2.0*r)+glam::Vec2::splat(0.5);
        let a=if self.assets.alpha==Pattern::Off {1.0}else{self.assets.alpha.alpha(uv)};
        let t=self.assets.texture.sample(uv*self.assets.texture_scale,true);
        let h=t.truncate().dot(Vec3::new(0.2126,0.7152,0.0722))*t.w;
        a*(1.0-self.assets.texture_strength+h*self.assets.texture_strength)
    }

    /// Polypaint under a dab at `hit` with `normal` in the brush's own frame. Vertex `i`
    /// belongs to symmetry copy `copy` of `images`, or with `None` to its selection image.
    #[allow(clippy::too_many_arguments)]
    fn paint_selected(&mut self,mesh:&mut SculptMesh,sel:&Selection,brush:&Brush,hit:Vec3,normal:Vec3,auto:Option<&[f32]>,images:&Images,copy:Option<usize>) {
        if !brush.paint||brush.rgb_intensity<=0.0{return;}
        let fresh:Vec<_>=sel.chunks().into_iter().filter(|c|self.paint_snapshotted.insert(*c)).collect();
        self.paint_snapshots.extend(crate::paint::snapshot(mesh,&fresh));
        let (u,v)=tangent(normal);let base=Vec3::from_array(brush.color.map(|x|x as f32/255.0));
        let values=sel.par_map(|i,vert| {
            let p=images.unmap(copy.unwrap_or_else(||sel.image(i)),mesh.positions[vert as usize]);
            let delta=p-hit;
            let uv=glam::Vec2::new(delta.dot(u),-delta.dot(v))/(2.0*brush.radius)+glam::Vec2::splat(0.5);
            let tex=self.assets.texture.sample(uv*self.assets.texture_scale,true);
            let rgb=base*tex.truncate();
            let weight=focal_falloff(sel.dist[i],brush.radius,brush.focal_shift)
                *if self.assets.alpha==Pattern::Off{1.0}else{self.assets.alpha.alpha(uv)}
                *crate::mask::freedom(&mesh.mask,vert)*tex.w*brush.rgb_intensity*0.35*auto.map_or(1.0,|a|a[i]);
            crate::paint::blend(crate::paint::at(mesh,vert as usize),rgb.to_array(),weight)
        });
        crate::paint::scatter(mesh,sel,&values);
    }

    fn select(&self, mesh: &SculptMesh, hit: Vec3, radius: f32) -> Selection {
        if self.images.len() > 1 {
            mesh.select_sphere_images(hit, radius, &self.images)
        } else {
            mesh.select_sphere(hit, radius)
        }
    }

    pub fn grabbing(&self) -> bool {
        self.grab.is_some()
    }

    /// Buffers for a GPU grab, available once the grab selection exists.
    pub fn gpu_grab_setup(&self) -> Option<GpuGrabSetup<'_>> {
        let g = self.grab.as_ref().filter(|g| g.gpu)?;
        Some(GpuGrabSetup {
            verts: &g.sel.verts,
            orig: &g.orig,
            weights: &g.weights,
            images: &g.sel.image,
            chunks: &g.chunks,
        })
    }

    /// The move the GPU should apply this frame for every symmetry image (index 0 the
    /// brush's own), if the grab moved.
    pub fn take_gpu_delta(&mut self) -> Option<Vec<Vec3>> {
        self.pending_delta.take()
    }

    /// Snapshot the owned positions of each chunk once per stroke, before it is modified.
    fn snapshot(&mut self, mesh: &SculptMesh, chunks: &[u32]) {
        let fresh: Vec<u32> = chunks.iter().copied().filter(|c| self.snapshotted.insert(*c)).collect();
        if !fresh.is_empty() {
            for data in mesh.snapshot_chunks(&fresh) {
                self.snapshot_index.insert(data.0, self.snapshots.len());
                self.snapshots.push(data);
            }
        }
    }

    /// Apply the brush at a surface hit. `view_dir` is the camera forward vector and
    /// `ray` the current cursor ray, both used by Grab.
    pub fn dab(
        &mut self,
        mesh: &mut SculptMesh,
        brush: &Brush,
        hit: Vec3,
        hit_normal: Vec3,
        view_dir: Vec3,
        ray: (Vec3, Vec3),
    ) {
        if brush.kind.is_mask() || brush.kind==BrushKind::ClipLine { return; }
        self.view = view_dir;
        if brush.kind.is_detail() && brush.sculpt {
            let Some(previous)=self.last_dab else {self.last_dab=Some(hit);return;};
            if previous.distance(hit)<brush.radius*0.04 {return;}
            self.last_dab=Some(hit);
            self.detail_segment(mesh,brush,previous,hit,hit_normal);
            return;
        }
        if brush.kind.is_drag() {
            self.grab_dab(mesh, brush, hit, view_dir, ray);
            return;
        }
        // Dab spacing so stroke strength does not depend on event rate.
        if let Some(last) = self.last_dab {
            if (hit - last).length() < brush.radius * self.spacing {
                return;
            }
        }
        let previous_hit = self.last_dab;
        self.last_dab = Some(hit);
        self.dabs += 1;
        // Square footprints turn with the stroke: along the last dab-to-dab move, kept
        // from the previous dab when there is none (the first dab of a stroke). Kept in
        // the brush's own frame; every symmetry copy turns it with itself.
        let fallback = self.direction.unwrap_or(tangent(hit_normal).0);
        let moved = previous_hit.map_or(Vec3::ZERO, |p| hit - p);
        let along = (moved - hit_normal * moved.dot(hit_normal)).normalize_or(fallback - hit_normal * fallback.dot(hit_normal)).normalize_or(tangent(hit_normal).0);
        self.direction = Some(along);
        // With symmetry every copy of the brush makes a whole dab of its own, one after
        // another, as in ZBrush: where copies overlap (a big brush, a stroke on a symmetry
        // plane) both apply, and the surface stays whole.
        let images = std::mem::replace(&mut self.images, Images::identity());
        for k in 0..images.len() {
            self.view = images.map(k, view_dir);
            self.dab_copy(mesh, brush, &images, k, hit, hit_normal, previous_hit, along);
        }
        self.images = images;
    }

    /// One symmetry copy (`k` of `images`) of a dab whose own hit, normal, previous hit and
    /// stroke direction are given in the brush's frame. While it runs `self.images` is the
    /// identity, so the selection is the copy's alone.
    #[allow(clippy::too_many_arguments)]
    fn dab_copy(
        &mut self,
        mesh: &mut SculptMesh,
        brush: &Brush,
        images: &Images,
        k: usize,
        own_hit: Vec3,
        own_normal: Vec3,
        own_previous: Option<Vec3>,
        along: Vec3,
    ) {
        let hit = images.map(k, own_hit);
        let hit_normal = images.map(k, own_normal);
        let previous_hit = own_previous.map(|p| images.map(k, p));
        let sel = self.select(mesh, hit, brush.radius);
        if sel.is_empty() {
            return;
        }
        self.last_count = sel.len();
        let chunks = sel.chunks();
        if brush.sculpt && brush.kind!=BrushKind::Paint {self.snapshot(mesh, &chunks);}
        let auto = self.auto_weights(mesh, &sel);
        self.paint_selected(mesh,&sel,brush,own_hit,own_normal,auto.as_deref(),images,Some(k));
        if !brush.sculpt || brush.kind==BrushKind::Paint {return;}
        let sign = if self.invert { -1.0 } else { 1.0 };
        let r = brush.radius;
        // Masked vertices (Ctrl) are held still.
        let mask = &mesh.mask;
        let square = brush.kind.is_square();
        let mut weights=std::mem::take(&mut self.weights);
        sel.dist
            .par_iter()
            .zip(sel.verts.par_iter())
            .enumerate()
            .map(|(_,(&d, &v))| {
                let p = mesh.positions[v as usize];
                let profile = if square {
                    square_footprint(images.unmap(k, p), own_hit, own_normal, along, r, brush.kind == BrushKind::MalletFast)
                } else {
                    focal_falloff(d, r, brush.focal_shift)
                };
                profile*self.stamp_weight(images,k,p,own_hit,own_normal,r)*crate::mask::freedom(mask, v)
            })
            .collect_into_vec(&mut weights);
        if let Some(a) = &auto {
            weights.par_iter_mut().zip(a.par_iter()).for_each(|(w, a)| *w *= a);
        }
        self.weights=weights;
        let w = &self.weights;
        let new = &mut self.scratch;

        match brush.kind {
            BrushKind::Draw | BrushKind::Blob | BrushKind::FormSoft => {
                // Push along the average normal under the brush: smoother than per-vertex
                // normals. Averaged over the brush's own side; the mirrored side uses the
                // mirrored normal.
                let n = sel
                    .verts
                    .par_iter()
                    .enumerate()
                    .zip(w.par_iter())
                    .map(|((i, &v), &wi)| if sel.side(i) > 0.0 { mesh.normal_at(v) * wi } else { Vec3::ZERO })
                    .reduce(|| Vec3::ZERO, |a, b| a + b)
                    .normalize_or(hit_normal);
                let images = &self.images;
                let amount = brush.strength * r * match brush.kind {BrushKind::Blob=>0.13,BrushKind::FormSoft=>0.04,_=>0.06} * sign;
                sel.par_map_into(new, |i, v| {
                    let n = images.map(sel.image(i), n);
                    let dome = if brush.kind==BrushKind::Blob {(1.0-(sel.dist[i]/r).powi(2)).max(0.0).sqrt()}else{1.0};
                    let p=mesh.positions[v as usize];
                    let relax=if brush.kind==BrushKind::FormSoft {
                        let nb=mesh.neighbors(v);
                        if nb.is_empty(){Vec3::ZERO}else{
                            (nb.iter().map(|&v|mesh.positions[v as usize]).sum::<Vec3>()/nb.len() as f32-p)*(0.08*brush.strength*w[i])
                        }
                    }else{Vec3::ZERO};
                    p + relax + n * (amount * w[i] * dome)
                });
            }
            BrushKind::Layer => {
                // The layer's normal is fixed by the first dab, in the brush's own frame.
                let n = images.map(k, *self.layer_normal.get_or_insert(own_normal));
                let snapshots = &self.snapshots;
                let index = &self.snapshot_index;
                sel.par_map_into(new, |i,v| {
                    let c = mesh.vert_chunk[v as usize];
                    let old = snapshots[index[&c]].1[(v-mesh.chunks[c as usize].vert_start) as usize];
                    let p = mesh.positions[v as usize];
                    let remaining = (r*0.14 - (p-old).dot(n)*sign).max(0.0);
                    p+n*(remaining.min(r*0.06)*brush.strength*w[i]*sign)
                });
            }
            BrushKind::Nudge => {
                let delta = (hit-previous_hit.unwrap_or(hit)).clamp_length_max(r*0.2);
                let direction = (delta-hit_normal*delta.dot(hit_normal))*brush.strength*0.65*sign;
                let images = &self.images;
                sel.par_map_into(new,|i,v| {
                    mesh.positions[v as usize]+images.map(sel.image(i),direction)*w[i]
                });
            }
            BrushKind::Sharpen => {
                sel.par_map_into(new,|i,v| {
                    let p=mesh.positions[v as usize];let nb=mesh.neighbors(v);
                    if nb.is_empty(){return p;}
                    let mean=nb.iter().map(|&v|mesh.positions[v as usize]).sum::<Vec3>()/nb.len() as f32;
                    p+(p-mean).clamp_length_max(r*0.08)*(brush.strength*0.65*w[i]*sign)
                });
            }
            BrushKind::Inflate => {
                let amount = brush.strength * r * 0.06 * sign;
                sel.par_map_into(new, |i, v| mesh.positions[v as usize] + mesh.normal_at(v) * (amount * w[i]));
            }
            BrushKind::InflateBalloon => {
                // A broad, even swell along each normal, relaxed so the bulge stays round
                // instead of following the creases under it.
                let amount = brush.strength * r * 0.07 * sign;
                sel.par_map_into(new, |i, v| {
                    let p = mesh.positions[v as usize];
                    let nb = mesh.neighbors(v);
                    let relax = if nb.is_empty() { Vec3::ZERO } else {
                        (nb.iter().map(|&n| mesh.positions[n as usize]).sum::<Vec3>() / nb.len() as f32 - p) * (0.25 * brush.strength * w[i])
                    };
                    p + relax + mesh.normal_at(v) * (amount * w[i].sqrt())
                });
            }
            BrushKind::Smooth | BrushKind::SmoothValleys | BrushKind::SmoothPeaks => {
                let k = brush.strength * 0.5;
                let kind = brush.kind;
                sel.par_map_into(new, |i, v| {
                    let nb = mesh.neighbors(v);
                    let p = mesh.positions[v as usize];
                    if nb.is_empty() {
                        return p;
                    }
                    let mut avg = Vec3::ZERO;
                    for &n in nb {
                        avg += mesh.positions[n as usize];
                    }
                    let avg = avg / nb.len() as f32;
                    // A vertex below its neighbours' mean sits in a valley, above it on a
                    // peak; the gate fades in with how clearly it is one or the other.
                    let gate = match kind {
                        BrushKind::Smooth => 1.0,
                        _ => {
                            let delta = avg - p;
                            let lean = delta.dot(mesh.normal_at(v)) / delta.length().max(1e-12);
                            let lean = if kind == BrushKind::SmoothValleys { lean } else { -lean };
                            (lean * 4.0).clamp(0.0, 1.0)
                        }
                    };
                    p.lerp(avg, (k * w[i] * gate).min(1.0))
                });
            }
            BrushKind::Flatten | BrushKind::TrimDynamic | BrushKind::Polish | BrushKind::Clay | BrushKind::ClayBuildup | BrushKind::Fill | BrushKind::Scrape | BrushKind::FormHard
            | BrushKind::ClayTubes | BrushKind::HPolish | BrushKind::TrimAdaptive | BrushKind::Planar | BrushKind::TrimSmoothBorder | BrushKind::MalletFast => {
                // Fit a plane through the weighted centre with the average normal (own side
                // only), pull towards it; the mirrored side uses the mirrored plane.
                // TrimAdaptive fits only the surface right under the brush centre.
                let adaptive = brush.kind == BrushKind::TrimAdaptive;
                let (c, n, wsum) = sel
                    .verts
                    .par_iter()
                    .enumerate()
                    .zip(w.par_iter())
                    .filter(|((i, _), _)| sel.side(*i) > 0.0)
                    .map(|((i, &v), &wi)| {
                        let wi = if adaptive { wi * (-(sel.dist[i] / (r * 0.3)).powi(2)).exp() } else { wi };
                        (mesh.positions[v as usize] * wi, mesh.normal_at(v) * wi, wi)
                    })
                    .reduce(
                        || (Vec3::ZERO, Vec3::ZERO, 0.0),
                        |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2),
                    );
                let c = if wsum > 1e-6 { c / wsum } else { hit };
                let n = n.normalize_or(hit_normal);
                // Planar keeps the plane of its first dab for the whole stroke.
                let (c, n) = if brush.kind == BrushKind::Planar {
                    // Kept in the brush's own frame; every copy uses its image.
                    let own = *self.plane.get_or_insert((images.unmap(k, c), images.unmap(k, n)));
                    (images.map(k, own.0), images.map(k, own.1))
                } else {
                    (c, n)
                };
                let images = &self.images;
                let k = brush.strength * match brush.kind {
                    BrushKind::Polish | BrushKind::Scrape => 0.7,
                    BrushKind::HPolish | BrushKind::MalletFast | BrushKind::Planar => 0.85,
                    _ => 0.5,
                };
                let kind = brush.kind;
                sel.par_map_into(new, |i, v| {
                    let p = mesh.positions[v as usize];
                    let img = sel.image(i);
                    let (c, n) = (images.map(img, c), images.map(img, n));
                    let d=(p-c).dot(n);
                    let amount=match kind {
                        BrushKind::FormHard => (r*0.18-d*sign).max(0.0).min(r*0.12)*sign*brush.strength*w[i],
                        BrushKind::Clay | BrushKind::ClayBuildup | BrushKind::ClayTubes => {
                            let buildup=match kind {BrushKind::ClayBuildup=>1.8,BrushKind::ClayTubes=>0.8,_=>1.0};
                            ((r*0.12*sign-d)*sign).max(0.0).min(r*0.08*buildup)*sign*k*w[i]
                        },
                        BrushKind::TrimDynamic | BrushKind::TrimAdaptive | BrushKind::TrimSmoothBorder=>-((d*sign).max(0.0).min(r*0.2))*sign*k*w[i],
                        BrushKind::Fill=>(-d*sign).max(0.0).min(r*0.2)*sign*k*w[i],
                        BrushKind::Scrape=>-(d*sign+r*0.035).max(0.0).min(r*0.2)*sign*k*w[i],
                        BrushKind::MalletFast=>-(d*sign+r*0.10).max(0.0).min(r*0.25)*sign*k*w[i],
                        _=>-d*k*w[i],
                    };
                    // TrimSmoothBorder relaxes the ring where the cut fades out, so it leaves
                    // no ledge.
                    let border = if kind == BrushKind::TrimSmoothBorder {
                        let nb = mesh.neighbors(v);
                        if nb.is_empty() { Vec3::ZERO } else {
                            let ring = 4.0 * w[i] * (1.0 - w[i]);
                            (nb.iter().map(|&q| mesh.positions[q as usize]).sum::<Vec3>() / nb.len() as f32 - p) * (0.6 * brush.strength * ring)
                        }
                    } else { Vec3::ZERO };
                    p+n*amount+border
                });
            }
            BrushKind::DamStandard | BrushKind::Pinch | BrushKind::Crease | BrushKind::OrbCracks | BrushKind::Magnify => {
                let kind = brush.kind;
                let (copies, copy) = (images, k);
                let images = &self.images;
                sel.par_map_into(new,|i,v| {
                    let p=mesh.positions[v as usize];
                    let k=sel.image(i);
                    let (c,n)=(images.map(k,hit),images.map(k,hit_normal));
                    let delta=c-p;let tangent=delta-n*delta.dot(n);
                    let width=if kind==BrushKind::OrbCracks {0.13}else{0.24};
                    let tight=(-((sel.dist[i]/r)/width).powi(2)).exp();
                    let (pinch,cut)=match kind {
                        BrushKind::Pinch=>(0.28,0.0),
                        BrushKind::Crease=>(0.15,0.055),
                        BrushKind::OrbCracks=>(0.12,0.16),
                        // Spread outward and swell: Pinch turned inside out.
                        BrushKind::Magnify=>(-0.22,-0.025),
                        _=>(0.06,0.1),
                    };
                    // Orb Cracks: the depth wanders along the cut, so it reads as a crack,
                    // not a groove. Taken on the brush's own side so symmetry mirrors it.
                    let rough=if kind==BrushKind::OrbCracks {
                        let q=copies.unmap(copy,p)*(9.0/r);
                        0.7+0.3*((q.x*1.7+q.y).sin()*(q.y*1.3-q.z).sin()*(q.z*1.9+q.x*0.6).sin())
                    }else{1.0};
                    p+tangent*(pinch*brush.strength*w[i]*sign)
                        -n*(cut*r*brush.strength*w[i]*tight*rough*sign)
                });
            }
            BrushKind::Paint=>unreachable!(),
            BrushKind::Grab | BrushKind::Elastic | BrushKind::SnakeHook | BrushKind::MoveTopological => unreachable!(),
            _ => unreachable!("gesture and detail tools are handled before normal dabs"),
        }
        mesh.scatter_positions(&sel, new);
        mesh.finish_edit(&chunks);
    }

    /// A surface segment carries a continuous repeat phase, independent of input rate.
    fn detail_segment(&mut self,mesh:&mut SculptMesh,brush:&Brush,start:Vec3,end:Vec3,normal:Vec3) {
        let r=brush.radius;let delta=end-start;let length=delta.length();
        let direction=delta.normalize_or(tangent(normal).0);
        let sideways=normal.cross(direction).normalize_or(tangent(normal).1);
        let centre=(start+end)*0.5;
        let sel=self.select(mesh,centre,r+length*0.5);
        if sel.is_empty(){return;}
        let chunks=sel.chunks();self.snapshot(mesh,&chunks);
        let auto=self.auto_weights(mesh,&sel);
        let images=self.images.clone();
        self.paint_selected(mesh,&sel,brush,centre,normal,auto.as_deref(),&images,None);
        let phase=self.detail_distance;self.detail_distance+=length;
        let sign=if self.invert{-1.0}else{1.0};
        let next=sel.par_map(|i,v| {
            let actual=mesh.positions[v as usize];
            let k=sel.image(i);
            let p=self.images.unmap(k,actual);
            let offset=p-start;let along=offset.dot(direction);let across=offset.dot(sideways);
            if along < -r*0.04 || along > length+r*0.04 || across.abs()>r || offset.dot(normal).abs()>r*0.65 {return actual;}
            let weight=focal_falloff(across.abs(),r,brush.focal_shift)
                *self.stamp_weight(&self.images,k,actual,centre,normal,r)*crate::mask::freedom(&mesh.mask,v)
                *auto.as_ref().map_or(1.0,|a|a[i]);
            if weight<=0.0 {return actual;}
            let height=crate::detail_brush::profile(brush.kind,across/r,(phase+along)/r)*r*0.12*brush.strength*sign*weight;
            let chunk=mesh.vert_chunk[v as usize];
            let old=self.snapshots[self.snapshot_index[&chunk]].1[(v-mesh.chunks[chunk as usize].vert_start) as usize];
            let n=self.images.map(k,normal);
            actual+n*(height-(actual-old).dot(n))
        });
        mesh.scatter_positions(&sel,&next);mesh.finish_edit(&chunks);
        self.last_count=sel.len();self.dabs+=1;
    }

    /// Position of vertex `v` when this stroke first touched its chunk.
    fn stroke_start_position(&self, mesh: &SculptMesh, v: u32) -> Vec3 {
        let chunk = mesh.vert_chunk[v as usize];
        match self.snapshot_index.get(&chunk) {
            Some(&k) => self.snapshots[k].1[(v - mesh.chunks[chunk as usize].vert_start) as usize],
            None => mesh.positions[v as usize],
        }
    }

    /// A detail relief (Zipper, Stitch, Rake) along a whole curve at once: every vertex near
    /// it is set from its nearest point on the curve, so the pattern follows the curve's
    /// smooth direction and is spaced by its arc length (see `crate::curve`). Heights are set
    /// relative to the stroke's start, as in the freehand relief.
    pub fn detail_curve(&mut self, mesh: &mut SculptMesh, brush: &Brush, curve: &crate::curve::Curve) {
        if curve.is_empty() {
            return;
        }
        let r = brush.radius;
        let (sel, feet) = crate::curve::band(mesh, curve, r, &self.images);
        if sel.is_empty() {
            return;
        }
        let chunks = sel.chunks();
        if brush.sculpt {
            self.snapshot(mesh, &chunks);
        }
        let mid = curve.points.len() / 2;
        let auto = self.auto_weights(mesh, &sel);
        let images = self.images.clone();
        self.paint_selected(mesh, &sel, brush, curve.points[mid], curve.normals[mid], auto.as_deref(), &images, None);
        self.last_count = sel.len();
        self.dabs += 1;
        if !brush.sculpt || !brush.kind.is_detail() {
            return;
        }
        let sign = if self.invert { -1.0 } else { 1.0 };
        let next = sel.par_map(|i, v| {
            let f = feet[i];
            let actual = mesh.positions[v as usize];
            // The foot is on the vertex's own side, so the stamp needs no mirroring.
            let weight = focal_falloff(f.across.abs(), r, brush.focal_shift)
                * self.stamp_weight(&Images::identity(), 0, actual, f.point, f.normal, r)
                * crate::mask::freedom(&mesh.mask, v)
                * auto.as_ref().map_or(1.0, |a| a[i]);
            if weight <= 0.0 {
                return actual;
            }
            let height = crate::detail_brush::profile(brush.kind, f.across / r, f.along / r) * r * 0.12 * brush.strength * sign * weight;
            let old = self.stroke_start_position(mesh, v);
            actual + f.normal * (height - (actual - old).dot(f.normal))
        });
        mesh.scatter_positions(&sel, &next);
        mesh.finish_edit(&chunks);
    }

    /// Dab brushes along a curve: a dab every `step` of arc length (at least the freehand
    /// dab spacing), at the curve's surface points and normals. Each piece starts afresh.
    pub fn dabs_along(&mut self, mesh: &mut SculptMesh, brush: &Brush, curve: &crate::curve::Curve, view_dir: Vec3, step: f32) {
        let step = step.max(brush.radius * 0.16);
        for piece in &curve.pieces {
            self.last_dab = None;
            let mut next = curve.along[piece.start];
            for i in piece.clone() {
                if curve.along[i] + 1e-6 < next {
                    continue;
                }
                let (p, n) = (curve.points[i], curve.normals[i]);
                self.dab(mesh, brush, p, n, view_dir, (p + n * brush.radius * 4.0, -n));
                next = curve.along[i] + step;
            }
        }
        self.last_dab = None;
    }

    /// Record the positions and colours of `chunks` for undo now, before the caller changes
    /// them outside the brush (a bent curve restores its base first).
    pub fn snapshot_now(&mut self, mesh: &SculptMesh, chunks: &[u32]) {
        self.snapshot(mesh, chunks);
        // Colours only once the model has paint: an unpainted level has none to keep.
        if !mesh.colors.is_empty() {
            let fresh: Vec<_> = chunks.iter().copied().filter(|c| self.paint_snapshotted.insert(*c)).collect();
            self.paint_snapshots.extend(crate::paint::snapshot(mesh, &fresh));
        }
    }

    /// Positions of every chunk the stroke touched, from before it touched them.
    pub fn position_snapshots(&self) -> &[(u32, Vec<Vec3>)] {
        &self.snapshots
    }

    /// Colours of every chunk the stroke painted, from before it painted them.
    pub fn paint_snapshots(&self) -> &crate::paint::Chunks {
        &self.paint_snapshots
    }

    fn grab_dab(&mut self, mesh: &mut SculptMesh, brush: &Brush, hit: Vec3, view_dir: Vec3, ray: (Vec3, Vec3)) {
        if self.grab.is_none() {
            let sel = self.select(mesh, hit, brush.radius);
            let auto = self.auto_weights(mesh, &sel);
            let images = self.images.clone();
            self.paint_selected(mesh,&sel,brush,hit,-view_dir,auto.as_deref(),&images,None);
            let chunks = sel.chunks();
            self.snapshot(mesh, &chunks);
            let r = brush.radius;
            // Each symmetry copy moves with its own image of the delta (`grab_offset`).
            let mask = &mesh.mask;
            let weight = |i: usize, d: f32, k: usize| {
                let f = focal_falloff(d, r, brush.focal_shift);
                let profile = match brush.kind { BrushKind::SnakeHook => f.powi(3), BrushKind::Elastic => f.sqrt(), _ => f };
                profile * self.stamp_weight(&self.images, k, mesh.positions[sel.verts[i] as usize], hit, view_dir, r)
                    * crate::mask::freedom(mask, sel.verts[i])
            };
            // Per vertex: Move Topological's and Auto Masking's factor.
            let joined = (brush.kind == BrushKind::MoveTopological).then(|| connected(mesh, &sel));
            let keep = |i: usize| joined.as_ref().map_or(1.0, |j| j[i] as u8 as f32) * auto.as_ref().map_or(1.0, |a| a[i]);
            let weights: Vec<f32> = (0..sel.len()).into_par_iter().map(|i| weight(i, sel.dist[i], sel.image(i)) * keep(i)).collect();
            // Vertices several copies reach get every copy's weight.
            let (mut overlap_of, mut overlaps) = (Vec::new(), Vec::new());
            if self.images.len() > 1 {
                let centres = self.images.of(hit);
                let shared: Vec<Option<Vec<(u8, f32)>>> = (0..sel.len())
                    .into_par_iter()
                    .map(|i| {
                        let p = mesh.positions[sel.verts[i] as usize];
                        let own = sel.image(i);
                        let others: Vec<(u8, f32)> = centres
                            .iter()
                            .enumerate()
                            .filter(|&(k, c)| k != own && p.distance(*c) <= r)
                            .map(|(k, c)| (k as u8, weight(i, p.distance(*c), k) * keep(i)))
                            .collect();
                        (!others.is_empty()).then(|| std::iter::once((own as u8, weights[i])).chain(others).collect())
                    })
                    .collect();
                if shared.iter().any(Option::is_some) {
                    overlap_of = shared
                        .into_iter()
                        .map(|list| match list {
                            Some(list) => {
                                overlaps.push(list);
                                (overlaps.len() - 1) as u32
                            }
                            None => u32::MAX,
                        })
                        .collect();
                }
            }
            let orig = sel.par_map(|_, v| mesh.positions[v as usize]);
            self.last_count = sel.len();
            // A brush smaller than the polygons under it picks up no vertex: nothing to
            // hand to the GPU (it would bind empty buffers), the stroke just does nothing.
            // Overlapping copies add up on the CPU; the GPU kernel moves each vertex by one.
            let gpu = self.gpu_grab && brush.kind.gpu_drag() && !sel.is_empty() && overlaps.is_empty();
            self.grab = Some(GrabState {
                sel,
                chunks,
                weights,
                orig,
                anchor: hit,
                gpu,
                delta: Vec3::ZERO,
                kind: brush.kind,
                radius: r,
                overlap_of,
                overlaps,
            });
            return;
        }
        let g = self.grab.as_mut().unwrap();
        // Intersect the cursor ray with the view-aligned plane through the anchor.
        let (o, d) = ray;
        let denom = d.dot(view_dir);
        if denom.abs() < 1e-6 {
            return;
        }
        let t = (g.anchor - o).dot(view_dir) / denom;
        let target = o + d * t;
        let delta = (target - g.anchor) * brush.strength.max(0.05) * if g.kind==BrushKind::SnakeHook {2.6}else{2.0};
        g.delta = delta;
        self.dabs += 1;
        if g.gpu {
            // Nothing to do on the CPU per frame: the GPU kernel moves the vertices.
            self.pending_delta = Some((0..self.images.len()).map(|k| self.images.map(k, delta)).collect());
            return;
        }
        // Fused compute + write: no temporary buffer, one parallel pass per chunk.
        let images = &self.images;
        mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + drag_offset(images, g, i));
        mesh.finish_edit(&g.chunks);
    }

    /// Finish the stroke and return the positions it replaced, per chunk, if anything
    /// changed. A GPU grab is applied to the CPU mesh here, once.
    pub fn end(mut self, mesh: &mut SculptMesh) -> Option<ChunkData> {
        if let Some(g) = self.grab.take() {
            if g.gpu && g.delta != Vec3::ZERO {
                let images = &self.images;
                mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + grab_offset(images, &g, i));
                mesh.finish_edit(&g.chunks);
            }
        }
        if self.snapshots.is_empty() { None } else { Some(self.snapshots) }
    }
}

/// Which selected vertices the surface joins to the brush centre without leaving the
/// brush: a flood over mesh edges from the vertex nearest the centre (and, with symmetry,
/// from the one nearest each image of it).
fn connected(mesh: &SculptMesh, sel: &Selection) -> Vec<bool> {
    let index: HashMap<u32, usize> = sel.verts.iter().enumerate().map(|(i, &v)| (v, i)).collect();
    let mut joined = vec![false; sel.len()];
    let mut stack = Vec::new();
    let mut seeds: HashMap<usize, usize> = HashMap::new();
    for i in 0..sel.len() {
        let seed = seeds.entry(sel.image(i)).or_insert(i);
        if sel.dist[i] < sel.dist[*seed] {
            *seed = i;
        }
    }
    for &i in seeds.values() {
        joined[i] = true;
        stack.push(i);
    }
    while let Some(i) = stack.pop() {
        for &n in mesh.neighbors(sel.verts[i]) {
            if let Some(&j) = index.get(&n) {
                if !joined[j] {
                    joined[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    joined
}

/// Square footprint of side 1.4 `r` centred on `hit`, turned to run along `along` in the
/// surface. Its sides have a short soft edge. A stamp (`hard_ends`, MalletFast) has hard
/// ends too; a stroked strip (ClayTubes, hPolish) fades smoothly front to back so
/// overlapping dabs blend into one even strip instead of leaving a ridge per dab.
fn square_footprint(p: Vec3, hit: Vec3, normal: Vec3, along: Vec3, r: f32, hard_ends: bool) -> f32 {
    let delta = p - hit;
    let across = normal.cross(along);
    let half = r * 0.7;
    let edge = |t: f32| {
        let e = ((1.0 - t) / 0.18).clamp(0.0, 1.0);
        e * e * (3.0 - 2.0 * e)
    };
    let t_along = delta.dot(along).abs() / half;
    let ends = if hard_ends { edge(t_along) } else { falloff(t_along, 1.0) };
    edge(delta.dot(across).abs() / half) * ends
}

/// Smooth falloff from 1 at the centre to 0 at the radius.
pub fn falloff(d: f32, r: f32) -> f32 {
    let x = (d / r).clamp(0.0, 1.0);
    let t = 1.0 - x * x;
    t * t
}

pub fn focal_falloff(d:f32,r:f32,shift:f32)->f32 {
    let f=(shift/100.0).clamp(-1.0,1.0);
    if f==0.0 {return falloff(d,r);}
    let plateau=f.max(0.0)*0.9;
    let x=((d/r-plateau)/(1.0-plateau)).clamp(0.0,1.0);
    (1.0-x*x).powf(2.0+(-f).max(0.0)*6.0)
}

#[cfg(test)]
mod brush_tests {
    use super::*;
    fn mesh()->SculptMesh {let (p,i)=crate::mesh::cube_sphere(24,1.0);SculptMesh::from_raw(p,i)}
    fn nearest(m:&SculptMesh,p:Vec3)->usize {(0..m.vertex_count()).min_by(|&a,&b|m.positions[a].distance_squared(p).total_cmp(&m.positions[b].distance_squared(p))).unwrap()}
    fn dab(m:&mut SculptMesh,k:BrushKind,invert:bool)->Stroke {
        let mut s=Stroke::begin(invert,false,false);
        s.dab(m,&Brush {kind:k,radius:0.4,strength:1.0,..Brush::default()},Vec3::Z,Vec3::Z,-Vec3::Z,(Vec3::Z*5.0,-Vec3::Z));s
    }
    #[test]
    fn dam_standard_cuts_and_alt_raises_the_same_crease() {
        let mut a=mesh();let mut b=mesh();let i=nearest(&a,Vec3::Z);let z=a.positions[i].z;
        dab(&mut a,BrushKind::DamStandard,false);dab(&mut b,BrushKind::DamStandard,true);
        assert!(a.positions[i].z<z-0.01);assert!(b.positions[i].z>z+0.01);
    }
    #[test]
    fn pinch_contracts_tangentially_and_inverts_to_spread() {
        let mut a=mesh();let mut b=mesh();let i=nearest(&a,Vec3::new(0.18,0.0,0.98));let p=a.positions[i];
        dab(&mut a,BrushKind::Pinch,false);dab(&mut b,BrushKind::Pinch,true);
        assert!(a.positions[i].truncate().length()<p.truncate().length());
        assert!(b.positions[i].truncate().length()>p.truncate().length());
        assert_eq!(a.positions[i].z,p.z);
    }
    #[test]
    fn added_sculpt_brushes_edit_and_round_trip_through_undo() {
        for k in [BrushKind::DamStandard,BrushKind::Pinch,BrushKind::Clay,BrushKind::ClayBuildup,BrushKind::Crease,BrushKind::TrimDynamic,BrushKind::Polish] {
            let mut doc=Multires::new(mesh(),100_000);let before=doc.mesh().positions.clone();
            let s=dab(doc.mesh_mut(),k,false);let data=s.end(doc.mesh_mut()).unwrap();
            let data=doc.finish_stroke(0,data);let after=doc.mesh().positions.clone();
            assert_ne!(before,after,"{k:?} must change the mesh");assert!(after.iter().all(|v|v.is_finite()));
            let mut h=History::new(8);h.push(UndoStep::Stroke{level:0,chunks:data});h.undo(&mut doc);
            assert_eq!(doc.mesh().positions,before,"{k:?} undo");h.redo(&mut doc);assert_eq!(doc.mesh().positions,after);
        }
    }
    #[test]
    fn opaque_black_alpha_blocks_sculpting_and_mask_blocks_new_brushes() {
        let mut m=mesh();let before=m.positions.clone();
        let image=crate::brush_asset::BrushImage {id:999,name:"black".into(),source:"black.png".into(),width:2,height:2,rgba:vec![[0,0,0,255];4]};
        let mut s=Stroke::begin(false,false,false).with_assets(StampSettings {alpha:Pattern::Image(std::sync::Arc::new(image)),..StampSettings::default()});
        s.dab(&mut m,&Brush::default(),Vec3::Z,Vec3::Z,-Vec3::Z,(Vec3::Z*5.0,-Vec3::Z));assert_eq!(m.positions,before);
        m.mask=vec![255;m.vertex_count()];dab(&mut m,BrushKind::DamStandard,false);assert_eq!(m.positions,before);
    }
    #[test]
    fn focal_shift_changes_profile_and_preserves_the_neutral_profile() {
        assert_eq!(focal_falloff(0.5,1.0,0.0),falloff(0.5,1.0));
        assert!(focal_falloff(0.5,1.0,80.0)>falloff(0.5,1.0));
        assert!(focal_falloff(0.5,1.0,-80.0)<falloff(0.5,1.0));
        for s in [-100.0,0.0,100.0] {assert_eq!(focal_falloff(1.0,1.0,s),0.0);assert_eq!(focal_falloff(0.0,1.0,s),1.0);}
    }

    const NEW: [BrushKind;8] = [BrushKind::Blob,BrushKind::Layer,BrushKind::Fill,BrushKind::Scrape,
        BrushKind::Nudge,BrushKind::Sharpen,BrushKind::Elastic,BrushKind::SnakeHook];

    fn stroke_path(m:&mut SculptMesh,kind:BrushKind,gpu:bool,symmetry:bool)->Stroke {
        let b=Brush{kind,radius:0.4,strength:0.8,..Brush::default()};
        let mut s=Stroke::begin(false,gpu,symmetry);
        for x in [0.0,0.08,0.16,0.24] {
            let hit=Vec3::new(x,0.0,1.0);
            s.dab(m,&b,hit,Vec3::Z,-Vec3::Z,(hit+Vec3::Z*4.0,-Vec3::Z));
        }
        s
    }

    #[test]
    fn drag_brushes_smaller_than_a_polygon_do_nothing() {
        // A coarse cube sphere: faces far wider than the brush, pressed mid-face.
        let (p,i)=crate::mesh::cube_sphere(2,1.0);
        for kind in [BrushKind::Grab,BrushKind::SnakeHook,BrushKind::Elastic,BrushKind::MoveTopological] {
            for gpu in [false,true] {
                let mut m=SculptMesh::from_raw(p.clone(),i.clone());
                let before=m.positions.clone();
                let b=Brush{kind,radius:1e-4,strength:1.0,..Brush::default()};
                let mut s=Stroke::begin(false,gpu,false);
                let hit=Vec3::new(0.1,0.13,1.0);
                for k in 0..4 {
                    s.dab(&mut m,&b,hit,Vec3::Z,-Vec3::Z,(hit+Vec3::new(k as f32*0.1,0.0,4.0),-Vec3::Z));
                }
                assert!(s.gpu_grab_setup().is_none(),"{kind:?}: nothing to hand to the GPU");
                assert!(s.end(&mut m).is_none(),"{kind:?}: no undo step");
                assert_eq!(m.positions,before,"{kind:?} gpu={gpu}");
            }
        }
    }

    #[test]
    fn new_brushes_change_geometry_and_round_trip_across_levels() {
        for kind in NEW {
            let mut doc=Multires::new(mesh(),100_000);assert!(doc.divide());
            let before=doc.mesh().positions.clone();
            let s=stroke_path(doc.mesh_mut(),kind,false,false);
            let snapshot=s.end(doc.mesh_mut()).unwrap();
            let data=doc.finish_stroke(1,snapshot);
            let after=doc.mesh().positions.clone();
            assert_ne!(before,after,"{kind:?}");assert!(after.iter().all(|p|p.is_finite()));
            let mut h=History::new(8);h.push(UndoStep::Stroke{level:1,chunks:data});
            doc.set_active(0);h.undo(&mut doc);assert_eq!(doc.active(),0);
            doc.set_active(1);
            assert_eq!(doc.mesh().positions,before,"{kind:?} undo");h.redo(&mut doc);
            assert!(doc.mesh().positions.iter().zip(&after).all(|(a,b)|a.distance(*b)<1e-6),"{kind:?} redo");
        }
    }

    #[test]
    fn layer_caps_each_stroke_and_next_stroke_can_build_again() {
        let mut m=mesh();let centre=nearest(&m,Vec3::Z);let before=m.positions[centre];
        let brush=Brush{kind:BrushKind::Layer,radius:0.4,strength:1.0,..Brush::default()};
        let run=|m:&mut SculptMesh| {
            let mut s=Stroke::begin(false,false,false);
            for i in 0..80 {
                let hit=Vec3::new(if i%2==0{-0.04}else{0.04},0.0,1.0);
                s.dab(m,&brush,hit,Vec3::Z,-Vec3::Z,(hit+Vec3::Z*4.0,-Vec3::Z));
            }
            s.end(m);
        };
        run(&mut m);let height=m.positions[centre].z-before.z;
        assert!(height>0.05 && height<=0.4*0.14+1e-6,"layer height {height}");
        run(&mut m);assert!(m.positions[centre].z-before.z>height*1.9);
    }

    #[test]
    fn fill_and_scrape_are_one_sided_and_alt_reverses_the_side() {
        for kind in [BrushKind::Fill,BrushKind::Scrape] {
            for invert in [false,true] {
                let mut m=mesh();let before=m.positions.clone();dab(&mut m,kind,invert);
                let changes:Vec<_>=m.positions.iter().zip(&before).map(|(a,b)|a.z-b.z).collect();
                let raises=(kind==BrushKind::Fill)^invert;
                assert!(changes.iter().all(|&d|if raises {d>=-1e-7}else{d<=1e-7}),"{kind:?} {invert}");
                assert!(changes.iter().any(|d|d.abs()>1e-6));
            }
        }
    }

    #[test]
    fn every_new_brush_respects_full_masks_and_hidden_geometry() {
        for kind in NEW {
            for hidden in [false,true] {
                let mut m=mesh();let before=m.positions.clone();
                if hidden {m.set_hidden(vec![1;m.triangle_count()]);}
                else {m.mask=vec![255;m.vertex_count()];}
                stroke_path(&mut m,kind,false,false).end(&mut m);
                assert_eq!(m.positions,before,"{kind:?}, hidden {hidden}");
            }
        }
    }

    /// A Snake Hook pull on +X, clear of its mirror image.
    fn hook_off_centre(m:&mut SculptMesh,gpu:bool)->Stroke {
        let b=Brush{kind:BrushKind::SnakeHook,radius:0.3,strength:0.8,..Brush::default()};
        let mut s=Stroke::begin(false,gpu,true);
        for x in [0.5,0.58,0.66] {
            let hit=Vec3::new(0.5,0.0,0.87);
            s.dab(m,&b,hit,hit,-Vec3::Z,(Vec3::new(x,0.0,0.87)+Vec3::Z*4.0,-Vec3::Z));
        }
        s
    }

    #[test]
    fn snake_hook_gpu_weights_match_cpu_and_elastic_uses_directional_deformation() {
        let mut cpu=mesh();let mut gpu=mesh();
        hook_off_centre(&mut cpu,false).end(&mut cpu);
        let before=gpu.positions.clone();
        let s=hook_off_centre(&mut gpu,true);
        assert_eq!(gpu.positions,before);assert!(s.gpu_grab_setup().is_some());
        s.end(&mut gpu);assert_eq!(gpu.positions,cpu.positions);
        let mut elastic=mesh();let s=stroke_path(&mut elastic,BrushKind::Elastic,true,false);
        assert!(s.gpu_grab_setup().is_none());s.end(&mut elastic);
        let mut grab=mesh();stroke_path(&mut grab,BrushKind::Grab,false,false).end(&mut grab);
        assert_ne!(elastic.positions,grab.positions);
        let mut snake=mesh();stroke_path(&mut snake,BrushKind::SnakeHook,false,false).end(&mut snake);
        assert_ne!(snake.positions,grab.positions);
    }
}

#[cfg(test)]
mod symmetry_tests {
    use super::*;
    use crate::mesh::cube_sphere;

    fn sphere() -> SculptMesh {
        let (p, i) = cube_sphere(40, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn nearest(mesh: &SculptMesh, p: Vec3) -> u32 {
        (0..mesh.vertex_count() as u32)
            .min_by(|&a, &b| {
                let da = (mesh.positions[a as usize] - p).length_squared();
                let db = (mesh.positions[b as usize] - p).length_squared();
                da.total_cmp(&db)
            })
            .unwrap()
    }

    #[test]
    fn draw_with_symmetry_moves_the_mirror_image_too() {
        let mut mesh = sphere();
        let hit = Vec3::new(0.6, 0.3, 0.74).normalize();
        let mirror = Vec3::new(-hit.x, hit.y, hit.z);
        let (a, b) = (nearest(&mesh, hit), nearest(&mesh, mirror));
        let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0, ..Brush::default() };
        let mut stroke = Stroke::begin(false, false, true);
        stroke.dab(&mut mesh, &brush, hit, hit, -Vec3::Z, (Vec3::Z * 5.0, -Vec3::Z));
        let (da, db) = (mesh.positions[a as usize] - pa, mesh.positions[b as usize] - pb);
        assert!(da.length() > 1e-3, "brush side did not move: {da:?}");
        // Mirror image of the displacement, within the discretisation of the mesh.
        let expect = Vec3::new(-da.x, da.y, da.z);
        assert!((db - expect).length() < da.length() * 0.2, "{db:?} vs {expect:?}");
    }

    #[test]
    fn draw_without_symmetry_leaves_the_mirror_image_alone() {
        let mut mesh = sphere();
        let hit = Vec3::new(0.6, 0.3, 0.74).normalize();
        let mirror = Vec3::new(-hit.x, hit.y, hit.z);
        let b = nearest(&mesh, mirror);
        let pb = mesh.positions[b as usize];
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0, ..Brush::default() };
        let mut stroke = Stroke::begin(false, false, false);
        stroke.dab(&mut mesh, &brush, hit, hit, -Vec3::Z, (Vec3::Z * 5.0, -Vec3::Z));
        assert_eq!(mesh.positions[b as usize], pb);
    }

    #[test]
    fn cpu_grab_with_symmetry_mirrors_the_pull() {
        let mut mesh = sphere();
        let hit = Vec3::new(0.6, 0.3, 0.74).normalize();
        let mirror = Vec3::new(-hit.x, hit.y, hit.z);
        let (a, b) = (nearest(&mesh, hit), nearest(&mesh, mirror));
        let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
        let brush = Brush { kind: BrushKind::Grab, radius: 0.3, strength: 1.0, ..Brush::default() };
        let mut stroke = Stroke::begin(false, false, true);
        let view = -Vec3::Z;
        // Anchor the grab, then pull the cursor 0.1 to +X in the view plane.
        stroke.dab(&mut mesh, &brush, hit, hit, view, (hit + Vec3::Z * 5.0, -Vec3::Z));
        let o = hit + Vec3::new(0.1, 0.0, 5.0);
        stroke.dab(&mut mesh, &brush, hit, hit, view, (o, -Vec3::Z));
        let (da, db) = (mesh.positions[a as usize] - pa, mesh.positions[b as usize] - pb);
        assert!(da.x > 0.05, "pull side moved {da:?}");
        assert!(db.x < -0.05, "mirror side should move the other way: {db:?}");
        assert!((da.y - db.y).abs() < 1e-3 && (da.z - db.z).abs() < 1e-3);
        assert!(stroke.end(&mut mesh).is_some());
    }
}

#[cfg(test)]
mod pressure_tests {
    use super::*;

    #[test]
    fn pen_pressure_scales_size_and_intensity_like_zbrush() {
        let b = Brush { kind: BrushKind::Draw, radius: 0.4, strength: 0.8, ..Brush::default() };
        // A mouse: unchanged.
        let m = b.at_pressure(None, true, true);
        assert_eq!((m.radius, m.strength), (0.4, 0.8));
        // Linear on both by default.
        let h = b.at_pressure(Some(0.5), true, true);
        assert!((h.radius - 0.2).abs() < 1e-6 && (h.strength - 0.4).abs() < 1e-6);
        // Each can be turned off.
        let s = b.at_pressure(Some(0.5), false, true);
        assert_eq!(s.radius, 0.4);
        let i = b.at_pressure(Some(0.5), true, false);
        assert_eq!(i.strength, 0.8);
        // The lightest touch keeps a small, non-zero radius and no strength.
        let z = b.at_pressure(Some(0.0), true, true);
        assert!((z.radius - 0.4 * MIN_PRESSURE_SIZE).abs() < 1e-6 && z.strength == 0.0);
        // Out of range pressure is clamped.
        let o = b.at_pressure(Some(1.7), true, true);
        assert_eq!((o.radius, o.strength), (0.4, 0.8));
        assert_eq!(b.at_pressure(Some(-1.0), true, true).strength, 0.0);
        // Paint's RGB Intensity follows Z Intensity's toggle.
        assert!((h.rgb_intensity - 0.5 * b.rgb_intensity).abs() < 1e-6);
        assert_eq!(i.rgb_intensity, b.rgb_intensity);
        // Drag brushes are untouched; detail reliefs keep their size.
        for kind in [BrushKind::Grab, BrushKind::Elastic, BrushKind::SnakeHook] {
            let g = Brush { kind, ..b }.at_pressure(Some(0.2), true, true);
            assert_eq!((g.radius, g.strength), (0.4, 0.8), "{kind:?}");
        }
        let z = Brush { kind: BrushKind::Zipper, ..b }.at_pressure(Some(0.5), true, true);
        assert_eq!(z.radius, 0.4);
        assert!((z.strength - 0.4).abs() < 1e-6);
    }
}

#[cfg(test)]
mod curve_tests {
    use super::*;
    use crate::curve::Curve;
    use crate::primitive::Primitive;

    fn plane() -> SculptMesh {
        let (p, i) = Primitive::Plane.build(160, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// A curve along +X at height `y` on the plane z = 0, from `x0` to `x1`.
    fn line(x0: f32, x1: f32, y: f32) -> Curve {
        let hits: Vec<_> = (0..=40).map(|i| Some((Vec3::new(x0 + (x1 - x0) * i as f32 / 40.0, y, 0.0), Vec3::Z))).collect();
        Curve::from_hits(&hits, 0.01)
    }

    fn zipper() -> Brush {
        Brush { kind: BrushKind::Zipper, radius: 0.25, strength: 1.0, ..Brush::default() }
    }

    #[test]
    fn zipper_along_a_curve_is_the_profile_in_the_curves_own_frame() {
        let mut m = plane();
        let before = m.positions.clone();
        let b = zipper();
        let mut s = Stroke::begin(false, false, false);
        s.detail_curve(&mut m, &b, &line(-0.6, 0.6, 0.0));
        assert!(s.end(&mut m).is_some());
        // Every vertex is set once from its own place: teeth square to the line, spaced by
        // arc length from its start, nothing past the ends.
        let r = b.radius;
        let mut checked = 0;
        for (p, q) in m.positions.iter().zip(&before) {
            if q.x < -0.6 - 0.02 * r || q.x > 0.6 + 0.02 * r || q.y.abs() > r {
                assert_eq!(p, q, "outside the band");
                continue;
            }
            let expect = crate::detail_brush::profile(BrushKind::Zipper, q.y / r, (q.x + 0.6) / r) * r * 0.12 * focal_falloff(q.y.abs(), r, 0.0);
            assert!((p.z - expect).abs() < 1e-4, "{q:?}: {} vs {expect}", p.z);
            assert!((p.truncate() - q.truncate()).length() < 1e-6, "moves along the normal only");
            checked += 1;
        }
        assert!(checked > 1000);
    }

    #[test]
    fn curve_reliefs_respect_masks_and_mirror_with_symmetry() {
        let b = zipper();
        // Fully masked: nothing moves.
        let mut m = plane();
        m.mask = vec![255; m.vertex_count()];
        let before = m.positions.clone();
        let mut s = Stroke::begin(false, false, false);
        s.detail_curve(&mut m, &b, &line(-0.6, 0.6, 0.0));
        assert_eq!(m.positions, before);
        // With symmetry, a curve on +X leaves its mirror image on -X.
        let mut m = plane();
        let mut s = Stroke::begin(false, false, true);
        s.detail_curve(&mut m, &b, &line(0.15, 0.65, 0.1));
        let height = |m: &SculptMesh, x: f32, y: f32| {
            m.positions.iter().min_by(|a, b| (a.truncate() - glam::Vec2::new(x, y)).length().total_cmp(&(b.truncate() - glam::Vec2::new(x, y)).length())).unwrap().z
        };
        let mut moved = 0;
        for k in 0..60 {
            let (x, y) = (0.2 + 0.4 * k as f32 / 60.0, 0.1 + 0.2 * ((k * 7 % 11) as f32 / 11.0 - 0.5));
            let (a, b) = (height(&m, x, y), height(&m, -x, y));
            assert!((a - b).abs() < 1e-5, "({x}, {y}): {a} vs mirrored {b}");
            moved += (a.abs() > 1e-4) as usize;
        }
        assert!(moved > 10);
    }

    #[test]
    fn dab_brushes_step_evenly_along_a_curve() {
        let mut m = plane();
        let before = m.positions.clone();
        let b = Brush { kind: BrushKind::Draw, radius: 0.1, strength: 0.5, ..Brush::default() };
        let mut s = Stroke::begin(false, false, false);
        s.dabs_along(&mut m, &b, &line(-0.5, 0.5, 0.0), -Vec3::Z, 0.05);
        // A dab every 0.05 over a 1.0 line: 21 dabs, and the ridge runs end to end.
        assert_eq!(s.dabs, 21);
        let raised = |x: f32| m.positions.iter().zip(&before).filter(|(_, q)| (q.x - x).abs() < 0.01 && q.y.abs() < 0.02).all(|(p, q)| p.z > q.z);
        assert!(raised(-0.45) && raised(0.0) && raised(0.45));
    }
}

#[cfg(test)]
mod advanced_brush_tests {
    use super::*;
    use crate::primitive::Primitive;

    /// The twelve brushes added in 0.7.
    const ADDED: [BrushKind; 12] = [
        BrushKind::ClayTubes, BrushKind::HPolish, BrushKind::TrimAdaptive, BrushKind::Planar,
        BrushKind::TrimSmoothBorder, BrushKind::MoveTopological, BrushKind::InflateBalloon,
        BrushKind::Magnify, BrushKind::OrbCracks, BrushKind::MalletFast, BrushKind::SmoothValleys,
        BrushKind::SmoothPeaks,
    ];

    fn sphere(n: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(n, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// A bumpy dome over [-1, 1]² whose triangles are exact mirror images across X = 0
    /// (each cell's diagonal flips with the side), so symmetric strokes can be compared
    /// vertex for vertex.
    fn symmetric_dome(n: u32) -> SculptMesh {
        let at = |i: u32, j: u32| {
            let (x, y) = (-1.0 + 2.0 * i as f32 / n as f32, -1.0 + 2.0 * j as f32 / n as f32);
            let bumps = 0.01 * (x.abs() * 23.0).sin() * (y * 19.0).sin();
            Vec3::new(x, y, 1.0 - 0.35 * (x * x + y * y) + bumps)
        };
        let positions = (0..=n).flat_map(|j| (0..=n).map(move |i| (i, j))).map(|(i, j)| at(i, j)).collect();
        let id = |i: u32, j: u32| j * (n + 1) + i;
        let mut indices = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let (a, b, c, d) = (id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1));
                if 2 * i + 1 > n {
                    indices.extend([a, b, c, a, c, d]);
                } else {
                    indices.extend([a, b, d, b, c, d]);
                }
            }
        }
        SculptMesh::from_raw(positions, indices)
    }

    /// A sphere with small bumps and dips (so it has valleys as well as peaks), the same on
    /// both sides of X = 0.
    fn bumpy(n: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(n, 1.0);
        let p = p
            .into_iter()
            .map(|q| q * (1.0 + 0.015 * (q.x.abs() * 23.0).sin() * (q.y * 19.0 + q.z * 7.0).sin()))
            .collect();
        SculptMesh::from_raw(p, i)
    }

    fn nearest(m: &SculptMesh, p: Vec3) -> usize {
        (0..m.vertex_count()).min_by(|&a, &b| m.positions[a].distance_squared(p).total_cmp(&m.positions[b].distance_squared(p))).unwrap()
    }

    /// A short stroke across the top of the model along +X (drag brushes pull with it).
    fn stroke(m: &mut SculptMesh, kind: BrushKind, invert: bool, gpu: bool, symmetry: bool, start: Vec3) -> Stroke {
        let b = Brush { kind, radius: 0.4, strength: 0.8, ..Brush::default() };
        let mut s = Stroke::begin(invert, gpu, symmetry);
        for k in 0..4 {
            let hit = start + Vec3::X * (0.08 * k as f32);
            let n = hit.normalize_or(Vec3::Z);
            s.dab(m, &b, hit, n, -Vec3::Z, (hit + Vec3::Z * 4.0, -Vec3::Z));
        }
        s
    }

    #[test]
    fn every_added_brush_is_listed_named_and_described() {
        for k in ADDED {
            assert!(BrushKind::ALL.contains(&k), "{k:?}");
            assert!(!k.name().is_empty() && k.description().ends_with('.'), "{k:?}");
        }
        assert!(BrushKind::MoveTopological.is_drag() && BrushKind::MoveTopological.gpu_drag());
    }

    #[test]
    fn added_brushes_sculpt_and_round_trip_undo_and_redo_across_levels() {
        for kind in ADDED {
            for invert in [false, true] {
                let mut doc = Multires::new(bumpy(24), 100_000);
                assert!(doc.divide());
                let before = doc.mesh().positions.clone();
                let s = stroke(doc.mesh_mut(), kind, invert, false, false, Vec3::new(-0.1, 0.05, 1.0));
                let snapshot = s.end(doc.mesh_mut()).unwrap();
                let data = doc.finish_stroke(1, snapshot);
                let after = doc.mesh().positions.clone();
                assert_ne!(before, after, "{kind:?} invert {invert}");
                assert!(after.iter().all(|p| p.is_finite()), "{kind:?}");
                let mut h = History::new(8);
                h.push(UndoStep::Stroke { level: 1, chunks: data });
                doc.set_active(0);
                h.undo(&mut doc);
                doc.set_active(1);
                assert_eq!(doc.mesh().positions, before, "{kind:?} undo");
                h.redo(&mut doc);
                assert!(doc.mesh().positions.iter().zip(&after).all(|(a, b)| a.distance(*b) < 1e-6), "{kind:?} redo");
            }
        }
    }

    #[test]
    fn added_brushes_leave_masked_and_hidden_surface_alone() {
        for kind in ADDED {
            for hidden in [false, true] {
                let mut m = bumpy(24);
                let before = m.positions.clone();
                if hidden {
                    m.set_hidden(vec![1; m.triangle_count()]);
                } else {
                    m.mask = vec![255; m.vertex_count()];
                }
                stroke(&mut m, kind, false, false, false, Vec3::new(-0.1, 0.05, 1.0)).end(&mut m);
                assert_eq!(m.positions, before, "{kind:?} hidden {hidden}");
            }
        }
    }

    #[test]
    fn added_brushes_mirror_across_x_with_symmetry() {
        for kind in ADDED {
            let mut m = symmetric_dome(80);
            let before = m.positions.clone();
            // Wholly on +X, so the mirror image is a separate stroke on -X.
            let start = m.positions[nearest(&m, Vec3::new(0.45, 0.2, 1.0))];
            stroke(&mut m, kind, false, false, true, start).end(&mut m);
            let most = m.positions.iter().zip(&before).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
            assert!(most > 1e-4, "{kind:?} moved nothing");
            for probe in [start, start + Vec3::new(0.1, 0.05, 0.0), start + Vec3::new(0.2, -0.08, 0.0), start + Vec3::new(0.05, 0.3, 0.0)] {
                let (a, b) = (nearest(&m, probe), nearest(&m, Vec3::new(-probe.x, probe.y, probe.z)));
                let (da, db) = (m.positions[a] - before[a], m.positions[b] - before[b]);
                let expect = Vec3::new(-da.x, da.y, da.z);
                assert!((db - expect).length() <= most * 0.01 + 1e-6, "{kind:?}: {da:?} mirrored {db:?}");
            }
        }
    }

    #[test]
    fn move_topological_on_the_gpu_matches_the_cpu_and_pen_pressure_leaves_it_alone() {
        // Clear of the mirror image (copies that overlap run on the CPU).
        let mut cpu = sphere(24);
        let mut gpu = sphere(24);
        stroke(&mut cpu, BrushKind::MoveTopological, false, false, true, Vec3::new(0.5, 0.0, 0.87)).end(&mut cpu);
        let before = gpu.positions.clone();
        let s = stroke(&mut gpu, BrushKind::MoveTopological, false, true, true, Vec3::new(0.5, 0.0, 0.87));
        assert_eq!(gpu.positions, before, "moved on the GPU during the stroke");
        assert!(s.gpu_grab_setup().is_some());
        s.end(&mut gpu);
        assert_eq!(gpu.positions, cpu.positions);
        let b = Brush { kind: BrushKind::MoveTopological, radius: 0.4, strength: 0.8, ..Brush::default() }.at_pressure(Some(0.3), true, true);
        assert_eq!((b.radius, b.strength), (0.4, 0.8));
        let b = Brush { kind: BrushKind::ClayTubes, radius: 0.4, strength: 0.8, ..Brush::default() }.at_pressure(Some(0.5), true, true);
        assert!((b.radius - 0.2).abs() < 1e-6 && (b.strength - 0.4).abs() < 1e-6);
    }

    #[test]
    fn move_topological_leaves_a_separate_surface_inside_the_brush_still() {
        // Two parallel sheets 0.1 apart; the brush reaches both.
        let (p, i) = Primitive::Plane.build(40, 1.0);
        let n = p.len() as u32;
        let mut positions = p.clone();
        positions.extend(p.iter().map(|q| *q + Vec3::Z * 0.1));
        let mut indices = i.clone();
        indices.extend(i.iter().map(|k| k + n));
        let run = |kind: BrushKind| {
            let mut m = SculptMesh::from_raw(positions.clone(), indices.clone());
            let before = m.positions.clone();
            let b = Brush { kind, radius: 0.4, strength: 1.0, ..Brush::default() };
            let mut s = Stroke::begin(false, false, false);
            let hit = Vec3::new(0.0, 0.0, 0.1);
            s.dab(&mut m, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::Z * 4.0, -Vec3::Z));
            s.dab(&mut m, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::new(0.2, 0.0, 4.0), -Vec3::Z));
            s.end(&mut m);
            let moved = |range: std::ops::Range<usize>| range.filter(|&v| m.positions[v] != before[v]).count();
            (moved(0..n as usize), moved(n as usize..2 * n as usize))
        };
        let (lower, upper) = run(BrushKind::MoveTopological);
        assert!(upper > 10 && lower == 0, "Move Topological moved {upper} on the clicked sheet, {lower} on the other");
        let (lower, upper) = run(BrushKind::Grab);
        assert!(upper > 10 && lower > 10, "Grab moves both sheets");
    }

    /// A plane with one groove along y = -0.15 and one ridge along y = 0.15.
    fn ridged_plane() -> SculptMesh {
        let (p, i) = Primitive::Plane.build(80, 1.0);
        let p = p
            .into_iter()
            .map(|q| q + Vec3::Z * (0.04 * (-((q.y - 0.15) / 0.04).powi(2)).exp() - 0.04 * (-((q.y + 0.15) / 0.04).powi(2)).exp()))
            .collect();
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn smooth_valleys_and_peaks_smooth_only_their_side() {
        let groove = Vec3::new(0.0, -0.15, -0.04);
        let ridge = Vec3::new(0.0, 0.15, 0.04);
        for kind in [BrushKind::SmoothValleys, BrushKind::SmoothPeaks, BrushKind::Smooth] {
            let mut m = ridged_plane();
            let (g, r) = (nearest(&m, groove), nearest(&m, ridge));
            let (gz, rz) = (m.positions[g].z, m.positions[r].z);
            let b = Brush { kind, radius: 0.5, strength: 1.0, ..Brush::default() };
            let mut s = Stroke::begin(false, false, false);
            for k in 0..6 {
                let hit = Vec3::new(-0.1 + 0.04 * k as f32, 0.0, 0.0);
                s.dab(&mut m, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::Z * 4.0, -Vec3::Z));
            }
            let (filled, flattened) = (m.positions[g].z - gz, rz - m.positions[r].z);
            match kind {
                BrushKind::SmoothValleys => assert!(filled > 1e-3 && flattened.abs() < 1e-4, "{filled} {flattened}"),
                BrushKind::SmoothPeaks => assert!(flattened > 1e-3 && filled.abs() < 1e-4, "{filled} {flattened}"),
                _ => assert!(filled > 1e-3 && flattened > 1e-3),
            }
        }
    }

    #[test]
    fn planar_keeps_the_plane_of_its_first_dab() {
        // Along a sphere, Flatten refits a lower, tilted plane each dab; Planar keeps the
        // plane at the top, so the far end of its stroke is lifted toward it.
        let end = |kind: BrushKind| {
            let mut m = sphere(40);
            let far = nearest(&m, Vec3::new(0.32, 0.0, 0.95).normalize());
            let z = m.positions[far].z;
            stroke(&mut m, kind, false, false, false, Vec3::Z).end(&mut m);
            m.positions[far].z - z
        };
        let (planar, flatten) = (end(BrushKind::Planar), end(BrushKind::Flatten));
        assert!(planar > flatten + 1e-3, "Planar {planar} vs Flatten {flatten}");
    }

    #[test]
    fn trims_only_cut_and_magnify_spreads_where_pinch_gathers() {
        for kind in [BrushKind::TrimAdaptive, BrushKind::TrimDynamic] {
            let mut m = sphere(40);
            let before = m.positions.clone();
            stroke(&mut m, kind, false, false, false, Vec3::Z).end(&mut m);
            let dz: Vec<f32> = m.positions.iter().zip(&before).map(|(a, b)| (*a - *b).dot(b.normalize())).collect();
            assert!(dz.iter().all(|&d| d <= 1e-6) && dz.iter().any(|&d| d < -1e-4), "{kind:?}");
        }
        let mut a = sphere(24);
        let mut b = sphere(24);
        let i = nearest(&a, Vec3::new(0.18, 0.0, 0.98));
        let r = a.positions[i].truncate().length();
        for (m, kind) in [(&mut a, BrushKind::Magnify), (&mut b, BrushKind::Pinch)] {
            let mut s = Stroke::begin(false, false, false);
            s.dab(m, &Brush { kind, radius: 0.4, strength: 1.0, ..Brush::default() }, Vec3::Z, Vec3::Z, -Vec3::Z, (Vec3::Z * 5.0, -Vec3::Z));
        }
        assert!(a.positions[i].truncate().length() > r && b.positions[i].truncate().length() < r);
    }

    #[test]
    fn square_brushes_reach_the_corners_of_their_square() {
        let r = 1.0;
        // Along +X: a corner of the square is fully on, where a round falloff has faded.
        let corner = Vec3::new(0.45, 0.45, 0.0);
        assert_eq!(square_footprint(corner, Vec3::ZERO, Vec3::Z, Vec3::X, r, true), 1.0);
        // A strip fades front to back but keeps its hard sides.
        let side = Vec3::new(0.0, 0.5, 0.0);
        assert_eq!(square_footprint(side, Vec3::ZERO, Vec3::Z, Vec3::X, r, false), 1.0);
        assert!(square_footprint(Vec3::new(0.5, 0.0, 0.0), Vec3::ZERO, Vec3::Z, Vec3::X, r, false) < 0.5);
        assert!(falloff(corner.length(), r) < 0.4);
        // Turned 45 degrees, a point near the corner is past the square's edge.
        let diagonal = Vec3::new(1.0, 1.0, 0.0).normalize();
        let near_corner = Vec3::new(0.6, 0.6, 0.0);
        assert!(square_footprint(near_corner, Vec3::ZERO, Vec3::Z, Vec3::X, r, true) > 0.5);
        assert_eq!(square_footprint(near_corner, Vec3::ZERO, Vec3::Z, diagonal, r, true), 0.0);
        // A ClayTubes stroke follows its direction: a dab after moving along +Y turns the
        // square with it.
        let mut s = Stroke::begin(false, false, false);
        let mut m = Primitive::Plane.build(60, 1.0);
        let mut mesh = SculptMesh::from_raw(std::mem::take(&mut m.0), std::mem::take(&mut m.1));
        let b = Brush { kind: BrushKind::ClayTubes, radius: 0.3, strength: 1.0, ..Brush::default() };
        for y in [0.0, 0.1] {
            let hit = Vec3::new(0.0, y, 0.0);
            s.dab(&mut mesh, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::Z * 4.0, -Vec3::Z));
        }
        assert!(s.direction.unwrap().distance(Vec3::Y) < 1e-5);
    }

    #[test]
    fn orb_cracks_cut_narrower_and_more_unevenly_than_dam_standard() {
        let profile = |kind: BrushKind| {
            let (p, i) = Primitive::Plane.build(120, 1.0);
            let mut m = SculptMesh::from_raw(p, i);
            let b = Brush { kind, radius: 0.4, strength: 1.0, ..Brush::default() };
            let mut s = Stroke::begin(false, false, false);
            for k in 0..12 {
                let hit = Vec3::new(-0.3 + 0.06 * k as f32, 0.0, 0.0);
                s.dab(&mut m, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::Z * 4.0, -Vec3::Z));
            }
            let depth = |x: f32, y: f32| -m.positions[nearest(&m, Vec3::new(x, y, 0.0))].z;
            let along: Vec<f32> = (0..8).map(|k| depth(-0.1 + 0.03 * k as f32, 0.0)).collect();
            let mean = along.iter().sum::<f32>() / along.len() as f32;
            assert!(mean > 0.005, "{kind:?} cuts into the surface: {mean}");
            let spread = along.iter().map(|d| (d - mean).abs()).fold(0.0, f32::max) / mean;
            (depth(0.05, 0.08) / depth(0.05, 0.0), spread)
        };
        let (orb_width, orb_spread) = profile(BrushKind::OrbCracks);
        let (dam_width, dam_spread) = profile(BrushKind::DamStandard);
        assert!(orb_width < dam_width, "Orb Cracks {orb_width} vs Dam Standard {dam_width}");
        assert!(orb_spread > dam_spread + 0.05, "Orb Cracks depth varies: {orb_spread} vs {dam_spread}");
    }
}

#[cfg(test)]
mod auto_mask_tests {
    use super::*;
    use crate::primitive::Primitive;

    /// Two sheets `gap` apart, both inside a brush at the top one: the top faces +Z
    /// (towards a viewer looking down -Z), the lower one faces +Z too (`flip` false) or -Z.
    fn sheets(gap: f32, flip: bool) -> (SculptMesh, usize) {
        let (p, i) = Primitive::Plane.build(40, 1.0);
        let n = p.len();
        let mut positions = p.clone();
        positions.extend(p.iter().map(|q| *q - Vec3::Z * gap));
        let mut indices = i.clone();
        for t in i.chunks(3) {
            let t = [t[0] + n as u32, t[1] + n as u32, t[2] + n as u32];
            indices.extend(if flip { [t[0], t[2], t[1]] } else { t });
        }
        (SculptMesh::from_raw(positions, indices), n)
    }

    /// Vertices moved in the top sheet and in the lower one after a stroke of `kind`.
    fn moved(kind: BrushKind, auto: AutoMask, gap: f32, flip: bool) -> (usize, usize) {
        let (mut m, n) = sheets(gap, flip);
        let before = m.positions.clone();
        let b = Brush { kind, radius: 0.4, strength: 1.0, ..Brush::default() };
        let mut s = Stroke::begin(false, false, false).with_assets(StampSettings { auto, ..StampSettings::default() });
        for k in 0..3 {
            let hit = Vec3::new(0.05 * k as f32, 0.0, 0.0);
            s.dab(&mut m, &b, hit, Vec3::Z, -Vec3::Z, (hit + Vec3::new(0.1 * k as f32, 0.0, 4.0), -Vec3::Z));
        }
        s.end(&mut m);
        let count = |r: std::ops::Range<usize>| r.filter(|&v| m.positions[v] != before[v]).count();
        (count(0..n), count(n..2 * n))
    }

    #[test]
    fn backface_mask_leaves_the_far_side_of_a_thin_part_alone() {
        // A thin slab: the lower sheet faces away from the viewer.
        for kind in [BrushKind::Draw, BrushKind::Inflate, BrushKind::Grab, BrushKind::Zipper] {
            let (top, far) = moved(kind, AutoMask::default(), 0.05, true);
            assert!(top > 10 && far > 10, "{kind:?} without BackfaceMask reaches through: {top} {far}");
            let (top, far) = moved(kind, AutoMask { backface: true, ..AutoMask::default() }, 0.05, true);
            assert!(top > 10 && far == 0, "{kind:?} with BackfaceMask: {top} {far}");
        }
    }

    #[test]
    fn topological_masking_keeps_to_the_surface_under_the_brush() {
        for kind in [BrushKind::Draw, BrushKind::ClayBuildup, BrushKind::Grab, BrushKind::Paint] {
            let topological = AutoMask { topological: true, ..AutoMask::default() };
            if kind == BrushKind::Paint {
                // Paint changes colour, not positions: check the colours instead.
                let (mut m, n) = sheets(0.1, false);
                let b = Brush { kind, radius: 0.4, strength: 1.0, paint: true, sculpt: false, ..Brush::default() };
                let mut s = Stroke::begin(false, false, false).with_assets(StampSettings { auto: topological, ..StampSettings::default() });
                s.dab(&mut m, &b, Vec3::ZERO, Vec3::Z, -Vec3::Z, (Vec3::Z * 4.0, -Vec3::Z));
                assert!(m.colors[..n].iter().any(|c| c[3] > 0));
                assert!(m.colors[n..].iter().all(|c| c[3] == 0), "paint stays on the touched sheet");
                continue;
            }
            let (top, other) = moved(kind, AutoMask::default(), 0.1, false);
            assert!(top > 10 && other > 10, "{kind:?} reaches both sheets: {top} {other}");
            let (top, other) = moved(kind, topological, 0.1, false);
            assert!(top > 10 && other == 0, "{kind:?} with Topological: {top} {other}");
        }
    }
}

#[cfg(test)]
mod symmetry_group_tests {
    use super::*;
    use crate::symmetry::{Axis, Symmetry};

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(40, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn nearest(m: &SculptMesh, p: Vec3) -> usize {
        (0..m.vertex_count()).min_by(|&a, &b| m.positions[a].distance_squared(p).total_cmp(&m.positions[b].distance_squared(p))).unwrap()
    }

    /// One Draw dab at `hit` under `sym`: every image of the probe moves like the image of
    /// the probe's move.
    fn check_draw(sym: Symmetry, hit: Vec3) {
        let mut m = sphere();
        let before = m.positions.clone();
        let b = Brush { kind: BrushKind::Draw, radius: 0.25, strength: 1.0, ..Brush::default() };
        let mut s = Stroke::begin(false, false, sym);
        s.dab(&mut m, &b, hit, hit, -Vec3::Z, (hit * 3.0, -hit));
        let images = sym.images();
        let a = nearest(&m, hit);
        let da = m.positions[a] - before[a];
        assert!(da.length() > 1e-3, "{sym:?}: the brush itself moved nothing");
        for k in 1..images.len() {
            let b = nearest(&m, images.map(k, hit));
            let db = m.positions[b] - before[b];
            let expect = images.map(k, da);
            assert!((db - expect).length() < da.length() * 0.25, "{sym:?} image {k}: {db:?} vs {expect:?}");
        }
    }

    #[test]
    fn draw_follows_every_mirror_and_radial_copy() {
        let hit = Vec3::new(0.45, 0.35, 0.82).normalize();
        check_draw(Symmetry { y: true, ..Default::default() }, hit);
        check_draw(Symmetry { z: true, ..Default::default() }, Vec3::new(0.3, 0.2, 0.93).normalize());
        check_draw(Symmetry { x: true, y: true, z: true, radial: None }, hit);
        check_draw(Symmetry { radial: Some((Axis::Z, 6)), ..Default::default() }, Vec3::new(0.6, 0.1, 0.79).normalize());
        check_draw(Symmetry { x: true, radial: Some((Axis::Y, 5)), ..Default::default() }, Vec3::new(0.3, 0.5, 0.81).normalize());
    }

    #[test]
    fn a_gpu_grab_with_radial_symmetry_ends_where_the_cpu_grab_does() {
        let sym = Symmetry { y: true, radial: Some((Axis::Z, 6)), ..Default::default() };
        let run = |gpu: bool| {
            let mut m = sphere();
            // Between the copies of the dihedral group: none overlap.
            let b = Brush { kind: BrushKind::Grab, radius: 0.15, strength: 1.0, ..Brush::default() };
            let mut s = Stroke::begin(false, gpu, sym);
            let a = 15f32.to_radians();
            let hit = Vec3::new(0.63 * a.cos(), 0.63 * a.sin(), 0.777).normalize();
            s.dab(&mut m, &b, hit, hit, -Vec3::Z, (hit + Vec3::Z * 5.0, -Vec3::Z));
            s.dab(&mut m, &b, hit, hit, -Vec3::Z, (hit + Vec3::new(0.1, 0.05, 5.0), -Vec3::Z));
            if gpu {
                let setup = s.gpu_grab_setup().unwrap();
                assert_eq!(setup.images.len(), setup.verts.len());
                assert!(setup.images.iter().any(|&k| k > 0));
                assert_eq!(s.take_gpu_delta().unwrap().len(), sym.images().len());
            }
            s.end(&mut m);
            m.positions
        };
        assert_eq!(run(true), run(false));
    }

    #[test]
    fn overlapping_copies_add_up_and_keep_the_surface_whole() {
        // A big Grab on the symmetry plane pulling sideways: the copies overlap, their
        // moves add up (on the CPU), and vertices on the plane do not move across it.
        let mut m = sphere();
        let before = m.positions.clone();
        let b = Brush { kind: BrushKind::Grab, radius: 0.6, strength: 1.0, ..Brush::default() };
        let mut s = Stroke::begin(false, true, true);
        let hit = Vec3::Z;
        s.dab(&mut m, &b, hit, hit, -Vec3::Z, (hit + Vec3::Z * 5.0, -Vec3::Z));
        assert!(s.gpu_grab_setup().is_none(), "overlapping copies run on the CPU");
        s.dab(&mut m, &b, hit, hit, -Vec3::Z, (hit + Vec3::new(0.2, 0.15, 5.0), -Vec3::Z));
        s.end(&mut m);
        for (p, q) in m.positions.iter().zip(&before) {
            if q.x.abs() < 1e-6 && q.distance(hit) < 0.6 {
                assert!(p.x.abs() < 1e-5, "on the plane, sideways moves cancel: {q:?} -> {p:?}");
            }
        }
        // Both sides moved up (the Y part adds), mirror images of each other.
        let (a, c) = (nearest(&m, Vec3::new(0.2, 0.0, 0.98)), nearest(&m, Vec3::new(-0.2, 0.0, 0.98)));
        let (da, dc) = (m.positions[a] - before[a], m.positions[c] - before[c]);
        assert!(da.y > 0.05 && (da.y - dc.y).abs() < 1e-4 && (da.x + dc.x).abs() < 1e-4, "{da:?} {dc:?}");
        // Neighbours across the plane stay close: no tear.
        let worst = (0..m.vertex_count() as u32)
            .flat_map(|v| m.neighbors(v).iter().map(move |&n| (v, n)))
            .map(|(v, n)| m.positions[v as usize].distance(m.positions[n as usize]) / before[v as usize].distance(before[n as usize]))
            .fold(0.0f32, f32::max);
        assert!(worst < 4.0, "edges stretch at most {worst}×");
    }

    #[test]
    fn masks_and_mask_shapes_follow_every_copy() {
        use crate::mask::MaskStroke;
        let sym = Symmetry { radial: Some((Axis::Z, 4)), ..Default::default() };
        let mut m = sphere();
        let b = Brush { kind: BrushKind::MaskPen, radius: 0.2, strength: 1.0, ..Brush::default() };
        let hit = Vec3::new(0.6, 0.1, 0.79).normalize();
        let mut s = MaskStroke::begin(false, sym);
        s.dab(&mut m, &b, hit);
        for q in sym.images().of(hit) {
            assert!(m.mask[nearest(&m, q)] > 200, "masked at {q:?}");
        }
    }
}
