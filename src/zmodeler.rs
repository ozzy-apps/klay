//! ZModeler (ZBrush): polygon modelling on a subtool, one action at a time on the point,
//! edge or polygon under the cursor.
//!
//! Klay keeps triangles, so ZModeler works on a polygon view of the subtool
//! ([`PolyMesh`]): triangles are paired into quads the way PolyF shows them (two triangles
//! whose shared edge is the clearly longest edge of both), the rest stay triangles. Each
//! action makes a new polygon mesh, which goes back to triangles with the pairing
//! remembered ([`PolyMesh::to_mesh`]), so quads stay quads from one action to the next.
//!
//! Actions (ZBrush names): on polygons **QMesh** and **Extrude** (along the normals),
//! **Inset**, **Delete**, **Polygroup**; on edges **Insert** (an edge loop across a ring of
//! quads) and **Delete** (dissolve the edge or its loop); on points **Move**. Polygon
//! targets are A Single Poly, Polygroup All and Polyloop; edge targets an edge or its loop.

use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::mesh::SculptMesh;
use glam::Vec3;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolyMesh {
    pub pos: Vec<Vec3>,
    /// Polygons, counter-clockwise seen from outside.
    pub faces: Vec<Vec<u32>>,
    /// Polygroup per polygon.
    pub groups: Vec<u16>,
    /// Polypaint per vertex (empty: none).
    pub colors: Vec<[u8; 4]>,
}

/// What a polygon action acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PolyTarget {
    #[default]
    Single,
    Polygroup,
    Loop,
}

impl PolyTarget {
    pub const ALL: [Self; 3] = [Self::Single, Self::Polygroup, Self::Loop];
    pub fn name(self) -> &'static str {
        match self {
            Self::Single => "A Single Poly",
            Self::Polygroup => "Polygroup All",
            Self::Loop => "Polyloop",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PolyAction {
    #[default]
    QMesh,
    Extrude,
    Inset,
    Delete,
    Polygroup,
}

impl PolyAction {
    pub const ALL: [Self; 5] = [Self::QMesh, Self::Extrude, Self::Inset, Self::Delete, Self::Polygroup];
    pub fn name(self) -> &'static str {
        match self {
            Self::QMesh => "QMesh",
            Self::Extrude => "Extrude",
            Self::Inset => "Inset",
            Self::Delete => "Delete",
            Self::Polygroup => "Polygroup",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdgeAction {
    #[default]
    Insert,
    Delete,
}

impl EdgeAction {
    pub const ALL: [Self; 2] = [Self::Insert, Self::Delete];
    pub fn name(self) -> &'static str {
        match self {
            Self::Insert => "Insert",
            Self::Delete => "Delete",
        }
    }
}

/// What is under the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hover {
    Point(u32),
    /// An edge, as it runs in the polygon under the cursor.
    Edge(u32, u32),
    Poly(u32),
}

fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

impl PolyMesh {
    /// The polygon view of `mesh`, and the polygon of each of its triangles.
    pub fn from_mesh(mesh: &SculptMesh) -> (Self, Vec<u32>) {
        let t = mesh.triangle_count();
        let tri = |i: usize| [mesh.indices[3 * i], mesh.indices[3 * i + 1], mesh.indices[3 * i + 2]];
        // Each triangle's diagonal: its longest edge when clearly the longest (as PolyF).
        let diagonal: Vec<Option<(u32, u32)>> = (0..t)
            .map(|i| {
                let v = tri(i);
                let mut e: Vec<(f32, u32, u32)> = (0..3).map(|k| (mesh.positions[v[k] as usize].distance(mesh.positions[v[(k + 1) % 3] as usize]), v[k], v[(k + 1) % 3])).collect();
                e.sort_by(|a, b| a.0.total_cmp(&b.0));
                (e[2].0 > 1.1 * e[1].0).then_some((e[2].1, e[2].2))
            })
            .collect();
        let mut by_edge: HashMap<(u32, u32), usize> = HashMap::default();
        for (i, d) in diagonal.iter().enumerate() {
            if let Some(&(a, b)) = d.as_ref() {
                by_edge.insert((a, b), i);
            }
        }
        let mut face_of = vec![u32::MAX; t];
        let mut pm = PolyMesh { pos: mesh.positions.clone(), colors: mesh.colors.clone(), ..Default::default() };
        for i in 0..t {
            if face_of[i] != u32::MAX {
                continue;
            }
            let f = pm.faces.len() as u32;
            face_of[i] = f;
            let v = tri(i);
            let partner = diagonal[i].and_then(|(a, b)| by_edge.get(&(b, a)).copied()).filter(|&j| face_of[j] == u32::MAX && j != i);
            match (partner, diagonal[i]) {
                (Some(j), Some((x, y))) => {
                    face_of[j] = f;
                    let z = v.into_iter().find(|&c| c != x && c != y).unwrap();
                    let w = tri(j).into_iter().find(|&c| c != x && c != y).unwrap();
                    pm.faces.push(vec![x, w, y, z]);
                }
                _ => pm.faces.push(v.to_vec()),
            }
            pm.groups.push(mesh.group(i as u32));
        }
        (pm, face_of)
    }

    /// Triangles again (quads along their shorter diagonal, other polygons as fans),
    /// unused vertices dropped. Returns the mesh, the polygon of each of its triangles and
    /// the mesh vertex of each of ours (`u32::MAX` for unused ones).
    pub fn to_mesh(&self) -> (SculptMesh, Vec<u32>, Vec<u32>) {
        let mut used = vec![u32::MAX; self.pos.len()];
        let mut pos = Vec::new();
        let mut colors = Vec::new();
        for f in &self.faces {
            for &v in f {
                if used[v as usize] == u32::MAX {
                    used[v as usize] = pos.len() as u32;
                    pos.push(self.pos[v as usize]);
                    if !self.colors.is_empty() {
                        colors.push(self.colors[v as usize]);
                    }
                }
            }
        }
        let mut indices = Vec::new();
        let mut groups = Vec::new();
        let mut tri_face = Vec::new();
        for (fi, f) in self.faces.iter().enumerate() {
            let v: Vec<u32> = f.iter().map(|&x| used[x as usize]).collect();
            let p = |k: usize| pos[v[k] as usize];
            if v.len() == 4 {
                if p(0).distance_squared(p(2)) <= p(1).distance_squared(p(3)) {
                    indices.extend_from_slice(&[v[0], v[1], v[2], v[0], v[2], v[3]]);
                } else {
                    indices.extend_from_slice(&[v[0], v[1], v[3], v[1], v[2], v[3]]);
                }
            } else {
                for k in 1..v.len() - 1 {
                    indices.extend_from_slice(&[v[0], v[k], v[k + 1]]);
                }
            }
            let n = if v.len() == 4 { 2 } else { v.len() - 2 };
            groups.extend(std::iter::repeat_n(self.groups[fi], n));
            tri_face.extend(std::iter::repeat_n(fi as u32, n));
        }
        let (mesh, remap, tri_of_old) = SculptMesh::from_raw_tracked(pos, indices, groups, colors);
        let mut face_of = vec![u32::MAX; mesh.triangle_count()];
        for (old, &new) in tri_of_old.iter().enumerate() {
            if new != u32::MAX {
                face_of[new as usize] = tri_face[old];
            }
        }
        let vmap = used.iter().map(|&u| if u == u32::MAX { u32::MAX } else { remap[u as usize] }).collect();
        (mesh, face_of, vmap)
    }

    /// Each undirected edge and the polygons on it, with where it starts in each.
    fn edges(&self) -> HashMap<(u32, u32), Vec<(u32, usize)>> {
        let mut m: HashMap<(u32, u32), Vec<(u32, usize)>> = HashMap::default();
        for (fi, f) in self.faces.iter().enumerate() {
            for k in 0..f.len() {
                m.entry(key(f[k], f[(k + 1) % f.len()])).or_default().push((fi as u32, k));
            }
        }
        m
    }

    pub fn normal(&self, f: u32) -> Vec3 {
        let f = &self.faces[f as usize];
        let mut n = Vec3::ZERO;
        for k in 0..f.len() {
            let (a, b) = (self.pos[f[k] as usize], self.pos[f[(k + 1) % f.len()] as usize]);
            n += a.cross(b);
        }
        n.normalize_or(Vec3::Z)
    }

    pub fn centre(&self, f: u32) -> Vec3 {
        let f = &self.faces[f as usize];
        f.iter().map(|&v| self.pos[v as usize]).sum::<Vec3>() / f.len() as f32
    }

    fn next_group(&self) -> u16 {
        self.groups.iter().copied().max().unwrap_or(0).saturating_add(1)
    }

    /// The polygons a polygon target covers, from polygon `f` (and, for a loop, the edge
    /// `across` of `f` the loop runs through).
    pub fn target(&self, f: u32, target: PolyTarget, across: Option<(u32, u32)>) -> Vec<u32> {
        match target {
            PolyTarget::Single => vec![f],
            PolyTarget::Polygroup => (0..self.faces.len() as u32).filter(|&g| self.groups[g as usize] == self.groups[f as usize]).collect(),
            PolyTarget::Loop => {
                let face = &self.faces[f as usize];
                let (a, b) = across.unwrap_or((face[0], face[1]));
                let mut out: Vec<u32> = self.ring(a, b).into_iter().map(|(g, _)| g).collect();
                if !out.contains(&f) {
                    out.push(f);
                }
                out.sort_unstable();
                out.dedup();
                out
            }
        }
    }

    /// The ring of quads an edge loop through edge (a, b) would cross: each quad and the
    /// slot of the crossed edge in it, walking both ways from (a, b) to a polygon that is
    /// not a quad, an open border, or round to the start.
    pub fn ring(&self, a: u32, b: u32) -> Vec<(u32, usize)> {
        let edges = self.edges();
        let Some(start) = edges.get(&key(a, b)) else { return Vec::new() };
        let mut out: Vec<(u32, usize)> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::default();
        for &(f0, k0) in start {
            let (mut f, mut k) = (f0, k0);
            loop {
                if !seen.insert(f) || self.faces[f as usize].len() != 4 {
                    break;
                }
                out.push((f, k));
                let face = &self.faces[f as usize];
                let (c, d) = (face[(k + 2) % 4], face[(k + 3) % 4]);
                let Some(next) = edges.get(&key(c, d)).and_then(|l| l.iter().find(|x| x.0 != f).copied()) else { break };
                (f, k) = next;
            }
        }
        out
    }

    /// The edges of the loop through edge (a, b): on through every vertex with four
    /// polygons around it, both ways, to an irregular vertex, a border or round.
    pub fn edge_loop(&self, a: u32, b: u32) -> Vec<(u32, u32)> {
        let edges = self.edges();
        let mut out = vec![(a, b)];
        let mut faces_at: HashMap<u32, Vec<u32>> = HashMap::default();
        for (fi, f) in self.faces.iter().enumerate() {
            for &v in f {
                faces_at.entry(v).or_default().push(fi as u32);
            }
        }
        for (from, to) in [(a, b), (b, a)] {
            let (mut p, mut q) = (from, to);
            for _ in 0..self.pos.len() {
                // At q: four polygons, all quads; the edge leaving q that shares no polygon
                // with (p, q).
                let around = faces_at.get(&q).map_or(&[][..], |v| v.as_slice());
                if around.len() != 4 || around.iter().any(|&f| self.faces[f as usize].len() != 4) {
                    break;
                }
                let on_pq: Vec<u32> = edges.get(&key(p, q)).map_or(Vec::new(), |l| l.iter().map(|x| x.0).collect());
                let mut next = None;
                for &f in around {
                    let face = &self.faces[f as usize];
                    let i = face.iter().position(|&v| v == q).unwrap();
                    for w in [face[(i + 1) % 4], face[(i + 3) % 4]] {
                        if w == p {
                            continue;
                        }
                        let shares = edges.get(&key(q, w)).is_some_and(|l| l.iter().any(|x| on_pq.contains(&x.0)));
                        if !shares {
                            next = Some(w);
                        }
                    }
                }
                let Some(w) = next else { break };
                if out.iter().any(|&(x, y)| key(x, y) == key(q, w)) {
                    break;
                }
                out.push((q, w));
                (p, q) = (q, w);
            }
        }
        out
    }

    /// QMesh / Extrude: `faces` pushed out by `dist` along their normals (each vertex along
    /// the mean of its pushed polygons'), joined to the rest by walls of quads in a new
    /// polygroup.
    pub fn extrude(&self, faces: &[u32], dist: f32) -> PolyMesh {
        let set: HashSet<u32> = faces.iter().copied().collect();
        let mut out = self.clone();
        let mut normal: HashMap<u32, Vec3> = HashMap::default();
        for &f in faces {
            let n = self.normal(f);
            for &v in &self.faces[f as usize] {
                *normal.entry(v).or_insert(Vec3::ZERO) += n;
            }
        }
        let mut copy: HashMap<u32, u32> = HashMap::default();
        let mut verts: Vec<u32> = normal.keys().copied().collect();
        verts.sort_unstable();
        for v in verts {
            let n = normal[&v].normalize_or(Vec3::Z);
            copy.insert(v, out.pos.len() as u32);
            out.pos.push(self.pos[v as usize] + n * dist);
            if !out.colors.is_empty() {
                out.colors.push(self.colors[v as usize]);
            }
        }
        // Walls along the region's border: where a pushed polygon's edge has no pushed
        // polygon on its other side.
        let directed: HashSet<(u32, u32)> = faces
            .iter()
            .flat_map(|&f| {
                let fv = &self.faces[f as usize];
                (0..fv.len()).map(move |k| (fv[k], fv[(k + 1) % fv.len()]))
            })
            .collect();
        let wall = out.next_group();
        for &(a, b) in &directed {
            if !directed.contains(&(b, a)) {
                out.faces.push(vec![a, b, copy[&b], copy[&a]]);
                out.groups.push(wall);
            }
        }
        for f in 0..self.faces.len() {
            if set.contains(&(f as u32)) {
                out.faces[f] = self.faces[f].iter().map(|v| copy[v]).collect();
            }
        }
        out
    }

    /// Inset: each of `faces` gets a smaller copy of itself inside, `t` of the way to its
    /// middle, joined by a ring of quads in a new polygroup.
    pub fn inset(&self, faces: &[u32], t: f32) -> PolyMesh {
        let mut out = self.clone();
        let ring = out.next_group();
        let t = t.clamp(0.0, 0.95);
        for &f in faces {
            let fv = self.faces[f as usize].clone();
            let c = self.centre(f);
            let inner: Vec<u32> = fv
                .iter()
                .map(|&v| {
                    out.pos.push(self.pos[v as usize].lerp(c, t));
                    if !out.colors.is_empty() {
                        out.colors.push(self.colors[v as usize]);
                    }
                    (out.pos.len() - 1) as u32
                })
                .collect();
            let n = fv.len();
            for k in 0..n {
                out.faces.push(vec![fv[k], fv[(k + 1) % n], inner[(k + 1) % n], inner[k]]);
                out.groups.push(ring);
            }
            out.faces[f as usize] = inner;
        }
        out
    }

    /// Delete: `faces` removed, leaving holes.
    pub fn delete(&self, faces: &[u32]) -> PolyMesh {
        let gone: HashSet<u32> = faces.iter().copied().collect();
        let mut out = PolyMesh { pos: self.pos.clone(), colors: self.colors.clone(), ..Default::default() };
        for (f, fv) in self.faces.iter().enumerate() {
            if !gone.contains(&(f as u32)) {
                out.faces.push(fv.clone());
                out.groups.push(self.groups[f]);
            }
        }
        out
    }

    /// Polygroup: `faces` become a new polygroup.
    pub fn regroup(&self, faces: &[u32]) -> PolyMesh {
        let mut out = self.clone();
        let g = self.next_group();
        for &f in faces {
            out.groups[f as usize] = g;
        }
        out
    }

    /// Insert: an edge loop across the ring of quads through edge (a, b), `t` of the way
    /// from `a`'s side. Polygons beyond the ring that share a cut edge take the new vertex
    /// too (a triangle at the end becomes a quad).
    pub fn insert_loop(&self, a: u32, b: u32, t: f32) -> PolyMesh {
        let ring = self.ring(a, b);
        if ring.is_empty() {
            return self.clone();
        }
        let t = t.clamp(0.02, 0.98);
        let mut out = self.clone();
        // The vertex on each cut edge, and which end `t` is measured from.
        let mut cut: HashMap<(u32, u32), u32> = HashMap::default();
        let mut side: HashMap<(u32, u32), u32> = HashMap::default();
        side.insert(key(a, b), a);
        // Walk the ring in order so every quad meets a cut edge whose side is known.
        let mut pending: Vec<(u32, usize)> = ring.clone();
        let mut guard = 0;
        while !pending.is_empty() && guard < ring.len() * 4 + 8 {
            guard += 1;
            let (f, k) = pending.remove(0);
            let fv = &self.faces[f as usize];
            let (p, q, r, s) = (fv[k], fv[(k + 1) % 4], fv[(k + 2) % 4], fv[(k + 3) % 4]);
            let known = side.get(&key(p, q)).copied().or_else(|| side.get(&key(r, s)).map(|&x| if x == s { p } else { q }));
            let Some(sp) = known else {
                pending.push((f, k));
                continue;
            };
            // Opposite edge (r, s): s faces p, r faces q.
            side.insert(key(p, q), sp);
            side.insert(key(r, s), if sp == p { s } else { r });
        }
        let mut point = |out: &mut PolyMesh, u: u32, v: u32| -> u32 {
            let k = key(u, v);
            if let Some(&m) = cut.get(&k) {
                return m;
            }
            let from = side.get(&k).copied().unwrap_or(u);
            let to = if from == u { v } else { u };
            out.pos.push(self.pos[from as usize].lerp(self.pos[to as usize], t));
            if !out.colors.is_empty() {
                out.colors.push(self.colors[from as usize]);
            }
            let m = (out.pos.len() - 1) as u32;
            cut.insert(k, m);
            m
        };
        let in_ring: HashSet<u32> = ring.iter().map(|r| r.0).collect();
        for &(f, k) in &ring {
            let fv = self.faces[f as usize].clone();
            let (p, q, r, s) = (fv[k], fv[(k + 1) % 4], fv[(k + 2) % 4], fv[(k + 3) % 4]);
            let m1 = point(&mut out, p, q);
            let m2 = point(&mut out, r, s);
            out.faces[f as usize] = vec![p, m1, m2, s];
            out.faces.push(vec![m1, q, r, m2]);
            out.groups.push(self.groups[f as usize]);
        }
        // Polygons outside the ring on a cut edge get its vertex.
        for f in 0..self.faces.len() {
            if in_ring.contains(&(f as u32)) {
                continue;
            }
            let fv = &self.faces[f];
            let mut nv = Vec::with_capacity(fv.len() + 2);
            for k in 0..fv.len() {
                let (u, v) = (fv[k], fv[(k + 1) % fv.len()]);
                nv.push(u);
                if let Some(&m) = cut.get(&key(u, v)) {
                    nv.push(m);
                }
            }
            out.faces[f] = nv;
        }
        out
    }

    /// Delete edges: each edge's two polygons become one; vertices left on a straight run
    /// between just two polygons go.
    pub fn dissolve(&self, edges: &[(u32, u32)]) -> PolyMesh {
        let mut out = self.clone();
        let mut alive = vec![true; out.faces.len()];
        let mut touched: HashSet<u32> = HashSet::default();
        for &(a, b) in edges {
            // Find the polygons on (a, b) now.
            let on: Vec<usize> = (0..out.faces.len())
                .filter(|&f| alive[f] && {
                    let fv = &out.faces[f];
                    (0..fv.len()).any(|k| key(fv[k], fv[(k + 1) % fv.len()]) == key(a, b))
                })
                .collect();
            if on.len() != 2 {
                continue;
            }
            let (f, g) = (on[0], on[1]);
            // f runs u → v, g runs v → u.
            let fv = out.faces[f].clone();
            let k = (0..fv.len()).find(|&k| key(fv[k], fv[(k + 1) % fv.len()]) == key(a, b)).unwrap();
            let (u, v) = (fv[k], fv[(k + 1) % fv.len()]);
            let n = fv.len();
            let f_rot: Vec<u32> = (0..n).map(|i| fv[(k + 1 + i) % n]).collect(); // v .. u
            let gv = out.faces[g].clone();
            let m = gv.len();
            let j = gv.iter().position(|&x| x == u).unwrap();
            let g_rot: Vec<u32> = (0..m).map(|i| gv[(j + i) % m]).collect(); // u .. v
            if g_rot.last() != Some(&v) {
                continue;
            }
            let mut merged = f_rot;
            merged.extend_from_slice(&g_rot[1..m - 1]);
            let mut check = merged.clone();
            check.sort_unstable();
            if check.windows(2).any(|w| w[0] == w[1]) {
                continue;
            }
            out.faces[f] = merged;
            alive[g] = false;
            touched.insert(u);
            touched.insert(v);
        }
        let mut faces = Vec::new();
        let mut groups = Vec::new();
        for f in 0..out.faces.len() {
            if alive[f] {
                faces.push(out.faces[f].clone());
                groups.push(out.groups[f]);
            }
        }
        out.faces = faces;
        out.groups = groups;
        // Vertices with only two edges left: drop them from their polygons.
        let mut degree: HashMap<u32, HashSet<u32>> = HashMap::default();
        for fv in &out.faces {
            for k in 0..fv.len() {
                let (x, y) = (fv[k], fv[(k + 1) % fv.len()]);
                degree.entry(x).or_default().insert(y);
                degree.entry(y).or_default().insert(x);
            }
        }
        for v in touched {
            if degree.get(&v).is_some_and(|d| d.len() == 2) {
                for fv in &mut out.faces {
                    if fv.len() > 3 {
                        fv.retain(|&x| x != v);
                    }
                }
            }
        }
        out
    }

    /// Move points by `delta`.
    pub fn moved(&self, points: &[u32], delta: Vec3) -> PolyMesh {
        let mut out = self.clone();
        for &v in points {
            out.pos[v as usize] += delta;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed box of quads (Cube3D with 2 segments: 24 quads).
    fn cube() -> PolyMesh {
        let (p, i) = crate::primitive::Primitive::Cube.build(2, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        PolyMesh::from_mesh(&mesh).0
    }

    fn closed(pm: &PolyMesh) -> bool {
        let mut count: HashMap<(u32, u32), i32> = HashMap::default();
        for f in &pm.faces {
            for k in 0..f.len() {
                *count.entry((f[k], f[(k + 1) % f.len()])).or_default() += 1;
            }
        }
        count.iter().all(|(&(a, b), &c)| c == 1 && count.get(&(b, a)) == Some(&1))
    }

    fn volume(pm: &PolyMesh) -> f32 {
        let (m, _, _) = pm.to_mesh();
        m.indices.chunks_exact(3).map(|t| m.positions[t[0] as usize].dot(m.positions[t[1] as usize].cross(m.positions[t[2] as usize])) / 6.0).sum()
    }

    #[test]
    fn triangles_pair_back_into_the_quads_they_came_from() {
        let pm = cube();
        assert_eq!(pm.faces.len(), 24);
        assert!(pm.faces.iter().all(|f| f.len() == 4));
        assert!(closed(&pm));
        // Through the triangle mesh and back: the same polygons, remembered exactly.
        let (mesh, face_of, vmap) = pm.to_mesh();
        assert_eq!(mesh.triangle_count(), 48);
        let mut per_face = vec![0; pm.faces.len()];
        for &f in &face_of {
            per_face[f as usize] += 1;
        }
        assert!(per_face.iter().all(|&k| k == 2));
        assert!(vmap.iter().all(|&v| v != u32::MAX));
    }

    #[test]
    fn extrude_inset_delete_and_regroup() {
        let pm = cube();
        let v0 = volume(&pm);
        // The top face of quads (+Y) pushed out by 0.5: closed, bigger by 2 x 2 x 0.5.
        let top: Vec<u32> = (0..pm.faces.len() as u32).filter(|&f| pm.normal(f).y > 0.9).collect();
        assert_eq!(top.len(), 4);
        let up = pm.extrude(&top, 0.5);
        assert!(closed(&up));
        assert!((volume(&up) - v0 - 2.0).abs() < 1e-3, "{}", volume(&up));
        assert_eq!(up.faces.len(), 24 + 8, "eight wall quads around the 2 x 2 top");
        // One quad inset: four ring quads and the inner one, still closed.
        let ins = pm.inset(&[top[0]], 0.3);
        assert!(closed(&ins));
        assert_eq!(ins.faces.len(), 24 + 4);
        // Deleted: a hole.
        assert!(!closed(&pm.delete(&[top[0]])));
        // A new polygroup for the top.
        let g = pm.regroup(&top);
        assert!(top.iter().all(|&f| g.groups[f as usize] == g.groups[top[0] as usize]));
        assert_ne!(g.groups[top[0] as usize], g.groups[(0..24u32).find(|f| !top.contains(f)).unwrap() as usize]);
        assert_eq!(pm.target(top[0], PolyTarget::Polygroup, None).len(), 24, "one polygroup on a plain cube");
    }

    #[test]
    fn edge_loops_insert_round_the_cube_and_dissolve_away() {
        let pm = cube();
        // An edge of the top face: its ring runs round the cube (8 quads).
        let f = (0..pm.faces.len() as u32).find(|&f| pm.normal(f).y > 0.9).unwrap();
        let (a, b) = (pm.faces[f as usize][0], pm.faces[f as usize][1]);
        assert_eq!(pm.ring(a, b).len(), 8);
        let cut = pm.insert_loop(a, b, 0.25);
        assert!(closed(&cut));
        assert_eq!(cut.faces.len(), 32);
        assert!(cut.faces.iter().all(|f| f.len() == 4));
        assert!((volume(&cut) - volume(&pm)).abs() < 1e-4, "the shape is unchanged");
        // The loop of edges through a cut edge, dissolved: back to 24 quads.
        let new_v = pm.pos.len() as u32;
        let e = cut.faces.iter().flat_map(|f| (0..4).map(move |k| (f[k], f[(k + 1) % 4]))).find(|&(x, y)| x >= new_v && y >= new_v).unwrap();
        let lp = cut.edge_loop(e.0, e.1);
        assert_eq!(lp.len(), 8, "{lp:?}");
        let back = cut.dissolve(&lp);
        assert!(closed(&back));
        assert_eq!(back.faces.len(), 24);
        assert!(back.faces.iter().all(|f| f.len() == 4));
    }
}
