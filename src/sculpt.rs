//! Brushes, strokes and undo. Every per-vertex pass is parallel; the mesh is only touched
//! through `Selection`-based bulk operations.

use crate::mesh::{SculptMesh, Selection};
use crate::multires::{ChunkData, Level, Multires};
use crate::mask::MaskData;
use crate::transform::Affine;
use crate::brush_asset::{Pattern, tangent};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
}

impl BrushKind {
    pub const ALL: [BrushKind; 33] = [
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
        Self::Stitch, Self::Rake,
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
        }
    }

    pub fn is_drag(self) -> bool { matches!(self,Self::Grab|Self::Elastic|Self::SnakeHook) }
    pub fn gpu_drag(self) -> bool { matches!(self,Self::Grab|Self::SnakeHook) }
    pub fn is_mask(self) -> bool { self.base_type()=="Mask" }
    pub fn is_shape(self) -> bool { matches!(self,Self::MaskRect|Self::MaskLasso|Self::MaskCircle|Self::MaskLine|Self::ClipLine) }
    pub fn is_detail(self) -> bool { matches!(self,Self::Zipper|Self::Stitch|Self::Rake) }

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
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit,
        }
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
        self.redo.clear();
        self.undo.push(step);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self, doc: &mut Multires) {
        if let Some(step) = self.undo.pop() {
            let inverse = apply_step(doc, step);
            self.redo.push(inverse);
        }
    }

    pub fn redo(&mut self, doc: &mut Multires) {
        if let Some(step) = self.redo.pop() {
            let inverse = apply_step(doc, step);
            self.undo.push(inverse);
        }
    }

    pub fn clear(&mut self) {
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
}

/// Data the renderer needs to run a grab on the GPU.
pub struct GpuGrabSetup<'a> {
    pub verts: &'a [u32],
    pub orig: &'a [Vec3],
    pub weights: &'a [f32],
    pub chunks: &'a [u32],
}

/// State of the stroke currently being painted.
pub struct Stroke {
    pub invert: bool,
    snapshots: Vec<(u32, Vec<Vec3>)>,
    snapshotted: HashSet<u32>,
    snapshot_index: HashMap<u32, usize>,
    layer_normal: Option<Vec3>,
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
    pending_delta: Option<Vec3>,
    /// Mirror every dab across the X = 0 plane.
    symmetry: bool,
    assets: StampSettings,
    paint_snapshots: crate::paint::Chunks,
    paint_snapshotted: HashSet<u32>,
}

#[derive(Clone, Debug)]
pub struct StampSettings {
    pub alpha: Pattern,
    pub texture: Pattern,
    pub texture_strength: f32,
    pub texture_scale: f32,
}

impl Default for StampSettings {
    fn default()->Self {Self {alpha:Pattern::Off,texture:Pattern::Off,texture_strength:0.5,texture_scale:2.0}}
}

/// Mirror image across the X = 0 plane.
fn mirror_x(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

/// Offset of a grabbed vertex: `ws` is the falloff weight, negative on the mirrored side
/// (same encoding as grab.wgsl).
fn grab_offset(delta: Vec3, ws: f32) -> Vec3 {
    Vec3::new(delta.x * ws, delta.y * ws.abs(), delta.z * ws.abs())
}

fn drag_offset(g: &GrabState, i: usize) -> Vec3 {
    let offset = grab_offset(g.delta, g.weights[i]);
    if g.kind != BrushKind::Elastic { return offset; }
    let side = g.sel.side(i);
    let anchor = if side < 0.0 { mirror_x(g.anchor) } else { g.anchor };
    let relative = (g.orig[i] - anchor) / g.radius;
    let direction = grab_offset(g.delta, side);
    // Directional coupling gives broad surrounding clay a softer, lateral response.
    offset + relative * (relative.dot(direction) * 0.35 * g.weights[i].abs())
}

impl Stroke {
    pub fn begin(invert: bool, gpu_grab: bool, symmetry: bool) -> Self {
        Self {
            invert,
            snapshots: Vec::new(),
            snapshotted: HashSet::new(),
            snapshot_index: HashMap::new(),
            layer_normal: None,
            detail_distance: 0.0,
            last_dab: None,
            grab: None,
            dabs: 0,
            last_count: 0,
            scratch: Vec::new(),
            weights: Vec::new(),
            gpu_grab,
            pending_delta: None,
            symmetry,
            assets: StampSettings::default(),
            paint_snapshots: Vec::new(),
            paint_snapshotted: HashSet::new(),
        }
    }

    pub fn with_assets(mut self,assets:StampSettings)->Self {self.assets=assets;self}
    pub fn take_paint(&mut self)->crate::paint::Chunks {std::mem::take(&mut self.paint_snapshots)}

    fn stamp_weight(&self,p:Vec3,hit:Vec3,normal:Vec3,r:f32,side:f32)->f32 {
        if self.assets.alpha==Pattern::Off && self.assets.texture==Pattern::Off {return 1.0;}
        let p=if side<0.0 {mirror_x(p)} else {p};
        let (u,v)=tangent(normal);
        let delta=p-hit;
        let uv=glam::Vec2::new(delta.dot(u),-delta.dot(v))/(2.0*r)+glam::Vec2::splat(0.5);
        let a=if self.assets.alpha==Pattern::Off {1.0}else{self.assets.alpha.alpha(uv)};
        let t=self.assets.texture.sample(uv*self.assets.texture_scale,true);
        let h=t.truncate().dot(Vec3::new(0.2126,0.7152,0.0722))*t.w;
        a*(1.0-self.assets.texture_strength+h*self.assets.texture_strength)
    }

    fn paint_selected(&mut self,mesh:&mut SculptMesh,sel:&Selection,brush:&Brush,hit:Vec3,normal:Vec3) {
        if !brush.paint||brush.rgb_intensity<=0.0{return;}
        let fresh:Vec<_>=sel.chunks().into_iter().filter(|c|self.paint_snapshotted.insert(*c)).collect();
        self.paint_snapshots.extend(crate::paint::snapshot(mesh,&fresh));
        let (u,v)=tangent(normal);let base=Vec3::from_array(brush.color.map(|x|x as f32/255.0));
        let values=sel.par_map(|i,vert| {
            let p=mesh.positions[vert as usize];let p=if sel.side(i)<0.0 {mirror_x(p)} else {p};
            let delta=p-hit;
            let uv=glam::Vec2::new(delta.dot(u),-delta.dot(v))/(2.0*brush.radius)+glam::Vec2::splat(0.5);
            let tex=self.assets.texture.sample(uv*self.assets.texture_scale,true);
            let rgb=base*tex.truncate();
            let weight=focal_falloff(sel.dist[i],brush.radius,brush.focal_shift)
                *if self.assets.alpha==Pattern::Off{1.0}else{self.assets.alpha.alpha(uv)}
                *crate::mask::freedom(&mesh.mask,vert)*tex.w*brush.rgb_intensity*0.35;
            crate::paint::blend(crate::paint::at(mesh,vert as usize),rgb.to_array(),weight)
        });
        crate::paint::scatter(mesh,sel,&values);
    }

    fn select(&self, mesh: &SculptMesh, hit: Vec3, radius: f32) -> Selection {
        if self.symmetry {
            mesh.select_sphere_mirror_x(hit, radius)
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
            chunks: &g.chunks,
        })
    }

    /// The delta the GPU should apply this frame, if the grab moved.
    pub fn take_gpu_delta(&mut self) -> Option<Vec3> {
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
            if (hit - last).length() < brush.radius * 0.15 {
                return;
            }
        }
        let previous_hit = self.last_dab;
        self.last_dab = Some(hit);
        self.dabs += 1;

        let sel = self.select(mesh, hit, brush.radius);
        if sel.is_empty() {
            return;
        }
        self.last_count = sel.len();
        let chunks = sel.chunks();
        if brush.sculpt && brush.kind!=BrushKind::Paint {self.snapshot(mesh, &chunks);}
        self.paint_selected(mesh,&sel,brush,hit,hit_normal);
        if !brush.sculpt || brush.kind==BrushKind::Paint {return;}
        let sign = if self.invert { -1.0 } else { 1.0 };
        let r = brush.radius;
        // Masked vertices (Ctrl) are held still.
        let mask = &mesh.mask;
        let mut weights=std::mem::take(&mut self.weights);
        sel.dist
            .par_iter()
            .zip(sel.verts.par_iter())
            .enumerate()
            .map(|(i,(&d, &v))| focal_falloff(d, r, brush.focal_shift)
                *self.stamp_weight(mesh.positions[v as usize],hit,hit_normal,r,sel.side(i))
                *crate::mask::freedom(mask, v))
            .collect_into_vec(&mut weights);
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
                let nm = mirror_x(n);
                let amount = brush.strength * r * match brush.kind {BrushKind::Blob=>0.13,BrushKind::FormSoft=>0.04,_=>0.06} * sign;
                sel.par_map_into(new, |i, v| {
                    let n = if sel.side(i) > 0.0 { n } else { nm };
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
                let n = *self.layer_normal.get_or_insert(hit_normal);
                let normal_mirror = mirror_x(n);
                let snapshots = &self.snapshots;
                let index = &self.snapshot_index;
                sel.par_map_into(new, |i,v| {
                    let c = mesh.vert_chunk[v as usize];
                    let old = snapshots[index[&c]].1[(v-mesh.chunks[c as usize].vert_start) as usize];
                    let n = if sel.side(i)>0.0 {n}else{normal_mirror};
                    let p = mesh.positions[v as usize];
                    let remaining = (r*0.14 - (p-old).dot(n)*sign).max(0.0);
                    p+n*(remaining.min(r*0.06)*brush.strength*w[i]*sign)
                });
            }
            BrushKind::Nudge => {
                let delta = (hit-previous_hit.unwrap_or(hit)).clamp_length_max(r*0.2);
                let direction = (delta-hit_normal*delta.dot(hit_normal))*brush.strength*0.65*sign;
                sel.par_map_into(new,|i,v| {
                    mesh.positions[v as usize]+if sel.side(i)>0.0 {direction}else{mirror_x(direction)}*w[i]
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
            BrushKind::Smooth => {
                let k = brush.strength * 0.5;
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
                    p.lerp(avg / nb.len() as f32, (k * w[i]).min(1.0))
                });
            }
            BrushKind::Flatten | BrushKind::TrimDynamic | BrushKind::Polish | BrushKind::Clay | BrushKind::ClayBuildup | BrushKind::Fill | BrushKind::Scrape | BrushKind::FormHard => {
                // Fit a plane through the weighted centre with the average normal (own side
                // only), pull towards it; the mirrored side uses the mirrored plane.
                let (c, n, wsum) = sel
                    .verts
                    .par_iter()
                    .enumerate()
                    .zip(w.par_iter())
                    .filter(|((i, _), _)| sel.side(*i) > 0.0)
                    .map(|((_, &v), &wi)| (mesh.positions[v as usize] * wi, mesh.normal_at(v) * wi, wi))
                    .reduce(
                        || (Vec3::ZERO, Vec3::ZERO, 0.0),
                        |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2),
                    );
                let c = c / wsum.max(1e-6);
                let n = n.normalize_or(hit_normal);
                let (cm, nm) = (mirror_x(c), mirror_x(n));
                let k = brush.strength * if matches!(brush.kind,BrushKind::Polish|BrushKind::Scrape) {0.7}else{0.5};
                sel.par_map_into(new, |i, v| {
                    let p = mesh.positions[v as usize];
                    let (c, n) = if sel.side(i) > 0.0 { (c, n) } else { (cm, nm) };
                    let d=(p-c).dot(n);
                    let amount=match brush.kind {
                        BrushKind::FormHard => (r*0.18-d*sign).max(0.0).min(r*0.12)*sign*brush.strength*w[i],
                        BrushKind::Clay | BrushKind::ClayBuildup => {
                            let buildup=if brush.kind==BrushKind::ClayBuildup {1.8}else{1.0};
                            ((r*0.12*sign-d)*sign).max(0.0).min(r*0.08*buildup)*sign*k*w[i]
                        },
                        BrushKind::TrimDynamic=>-((d*sign).max(0.0).min(r*0.2))*sign*k*w[i],
                        BrushKind::Fill=>(-d*sign).max(0.0).min(r*0.2)*sign*k*w[i],
                        BrushKind::Scrape=>-(d*sign+r*0.035).max(0.0).min(r*0.2)*sign*k*w[i],
                        _=>-d*k*w[i],
                    };
                    p+n*amount
                });
            }
            BrushKind::DamStandard | BrushKind::Pinch | BrushKind::Crease => {
                sel.par_map_into(new,|i,v| {
                    let p=mesh.positions[v as usize];
                    let (c,n)=if sel.side(i)>0.0 {(hit,hit_normal)}else{(mirror_x(hit),mirror_x(hit_normal))};
                    let delta=c-p;let tangent=delta-n*delta.dot(n);
                    let tight=(-((sel.dist[i]/r)/0.24).powi(2)).exp();
                    let (pinch,cut)=match brush.kind {
                        BrushKind::Pinch=>(0.28,0.0),
                        BrushKind::Crease=>(0.15,0.055),
                        _=>(0.06,0.1),
                    };
                    p+tangent*(pinch*brush.strength*w[i]*sign)
                        -n*(cut*r*brush.strength*w[i]*tight*sign)
                });
            }
            BrushKind::Paint=>unreachable!(),
            BrushKind::Grab | BrushKind::Elastic | BrushKind::SnakeHook => unreachable!(),
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
        self.paint_selected(mesh,&sel,brush,centre,normal);
        let phase=self.detail_distance;self.detail_distance+=length;
        let sign=if self.invert{-1.0}else{1.0};
        let next=sel.par_map(|i,v| {
            let actual=mesh.positions[v as usize];
            let p=if sel.side(i)<0.0 {mirror_x(actual)}else{actual};
            let offset=p-start;let along=offset.dot(direction);let across=offset.dot(sideways);
            if along < -r*0.04 || along > length+r*0.04 || across.abs()>r || offset.dot(normal).abs()>r*0.65 {return actual;}
            let weight=focal_falloff(across.abs(),r,brush.focal_shift)
                *self.stamp_weight(actual,centre,normal,r,sel.side(i))*crate::mask::freedom(&mesh.mask,v);
            if weight<=0.0 {return actual;}
            let height=crate::detail_brush::profile(brush.kind,across/r,(phase+along)/r)*r*0.12*brush.strength*sign*weight;
            let chunk=mesh.vert_chunk[v as usize];
            let old=self.snapshots[self.snapshot_index[&chunk]].1[(v-mesh.chunks[chunk as usize].vert_start) as usize];
            let n=if sel.side(i)<0.0 {mirror_x(normal)}else{normal};
            actual+n*(height-(actual-old).dot(n))
        });
        mesh.scatter_positions(&sel,&next);mesh.finish_edit(&chunks);
        self.last_count=sel.len();self.dabs+=1;
    }

    fn grab_dab(&mut self, mesh: &mut SculptMesh, brush: &Brush, hit: Vec3, view_dir: Vec3, ray: (Vec3, Vec3)) {
        if self.grab.is_none() {
            let sel = self.select(mesh, hit, brush.radius);
            self.paint_selected(mesh,&sel,brush,hit,-view_dir);
            let chunks = sel.chunks();
            self.snapshot(mesh, &chunks);
            let r = brush.radius;
            // Signed by side: the mirrored half moves with a mirrored delta.
            let mask = &mesh.mask;
            let weights = sel
                .dist
                .par_iter()
                .enumerate()
                .map(|(i, &d)| {
                    let f=focal_falloff(d,r,brush.focal_shift);
                    let profile=match brush.kind {BrushKind::SnakeHook=>f.powi(3),BrushKind::Elastic=>f.sqrt(),_=>f};
                    profile*self.stamp_weight(mesh.positions[sel.verts[i] as usize],hit,view_dir,r,sel.side(i))
                        *sel.side(i)*crate::mask::freedom(mask, sel.verts[i])
                })
                .collect();
            let orig = sel.par_map(|_, v| mesh.positions[v as usize]);
            self.last_count = sel.len();
            self.grab = Some(GrabState {
                sel,
                chunks,
                weights,
                orig,
                anchor: hit,
                gpu: self.gpu_grab && brush.kind.gpu_drag(),
                delta: Vec3::ZERO,
                kind: brush.kind,
                radius: r,
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
            self.pending_delta = Some(delta);
            return;
        }
        // Fused compute + write: no temporary buffer, one parallel pass per chunk.
        mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + drag_offset(g,i));
        mesh.finish_edit(&g.chunks);
    }

    /// Finish the stroke and return the positions it replaced, per chunk, if anything
    /// changed. A GPU grab is applied to the CPU mesh here, once.
    pub fn end(mut self, mesh: &mut SculptMesh) -> Option<ChunkData> {
        if let Some(g) = self.grab.take() {
            if g.gpu && g.delta != Vec3::ZERO {
                let delta = g.delta;
                mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + grab_offset(delta, g.weights[i]));
                mesh.finish_edit(&g.chunks);
            }
        }
        if self.snapshots.is_empty() { None } else { Some(self.snapshots) }
    }
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

    #[test]
    fn snake_hook_gpu_weights_match_cpu_and_elastic_uses_directional_deformation() {
        let mut cpu=mesh();let mut gpu=mesh();
        stroke_path(&mut cpu,BrushKind::SnakeHook,false,true).end(&mut cpu);
        let before=gpu.positions.clone();
        let s=stroke_path(&mut gpu,BrushKind::SnakeHook,true,true);
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
