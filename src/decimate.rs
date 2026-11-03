//! Decimation (ZBrush: Decimation Master): far fewer triangles with the shape kept, for
//! export and 3D printing.
//!
//! Quadric edge collapse (Garland and Heckbert 1997) in the fast form of Forstmann's
//! simplifier: every vertex sums the squared distance to the planes of its triangles; an
//! edge collapses to the point that is nearest all of both ends' planes, and its cost is
//! that distance. Instead of a priority queue, sweeps collapse every edge cheaper than a
//! threshold, until the target is met. The threshold is taken from the costs themselves
//! each sweep (the cheapest slice of what is left), so the order stays close to greedy
//! for any model. Additions here:
//!
//! - **Manifold**: an edge collapses only when the two ends share exactly the neighbours
//!   of the triangles on it (the link condition), so no pinches or folds come out.
//! - **Keep Borders / Keep Groups**: open borders and polygroup borders get planes across
//!   them with a large weight, so they stay where they are while their vertices thin out.
//! - **Keep Polypaint**: colours are interpolated along each collapsed edge.
//! - No triangle may turn over or become a sliver.

use crate::mesh::SculptMesh;
use glam::{DVec3, Vec3};
use rayon::prelude::*;

/// Decimation settings (ZBrush: Decimation Master).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Percentage of the triangles to keep (ZBrush: % of decimation).
    pub keep_percent: f32,
    pub keep_borders: bool,
    pub keep_groups: bool,
    pub keep_paint: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { keep_percent: 20.0, keep_borders: true, keep_groups: true, keep_paint: true }
    }
}

/// A symmetric 4 × 4 quadric, upper triangle.
#[derive(Clone, Copy, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    /// The squared distance to the plane `n · x + d = 0`, times `w`.
    fn plane(n: DVec3, d: f64, w: f64) -> Self {
        let (a, b, c) = (n.x, n.y, n.z);
        Self([a * a, a * b, a * c, a * d, b * b, b * c, b * d, c * c, c * d, d * d].map(|x| x * w))
    }

    fn add(&mut self, o: &Self) {
        for k in 0..10 {
            self.0[k] += o.0[k];
        }
    }

    fn sum(&self, o: &Self) -> Self {
        let mut s = *self;
        s.add(o);
        s
    }

    fn error(&self, p: DVec3) -> f64 {
        let q = &self.0;
        let (x, y, z) = (p.x, p.y, p.z);
        q[0] * x * x + 2.0 * q[1] * x * y + 2.0 * q[2] * x * z + 2.0 * q[3] * x + q[4] * y * y + 2.0 * q[5] * y * z + 2.0 * q[6] * y + q[7] * z * z + 2.0 * q[8] * z + q[9]
    }

    /// The point of least error, if the quadric pins one down.
    fn optimum(&self) -> Option<DVec3> {
        let q = &self.0;
        let m = glam::DMat3::from_cols(DVec3::new(q[0], q[1], q[2]), DVec3::new(q[1], q[4], q[5]), DVec3::new(q[2], q[5], q[7]));
        let det = m.determinant();
        let scale = q[0] * q[4] * q[7] + 1e-30;
        if det.abs() <= 1e-9 * scale.abs() {
            return None;
        }
        Some(m.inverse() * DVec3::new(-q[3], -q[6], -q[8]))
    }
}

struct Tri {
    v: [u32; 3],
    /// Cost of collapsing each edge (k to k + 1), and the least of them.
    err: [f64; 4],
    deleted: bool,
    dirty: bool,
    n: DVec3,
}

#[derive(Clone, Default)]
struct Vert {
    p: DVec3,
    q: Quadric,
    tstart: u32,
    tcount: u32,
}

/// What a decimation made.
pub struct Decimated {
    pub mesh: SculptMesh,
    /// Triangles before and after (the app counts from the mesh; tests read these).
    #[allow(dead_code)]
    pub before: usize,
    #[allow(dead_code)]
    pub after: usize,
}

struct Simplifier {
    v: Vec<Vert>,
    t: Vec<Tri>,
    /// Triangle and corner, per vertex from `tstart`.
    refs: Vec<(u32, u8)>,
    groups: Vec<u16>,
    colors: Vec<[u8; 4]>,
}

impl Simplifier {
    /// Rebuild the vertex-to-triangle references, dropping deleted triangles.
    fn compact(&mut self) {
        let mut keep = 0;
        for i in 0..self.t.len() {
            if !self.t[i].deleted {
                self.t.swap(keep, i);
                if !self.groups.is_empty() {
                    self.groups.swap(keep, i);
                }
                keep += 1;
            }
        }
        self.t.truncate(keep);
        if !self.groups.is_empty() {
            self.groups.truncate(keep);
        }
        self.rebuild_refs();
    }

    fn rebuild_refs(&mut self) {
        for v in &mut self.v {
            v.tcount = 0;
        }
        for t in &self.t {
            for &k in &t.v {
                self.v[k as usize].tcount += 1;
            }
        }
        let mut start = 0;
        for v in &mut self.v {
            v.tstart = start;
            start += v.tcount;
            v.tcount = 0;
        }
        self.refs.clear();
        self.refs.resize(start as usize, (0, 0));
        for (i, t) in self.t.iter().enumerate() {
            for (c, &k) in t.v.iter().enumerate() {
                let v = &mut self.v[k as usize];
                self.refs[(v.tstart + v.tcount) as usize] = (i as u32, c as u8);
                v.tcount += 1;
            }
        }
    }

    fn tris_of(&self, v: u32) -> std::ops::Range<usize> {
        let x = &self.v[v as usize];
        x.tstart as usize..(x.tstart + x.tcount) as usize
    }

    /// Cost and target point of collapsing `a` and `b`.
    fn cost(&self, a: u32, b: u32) -> (f64, DVec3) {
        Self::cost_in(&self.v, a, b)
    }

    fn cost_in(v: &[Vert], a: u32, b: u32) -> (f64, DVec3) {
        let q = v[a as usize].q.sum(&v[b as usize].q);
        let (pa, pb) = (v[a as usize].p, v[b as usize].p);
        if let Some(p) = q.optimum() {
            // Not far off the edge: an optimum way out is a sign of a near-flat quadric.
            let len = pa.distance(pb);
            let mid = (pa + pb) * 0.5;
            if p.distance(mid) <= len {
                return (q.error(p), p);
            }
        }
        [pa, pb, (pa + pb) * 0.5].into_iter().map(|p| (q.error(p), p)).min_by(|x, y| x.0.total_cmp(&y.0)).unwrap()
    }

    fn edge_errors(&mut self, i: usize) {
        let v = self.t[i].v;
        let mut e = [0.0; 4];
        for k in 0..3 {
            e[k] = self.cost(v[k], v[(k + 1) % 3]).0;
        }
        e[3] = e[0].min(e[1]).min(e[2]);
        self.t[i].err = e;
    }

    /// Whether moving `v` (collapsing into `other`) to `p` turns a triangle over or makes
    /// a sliver; `gone[k]` marks the triangles around `v` that hold `other` (they go).
    fn flips(&self, p: DVec3, v: u32, other: u32, gone: &mut Vec<bool>) -> bool {
        let range = self.tris_of(v);
        gone.clear();
        gone.resize(range.len(), false);
        for (k, r) in range.clone().enumerate() {
            let (ti, c) = self.refs[r];
            let t = &self.t[ti as usize];
            if t.deleted {
                continue;
            }
            let c = c as usize;
            let (i1, i2) = (t.v[(c + 1) % 3], t.v[(c + 2) % 3]);
            if i1 == other || i2 == other {
                gone[k] = true;
                continue;
            }
            let (p1, p2) = (self.v[i1 as usize].p, self.v[i2 as usize].p);
            let d1 = (p1 - p).normalize_or_zero();
            let d2 = (p2 - p).normalize_or_zero();
            if d1.dot(d2).abs() > 0.999 {
                return true;
            }
            let n = d1.cross(d2).normalize_or_zero();
            if n.dot(t.n) < 0.2 {
                return true;
            }
        }
        false
    }

    /// The link condition: `a` and `b` share exactly the neighbours across the triangles on
    /// their edge (one for a border edge, two inside). Also whether the edge is a border.
    fn manifold_collapse(&self, a: u32, b: u32, nb: &mut Vec<u32>) -> Option<bool> {
        nb.clear();
        let mut on_edge = 0;
        for r in self.tris_of(a) {
            let (ti, _) = self.refs[r];
            let t = &self.t[ti as usize];
            if t.deleted {
                continue;
            }
            if t.v.contains(&b) {
                on_edge += 1;
            }
            for &x in &t.v {
                if x != a && !nb.contains(&x) {
                    nb.push(x);
                }
            }
        }
        if on_edge == 0 {
            return None;
        }
        let mut seen: Vec<u32> = Vec::new();
        for r in self.tris_of(b) {
            let (ti, _) = self.refs[r];
            let t = &self.t[ti as usize];
            if t.deleted {
                continue;
            }
            for &x in &t.v {
                if x != a && x != b && nb.contains(&x) && !seen.contains(&x) {
                    seen.push(x);
                }
            }
        }
        (seen.len() == on_edge).then_some(on_edge == 1)
    }

    /// Point `v`'s triangles at `into`; the ones marked in `gone` are deleted.
    fn retarget(&mut self, into: u32, v: u32, gone: &[bool], deleted: &mut usize) {
        for (k, r) in self.tris_of(v).enumerate() {
            let (ti, c) = self.refs[r];
            let ti = ti as usize;
            if self.t[ti].deleted {
                continue;
            }
            if gone[k] {
                self.t[ti].deleted = true;
                *deleted += 1;
                continue;
            }
            self.t[ti].v[c as usize] = into;
            self.t[ti].dirty = true;
            self.edge_errors(ti);
            self.refs.push((ti as u32, c));
        }
    }
}

/// A mesh as plain arrays, to decimate on another thread.
pub struct Raw {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Per triangle, or empty.
    pub groups: Vec<u16>,
    /// Per vertex, or empty.
    pub colors: Vec<[u8; 4]>,
}

impl Raw {
    pub fn of(mesh: &SculptMesh) -> Self {
        Self { positions: mesh.positions.clone(), indices: mesh.indices.clone(), groups: mesh.groups.clone(), colors: mesh.colors.clone() }
    }
}

/// Decimate `mesh` to about `s.keep_percent` percent of its triangles. `None` when there is
/// nothing to decimate.
#[cfg(test)]
pub fn decimate(mesh: &SculptMesh, s: &Settings) -> Option<Decimated> {
    decimate_raw(&Raw::of(mesh), s)
}

/// [`decimate`] on plain arrays.
pub fn decimate_raw(mesh: &Raw, s: &Settings) -> Option<Decimated> {
    let before = mesh.indices.len() / 3;
    let target = ((before as f64 * s.keep_percent.clamp(0.1, 100.0) as f64 / 100.0).round() as usize).max(4);
    if before <= target || before < 8 {
        return None;
    }
    // Work at unit size, so the thresholds below fit every model.
    let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let centre = ((lo + hi) * 0.5).as_dvec3();
    let size = ((hi - lo).length() as f64).max(1e-12);
    let mut sim = Simplifier {
        v: mesh.positions.iter().map(|p| Vert { p: (p.as_dvec3() - centre) / size, ..Default::default() }).collect(),
        t: mesh.indices.chunks_exact(3).map(|c| Tri { v: [c[0], c[1], c[2]], err: [0.0; 4], deleted: false, dirty: false, n: DVec3::ZERO }).collect(),
        refs: Vec::new(),
        groups: if mesh.groups.len() == before { mesh.groups.clone() } else { Vec::new() },
        colors: if s.keep_paint && mesh.colors.len() == mesh.positions.len() { mesh.colors.clone() } else { Vec::new() },
    };
    // Quadrics: each triangle's plane, weighted by its area.
    for t in &mut sim.t {
        let [a, b, c] = t.v.map(|k| sim.v[k as usize].p);
        let n = (b - a).cross(c - a);
        let area = n.length() * 0.5;
        t.n = n.normalize_or_zero();
        let q = Quadric::plane(t.n, -t.n.dot(a), area.max(1e-18));
        for &k in &t.v {
            sim.v[k as usize].q.add(&q);
        }
    }
    // Borders to keep: a plane through each border edge, across the surface, weighted far
    // above any triangle.
    if s.keep_borders || s.keep_groups {
        // Every edge once per triangle, sorted so an edge's triangles sit together.
        let mut edges: Vec<(u32, u32, u32)> = (0..sim.t.len() as u32)
            .into_par_iter()
            .flat_map_iter(|i| {
                let v = sim.t[i as usize].v;
                (0..3).map(move |k| {
                    let (a, b) = (v[k], v[(k + 1) % 3]);
                    (a.min(b), a.max(b), i)
                })
            })
            .collect();
        edges.par_sort_unstable();
        let mut k = 0;
        while k < edges.len() {
            let (a, b, ti) = edges[k];
            let mut end = k + 1;
            while end < edges.len() && edges[end].0 == a && edges[end].1 == b {
                end += 1;
            }
            let g = |e: &(u32, u32, u32)| sim.groups.get(e.2 as usize).copied().unwrap_or(0);
            let border = end - k == 1 && s.keep_borders;
            let group_border = s.keep_groups && edges[k + 1..end].iter().any(|e| g(e) != g(&edges[k]));
            k = end;
            if !(border || group_border) {
                continue;
            }
            let (pa, pb) = (sim.v[a as usize].p, sim.v[b as usize].p);
            let along = pb - pa;
            let across = along.cross(sim.t[ti as usize].n).normalize_or_zero();
            if across == DVec3::ZERO {
                continue;
            }
            let w = along.length_squared() * 1e3;
            let q = Quadric::plane(across, -across.dot(pa), w);
            sim.v[a as usize].q.add(&q);
            sim.v[b as usize].q.add(&q);
        }
    }
    sim.rebuild_refs();
    {
        let v = &sim.v;
        let cost = |a: u32, b: u32| Simplifier::cost_in(v, a, b).0;
        sim.t.par_iter_mut().for_each(|t| {
            let mut e = [0.0; 4];
            for k in 0..3 {
                e[k] = cost(t.v[k], t.v[(k + 1) % 3]);
            }
            e[3] = e[0].min(e[1]).min(e[2]);
            t.err = e;
        });
    }

    let mut deleted = 0usize;
    let (mut gone0, mut gone1, mut nb) = (Vec::new(), Vec::new(), Vec::new());
    // How much of the cheapest end a sweep takes on; doubled when a sweep gets nowhere.
    let mut boost = 1.0f64;
    let mut samples: Vec<f64> = Vec::new();
    for sweep in 0..1000 {
        if crate::jobs::cancelled() {
            return None;
        }
        let remaining = before - deleted;
        if remaining <= target {
            break;
        }
        if sweep % 5 == 0 && sweep > 0 {
            sim.compact();
        }
        for t in &mut sim.t {
            t.dirty = false;
        }
        // The threshold: the cost below which about the slice still to go lies (at most a
        // quarter of what is left per sweep), from a sample of the triangles.
        let n = sim.t.len();
        let frac = ((remaining - target) as f64 / remaining as f64 * 0.5 * boost).clamp(0.01, 1.0).min(0.25 * boost);
        samples.clear();
        samples.extend(sim.t.iter().step_by((n / 65_536).max(1)).filter(|t| !t.deleted).map(|t| t.err[3]));
        if samples.is_empty() {
            break;
        }
        let k = ((samples.len() as f64 * frac) as usize).min(samples.len() - 1);
        let threshold = *samples.select_nth_unstable_by(k, f64::total_cmp).1;
        let deleted_before = deleted;
        for i in 0..n {
            if sim.t[i].err[3] > threshold || sim.t[i].deleted || sim.t[i].dirty {
                continue;
            }
            for j in 0..3 {
                if sim.t[i].err[j] > threshold {
                    continue;
                }
                let (a, b) = (sim.t[i].v[j], sim.t[i].v[(j + 1) % 3]);
                let Some(_border_edge) = sim.manifold_collapse(a, b, &mut nb) else { continue };
                let (_, p) = sim.cost(a, b);
                if sim.flips(p, a, b, &mut gone0) || sim.flips(p, b, a, &mut gone1) {
                    continue;
                }
                // Colour at the new point, along the edge.
                if !sim.colors.is_empty() {
                    let (pa, pb) = (sim.v[a as usize].p, sim.v[b as usize].p);
                    let ab = pb - pa;
                    let f = ((p - pa).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0) as f32;
                    let (ca, cb) = (sim.colors[a as usize], sim.colors[b as usize]);
                    sim.colors[a as usize] = std::array::from_fn(|k| (ca[k] as f32 + (cb[k] as f32 - ca[k] as f32) * f).round() as u8);
                }
                sim.v[a as usize].p = p;
                let qb = sim.v[b as usize].q;
                sim.v[a as usize].q.add(&qb);
                let start = sim.refs.len() as u32;
                sim.retarget(a, a, &gone0, &mut deleted);
                sim.retarget(a, b, &gone1, &mut deleted);
                let count = sim.refs.len() as u32 - start;
                sim.v[a as usize].tstart = start;
                sim.v[a as usize].tcount = count;
                sim.v[b as usize].tcount = 0;
                break;
            }
            if before - deleted <= target {
                break;
            }
        }
        if deleted == deleted_before {
            if boost >= 8.0 {
                break;
            }
            boost *= 2.0;
        } else {
            boost = 1.0;
        }
    }
    sim.compact();

    // Back to the model's size; unused vertices dropped.
    let mut map = vec![u32::MAX; sim.v.len()];
    let mut positions = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::with_capacity(sim.t.len() * 3);
    for t in &sim.t {
        for &k in &t.v {
            if map[k as usize] == u32::MAX {
                map[k as usize] = positions.len() as u32;
                positions.push((sim.v[k as usize].p * size + centre).as_vec3());
                if !sim.colors.is_empty() {
                    colors.push(sim.colors[k as usize]);
                }
            }
            indices.push(map[k as usize]);
        }
    }
    let after = indices.len() / 3;
    let groups = if sim.groups.iter().any(|&g| g != 0) { sim.groups } else { Vec::new() };
    Some(Decimated { mesh: SculptMesh::from_raw_painted(positions, indices, groups, colors), before, after })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn edge_counts(m: &SculptMesh) -> HashMap<(u32, u32), u32> {
        let mut c: HashMap<(u32, u32), u32> = HashMap::new();
        for t in m.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *c.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        c
    }

    fn bumpy(seg: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(seg, 1.0);
        let p = p.into_iter().map(|q| {
            let d = q.normalize();
            d * (1.0 + 0.05 * (d.x * 6.0).sin() * (d.y * 5.0).cos())
        }).collect();
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn a_sphere_keeps_its_shape_closed_at_a_tenth() {
        let m = bumpy(60);
        let r = decimate(&m, &Settings { keep_percent: 10.0, ..Default::default() }).unwrap();
        let target = r.before / 10;
        assert!(r.after <= target + target / 50 && r.after > target * 9 / 10, "{} → {} (target {target})", r.before, r.after);
        // Closed and manifold: every edge between exactly two triangles.
        let open = edge_counts(&r.mesh).values().filter(|&&k| k != 2).count();
        assert_eq!(open, 0);
        // On the original surface: every vertex within a small distance of the bumpy shape.
        let off = r.mesh.positions.iter().map(|p| {
            let d = p.normalize();
            (p.length() - (1.0 + 0.05 * (d.x * 6.0).sin() * (d.y * 5.0).cos())).abs()
        }).fold(0.0f32, f32::max);
        assert!(off < 0.01, "off the shape by {off}");
    }

    #[test]
    fn flat_parts_go_first_and_detail_stays() {
        // A plane with one sharp ridge: the flat part takes few triangles, the ridge many.
        let (p, i) = crate::primitive::Primitive::Plane.build(80, 1.0);
        let p: Vec<Vec3> = p.into_iter().map(|q| Vec3::new(q.x, q.y, q.z + 0.1 * (-(q.x / 0.05).powi(2)).exp())).collect();
        let m = SculptMesh::from_raw(p, i);
        let r = decimate(&m, &Settings { keep_percent: 5.0, ..Default::default() }).unwrap();
        let near = r.mesh.indices.chunks_exact(3).filter(|t| t.iter().all(|&v| r.mesh.positions[v as usize].x.abs() < 0.15)).count();
        assert!(near * 2 > r.after, "{near} of {} triangles on the ridge", r.after);
    }

    #[test]
    fn borders_stay_put_with_keep_borders() {
        let (p, i) = crate::primitive::Primitive::Plane.build(60, 1.0);
        let m = SculptMesh::from_raw(p, i);
        let (lo, hi) = m.positions.iter().fold((Vec3::splat(9.0), Vec3::splat(-9.0)), |(a, b), p| (a.min(*p), b.max(*p)));
        let r = decimate(&m, &Settings { keep_percent: 5.0, ..Default::default() }).unwrap();
        let (lo2, hi2) = r.mesh.positions.iter().fold((Vec3::splat(9.0), Vec3::splat(-9.0)), |(a, b), p| (a.min(*p), b.max(*p)));
        assert!((lo - lo2).length() < 1e-4 && (hi - hi2).length() < 1e-4, "{lo} {hi} → {lo2} {hi2}");
        // Every border vertex still lies on the square's outline.
        let counts = edge_counts(&r.mesh);
        for (&(a, b), &c) in &counts {
            if c == 1 {
                for v in [a, b] {
                    let p = r.mesh.positions[v as usize];
                    let on = (p.x - lo.x).abs() < 1e-4 || (p.x - hi.x).abs() < 1e-4 || (p.y - lo.y).abs() < 1e-4 || (p.y - hi.y).abs() < 1e-4;
                    assert!(on, "border vertex {p} off the outline");
                }
            }
        }
    }

    #[test]
    fn group_borders_and_paint_are_kept() {
        let mut m = bumpy(40);
        let groups: Vec<u16> = m.indices.chunks_exact(3).map(|t| (m.positions[t[0] as usize].y + m.positions[t[1] as usize].y + m.positions[t[2] as usize].y > 0.0) as u16).collect();
        let colors: Vec<[u8; 4]> = m.positions.iter().map(|p| if p.x > 0.0 { [220, 40, 40, 255] } else { [40, 40, 220, 255] }).collect();
        m = SculptMesh::from_raw_painted(m.positions.clone(), m.indices.clone(), groups, colors);
        let r = decimate(&m, &Settings { keep_percent: 15.0, ..Default::default() }).unwrap();
        assert_eq!(r.mesh.group_count(), 2);
        // The group border stays near y = 0 (the cube sphere's equator triangles straddle it).
        for (t, tri) in r.mesh.indices.chunks_exact(3).enumerate() {
            let y = tri.iter().map(|&v| r.mesh.positions[v as usize].y).sum::<f32>() / 3.0;
            let g = r.mesh.group(t as u32);
            assert!(if g == 1 { y > -0.08 } else { y < 0.08 }, "group {g} triangle at y {y}");
        }
        // Paint stays on its side.
        for (p, c) in r.mesh.positions.iter().zip(&r.mesh.colors) {
            if p.x > 0.1 {
                assert!(c[0] > 150 && c[2] < 100, "{c:?} at {p}");
            } else if p.x < -0.1 {
                assert!(c[2] > 150 && c[0] < 100, "{c:?} at {p}");
            }
        }
        let r = decimate(&m, &Settings { keep_percent: 15.0, keep_paint: false, ..Default::default() }).unwrap();
        assert!(r.mesh.colors.is_empty());
    }

    /// `cargo test --release decimate_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decimate_speed() {
        for seg in [300u32, 600] {
            let m = bumpy(seg);
            let t = std::time::Instant::now();
            let r = decimate(&m, &Settings::default()).unwrap();
            eprintln!("{} → {} triangles in {:.2} s", r.before, r.after, t.elapsed().as_secs_f32());
        }
    }
}
