//! ZRemesher (ZBrush): automatic retopology into clean quads that follow the form.
//!
//! The method is Instant Meshes (Jakob, Tarini, Panozzo, Sorkine-Hornung 2015), on a graph
//! of samples of the surface:
//!
//! 1. **Samples.** The surface is subdivided until its edges are short enough, then its
//!    vertices are gathered into cells a quarter of the quad size across (split by normal
//!    direction, so the two sides of a thin part stay apart); each cell is a sample, and
//!    samples whose vertices share an edge are neighbours. Coarser copies of the graph
//!    (cells twice as large each time) speed up the smoothing below.
//! 2. **Directions.** Every sample gets a direction in its tangent plane, meaning one of four
//!    (it and its quarter turns); the field is smoothed from the coarsest graph down, each
//!    sample turning toward the mean of its neighbours' best-matching quarter turns.
//!    Directions at polygroup borders and open borders are held along the border, so edge
//!    loops follow them (ZBrush: KeepGroups).
//! 3. **Lattice.** Every sample also gets a point of a square lattice of the quad size laid
//!    along its directions; these are smoothed the same way, so neighbouring lattices agree.
//! 4. **Quads.** Neighbouring samples whose lattice points coincide become one vertex; those
//!    one lattice step apart are joined by an edge. Walking the edges around each vertex in
//!    turn gives faces of three to eight sides, holes in a closed surface are closed, and
//!    every face is split into quads through its middle (the lattice is laid at twice the
//!    quad size for this), so the result is all quads. Vertices are moved onto the old
//!    surface and spaced evenly along it; faces take the polygroup under them.
//!
//! **Guide curves** (ZBrush: the ZRemesherGuide brush) steer the loops: samples along a
//! curve turn toward its direction and shift their lattice so a lattice line runs on it, both
//! by Curves Strength, and the smoothing carries that into the rest of the field. Vertices of
//! the loop that comes out on a curve are put on it.
//!
//! **Edge lines** are held the same way at full strength, and the loop on each is put
//! exactly on it, corners where lines meet included: creased edges (KeepCreases, and the
//! result is creased along them again) and edges sharper than [`EDGE_ANGLE`] (Detect
//! Edges, keeping only lines at least two quads long, so surface noise does not count).
//!
//! **FreezeBorder** keeps every open border exactly as it was, vertex for vertex, so the
//! result welds back to whatever it was cut from: a strip of quads (and triangles where
//! the counts differ) joins the old border to the new one.

use crate::{mesh::SculptMesh, symmetry::MirrorPlane};
use crate::fx::FxHashMap as HashMap;
use glam::Vec3;
use rayon::prelude::*;
use std::time::Instant;

/// ZRemesher settings (ZBrush: Geometry › ZRemesher).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Target polygon count, in thousands (ZBrush: Target Polygons Count).
    pub target_k: f32,
    /// Hold edge loops along polygroup borders (ZBrush: KeepGroups).
    pub keep_groups: bool,
    /// 0..=100: how much smaller quads get where the surface curves more (ZBrush:
    /// AdaptiveSize); 0 keeps them all one size.
    pub adaptive: f32,
    /// 0..=100: how strongly guide curves steer the loops (ZBrush: Curves Strength); 100
    /// holds the direction along a curve exactly.
    pub curves_strength: f32,
    /// Polypaint sets the density (ZBrush: Use Polypaint): red for more quads, blue for
    /// fewer, unpainted and white as they are.
    pub use_polypaint: bool,
    /// 1..=4: how many times more (red) or fewer (blue) quads polypaint asks for (ZBrush:
    /// ColorDensity).
    pub color_density: f32,
    /// Remesh the positive half and mirror it across this plane.
    pub mirror: Option<MirrorPlane>,
    /// Keep open borders exactly, vertex for vertex (ZBrush: FreezeBorder).
    pub freeze_border: bool,
    /// Edge loops along creased edges, creased again (ZBrush: KeepCreases).
    pub keep_creases: bool,
    /// Start the direction field a quarter turn (45°) round, for a different but equally
    /// valid layout (ZBrush: Alt + ZRemesher runs its other algorithm). Set per click,
    /// not a saved setting.
    pub alternate: bool,
    /// Edge loops along sharp edges (ZBrush: Detect Edges).
    pub detect_edges: bool,
}

/// Detect Edges: how sharply the surface must turn at an edge, in degrees.
pub const EDGE_ANGLE: f32 = 40.0;

/// Lines ZRemesher puts an edge loop exactly on, and the points where they meet.
#[derive(Default)]
struct Features {
    lines: Vec<Vec<Vec3>>,
    corners: Vec<Vec3>,
}

impl Features {
    fn folded(&self, plane: MirrorPlane) -> Self {
        let f = |p: &Vec3| plane.fold(*p);
        Self { lines: self.lines.iter().map(|l| l.iter().map(f).collect()).collect(), corners: self.corners.iter().map(f).collect() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { target_k: 5.0, keep_groups: true, adaptive: 50.0, curves_strength: 50.0, use_polypaint: false, color_density: 2.0, mirror: None, freeze_border: false, keep_creases: true, detect_edges: false, alternate: false }
    }
}

#[cfg(test)]
const TEST_X_PLANE: MirrorPlane = MirrorPlane { axis: crate::symmetry::Axis::X, at: 0.0 };

/// The density polypaint asks for at each vertex, as the log of the factor on the quad count:
/// red up to `ln(color_density)`, blue down to minus that, unpainted and grey nothing.
fn paint_density(mesh: &SculptMesh, color_density: f32) -> Vec<f32> {
    let k = color_density.clamp(1.0, 4.0).ln();
    if mesh.colors.len() != mesh.vertex_count() || k <= 0.0 {
        return Vec::new();
    }
    mesh.colors
        .iter()
        .map(|c| {
            if c[3] == 0 {
                return 0.0;
            }
            let [r, g, b] = [c[0], c[1], c[2]].map(|x| x as f32 / 255.0);
            let red = (r - g.max(b)).max(0.0);
            let blue = (b - r.max(g)).max(0.0);
            k * (red - blue).clamp(-1.0, 1.0)
        })
        .collect()
}

/// Guide curves as segments, by grid cell, for the nearest point on any of them.
struct Guides {
    segs: Vec<(Vec3, Vec3)>,
    cell: f32,
    grid: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl Guides {
    fn new(curves: &[Vec<Vec3>], cell: f32) -> Self {
        let cell = cell.max(1e-6);
        let mut segs = Vec::new();
        let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
        for c in curves {
            for w in c.windows(2) {
                let (a, b) = (w[0], w[1]);
                if a.distance_squared(b) <= 0.0 {
                    continue;
                }
                let k = segs.len() as u32;
                segs.push((a, b));
                // Every cell the segment passes through (sampled at half a cell).
                let n = (a.distance(b) / (cell * 0.5)).ceil().max(1.0) as usize;
                let mut last = None;
                for i in 0..=n {
                    let c = (a.lerp(b, i as f32 / n as f32) / cell).floor();
                    let key = (c.x as i32, c.y as i32, c.z as i32);
                    if last != Some(key) {
                        let list = grid.entry(key).or_default();
                        if list.last() != Some(&k) {
                            list.push(k);
                        }
                        last = Some(key);
                    }
                }
            }
        }
        Self { segs, cell, grid }
    }

    fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The nearest point on a guide within `reach` of `p`: its distance, the point and the
    /// guide's direction there.
    fn nearest(&self, p: Vec3, reach: f32) -> Option<(f32, Vec3, Vec3)> {
        let mut best: Option<(f32, Vec3, Vec3)> = None;
        let mut try_seg = |&(a, b): &(Vec3, Vec3)| {
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0);
            let q = a + ab * t;
            let d = q.distance(p);
            if d <= reach && best.is_none_or(|x| d < x.0) {
                best = Some((d, q, ab.normalize()));
            }
        };
        let k = (reach / self.cell).ceil() as i32;
        if k > 3 {
            self.segs.iter().for_each(&mut try_seg);
        } else {
            let c = (p / self.cell).floor();
            for dx in -k..=k {
                for dy in -k..=k {
                    for dz in -k..=k {
                        for &s in self.grid.get(&(c.x as i32 + dx, c.y as i32 + dy, c.z as i32 + dz)).into_iter().flatten() {
                            try_seg(&self.segs[s as usize]);
                        }
                    }
                }
            }
        }
        best
    }
}

/// Where a sample is pulled by a guide: the guide's direction in its tangent plane, the
/// nearest point on the guide, and how strongly (0 for not at all).
#[derive(Clone, Copy, Default)]
struct Pull {
    dir: Vec3,
    at: Vec3,
    w: f32,
}

/// The pull of `guides` on every sample of `g` within `reach` of one, with weight `w`.
fn pulls(g: &Graph, guides: &Guides, reach: f32, w: f32) -> Vec<Pull> {
    if guides.is_empty() || w <= 0.0 {
        return vec![Pull::default(); g.len()];
    }
    (0..g.len())
        .into_par_iter()
        .map(|i| {
            let n = g.n[i];
            guides
                .nearest(g.p[i], reach)
                .and_then(|(_, at, d)| {
                    let d = d - n * d.dot(n);
                    (d.length() > 0.3).then(|| Pull { dir: d.normalize(), at, w })
                })
                .unwrap_or_default()
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub samples: usize,
    pub levels: usize,
    pub quads: usize,
    /// Faces other than quads before the split, and holes closed.
    pub odd_faces: usize,
    pub holes: usize,
    /// Vertices put on a guide curve.
    pub on_guides: usize,
    pub ms: f32,
}

/// A graph of surface samples.
struct Graph {
    p: Vec<Vec3>,
    n: Vec<Vec3>,
    /// Neighbours, CSR.
    off: Vec<u32>,
    adj: Vec<u32>,
    /// A direction to hold (border tangent), or zero.
    held: Vec<Vec3>,
    /// The log of how many times more quads are wanted here (polypaint density).
    dens: Vec<f32>,
}

impl Graph {
    fn neighbours(&self, i: usize) -> &[u32] {
        &self.adj[self.off[i] as usize..self.off[i + 1] as usize]
    }

    fn len(&self) -> usize {
        self.p.len()
    }

    /// Sets of samples that are not neighbours of each other, to update in parallel.
    fn colours(&self) -> Vec<Vec<u32>> {
        let mut colour = vec![u8::MAX; self.len()];
        let mut sets: Vec<Vec<u32>> = Vec::new();
        for i in 0..self.len() {
            let mut used = 0u64;
            for &j in self.neighbours(i) {
                let c = colour[j as usize];
                if c < 64 {
                    used |= 1 << c;
                }
            }
            let c = (!used).trailing_zeros().min(63) as u8;
            colour[i] = c;
            if sets.len() <= c as usize {
                sets.resize(c as usize + 1, Vec::new());
            }
            sets[c as usize].push(i as u32);
        }
        sets
    }
}

/// Cell key: a grid cell and which of six directions the normal mostly faces.
fn cell_key(p: Vec3, n: Vec3, inv: f32) -> (i32, i32, i32, u8) {
    let c = (p * inv).floor();
    let a = n.abs();
    let axis = if a.x >= a.y && a.x >= a.z { 0 } else if a.y >= a.z { 1 } else { 2 };
    let side = (n[axis] < 0.0) as u8;
    (c.x as i32, c.y as i32, c.z as i32, axis as u8 * 2 + side)
}

/// Gather `g`'s samples into cells `cell` across: the coarser graph and each sample's cell.
fn coarsen(g: &Graph, cell: f32) -> (Graph, Vec<u32>) {
    let inv = 1.0 / cell;
    let mut index: HashMap<(i32, i32, i32, u8), u32> = HashMap::default();
    let mut parent = Vec::with_capacity(g.len());
    let (mut p, mut n, mut held, mut count): (Vec<Vec3>, Vec<Vec3>, Vec<Vec3>, Vec<f32>) = Default::default();
    let mut dens: Vec<f32> = Vec::new();
    for i in 0..g.len() {
        let key = cell_key(g.p[i], g.n[i], inv);
        let k = *index.entry(key).or_insert_with(|| {
            p.push(Vec3::ZERO);
            n.push(Vec3::ZERO);
            held.push(Vec3::ZERO);
            count.push(0.0);
            dens.push(0.0);
            (p.len() - 1) as u32
        });
        parent.push(k);
        let k = k as usize;
        dens[k] += g.dens[i];
        p[k] += g.p[i];
        n[k] += g.n[i];
        count[k] += 1.0;
        if held[k] == Vec3::ZERO {
            held[k] = g.held[i];
        }
    }
    for k in 0..p.len() {
        p[k] /= count[k];
        dens[k] /= count[k];
        n[k] = n[k].normalize_or(Vec3::Z);
        if held[k] != Vec3::ZERO {
            held[k] = (held[k] - n[k] * held[k].dot(n[k])).normalize_or_zero();
        }
    }
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for i in 0..g.len() {
        for &j in g.neighbours(i) {
            let (a, b) = (parent[i], parent[j as usize]);
            if a != b {
                edges.push((a, b));
            }
        }
    }
    let graph = graph_from(p, n, held, dens, edges);
    (graph, parent)
}

fn graph_from(p: Vec<Vec3>, n: Vec<Vec3>, held: Vec<Vec3>, dens: Vec<f32>, mut edges: Vec<(u32, u32)>) -> Graph {
    let m = p.len();
    edges.extend(edges.clone().into_iter().map(|(a, b)| (b, a)));
    edges.par_sort_unstable();
    edges.dedup();
    let mut off = vec![0u32; m + 1];
    for &(a, _) in &edges {
        off[a as usize + 1] += 1;
    }
    for i in 0..m {
        off[i + 1] += off[i];
    }
    let adj = edges.into_iter().map(|e| e.1).collect();
    Graph { p, n, off, adj, held, dens }
}

/// The sample graph of `mesh`'s vertices: one sample per vertex, neighbours along edges.
/// Directions are held along open borders, and with `groups` along polygroup borders.
fn vertex_graph(positions: &[Vec3], indices: &[u32], groups: &[u16], dens: &[f32]) -> Graph {
    let m = positions.len();
    let mut n = vec![Vec3::ZERO; m];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
        let f = (b - a).cross(c - a);
        for &v in t {
            n[v as usize] += f;
        }
    }
    let n: Vec<Vec3> = n.into_iter().map(|x| x.normalize_or(Vec3::Z)).collect();
    // Edges, with how many triangles and groups each has.
    let mut faces: HashMap<(u32, u32), (u32, u16, bool)> = HashMap::default();
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let g = groups.get(t).copied().unwrap_or(0);
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let e = faces.entry((a.min(b), a.max(b))).or_insert((0, g, false));
            e.0 += 1;
            if e.1 != g {
                e.2 = true;
            }
        }
    }
    let mut held = vec![Vec3::ZERO; m];
    let mut edges = Vec::with_capacity(faces.len());
    for (&(a, b), &(count, _, split)) in &faces {
        edges.push((a, b));
        if count == 1 || split {
            let d = positions[b as usize] - positions[a as usize];
            for v in [a, b] {
                if held[v as usize] == Vec3::ZERO {
                    held[v as usize] = d;
                }
            }
        }
    }
    for v in 0..m {
        if held[v] != Vec3::ZERO {
            held[v] = (held[v] - n[v] * held[v].dot(n[v])).normalize_or_zero();
        }
    }
    let dens = if dens.len() == m { dens.to_vec() } else { vec![0.0; m] };
    graph_from(positions.to_vec(), n, held, dens, edges)
}

/// The pair of (one of four turns of `q0`, one of four turns of `q1`) that agree best.
fn compat_orientation(q0: Vec3, n0: Vec3, q1: Vec3, n1: Vec3) -> (Vec3, Vec3) {
    let a = [q0, n0.cross(q0)];
    let b = [q1, n1.cross(q1)];
    let (mut best, mut bi, mut bj) = (-1.0f32, 0, 0);
    for (i, x) in a.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            let s = x.dot(*y).abs();
            if s > best {
                best = s;
                bi = i;
                bj = j;
            }
        }
    }
    let s = a[bi].dot(b[bj]);
    (a[bi], b[bj] * s.signum())
}

/// The point between two samples, on both tangent planes as nearly as can be.
fn middle_point(p0: Vec3, n0: Vec3, p1: Vec3, n1: Vec3) -> Vec3 {
    let (n0p0, n0p1, n1p0, n1p1, n0n1) = (n0.dot(p0), n0.dot(p1), n1.dot(p0), n1.dot(p1), n0.dot(n1));
    let denom = 1.0 / (1.0 - n0n1 * n0n1 + 1e-4);
    let l0 = 2.0 * (n0p1 - n0p0 - n0n1 * (n1p0 - n1p1)) * denom;
    let l1 = 2.0 * (n1p0 - n1p1 - n0n1 * (n0p1 - n0p0)) * denom;
    0.5 * (p0 + p1) - 0.25 * (n0 * l0 + n1 * l1)
}

/// The lattice point (through `o`, along `q` and `n × q`, step `scale`) below `p`.
fn floor4(o: Vec3, q: Vec3, n: Vec3, p: Vec3, scale: f32) -> Vec3 {
    let t = n.cross(q);
    let d = p - o;
    o + q * (q.dot(d) / scale).floor() * scale + t * (t.dot(d) / scale).floor() * scale
}

/// The lattice point nearest `p`.
fn round4(o: Vec3, q: Vec3, n: Vec3, p: Vec3, scale: f32) -> Vec3 {
    let t = n.cross(q);
    let d = p - o;
    o + q * (q.dot(d) / scale).round() * scale + t * (t.dot(d) / scale).round() * scale
}

/// The pair of lattice points of two samples (lattices through `o0` and `o1`) that are
/// nearest each other, near the point between them.
#[allow(clippy::too_many_arguments)]
fn compat_position(p0: Vec3, n0: Vec3, q0: Vec3, o0: Vec3, p1: Vec3, n1: Vec3, q1: Vec3, o1: Vec3, s0: f32, s1: f32) -> (Vec3, Vec3) {
    let (t0, t1) = (n0.cross(q0), n1.cross(q1));
    let mid = middle_point(p0, n0, p1, n1);
    let a = floor4(o0, q0, n0, mid, s0);
    let b = floor4(o1, q1, n1, mid, s1);
    let (mut best, mut bi, mut bj) = (f32::INFINITY, 0, 0);
    for i in 0..4 {
        let x = a + (q0 * (i & 1) as f32 + t0 * ((i >> 1) & 1) as f32) * s0;
        for j in 0..4 {
            let y = b + (q1 * (j & 1) as f32 + t1 * ((j >> 1) & 1) as f32) * s1;
            let c = x.distance_squared(y);
            if c < best {
                best = c;
                bi = i;
                bj = j;
            }
        }
    }
    (
        a + (q0 * (bi & 1) as f32 + t0 * ((bi >> 1) & 1) as f32) * s0,
        b + (q1 * (bj & 1) as f32 + t1 * ((bj >> 1) & 1) as f32) * s1,
    )
}

/// Smooth the direction field `q` of `g`, `sweeps` times.
fn smooth_directions(g: &Graph, q: &mut [Vec3], colours: &[Vec<u32>], sweeps: usize, pull: &[Pull]) {
    for _ in 0..sweeps {
        for set in colours {
            let new: Vec<(u32, Vec3)> = set
                .par_iter()
                .map(|&i| {
                    let i = i as usize;
                    if g.held[i] != Vec3::ZERO {
                        return (i as u32, g.held[i]);
                    }
                    let n = g.n[i];
                    let mut sum = q[i];
                    let mut w = 0.0f32;
                    for &j in g.neighbours(i) {
                        let (a, b) = compat_orientation(sum, n, q[j as usize], g.n[j as usize]);
                        sum = a * w + b;
                        w += 1.0;
                        sum -= n * n.dot(sum);
                        sum = sum.normalize_or(q[i]);
                    }
                    // Along a guide: turned toward it.
                    let pl = pull[i];
                    if pl.w > 0.0 {
                        let (a, b) = compat_orientation(sum, n, pl.dir, n);
                        let v = a * (1.0 - pl.w) + b * pl.w;
                        sum = (v - n * n.dot(v)).normalize_or(b);
                    }
                    (i as u32, sum)
                })
                .collect();
            for (i, v) in new {
                q[i as usize] = v;
            }
        }
    }
}

/// Smooth the lattice points `o` of `g` (directions `q`, each sample's step in `scale`),
/// `sweeps` times.
fn smooth_positions(g: &Graph, q: &[Vec3], o: &mut [Vec3], colours: &[Vec<u32>], sweeps: usize, scale: &[f32], pull: &[Pull]) {
    for _ in 0..sweeps {
        for set in colours {
            let new: Vec<(u32, Vec3)> = set
                .par_iter()
                .map(|&i| {
                    let i = i as usize;
                    let (p, n) = (g.p[i], g.n[i]);
                    let mut sum = o[i];
                    let mut w = 0.0f32;
                    for &j in g.neighbours(i) {
                        let j = j as usize;
                        let (qi, qj) = compat_orientation(q[i], n, q[j], g.n[j]);
                        let (a, b) = compat_position(p, n, qi, sum, g.p[j], g.n[j], qj, o[j], scale[i], scale[j]);
                        sum = (a * w + b) / (w + 1.0);
                        w += 1.0;
                        sum -= n * n.dot(sum - p);
                    }
                    // Along a guide: the lattice line across it is shifted onto it.
                    let pl = pull[i];
                    if pl.w > 0.0 {
                        let t = n.cross(q[i]);
                        let axis = if q[i].dot(pl.dir).abs() >= t.dot(pl.dir).abs() { t } else { q[i] };
                        let off = axis.dot(sum - pl.at);
                        let r = off - (off / scale[i]).round() * scale[i];
                        sum -= axis * r * pl.w;
                    }
                    (i as u32, round4(sum, q[i], n, p, scale[i]))
                })
                .collect();
            for (i, v) in new {
                o[i as usize] = v;
            }
        }
    }
}

/// AdaptiveSize and polypaint density: each sample's lattice step, `scale` scaled down where
/// the surface curves more than usual and up where it is flatter (by up to two), by an amount
/// that grows with `adaptive` (0..=100), and by the density painted there; smoothed so it
/// changes gently, and normalised so the surface still gets about as many quads as at one
/// step everywhere.
fn adaptive_scale(g: &Graph, scale: f32, adaptive: f32) -> Vec<f32> {
    let m = g.len();
    let strength = (adaptive / 100.0).clamp(0.0, 1.0);
    let painted = g.dens.iter().any(|&d| d != 0.0);
    if (strength <= 0.0 && !painted) || m == 0 {
        return vec![scale; m];
    }
    // Curvature: how fast the normal turns toward the neighbours.
    let mut ln: Vec<f32> = vec![0.0; m];
    if strength > 0.0 {
        let curv: Vec<f32> = (0..m)
            .into_par_iter()
            .map(|i| {
                let nb = g.neighbours(i);
                if nb.is_empty() {
                    return 0.0;
                }
                nb.iter().map(|&j| (g.n[i] - g.n[j as usize]).length() / g.p[i].distance(g.p[j as usize]).max(1e-9)).sum::<f32>() / nb.len() as f32
            })
            .collect();
        let mut sorted = curv.clone();
        sorted.sort_by(f32::total_cmp);
        let typical = sorted[m / 2].max(1e-6 / scale);
        // In log steps: bends twice the typical halve the step at full strength (bounded).
        for i in 0..m {
            ln[i] = (-(curv[i] / typical).max(1e-3).ln() * 0.5 * strength).clamp(-0.69, 0.69);
        }
    }
    // Density: k times the quads is a step 1 / sqrt(k) as long.
    for i in 0..m {
        ln[i] -= 0.5 * g.dens[i];
    }
    for _ in 0..8 {
        ln = (0..m)
            .map(|i| {
                let nb = g.neighbours(i);
                if nb.is_empty() {
                    return ln[i];
                }
                0.5 * ln[i] + 0.5 * nb.iter().map(|&j| ln[j as usize]).sum::<f32>() / nb.len() as f32
            })
            .collect();
    }
    // Quads per sample go as 1 / step²: keep their mean.
    let mean: f32 = ln.iter().map(|&x| (-2.0 * x).exp()).sum::<f32>() / m as f32;
    let fix = mean.sqrt();
    ln.iter().map(|&x| scale * x.exp() * fix).collect()
}

/// Union-find.
fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        parent[x as usize] = parent[parent[x as usize] as usize];
        x = parent[x as usize];
    }
    x
}

/// Midpoint subdivision, each triangle into four.
fn subdivide(positions: &[Vec3], indices: &[u32], groups: &[u16], dens: &mut Vec<f32>) -> (Vec<Vec3>, Vec<u32>, Vec<u16>) {
    let mut p = positions.to_vec();
    let carry = !dens.is_empty();
    let mut mid: HashMap<(u32, u32), u32> = HashMap::default();
    let mut out = Vec::with_capacity(indices.len() * 4);
    let mut g = Vec::with_capacity(groups.len() * 4);
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let mut m = [0u32; 3];
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            m[k] = *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
                p.push((positions[a as usize] + positions[b as usize]) * 0.5);
                if carry {
                    dens.push((dens[a as usize] + dens[b as usize]) * 0.5);
                }
                (p.len() - 1) as u32
            });
        }
        out.extend_from_slice(&[tri[0], m[0], m[2], m[0], tri[1], m[1], m[2], m[1], tri[2], m[0], m[1], m[2]]);
        if !groups.is_empty() {
            g.extend(std::iter::repeat_n(groups[t], 4));
        }
    }
    (p, out, g)
}

/// The closest point of `mesh` to `p` found along `n` (both ways), and its triangle.
fn project(mesh: &SculptMesh, p: Vec3, n: Vec3, reach: f32) -> Option<(Vec3, u32)> {
    let mut best: Option<(f32, Vec3, u32)> = None;
    for d in [n, -n] {
        if let Some(h) = mesh.raycast(p - d * reach, d) {
            let dist = h.point.distance(p);
            if dist <= reach && best.is_none_or(|b| dist < b.0) {
                best = Some((dist, h.point, h.tri));
            }
        }
    }
    best.map(|b| (b.1, b.2))
}

/// ZRemesh `mesh` into about `s.target_k` thousand quads, with edge loops steered along
/// `guides` (curves on the surface, ZBrush: ZRemesherGuide) by `s.curves_strength`.
pub fn zremesh(mesh: &SculptMesh, s: &Settings, guides: &[Vec<Vec3>]) -> Result<(SculptMesh, Stats), String> {
    // The edge lines: creases, and sharp edges in lines of two quads or more.
    let quad = (surface_area(mesh) / (s.target_k * 1000.0).max(50.0)).sqrt();
    let creased = if s.keep_creases { mesh.crease_edges() } else { Vec::new() };
    let mut edges = if s.detect_edges { crate::crease::long_lines_only(mesh, &crate::crease::sharp_edges(mesh, EDGE_ANGLE), quad * 2.0) } else { Vec::new() };
    edges.extend_from_slice(&creased);
    edges.sort_unstable();
    edges.dedup();
    let (lines, corners) = crate::crease::lines(mesh, &edges, 0.0);
    let features = Features { lines, corners };
    crate::jobs::check()?;
    let (mut out, stats) = match s.mirror {
        Some(plane) => symmetric(mesh, s, guides, &features, plane)?,
        None => remesh(mesh, s, guides, &[], &features, None)?,
    };
    if !creased.is_empty() {
        let (mut lines, _) = crate::crease::lines(mesh, &creased, 0.0);
        if let Some(plane) = s.mirror {
            for point in lines.iter_mut().flatten() {
                *point = plane.fold(*point);
            }
        }
        let quad = stats_quad(&out);
        crease_along(&mut out, &lines, quad, s.mirror);
    }
    if let Some(why) = out.subdivision_blocker() {
        return Err(format!("the result {why} · change Target Polygons Count"));
    }
    Ok((out, stats))
}

fn surface_area(mesh: &SculptMesh) -> f32 {
    mesh.indices
        .par_chunks_exact(3)
        .map(|t| (mesh.positions[t[1] as usize] - mesh.positions[t[0] as usize]).cross(mesh.positions[t[2] as usize] - mesh.positions[t[0] as usize]).length() * 0.5)
        .sum()
}

/// Crease the edges of `out` that lie on `lines` (KeepCreases).
fn crease_along(out: &mut SculptMesh, lines: &[Vec<Vec3>], quad: f32, mirror: Option<MirrorPlane>) {
    let g = Guides::new(lines, quad);
    let (on, mid) = (quad * 0.02, quad * 0.15);
    let fold = |p| mirror.map_or(p, |plane| plane.fold(p));
    let near: Vec<bool> = out.positions.par_iter().map(|&p| g.nearest(fold(p), on).is_some()).collect();
    let mut edges: Vec<(u32, u32)> = out
        .indices
        .par_chunks_exact(3)
        .flat_map_iter(|t| {
            let (near, g, p) = (&near, &g, &out.positions);
            (0..3).filter_map(move |k| {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                (near[a as usize] && near[b as usize] && g.nearest(fold((p[a as usize] + p[b as usize]) * 0.5), mid).is_some()).then_some((a.min(b), a.max(b)))
            })
        })
        .collect();
    edges.par_sort_unstable();
    edges.dedup();
    let bits = out.crease_bits(&edges);
    out.set_creases(bits);
}

/// Remesh the positive half to half the target, hold the cut at full strength, then
/// reflect and weld along the plane.
fn symmetric(mesh: &SculptMesh, s: &Settings, guides: &[Vec<Vec3>], features: &Features, plane: MirrorPlane) -> Result<(SculptMesh, Stats), String> {
    let start = Instant::now();
    let half = crate::subtool_ops::positive_half(mesh, plane)
        .ok_or_else(|| format!("No surface on the +{} side of the mirror plane · try SubTool Center or another axis", plane.axis.name()))?;
    let folded: Vec<Vec<Vec3>> = guides.iter().map(|g| g.iter().map(|&p| plane.fold(p)).collect()).collect();
    // The cut edge: open edges with both ends on the plane.
    let mut count: HashMap<(u32, u32), u32> = HashMap::default();
    for t in half.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let seam: Vec<Vec<Vec3>> = count
        .iter()
        .filter(|&(&(a, b), &c)| c == 1 && plane.distance(half.positions[a as usize]) == 0.0 && plane.distance(half.positions[b as usize]) == 0.0)
        .map(|(&(a, b), _)| vec![half.positions[a as usize], half.positions[b as usize]])
        .collect();
    let hs = Settings { target_k: s.target_k * 0.5, mirror: None, ..*s };
    let (out, mut stats) = remesh(&half, &hs, &folded, &seam, &features.folded(plane), Some(plane))?;
    // Frozen border vertices stay exactly where they are.
    let frozen: crate::fx::FxHashSet<[u32; 3]> = if s.freeze_border { half.positions.iter().map(|p| p.to_array().map(f32::to_bits)).collect() } else { Default::default() };
    // Its open border goes onto the plane; nothing else may reach it, or it would weld.
    let mut edges: HashMap<(u32, u32), u32> = HashMap::default();
    for t in out.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut border = vec![false; out.vertex_count()];
    for (&(a, b), &c) in &edges {
        if c == 1 {
            border[a as usize] = true;
            border[b as usize] = true;
        }
    }
    let (lo, hi) = out.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let nudge = (hi - lo).length() * 1e-4;
    let reach = stats_quad(&out);
    let mut positions = out.positions.clone();
    for (v, p) in positions.iter_mut().enumerate() {
        if frozen.contains(&p.to_array().map(f32::to_bits)) {
            continue;
        }
        let distance = plane.distance(*p);
        if border[v] && distance.abs() < reach {
            *p = plane.project(*p);
        } else if distance <= 0.0 {
            *p = plane.project(*p) + plane.axis.unit() * nudge;
        }
    }
    let groups: Vec<u16> = (0..out.triangle_count() as u32).map(|t| out.group(t)).collect();
    let grouped = out.group_count() > 1;
    let snapped = SculptMesh::from_raw_full(positions, out.indices.clone(), if grouped { groups } else { Vec::new() }, Vec::new(), out.quads.clone());
    let full = crate::subtool_ops::mirror_half(&snapped, plane);
    stats.quads = full.triangle_count() / 2;
    stats.ms = start.elapsed().as_secs_f32() * 1000.0;
    Ok((full, stats))
}

/// About the edge length of `m`'s quads.
fn stats_quad(m: &SculptMesh) -> f32 {
    let t = &m.indices;
    if t.len() < 3 {
        return 0.0;
    }
    let step = (t.len() / 3 / 512).max(1);
    let (sum, n) = t.chunks_exact(3).step_by(step).fold((0.0f32, 0.0f32), |(s, n), c| (s + m.positions[c[0] as usize].distance(m.positions[c[1] as usize]), n + 1.0));
    sum / n
}

/// ZRemesh `mesh`, with `seams` held as full-strength guides (the symmetry plane's cut) and
/// loops put exactly on the `features`. FreezeBorder leaves out the cut on the mirror plane.
fn remesh(mesh: &SculptMesh, s: &Settings, guides: &[Vec<Vec3>], seams: &[Vec<Vec3>], features: &Features, cut: Option<MirrorPlane>) -> Result<(SculptMesh, Stats), String> {
    let start = Instant::now();
    let mut stats = Stats::default();
    let target = (s.target_k * 1000.0).max(50.0);
    let area: f32 = mesh
        .indices
        .par_chunks_exact(3)
        .map(|t| (mesh.positions[t[1] as usize] - mesh.positions[t[0] as usize]).cross(mesh.positions[t[2] as usize] - mesh.positions[t[0] as usize]).length() * 0.5)
        .sum();
    if area <= 0.0 {
        return Err("the surface has no area".into());
    }
    // Quad size, and the lattice step: twice it, as every face is split in the end.
    let quad = (area / target).sqrt();
    let scale = quad * 2.0;

    // 1. Samples: subdivide until edges are well under the cell size.
    let groups: Vec<u16> = if s.keep_groups { (0..mesh.triangle_count() as u32).map(|t| mesh.group(t)).collect() } else { Vec::new() };
    let (mut pos, mut idx, mut grp) = (mesh.positions.clone(), mesh.indices.clone(), groups);
    let mut dens = if s.use_polypaint { paint_density(mesh, s.color_density) } else { Vec::new() };
    let cell = scale / 4.0;
    for _ in 0..5 {
        let edge: f32 = idx.chunks_exact(3).step_by((idx.len() / 3 / 4096).max(1)).map(|t| pos[t[0] as usize].distance(pos[t[1] as usize])).sum::<f32>()
            / (idx.len() / 3).div_ceil((idx.len() / 3 / 4096).max(1)).max(1) as f32;
        if edge <= cell || idx.len() / 3 > 4_000_000 {
            break;
        }
        (pos, idx, grp) = subdivide(&pos, &idx, &grp, &mut dens);
    }
    let fine = vertex_graph(&pos, &idx, &grp, &dens);
    let (g0, _) = coarsen(&fine, cell);
    drop(fine);
    stats.samples = g0.len();
    let mut levels: Vec<(Graph, Vec<u32>)> = vec![(g0, Vec::new())];
    // Each level's cell size, for how far a guide reaches there.
    let mut cells = vec![cell];
    let mut c = cell;
    while levels.last().unwrap().0.len() > 400 && levels.len() < 14 {
        c *= 2.0;
        cells.push(c);
        let (coarse, parent) = coarsen(&levels.last().unwrap().0, c);
        if coarse.len() as f32 > levels.last().unwrap().0.len() as f32 * 0.8 {
            break;
        }
        levels.last_mut().unwrap().1 = parent;
        levels.push((coarse, Vec::new()));
    }
    cells.truncate(levels.len());
    stats.levels = levels.len();
    // Guide curves pull the samples within about a cell of them, at every level.
    let strength = (s.curves_strength / 100.0).clamp(0.0, 1.0);
    let guides = Guides::new(guides, quad);
    let seams: Vec<Vec<Vec3>> = seams.iter().chain(&features.lines).cloned().collect();
    let seams = Guides::new(&seams, quad);
    // A seam holds fully, ahead of any guide.
    let pull: Vec<Vec<Pull>> = levels
        .iter()
        .zip(&cells)
        .map(|((g, _), &c)| {
            let mut p = pulls(g, &guides, c * 0.75, strength);
            for (x, y) in p.iter_mut().zip(pulls(g, &seams, c * 0.75, 1.0)) {
                if y.w > 0.0 {
                    *x = y;
                }
            }
            p
        })
        .collect();
    // The lattice step of every sample: smaller where the surface curves more
    // (AdaptiveSize), keeping the quad count about the same.
    let mut scales: Vec<Vec<f32>> = vec![adaptive_scale(&levels[0].0, scale, s.adaptive)];
    for l in 1..levels.len() {
        let mut sum = vec![0.0f32; levels[l].0.len()];
        let mut cnt = vec![0.0f32; levels[l].0.len()];
        for (i, &pa) in levels[l - 1].1.iter().enumerate() {
            sum[pa as usize] += scales[l - 1][i];
            cnt[pa as usize] += 1.0;
        }
        scales.push(sum.iter().zip(&cnt).map(|(s, c)| s / c.max(1.0)).collect());
    }
    let colours: Vec<Vec<Vec<u32>>> = levels.iter().map(|(g, _)| g.colours()).collect();

    // Smoothing passes per level (more on the finest, which the quads come from).
    let sweeps = |l: usize| -> usize { if l == 0 { 12 } else { 8 } };
    crate::jobs::check()?;
    // 2. Directions, coarse to fine.
    let top = levels.len() - 1;
    let mut q: Vec<Vec3> = levels[top]
        .0
        .n
        .iter()
        .map(|n| {
            let u = n.any_orthonormal_vector();
            if s.alternate { (u + n.cross(u)).normalize() } else { u }
        })
        .collect();
    for l in (0..=top).rev() {
        let g = &levels[l].0;
        if l < top {
            let parent = &levels[l].1;
            q = (0..g.len())
                .map(|i| {
                    let v = q[parent[i] as usize];
                    (v - g.n[i] * v.dot(g.n[i])).normalize_or(g.n[i].any_orthonormal_vector())
                })
                .collect();
        }
        smooth_directions(g, &mut q, &colours[l], sweeps(l), &pull[l]);
    }
    // Directions at every level, for the lattice pass.
    let mut qs: Vec<Vec<Vec3>> = vec![q];
    for l in 1..=top {
        let g = &levels[l].0;
        let mut acc = vec![Vec3::ZERO; g.len()];
        let prev = &qs[l - 1];
        let parent = &levels[l - 1].1;
        for (i, &pa) in parent.iter().enumerate() {
            let pa = pa as usize;
            let v = prev[i];
            acc[pa] = if acc[pa] == Vec3::ZERO {
                v
            } else {
                let (a, b) = compat_orientation(acc[pa], g.n[pa], v, g.n[pa]);
                a + b
            };
        }
        qs.push((0..g.len()).map(|k| (acc[k] - g.n[k] * acc[k].dot(g.n[k])).normalize_or(g.n[k].any_orthonormal_vector())).collect());
    }

    crate::jobs::check()?;
    // 3. Lattice, coarse to fine.
    let mut o: Vec<Vec3> = levels[top].0.p.clone();
    for l in (0..=top).rev() {
        let g = &levels[l].0;
        if l < top {
            let parent = &levels[l].1;
            o = (0..g.len())
                .map(|i| {
                    let v = o[parent[i] as usize];
                    v - g.n[i] * g.n[i].dot(v - g.p[i])
                })
                .collect();
        }
        smooth_positions(g, &qs[l], &mut o, &colours[l], sweeps(l), &scales[l], &pull[l]);
    }
    let g = &levels[0].0;
    let q = &qs[0];

    crate::jobs::check()?;
    // 4. Vertices and edges.
    let m = g.len();
    let mut uf: Vec<u32> = (0..m as u32).collect();
    let mut steps: Vec<(u32, u32)> = Vec::new();
    for i in 0..m {
        for &j in g.neighbours(i) {
            let j = j as usize;
            if j <= i {
                continue;
            }
            let (qi, qj) = compat_orientation(q[i], g.n[i], q[j], g.n[j]);
            let (si, sj) = (scales[0][i], scales[0][j]);
            let (a, b) = compat_position(g.p[i], g.n[i], qi, o[i], g.p[j], g.n[j], qj, o[j], si, sj);
            let scale = (si + sj) * 0.5;
            let d = b - a;
            let ti = g.n[i].cross(qi);
            let (x, y) = ((qi.dot(d) / scale).round() as i32, (ti.dot(d) / scale).round() as i32);
            // Where each sample's own lattice point is relative to the matched pair.
            let (ri, rj) = (o[i] - a, o[j] - b);
            let (xi, yi) = ((qi.dot(ri) / scale).round() as i32, (ti.dot(ri) / scale).round() as i32);
            let (xj, yj) = ((qi.dot(rj) / scale).round() as i32, (ti.dot(rj) / scale).round() as i32);
            let (dx, dy) = (x + xj - xi, y + yj - yi);
            match dx.abs() + dy.abs() {
                0 => {
                    let (ra, rb) = (find(&mut uf, i as u32), find(&mut uf, j as u32));
                    if ra != rb {
                        uf[ra as usize] = rb;
                    }
                }
                1 => steps.push((i as u32, j as u32)),
                _ => {}
            }
        }
    }
    let mut vid = vec![u32::MAX; m];
    let mut vp: Vec<Vec3> = Vec::new();
    let mut vn: Vec<Vec3> = Vec::new();
    let mut vq: Vec<Vec3> = Vec::new();
    let mut cnt: Vec<f32> = Vec::new();
    // Each vertex's lattice step (the mean of its samples').
    let mut vs: Vec<f32> = Vec::new();
    for i in 0..m {
        let r = find(&mut uf, i as u32) as usize;
        if vid[r] == u32::MAX {
            vid[r] = vp.len() as u32;
            vp.push(Vec3::ZERO);
            vn.push(Vec3::ZERO);
            vq.push(q[r]);
            cnt.push(0.0);
            vs.push(0.0);
        }
        let k = vid[r] as usize;
        vid[i] = vid[r];
        vp[k] += o[i];
        vn[k] += g.n[i];
        vs[k] += scales[0][i];
        cnt[k] += 1.0;
    }
    for k in 0..vp.len() {
        vp[k] /= cnt[k];
        vs[k] /= cnt[k];
        vn[k] = vn[k].normalize_or(Vec3::Z);
    }
    let biggest = scales[0].iter().copied().fold(scale, f32::max);
    // One lattice point split into nearby vertices is joined again.
    {
        let k = vp.len();
        let mut uf2: Vec<u32> = (0..k as u32).collect();
        let inv = 1.0 / (biggest * 0.4);
        let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
        for v in 0..k {
            let c = (vp[v] * inv).floor();
            grid.entry((c.x as i32, c.y as i32, c.z as i32)).or_default().push(v as u32);
        }
        for v in 0..k {
            let c = (vp[v] * inv).floor();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(list) = grid.get(&(c.x as i32 + dx, c.y as i32 + dy, c.z as i32 + dz)) {
                            for &w in list {
                                if (w as usize) > v && vp[v].distance(vp[w as usize]) < vs[v].min(vs[w as usize]) * 0.4 && vn[v].dot(vn[w as usize]) > 0.5 {
                                    let (ra, rb) = (find(&mut uf2, v as u32), find(&mut uf2, w));
                                    if ra != rb {
                                        uf2[ra as usize] = rb;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut remap = vec![u32::MAX; k];
        let (mut np, mut nn, mut nq, mut nc, mut ns): (Vec<Vec3>, Vec<Vec3>, Vec<Vec3>, Vec<f32>, Vec<f32>) = Default::default();
        for v in 0..k {
            let r = find(&mut uf2, v as u32) as usize;
            if remap[r] == u32::MAX {
                remap[r] = np.len() as u32;
                np.push(Vec3::ZERO);
                nn.push(Vec3::ZERO);
                nq.push(vq[r]);
                nc.push(0.0);
                ns.push(0.0);
            }
            let t = remap[r] as usize;
            remap[v] = remap[r];
            np[t] += vp[v] * cnt[v];
            nn[t] += vn[v] * cnt[v];
            ns[t] += vs[v] * cnt[v];
            nc[t] += cnt[v];
        }
        for t in 0..np.len() {
            np[t] /= nc[t];
            ns[t] /= nc[t];
            nn[t] = nn[t].normalize_or(Vec3::Z);
        }
        for x in vid.iter_mut() {
            *x = remap[*x as usize];
        }
        (vp, vn, vq, vs) = (np, nn, nq, ns);
    }
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); vp.len()];
    for &(i, j) in &steps {
        let (a, b) = (vid[i as usize], vid[j as usize]);
        // A step is one lattice edge long; far longer ones are strays.
        if a != b && !nb[a as usize].contains(&b) && vp[a as usize].distance(vp[b as usize]) < (vs[a as usize] + vs[b as usize]) {
            nb[a as usize].push(b);
            nb[b as usize].push(a);
        }
    }
    // Dangling edges go, repeatedly.
    loop {
        let lone: Vec<usize> = (0..nb.len()).filter(|&v| nb[v].len() == 1).collect();
        if lone.is_empty() {
            break;
        }
        for v in lone {
            if let Some(&w) = nb[v].first() {
                nb[w as usize].retain(|&x| x != v as u32);
            }
            nb[v].clear();
        }
    }
    // Neighbours in turn around each vertex's normal.
    for v in 0..nb.len() {
        let n = vn[v];
        let u = vq[v] - n * vq[v].dot(n);
        let u = u.normalize_or(n.any_orthonormal_vector());
        let w = n.cross(u);
        let pv = vp[v];
        nb[v].sort_by(|&a, &b| {
            let da = vp[a as usize] - pv;
            let db = vp[b as usize] - pv;
            da.dot(w).atan2(da.dot(u)).total_cmp(&db.dot(w).atan2(db.dot(u)))
        });
    }
    // Faces: from a → b, turn to the neighbour of b just clockwise of a.
    let mut used: HashMap<(u32, u32), ()> = HashMap::default();
    let mut faces: Vec<Vec<u32>> = Vec::new();
    for a in 0..nb.len() as u32 {
        for &b in &nb[a as usize].clone() {
            if used.contains_key(&(a, b)) {
                continue;
            }
            let mut face = vec![a];
            let (mut prev, mut cur) = (a, b);
            let mut ok = false;
            for _ in 0..8 {
                face.push(cur);
                let list = &nb[cur as usize];
                let Some(k) = list.iter().position(|&x| x == prev) else { break };
                let next = list[(k + list.len() - 1) % list.len()];
                prev = cur;
                cur = next;
                if cur == a {
                    ok = true;
                    break;
                }
            }
            if ok && face.len() >= 3 && face.len() <= 8 && {
                let mut f = face.clone();
                f.sort_unstable();
                f.windows(2).all(|w| w[0] != w[1])
            } && (0..face.len()).all(|k| !used.contains_key(&(face[k], face[(k + 1) % face.len()])))
            {
                for k in 0..face.len() {
                    used.insert((face[k], face[(k + 1) % face.len()]), ());
                }
                if face.len() != 4 {
                    stats.odd_faces += 1;
                }
                faces.push(face);
            }
        }
    }
    // A stray diagonal splits a quad into two triangles: join such pairs again, the longest
    // shared edge first, when the quad they make is convex.
    {
        let mut tri_at: HashMap<(u32, u32), usize> = HashMap::default();
        let mut pairs: Vec<(f32, usize, usize, u32, u32)> = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            if f.len() != 3 {
                continue;
            }
            for k in 0..3 {
                let (a, b) = (f[k], f[(k + 1) % 3]);
                if let Some(&gi) = tri_at.get(&(b, a)) {
                    pairs.push((vp[a as usize].distance(vp[b as usize]), fi, gi, a, b));
                }
                tri_at.insert((a, b), fi);
            }
        }
        pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
        let mut gone = vec![false; faces.len()];
        let mut merged = Vec::new();
        for (_, fi, gi, a, b) in pairs {
            if gone[fi] || gone[gi] {
                continue;
            }
            // f runs a → b → c, g runs b → a → d: the quad is a, d, b, c.
            let c = faces[fi].iter().copied().find(|&v| v != a && v != b).unwrap();
            let d = faces[gi].iter().copied().find(|&v| v != a && v != b).unwrap();
            if c == d {
                continue;
            }
            let quad = [a, d, b, c];
            let n = vn[a as usize] + vn[b as usize];
            let convex = (0..4).all(|k| {
                let (p0, p1, p2) = (vp[quad[k] as usize], vp[quad[(k + 1) % 4] as usize], vp[quad[(k + 2) % 4] as usize]);
                (p1 - p0).cross(p2 - p1).dot(n) > 0.0
            });
            if convex {
                gone[fi] = true;
                gone[gi] = true;
                merged.push(quad.to_vec());
            }
        }
        let mut kept: Vec<Vec<u32>> = faces.into_iter().enumerate().filter(|(i, _)| !gone[*i]).map(|(_, f)| f).collect();
        kept.extend(merged);
        faces = kept;
        stats.odd_faces = faces.iter().filter(|f| f.len() != 4).count();
    }
    if faces.is_empty() {
        return Err("no quads came out · raise the target count".into());
    }
    // Holes in a closed surface are closed: the border run backwards is a face.
    let closed = crate::wedge::closed(mesh);
    if closed {
        let mut next: HashMap<u32, Vec<u32>> = HashMap::default();
        for (&(a, b), _) in &used {
            if !used.contains_key(&(b, a)) {
                next.entry(b).or_default().push(a);
            }
        }
        let mut starts: Vec<u32> = next.keys().copied().collect();
        starts.sort_unstable();
        for s0 in starts {
            while let Some(first) = next.get_mut(&s0).and_then(|v| v.pop()) {
                let mut lp = vec![s0];
                let mut at = first;
                while at != s0 && lp.len() <= 256 {
                    lp.push(at);
                    let Some(n) = next.get_mut(&at).and_then(|v| v.pop()) else { break };
                    at = n;
                }
                if at == s0 && lp.len() >= 3 {
                    stats.holes += 1;
                    faces.push(lp);
                }
            }
        }
    }

    // A face wound against the surface where another face has the same two edges at a
    // corner is the back of a border vertex (the walk round a short open border closes
    // behind it): it goes. Two faces wound alike sharing a corner's edges (a vertex with
    // two neighbours) are a valid corner, kept; the triangulation below keeps their quads
    // off one diagonal.
    {
        let mut corners: Vec<((u32, u32, u32), usize)> = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            let n = f.len();
            for k in 0..n {
                let (p, v, q) = (f[(k + n - 1) % n], f[k], f[(k + 1) % n]);
                corners.push(((v, p.min(q), p.max(q)), fi));
            }
        }
        corners.sort_unstable();
        let wound = |f: &[u32]| -> f32 {
            let c = f.iter().map(|&v| vp[v as usize]).sum::<Vec3>() / f.len() as f32;
            let n = f.iter().map(|&v| vn[v as usize]).sum::<Vec3>();
            (0..f.len()).map(|k| (vp[f[k] as usize] - c).cross(vp[f[(k + 1) % f.len()] as usize] - c)).sum::<Vec3>().dot(n)
        };
        let mut gone = vec![false; faces.len()];
        for w in corners.windows(2) {
            let (f, g) = (w[0].1, w[1].1);
            if w[0].0 != w[1].0 || f == g || gone[f] || gone[g] {
                continue;
            }
            let (wf, wg) = (wound(&faces[f]), wound(&faces[g]));
            if (wf < 0.0) != (wg < 0.0) {
                gone[if wf < 0.0 { f } else { g }] = true;
            }
        }
        if gone.contains(&true) {
            faces = faces.into_iter().enumerate().filter(|(i, _)| !gone[*i]).map(|(_, f)| f).collect();
            stats.odd_faces = faces.iter().filter(|f| f.len() != 4).count();
        }
    }

    // Split every face into quads through its middle.
    let mut out_p = vp.clone();
    let mut mids: HashMap<(u32, u32), u32> = HashMap::default();
    let mut quads: Vec<[u32; 4]> = Vec::new();
    for f in &faces {
        let c = out_p.len() as u32;
        out_p.push(f.iter().map(|&v| vp[v as usize]).sum::<Vec3>() / f.len() as f32);
        let mut mid = |a: u32, b: u32, out_p: &mut Vec<Vec3>| -> u32 {
            *mids.entry((a.min(b), a.max(b))).or_insert_with(|| {
                out_p.push((vp[a as usize] + vp[b as usize]) * 0.5);
                (out_p.len() - 1) as u32
            })
        };
        let k = f.len();
        let ms: Vec<u32> = (0..k).map(|i| mid(f[i], f[(i + 1) % k], &mut out_p)).collect();
        for i in 0..k {
            quads.push([f[i], ms[i], c, ms[(i + k - 1) % k]]);
        }
    }
    // FreezeBorder: the old open borders come back exactly, each joined to the new border
    // nearest it by a strip. A triangle is kept as a quad with its last corner repeated.
    let mut pinned = vec![false; out_p.len()];
    if s.freeze_border && !closed {
        let strip = frozen_strips(mesh, &quads, &mut out_p, cut);
        pinned.resize(out_p.len(), true);
        quads.extend(strip);
    }
    let is_tri = |q4: &[u32; 4]| q4[2] == q4[3];
    stats.quads = quads.iter().filter(|q| !is_tri(q)).count();
    stats.odd_faces += quads.len() - stats.quads;

    // Onto the old surface, then even spacing along it.
    let reach = biggest * 1.5;
    let mut vnorm = vec![Vec3::ZERO; out_p.len()];
    let normals = |p: &[Vec3], out: &mut Vec<Vec3>| {
        out.iter_mut().for_each(|n| *n = Vec3::ZERO);
        for q4 in &quads {
            let n = (p[q4[2] as usize] - p[q4[0] as usize]).cross(p[q4[3] as usize] - p[q4[1] as usize]);
            for &v in q4 {
                out[v as usize] += n;
            }
        }
        out.iter_mut().for_each(|n| *n = n.normalize_or(Vec3::Z));
    };
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); out_p.len()];
    for q4 in &quads {
        for k in 0..4 {
            let (a, b) = (q4[k], q4[(k + 1) % 4]);
            if a != b && !adj[a as usize].contains(&b) {
                adj[a as usize].push(b);
                adj[b as usize].push(a);
            }
        }
    }
    for pass in 0..4 {
        normals(&out_p, &mut vnorm);
        if pass > 0 {
            let relaxed: Vec<Vec3> = (0..out_p.len())
                .into_par_iter()
                .map(|v| {
                    let a = &adj[v];
                    if a.is_empty() || pinned[v] {
                        return out_p[v];
                    }
                    let c = a.iter().map(|&w| out_p[w as usize]).sum::<Vec3>() / a.len() as f32;
                    let n = vnorm[v];
                    let d = c - out_p[v];
                    out_p[v] + (d - n * n.dot(d)) * 0.5
                })
                .collect();
            out_p = relaxed;
        }
        let projected: Vec<Option<(Vec3, u32)>> = out_p.par_iter().zip(vnorm.par_iter()).map(|(&p, &n)| project(mesh, p, n, reach)).collect();
        for (v, hit) in projected.into_iter().enumerate() {
            if let Some((p, _)) = hit {
                if !pinned[v] {
                    out_p[v] = p;
                }
            }
        }
    }
    // The loop that came out on a guide goes onto it.
    if !guides.is_empty() && strength > 0.0 {
        let snapped: Vec<(usize, Vec3)> = (0..out_p.len())
            .into_par_iter()
            .filter(|&v| !pinned[v])
            .filter_map(|v| guides.nearest(out_p[v], quad * 0.3).map(|(_, q, _)| (v, out_p[v].lerp(q, strength))))
            .collect();
        stats.on_guides = snapped.len();
        for (v, q) in snapped {
            out_p[v] = q;
        }
    }
    // The loops on edge lines go exactly onto them, corners first.
    if !features.lines.is_empty() {
        let lines = Guides::new(&features.lines, quad);
        let cell = quad.max(1e-6);
        let mut corners: HashMap<(i32, i32, i32), Vec<Vec3>> = HashMap::default();
        for &c in &features.corners {
            let k = (c / cell).floor();
            corners.entry((k.x as i32, k.y as i32, k.z as i32)).or_default().push(c);
        }
        let corner = |p: Vec3| -> Option<Vec3> {
            let k = (p / cell).floor();
            let mut best: Option<(f32, Vec3)> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &c in corners.get(&(k.x as i32 + dx, k.y as i32 + dy, k.z as i32 + dz)).into_iter().flatten() {
                            let d = c.distance(p);
                            if d < quad * 0.5 && best.is_none_or(|b| d < b.0) {
                                best = Some((d, c));
                            }
                        }
                    }
                }
            }
            best.map(|b| b.1)
        };
        let snapped: Vec<(usize, Vec3)> = (0..out_p.len())
            .into_par_iter()
            .filter(|&v| !pinned[v])
            .filter_map(|v| corner(out_p[v]).or_else(|| lines.nearest(out_p[v], quad * 0.4).map(|(_, q, _)| q)).map(|q| (v, q)))
            .collect();
        for (v, q) in snapped {
            out_p[v] = q;
        }
    }

    // Polygroups: each quad takes the one under its middle. Vertices between quads of
    // different groups go onto the old group border, so the borders stay crisp.
    let grouped = s.keep_groups && mesh.group_count() > 1;
    let mut quad_group = vec![0u16; quads.len()];
    if grouped {
        quad_group = quads
            .par_iter()
            .map(|q4| {
                let c = q4.iter().map(|&v| out_p[v as usize]).sum::<Vec3>() / 4.0;
                let n = (out_p[q4[2] as usize] - out_p[q4[0] as usize]).cross(out_p[q4[3] as usize] - out_p[q4[1] as usize]).normalize_or(Vec3::Z);
                project(mesh, c, n, reach).map_or(0, |(_, t)| mesh.group(t))
            })
            .collect();
        let mut around: Vec<Vec<u16>> = vec![Vec::new(); out_p.len()];
        for (q4, &g) in quads.iter().zip(&quad_group) {
            for &v in q4 {
                if !around[v as usize].contains(&g) {
                    around[v as usize].push(g);
                }
            }
        }
        let border: Vec<bool> = around.iter().map(|g| g.len() >= 2).collect();
        // Old corners: vertices where three or more groups meet.
        let mut old_around: Vec<Vec<u16>> = vec![Vec::new(); mesh.vertex_count()];
        for (t, tri) in mesh.indices.chunks_exact(3).enumerate() {
            let g = mesh.group(t as u32);
            for &v in tri {
                if !old_around[v as usize].contains(&g) {
                    old_around[v as usize].push(g);
                }
            }
        }
        let corners: Vec<Vec3> = (0..mesh.vertex_count()).filter(|&v| old_around[v].len() >= 3).map(|v| mesh.positions[v]).collect();
        // The old border edges, by grid cell.
        let cell = quad.max(1e-6);
        let mut segs: HashMap<(i32, i32, i32), Vec<(Vec3, Vec3)>> = HashMap::default();
        let mut sides: HashMap<(u32, u32), u16> = HashMap::default();
        for (t, tri) in mesh.indices.chunks_exact(3).enumerate() {
            let g = mesh.group(t as u32);
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                match sides.get(&key) {
                    Some(&h) if h != g => {
                        let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
                        let c = ((pa + pb) * 0.5 / cell).floor();
                        segs.entry((c.x as i32, c.y as i32, c.z as i32)).or_default().push((pa, pb));
                    }
                    None => {
                        sides.insert(key, g);
                    }
                    _ => {}
                }
            }
        }
        let snapped: Vec<(usize, Vec3)> = (0..out_p.len())
            .into_par_iter()
            .filter(|&v| border[v] && !pinned[v])
            .filter_map(|v| {
                let p = out_p[v];
                let c = (p / cell).floor();
                let mut best: Option<(f32, Vec3)> = None;
                for dx in -2..=2 {
                    for dy in -2..=2 {
                        for dz in -2..=2 {
                            for &(a, b) in segs.get(&(c.x as i32 + dx, c.y as i32 + dy, c.z as i32 + dz)).into_iter().flatten() {
                                let ab = b - a;
                                let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
                                let q = a + ab * t;
                                let d = q.distance_squared(p);
                                if best.is_none_or(|x| d < x.0) {
                                    best = Some((d, q));
                                }
                            }
                        }
                    }
                }
                best.filter(|b| b.0 < (quad * 1.5) * (quad * 1.5)).map(|b| (v, b.1))
            })
            .collect();
        for (v, q) in snapped {
            out_p[v] = q;
        }
        // Where three groups meet, onto the old corner.
        for v in 0..out_p.len() {
            if around[v].len() >= 3 && !pinned[v] {
                if let Some(c) = corners.iter().copied().min_by(|a, b| a.distance_squared(out_p[v]).total_cmp(&b.distance_squared(out_p[v]))) {
                    if c.distance(out_p[v]) < quad * 1.5 {
                        out_p[v] = c;
                    }
                }
            }
        }
    }

    // Triangles (each quad along its shorter diagonal, unless that is an edge or the
    // diagonal of another quad: two quads on a vertex with two neighbours share three
    // corners, and one diagonal between them would be an edge in four triangles).
    let mut indices = Vec::with_capacity(quads.len() * 6);
    let mut tri_groups = Vec::with_capacity(quads.len() * 2);
    let mut halves = Vec::with_capacity(quads.len() * 2);
    let mut taken: crate::fx::FxHashSet<(u32, u32)> = quads
        .iter()
        .flat_map(|q4| (0..4).map(move |k| (q4[k].min(q4[(k + 1) % 4]), q4[k].max(q4[(k + 1) % 4]))))
        .collect();
    for (q4, &g) in quads.iter().zip(&quad_group) {
        let p = |v: u32| out_p[v as usize];
        if is_tri(q4) {
            indices.extend_from_slice(&[q4[0], q4[1], q4[2]]);
            tri_groups.push(g);
            halves.push(crate::mesh::LONE);
            continue;
        }
        let key = |a: u32, b: u32| (a.min(b), a.max(b));
        let mut first = p(q4[0]).distance_squared(p(q4[2])) <= p(q4[1]).distance_squared(p(q4[3]));
        if taken.contains(&if first { key(q4[0], q4[2]) } else { key(q4[1], q4[3]) }) {
            first = !first;
        }
        if first {
            taken.insert(key(q4[0], q4[2]));
            indices.extend_from_slice(&[q4[0], q4[1], q4[2], q4[0], q4[2], q4[3]]);
            halves.extend_from_slice(&[2, 0]);
        } else {
            taken.insert(key(q4[1], q4[3]));
            indices.extend_from_slice(&[q4[0], q4[1], q4[3], q4[1], q4[2], q4[3]]);
            halves.extend_from_slice(&[1, 2]);
        }
        tri_groups.extend_from_slice(&[g, g]);
    }
    let out = SculptMesh::from_raw_full(out_p, indices, if grouped { tri_groups } else { Vec::new() }, Vec::new(), halves);
    stats.ms = start.elapsed().as_secs_f32() * 1000.0;
    Ok((out, stats))
}

/// Open border loops of faces given as corner lists (a triangle as a quad with its last
/// corner repeated), each in the direction its faces run along it.
fn border_loops<'a>(faces: impl Iterator<Item = &'a [u32]>) -> Vec<Vec<u32>> {
    let mut directed: crate::fx::FxHashSet<(u32, u32)> = Default::default();
    for f in faces {
        for k in 0..f.len() {
            let (a, b) = (f[k], f[(k + 1) % f.len()]);
            if a != b {
                directed.insert((a, b));
            }
        }
    }
    let mut next: HashMap<u32, u32> = HashMap::default();
    for &(a, b) in &directed {
        if !directed.contains(&(b, a)) {
            next.insert(a, b);
        }
    }
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut seen: crate::fx::FxHashSet<u32> = Default::default();
    let mut out = Vec::new();
    for s0 in starts {
        if seen.contains(&s0) {
            continue;
        }
        let mut lp = vec![s0];
        seen.insert(s0);
        let mut at = next[&s0];
        while at != s0 && seen.insert(at) {
            lp.push(at);
            let Some(&n) = next.get(&at) else { break };
            at = n;
        }
        if at == s0 && lp.len() >= 3 {
            out.push(lp);
        }
    }
    out
}

/// FreezeBorder: every open border of `mesh` is added to `out_p` as it is and joined to the
/// new border of `quads` nearest it by a strip of quads, with triangles where one side has
/// more vertices than the other. Returns the strip. A border crossing the mirror plane is frozen
/// from the plane to the plane, joined to the stretch of the new border between the same
/// points, and its mirror image joins it there.
fn frozen_strips(mesh: &SculptMesh, quads: &[[u32; 4]], out_p: &mut Vec<Vec3>, plane: Option<MirrorPlane>) -> Vec<[u32; 4]> {
    let mp = &mesh.positions;
    let cut = |a: u32, b: u32| plane.is_some_and(|p| {
        p.distance(mp[a as usize]) == 0.0 && p.distance(mp[b as usize]) == 0.0
    });
    // Old borders: whole loops, and pieces of loops between cut edges.
    let mut olds: Vec<(Vec<u32>, bool)> = Vec::new();
    for l in border_loops(mesh.indices.chunks_exact(3)) {
        let n = l.len();
        let Some(k0) = (0..n).find(|&k| cut(l[k], l[(k + 1) % n])) else {
            olds.push((l, true));
            continue;
        };
        // Walk from just after a cut edge, splitting at every cut edge.
        let mut piece = vec![l[(k0 + 1) % n]];
        for s in 1..=n {
            let (a, b) = (l[(k0 + s) % n], l[(k0 + s + 1) % n]);
            if cut(a, b) {
                if piece.len() >= 2 {
                    olds.push((std::mem::take(&mut piece), false));
                }
                piece = vec![b];
            } else {
                piece.push(b);
            }
        }
    }
    let new_loops = border_loops(quads.iter().map(|q| &q[..]));
    let length = |pts: &[Vec3], closed: bool| (0..pts.len() - (!closed) as usize).map(|k| pts[k].distance(pts[(k + 1) % pts.len()])).sum::<f32>();
    let mut id: HashMap<u32, u32> = HashMap::default();
    let mut strip = Vec::new();
    let mut new_used = vec![false; new_loops.len()];
    // Longest first, so a whole loop is matched before the pieces near it.
    olds.sort_by(|a, b| length(&b.0.iter().map(|&v| mp[v as usize]).collect::<Vec<_>>(), b.1).total_cmp(&length(&a.0.iter().map(|&v| mp[v as usize]).collect::<Vec<_>>(), a.1)));
    for (old, closed) in olds {
        let op: Vec<Vec3> = old.iter().map(|&v| mp[v as usize]).collect();
        let (ol, oc) = (length(&op, closed), op.iter().sum::<Vec3>() / op.len() as f32);
        // The new border it joins: a whole loop of about the same length and place, or for
        // a piece the stretch of a loop between the points nearest its ends.
        let mut best: Option<(f32, usize, Vec<u32>)> = None;
        for (i, c) in new_loops.iter().enumerate() {
            if new_used[i] && closed {
                continue;
            }
            let cp: Vec<Vec3> = c.iter().map(|&v| out_p[v as usize]).collect();
            let m = c.len();
            let (stretch, d) = if closed {
                let (cl, cc) = (length(&cp, true), cp.iter().sum::<Vec3>() / m as f32);
                if !(cl > ol * 0.5 && cl < ol * 2.0) {
                    continue;
                }
                (c.clone(), cc.distance(oc))
            } else {
                let near = |q: Vec3| (0..m).min_by(|&x, &y| cp[x].distance_squared(q).total_cmp(&cp[y].distance_squared(q))).unwrap();
                let (k0, k1) = (near(op[0]), near(*op.last().unwrap()));
                if k0 == k1 {
                    continue;
                }
                let run = |from: usize, to: usize| -> Vec<u32> { (0..=(to + m - from) % m).map(|s| c[(from + s) % m]).collect() };
                let mid = op[op.len() / 2];
                let off = |r: &[u32]| r.iter().map(|&v| out_p[v as usize].distance_squared(mid)).fold(f32::INFINITY, f32::min);
                let (fwd, back) = (run(k0, k1), run(k1, k0));
                let r = if off(&fwd) <= off(&back) { fwd } else { back };
                let d = cp[k0].distance(op[0]) + cp[k1].distance(*op.last().unwrap()) + off(&r).sqrt();
                (r, d)
            };
            if d < ol * 0.25 + 1e-6 && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, i, stretch));
            }
        }
        let Some((_, i, c)) = best else { continue };
        if closed {
            new_used[i] = true;
        }
        let mut b: Vec<u32> = old
            .iter()
            .map(|&v| {
                *id.entry(v).or_insert_with(|| {
                    out_p.push(mp[v as usize]);
                    (out_p.len() - 1) as u32
                })
            })
            .collect();
        let (m, n) = (c.len(), b.len());
        let pc = |k: usize| out_p[c[k % m] as usize];
        if closed {
            // Start at the vertex nearest the new loop's first, running the same way.
            let start = (0..n).min_by(|&x, &y| out_p[b[x] as usize].distance_squared(pc(0)).total_cmp(&out_p[b[y] as usize].distance_squared(pc(0)))).unwrap();
            b.rotate_left(start);
            if (out_p[b[1] as usize] - out_p[b[n - 1] as usize]).dot(pc(1) - pc(m - 1)) < 0.0 {
                b[1..].reverse();
            }
        } else if out_p[b[0] as usize].distance_squared(pc(0)) > out_p[b[n - 1] as usize].distance_squared(pc(0)) {
            b.reverse();
        }
        let pb = |k: usize| out_p[b[k % n] as usize];
        // Segments along each side, and how far along each vertex is (0 to 1).
        let (sm, sn) = if closed { (m, n) } else { (m - 1, n - 1) };
        let params = |len: usize, at: &dyn Fn(usize) -> Vec3| -> Vec<f32> {
            let mut u = vec![0.0f32; len + 1];
            for k in 0..len {
                u[k + 1] = u[k] + at(k).distance(at(k + 1));
            }
            let total = u[len].max(1e-12);
            u.iter_mut().for_each(|x| *x /= total);
            u
        };
        let (uc, ub) = (params(sm, &pc), params(sn, &pb));
        let (mut i, mut j) = (0usize, 0usize);
        while i < sm || j < sn {
            let (ci, ci1, bj, bj1) = (c[i % m], c[(i + 1) % m], b[j % n], b[(j + 1) % n]);
            let both = i < sm && j < sn && (uc[i + 1] - ub[j + 1]).abs() < 0.5 * (uc[i + 1] - uc[i]).min(ub[j + 1] - ub[j]);
            if both {
                strip.push([bj, bj1, ci1, ci]);
                i += 1;
                j += 1;
            } else if i < sm && (j >= sn || uc[i + 1] < ub[j + 1]) {
                strip.push([bj, ci1, ci, ci]);
                i += 1;
            } else {
                strip.push([bj, bj1, ci, ci]);
                j += 1;
            }
        }
    }
    strip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(seg: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(seg, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// Edges by how many triangles hold them.
    fn edge_counts(m: &SculptMesh) -> HashMap<(u32, u32), u32> {
        let mut c: HashMap<(u32, u32), u32> = HashMap::default();
        for t in m.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *c.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        c
    }

    /// Positions as exact bit patterns, for comparing vertex sets.
    fn bits(p: Vec3) -> [u32; 3] {
        p.to_array().map(f32::to_bits)
    }

    /// The open border vertices of `m`.
    fn border_points(m: &SculptMesh) -> Vec<[u32; 3]> {
        let mut out: Vec<[u32; 3]> = edge_counts(m).into_iter().filter(|e| e.1 == 1).flat_map(|((a, b), _)| [bits(m.positions[a as usize]), bits(m.positions[b as usize])]).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    #[test]
    fn detect_edges_puts_loops_on_a_cubes_edges_and_corners() {
        let (p, i) = crate::primitive::box_surface([24; 3], |p| p);
        let m = SculptMesh::from_raw(p, i);
        let run = |detect: bool| zremesh(&m, &Settings { target_k: 0.6, keep_groups: false, adaptive: 0.0, detect_edges: detect, ..Settings::default() }, &[]).unwrap().0;
        let on_edge = |o: &SculptMesh| o.positions.iter().filter(|p| p.abs().to_array().iter().filter(|&&c| (c - 1.0).abs() < 1e-5).count() >= 2).count();
        let corners = |o: &SculptMesh| o.positions.iter().filter(|p| (p.abs() - Vec3::ONE).length() < 1e-5).count();
        let (on, off) = (run(true), run(false));
        eprintln!("on edges: {} with Detect Edges, {} without; corners {} / {}", on_edge(&on), on_edge(&off), corners(&on), corners(&off));
        assert_eq!(corners(&on), 8);
        // Quads about 0.2 across: some ten vertices along each of the twelve edges.
        assert!(on_edge(&on) >= 12 * 8, "{}", on_edge(&on));
        assert!(on_edge(&on) > on_edge(&off) * 2);
        assert!(on.positions.iter().all(|p| (p.abs().max_element() - 1.0).abs() < 0.02));
    }

    #[test]
    fn keep_creases_runs_a_creased_loop_along_a_crease() {
        let mut m = sphere(30);
        let ring: Vec<(u32, u32)> = edge_counts(&m).into_keys().filter(|&(a, b)| m.positions[a as usize].y.abs() < 1e-6 && m.positions[b as usize].y.abs() < 1e-6).collect();
        let mut ring = ring;
        ring.sort_unstable();
        assert_eq!(ring.len(), 4 * 30);
        let bits = m.crease_bits(&ring);
        m.set_creases(bits);
        let s = Settings { target_k: 1.0, keep_groups: false, adaptive: 0.0, ..Settings::default() };
        let (out, _) = zremesh(&m, &s, &[]).unwrap();
        let creased = out.crease_edges();
        eprintln!("{} creased edges", creased.len());
        // About the equator over the quad size, all on it.
        assert!(creased.len() >= 40, "{}", creased.len());
        assert!(creased.iter().all(|&(a, b)| out.positions[a as usize].y.abs() < 1e-4 && out.positions[b as usize].y.abs() < 1e-4));
        let (plain, _) = zremesh(&m, &Settings { keep_creases: false, ..s }, &[]).unwrap();
        assert!(plain.creases.is_empty());
    }

    #[test]
    fn freeze_border_keeps_every_border_vertex_also_with_symmetry() {
        let s = sphere(30);
        let dome = crate::subtool_ops::part(&s, |t| {
            let i = 3 * t as usize;
            (s.positions[s.indices[i] as usize] + s.positions[s.indices[i + 1] as usize] + s.positions[s.indices[i + 2] as usize]).y > 0.0
        })
        .unwrap();
        let before = border_points(&dome);
        assert_eq!(before.len(), 4 * 30);
        for symmetric in [false, true] {
            let st = Settings { target_k: 0.5, keep_groups: false, adaptive: 0.0, freeze_border: true, mirror: symmetric.then_some(TEST_X_PLANE), ..Settings::default() };
            let (out, stats) = zremesh(&dome, &st, &[]).unwrap();
            eprintln!("symmetric {symmetric}: {stats:?}");
            let after = border_points(&out);
            assert_eq!(after.len(), before.len());
            if symmetric {
                // The −X side is the mirror of the +X side, which this sphere matches only to
                // the last bit.
                let f = |b: &[u32; 3]| Vec3::from_array(b.map(f32::from_bits));
                assert!(before.iter().all(|b| after.iter().any(|a| f(a).distance(f(b)) < 1e-6)));
            } else {
                assert_eq!(after, before);
            }
            assert!(edge_counts(&out).values().all(|&k| k <= 2), "manifold");
            assert_eq!(edge_counts(&out).values().filter(|&&k| k == 1).count(), 4 * 30, "one border, as before");
            // Without it the border is the new one.
            let (free, _) = zremesh(&dome, &Settings { freeze_border: false, ..st }, &[]).unwrap();
            assert_ne!(border_points(&free), before);
        }
    }

    #[test]
    fn a_sphere_becomes_even_quads_near_the_target() {
        // A dense, uneven sphere: squash its vertices toward the poles first.
        let m = sphere(40);
        let (out, stats) = zremesh(&m, &Settings { target_k: 1.0, keep_groups: false, adaptive: 0.0, ..Settings::default() }, &[]).unwrap();
        eprintln!("{stats:?}");
        // About the target count (split quads: twice the triangles).
        let quads = out.triangle_count() / 2;
        assert!((600..1600).contains(&quads), "{quads} quads, {stats:?}");
        // Closed and on the sphere.
        let counts = edge_counts(&out);
        let open = counts.values().filter(|&&k| k != 2).count();
        assert!(open * 50 < counts.len(), "{open} of {} edges not between two triangles", counts.len());
        assert!(out.positions.iter().all(|p| (p.length() - 1.0).abs() < 0.02));
        // Even: quad edges within a modest spread of their mean.
        let mut lens = Vec::new();
        for t in out.indices.chunks_exact(6) {
            // The shared diagonal is the longest; take the other edges.
            let tri = [t[0], t[1], t[2]];
            let mut e: Vec<f32> = (0..3).map(|k| out.positions[tri[k] as usize].distance(out.positions[tri[(k + 1) % 3] as usize])).collect();
            e.sort_by(f32::total_cmp);
            lens.extend_from_slice(&e[..2]);
        }
        let mean = lens.iter().sum::<f32>() / lens.len() as f32;
        let within = lens.iter().filter(|&&l| (l / mean - 1.0).abs() < 0.4).count() as f32 / lens.len() as f32;
        assert!(within > 0.85, "{within} of edges near the mean {mean}");
        // Mostly regular: most vertices have four quad edges (diagonals are each
        // triangle pair's long shared edge).
        let mut diag: std::collections::HashSet<(u32, u32)> = Default::default();
        for t in out.indices.chunks_exact(3) {
            let e: Vec<(f32, u32, u32)> = (0..3).map(|k| (out.positions[t[k] as usize].distance(out.positions[t[(k + 1) % 3] as usize]), t[k], t[(k + 1) % 3])).collect();
            let l = e.iter().cloned().fold((0.0, 0, 0), |a, b| if b.0 > a.0 { b } else { a });
            diag.insert((l.1.min(l.2), l.1.max(l.2)));
        }
        let valence = |v: u32| out.neighbors(v).iter().filter(|&&w| !diag.contains(&(v.min(w), v.max(w)))).count();
        let regular = (0..out.vertex_count() as u32).filter(|&v| valence(v) == 4).count() as f32 / out.vertex_count() as f32;
        assert!(regular > 0.7, "{regular}");
    }

    #[test]
    fn adaptive_size_puts_more_quads_where_the_surface_curves() {
        // A big ball and a small one: the small one curves five times as much.
        let (p1, i1) = crate::mesh::cube_sphere(30, 1.0);
        let (p2, i2) = crate::mesh::cube_sphere(30, 0.2);
        let n = p1.len() as u32;
        let mut pos = p1;
        pos.extend(p2.into_iter().map(|p| p + Vec3::new(2.0, 0.0, 0.0)));
        let mut idx = i1;
        idx.extend(i2.into_iter().map(|i| i + n));
        let m = SculptMesh::from_raw(pos, idx);
        let small = |s: f32| {
            let (out, _) = zremesh(&m, &Settings { target_k: 2.0, keep_groups: false, adaptive: s, ..Settings::default() }, &[]).unwrap();
            let quads = out.triangle_count() / 2;
            let on_small = out.indices.chunks_exact(6).filter(|q| out.positions[q[0] as usize].x > 1.5).count();
            (on_small, quads)
        };
        let (even, total_even) = small(0.0);
        let (adapt, total_adapt) = small(100.0);
        assert!(adapt as f32 > even as f32 * 1.8, "small ball: {even} quads even, {adapt} adaptive");
        assert!((total_adapt as f32 / total_even as f32 - 1.0).abs() < 0.35, "total {total_even} vs {total_adapt}");
    }

    /// Quad edges (each quad's two triangles less their shared diagonal).
    fn quad_edges(m: &SculptMesh) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for t in m.indices.chunks_exact(6) {
            let e = |a: u32, b: u32| (a.min(b), a.max(b));
            let first: Vec<_> = (0..3).map(|k| e(t[k], t[(k + 1) % 3])).collect();
            let second: Vec<_> = (0..3).map(|k| e(t[3 + k], t[3 + (k + 1) % 3])).collect();
            out.extend(first.iter().filter(|x| !second.contains(x)));
            out.extend(second.iter().filter(|x| !first.contains(x)));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    #[test]
    fn a_guide_curve_puts_an_edge_loop_on_it() {
        // A great circle tilted away from every axis, so the sphere's own field would not
        // follow it.
        let m = sphere(40);
        let axis = Vec3::new(1.0, 2.0, 3.0).normalize();
        let u = axis.any_orthonormal_vector();
        let v = axis.cross(u);
        let circle: Vec<Vec3> = (0..=256).map(|k| {
            let a = k as f32 / 256.0 * std::f32::consts::TAU;
            u * a.cos() + v * a.sin()
        }).collect();
        let along = |guides: &[Vec<Vec3>], strength: f32| {
            let s = Settings { target_k: 1.0, keep_groups: false, adaptive: 0.0, curves_strength: strength, ..Settings::default() };
            let (out, stats) = zremesh(&m, &s, guides).unwrap();
            let quad = (4.0 * std::f32::consts::PI / 1000.0).sqrt();
            // Distance from the circle, for points on the unit sphere.
            let off = |p: Vec3| p.dot(axis).abs();
            let on: usize = quad_edges(&out)
                .iter()
                .filter(|&&(a, b)| {
                    let (pa, pb) = (out.positions[a as usize], out.positions[b as usize]);
                    off(pa) < quad * 0.15 && off(pb) < quad * 0.15 && pa.distance(pb) > quad * 0.5
                })
                .count();
            (on, quad, stats)
        };
        let (free, quad, _) = along(&[], 50.0);
        let (guided, _, stats) = along(&[circle.clone()], 100.0);
        let (half, _, _) = along(&[circle], 50.0);
        let expected = (std::f32::consts::TAU / quad) as usize;
        eprintln!("edges on the circle: free {free}, strength 50 {half}, strength 100 {guided} of about {expected} · {stats:?}");
        assert!(guided as f32 > expected as f32 * 0.8, "{guided} of {expected} edges on the guide");
        assert!(half as f32 > expected as f32 * 0.6, "{half} of {expected} edges on the guide at strength 50");
        assert!((free as f32) < guided as f32 * 0.5, "the sphere had {free} edges along the circle unguided");
    }

    #[test]
    fn red_polypaint_asks_for_more_quads_and_blue_for_fewer() {
        let mut m = sphere(40);
        m.colors = m.positions.iter().map(|p| if p.y > 0.0 { [230, 20, 20, 255] } else { [0; 4] }).collect();
        let split = |out: &SculptMesh| {
            let (mut up, mut down) = (0, 0);
            for q in out.indices.chunks_exact(6) {
                let c = (out.positions[q[0] as usize] + out.positions[q[1] as usize] + out.positions[q[2] as usize]) / 3.0;
                if c.y > 0.2 { up += 1 } else if c.y < -0.2 { down += 1 }
            }
            up as f32 / down.max(1) as f32
        };
        let base = Settings { target_k: 1.0, keep_groups: false, adaptive: 0.0, color_density: 4.0, ..Settings::default() };
        let (plain, _) = zremesh(&m, &base, &[]).unwrap();
        let (dense, _) = zremesh(&m, &Settings { use_polypaint: true, ..base }, &[]).unwrap();
        let (r_plain, r_dense) = (split(&plain), split(&dense));
        eprintln!("red half / other half: {r_plain} plain, {r_dense} with polypaint; {} vs {} quads", plain.triangle_count() / 2, dense.triangle_count() / 2);
        assert!((r_plain - 1.0).abs() < 0.25, "{r_plain}");
        assert!(r_dense > 2.2, "{r_dense}");
        // Blue asks for fewer.
        m.colors = m.positions.iter().map(|p| if p.y > 0.0 { [20, 20, 230, 255] } else { [0; 4] }).collect();
        let (sparse, _) = zremesh(&m, &Settings { use_polypaint: true, ..base }, &[]).unwrap();
        assert!(split(&sparse) < 0.6, "{}", split(&sparse));
        // The total stays about the target.
        let total = dense.triangle_count() / 2;
        assert!((600..1700).contains(&total), "{total}");
    }

    #[test]
    fn symmetric_remeshing_mirrors_with_a_loop_on_the_plane() {
        // An egg off-centre in Y and Z, so the plane is the only symmetry.
        let (p, i) = crate::mesh::cube_sphere(40, 1.0);
        let p: Vec<Vec3> = p.into_iter().map(|q| Vec3::new(q.x, q.y * 1.3 + 0.2 * q.y * q.y, q.z + 0.1)).collect();
        let m = SculptMesh::from_raw(p, i);
        let s = Settings { target_k: 1.0, keep_groups: false, adaptive: 0.0, mirror: Some(TEST_X_PLANE), ..Settings::default() };
        let (out, stats) = zremesh(&m, &s, &[]).unwrap();
        eprintln!("{stats:?}");
        let quads = out.triangle_count() / 2;
        assert!((600..1700).contains(&quads), "{quads} quads");
        // Every vertex has its mirror image.
        let key = |p: Vec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
        let all: std::collections::HashSet<_> = out.positions.iter().map(|&p| key(p)).collect();
        let lonely = out.positions.iter().filter(|p| !all.contains(&key(Vec3::new(-p.x, p.y, p.z)))).count();
        assert_eq!(lonely, 0, "vertices without a mirror image");
        // A loop on the plane, and the mesh closed across it.
        let on = out.positions.iter().filter(|p| p.x == 0.0).count();
        let quad = (4.0 * std::f32::consts::PI * 1.3 / 1000.0).sqrt();
        assert!(on as f32 > 5.0 / quad, "{on} vertices on the plane");
        let counts = edge_counts(&out);
        let open = counts.values().filter(|&&k| k != 2).count();
        assert_eq!(open, 0, "{open} edges not between two triangles");
    }

    #[test]
    #[ignore]
    fn debug_sphere() {
        for k in [1.0f32, 4.0, 16.0] {
            let m = sphere(60);
            let r = zremesh(&m, &Settings { target_k: k, keep_groups: false, adaptive: 0.0, ..Settings::default() }, &[]);
            eprintln!("{k}: {:?}", r.map(|x| x.1));
        }
    }

    #[test]
    #[ignore]
    fn debug_freeze_dome() {
        // KLAY_OBJ=path cargo test --release debug_freeze_dome -- --ignored
        let s = sphere(30);
        let dome = crate::subtool_ops::part(&s, |t| {
            let i = 3 * t as usize;
            (s.positions[s.indices[i] as usize] + s.positions[s.indices[i + 1] as usize] + s.positions[s.indices[i + 2] as usize]).y > 0.0
        })
        .unwrap();
        let sym = std::env::var("KLAY_SYM").is_ok();
        let (out, _) = zremesh(&dome, &Settings { target_k: 0.5, keep_groups: false, adaptive: 0.0, freeze_border: true, mirror: sym.then_some(TEST_X_PLANE), ..Settings::default() }, &[]).unwrap();
        let mut f = String::new();
        for p in &out.positions {
            f += &format!("v {} {} {}\n", p.x, p.y, p.z);
        }
        for t in out.indices.chunks_exact(3) {
            f += &format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1);
        }
        std::fs::write(std::env::var("KLAY_OBJ").unwrap(), f).unwrap();
    }

    #[test]
    #[ignore]
    fn debug_plane() {
        let (p, i) = crate::primitive::Primitive::Plane.build(60, 1.0);
        let m = SculptMesh::from_raw(p, i);
        let r = zremesh(&m, &Settings { target_k: 0.4, keep_groups: false, adaptive: 0.0, ..Settings::default() }, &[]);
        eprintln!("{:?}", r.map(|x| x.1));
    }
}

#[cfg(test)]
mod manifold_tests {
    use super::*;

    /// Every triangle has three distinct corners, every incident triangle holds its vertex,
    /// and the neighbour lists match the triangles.
    fn check(m: &SculptMesh, what: &str) {
        assert_eq!(m.subdivision_blocker(), None, "{what}");
        for v in 0..m.vertex_count() as u32 {
            for &t in m.incident(v) {
                let tri = &m.indices[3 * t as usize..3 * t as usize + 3];
                assert!(tri.contains(&v), "{what}: vertex {v} incident to {t} {tri:?} which lacks it");
                for &w in tri {
                    assert!(w == v || m.neighbors(v).contains(&w), "{what}: {v} lacks neighbour {w}");
                }
            }
        }
    }

    fn remesh_paint_divide(label: &str, m: SculptMesh) {
        check(&m, &format!("{label}: input"));
        for target_k in [0.5f32, 2.0] {
            let s = Settings { target_k, ..Settings::default() };
            let (out, _) = zremesh(&m, &s, &[]).unwrap();
            check(&out, &format!("{label} k={target_k}: remeshed"));
            let mut doc = crate::multires::Multires::new(out, usize::MAX);
            crate::paint::ensure(doc.mesh_mut());
            assert!(doc.divide(), "{label} k={target_k}: divide");
            check(doc.mesh(), &format!("{label} k={target_k}: divided"));
        }
    }

    /// A plane with a polygon pulled out by ZModeler has open borders that fold through
    /// 90°; the remesh of it used to give a border vertex a face on its back too, and the
    /// two quads on the same three vertices crashed Divide.
    #[test]
    fn zmodeled_planes_remesh_two_manifold_and_divide() {
        for seg in [2u32, 8] {
            let (p, i) = crate::primitive::Primitive::Plane.build(seg, 1.0);
            let m = SculptMesh::from_raw(p.clone(), i.clone());
            remesh_paint_divide(&format!("plane {seg}"), SculptMesh::from_raw(p, i));
            let (pm, _) = crate::zmodeler::PolyMesh::from_mesh(&m);
            let f = pm.faces.len() as u32 / 2;
            let all: Vec<u32> = (0..pm.faces.len() as u32).collect();
            for (name, poly) in [("extrude", pm.extrude(&[f], 0.3)), ("extrude all", pm.extrude(&all, 0.3)), ("inset", pm.inset(&[f], 0.3)), ("delete", pm.delete(&[f]))] {
                let (m2, _, _) = poly.to_mesh();
                remesh_paint_divide(&format!("plane {seg} {name}"), m2);
            }
        }
    }
}

#[cfg(test)]
mod quad_tests {
    use super::*;

    #[test]
    fn the_result_knows_its_quads_whatever_their_shape_and_alt_lays_them_out_differently() {
        let (p, i) = crate::mesh::cube_sphere(20, 1.0);
        let m = SculptMesh::from_raw(p, i);
        let s = Settings { target_k: 0.3, keep_groups: false, adaptive: 0.0, ..Settings::default() };
        let (out, stats) = zremesh(&m, &s, &[]).unwrap();
        let lone = out.quads.iter().filter(|&&q| q == crate::mesh::LONE).count();
        assert_eq!(crate::mesh::quad_tests::all_paired(&out), out.triangle_count() - lone);
        assert_eq!(out.triangle_count(), 2 * stats.quads + lone);
        // ZModeler sees exactly those quads, big and skewed as they are (guessing from edge
        // lengths misses some of them).
        let (pm, _) = crate::zmodeler::PolyMesh::from_mesh(&out);
        assert_eq!(pm.faces.len(), stats.quads + lone);
        let guessed = crate::zmodeler::PolyMesh::from_mesh(&SculptMesh::from_raw(out.positions.clone(), out.indices.clone())).0;
        eprintln!("{} quads; {} faces guessed from edge lengths", stats.quads, guessed.faces.len());
        assert!(guessed.faces.len() > pm.faces.len(), "the guess leaves triangles");
        // Symmetric: the mirrored half keeps its quads too.
        let (sym, st) = zremesh(&m, &Settings { mirror: Some(TEST_X_PLANE), ..s }, &[]).unwrap();
        let lone = sym.quads.iter().filter(|&&q| q == crate::mesh::LONE).count();
        assert_eq!(crate::mesh::quad_tests::all_paired(&sym), sym.triangle_count() - lone);
        assert_eq!(crate::zmodeler::PolyMesh::from_mesh(&sym).0.faces.len(), st.quads + lone);
        // Alt: another layout.
        let (alt, _) = zremesh(&m, &Settings { alternate: true, ..s }, &[]).unwrap();
        let key = |m: &SculptMesh| {
            let mut k: Vec<[i64; 3]> = m.positions.iter().map(|p| (*p * 1e3).round().to_array().map(|c| c as i64)).collect();
            k.sort_unstable();
            k
        };
        assert_ne!(key(&alt), key(&out), "Alt lays the loops out differently");
    }
}
