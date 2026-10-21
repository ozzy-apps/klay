//! Wedge: a DynaMesh-style remesher (wedging clay kneads it to an even consistency).
//!
//! The active subtool's surface is turned into an even, closed, manifold mesh at a chosen
//! voxel resolution:
//!
//! 1. **Scan.** Along each of the three axes, every triangle is rasterised onto the lines
//!    of a regular grid with exact, watertight edge tests (shared edges are evaluated the
//!    same way from both sides, ties broken by one fixed rule), giving the depth where each
//!    line crosses the surface and on which side. Summing those signs along a line is the
//!    winding number, so overlapping or self-intersecting shells merge into their union;
//!    only the points where the winding number changes between zero and non-zero are kept.
//!    Memory follows the surface, not the volume: a line stores only its crossings.
//! 2. **Cells.** A grid point is inside when an odd number of crossings of its z line lie
//!    below it. Walking the crossings of four neighbouring z lines together gives every
//!    cell whose eight corners disagree, column by column, with no volume pass.
//! 3. **Topology.** A cell face whose corners alternate inside / outside (a checkerboard)
//!    is ambiguous and could give a non-manifold result, so one of its outside corners is
//!    turned inside, until no such face is left.
//! 4. **Surface.** Each cell holds one vertex per separate patch of surface through it
//!    (dual marching cubes, which with no ambiguous face makes a manifold), placed at the
//!    mean of its patch's edge crossings, which lie exactly on the old surface. Every grid
//!    edge the surface crosses gives a quad joining the vertices of its four cells, split
//!    along its shorter diagonal.
//!    Each vertex then slides along its patch's normal to fit the tangent planes of the old
//!    triangles at its crossings, so curved surfaces come out smooth.
//! 5. **Groups, Blur, Project.** A quad takes the polygroup of the old triangle its edge
//!    crossed, and vertices where groups meet move onto the old group border. With Project
//!    the new vertices are smoothed `blur` times and then moved to the closest point of the
//!    old surface, found by walking the old mesh from the triangle the vertex came from
//!    (ZBrush: Blur only acts with Project).
//!
//! Everything is parallel over triangles, lines or cells with rayon.

use crate::mesh::SculptMesh;
use glam::Vec3;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::Instant;

/// Wedge settings (ZBrush: Tool > Geometry > DynaMesh).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Voxels across two world units, the size of a new project's primitive (ZBrush's
    /// Resolution is relative to the tool's size the same way: a bigger model gets more
    /// polygons at the same value).
    pub resolution: u32,
    /// Smoothing passes before projecting; only used with `project`.
    pub blur: u32,
    /// Move the new vertices onto the old surface, keeping detail down to the voxel size.
    pub project: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            resolution: 128,
            blur: 2,
            project: false,
        }
    }
}

impl Settings {
    /// Edge length of a voxel, world units.
    pub fn voxel(&self) -> f32 {
        2.0 / self.resolution.max(1) as f32
    }

    /// About how many vertices the result will have for a surface of `area`: one per cell
    /// the surface passes through, about 1.5 per voxel face of area on average (the mean of
    /// |nx| + |ny| + |nz| over all directions).
    pub fn estimate(&self, area: f32) -> usize {
        (1.5 * area / (self.voxel() * self.voxel())) as usize
    }
}

/// Highest resolution offered (ZBrush goes to 4096 as well).
pub const MAX_RESOLUTION: u32 = 4096;
/// Grid points along one axis Wedge accepts.
const MAX_POINTS: u64 = 1 << 20;

pub struct Output {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Per triangle; empty when the input had no polygroups.
    pub groups: Vec<u16>,
    pub stats: Stats,
}

#[derive(Default, Debug, Clone)]
pub struct Stats {
    /// Grid points along x, y, z.
    pub grid: [u32; 3],
    pub crossings: usize,
    pub cells: usize,
    /// Corners turned inside to remove ambiguous faces.
    pub flips: usize,
    pub scan_ms: f32,
    pub cells_ms: f32,
    pub surface_ms: f32,
    /// Blur, Project and polygroup borders.
    pub project_ms: f32,
    /// Vertices on a polygroup border, moved onto the old border.
    pub border_vertices: usize,
}

/// Whether every edge of `mesh` lies on exactly two triangles (Wedge needs a solid).
pub fn closed(mesh: &SculptMesh) -> bool {
    (0..mesh.vertex_count() as u32).into_par_iter().all(|v| {
        mesh.neighbors(v).iter().filter(|&&w| w > v).all(|&w| {
            mesh.incident(v)
                .iter()
                .filter(|&&t| mesh.indices[3 * t as usize..3 * t as usize + 3].contains(&w))
                .count()
                == 2
        })
    })
}

/// Surface area of a mesh (for the vertex estimate).
pub fn area(mesh: &SculptMesh) -> f32 {
    mesh.indices
        .par_chunks_exact(3)
        .map(|t| {
            let p = |k: usize| mesh.positions[t[k] as usize];
            (p(1) - p(0)).cross(p(2) - p(0)).length() * 0.5
        })
        .sum()
}

/// The regular grid: point `(i, j, k)` sits at `origin + h (i, j, k)`.
#[derive(Clone, Copy, Debug)]
struct Grid {
    origin: [f64; 3],
    h: f64,
    inv_h: f64,
    n: [usize; 3],
}

impl Grid {
    fn around(mesh: &SculptMesh, voxel: f32) -> Result<Self, String> {
        let b = mesh.bounds();
        let h = voxel as f64;
        // Two voxels of padding, and an offset that keeps grid lines off axis-aligned
        // faces such as a cube primitive's (they would make every test a tie).
        let pad = 2.0 + 0.371_337;
        let origin = [0, 1, 2].map(|a| b.min[a] as f64 - pad * h);
        let n = [0, 1, 2].map(|a| (((b.max[a] - b.min[a]) as f64 / h).ceil() as u64 + 6) as usize);
        if n.iter().any(|&m| m as u64 > MAX_POINTS / 64) || (n[0] as u64 * n[1] as u64) > MAX_POINTS * 16 {
            return Err(format!(
                "a grid of {} x {} x {} is too large · lower the resolution",
                n[0], n[1], n[2]
            ));
        }
        Ok(Self { origin, h, inv_h: 1.0 / h, n })
    }

    /// `p` in grid units.
    fn local(&self, p: Vec3) -> [f64; 3] {
        [0, 1, 2].map(|a| (p[a] as f64 - self.origin[a]) * self.inv_h)
    }

    /// `p` in grid units, roughly (for quick rejection only).
    fn local_f32(&self, p: Vec3) -> Vec3 {
        (p - Vec3::new(self.origin[0] as f32, self.origin[1] as f32, self.origin[2] as f32)) * self.inv_h as f32
    }

    fn world(&self, g: [f64; 3]) -> Vec3 {
        Vec3::new(
            (self.origin[0] + g[0] * self.h) as f32,
            (self.origin[1] + g[1] * self.h) as f32,
            (self.origin[2] + g[2] * self.h) as f32,
        )
    }
}

/// Crossings of every grid line along one axis, in CSR form: line `l` crosses the surface
/// at depths `depth[start[l]..start[l + 1]]` (grid units, ascending), each made by triangle
/// `tri[..]`. Only changes between outside (winding zero) and inside are kept, so the
/// depths alternate entering and leaving.
struct Lines {
    start: Vec<u32>,
    depth: Vec<f32>,
    tri: Vec<u32>,
}

impl Lines {
    fn line(&self, l: usize) -> &[f32] {
        &self.depth[self.start[l] as usize..self.start[l + 1] as usize]
    }

    /// The crossing in `[lo, lo + 1)` of line `l`, if there is exactly one... or the middle
    /// one of an odd number: its depth and triangle.
    fn crossing(&self, l: usize, lo: usize) -> Option<(f32, u32)> {
        let s = self.start[l] as usize;
        let d = self.line(l);
        let a = d.partition_point(|&x| x < lo as f32);
        let b = d.partition_point(|&x| x < (lo + 1) as f32);
        if (b - a) % 2 == 1 {
            let m = a + (b - a) / 2;
            Some((d[m], self.tri[s + m]))
        } else {
            None
        }
    }
}

/// One surface crossing found by the scan: line and depth packed so a plain sort orders
/// by line, then depth (depths are non-negative, so their bits sort like the values).
#[derive(Clone, Copy)]
struct Hit {
    key: u64,
    tri: u32,
    sign: i32,
}

/// Rasterise every triangle onto the lines of all three axes in one pass over the
/// triangles, then keep each line's winding changes.
fn scan(mesh: &SculptMesh, grid: &Grid) -> [Lines; 3] {
    let parts: Vec<[Vec<Hit>; 3]> = mesh
        .indices
        .par_chunks(3 * 4096)
        .enumerate()
        .map(|(part, ix)| {
            let mut out: [Vec<Hit>; 3] = Default::default();
            for (t, tri) in ix.chunks_exact(3).enumerate() {
                let t = (part * 4096 + t) as u32;
                // Most triangles of a dense mesh cover no line at all: a rough bounding box,
                // widened so rounding can only let more through, rules them out cheaply.
                let q = [0, 1, 2].map(|k| grid.local_f32(mesh.positions[tri[k] as usize]));
                let lo = q[0].min(q[1]).min(q[2]) - 1e-3;
                let hi = q[0].max(q[1]).max(q[2]) + 1e-3;
                let spans = (hi.floor() - lo.ceil()).cmpge(Vec3::ZERO);
                let wanted = [spans.y && spans.z, spans.z && spans.x, spans.x && spans.y];
                if !wanted.iter().any(|&w| w) {
                    continue;
                }
                // Exact grid coordinates, computed the same way for every triangle that uses
                // the vertex, so shared edges give bit-identical edge functions.
                let v = [0, 1, 2].map(|k| grid.local(mesh.positions[tri[k] as usize]));
                for (axis, o) in out.iter_mut().enumerate() {
                    if wanted[axis] {
                        rasterise(&v, tri, t, axis, grid, o);
                    }
                }
            }
            out
        })
        .collect();
    let mut all: [Vec<Hit>; 3] = Default::default();
    for (axis, hits) in all.iter_mut().enumerate() {
        hits.reserve(parts.iter().map(|p| p[axis].len()).sum());
    }
    for p in parts {
        for (axis, h) in p.into_iter().enumerate() {
            all[axis].extend(h);
        }
    }
    all.map(|mut hits| {
        hits.par_sort_unstable_by_key(|h| h.key);
        hits
    })
    .into_iter()
    .enumerate()
    .map(|(axis, hits)| lines_of(hits, grid.n[(axis + 1) % 3] * grid.n[(axis + 2) % 3]))
    .collect::<Vec<_>>()
    .try_into()
    .unwrap_or_else(|_| unreachable!())
}

/// Per line: the running winding number of its sorted hits, keeping the steps between
/// zero and non-zero.
fn lines_of(hits: Vec<Hit>, lines: usize) -> Lines {
    // Where each line's hits start, found in parallel by binary search.
    let bounds: Vec<usize> = (0..=lines)
        .into_par_iter()
        .map(|l| hits.partition_point(|h| (h.key >> 32) < l as u64))
        .collect();
    let kept: Vec<Vec<(f32, u32)>> = (0..lines)
        .into_par_iter()
        .with_min_len(1024)
        .map(|l| {
            let mut out = Vec::new();
            let mut w = 0i32;
            for h in &hits[bounds[l]..bounds[l + 1]] {
                let was = w != 0;
                w += h.sign;
                if was != (w != 0) {
                    out.push((f32::from_bits(h.key as u32), h.tri));
                }
            }
            out
        })
        .collect();
    let mut start = Vec::with_capacity(lines + 1);
    start.push(0u32);
    let total = kept.iter().map(Vec::len).sum();
    let (mut depth, mut tri) = (Vec::with_capacity(total), Vec::with_capacity(total));
    for k in &kept {
        for &(d, t) in k {
            depth.push(d);
            tri.push(t);
        }
        start.push(depth.len() as u32);
    }
    Lines { start, depth, tri }
}

/// Edge function of the directed edge `i -> j` at `(x, y)` in the plane `(b, c)`,
/// evaluated on the edge in index order so both triangles sharing it compute the same
/// number, with an exact zero broken by a fixed rule (as if the point were nudged by
/// `(e, e^2)`): every line point on a shared edge lands in exactly one of its triangles.
/// `v` holds the triangle's three corners, `ids` their vertex ids, `i` and `j` are corner
/// slots (0..3).
#[inline]
fn edge(v: &[[f64; 3]; 3], ids: &[u32], i: usize, j: usize, x: f64, y: f64, b: usize, c: usize) -> f64 {
    let (lo, hi, flip) = if ids[i] < ids[j] { (i, j, 1.0) } else { (j, i, -1.0) };
    let (a, q) = (&v[lo], &v[hi]);
    let (dx, dy) = (q[b] - a[b], q[c] - a[c]);
    let mut e = dx * (y - a[c]) - dy * (x - a[b]);
    if e == 0.0 {
        e = if dy != 0.0 { -dy.signum() } else { dx.signum() } * f64::MIN_POSITIVE;
    }
    e * flip
}

/// Rasterise triangle `t` (corners `v` in grid units, vertex ids `tri`) onto the lines
/// along `axis`.
#[inline]
fn rasterise(v: &[[f64; 3]; 3], tri: &[u32], t: u32, axis: usize, grid: &Grid, out: &mut Vec<Hit>) {
    let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
    let (nb, nc) = (grid.n[b], grid.n[c]);
    let (v0, v1, v2) = (&v[0], &v[1], &v[2]);
    let area = (v1[b] - v0[b]) * (v2[c] - v0[c]) - (v1[c] - v0[c]) * (v2[b] - v0[b]);
    if area == 0.0 {
        // Parallel to the lines: its neighbours cover every line.
        return;
    }
    let lo_b = v0[b].min(v1[b]).min(v2[b]).ceil().max(0.0) as usize;
    let hi_b = (v0[b].max(v1[b]).max(v2[b]).floor() as i64).min(nb as i64 - 1);
    let lo_c = v0[c].min(v1[c]).min(v2[c]).ceil().max(0.0) as usize;
    let hi_c = (v0[c].max(v1[c]).max(v2[c]).floor() as i64).min(nc as i64 - 1);
    if hi_b < lo_b as i64 || hi_c < lo_c as i64 {
        return;
    }
    for y in lo_c..=hi_c as usize {
        for x in lo_b..=hi_b as usize {
            let (xf, yf) = (x as f64, y as f64);
            let e0 = edge(v, tri, 1, 2, xf, yf, b, c); // weight of corner 0
            let e1 = edge(v, tri, 2, 0, xf, yf, b, c); // weight of corner 1
            let e2 = edge(v, tri, 0, 1, xf, yf, b, c); // weight of corner 2
            // Counter-clockwise in (b, c) means the normal points along +axis: a ray in
            // that direction leaves the solid there.
            let sign = if e0 > 0.0 && e1 > 0.0 && e2 > 0.0 {
                -1
            } else if e0 < 0.0 && e1 < 0.0 && e2 < 0.0 {
                1
            } else {
                continue;
            };
            let s = e0 + e1 + e2;
            let d = ((e0 * v0[axis] + e1 * v1[axis] + e2 * v2[axis]) / s).max(0.0) as f32;
            let line = (x + y * nb) as u64;
            out.push(Hit {
                key: (line << 32) | d.to_bits() as u64,
                tri: t,
                sign,
            });
        }
    }
}

// ---------------------------------------------------------------- cell tables

/// Corner `c` of a cell is at offset `(c & 1, c >> 1 & 1, c >> 2 & 1)`.
/// Edges: 0-3 along x, 4-7 along y, 8-11 along z.
const EDGES: [(u8, u8); 12] = [
    (0, 1), (2, 3), (4, 5), (6, 7),
    (0, 2), (1, 3), (4, 6), (5, 7),
    (0, 4), (1, 5), (2, 6), (3, 7),
];
/// The four edges of each face and its four corners in cyclic order.
const FACES: [([u8; 4], [u8; 4]); 6] = [
    ([0, 5, 1, 4], [0, 1, 3, 2]),   // z = 0
    ([2, 7, 3, 6], [4, 5, 7, 6]),   // z = 1
    ([0, 9, 2, 8], [0, 1, 5, 4]),   // y = 0
    ([1, 11, 3, 10], [2, 3, 7, 6]), // y = 1
    ([4, 10, 6, 8], [0, 2, 6, 4]),  // x = 0
    ([5, 11, 7, 9], [1, 3, 7, 5]),  // x = 1
];

/// Per corner configuration: the surface patch each edge belongs to (255 for edges the
/// surface does not cross), the number of patches, and whether a face is ambiguous.
struct Table {
    patch: [[u8; 12]; 256],
    patches: [u8; 256],
    ambiguous: [u8; 256],
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let mut t = Table {
        patch: [[255; 12]; 256],
        patches: [0; 256],
        ambiguous: [0; 256],
    };
    for cfg in 0..256usize {
        let inside = |c: u8| cfg >> c & 1 == 1;
        let crosses = |e: usize| inside(EDGES[e].0) != inside(EDGES[e].1);
        let mut parent: [usize; 12] = std::array::from_fn(|i| i);
        fn find(p: &mut [usize; 12], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        for (fi, (edges, corners)) in FACES.iter().enumerate() {
            let cross: Vec<usize> = edges.iter().map(|&e| e as usize).filter(|&e| crosses(e)).collect();
            if cross.len() == 2 {
                let (a, b) = (find(&mut parent, cross[0]), find(&mut parent, cross[1]));
                parent[a] = b;
            } else if cross.len() == 4 {
                t.ambiguous[cfg] |= 1 << fi;
                // Not used after the topology pass; pair the edges around inside corners.
                for k in 0..4 {
                    if inside(corners[k]) {
                        let (e_prev, e_next) = (edges[(k + 3) % 4] as usize, edges[k] as usize);
                        let (a, b) = (find(&mut parent, e_prev), find(&mut parent, e_next));
                        parent[a] = b;
                    }
                }
            }
        }
        let mut ids = [255u8; 12];
        let mut next = 0u8;
        for e in 0..12 {
            if crosses(e) {
                let r = find(&mut parent, e);
                if ids[r] == 255 {
                    ids[r] = next;
                    next += 1;
                }
                t.patch[cfg][e] = ids[r];
            }
        }
        t.patches[cfg] = next;
    }
    t
});

// ---------------------------------------------------------------- the field

/// Inside / outside at grid points, from the z lines and the corners turned inside.
struct Field<'a> {
    grid: Grid,
    z: &'a Lines,
    /// Corners turned inside by the topology pass, by z line.
    flips: HashMap<usize, Vec<u32>>,
}

impl Field<'_> {
    fn column(&self, i: usize, j: usize) -> usize {
        i + j * self.grid.n[0]
    }

    fn inside(&self, i: usize, j: usize, k: usize) -> bool {
        let col = self.column(i, j);
        if let Some(f) = self.flips.get(&col) {
            if f.contains(&(k as u32)) {
                return true;
            }
        }
        self.z.line(col).partition_point(|&d| d < k as f32) % 2 == 1
    }

    /// Cells of the cell column `(i, j)` whose corners disagree, with their corner
    /// configuration, appended as `(k, config)`.
    fn cell_column(&self, i: usize, j: usize, out: &mut Vec<(u32, u8)>) {
        let nz = self.grid.n[2];
        let cols = [self.column(i, j), self.column(i + 1, j), self.column(i, j + 1), self.column(i + 1, j + 1)];
        let flipped = cols.iter().any(|c| self.flips.contains_key(c));
        if !flipped && cols.iter().all(|&c| self.z.line(c).is_empty()) {
            return;
        }
        if flipped {
            // Rare: evaluate every layer.
            let layer = |k: usize| -> u8 {
                (self.inside(i, j, k) as u8)
                    | (self.inside(i + 1, j, k) as u8) << 1
                    | (self.inside(i, j + 1, k) as u8) << 2
                    | (self.inside(i + 1, j + 1, k) as u8) << 3
            };
            let mut below = layer(0);
            for k in 0..nz - 1 {
                let above = layer(k + 1);
                let cfg = below | above << 4;
                if cfg != 0 && cfg != 255 {
                    out.push((k as u32, cfg));
                }
                below = above;
            }
            return;
        }
        // Runs of point layers with the same pattern of the four columns.
        let mut ev: Vec<(f32, u8)> = Vec::new();
        for (bit, &c) in cols.iter().enumerate() {
            ev.extend(self.z.line(c).iter().map(|&d| (d, bit as u8)));
        }
        ev.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut runs: Vec<(i64, i64, u8)> = Vec::new();
        let push = |runs: &mut Vec<(i64, i64, u8)>, a: i64, b: i64, p: u8| {
            let (a, b) = (a.max(0), b.min(nz as i64 - 1));
            if a > b {
                return;
            }
            match runs.last_mut() {
                Some(last) if last.2 == p => last.1 = b,
                _ => runs.push((a, b, p)),
            }
        };
        let (mut from, mut p) = (0i64, 0u8);
        for &(d, bit) in &ev {
            // The state before this crossing holds for layers k <= floor(d).
            let to = d.floor() as i64;
            push(&mut runs, from, to, p);
            from = from.max(to + 1);
            p ^= 1 << bit;
        }
        push(&mut runs, from, nz as i64 - 1, p);
        let uniform = |p: u8| p == 0 || p == 15;
        let mut last = -1i64;
        for (r, &(a, b, p)) in runs.iter().enumerate() {
            let next = runs.get(r + 1).map(|n| n.2);
            let (lo, hi) = if !uniform(p) {
                (a - 1, b)
            } else if next.is_some_and(|q| q != p) {
                (b, b)
            } else {
                continue;
            };
            for k in lo.max(last + 1).max(0)..=hi.min(nz as i64 - 2) {
                let below = if k >= a { p } else { runs[r - 1].2 };
                let above = if k + 1 <= b { p } else { next.unwrap_or(0) };
                let cfg = below | above << 4;
                if cfg != 0 && cfg != 255 {
                    out.push((k as u32, cfg));
                }
                last = k;
            }
        }
    }

    /// Every cell the surface passes through, sorted by key, with its configuration.
    fn cells(&self) -> (Vec<u64>, Vec<u8>) {
        let [nx, ny, nz] = self.grid.n;
        let rows: Vec<(Vec<u64>, Vec<u8>)> = (0..ny - 1)
            .into_par_iter()
            .map(|j| {
                let (mut keys, mut cfgs) = (Vec::new(), Vec::new());
                let mut buf = Vec::new();
                for i in 0..nx - 1 {
                    buf.clear();
                    self.cell_column(i, j, &mut buf);
                    let base = ((j * nx + i) * nz) as u64;
                    for &(k, cfg) in &buf {
                        keys.push(base + k as u64);
                        cfgs.push(cfg);
                    }
                }
                (keys, cfgs)
            })
            .collect();
        let total = rows.iter().map(|r| r.0.len()).sum();
        let (mut keys, mut cfgs) = (Vec::with_capacity(total), Vec::with_capacity(total));
        for (k, c) in rows {
            keys.extend(k);
            cfgs.extend(c);
        }
        (keys, cfgs)
    }

    fn key(&self, i: usize, j: usize, k: usize) -> u64 {
        ((j * self.grid.n[0] + i) * self.grid.n[2] + k) as u64
    }

    fn unkey(&self, key: u64) -> (usize, usize, usize) {
        let [nx, _, nz] = self.grid.n;
        let k = (key % nz as u64) as usize;
        let col = (key / nz as u64) as usize;
        (col % nx, col / nx, k)
    }
}

/// Remesh `mesh` (closed) at `s.resolution`, carrying its polygroups over.
pub fn remesh(mesh: &SculptMesh, s: &Settings) -> Result<Output, String> {
    let mut stats = Stats::default();
    let grid = Grid::around(mesh, s.voxel())?;
    stats.grid = grid.n.map(|n| n as u32);

    // 1. Scans along the three axes.
    let t = Instant::now();
    let [x, y, z] = scan(mesh, &grid);
    stats.crossings = x.depth.len() + y.depth.len() + z.depth.len();
    stats.scan_ms = t.elapsed().as_secs_f32() * 1000.0;
    if z.depth.is_empty() {
        return Err("the surface is smaller than one voxel · raise the resolution".into());
    }

    // 2–3. Cells, then corners turned inside until no face is ambiguous.
    let t = Instant::now();
    let mut field = Field {
        grid,
        z: &z,
        flips: HashMap::new(),
    };
    let table = &*TABLE;
    let (keys, cfgs) = loop {
        let (keys, cfgs) = field.cells();
        let flips: HashSet<(usize, usize, usize)> = keys
            .par_iter()
            .zip(cfgs.par_iter())
            .filter(|(_, c)| table.ambiguous[**c as usize] != 0)
            .flat_map_iter(|(&key, &cfg)| {
                let (i, j, k) = field.unkey(key);
                let mut out = Vec::new();
                for (fi, (_, corners)) in FACES.iter().enumerate() {
                    if table.ambiguous[cfg as usize] >> fi & 1 == 1 {
                        // The first outside corner of the face, in a fixed order.
                        let c = corners.iter().copied().find(|&c| cfg >> c & 1 == 0).unwrap();
                        out.push((i + (c & 1) as usize, j + (c >> 1 & 1) as usize, k + (c >> 2 & 1) as usize));
                    }
                }
                out
            })
            .collect();
        if flips.is_empty() {
            break (keys, cfgs);
        }
        stats.flips += flips.len();
        for (i, j, k) in flips {
            let col = field.column(i, j);
            field.flips.entry(col).or_default().push(k as u32);
        }
    };
    stats.cells = keys.len();
    stats.cells_ms = t.elapsed().as_secs_f32() * 1000.0;

    // 4. Vertices: one per patch per cell, at the mean of the patch's crossings.
    let t = Instant::now();
    let h = grid.h as f32;
    let [nx, ny, nz] = grid.n;
    let mut first = Vec::with_capacity(keys.len() + 1);
    first.push(0u32);
    for &c in &cfgs {
        first.push(first.last().unwrap() + table.patches[c as usize] as u32);
    }
    let n_verts = *first.last().unwrap() as usize;
    // Exact crossing of a cell's edge: world position and the triangle that made it, or
    // the edge midpoint (no triangle) where a turned corner moved the crossing.
    let crossing = |i: usize, j: usize, k: usize, e: usize| -> (Vec3, u32) {
        let (a, b) = EDGES[e];
        let pa = [i + (a & 1) as usize, j + (a >> 1 & 1) as usize, k + (a >> 2 & 1) as usize];
        let pb = [i + (b & 1) as usize, j + (b >> 1 & 1) as usize, k + (b >> 2 & 1) as usize];
        let exact = match e / 4 {
            0 => x.crossing(pa[1] + pa[2] * ny, pa[0]).map(|(d, t)| ([d as f64, pa[1] as f64, pa[2] as f64], t)),
            1 => y.crossing(pa[2] + pa[0] * nz, pa[1]).map(|(d, t)| ([pa[0] as f64, d as f64, pa[2] as f64], t)),
            _ => z.crossing(pa[0] + pa[1] * nx, pa[2]).map(|(d, t)| ([pa[0] as f64, pa[1] as f64, d as f64], t)),
        };
        let flipped = |p: [usize; 3]| field.flips.get(&field.column(p[0], p[1])).is_some_and(|f| f.contains(&(p[2] as u32)));
        match exact {
            Some((g, t)) if !flipped(pa) && !flipped(pb) => (grid.world(g), t),
            _ => (grid.world([0, 1, 2].map(|a| (pa[a] + pb[a]) as f64 * 0.5)), u32::MAX),
        }
    };
    // Per vertex: position and a triangle of the old surface it came from.
    let mut positions = vec![Vec3::ZERO; n_verts];
    let mut origin_tri = vec![u32::MAX; n_verts];
    {
        let ranges = first.windows(2).map(|w| (w[0] as usize, w[1] as usize));
        let pos_parts = crate::mesh::split_ranges(&mut positions, ranges.clone());
        let tri_parts = crate::mesh::split_ranges(&mut origin_tri, ranges);
        pos_parts
            .into_par_iter()
            .zip(tri_parts.into_par_iter())
            .zip(keys.par_iter().zip(cfgs.par_iter()))
            .with_min_len(256)
            .for_each(|((pos, tris), (&key, &cfg))| {
                let (i, j, k) = field.unkey(key);
                let mut sum = [Vec3::ZERO; 4];
                let mut cnt = [0u32; 4];
                let mut hits: [(Vec3, u32); 12] = [(Vec3::ZERO, u32::MAX); 12];
                for e in 0..12 {
                    let pt = table.patch[cfg as usize][e];
                    if pt == 255 {
                        continue;
                    }
                    let (p, t) = crossing(i, j, k, e);
                    sum[pt as usize] += p;
                    cnt[pt as usize] += 1;
                    hits[e] = (p, t);
                }
                for v in 0..pos.len() {
                    pos[v] = sum[v] / cnt[v].max(1) as f32;
                }
                // The mean of points on a curved surface lies off it. Slide each vertex along
                // its patch's mean normal to fit the tangent planes of the old triangles at
                // its crossings in the least-squares sense (dual contouring restricted to one
                // direction, so it never goes unstable), at most half a voxel.
                let mut nsum = [Vec3::ZERO; 4];
                let mut normals = [Vec3::ZERO; 12];
                for e in 0..12 {
                    let pt = table.patch[cfg as usize][e];
                    let (_, t) = hits[e];
                    if pt != 255 && t != u32::MAX {
                        let i3 = 3 * t as usize;
                        let q = |c: usize| mesh.positions[mesh.indices[i3 + c] as usize];
                        normals[e] = (q(1) - q(0)).cross(q(2) - q(0)).normalize_or_zero();
                        nsum[pt as usize] += normals[e];
                    }
                }
                for v in 0..pos.len() {
                    let n = nsum[v].normalize_or_zero();
                    if n == Vec3::ZERO {
                        continue;
                    }
                    let (mut num, mut den) = (0.0f32, 0.0f32);
                    for e in 0..12 {
                        if table.patch[cfg as usize][e] as usize == v && hits[e].1 != u32::MAX {
                            let (ni, pi) = (normals[e], hits[e].0);
                            let a = ni.dot(n);
                            num += a * ni.dot(pi - pos[v]);
                            den += a * a;
                        }
                    }
                    if den > 1e-6 {
                        let t = (num / den).clamp(-0.5 * h, 0.5 * h);
                        pos[v] += n * t;
                    }
                }
                // The old triangle of the crossing nearest each vertex: where Project's
                // walk starts.
                let mut best = [f32::INFINITY; 4];
                for e in 0..12 {
                    let pt = table.patch[cfg as usize][e];
                    let (p, t) = hits[e];
                    if pt != 255 && t != u32::MAX {
                        let d = (p - pos[pt as usize]).length_squared();
                        if d < best[pt as usize] {
                            best[pt as usize] = d;
                            tris[pt as usize] = t;
                        }
                    }
                }
            });
    }

    // Quads: each cell makes the quads of its three edges that end at its far corner
    // (every crossed edge of the grid is the far edge of exactly one cell).
    let find = |i: usize, j: usize, k: usize| -> usize {
        let key = field.key(i, j, k);
        keys.binary_search(&key).expect("the cells around a crossed edge are all surface cells")
    };
    let vertex = |cell: usize, e: usize| -> u32 { first[cell] + table.patch[cfgs[cell] as usize][e] as u32 };
    let has_groups = !mesh.groups.is_empty();
    let quads: Vec<([u32; 4], u16)> = keys
        .par_iter()
        .zip(cfgs.par_iter())
        .enumerate()
        .with_min_len(256)
        .flat_map_iter(|(cell, (&key, &cfg))| {
            let (i, j, k) = field.unkey(key);
            let mut out: Vec<([u32; 4], u16)> = Vec::new();
            let inside = |c: u8| cfg >> c & 1 == 1;
            let group = |e: usize| -> u16 {
                if has_groups {
                    let (_, t) = crossing(i, j, k, e);
                    if t != u32::MAX {
                        return mesh.groups[t as usize];
                    }
                }
                0
            };
            // z edge 11 (corners 3 -> 7): cells around it counter-clockwise seen from +z.
            if inside(3) != inside(7) {
                let ring = [
                    vertex(cell, 11),
                    vertex(find(i + 1, j, k), 10),
                    vertex(find(i + 1, j + 1, k), 8),
                    vertex(find(i, j + 1, k), 9),
                ];
                out.push((orient(ring, inside(3)), group(11)));
            }
            // x edge 3 (corners 6 -> 7), seen from +x.
            if inside(6) != inside(7) {
                let ring = [
                    vertex(cell, 3),
                    vertex(find(i, j + 1, k), 2),
                    vertex(find(i, j + 1, k + 1), 0),
                    vertex(find(i, j, k + 1), 1),
                ];
                out.push((orient(ring, inside(6)), group(3)));
            }
            // y edge 7 (corners 5 -> 7), seen from +y.
            if inside(5) != inside(7) {
                let ring = [
                    vertex(cell, 7),
                    vertex(find(i, j, k + 1), 5),
                    vertex(find(i + 1, j, k + 1), 4),
                    vertex(find(i + 1, j, k), 6),
                ];
                out.push((orient(ring, inside(5)), group(7)));
            }
            out
        })
        .collect();
    drop(field);
    stats.surface_ms = t.elapsed().as_secs_f32() * 1000.0;

    // 5. Blur and Project, then polygroup borders onto the old ones.
    let t = Instant::now();
    if s.project {
        if s.blur > 0 {
            positions = blur(&positions, &quads, s.blur);
        }
        project(mesh, &mut positions, &origin_tri);
    }
    if has_groups {
        stats.border_vertices = snap_borders(mesh, &mut positions, &quads, &origin_tri, 1.5 * h);
    }
    stats.project_ms = t.elapsed().as_secs_f32() * 1000.0;

    // Split every quad along its shorter diagonal.
    let mut indices = Vec::with_capacity(quads.len() * 6);
    let mut groups = if has_groups { Vec::with_capacity(quads.len() * 2) } else { Vec::new() };
    for (q, g) in &quads {
        let p = |v: u32| positions[v as usize];
        if (p(q[0]) - p(q[2])).length_squared() <= (p(q[1]) - p(q[3])).length_squared() {
            indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
        } else {
            indices.extend_from_slice(&[q[0], q[1], q[3], q[1], q[2], q[3]]);
        }
        if has_groups {
            groups.extend_from_slice(&[*g, *g]);
        }
    }
    Ok(Output {
        positions,
        indices,
        groups,
        stats,
    })
}

/// A ring of four vertices listed counter-clockwise seen from the axis's positive side,
/// as a quad facing out of the solid: kept when the inside is behind (at the edge's start),
/// reversed otherwise.
fn orient(ring: [u32; 4], inside_at_start: bool) -> [u32; 4] {
    if inside_at_start { ring } else { [ring[0], ring[3], ring[2], ring[1]] }
}

/// Laplacian smoothing over the quad mesh, `passes` times (half a step towards the mean
/// of the neighbours each time).
fn blur(positions: &[Vec3], quads: &[([u32; 4], u16)], passes: u32) -> Vec<Vec3> {
    let n = positions.len();
    // Neighbour lists from the quad edges, CSR.
    let mut count = vec![0u32; n + 1];
    for (q, _) in quads {
        for k in 0..4 {
            count[q[k] as usize + 1] += 2;
        }
    }
    for i in 0..n {
        count[i + 1] += count[i];
    }
    let mut fill = count.clone();
    let mut nb = vec![0u32; count[n] as usize];
    for (q, _) in quads {
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            nb[fill[a as usize] as usize] = b;
            fill[a as usize] += 1;
            nb[fill[b as usize] as usize] = a;
            fill[b as usize] += 1;
        }
    }
    let mut cur = positions.to_vec();
    for _ in 0..passes {
        cur = (0..n)
            .into_par_iter()
            .map(|v| {
                let ns = &nb[count[v] as usize..count[v + 1] as usize];
                if ns.is_empty() {
                    return cur[v];
                }
                // Every edge appears twice in the list; the mean is the same.
                let mean = ns.iter().map(|&w| cur[w as usize]).sum::<Vec3>() / ns.len() as f32;
                cur[v].lerp(mean, 0.5)
            })
            .collect();
    }
    cur
}

/// Move every vertex to the closest point of `mesh`: walk from a corner of the old
/// triangle it came from to whichever neighbouring vertex is nearer, until none is, then
/// take the closest point on the triangles around the vertex reached.
fn project(mesh: &SculptMesh, positions: &mut [Vec3], origin_tri: &[u32]) {
    positions.par_iter_mut().zip(origin_tri.par_iter()).for_each(|(p, &t0)| {
        if t0 == u32::MAX {
            return;
        }
        let v = nearest_vertex(mesh, *p, t0);
        let mut best = (f32::INFINITY, *p);
        let mut try_tris = |v: u32| {
            for &t in mesh.incident(v) {
                let i = 3 * t as usize;
                let q = closest_on_triangle(
                    *p,
                    mesh.positions[mesh.indices[i] as usize],
                    mesh.positions[mesh.indices[i + 1] as usize],
                    mesh.positions[mesh.indices[i + 2] as usize],
                );
                let d = (q - *p).length_squared();
                if d < best.0 {
                    best = (d, q);
                }
            }
        };
        try_tris(v);
        *p = best.1;
    });
}

/// The vertex of `mesh` nearest `p`, walking from a corner of triangle `t0` to whichever
/// neighbour is nearer until none is.
fn nearest_vertex(mesh: &SculptMesh, p: Vec3, t0: u32) -> u32 {
    let d2 = |v: u32| (mesh.positions[v as usize] - p).length_squared();
    let tri = &mesh.indices[3 * t0 as usize..3 * t0 as usize + 3];
    let mut v = *tri.iter().min_by(|&&a, &&b| d2(a).total_cmp(&d2(b))).unwrap();
    let mut dv = d2(v);
    for _ in 0..256 {
        let next = mesh.neighbors(v).iter().copied().map(|w| (w, d2(w))).min_by(|a, b| a.1.total_cmp(&b.1));
        match next {
            Some((w, d)) if d < dv => (v, dv) = (w, d),
            _ => break,
        }
    }
    v
}

/// Keep polygroup borders: every new vertex where quads of different groups meet moves to
/// the closest point on the old mesh's edges between those two groups, searched a few
/// rings around the old vertex nearest it, if one lies within `reach`. Without this the
/// border follows the voxel steps. Returns the number of vertices moved.
fn snap_borders(mesh: &SculptMesh, positions: &mut [Vec3], quads: &[([u32; 4], u16)], origin_tri: &[u32], reach: f32) -> usize {
    // The (up to) two groups around each new vertex; a third makes it a corner, left alone.
    const FREE: u32 = u32::MAX;
    let seen: Vec<[std::sync::atomic::AtomicU32; 2]> = (0..positions.len())
        .map(|_| [std::sync::atomic::AtomicU32::new(FREE), std::sync::atomic::AtomicU32::new(FREE)])
        .collect();
    let corner: Vec<std::sync::atomic::AtomicBool> = (0..positions.len()).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    use std::sync::atomic::Ordering::Relaxed as R;
    quads.par_iter().for_each(|(q, g)| {
        let g = *g as u32;
        for &v in q {
            let s = &seen[v as usize];
            match s[0].compare_exchange(FREE, g, R, R) {
                Ok(_) => {}
                Err(a) if a == g => {}
                Err(_) => match s[1].compare_exchange(FREE, g, R, R) {
                    Ok(_) => {}
                    Err(b) if b == g => {}
                    Err(_) => corner[v as usize].store(true, R),
                },
            }
        }
    });
    let group = |t: u32| mesh.groups[t as usize] as u32;
    let moved: Vec<(usize, Vec3)> = (0..positions.len())
        .into_par_iter()
        .filter(|&v| seen[v][1].load(R) != FREE && !corner[v].load(R) && origin_tri[v] != u32::MAX)
        .filter_map(|v| {
            let (ga, gb) = (seen[v][0].load(R), seen[v][1].load(R));
            let p = positions[v];
            let start = nearest_vertex(mesh, p, origin_tri[v]);
            // A few rings around it, breadth first.
            let mut ring = vec![start];
            let mut visited = vec![start];
            let mut best: Option<(f32, Vec3)> = None;
            for _ in 0..4 {
                let mut next = Vec::new();
                for &u in &ring {
                    for &w in mesh.neighbors(u) {
                        // The edge u-w lies between groups ga and gb?
                        let mut gs = [FREE; 2];
                        let mut k = 0;
                        for &t in mesh.incident(u) {
                            if k < 2 && mesh.indices[3 * t as usize..3 * t as usize + 3].contains(&w) {
                                gs[k] = group(t);
                                k += 1;
                            }
                        }
                        if k == 2 && ((gs[0] == ga && gs[1] == gb) || (gs[0] == gb && gs[1] == ga)) {
                            let (a, b) = (mesh.positions[u as usize], mesh.positions[w as usize]);
                            let ab = b - a;
                            let f = ((p - a).dot(ab) / ab.length_squared().max(1e-20)).clamp(0.0, 1.0);
                            let q = a + ab * f;
                            let d = (q - p).length_squared();
                            if best.is_none_or(|(bd, _)| d < bd) {
                                best = Some((d, q));
                            }
                        }
                        if !visited.contains(&w) {
                            visited.push(w);
                            next.push(w);
                        }
                    }
                }
                ring = next;
            }
            best.filter(|(d, _)| *d <= reach * reach).map(|(_, q)| (v, q))
        })
        .collect();
    for &(v, q) in &moved {
        positions[v] = q;
    }
    moved.len()
}

/// Closest point to `p` on triangle `abc` (Ericson, Real-Time Collision Detection).
fn closest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::Primitive;
    use std::collections::HashMap;

    fn mesh_of(p: Primitive, segments: u32) -> SculptMesh {
        let (pos, idx) = p.build(segments, 1.0);
        SculptMesh::from_raw(pos, idx)
    }

    /// Closed (every edge on two triangles, used once each way), no degenerate triangle,
    /// and the signed volume.
    fn check_closed(pos: &[Vec3], idx: &[u32]) -> f32 {
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for t in idx.chunks_exact(3) {
            assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2], "repeated corner");
            for k in 0..3 {
                *edges.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
            }
        }
        for (&(a, b), &n) in &edges {
            assert_eq!(n, 1, "edge {a}-{b} used {n} times in one direction");
            assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}-{b} has no twin: open or non-manifold");
        }
        idx.chunks_exact(3)
            .map(|t| pos[t[0] as usize].dot(pos[t[1] as usize].cross(pos[t[2] as usize])) / 6.0)
            .sum()
    }

    fn volume(m: &SculptMesh) -> f32 {
        m.indices
            .chunks_exact(3)
            .map(|t| m.positions[t[0] as usize].dot(m.positions[t[1] as usize].cross(m.positions[t[2] as usize])) / 6.0)
            .sum()
    }

    #[test]
    fn every_closed_primitive_wedges_into_a_closed_manifold_of_the_same_volume() {
        for p in [Primitive::Sphere, Primitive::Cube, Primitive::Cylinder, Primitive::Ring] {
            let m = mesh_of(p, p.default_segments());
            for resolution in [24, 64, 128] {
                let s = Settings { resolution, ..Settings::default() };
                let out = remesh(&m, &s).unwrap_or_else(|e| panic!("{p:?} at {resolution}: {e}"));
                let v = check_closed(&out.positions, &out.indices);
                let v0 = volume(&m);
                let err = (v - v0).abs() / v0;
                // A voxel of error on a shape a few dozen voxels across.
                assert!(err < 6.0 / resolution as f32, "{p:?} at {resolution}: volume {v} vs {v0}");
                // And the chunked mesh and Loop subdivision accept it.
                let mesh = SculptMesh::from_raw(out.positions, out.indices);
                assert!(!mesh.edges_over_two_triangles());
                let euler = mesh.vertex_count() as i64 - mesh.edge_count() as i64 + mesh.triangle_count() as i64;
                assert_eq!(euler, if p == Primitive::Ring { 0 } else { 2 }, "{p:?} at {resolution}: genus changed");
            }
        }
    }

    #[test]
    fn the_result_is_even_and_follows_the_resolution() {
        let m = mesh_of(Primitive::Sphere, 40);
        let s = Settings::default();
        let out = remesh(&m, &s).unwrap();
        // Vertex count near the estimate from the surface area.
        let est = s.estimate(area(&m));
        let n = out.positions.len();
        assert!(n > est * 7 / 10 && n < est * 15 / 10, "{n} vertices, estimate {est}");
        // Edges near the voxel size, every vertex near the sphere.
        let h = s.voxel();
        let mut longest = 0.0f32;
        for t in out.indices.chunks_exact(3) {
            for k in 0..3 {
                let e = (out.positions[t[k] as usize] - out.positions[t[(k + 1) % 3] as usize]).length();
                longest = longest.max(e);
            }
        }
        assert!(longest < 2.5 * h, "longest edge {longest} for voxel {h}");
        for p in &out.positions {
            assert!((p.length() - 1.0).abs() < 0.1 * h, "vertex {p:?} off the sphere by {}", p.length() - 1.0);
        }
        // Twice the resolution: about four times the vertices.
        let fine = remesh(&m, &Settings { resolution: 256, ..s }).unwrap();
        let ratio = fine.positions.len() as f32 / n as f32;
        assert!((3.5..4.5).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn overlapping_shells_merge_into_their_union() {
        // Two spheres overlapping by half a radius, as one mesh.
        let (mut pos, mut idx) = Primitive::Sphere.build(20, 1.0);
        let (p2, i2) = Primitive::Sphere.build(20, 1.0);
        let n = pos.len() as u32;
        pos.extend(p2.iter().map(|&p| p + Vec3::X * 1.5));
        idx.extend(i2.iter().map(|&i| i + n));
        let m = SculptMesh::from_raw(pos, idx);
        let out = remesh(&m, &Settings::default()).unwrap();
        let v = check_closed(&out.positions, &out.indices);
        // Union of two unit spheres 1.5 apart: 2 (4/3 pi) minus the lens.
        let lens = std::f32::consts::PI / 12.0 * (4.0 + 1.5) * (2.0 - 1.5f32).powi(2);
        let want = 2.0 * 4.0 / 3.0 * std::f32::consts::PI - lens;
        assert!((v - want).abs() / want < 0.03, "union volume {v} vs {want}");
        // One closed piece: Euler characteristic 2.
        let mesh = SculptMesh::from_raw(out.positions, out.indices);
        assert_eq!(mesh.vertex_count() as i64 - mesh.edge_count() as i64 + mesh.triangle_count() as i64, 2);
    }

    #[test]
    fn project_keeps_the_surface_and_groups_ride_along() {
        let (pos, idx) = Primitive::Cube.build(16, 1.0);
        let mut m = SculptMesh::from_raw(pos, idx);
        // Two groups: the top half and the rest.
        let g: Vec<u16> = (0..m.triangle_count())
            .map(|t| {
                let c = (0..3).map(|k| m.positions[m.indices[3 * t + k] as usize]).sum::<Vec3>() / 3.0;
                (c.y > 0.0) as u16
            })
            .collect();
        m.set_groups(g);
        let plain = remesh(&m, &Settings { resolution: 64, ..Settings::default() }).unwrap();
        let projected = remesh(&m, &Settings { resolution: 64, blur: 2, project: true }).unwrap();
        // Projected vertices lie on the cube's surface: one coordinate at +-1.
        let off = |p: &Vec3| (p.abs().max_element() - 1.0).abs();
        let worst = projected.positions.iter().map(off).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "projected vertex {worst} off the surface");
        let worst_plain = plain.positions.iter().map(off).fold(0.0f32, f32::max);
        assert!(worst_plain > worst, "projection moved nothing");
        check_closed(&projected.positions, &projected.indices);
        // Groups: triangles above follow group 1, below group 0, except along the border.
        assert_eq!(projected.groups.len(), projected.indices.len() / 3);
        let wrong = projected
            .indices
            .chunks_exact(3)
            .zip(&projected.groups)
            .filter(|(t, g)| {
                let c = t.iter().map(|&v| projected.positions[v as usize]).sum::<Vec3>() / 3.0;
                c.y.abs() > 0.1 && (c.y > 0.0) as u16 != **g
            })
            .count();
        assert_eq!(wrong, 0);
        // The chunked mesh keeps each triangle's group through its reordering.
        let n = projected.groups.iter().filter(|&&g| g == 1).count();
        let mesh = SculptMesh::from_raw_with_groups(projected.positions, projected.indices, projected.groups);
        assert_eq!(mesh.group_count(), 2);
        assert_eq!(mesh.groups.iter().filter(|&&g| g == 1).count(), n);
        for t in 0..mesh.triangle_count() as u32 {
            let c = (0..3).map(|k| mesh.positions[mesh.indices[3 * t as usize + k] as usize]).sum::<Vec3>() / 3.0;
            if c.y.abs() > 0.1 {
                assert_eq!(mesh.group(t), (c.y > 0.0) as u16);
            }
        }
    }

    #[test]
    fn polygroup_borders_stay_on_the_old_border() {
        // A cube split into two groups along the lattice line y = 0.25.
        let (pos, idx) = Primitive::Cube.build(16, 1.0);
        let mut m = SculptMesh::from_raw(pos, idx);
        let g: Vec<u16> = (0..m.triangle_count())
            .map(|t| {
                let c = (0..3).map(|k| m.positions[m.indices[3 * t + k] as usize]).sum::<Vec3>() / 3.0;
                (c.y > 0.25) as u16
            })
            .collect();
        m.set_groups(g);
        for project in [false, true] {
            let out = remesh(&m, &Settings { resolution: 64, blur: 2, project }).unwrap();
            assert!(out.stats.border_vertices > 0);
            check_closed(&out.positions, &out.indices);
            // Vertices with triangles of both groups around them sit on y = 0.25.
            let mut seen: Vec<[bool; 2]> = vec![[false; 2]; out.positions.len()];
            for (t, &g) in out.indices.chunks_exact(3).zip(&out.groups) {
                for &v in t {
                    seen[v as usize][g as usize] = true;
                }
            }
            let border: Vec<Vec3> = (0..out.positions.len()).filter(|&v| seen[v] == [true, true]).map(|v| out.positions[v]).collect();
            assert!(!border.is_empty());
            let worst = border.iter().map(|p| (p.y - 0.25).abs()).fold(0.0f32, f32::max);
            // Corners where the border turns around the cube's edges may stay a bit off.
            let off = border.iter().filter(|p| (p.y - 0.25).abs() > 1e-4).count();
            assert!(off * 10 < border.len(), "{off} of {} border vertices off the border (worst {worst})", border.len());
            // Nothing folded over: every triangle still faces outwards.
            for t in out.indices.chunks_exact(3) {
                let (a, b, c) = (out.positions[t[0] as usize], out.positions[t[1] as usize], out.positions[t[2] as usize]);
                let n = (b - a).cross(c - a);
                assert!(n.dot((a + b + c) / 3.0) > 0.0, "a triangle folded at {a:?}");
            }
        }
    }

    #[test]
    fn tangled_and_overlapping_input_still_gives_a_closed_manifold() {
        // A sphere pushed in and out by a high-frequency pattern strong enough to fold the
        // surface through itself in places, plus overlapping shells of every primitive.
        let (mut pos, mut idx) = Primitive::Sphere.build(30, 1.0);
        for p in pos.iter_mut() {
            let n = p.normalize();
            let f = (7.3 * p.x).sin() * (6.1 * p.y).cos() * (5.7 * p.z + 1.0).sin();
            *p = n * (1.0 + 0.45 * f);
        }
        let mut offset = 0.0;
        for (k, prim) in [Primitive::Cube, Primitive::Cylinder, Primitive::Ring].into_iter().enumerate() {
            let (p2, i2) = prim.build(prim.default_segments() / 2, 0.6);
            let base = pos.len() as u32;
            offset += 0.55;
            let shift = Vec3::new(offset, 0.2 * k as f32, -0.1 * k as f32);
            pos.extend(p2.iter().map(|&p| p + shift));
            idx.extend(i2.iter().map(|&i| i + base));
        }
        let m = SculptMesh::from_raw(pos, idx);
        let mut flips = 0;
        for resolution in [20, 33, 57, 101, 160] {
            for project in [false, true] {
                let out = remesh(&m, &Settings { resolution, blur: 1, project }).unwrap();
                flips += out.stats.flips;
                let v = check_closed(&out.positions, &out.indices);
                assert!(v > 0.0, "inside out at {resolution}");
                let mesh = SculptMesh::from_raw(out.positions, out.indices);
                assert!(!mesh.edges_over_two_triangles(), "non-manifold at {resolution}");
                // And it divides like any other mesh.
                let mut mr = crate::multires::Multires::new(mesh, usize::MAX);
                assert!(mr.divide());
                assert!(!mr.mesh().edges_over_two_triangles());
            }
        }
        assert!(flips > 0, "the ambiguous-face pass was exercised");
    }
}
