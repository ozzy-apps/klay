//! Brushes, strokes and undo. Every per-vertex pass is parallel; the mesh is only touched
//! through `Selection`-based bulk operations.

use crate::mesh::{SculptMesh, Selection};
use crate::multires::{ChunkData, Level, Multires};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrushKind {
    Draw,
    Smooth,
    Grab,
    Flatten,
    Inflate,
}

impl BrushKind {
    pub const ALL: [BrushKind; 5] = [
        BrushKind::Draw,
        BrushKind::Smooth,
        BrushKind::Grab,
        BrushKind::Flatten,
        BrushKind::Inflate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BrushKind::Draw => "Draw",
            BrushKind::Smooth => "Smooth",
            BrushKind::Grab => "Grab",
            BrushKind::Flatten => "Flatten",
            BrushKind::Inflate => "Inflate",
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
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            kind: BrushKind::Draw,
            radius: 0.25,
            strength: 0.5,
        }
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
    /// Every level was mapped by `p -> center + scale (p - center) + translate`
    /// (Deformation: Offset, Size).
    Transform {
        center: Vec3,
        scale: f32,
        translate: Vec3,
    },
    /// Hidden triangles of every level before a visibility change (run-length coded).
    Visibility(Vec<Vec<u32>>),
    /// Polygroups of every level before a group change (run-length coded).
    Groups(Vec<crate::polygroup::GroupRuns>),
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
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(|s| match s {
                UndoStep::Stroke { chunks, .. } => chunks.iter().map(|(_, v)| v.len() * 12).sum::<usize>(),
                UndoStep::Divide(level) => level.as_ref().map_or(0, |l| l.bytes()),
                UndoStep::Transform { .. } => 0,
                UndoStep::Visibility(runs) => runs.iter().map(|r| r.len() * 4).sum(),
                UndoStep::Groups(runs) => runs.iter().map(|r| r.bytes()).sum(),
            })
            .sum()
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
        UndoStep::Transform {
            center,
            scale,
            translate,
        } => {
            doc.transform(center, scale, translate);
            // p = c + (p' - c - t) / s, written in the same form.
            UndoStep::Transform {
                center: center + translate,
                scale: 1.0 / scale,
                translate: -translate,
            }
        }
        UndoStep::Visibility(runs) => UndoStep::Visibility(doc.restore_visibility(runs)),
        UndoStep::Groups(runs) => UndoStep::Groups(doc.restore_groups(runs)),
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

impl Stroke {
    pub fn begin(invert: bool, gpu_grab: bool, symmetry: bool) -> Self {
        Self {
            invert,
            snapshots: Vec::new(),
            snapshotted: HashSet::new(),
            last_dab: None,
            grab: None,
            dabs: 0,
            last_count: 0,
            scratch: Vec::new(),
            weights: Vec::new(),
            gpu_grab,
            pending_delta: None,
            symmetry,
        }
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
            self.snapshots.extend(mesh.snapshot_chunks(&fresh));
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
        if brush.kind == BrushKind::Grab {
            self.grab_dab(mesh, brush, hit, view_dir, ray);
            return;
        }
        // Dab spacing so stroke strength does not depend on event rate.
        if let Some(last) = self.last_dab {
            if (hit - last).length() < brush.radius * 0.15 {
                return;
            }
        }
        self.last_dab = Some(hit);
        self.dabs += 1;

        let sel = self.select(mesh, hit, brush.radius);
        if sel.is_empty() {
            return;
        }
        self.last_count = sel.len();
        let chunks = sel.chunks();
        self.snapshot(mesh, &chunks);
        let sign = if self.invert { -1.0 } else { 1.0 };
        let r = brush.radius;
        sel.dist
            .par_iter()
            .map(|&d| falloff(d, r))
            .collect_into_vec(&mut self.weights);
        let w = &self.weights;
        let new = &mut self.scratch;

        match brush.kind {
            BrushKind::Draw => {
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
                let amount = brush.strength * r * 0.06 * sign;
                sel.par_map_into(new, |i, v| {
                    let n = if sel.side(i) > 0.0 { n } else { nm };
                    mesh.positions[v as usize] + n * (amount * w[i])
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
            BrushKind::Flatten => {
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
                let k = brush.strength * 0.5;
                sel.par_map_into(new, |i, v| {
                    let p = mesh.positions[v as usize];
                    let (c, n) = if sel.side(i) > 0.0 { (c, n) } else { (cm, nm) };
                    p - n * ((p - c).dot(n) * k * w[i])
                });
            }
            BrushKind::Grab => unreachable!(),
        }
        mesh.scatter_positions(&sel, new);
        mesh.finish_edit(&chunks);
    }

    fn grab_dab(&mut self, mesh: &mut SculptMesh, brush: &Brush, hit: Vec3, view_dir: Vec3, ray: (Vec3, Vec3)) {
        if self.grab.is_none() {
            let sel = self.select(mesh, hit, brush.radius);
            let chunks = sel.chunks();
            self.snapshot(mesh, &chunks);
            let r = brush.radius;
            // Signed by side: the mirrored half moves with a mirrored delta.
            let weights = sel
                .dist
                .par_iter()
                .enumerate()
                .map(|(i, &d)| falloff(d, r) * sel.side(i))
                .collect();
            let orig = sel.par_map(|_, v| mesh.positions[v as usize]);
            self.last_count = sel.len();
            self.grab = Some(GrabState {
                sel,
                chunks,
                weights,
                orig,
                anchor: hit,
                gpu: self.gpu_grab,
                delta: Vec3::ZERO,
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
        let delta = (target - g.anchor) * brush.strength.max(0.05) * 2.0;
        g.delta = delta;
        self.dabs += 1;
        if g.gpu {
            // Nothing to do on the CPU per frame: the GPU kernel moves the vertices.
            self.pending_delta = Some(delta);
            return;
        }
        // Fused compute + write: no temporary buffer, one parallel pass per chunk.
        mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + grab_offset(delta, g.weights[i]));
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
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0 };
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
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0 };
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
        let brush = Brush { kind: BrushKind::Grab, radius: 0.3, strength: 1.0 };
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
