//! UV Master (ZBrush): automatic UV unwrapping, one UV per triangle corner.
//!
//! 1. **Charts.** Triangles are grown into regions that face roughly one way (or, with
//!    Polygroups, taken from the polygroups), refined a few times so the seams fall where
//!    the surface turns, tiny islands merged into a neighbour, and any chart that is not a
//!    disk (a tube, a closed shell) split until it is, so each can lie flat.
//! 2. **Flattening.** Each chart is laid flat by least squares conformal maps (Lévy et al.
//!    2002): the map that keeps angles best, two far-apart boundary vertices pinned, solved
//!    by conjugate gradients from the chart's projection onto its own plane.
//! 3. **Layout.** Every chart is scaled to the same texel density as the surface, turned to
//!    its tightest box, and the boxes are packed in shelves into the unit square with a
//!    margin between islands.

use glam::{DVec2, DVec3, Vec2, Vec3};
use rayon::prelude::*;
use std::collections::BinaryHeap;

/// Unwrap settings (ZBrush: UV Master).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Charts follow the polygroups (ZBrush: Polygroups).
    pub groups: bool,
    /// 20..=90: how far the surface may turn within one chart, in degrees. Larger gives
    /// fewer seams and more stretch.
    pub max_angle: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { groups: false, max_angle: 60.0 }
    }
}

/// An unwrap: the UVs of every triangle's three corners, in triangle order.
#[derive(Clone, Debug, Default)]
pub struct Unwrap {
    pub uvs: Vec<[Vec2; 3]>,
    pub stats: Stats,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub charts: usize,
    /// Area-weighted mean angle distortion (0 for none; σmax/σmin − 1 per triangle).
    pub distortion: f32,
    /// Triangles turned over in UV space.
    pub flipped: usize,
    /// How much of the unit square the islands cover.
    pub coverage: f32,
    pub ms: f32,
}

const NONE: u32 = u32::MAX;

/// The neighbour across each edge of every triangle (`NONE` at a border or where more than
/// two triangles meet), in the same direction as the triangle's edge k → k + 1.
fn neighbours(indices: &[u32]) -> Vec<[u32; 3]> {
    let n = indices.len() / 3;
    let mut edges: Vec<(u32, u32, u32, u8)> = (0..n as u32)
        .into_par_iter()
        .flat_map_iter(|t| {
            (0..3u8).map(move |k| {
                let (a, b) = (indices[3 * t as usize + k as usize], indices[3 * t as usize + (k as usize + 1) % 3]);
                (a.min(b), a.max(b), t, k)
            })
        })
        .collect();
    edges.par_sort_unstable();
    let mut nb = vec![[NONE; 3]; n];
    let mut i = 0;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j].0 == edges[i].0 && edges[j].1 == edges[i].1 {
            j += 1;
        }
        if j - i == 2 {
            let (x, y) = (edges[i], edges[i + 1]);
            nb[x.2 as usize][x.3 as usize] = y.2;
            nb[y.2 as usize][y.3 as usize] = x.2;
        }
        i = j;
    }
    nb
}

#[derive(PartialEq)]
struct Grow {
    cost: f64,
    tri: u32,
    chart: u32,
}

impl Eq for Grow {}

impl PartialOrd for Grow {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Grow {
    // Cheapest first.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.cost.total_cmp(&self.cost).then(o.tri.cmp(&self.tri))
    }
}

struct Surface<'a> {
    idx: &'a [u32],
    nb: Vec<[u32; 3]>,
    normal: Vec<DVec3>,
    area: Vec<f64>,
    centre: Vec<DVec3>,
    /// Triangles may only join charts of their own polygroup.
    group: Vec<u16>,
}

impl Surface<'_> {
    fn tri(&self, t: usize) -> [u32; 3] {
        [self.idx[3 * t], self.idx[3 * t + 1], self.idx[3 * t + 2]]
    }

    fn may_join(&self, t: usize, u: u32) -> bool {
        u != NONE && self.group[t] == self.group[u as usize]
    }

    /// Grow charts from `seeds` all at once, each triangle to the chart whose mean normal
    /// it faces best; `limit` (a cosine) leaves triangles that face too far away unclaimed.
    fn flood(&self, seeds: &[u32], normals: &[DVec3], limit: f64, chart: &mut [u32]) {
        chart.iter_mut().for_each(|c| *c = NONE);
        let mut heap = BinaryHeap::new();
        for (c, &s) in seeds.iter().enumerate() {
            heap.push(Grow { cost: 0.0, tri: s, chart: c as u32 });
        }
        while let Some(g) = heap.pop() {
            let t = g.tri as usize;
            if chart[t] != NONE {
                continue;
            }
            chart[t] = g.chart;
            for &u in &self.nb[t] {
                if !self.may_join(t, u) || chart[u as usize] != NONE {
                    continue;
                }
                let facing = self.normal[u as usize].dot(normals[g.chart as usize]);
                if facing < limit {
                    continue;
                }
                // Facing away costs most; distance from the seed a little, for round charts.
                let d = self.centre[u as usize].distance(self.centre[seeds[g.chart as usize] as usize]);
                heap.push(Grow { cost: (1.0 - facing) * 4.0 + d * 1e-3 + g.cost * 1e-3, tri: u, chart: g.chart });
            }
        }
    }

    /// Mean normal of each chart, and its most typical triangle (facing that way most).
    fn proxies(&self, chart: &[u32], count: usize) -> (Vec<DVec3>, Vec<u32>) {
        let mut n = vec![DVec3::ZERO; count];
        for (t, &c) in chart.iter().enumerate() {
            if c != NONE {
                n[c as usize] += self.normal[t] * self.area[t];
            }
        }
        let n: Vec<DVec3> = n.into_iter().map(|x| x.normalize_or(DVec3::Z)).collect();
        let mut best = vec![(f64::NEG_INFINITY, NONE); count];
        for (t, &c) in chart.iter().enumerate() {
            if c != NONE {
                let f = self.normal[t].dot(n[c as usize]);
                if f > best[c as usize].0 {
                    best[c as usize] = (f, t as u32);
                }
            }
        }
        (n, best.into_iter().map(|b| b.1).collect())
    }
}

/// Charts: one id per triangle, ids 0..count.
fn charts(s: &Surface, settings: &Settings) -> (Vec<u32>, usize) {
    let n = s.area.len();
    // With Polygroups the charts are the polygroups (each connected part): no angle limit.
    let limit = if settings.groups { -2.0 } else { (settings.max_angle.clamp(20.0, 90.0) as f64).to_radians().cos() };
    let mut chart = vec![NONE; n];
    // Seeds: grow one chart at a time within the angle, then a new one from what is left.
    let mut seeds: Vec<u32> = Vec::new();
    let mut normals: Vec<DVec3> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if chart[start] != NONE {
            continue;
        }
        let c = seeds.len() as u32;
        seeds.push(start as u32);
        let base = s.normal[start];
        normals.push(base);
        chart[start] = c;
        stack.push(start as u32);
        while let Some(t) = stack.pop() {
            for &u in &s.nb[t as usize] {
                if s.may_join(t as usize, u) && chart[u as usize] == NONE && s.normal[u as usize].dot(base) >= limit {
                    chart[u as usize] = c;
                    stack.push(u);
                }
            }
        }
    }
    // Refine: re-grow from each chart's most typical triangle toward its mean normal.
    for _ in 0..4 {
        let (nm, sd) = s.proxies(&chart, seeds.len());
        let keep: Vec<usize> = (0..sd.len()).filter(|&c| sd[c] != NONE).collect();
        seeds = keep.iter().map(|&c| sd[c]).collect();
        normals = keep.iter().map(|&c| nm[c]).collect();
        s.flood(&seeds, &normals, limit, &mut chart);
        // Anything left unclaimed starts charts of its own.
        for t in 0..n {
            if chart[t] == NONE {
                let c = seeds.len() as u32;
                seeds.push(t as u32);
                normals.push(s.normal[t]);
                chart[t] = c;
                stack.push(t as u32);
                while let Some(x) = stack.pop() {
                    for &u in &s.nb[x as usize] {
                        if s.may_join(x as usize, u) && chart[u as usize] == NONE && s.normal[u as usize].dot(s.normal[t]) >= limit {
                            chart[u as usize] = c;
                            stack.push(u);
                        }
                    }
                }
            }
        }
    }
    renumber(&mut chart)
}

/// Chart ids made 0..count in order of first use.
fn renumber(chart: &mut [u32]) -> (Vec<u32>, usize) {
    let mut map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for c in chart.iter_mut() {
        let k = map.len() as u32;
        *c = *map.entry(*c).or_insert(k);
    }
    (chart.to_vec(), map.len())
}

/// Tiny charts (under `min` triangles) go into the neighbour they share most edges with.
fn merge_small(s: &Surface, chart: &mut [u32], count: usize, min: usize) -> usize {
    let mut size = vec![0usize; count];
    for &c in chart.iter() {
        size[c as usize] += 1;
    }
    for _ in 0..3 {
        let mut changed = false;
        for c in 0..count {
            if size[c] == 0 || size[c] >= min {
                continue;
            }
            let mut votes: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
            for t in 0..chart.len() {
                if chart[t] as usize != c {
                    continue;
                }
                for &u in &s.nb[t] {
                    if s.may_join(t, u) && chart[u as usize] as usize != c {
                        *votes.entry(chart[u as usize]).or_default() += 1;
                    }
                }
            }
            if let Some((&into, _)) = votes.iter().max_by_key(|(k, v)| (**v, std::cmp::Reverse(**k))) {
                for x in chart.iter_mut() {
                    if *x as usize == c {
                        *x = into;
                    }
                }
                size[into as usize] += size[c];
                size[c] = 0;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    renumber(chart).1
}

/// Whether the triangles `tris` make a disk: connected, V − E + F = 1.
fn is_disk(s: &Surface, tris: &[u32]) -> bool {
    let mut verts: Vec<u32> = tris.iter().flat_map(|&t| s.tri(t as usize)).collect();
    verts.sort_unstable();
    verts.dedup();
    let mut edges: Vec<(u32, u32)> = tris
        .iter()
        .flat_map(|&t| {
            let v = s.tri(t as usize);
            (0..3).map(move |k| (v[k].min(v[(k + 1) % 3]), v[k].max(v[(k + 1) % 3])))
        })
        .collect();
    edges.sort_unstable();
    edges.dedup();
    verts.len() as i64 - edges.len() as i64 + tris.len() as i64 == 1
}

/// Split charts that are not disks in two, from two triangles far apart, until all are.
fn make_disks(s: &Surface, chart: &mut Vec<u32>, mut count: usize) -> usize {
    let mut queue: Vec<u32> = (0..count as u32).collect();
    let mut rounds = 0;
    while let Some(c) = queue.pop() {
        rounds += 1;
        if rounds > 100_000 {
            break;
        }
        let tris: Vec<u32> = (0..chart.len() as u32).filter(|&t| chart[t as usize] == c).collect();
        if tris.len() < 2 || is_disk(s, &tris) {
            continue;
        }
        // Two far-apart triangles: farthest from the first, then farthest from that.
        let far = |from: u32| tris.iter().copied().max_by(|&a, &b| s.centre[a as usize].distance_squared(s.centre[from as usize]).total_cmp(&s.centre[b as usize].distance_squared(s.centre[from as usize]))).unwrap();
        let a = far(tris[0]);
        let b = far(a);
        // Grow both within the chart, nearest seed first.
        let new = count as u32;
        count += 1;
        let mut owner: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut heap = BinaryHeap::new();
        heap.push(Grow { cost: 0.0, tri: a, chart: c });
        heap.push(Grow { cost: 0.0, tri: b, chart: new });
        while let Some(g) = heap.pop() {
            if owner.contains_key(&g.tri) {
                continue;
            }
            owner.insert(g.tri, g.chart);
            for &u in &s.nb[g.tri as usize] {
                if u != NONE && chart[u as usize] == c && !owner.contains_key(&u) {
                    let d = s.centre[u as usize].distance(s.centre[g.tri as usize]);
                    heap.push(Grow { cost: g.cost + d, tri: u, chart: g.chart });
                }
            }
        }
        for (&t, &o) in &owner {
            chart[t as usize] = o;
        }
        // A part the seeds could not reach (the chart was not connected) gets its own.
        for &t in &tris {
            if !owner.contains_key(&t) {
                chart[t as usize] = count as u32;
            }
        }
        if tris.iter().any(|t| !owner.contains_key(t)) {
            queue.push(count as u32);
            count += 1;
        }
        queue.push(c);
        queue.push(new);
    }
    renumber(chart).1
}

/// LSCM for one chart: UVs of its local vertices `verts` (indices into the mesh), from its
/// triangles `tris` given in local vertex indices.
fn flatten(p: &[Vec3], verts: &[u32], tris: &[[u32; 3]]) -> Vec<DVec2> {
    let m = verts.len();
    let pos: Vec<DVec3> = verts.iter().map(|&v| p[v as usize].as_dvec3()).collect();
    if m < 3 {
        return pos.iter().map(|q| DVec2::new(q.x, q.y)).collect();
    }
    // Starting point: projection onto the chart's own plane.
    let mut normal = DVec3::ZERO;
    for t in tris {
        let [a, b, c] = t.map(|k| pos[k as usize]);
        normal += (b - a).cross(c - a);
    }
    let normal = normal.normalize_or(DVec3::Z);
    let ax = normal.any_orthonormal_vector();
    let ay = normal.cross(ax);
    let mut uv: Vec<DVec2> = pos.iter().map(|q| DVec2::new(q.dot(ax), q.dot(ay))).collect();
    // Pins: the two vertices farthest apart (roughly), fixed where the projection put them.
    let far = |from: usize| (0..m).max_by(|&a, &b| pos[a].distance_squared(pos[from]).total_cmp(&pos[b].distance_squared(pos[from]))).unwrap();
    let a = far(0);
    let b = far(a);
    if a == b {
        return uv;
    }
    let pinned = |v: usize| v == a || v == b;
    // Free variables: U and V of every vertex but the pins.
    let mut var = vec![usize::MAX; m];
    let mut k = 0;
    for v in 0..m {
        if !pinned(v) {
            var[v] = k;
            k += 2;
        }
    }
    // Rows: two per triangle, (column, value) for the free variables and a constant.
    struct Row {
        cols: [(u32, f64); 6],
        len: u8,
        rhs: f64,
    }
    let mut rows: Vec<Row> = Vec::with_capacity(tris.len() * 2);
    for t in tris {
        let [p0, p1, p2] = t.map(|k| pos[k as usize]);
        let e1 = p1 - p0;
        let e2 = p2 - p0;
        let n = e1.cross(e2);
        let area2 = n.length();
        if area2 < 1e-24 {
            continue;
        }
        let x = e1.normalize();
        let y = n.normalize().cross(x);
        let z = [DVec2::ZERO, DVec2::new(e1.length(), 0.0), DVec2::new(e2.dot(x), e2.dot(y))];
        // W_j = z_k − z_l for (j, k, l) cyclic.
        let w = [z[2] - z[1], z[0] - z[2], z[1] - z[0]];
        let s = 1.0 / area2.sqrt();
        let mut re = Row { cols: [(0, 0.0); 6], len: 0, rhs: 0.0 };
        let mut im = Row { cols: [(0, 0.0); 6], len: 0, rhs: 0.0 };
        for j in 0..3 {
            let v = t[j] as usize;
            let (wa, wb) = (w[j].x * s, w[j].y * s);
            // (wa + i wb)(U + i V) = (wa U − wb V) + i (wb U + wa V)
            if pinned(v) {
                re.rhs -= wa * uv[v].x - wb * uv[v].y;
                im.rhs -= wb * uv[v].x + wa * uv[v].y;
            } else {
                let c = var[v] as u32;
                re.cols[re.len as usize] = (c, wa);
                re.cols[re.len as usize + 1] = (c + 1, -wb);
                re.len += 2;
                im.cols[im.len as usize] = (c, wb);
                im.cols[im.len as usize + 1] = (c + 1, wa);
                im.len += 2;
            }
        }
        rows.push(re);
        rows.push(im);
    }
    // CGLS: least squares A x ≈ b from the projection.
    let mut x = vec![0.0f64; k];
    for v in 0..m {
        if !pinned(v) {
            x[var[v]] = uv[v].x;
            x[var[v] + 1] = uv[v].y;
        }
    }
    let apply = |x: &[f64], out: &mut Vec<f64>| {
        out.clear();
        out.extend(rows.iter().map(|r| r.cols[..r.len as usize].iter().map(|&(c, v)| v * x[c as usize]).sum::<f64>()));
    };
    let apply_t = |r: &[f64], out: &mut Vec<f64>| {
        out.clear();
        out.resize(k, 0.0);
        for (row, &rv) in rows.iter().zip(r) {
            for &(c, v) in &row.cols[..row.len as usize] {
                out[c as usize] += v * rv;
            }
        }
    };
    let mut ax_ = Vec::new();
    apply(&x, &mut ax_);
    let mut r: Vec<f64> = rows.iter().zip(&ax_).map(|(row, &a)| row.rhs - a).collect();
    let mut s_ = Vec::new();
    apply_t(&r, &mut s_);
    let mut pdir = s_.clone();
    let mut gamma: f64 = s_.iter().map(|v| v * v).sum();
    let start = gamma.sqrt().max(1e-300);
    let mut q = Vec::new();
    for _ in 0..(4 * k).clamp(200, 20_000) {
        if gamma.sqrt() <= start * 1e-7 {
            break;
        }
        apply(&pdir, &mut q);
        let qq: f64 = q.iter().map(|v| v * v).sum();
        if qq <= 0.0 {
            break;
        }
        let alpha = gamma / qq;
        for i in 0..k {
            x[i] += alpha * pdir[i];
        }
        for i in 0..r.len() {
            r[i] -= alpha * q[i];
        }
        apply_t(&r, &mut s_);
        let g: f64 = s_.iter().map(|v| v * v).sum();
        let beta = g / gamma;
        gamma = g;
        for i in 0..k {
            pdir[i] = s_[i] + beta * pdir[i];
        }
    }
    for v in 0..m {
        if !pinned(v) {
            uv[v] = DVec2::new(x[var[v]], x[var[v] + 1]);
        }
    }
    uv
}

/// Unwrap a mesh. `groups` (per triangle, may be empty) is used with `s.groups`.
pub fn unwrap(positions: &[Vec3], indices: &[u32], groups: &[u16], s: &Settings) -> Result<Unwrap, String> {
    let start = std::time::Instant::now();
    let n = indices.len() / 3;
    if n == 0 {
        return Err("no triangles to unwrap".into());
    }
    let mut surface = Surface {
        idx: indices,
        nb: neighbours(indices),
        normal: Vec::with_capacity(n),
        area: Vec::with_capacity(n),
        centre: Vec::with_capacity(n),
        group: if s.groups && groups.len() == n { groups.to_vec() } else { vec![0; n] },
    };
    for t in 0..n {
        let [a, b, c] = surface.tri(t).map(|v| positions[v as usize].as_dvec3());
        let nr = (b - a).cross(c - a);
        surface.area.push(nr.length() * 0.5);
        surface.normal.push(nr.normalize_or(DVec3::Z));
        surface.centre.push((a + b + c) / 3.0);
    }
    // Charts follow the large-scale form: grow them on normals smoothed over the
    // neighbourhood, so fine detail (ridges, pores, noise) does not cut the surface into
    // crumbs. The flattening below still sees the real surface.
    let passes = (6.0 + (n as f64).sqrt() / 20.0).min(40.0) as usize;
    for _ in 0..passes {
        let prev = surface.normal.clone();
        surface.normal = (0..n)
            .into_par_iter()
            .map(|t| {
                let mut sum = prev[t] * surface.area[t];
                for &u in &surface.nb[t] {
                    if u != NONE {
                        sum += prev[u as usize] * surface.area[u as usize];
                    }
                }
                sum.normalize_or(prev[t])
            })
            .collect();
    }
    crate::jobs::check()?;
    let (mut chart, count) = charts(&surface, s);
    let min = (n / 2000).clamp(4, 64);
    let grown = count;
    let count = merge_small(&surface, &mut chart, count, min);
    let merged = count;
    let count = make_disks(&surface, &mut chart, count);
    log::debug!("uv charts: {grown} grown, {merged} merged, {count} disks");

    // Per chart: its triangles and local vertices.
    let mut members: Vec<Vec<u32>> = vec![Vec::new(); count];
    for (t, &c) in chart.iter().enumerate() {
        members[c as usize].push(t as u32);
    }
    struct Island {
        tris: Vec<u32>,
        local: Vec<[u32; 3]>,
        uv: Vec<DVec2>,
        lo: DVec2,
        hi: DVec2,
    }
    let mut islands: Vec<Island> = members
        .into_par_iter()
        .filter(|t| !t.is_empty())
        .map(|tris| {
            let mut index: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            let mut verts = Vec::new();
            let local: Vec<[u32; 3]> = tris
                .iter()
                .map(|&t| {
                    surface.tri(t as usize).map(|v| {
                        *index.entry(v).or_insert_with(|| {
                            verts.push(v);
                            verts.len() as u32 - 1
                        })
                    })
                })
                .collect();
            let mut uv = flatten(positions, &verts, &local);
            // Same texel density as the surface.
            let (mut a3, mut a2) = (0.0, 0.0);
            for (i, &t) in tris.iter().enumerate() {
                a3 += surface.area[t as usize];
                let [p, q, r] = local[i].map(|k| uv[k as usize]);
                a2 += ((q - p).perp_dot(r - p) * 0.5).abs();
            }
            let scale = if a2 > 0.0 { (a3 / a2).sqrt() } else { 1.0 };
            // Mirrored? Turn it the right way round (most triangles counter-clockwise).
            let signed: f64 = local.iter().map(|t| {
                let [p, q, r] = t.map(|k| uv[k as usize]);
                (q - p).perp_dot(r - p)
            }).sum();
            let flip = if signed < 0.0 { -1.0 } else { 1.0 };
            for x in &mut uv {
                *x = DVec2::new(x.x * scale * flip, x.y * scale);
            }
            // Its tightest box: try the directions of its edges (on the hull) and keep the
            // smallest area.
            let centre = uv.iter().copied().sum::<DVec2>() / uv.len() as f64;
            let mut best = (f64::INFINITY, DVec2::X);
            for t in local.iter().step_by((local.len() / 64).max(1)) {
                for k in 0..3 {
                    let d = (uv[t[(k + 1) % 3] as usize] - uv[t[k] as usize]).normalize_or_zero();
                    if d == DVec2::ZERO {
                        continue;
                    }
                    let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
                    for &q in &uv {
                        let r = q - centre;
                        let x = DVec2::new(r.dot(d), d.perp_dot(r));
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                    let area = (hi - lo).x * (hi - lo).y;
                    if area < best.0 {
                        best = (area, d);
                    }
                }
            }
            let d = best.1;
            for x in &mut uv {
                let r = *x - centre;
                *x = DVec2::new(r.dot(d), d.perp_dot(r));
            }
            // Wider than tall.
            let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
            for &q in &uv {
                lo = lo.min(q);
                hi = hi.max(q);
            }
            if hi.y - lo.y > hi.x - lo.x {
                for x in &mut uv {
                    *x = DVec2::new(x.y, -x.x);
                }
                (lo, hi) = (DVec2::new(lo.y, -hi.x), DVec2::new(hi.y, -lo.x));
            }
            Island { tris, local, uv, lo, hi }
        })
        .collect();

    crate::jobs::check()?;
    // Pack: tallest first, in shelves across a square about the size of all the boxes.
    let total: f64 = islands.iter().map(|i| (i.hi - i.lo).x * (i.hi - i.lo).y).sum();
    let margin = total.sqrt() * 0.008;
    islands.sort_by(|a, b| (b.hi - b.lo).y.total_cmp(&(a.hi - a.lo).y));
    let widest = islands.iter().map(|i| (i.hi - i.lo).x).fold(0.0, f64::max);
    // Shelves `width` wide: the offsets and the side of the square they need.
    let shelves = |width: f64| -> (f64, Vec<DVec2>) {
        let (mut x, mut y, mut row) = (margin, margin, 0.0f64);
        let mut offsets = Vec::with_capacity(islands.len());
        for i in &islands {
            let size = i.hi - i.lo;
            if x + size.x + margin > width && x > margin {
                x = margin;
                y += row + margin;
                row = 0.0;
            }
            offsets.push(DVec2::new(x, y) - i.lo);
            x += size.x + margin;
            row = row.max(size.y);
        }
        (width.max(y + row + margin), offsets)
    };
    // The width that gives the smallest square, of a range of tries.
    let (side, offsets) = (0..=24)
        .map(|k| (total.sqrt() * (0.9 + k as f64 * 0.025)).max(widest + 2.0 * margin))
        .map(shelves)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap();
    let mut uvs = vec![[Vec2::ZERO; 3]; n];
    let mut stats = Stats { charts: islands.len(), ..Default::default() };
    let (mut weighted, mut area_sum, mut uv_area) = (0.0f64, 0.0f64, 0.0f64);
    for (i, off) in islands.iter().zip(&offsets) {
        for (k, &t) in i.tris.iter().enumerate() {
            let c = i.local[k].map(|v| (i.uv[v as usize] + *off) / side);
            uvs[t as usize] = c.map(|q| q.as_vec2());
            // Distortion: singular values of the map from the triangle to its UVs.
            let [p0, p1, p2] = surface.tri(t as usize).map(|v| positions[v as usize].as_dvec3());
            let (e1, e2) = (p1 - p0, p2 - p0);
            let nrm = e1.cross(e2);
            if nrm.length() < 1e-24 {
                continue;
            }
            let ax = e1.normalize();
            let ay = nrm.normalize().cross(ax);
            let (s1, s2) = (DVec2::new(e1.length(), 0.0), DVec2::new(e2.dot(ax), e2.dot(ay)));
            let (t1, t2) = (c[1] - c[0], c[2] - c[0]);
            let det = s1.perp_dot(s2);
            // J maps s to t: J = [t1 t2] [s1 s2]^-1
            let inv = glam::DMat2::from_cols(s1, s2).inverse();
            let j = glam::DMat2::from_cols(t1, t2) * inv;
            let (a, b, cc, d) = (j.x_axis.x, j.y_axis.x, j.x_axis.y, j.y_axis.y);
            let e = 0.5 * (a * a + b * b + cc * cc + d * d);
            let f = ((a * a + b * b - cc * cc - d * d).powi(2) * 0.25 + (a * cc + b * d).powi(2)).sqrt();
            let (smax, smin) = ((e + f).max(0.0).sqrt(), (e - f).max(0.0).sqrt());
            let area = det.abs() * 0.5;
            if t1.perp_dot(t2) < 0.0 {
                stats.flipped += 1;
            }
            if smin > 0.0 {
                weighted += (smax / smin - 1.0).min(10.0) * area;
            }
            area_sum += area;
            uv_area += (t1.perp_dot(t2) * 0.5).abs();
        }
    }
    stats.distortion = (weighted / area_sum.max(1e-300)) as f32;
    stats.coverage = uv_area as f32;
    stats.ms = start.elapsed().as_secs_f32() * 1000.0;
    Ok(Unwrap { uvs, stats })
}

/// A picture of the layout, `size` pixels square, RGBA: islands filled, their triangle
/// edges drawn lighter, on a dark square (the UV Map preview). V runs up, as in ZBrush.
pub fn layout_image(uvs: &[[Vec2; 3]], size: usize) -> Vec<[u8; 4]> {
    let bg = [28u8, 30, 36, 255];
    let fill = [120u8, 96, 72, 255];
    let edge = [236u8, 196, 150, 255];
    let mut px = vec![bg; size * size];
    let s = size as f32;
    for c in uvs {
        let q = c.map(|p| Vec2::new(p.x * s, (1.0 - p.y) * s));
        let area = (q[1] - q[0]).perp_dot(q[2] - q[0]);
        if area.abs() < 1e-9 {
            continue;
        }
        let lo = q[0].min(q[1]).min(q[2]).max(Vec2::ZERO);
        let hi = q[0].max(q[1]).max(q[2]).min(Vec2::splat(s - 1.0));
        for y in lo.y.floor() as usize..=hi.y.ceil().min(s - 1.0) as usize {
            for x in lo.x.floor() as usize..=hi.x.ceil().min(s - 1.0) as usize {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                // Distance to each edge in pixels, signed inside.
                let mut nearest = f32::INFINITY;
                let mut inside = true;
                for k in 0..3 {
                    let (a, b) = (q[k], q[(k + 1) % 3]);
                    let d = (b - a).perp_dot(p - a) / (b - a).length().max(1e-6) * area.signum();
                    inside &= d >= -0.5;
                    nearest = nearest.min(d.abs());
                }
                if !inside {
                    continue;
                }
                let cell = &mut px[y * size + x];
                if nearest < 0.6 {
                    *cell = edge;
                } else if *cell == bg {
                    *cell = fill;
                }
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Islands drawn into a grid: cells covered more than once, of those covered.
    fn overlap(u: &Unwrap, res: usize) -> f32 {
        let mut grid = vec![0u8; res * res];
        let mut owner = vec![u32::MAX; res * res];
        let mut twice = 0usize;
        let mut once = 0usize;
        // Which island each triangle is on is not kept; count cells hit by two triangles
        // whose UVs do not share a vertex, which only happens across islands or folds.
        for (t, c) in u.uvs.iter().enumerate() {
            let lo = c[0].min(c[1]).min(c[2]) * res as f32;
            let hi = c[0].max(c[1]).max(c[2]) * res as f32;
            for y in lo.y.floor().max(0.0) as usize..(hi.y.ceil() as usize).min(res) {
                for x in lo.x.floor().max(0.0) as usize..(hi.x.ceil() as usize).min(res) {
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / res as f32;
                    // Strictly inside: a cell centre on an edge shared by two triangles counts for neither.
                    let inside = (0..3).all(|k| (c[(k + 1) % 3] - c[k]).perp_dot(p - c[k]) > 1e-9);
                    if !inside {
                        continue;
                    }
                    let cell = y * res + x;
                    if grid[cell] == 0 {
                        once += 1;
                        owner[cell] = t as u32;
                    } else if grid[cell] == 1 && owner[cell] != t as u32 {
                        twice += 1;
                    }
                    grid[cell] = grid[cell].saturating_add(1);
                }
            }
        }
        twice as f32 / once.max(1) as f32
    }

    fn check(u: &Unwrap) {
        assert!(u.uvs.iter().flatten().all(|q| (0.0..=1.0).contains(&q.x) && (0.0..=1.0).contains(&q.y)), "outside the unit square");
        assert_eq!(u.stats.flipped, 0, "{:?}", u.stats);
        let o = overlap(u, 512);
        assert!(o < 0.002, "{o} of the covered cells overlap · {:?}", u.stats);
    }

    #[test]
    fn a_plane_is_one_flat_island() {
        let (p, i) = crate::primitive::Primitive::Plane.build(30, 1.0);
        let u = unwrap(&p, &i, &[], &Settings::default()).unwrap();
        assert_eq!(u.stats.charts, 1);
        assert!(u.stats.distortion < 1e-3, "{:?}", u.stats);
        assert!(u.stats.coverage > 0.6, "{:?}", u.stats);
        check(&u);
    }

    #[test]
    fn a_sphere_unwraps_into_a_few_islands_with_little_stretch() {
        let (p, i) = crate::mesh::cube_sphere(24, 1.0);
        let u = unwrap(&p, &i, &[], &Settings::default()).unwrap();
        eprintln!("{:?}", u.stats);
        assert!((4..=24).contains(&u.stats.charts), "{:?}", u.stats);
        assert!(u.stats.distortion < 0.12, "{:?}", u.stats);
        assert!(u.stats.coverage > 0.4, "{:?}", u.stats);
        check(&u);
    }

    #[test]
    fn a_torus_and_a_cylinder_are_cut_into_disks() {
        for prim in [crate::primitive::Primitive::Ring, crate::primitive::Primitive::Cylinder] {
            let (p, i) = prim.build(24, 1.0);
            let u = unwrap(&p, &i, &[], &Settings { max_angle: 90.0, ..Default::default() }).unwrap();
            eprintln!("{prim:?}: {:?}", u.stats);
            assert!(u.stats.distortion < 0.3, "{prim:?}: {:?}", u.stats);
            check(&u);
        }
    }

    #[test]
    fn polygroups_become_the_islands() {
        let (p, i) = crate::mesh::cube_sphere(20, 1.0);
        let groups: Vec<u16> = i.chunks_exact(3).map(|t| {
            let c = (p[t[0] as usize] + p[t[1] as usize] + p[t[2] as usize]) / 3.0;
            if c.y > 0.0 { 1 } else { 2 }
        }).collect();
        let u = unwrap(&p, &i, &groups, &Settings { groups: true, max_angle: 90.0 }).unwrap();
        eprintln!("{:?}", u.stats);
        // Two hemispheres: two disks, nothing more needed.
        assert_eq!(u.stats.charts, 2, "{:?}", u.stats);
        check(&u);
    }

    /// `cargo test --release unwrap_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn unwrap_speed() {
        for seg in [40u32, 100] {
            let (p, i) = crate::mesh::cube_sphere(seg, 1.0);
            let u = unwrap(&p, &i, &[], &Settings::default()).unwrap();
            eprintln!("{} triangles: {:?}", i.len() / 3, u.stats);
        }
    }
}
