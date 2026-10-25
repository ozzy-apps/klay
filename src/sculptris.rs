//! Sculptris Pro (ZBrush): the mesh is retessellated under the brush as you sculpt, so
//! detail goes where you work and the triangles there stay even, whatever the mesh was
//! before. Edges near the brush longer than 4/3 of the target length are split, shorter
//! than 4/5 collapsed, edges are flipped toward valence six, and vertices are relaxed along
//! the surface (Botsch & Kobbelt's isotropic remeshing, applied locally). The target follows
//! Draw Size (ZBrush's adaptive size): a small brush makes small triangles.
//!
//! Only the triangles touching a vertex within reach of a brush centre change. Their
//! vertices on the edge of that region (touching a triangle outside it) stay where they
//! are, as do masked vertices, mesh borders and hidden triangles, and polygroup borders are
//! never collapsed or flipped. The new mesh keeps every vertex in its chunk (new ones join
//! their parent's) and is rebuilt with [`SculptMesh::from_assigned`].

use crate::mesh::SculptMesh;
use glam::Vec3;
use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};

/// The target edge length for a brush of `radius` at Detail `detail` (0..=100): 50 makes
/// edges a fifth of the radius, every 25 halves them.
pub fn target_edge(radius: f32, detail: f32) -> f32 {
    radius * 0.8 * 0.5f32.powf(detail.clamp(0.0, 100.0) / 25.0)
}

/// Most vertices one pass may add, so a huge brush with fine detail stays responsive.
const MAX_NEW: usize = 400_000;

/// What a pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub splits: usize,
    pub collapses: usize,
    pub flips: usize,
}

impl Stats {
    pub fn changed(&self) -> bool {
        self.splits + self.collapses + self.flips > 0
    }
}

/// The region being remeshed, with its own vertex and triangle numbers.
struct Local {
    pos: Vec<Vec3>,
    nrm: Vec<Vec3>,
    mask: Vec<u8>,
    color: Vec<[u8; 4]>,
    chunk: Vec<u32>,
    /// Vertex number in the mesh; `u32::MAX` for new vertices.
    global: Vec<u32>,
    /// Stays put: on the region's edge, a mesh border, or masked.
    fixed: Vec<bool>,
    dead: Vec<bool>,
    tris: Vec<[u32; 3]>,
    group: Vec<u16>,
    alive: Vec<bool>,
    vt: Vec<Vec<u32>>,
    /// Neighbours a vertex has outside the region (fixed vertices), for its valence.
    outside: Vec<u32>,
    /// Pairs of fixed vertices joined by an edge outside the region.
    joined: HashSet<(u32, u32)>,
    centres: Vec<Vec3>,
    reach2: f32,
}

fn tri_normal(a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    (b - a).cross(c - a)
}

impl Local {
    /// The live triangles on edge (a, b), and how many there are.
    fn edge_tris(&self, a: u32, b: u32) -> ([u32; 2], usize) {
        let mut out = [u32::MAX; 2];
        let mut k = 0;
        for &t in &self.vt[a as usize] {
            if self.tris[t as usize].contains(&b) {
                if k < 2 {
                    out[k] = t;
                }
                k += 1;
            }
        }
        (out, k)
    }

    fn neighbours(&self, v: u32) -> Vec<u32> {
        let mut n: Vec<u32> = self.vt[v as usize].iter().flat_map(|&t| self.tris[t as usize]).filter(|&w| w != v).collect();
        n.sort_unstable();
        n.dedup();
        n
    }

    fn near(&self, p: Vec3) -> bool {
        self.centres.iter().any(|c| c.distance_squared(p) <= self.reach2)
    }

    /// Whether segment (a, b) passes within reach of a centre.
    fn crosses(&self, a: u32, b: u32) -> bool {
        let (pa, pb) = (self.pos[a as usize], self.pos[b as usize]);
        let ab = pb - pa;
        let l2 = ab.length_squared().max(1e-20);
        self.centres.iter().any(|&c| {
            let t = ((c - pa).dot(ab) / l2).clamp(0.0, 1.0);
            (pa + ab * t).distance_squared(c) <= self.reach2
        })
    }

    /// Edges at `v`: around a free vertex as many as its triangles; a fixed one's fan in the
    /// region is open, so one more, plus those outside.
    fn valence(&self, v: u32) -> i32 {
        let n = self.vt[v as usize].len() as u32;
        (if self.fixed[v as usize] { n + 1 + self.outside[v as usize] } else { n }) as i32
    }

    /// Every live edge with two triangles once, as (a, b) with a < b: the triangle that
    /// runs a → b lists it (the other runs b → a). Edges with one triangle may be missed;
    /// nothing changes them.
    fn edges(&self) -> Vec<(u32, u32)> {
        let mut e: Vec<(u32, u32)> = Vec::with_capacity(self.tris.len() * 3 / 2 + 8);
        for (t, tri) in self.tris.iter().enumerate() {
            if !self.alive[t] {
                continue;
            }
            for i in 0..3 {
                let (a, b) = (tri[i], tri[(i + 1) % 3]);
                if a < b {
                    e.push((a, b));
                }
            }
        }
        e
    }

    fn len(&self, a: u32, b: u32) -> f32 {
        self.pos[a as usize].distance(self.pos[b as usize])
    }

    /// Vertex normal from the region's triangles around `v`.
    fn normal(&self, v: u32) -> Vec3 {
        let n: Vec3 = self.vt[v as usize]
            .iter()
            .map(|&t| {
                let [a, b, c] = self.tris[t as usize];
                tri_normal(self.pos[a as usize], self.pos[b as usize], self.pos[c as usize])
            })
            .sum();
        n.normalize_or(self.nrm[v as usize])
    }

    fn add_vertex(&mut self, p: Vec3, n: Vec3, mask: u8, color: [u8; 4], chunk: u32) -> u32 {
        self.pos.push(p);
        self.nrm.push(n);
        self.mask.push(mask);
        self.color.push(color);
        self.chunk.push(chunk);
        self.global.push(u32::MAX);
        self.fixed.push(false);
        self.dead.push(false);
        self.vt.push(Vec::new());
        self.outside.push(0);
        (self.pos.len() - 1) as u32
    }

    /// Split edge (a, b) at its midpoint, bent to follow the surface (the midpoint of the
    /// cubic through both ends with tangents in their tangent planes).
    fn split(&mut self, a: u32, b: u32) -> bool {
        let (ts, k) = self.edge_tris(a, b);
        if k != 2 {
            return false;
        }
        let (pa, pb) = (self.pos[a as usize], self.pos[b as usize]);
        let (na, nb) = (self.nrm[a as usize], self.nrm[b as usize]);
        let d = pb - pa;
        let (ta, tb) = (d - na * na.dot(d), d - nb * nb.dot(d));
        let m = (pa + pb) * 0.5 + (ta - tb) * 0.125;
        let mix = |x: u8, y: u8| ((x as u16 + y as u16 + 1) / 2) as u8;
        let (ca, cb) = (self.color[a as usize], self.color[b as usize]);
        let color = [mix(ca[0], cb[0]), mix(ca[1], cb[1]), mix(ca[2], cb[2]), mix(ca[3], cb[3])];
        let mask = mix(self.mask[a as usize], self.mask[b as usize]);
        let m = self.add_vertex(m, (na + nb).normalize_or(na), mask, color, self.chunk[a as usize]);
        for t in ts {
            let tri = self.tris[t as usize];
            let i = (0..3).find(|&i| {
                let (x, y) = (tri[i], tri[(i + 1) % 3]);
                (x == a && y == b) || (x == b && y == a)
            });
            let Some(i) = i else { continue };
            let (x, y, c) = (tri[i], tri[(i + 1) % 3], tri[(i + 2) % 3]);
            self.tris[t as usize] = [x, m, c];
            let t2 = self.tris.len() as u32;
            self.tris.push([m, y, c]);
            self.group.push(self.group[t as usize]);
            self.alive.push(true);
            self.vt[y as usize].retain(|&u| u != t);
            self.vt[y as usize].push(t2);
            self.vt[c as usize].push(t2);
            self.vt[m as usize].extend([t, t2]);
        }
        true
    }

    /// One polygroup around `v`, if its triangles share one.
    fn group_of(&self, v: u32) -> Option<u16> {
        let mut g = None;
        for &t in &self.vt[v as usize] {
            let x = self.group[t as usize];
            if g.is_some_and(|y| y != x) {
                return None;
            }
            g = Some(x);
        }
        g
    }

    /// Collapse edge (a, b) if that keeps the surface sound: the ends share exactly the two
    /// opposite vertices, no triangle turns over or grows an edge past `hi`, and no
    /// polygroup border moves.
    fn collapse(&mut self, a: u32, b: u32, hi: f32) -> bool {
        let (fa, fb) = (self.fixed[a as usize], self.fixed[b as usize]);
        if fa && fb {
            return false;
        }
        // Keep the fixed end, if there is one.
        let (keep, gone) = if fb { (b, a) } else { (a, b) };
        let (ts, k) = self.edge_tris(keep, gone);
        if k != 2 {
            return false;
        }
        match (self.group_of(keep), self.group_of(gone)) {
            (Some(x), Some(y)) if x == y => {}
            _ => return false,
        }
        let opposite: Vec<u32> = ts.iter().map(|&t| self.tris[t as usize].into_iter().find(|&v| v != keep && v != gone).unwrap()).collect();
        let (nk, ng) = (self.neighbours(keep), self.neighbours(gone));
        let shared = nk.iter().filter(|v| ng.binary_search(v).is_ok()).count();
        if shared != 2 || opposite[0] == opposite[1] || nk.len() <= 3 || ng.len() <= 3 {
            return false;
        }
        let p = if fb || fa { self.pos[keep as usize] } else { (self.pos[keep as usize] + self.pos[gone as usize]) * 0.5 };
        for &w in nk.iter().chain(&ng) {
            if w != keep && w != gone && self.pos[w as usize].distance(p) > hi {
                return false;
            }
        }
        for v in [keep, gone] {
            for &t in &self.vt[v as usize] {
                if ts.contains(&t) {
                    continue;
                }
                let tri = self.tris[t as usize];
                let at = |u: u32| if u == keep || u == gone { p } else { self.pos[u as usize] };
                let before = tri_normal(self.pos[tri[0] as usize], self.pos[tri[1] as usize], self.pos[tri[2] as usize]);
                let after = tri_normal(at(tri[0]), at(tri[1]), at(tri[2]));
                if after.length_squared() < before.length_squared() * 1e-4 || after.normalize_or_zero().dot(before.normalize_or_zero()) < 0.3 {
                    return false;
                }
            }
        }
        for t in ts {
            self.alive[t as usize] = false;
            for v in self.tris[t as usize] {
                self.vt[v as usize].retain(|&u| u != t);
            }
        }
        let moved = std::mem::take(&mut self.vt[gone as usize]);
        for &t in &moved {
            for v in &mut self.tris[t as usize] {
                if *v == gone {
                    *v = keep;
                }
            }
        }
        self.vt[keep as usize].extend(moved);
        self.pos[keep as usize] = p;
        self.dead[gone as usize] = true;
        true
    }

    /// Flip edge (a, b) to the other diagonal of its two triangles when that brings the
    /// four vertices' valences nearer six, or keeps them and makes the pair more even (the
    /// angles facing the edge add up to well over half a turn), without turning a triangle
    /// over.
    fn flip(&mut self, a: u32, b: u32) -> bool {
        let (ts, k) = self.edge_tris(a, b);
        if k != 2 || self.group[ts[0] as usize] != self.group[ts[1] as usize] {
            return false;
        }
        // Orient: t0 runs a → b.
        let t0_runs = |t: u32| {
            let tri = self.tris[t as usize];
            (0..3).any(|i| tri[i] == a && tri[(i + 1) % 3] == b)
        };
        let (t0, t1) = if t0_runs(ts[0]) { (ts[0], ts[1]) } else { (ts[1], ts[0]) };
        let third = |t: u32| self.tris[t as usize].into_iter().find(|&v| v != a && v != b).unwrap();
        let (c, d) = (third(t0), third(t1));
        if c == d || self.edge_tris(c, d).1 > 0 || self.joined.contains(&(c.min(d), c.max(d))) {
            return false;
        }
        let (va, vb, vc, vd) = (self.valence(a), self.valence(b), self.valence(c), self.valence(d));
        if va <= 3 || vb <= 3 {
            return false;
        }
        let dev = |x: i32| (x - 6) * (x - 6);
        let (before, after) = (dev(va) + dev(vb) + dev(vc) + dev(vd), dev(va - 1) + dev(vb - 1) + dev(vc + 1) + dev(vd + 1));
        let p = |v: u32| self.pos[v as usize];
        let angle = |at: Vec3, x: Vec3, y: Vec3| (x - at).normalize_or_zero().dot((y - at).normalize_or_zero()).clamp(-1.0, 1.0).acos();
        let facing = angle(p(c), p(a), p(b)) + angle(p(d), p(a), p(b));
        let delaunay = facing > std::f32::consts::PI * 1.08;
        if !(after < before || (after == before && delaunay)) {
            return false;
        }
        let old = (tri_normal(p(a), p(b), p(c)) + tri_normal(p(b), p(a), p(d))).normalize_or_zero();
        let (n0, n1) = (tri_normal(p(c), p(a), p(d)), tri_normal(p(d), p(b), p(c)));
        if n0.normalize_or_zero().dot(old) < 0.5 || n1.normalize_or_zero().dot(old) < 0.5 {
            return false;
        }
        self.tris[t0 as usize] = [c, a, d];
        self.tris[t1 as usize] = [d, b, c];
        self.vt[a as usize].retain(|&u| u != t1);
        self.vt[b as usize].retain(|&u| u != t0);
        self.vt[c as usize].push(t1);
        self.vt[d as usize].push(t0);
        true
    }

    /// Move the free vertices half way to the middle of their neighbours, along the surface
    /// (not those on a polygroup border).
    fn relax(&mut self) {
        let moves: Vec<(usize, Vec3)> = (0..self.pos.len())
            .filter(|&v| !self.fixed[v] && !self.dead[v] && !self.vt[v].is_empty() && self.near(self.pos[v]))
            // Polygroup borders stay where they are.
            .filter(|&v| self.group_of(v as u32).is_some())
            .map(|v| {
                // Around a free vertex every neighbour is in two of its triangles, so the
                // mean over the triangles' other corners is the neighbours' mean.
                let mut sum = Vec3::ZERO;
                for &t in &self.vt[v] {
                    for w in self.tris[t as usize] {
                        if w as usize != v {
                            sum += self.pos[w as usize];
                        }
                    }
                }
                let centre = sum / (2 * self.vt[v].len()) as f32;
                let n = self.normal(v as u32);
                let d = centre - self.pos[v];
                (v, self.pos[v] + (d - n * n.dot(d)) * 0.5)
            })
            .collect();
        // One at a time, and only where no triangle around turns over or flattens away.
        for (v, p) in moves {
            let ok = self.vt[v].iter().all(|&t| {
                let tri = self.tris[t as usize];
                let at = |u: u32| if u as usize == v { p } else { self.pos[u as usize] };
                let before = tri_normal(self.pos[tri[0] as usize], self.pos[tri[1] as usize], self.pos[tri[2] as usize]);
                let after = tri_normal(at(tri[0]), at(tri[1]), at(tri[2]));
                after.length_squared() > before.length_squared() * 0.01 && after.normalize_or_zero().dot(before.normalize_or_zero()) > 0.5
            });
            if ok {
                self.pos[v] = p;
            }
        }
    }
}

/// `mesh` retessellated within `reach` of each of `centres` toward edges `target` long, or
/// `None` when nothing there needed it.
pub fn retessellated(mesh: &SculptMesh, centres: &[Vec3], reach: f32, target: f32) -> Option<(SculptMesh, Stats)> {
    let (hi, lo) = (target * 4.0 / 3.0, target * 0.8);
    // The region: visible triangles touching a vertex within reach (or the nearest few
    // when the brush is smaller than the triangles under it), and one ring more, so the
    // edges where the brush meets coarser triangles can be split too.
    let mut seeds: Vec<u32> = Vec::new();
    for &c in centres {
        let mut r = reach;
        for _ in 0..6 {
            let sel = mesh.select_sphere(c, r);
            if sel.verts.len() >= 3 {
                seeds.extend_from_slice(&sel.verts);
                break;
            }
            r *= 2.0;
        }
    }
    seeds.sort_unstable();
    seeds.dedup();
    let mut region: Vec<u32> = seeds.iter().flat_map(|&v| mesh.incident(v).iter().copied()).filter(|&t| mesh.visible(t)).collect();
    region.sort_unstable();
    region.dedup();
    let ring: Vec<u32> = region.iter().flat_map(|&t| mesh.indices[3 * t as usize..3 * t as usize + 3].iter().copied()).collect();
    region.extend(ring.iter().flat_map(|&v| mesh.incident(v).iter().copied()).filter(|&t| mesh.visible(t)));
    region.sort_unstable();
    region.dedup();
    if region.is_empty() {
        return None;
    }
    let in_region: HashSet<u32> = region.iter().copied().collect();
    let mut local_of: HashMap<u32, u32> = HashMap::default();
    let mut l = Local {
        pos: Vec::new(),
        nrm: Vec::new(),
        mask: Vec::new(),
        color: Vec::new(),
        chunk: Vec::new(),
        global: Vec::new(),
        fixed: Vec::new(),
        dead: Vec::new(),
        tris: Vec::with_capacity(region.len()),
        group: Vec::with_capacity(region.len()),
        alive: Vec::with_capacity(region.len()),
        vt: Vec::new(),
        outside: Vec::new(),
        joined: HashSet::default(),
        centres: centres.to_vec(),
        reach2: reach * reach,
    };
    for &t in &region {
        let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
        let mut lt = [0u32; 3];
        for (k, &g) in tri.iter().enumerate() {
            lt[k] = *local_of.entry(g).or_insert_with(|| {
                let v = g as usize;
                let edge = mesh.incident(g).iter().any(|t| !in_region.contains(t));
                let masked = mesh.mask.get(v).is_some_and(|&m| m > 0);
                l.pos.push(mesh.positions[v]);
                l.nrm.push(mesh.normal_at(g));
                l.mask.push(mesh.mask.get(v).copied().unwrap_or(0));
                l.color.push(mesh.colors.get(v).copied().unwrap_or([0; 4]));
                l.chunk.push(mesh.vert_chunk[v]);
                l.global.push(g);
                l.fixed.push(edge || masked);
                l.dead.push(false);
                l.vt.push(Vec::new());
                l.outside.push(0);
                (l.pos.len() - 1) as u32
            });
        }
        let i = l.tris.len() as u32;
        for &v in &lt {
            l.vt[v as usize].push(i);
        }
        l.tris.push(lt);
        l.group.push(mesh.group(t));
        l.alive.push(true);
    }
    // Neighbours outside the region, for the valence of the vertices on its edge.
    for v in 0..l.pos.len() {
        if l.fixed[v] {
            let g = l.global[v];
            l.outside[v] = mesh.neighbors(g).iter().filter(|w| !local_of.contains_key(w)).count() as u32;
            for w in mesh.neighbors(g) {
                if let Some(&lw) = local_of.get(w) {
                    if l.fixed[lw as usize] {
                        l.joined.insert(((v as u32).min(lw), (v as u32).max(lw)));
                    }
                }
            }
        }
    }
    // Mesh borders inside the region (an edge with one triangle) stay too.
    for v in 0..l.pos.len() as u32 {
        if !l.fixed[v as usize] && l.neighbours(v).iter().any(|&w| l.edge_tris(v, w).1 == 1) {
            l.fixed[v as usize] = true;
        }
    }
    let first_new = l.pos.len();
    let mut stats = Stats::default();
    for _round in 0..3 {
        let before = stats;
        // Split long edges, longest first, until none is left (each pass halves them).
        for _ in 0..8 {
            let mut long: Vec<(f32, u32, u32)> = l
                .edges()
                .into_iter()
                .filter(|&(a, b)| !(l.mask[a as usize] > 127 && l.mask[b as usize] > 127))
                .map(|(a, b)| (l.len(a, b), a, b))
                .filter(|&(d, a, b)| d > hi && l.crosses(a, b))
                .collect();
            if long.is_empty() || l.pos.len() - first_new > MAX_NEW {
                break;
            }
            long.sort_unstable_by(|x, y| y.0.total_cmp(&x.0));
            let mut any = false;
            for (_, a, b) in long {
                if l.len(a, b) > hi && l.split(a, b) {
                    stats.splits += 1;
                    any = true;
                }
            }
            if !any {
                break;
            }
        }
        // Collapse short edges, shortest first.
        let mut short: Vec<(f32, u32, u32)> = l
            .edges()
            .into_iter()
            .map(|(a, b)| (l.len(a, b), a, b))
            .filter(|&(d, a, b)| d < lo && l.near((l.pos[a as usize] + l.pos[b as usize]) * 0.5))
            .collect();
        short.sort_unstable_by(|x, y| x.0.total_cmp(&y.0));
        for (_, a, b) in short {
            if !l.dead[a as usize] && !l.dead[b as usize] && l.len(a, b) < lo && l.collapse(a, b, hi) {
                stats.collapses += 1;
            }
        }
        // Flip toward valence six.
        for (a, b) in l.edges() {
            if l.near((l.pos[a as usize] + l.pos[b as usize]) * 0.5) && l.flip(a, b) {
                stats.flips += 1;
            }
        }
        l.relax();
        if stats == before {
            break;
        }
    }
    if !stats.changed() {
        return None;
    }
    Some((rebuilt(mesh, &l, &region), stats))
}

/// The mesh with the region replaced by `l`'s triangles: new vertices join their parent's
/// chunk, collapsed ones go, and only the chunks this touches are rebuilt.
fn rebuilt(mesh: &SculptMesh, l: &Local, region: &[u32]) -> SculptMesh {
    use crate::mesh::{NewVertex, Patch, VRef};
    let mut patch = Patch { removed_tris: region.to_vec(), ..Patch::default() };
    let mut refs = vec![VRef::New(u32::MAX); l.pos.len()];
    for v in 0..l.pos.len() {
        let g = l.global[v];
        if g != u32::MAX {
            refs[v] = VRef::Old(g);
            if l.dead[v] {
                patch.dead.push(g);
            } else if l.pos[v] != mesh.positions[g as usize] {
                patch.moved.push((g, l.pos[v]));
            }
        } else if !l.dead[v] {
            refs[v] = VRef::New(patch.new_verts.len() as u32);
            patch.new_verts.push(NewVertex { pos: l.pos[v], chunk: l.chunk[v], mask: l.mask[v], color: l.color[v] });
        }
    }
    for (t, tri) in l.tris.iter().enumerate() {
        if l.alive[t] {
            patch.new_tris.push((tri.map(|v| refs[v as usize]), l.group[t]));
        }
    }
    mesh.patched(&patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(segments: u32) -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(segments, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// Every edge has exactly two triangles, and every triangle faces out of the sphere.
    fn closed_and_outward(m: &SculptMesh) -> bool {
        let mut count: HashMap<(u32, u32), u32> = HashMap::default();
        for t in m.indices.chunks_exact(3) {
            for i in 0..3 {
                *count.entry((t[i], t[(i + 1) % 3])).or_default() += 1;
            }
            let n = tri_normal(m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            if n.dot(m.positions[t[0] as usize]) <= 0.0 {
                eprintln!("inward triangle {t:?} at {}", m.positions[t[0] as usize]);
                return false;
            }
        }
        // Each directed edge once, and its reverse once.
        let bad: Vec<_> = count.iter().filter(|&(&(a, b), &k)| !(k == 1 && count.get(&(b, a)) == Some(&1))).collect();
        if !bad.is_empty() {
            eprintln!("{} bad edges, e.g. {:?}", bad.len(), &bad[..bad.len().min(4)]);
        }
        bad.is_empty()
    }

    fn edge_lengths_near(m: &SculptMesh, c: Vec3, r: f32) -> Vec<f32> {
        let mut out = Vec::new();
        for t in m.indices.chunks_exact(3) {
            let ps = [t[0], t[1], t[2]].map(|v| m.positions[v as usize]);
            if ps.iter().all(|p| p.distance(c) < r) {
                for i in 0..3 {
                    out.push(ps[i].distance(ps[(i + 1) % 3]));
                }
            }
        }
        out
    }

    #[test]
    fn refines_under_the_brush_and_stays_closed() {
        let m = sphere(8);
        let c = Vec3::new(0.0, 0.0, 1.0);
        let target = 0.04;
        let mut mesh = m;
        for _ in 0..4 {
            match retessellated(&mesh, &[c], 0.35, target) {
                Some((next, _)) => mesh = next,
                None => break,
            }
        }
        assert!(closed_and_outward(&mesh), "a closed, outward surface");
        // Near the centre, edges come near the target; far away the mesh is untouched.
        let near = edge_lengths_near(&mesh, c, 0.2);
        let mean = near.iter().sum::<f32>() / near.len() as f32;
        assert!(mean < target * 1.5 && mean > target * 0.6, "mean {mean}");
        let far_before = edge_lengths_near(&sphere(8), -c, 0.6);
        let far_after = edge_lengths_near(&mesh, -c, 0.6);
        assert_eq!(far_before.len(), far_after.len(), "the far side keeps its triangles");
        // Still on the sphere, roughly (split points follow the curvature).
        assert!(mesh.positions.iter().all(|p| (p.length() - 1.0).abs() < 0.03));
        // Chunks hold their vertices: none over the limit.
        assert!(mesh.chunks.iter().all(|ch| (ch.vert_end - ch.vert_start) as usize <= crate::mesh::CHUNK_TARGET));
    }

    #[test]
    fn coarsens_when_the_target_grows_and_keeps_groups_and_masks() {
        let mut mesh = sphere(24);
        let n = mesh.vertex_count();
        // Two polygroups split at z = 0; mask the -x half.
        let groups: Vec<u16> = mesh.indices.chunks_exact(3).map(|t| (t.iter().map(|&v| mesh.positions[v as usize].z).sum::<f32>() > 0.0) as u16).collect();
        mesh.set_groups(groups);
        mesh.mask = mesh.positions.iter().map(|p| if p.x < -0.2 { 255 } else { 0 }).collect();
        let masked_before: Vec<Vec3> = mesh.positions.iter().zip(&mesh.mask).filter(|(_, m)| **m == 255).map(|(p, _)| *p).collect();
        let c = Vec3::new(0.0, 0.0, 1.0);
        let (next, stats) = retessellated(&mesh, &[c], 0.8, 0.3).expect("coarser");
        assert!(stats.collapses > 0 && next.vertex_count() < n, "{stats:?}");
        assert!(closed_and_outward(&next));
        // Masked vertices are all still there, where they were.
        let masked_after: HashSet<[u32; 3]> = next.positions.iter().zip(&next.mask).filter(|(_, m)| **m == 255).map(|(p, _)| p.to_array().map(f32::to_bits)).collect();
        assert!(masked_before.iter().all(|p| masked_after.contains(&p.to_array().map(f32::to_bits))));
        // The group border stays on z = 0: every triangle's group matches its side.
        for (t, tri) in next.indices.chunks_exact(3).enumerate() {
            let z = tri.iter().map(|&v| next.positions[v as usize].z).sum::<f32>() / 3.0;
            assert_eq!(next.group(t as u32), (z > 0.0) as u16, "triangle {t} at z {z}");
        }
    }

    #[test]
    fn target_follows_draw_size_and_detail() {
        assert!((target_edge(1.0, 50.0) - 0.2).abs() < 1e-6);
        assert!((target_edge(0.5, 75.0) - 0.05).abs() < 1e-6);
        assert!(target_edge(1.0, 0.0) > target_edge(1.0, 100.0));
    }

    #[test]
    #[ignore]
    fn time_a_pass() {
        for seg in [60u32, 200, 400] {
            let mesh = sphere(seg);
            let edge = 2.0 / seg as f32;
            let c = Vec3::new(0.0, 0.0, 1.0);
            let t = std::time::Instant::now();
            let (m, stats) = retessellated(&mesh, &[c], 0.15, edge * 0.6).unwrap();
            let a = t.elapsed().as_secs_f32() * 1000.0;
            let t = std::time::Instant::now();
            let again = retessellated(&m, &[c + Vec3::X * 0.05], 0.15, edge * 0.6);
            eprintln!(
                "{} verts: refine pass {a:.1} ms ({stats:?}), next pass {:.1} ms ({:?})",
                mesh.vertex_count(),
                t.elapsed().as_secs_f32() * 1000.0,
                again.map(|x| x.1)
            );
        }
    }
}
