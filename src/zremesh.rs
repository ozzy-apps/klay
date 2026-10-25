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

use crate::mesh::SculptMesh;
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
}

impl Default for Settings {
    fn default() -> Self {
        Self { target_k: 5.0, keep_groups: true }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub samples: usize,
    pub levels: usize,
    pub quads: usize,
    /// Faces other than quads before the split, and holes closed.
    pub odd_faces: usize,
    pub holes: usize,
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
    for i in 0..g.len() {
        let key = cell_key(g.p[i], g.n[i], inv);
        let k = *index.entry(key).or_insert_with(|| {
            p.push(Vec3::ZERO);
            n.push(Vec3::ZERO);
            held.push(Vec3::ZERO);
            count.push(0.0);
            (p.len() - 1) as u32
        });
        parent.push(k);
        let k = k as usize;
        p[k] += g.p[i];
        n[k] += g.n[i];
        count[k] += 1.0;
        if held[k] == Vec3::ZERO {
            held[k] = g.held[i];
        }
    }
    for k in 0..p.len() {
        p[k] /= count[k];
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
    let graph = graph_from(p, n, held, edges);
    (graph, parent)
}

fn graph_from(p: Vec<Vec3>, n: Vec<Vec3>, held: Vec<Vec3>, mut edges: Vec<(u32, u32)>) -> Graph {
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
    Graph { p, n, off, adj, held }
}

/// The sample graph of `mesh`'s vertices: one sample per vertex, neighbours along edges.
/// Directions are held along open borders, and with `groups` along polygroup borders.
fn vertex_graph(positions: &[Vec3], indices: &[u32], groups: &[u16]) -> Graph {
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
    graph_from(positions.to_vec(), n, held, edges)
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
fn compat_position(p0: Vec3, n0: Vec3, q0: Vec3, o0: Vec3, p1: Vec3, n1: Vec3, q1: Vec3, o1: Vec3, scale: f32) -> (Vec3, Vec3) {
    let (t0, t1) = (n0.cross(q0), n1.cross(q1));
    let mid = middle_point(p0, n0, p1, n1);
    let a = floor4(o0, q0, n0, mid, scale);
    let b = floor4(o1, q1, n1, mid, scale);
    let (mut best, mut bi, mut bj) = (f32::INFINITY, 0, 0);
    for i in 0..4 {
        let x = a + (q0 * (i & 1) as f32 + t0 * ((i >> 1) & 1) as f32) * scale;
        for j in 0..4 {
            let y = b + (q1 * (j & 1) as f32 + t1 * ((j >> 1) & 1) as f32) * scale;
            let c = x.distance_squared(y);
            if c < best {
                best = c;
                bi = i;
                bj = j;
            }
        }
    }
    (
        a + (q0 * (bi & 1) as f32 + t0 * ((bi >> 1) & 1) as f32) * scale,
        b + (q1 * (bj & 1) as f32 + t1 * ((bj >> 1) & 1) as f32) * scale,
    )
}

/// Smooth the direction field `q` of `g`, `sweeps` times.
fn smooth_directions(g: &Graph, q: &mut [Vec3], colours: &[Vec<u32>], sweeps: usize) {
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
                    (i as u32, sum)
                })
                .collect();
            for (i, v) in new {
                q[i as usize] = v;
            }
        }
    }
}

/// Smooth the lattice points `o` of `g` (directions `q`, step `scale`), `sweeps` times.
fn smooth_positions(g: &Graph, q: &[Vec3], o: &mut [Vec3], colours: &[Vec<u32>], sweeps: usize, scale: f32) {
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
                        let (a, b) = compat_position(p, n, qi, sum, g.p[j], g.n[j], qj, o[j], scale);
                        sum = (a * w + b) / (w + 1.0);
                        w += 1.0;
                        sum -= n * n.dot(sum - p);
                    }
                    (i as u32, round4(sum, q[i], n, p, scale))
                })
                .collect();
            for (i, v) in new {
                o[i as usize] = v;
            }
        }
    }
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
fn subdivide(positions: &[Vec3], indices: &[u32], groups: &[u16]) -> (Vec<Vec3>, Vec<u32>, Vec<u16>) {
    let mut p = positions.to_vec();
    let mut mid: HashMap<(u32, u32), u32> = HashMap::default();
    let mut out = Vec::with_capacity(indices.len() * 4);
    let mut g = Vec::with_capacity(groups.len() * 4);
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let mut m = [0u32; 3];
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            m[k] = *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
                p.push((positions[a as usize] + positions[b as usize]) * 0.5);
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

/// ZRemesh `mesh` into about `s.target_k` thousand quads.
pub fn zremesh(mesh: &SculptMesh, s: &Settings) -> Result<(SculptMesh, Stats), String> {
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
    let cell = scale / 4.0;
    for _ in 0..5 {
        let edge: f32 = idx.chunks_exact(3).step_by((idx.len() / 3 / 4096).max(1)).map(|t| pos[t[0] as usize].distance(pos[t[1] as usize])).sum::<f32>()
            / (idx.len() / 3).div_ceil((idx.len() / 3 / 4096).max(1)).max(1) as f32;
        if edge <= cell || idx.len() / 3 > 4_000_000 {
            break;
        }
        (pos, idx, grp) = subdivide(&pos, &idx, &grp);
    }
    let fine = vertex_graph(&pos, &idx, &grp);
    let (g0, _) = coarsen(&fine, cell);
    drop(fine);
    stats.samples = g0.len();
    let mut levels: Vec<(Graph, Vec<u32>)> = vec![(g0, Vec::new())];
    let mut c = cell;
    while levels.last().unwrap().0.len() > 400 && levels.len() < 14 {
        c *= 2.0;
        let (coarse, parent) = coarsen(&levels.last().unwrap().0, c);
        if coarse.len() as f32 > levels.last().unwrap().0.len() as f32 * 0.8 {
            break;
        }
        levels.last_mut().unwrap().1 = parent;
        levels.push((coarse, Vec::new()));
    }
    stats.levels = levels.len();
    let colours: Vec<Vec<Vec<u32>>> = levels.iter().map(|(g, _)| g.colours()).collect();

    // Smoothing passes per level (more on the finest, which the quads come from).
    let sweeps = |l: usize| -> usize { if l == 0 { 12 } else { 8 } };
    // 2. Directions, coarse to fine.
    let top = levels.len() - 1;
    let mut q: Vec<Vec3> = levels[top].0.n.iter().map(|n| n.any_orthonormal_vector()).collect();
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
        smooth_directions(g, &mut q, &colours[l], sweeps(l));
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
        smooth_positions(g, &qs[l], &mut o, &colours[l], sweeps(l), scale);
    }
    let g = &levels[0].0;
    let q = &qs[0];

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
            let (a, b) = compat_position(g.p[i], g.n[i], qi, o[i], g.p[j], g.n[j], qj, o[j], scale);
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
    for i in 0..m {
        let r = find(&mut uf, i as u32) as usize;
        if vid[r] == u32::MAX {
            vid[r] = vp.len() as u32;
            vp.push(Vec3::ZERO);
            vn.push(Vec3::ZERO);
            vq.push(q[r]);
            cnt.push(0.0);
        }
        let k = vid[r] as usize;
        vid[i] = vid[r];
        vp[k] += o[i];
        vn[k] += g.n[i];
        cnt[k] += 1.0;
    }
    for k in 0..vp.len() {
        vp[k] /= cnt[k];
        vn[k] = vn[k].normalize_or(Vec3::Z);
    }
    // One lattice point split into nearby vertices is joined again.
    {
        let k = vp.len();
        let mut uf2: Vec<u32> = (0..k as u32).collect();
        let inv = 1.0 / (scale * 0.4);
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
                                if (w as usize) > v && vp[v].distance(vp[w as usize]) < scale * 0.4 && vn[v].dot(vn[w as usize]) > 0.5 {
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
        let (mut np, mut nn, mut nq, mut nc): (Vec<Vec3>, Vec<Vec3>, Vec<Vec3>, Vec<f32>) = Default::default();
        for v in 0..k {
            let r = find(&mut uf2, v as u32) as usize;
            if remap[r] == u32::MAX {
                remap[r] = np.len() as u32;
                np.push(Vec3::ZERO);
                nn.push(Vec3::ZERO);
                nq.push(vq[r]);
                nc.push(0.0);
            }
            let t = remap[r] as usize;
            remap[v] = remap[r];
            np[t] += vp[v] * cnt[v];
            nn[t] += vn[v] * cnt[v];
            nc[t] += cnt[v];
        }
        for t in 0..np.len() {
            np[t] /= nc[t];
            nn[t] = nn[t].normalize_or(Vec3::Z);
        }
        for x in vid.iter_mut() {
            *x = remap[*x as usize];
        }
        (vp, vn, vq) = (np, nn, nq);
    }
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); vp.len()];
    for &(i, j) in &steps {
        let (a, b) = (vid[i as usize], vid[j as usize]);
        // A step is one lattice edge long; far longer ones are strays.
        if a != b && !nb[a as usize].contains(&b) && vp[a as usize].distance(vp[b as usize]) < scale * 2.0 {
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
    stats.quads = quads.len();

    // Onto the old surface, then even spacing along it.
    let reach = scale * 1.5;
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
            if !adj[a as usize].contains(&b) {
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
                    if a.is_empty() {
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
                out_p[v] = p;
            }
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
            .filter(|&v| border[v])
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
            if around[v].len() >= 3 {
                if let Some(c) = corners.iter().copied().min_by(|a, b| a.distance_squared(out_p[v]).total_cmp(&b.distance_squared(out_p[v]))) {
                    if c.distance(out_p[v]) < quad * 1.5 {
                        out_p[v] = c;
                    }
                }
            }
        }
    }

    // Triangles (each quad along its shorter diagonal).
    let mut indices = Vec::with_capacity(quads.len() * 6);
    let mut tri_groups = Vec::with_capacity(quads.len() * 2);
    for (q4, &g) in quads.iter().zip(&quad_group) {
        let p = |v: u32| out_p[v as usize];
        if p(q4[0]).distance_squared(p(q4[2])) <= p(q4[1]).distance_squared(p(q4[3])) {
            indices.extend_from_slice(&[q4[0], q4[1], q4[2], q4[0], q4[2], q4[3]]);
        } else {
            indices.extend_from_slice(&[q4[0], q4[1], q4[3], q4[1], q4[2], q4[3]]);
        }
        tri_groups.extend_from_slice(&[g, g]);
    }
    let out = SculptMesh::from_raw_with_groups(out_p, indices, if grouped { tri_groups } else { Vec::new() });
    stats.ms = start.elapsed().as_secs_f32() * 1000.0;
    Ok((out, stats))
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

    #[test]
    fn a_sphere_becomes_even_quads_near_the_target() {
        // A dense, uneven sphere: squash its vertices toward the poles first.
        let m = sphere(40);
        let (out, stats) = zremesh(&m, &Settings { target_k: 1.0, keep_groups: false }).unwrap();
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
    #[ignore]
    fn debug_sphere() {
        for k in [1.0f32, 4.0, 16.0] {
            let m = sphere(60);
            let r = zremesh(&m, &Settings { target_k: k, keep_groups: false });
            eprintln!("{k}: {:?}", r.map(|x| x.1));
        }
    }

    #[test]
    #[ignore]
    fn debug_plane() {
        let (p, i) = crate::primitive::Primitive::Plane.build(60, 1.0);
        let m = SculptMesh::from_raw(p, i);
        let r = zremesh(&m, &Settings { target_k: 0.4, keep_groups: false });
        eprintln!("{:?}", r.map(|x| x.1));
    }
}
