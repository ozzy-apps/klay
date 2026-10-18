//! Brushes, strokes and undo. Every per-vertex pass is parallel; the mesh is only touched
//! through `Selection`-based bulk operations.

use crate::mesh::{SculptMesh, Selection};
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

/// One undo step: the owned vertex positions of every chunk a stroke modified.
pub struct UndoStep {
    pub chunks: Vec<(u32, Vec<Vec3>)>,
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

    pub fn undo(&mut self, mesh: &mut SculptMesh) {
        if let Some(step) = self.undo.pop() {
            let inverse = apply_step(mesh, step);
            self.redo.push(inverse);
        }
    }

    pub fn redo(&mut self, mesh: &mut SculptMesh) {
        if let Some(step) = self.redo.pop() {
            let inverse = apply_step(mesh, step);
            self.undo.push(inverse);
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Approximate memory held by history, in bytes.
    pub fn bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(|s| s.chunks.iter().map(|(_, v)| v.len() * 12).sum::<usize>())
            .sum()
    }
}

/// Restore chunk positions from a step and return the inverse step.
fn apply_step(mesh: &mut SculptMesh, mut step: UndoStep) -> UndoStep {
    step.chunks.sort_unstable_by_key(|(c, _)| *c);
    let chunk_ids: Vec<u32> = step.chunks.iter().map(|(c, _)| *c).collect();
    let inverse = mesh.snapshot_chunks(&chunk_ids);
    mesh.restore_chunks(&step.chunks);
    mesh.finish_edit(&chunk_ids);
    UndoStep { chunks: inverse }
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
}

impl Stroke {
    pub fn begin(invert: bool, gpu_grab: bool) -> Self {
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

        let sel = mesh.select_sphere(hit, brush.radius);
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
                // Push along the average normal under the brush: smoother than per-vertex normals.
                let n = sel
                    .verts
                    .par_iter()
                    .zip(w.par_iter())
                    .map(|(&v, &wi)| mesh.normal_at(v) * wi)
                    .reduce(|| Vec3::ZERO, |a, b| a + b)
                    .normalize_or(hit_normal);
                let amount = brush.strength * r * 0.06 * sign;
                sel.par_map_into(new, |i, v| mesh.positions[v as usize] + n * (amount * w[i]));
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
                // Fit a plane through the weighted centre with the average normal, pull towards it.
                let (c, n, wsum) = sel
                    .verts
                    .par_iter()
                    .zip(w.par_iter())
                    .map(|(&v, &wi)| (mesh.positions[v as usize] * wi, mesh.normal_at(v) * wi, wi))
                    .reduce(
                        || (Vec3::ZERO, Vec3::ZERO, 0.0),
                        |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2),
                    );
                let c = c / wsum.max(1e-6);
                let n = n.normalize_or(hit_normal);
                let k = brush.strength * 0.5;
                sel.par_map_into(new, |i, v| {
                    let p = mesh.positions[v as usize];
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
            let sel = mesh.select_sphere(hit, brush.radius);
            let chunks = sel.chunks();
            self.snapshot(mesh, &chunks);
            let r = brush.radius;
            let weights = sel.dist.par_iter().map(|&d| falloff(d, r)).collect();
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
        mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + delta * g.weights[i]);
        mesh.finish_edit(&g.chunks);
    }

    /// Finish the stroke and produce its undo step, if anything changed. A GPU grab is
    /// applied to the CPU mesh here, once.
    pub fn end(mut self, mesh: &mut SculptMesh) -> Option<UndoStep> {
        if let Some(g) = self.grab.take() {
            if g.gpu && g.delta != Vec3::ZERO {
                let delta = g.delta;
                mesh.scatter_positions_with(&g.sel, |i, _| g.orig[i] + delta * g.weights[i]);
                mesh.finish_edit(&g.chunks);
            }
        }
        if self.snapshots.is_empty() {
            None
        } else {
            Some(UndoStep {
                chunks: self.snapshots,
            })
        }
    }
}

/// Smooth falloff from 1 at the centre to 0 at the radius.
pub fn falloff(d: f32, r: f32) -> f32 {
    let x = (d / r).clamp(0.0, 1.0);
    let t = 1.0 - x * x;
    t * t
}
