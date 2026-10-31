//! ProjectAll (ZBrush: SubTool › Project › ProjectAll): the shape and polypaint of the
//! other visible subtools copied onto the active one.
//!
//! The retopology workflow: duplicate a sculpt, ZRemesh the copy, then Divide it and
//! ProjectAll at each level so the clean mesh takes on the sculpt's detail. Every vertex of
//! the shown level looks along its normal, outward and inward, for the nearest surface of a
//! source within **Dist**, and moves onto it (masked vertices less, hidden ones not at all).
//! Only surfaces facing the same way count, so a vertex is not pulled through to the far
//! side of a thin part. **PA Blur** then smooths the moves to calm noisy spots.
//!
//! The sources go into one bounding volume hierarchy of their triangles, so millions of
//! short rays stay cheap.

use crate::mesh::SculptMesh;
use glam::Vec3;
use rayon::prelude::*;

/// ProjectAll settings (ZBrush: SubTool › Project).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// How far a vertex may move, as a percentage of the active subtool's size (its bounds'
    /// diagonal).
    pub dist: f32,
    /// Smoothing passes over the moves, 0..=10 (ZBrush: PA Blur).
    pub blur: f32,
    /// Look outward and inward along the normal (ZBrush: Outer, Inner).
    pub outer: bool,
    pub inner: bool,
    /// Take the sources' polypaint too.
    pub paint: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { dist: 4.0, blur: 0.0, outer: true, inner: true, paint: true }
    }
}

/// What a projection changed.
#[derive(Clone, Debug, Default)]
pub struct Projection {
    pub positions: Vec<Vec3>,
    /// New colours when a source had paint (and Settings::paint is on).
    pub colors: Option<Vec<[u8; 4]>>,
    /// Vertices that found a surface, and ones that did not (too far or facing away).
    pub moved: usize,
    pub missed: usize,
}

/// The triangles of every source, with their colours, in a bounding volume hierarchy.
pub struct Sources {
    p: Vec<Vec3>,
    tri: Vec<[u32; 3]>,
    /// Per vertex; empty when no source has paint.
    colors: Vec<[u8; 4]>,
    nodes: Vec<Node>,
}

#[derive(Clone, Copy)]
struct Node {
    min: Vec3,
    max: Vec3,
    /// A leaf holds `count` triangles from `start`; an inner node's children are the next
    /// node and `start`.
    start: u32,
    count: u32,
}

/// The nearest hit along a ray: distance, triangle and barycentrics of b and c.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f32,
    pub tri: u32,
    pub u: f32,
    pub v: f32,
    pub normal: Vec3,
}

const LEAF: usize = 4;

impl Sources {
    /// Every visible triangle of `meshes`.
    pub fn new(meshes: &[&SculptMesh]) -> Self {
        let painted = meshes.iter().any(|m| m.colors.iter().any(|c| c[3] > 0));
        let (mut p, mut tri, mut colors) = (Vec::new(), Vec::new(), Vec::new());
        for m in meshes {
            let base = p.len() as u32;
            p.extend_from_slice(&m.positions);
            if painted {
                if m.colors.len() == m.positions.len() {
                    colors.extend_from_slice(&m.colors);
                } else {
                    colors.extend(std::iter::repeat_n([0u8; 4], m.positions.len()));
                }
            }
            for (t, c) in m.indices.chunks_exact(3).enumerate() {
                if m.hidden.is_empty() || m.hidden[t] == 0 {
                    tri.push([c[0] + base, c[1] + base, c[2] + base]);
                }
            }
        }
        let mut s = Self { p, tri, colors, nodes: Vec::new() };
        s.build();
        s
    }

    /// Median splits along the longest axis of the triangles' centres, laid out depth
    /// first so an inner node's first child follows it.
    fn build(&mut self) {
        let mut tri = std::mem::take(&mut self.tri);
        self.nodes = if tri.is_empty() { Vec::new() } else { Self::split(&self.p, &mut tri, 0) };
        self.tri = tri;
    }

    /// The nodes over `tri`, which starts at triangle `first`; a node's `start` is
    /// relative to the returned list for inner nodes, absolute for leaves. Large halves are
    /// built in parallel.
    fn split(p: &[Vec3], tri: &mut [[u32; 3]], first: usize) -> Vec<Node> {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for t in tri.iter() {
            for &v in t {
                lo = lo.min(p[v as usize]);
                hi = hi.max(p[v as usize]);
            }
        }
        let n = tri.len();
        if n <= LEAF {
            return vec![Node { min: lo, max: hi, start: first as u32, count: n as u32 }];
        }
        let ext = hi - lo;
        let axis = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
        let centre = |t: &[u32; 3]| p[t[0] as usize][axis] + p[t[1] as usize][axis] + p[t[2] as usize][axis];
        let mid = n / 2;
        tri.select_nth_unstable_by(mid, |x, y| centre(x).total_cmp(&centre(y)));
        let (left, right) = tri.split_at_mut(mid);
        let (l, r) = if n > 50_000 {
            rayon::join(|| Self::split(p, left, first), || Self::split(p, right, first + mid))
        } else {
            (Self::split(p, left, first), Self::split(p, right, first + mid))
        };
        let mut nodes = Vec::with_capacity(1 + l.len() + r.len());
        let second = 1 + l.len();
        nodes.push(Node { min: lo, max: hi, start: second as u32, count: 0 });
        let shift = |v: &Vec<Node>, by: usize| v.iter().map(move |x| if x.count == 0 { Node { start: x.start + by as u32, ..*x } } else { *x }).collect::<Vec<_>>();
        nodes.extend(shift(&l, 1));
        nodes.extend(shift(&r, second));
        nodes
    }

    /// The nearest triangle hit by the ray from `o` along unit `d` within `tmax`.
    pub fn hit(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv = Vec3::ONE / d;
        let mut best: Option<Hit> = None;
        let mut limit = tmax;
        let mut stack = [0u32; 64];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let k = stack[top] as usize;
            let node = self.nodes[k];
            if !slab(o, inv, node.min, node.max, limit) {
                continue;
            }
            if node.count > 0 {
                for t in node.start..node.start + node.count {
                    let [a, b, c] = self.tri[t as usize];
                    let (a, b, c) = (self.p[a as usize], self.p[b as usize], self.p[c as usize]);
                    if let Some((t_hit, u, v)) = ray_triangle(o, d, a, b, c) {
                        if t_hit <= limit {
                            limit = t_hit;
                            best = Some(Hit { t: t_hit, tri: t, u, v, normal: (b - a).cross(c - a).normalize_or_zero() });
                        }
                    }
                }
            } else if top + 2 <= stack.len() {
                stack[top] = node.start;
                stack[top + 1] = k as u32 + 1;
                top += 2;
            }
        }
        best
    }

    /// The colour at a hit, if the sources have paint there.
    fn color(&self, h: &Hit) -> Option<[u8; 4]> {
        if self.colors.is_empty() {
            return None;
        }
        let [a, b, c] = self.tri[h.tri as usize];
        let w = [1.0 - h.u - h.v, h.u, h.v];
        let cs = [self.colors[a as usize], self.colors[b as usize], self.colors[c as usize]];
        let mut out = [0u8; 4];
        for ch in 0..4 {
            out[ch] = (0..3).map(|k| cs[k][ch] as f32 * w[k]).sum::<f32>().round().clamp(0.0, 255.0) as u8;
        }
        (out[3] > 0).then_some(out)
    }
}

/// Whether the ray enters the box before `tmax`.
fn slab(o: Vec3, inv: Vec3, lo: Vec3, hi: Vec3, tmax: f32) -> bool {
    let t0 = (lo - o) * inv;
    let t1 = (hi - o) * inv;
    let near = t0.min(t1).max_element().max(0.0);
    let far = t0.max(t1).min_element().min(tmax);
    near <= far
}

/// Möller–Trumbore, with the barycentrics of b and c. A ray through an edge or corner
/// counts for every triangle there (a hair of tolerance), so it cannot slip between them.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<(f32, f32, f32)> {
    const TOL: f32 = 1e-5;
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(-TOL..=1.0 + TOL).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < -TOL || u + v > 1.0 + TOL {
        return None;
    }
    let t = e2.dot(q) * inv;
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let k = (u + v).max(1.0);
    (t >= 0.0).then_some((t, u / k, v / k))
}

/// Project `sources` onto `mesh` with `s`.
pub fn project(mesh: &SculptMesh, sources: &Sources, s: &Settings) -> Projection {
    let n = mesh.vertex_count();
    let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)));
    let size = (hi - lo).length().max(1e-9);
    let dist = s.dist.max(0.0) / 100.0 * size;
    // A ray starts a hair behind the vertex, so a vertex already on a source finds it.
    let eps = size * 1e-6;
    let paint = s.paint && !sources.colors.is_empty();
    // Per vertex: how far it moves and the colour it takes, or None for no surface found.
    let found: Vec<Option<(Vec3, Option<[u8; 4]>)>> = (0..n as u32)
        .into_par_iter()
        .map(|v| {
            let vi = v as usize;
            if !mesh.shown.is_empty() && mesh.shown[vi] == 0 {
                return None;
            }
            let p = mesh.positions[vi];
            let nv = mesh.normal_at(v);
            let mut best: Option<Hit> = None;
            let mut best_dir = Vec3::ZERO;
            for (dir, on) in [(nv, s.outer), (-nv, s.inner)] {
                if !on {
                    continue;
                }
                if let Some(h) = sources.hit(p - dir * eps, dir, dist + eps) {
                    // The surface has to face the way the vertex does.
                    if h.normal.dot(nv) > 0.0 && best.is_none_or(|b| h.t < b.t) {
                        best = Some(h);
                        best_dir = dir;
                    }
                }
            }
            best.map(|h| ((h.t - eps) * best_dir, if paint { sources.color(&h) } else { None }))
        })
        .collect();
    let moved = found.iter().filter(|f| f.is_some()).count();
    let missed = (0..n).filter(|&v| found[v].is_none() && (mesh.shown.is_empty() || mesh.shown[v] != 0)).count();
    // PA Blur: each pass moves every found vertex halfway to the mean move of its found
    // neighbours.
    let mut delta: Vec<Vec3> = found.iter().map(|f| f.map_or(Vec3::ZERO, |x| x.0)).collect();
    for _ in 0..s.blur.round().clamp(0.0, 10.0) as u32 {
        delta = (0..n)
            .into_par_iter()
            .map(|v| {
                if found[v].is_none() {
                    return Vec3::ZERO;
                }
                let (a, b) = (mesh.adj_offsets[v] as usize, mesh.adj_offsets[v + 1] as usize);
                let (sum, k) = mesh.adj_verts[a..b]
                    .iter()
                    .filter(|&&w| found[w as usize].is_some())
                    .fold((Vec3::ZERO, 0.0f32), |(s, k), &w| (s + delta[w as usize], k + 1.0));
                if k == 0.0 { delta[v] } else { delta[v] * 0.5 + sum / k * 0.5 }
            })
            .collect();
    }
    let positions = (0..n)
        .into_par_iter()
        .map(|v| mesh.positions[v] + delta[v] * crate::mask::freedom(&mesh.mask, v as u32))
        .collect();
    let colors = paint.then(|| {
        let mut c = if mesh.colors.len() == n { mesh.colors.clone() } else { vec![[0u8; 4]; n] };
        for v in 0..n {
            if let Some((_, Some(col))) = found[v] {
                let f = crate::mask::freedom(&mesh.mask, v as u32);
                if f >= 1.0 {
                    c[v] = col;
                } else if f > 0.0 {
                    for ch in 0..4 {
                        c[v][ch] = (c[v][ch] as f32 + (col[ch] as f32 - c[v][ch] as f32) * f).round() as u8;
                    }
                }
            }
        }
        c
    });
    Projection { positions, colors, moved, missed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(seg: u32, r: f32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(seg, r);
        SculptMesh::from_raw(p, i)
    }

    /// A unit sphere with bumps: the radius in direction `d`.
    fn bumpy_radius(d: Vec3) -> f32 {
        1.0 + 0.04 * (d.x * 6.0).sin() * (d.y * 5.0).cos() + 0.03 * (d.z * 7.0).sin()
    }

    fn bumpy(seg: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(seg, 1.0);
        let p = p.into_iter().map(|q| q.normalize() * bumpy_radius(q.normalize())).collect();
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn the_hierarchy_finds_the_same_hits_as_every_triangle() {
        let m = bumpy(12);
        let s = Sources::new(&[&m]);
        for k in 0..200 {
            let a = k as f32 * 0.37;
            let d = Vec3::new(a.cos() * (a * 0.3).sin(), (a * 0.7).cos(), a.sin()).normalize();
            let o = Vec3::new(0.1, -0.05, 0.02);
            let brute = m.indices.chunks_exact(3).filter_map(|t| ray_triangle(o, d, m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]).map(|h| h.0)).fold(f32::INFINITY, f32::min);
            let h = s.hit(o, d, 10.0).map_or(f32::INFINITY, |h| h.t);
            assert!((brute - h).abs() < 1e-5, "ray {k}: {brute} vs {h}");
        }
    }

    #[test]
    fn a_smooth_sphere_takes_on_the_bumps() {
        let source = bumpy(80);
        let target = sphere(30, 1.0);
        let sources = Sources::new(&[&source]);
        let r = project(&target, &sources, &Settings::default());
        assert_eq!(r.missed, 0, "{} moved", r.moved);
        let err = r.positions.iter().map(|p| (p.length() - bumpy_radius(p.normalize())).abs()).fold(0.0f32, f32::max);
        assert!(err < 0.004, "off the bumpy surface by up to {err}");
        // Before, the smooth sphere was off by up to the bumps' height.
        let before = target.positions.iter().map(|p| (p.length() - bumpy_radius(p.normalize())).abs()).fold(0.0f32, f32::max);
        assert!(before > 0.03, "{before}");
    }

    #[test]
    fn dist_inner_outer_and_the_mask_limit_it() {
        let source = sphere(40, 1.0);
        let sources = Sources::new(&[&source]);
        // A smaller sphere sits inside: only outward rays reach the source.
        let inside = sphere(20, 0.95);
        let r = project(&inside, &sources, &Settings { inner: true, outer: false, ..Default::default() });
        assert_eq!(r.moved, 0, "nothing inward");
        let r = project(&inside, &sources, &Settings::default());
        assert!(r.positions.iter().all(|p| (p.length() - 1.0).abs() < 0.003));
        // Too far for a small Dist (a sphere of radius 0.95 is 3.8 across: 0.05 is 1.3 %).
        let r = project(&inside, &sources, &Settings { dist: 1.0, ..Default::default() });
        assert_eq!(r.moved, 0);
        // A masked half stays.
        let mut masked = sphere(20, 0.95);
        masked.mask = masked.positions.iter().map(|p| if p.x > 0.0 { 255 } else { 0 }).collect();
        let r = project(&masked, &sources, &Settings::default());
        for (p, q) in r.positions.iter().zip(&masked.positions) {
            if q.x > 0.0 {
                assert_eq!(p, q);
            } else {
                assert!((p.length() - 1.0).abs() < 0.003);
            }
        }
    }

    #[test]
    fn the_far_side_of_a_thin_part_is_left_alone() {
        // A thin shell: a sphere of radius 1 and one of 0.97 turned inside out. A vertex
        // between them looking inward sees the inner sheet, which faces the other way.
        let outer = sphere(30, 1.0);
        let (p, mut i) = crate::mesh::cube_sphere(30, 0.97);
        for t in i.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
        let inner = SculptMesh::from_raw(p, i);
        let sources = Sources::new(&[&outer, &inner]);
        let target = sphere(16, 0.985);
        let r = project(&target, &sources, &Settings::default());
        assert!(r.positions.iter().all(|p| (p.length() - 1.0).abs() < 0.003), "pulled to the inner sheet");
    }

    #[test]
    fn paint_comes_along() {
        let mut source = sphere(30, 1.0);
        source.colors = source.positions.iter().map(|p| if p.y > 0.0 { [200, 30, 30, 255] } else { [0; 4] }).collect();
        let sources = Sources::new(&[&source]);
        let target = sphere(16, 0.99);
        let r = project(&target, &sources, &Settings::default());
        let c = r.colors.expect("the source has paint");
        for (p, col) in target.positions.iter().zip(&c) {
            if p.y > 0.15 {
                assert_eq!(*col, [200, 30, 30, 255]);
            } else if p.y < -0.15 {
                assert_eq!(col[3], 0);
            }
        }
        let r = project(&target, &sources, &Settings { paint: false, ..Default::default() });
        assert!(r.colors.is_none());
    }

    /// `cargo test --release project_all_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn project_all_speed() {
        let source = bumpy(400);
        let target = sphere(300, 1.0);
        let t = std::time::Instant::now();
        let sources = Sources::new(&[&source]);
        let built = t.elapsed();
        let r = project(&target, &sources, &Settings::default());
        eprintln!(
            "{} source triangles: hierarchy {:.0} ms; {} vertices projected in {:.0} ms ({} missed)",
            source.triangle_count(), built.as_secs_f32() * 1000.0, target.vertex_count(), (t.elapsed() - built).as_secs_f32() * 1000.0, r.missed
        );
    }

    #[test]
    fn pa_blur_smooths_the_moves() {
        let source = bumpy(80);
        let target = sphere(30, 1.0);
        let sources = Sources::new(&[&source]);
        let sharp = project(&target, &sources, &Settings::default());
        let soft = project(&target, &sources, &Settings { blur: 10.0, ..Default::default() });
        let spread = |ps: &[Vec3]| {
            let l: Vec<f32> = ps.iter().map(|p| p.length()).collect();
            let mean = l.iter().sum::<f32>() / l.len() as f32;
            l.iter().map(|x| (x - mean).powi(2)).sum::<f32>()
        };
        assert!(spread(&soft.positions) < spread(&sharp.positions) * 0.9);
    }
}
