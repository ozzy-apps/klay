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
//! **Inset**, **Bevel**, **Move**, **Scale**, **Inflate**, **Split**, **Flip**, **Delete**,
//! **Polygroup**, **Crease**, **Mask** and **Bridge**; on edges **Insert** (an edge loop
//! across a ring of quads), **Delete** (dissolve the edge or its loop), **Bevel**, **Spin**,
//! **Crease**, **Collapse**, **Slide**, **Extrude**, **Close Hole**, **Move**, **Mask** and
//! **Bridge**, **Align**, **Insert Point**, **Unweld** and **Transpose**; on points
//! **Move**, **Bevel**, **Crease**, **Delete**, **Slide**, **Stitch** and **Mask**; and on
//! polygons also **Spin Edges**, **Align**, **Insert Point**, **Unweld**, **Equalize** and
//! **Transpose**. Polygon targets are A Single Poly, Polygroup All and Polyloop; edge
//! targets an edge or its loop. The polygon view carries the mask and the creases, so
//! they survive every action.
//!
//! A drag does not rebuild the subtool at every step: an action that only moves points
//! says where they go ([`PolyMesh::moved_points`] and its kin), and one that makes
//! polygons is built once, after which its points move linearly with the drag's amount
//! (every such action is a `lerp` in it), so the app updates positions in place.

use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::mesh::SculptMesh;
use crate::transform::Affine;
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
    /// Mask per vertex (empty: none).
    pub mask: Vec<u8>,
    /// Creased edges, as (low, high) vertex ids.
    pub creases: HashSet<(u32, u32)>,
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
    /// Join this polygon to a second one clicked next by a tube of quads.
    Bridge,
    /// Chamfer the edges round the polygon: a smaller polygon with a rim.
    Bevel,
    /// Drag the polygons in the view plane.
    Move,
    /// Drag to scale the polygons about their middle.
    Scale,
    /// Drag to push the polygons' points along their own normals.
    Inflate,
    /// A point in the middle and one on each edge: a quad per corner.
    Split,
    /// Turn the polygons round (their normals the other way).
    Flip,
    /// Crease every edge of the polygons, or uncrease them all when they all are.
    Crease,
    /// Mask the polygons' points, or unmask them when they all are.
    Mask,
    /// Spin Edges: the polygons' border points move one slot round it.
    Spin,
    /// Flatten the polygons' points onto the clicked polygon's plane.
    Align,
    /// A point where the cursor is, joined to the polygon's corners.
    InsertPoint,
    /// Separate the polygons from their neighbours: the shared points duplicated.
    Unweld,
    /// Make the polygons regular and the same size about their middles.
    Equalize,
    /// Move, scale and turn the polygons with Gizmo 3D's handles.
    Transpose,
}

impl PolyAction {
    pub const ALL: [Self; 20] = [
        Self::QMesh,
        Self::Extrude,
        Self::Inset,
        Self::Bevel,
        Self::Move,
        Self::Scale,
        Self::Inflate,
        Self::Transpose,
        Self::Split,
        Self::Flip,
        Self::Spin,
        Self::Align,
        Self::Equalize,
        Self::InsertPoint,
        Self::Unweld,
        Self::Delete,
        Self::Polygroup,
        Self::Crease,
        Self::Mask,
        Self::Bridge,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::QMesh => "QMesh",
            Self::Extrude => "Extrude",
            Self::Inset => "Inset",
            Self::Bevel => "Bevel",
            Self::Move => "Move",
            Self::Scale => "Scale",
            Self::Inflate => "Inflate",
            Self::Split => "Split",
            Self::Flip => "Flip",
            Self::Delete => "Delete",
            Self::Polygroup => "Polygroup",
            Self::Crease => "Crease",
            Self::Mask => "Mask",
            Self::Bridge => "Bridge",
            Self::Spin => "Spin Edges",
            Self::Align => "Align",
            Self::InsertPoint => "Insert Point",
            Self::Unweld => "Unweld",
            Self::Equalize => "Equalize",
            Self::Transpose => "Transpose",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::QMesh => "drag out or in",
            Self::Extrude => "drag out or in",
            Self::Inset => "drag in",
            Self::Bevel => "drag for the width",
            Self::Move => "drag",
            Self::Scale => "drag right to grow, left to shrink",
            Self::Inflate => "drag up to push out, down to pull in",
            Self::Split => "click",
            Self::Flip => "click",
            Self::Delete => "click",
            Self::Polygroup => "click",
            Self::Crease => "click: crease every edge · click again to uncrease",
            Self::Mask => "click: mask the points · click again to unmask",
            Self::Bridge => "click one, then the other",
            Self::Spin => "click: the points turn one slot round the polygon",
            Self::Align => "click: the points flattened onto its plane",
            Self::InsertPoint => "click where the point goes",
            Self::Unweld => "click: the polygon comes apart from its neighbours",
            Self::Equalize => "click: regular polygons of the same size",
            Self::Transpose => "click to put the gizmo on it, then drag its handles",
        }
    }
    /// Whether the action happens on a click, without a drag.
    pub fn is_click(self) -> bool {
        matches!(
            self,
            Self::Delete | Self::Polygroup | Self::Bridge | Self::Split | Self::Flip | Self::Crease | Self::Mask | Self::Spin | Self::Align | Self::InsertPoint | Self::Unweld | Self::Equalize | Self::Transpose
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdgeAction {
    #[default]
    Insert,
    Delete,
    /// Chamfer the edge loop into a flat strip.
    Bevel,
    /// Turn the edge one step round its two polygons.
    Spin,
    /// Join this open edge to a second one clicked next by a polygon.
    Bridge,
    /// Crease the edge (or its loop), or uncrease when creased.
    Crease,
    /// Merge the edge's two points into one at its middle.
    Collapse,
    /// Drag the edge loop sideways along the edges that cross it.
    Slide,
    /// Drag an open edge out as a new polygon.
    Extrude,
    /// Fill the hole an open edge is on with one polygon.
    CloseHole,
    /// Drag the edge (or its loop) in the view plane.
    Move,
    /// Mask the edge's points (or the loop's), or unmask when masked.
    Mask,
    /// Straighten the loop through the edge (a closed loop is flattened).
    Align,
    /// A point on the edge where the cursor is, splitting it.
    InsertPoint,
    /// Cut the mesh along the edge (or its loop): the points on it duplicated.
    Unweld,
    /// Move, scale and turn the edge (or its loop) with Gizmo 3D's handles.
    Transpose,
}

impl EdgeAction {
    pub const ALL: [Self; 16] = [
        Self::Insert,
        Self::Delete,
        Self::Bevel,
        Self::Slide,
        Self::Move,
        Self::Transpose,
        Self::Extrude,
        Self::Collapse,
        Self::Spin,
        Self::Align,
        Self::InsertPoint,
        Self::Unweld,
        Self::Crease,
        Self::Mask,
        Self::CloseHole,
        Self::Bridge,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Insert => "Insert",
            Self::Delete => "Delete",
            Self::Bevel => "Bevel",
            Self::Spin => "Spin",
            Self::Bridge => "Bridge",
            Self::Crease => "Crease",
            Self::Collapse => "Collapse",
            Self::Slide => "Slide",
            Self::Extrude => "Extrude",
            Self::CloseHole => "Close Hole",
            Self::Move => "Move",
            Self::Mask => "Mask",
            Self::Align => "Align",
            Self::InsertPoint => "Insert Point",
            Self::Unweld => "Unweld",
            Self::Transpose => "Transpose",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::Insert => "click where the loop goes, drag to slide it",
            Self::Delete => "click",
            Self::Bevel => "drag for the width",
            Self::Spin => "click",
            Self::Bridge => "click one open edge, then the other",
            Self::Crease => "click · click again to uncrease",
            Self::Collapse => "click: the two points become one",
            Self::Slide => "drag across the loop",
            Self::Extrude => "drag an open edge out",
            Self::CloseHole => "click an open edge",
            Self::Move => "drag",
            Self::Mask => "click · click again to unmask",
            Self::Align => "click: the loop through the edge straightened",
            Self::InsertPoint => "click where the point goes on the edge",
            Self::Unweld => "click: the mesh cut along the edge",
            Self::Transpose => "click to put the gizmo on it, then drag its handles",
        }
    }
    /// Whether EdgeLoop widens the action from the edge to its loop.
    pub fn takes_loop(self) -> bool {
        matches!(self, Self::Delete | Self::Bevel | Self::Crease | Self::Slide | Self::Move | Self::Mask | Self::Unweld | Self::Transpose)
    }
    /// Whether the action happens on a click, without a drag.
    pub fn is_click(self) -> bool {
        matches!(self, Self::Delete | Self::Spin | Self::Bridge | Self::Crease | Self::Collapse | Self::CloseHole | Self::Mask | Self::Align | Self::InsertPoint | Self::Unweld | Self::Transpose)
    }
}

/// What ZModeler does to a point (ZBrush: the point actions).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointAction {
    #[default]
    Move,
    /// Chamfer the corner off: the polygons round the point cut back, a polygon in the gap.
    Bevel,
    /// Crease every edge at the point, or uncrease them when they all are.
    Crease,
    /// Take the point out: the polygons round it become one.
    Delete,
    /// Drag the point along the edge that runs the way of the drag.
    Slide,
    /// Weld a second point clicked next onto this one.
    Stitch,
    /// Mask the point, or unmask it when masked.
    Mask,
}

impl PointAction {
    pub const ALL: [Self; 7] = [Self::Move, Self::Bevel, Self::Slide, Self::Delete, Self::Stitch, Self::Crease, Self::Mask];
    pub fn name(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Bevel => "Bevel",
            Self::Crease => "Crease",
            Self::Delete => "Delete",
            Self::Slide => "Slide",
            Self::Stitch => "Stitch",
            Self::Mask => "Mask",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::Move => "drag",
            Self::Bevel => "drag for the size",
            Self::Crease => "click · click again to uncrease",
            Self::Delete => "click: the polygons round it become one",
            Self::Slide => "drag along an edge",
            Self::Stitch => "click one point, then the one to weld it to",
            Self::Mask => "click · click again to unmask",
        }
    }
    /// Whether the action happens on a click, without a drag.
    pub fn is_click(self) -> bool {
        matches!(self, Self::Crease | Self::Delete | Self::Stitch | Self::Mask)
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
        // Each triangle's diagonal: the edge the mesh marks, or while it marks none, its
        // longest edge when clearly the longest (as PolyF).
        let diagonal: Vec<Option<(u32, u32)>> = (0..t)
            .map(|i| {
                let v = tri(i);
                if let Some(&q) = mesh.quads.get(i) {
                    let k = q as usize;
                    return (k < 3).then(|| (v[k], v[(k + 1) % 3]));
                }
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
        let mut pm = PolyMesh {
            pos: mesh.positions.clone(),
            colors: mesh.colors.clone(),
            mask: mesh.mask.clone(),
            creases: mesh.crease_edges().into_iter().collect(),
            ..Default::default()
        };
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
        let mut mask = Vec::new();
        for f in &self.faces {
            for &v in f {
                if used[v as usize] == u32::MAX {
                    used[v as usize] = pos.len() as u32;
                    pos.push(self.pos[v as usize]);
                    if !self.colors.is_empty() {
                        colors.push(self.colors[v as usize]);
                    }
                    if !self.mask.is_empty() {
                        mask.push(self.mask[v as usize]);
                    }
                }
            }
        }
        let mut indices = Vec::new();
        let mut groups = Vec::new();
        let mut quads = Vec::new();
        let mut tri_face = Vec::new();
        for (fi, f) in self.faces.iter().enumerate() {
            let v: Vec<u32> = f.iter().map(|&x| used[x as usize]).collect();
            let p = |k: usize| pos[v[k] as usize];
            if v.len() == 4 {
                // The quad along its shorter diagonal; both halves know it (see
                // `SculptMesh::quads`).
                if p(0).distance_squared(p(2)) <= p(1).distance_squared(p(3)) {
                    indices.extend_from_slice(&[v[0], v[1], v[2], v[0], v[2], v[3]]);
                    quads.extend_from_slice(&[2, 0]);
                } else {
                    indices.extend_from_slice(&[v[0], v[1], v[3], v[1], v[2], v[3]]);
                    quads.extend_from_slice(&[1, 2]);
                }
            } else {
                for k in 1..v.len() - 1 {
                    indices.extend_from_slice(&[v[0], v[k], v[k + 1]]);
                    quads.push(crate::mesh::LONE);
                }
            }
            let n = if v.len() == 4 { 2 } else { v.len() - 2 };
            groups.extend(std::iter::repeat_n(self.groups[fi], n));
            tri_face.extend(std::iter::repeat_n(fi as u32, n));
        }
        let (mut mesh, remap, tri_of_old) = SculptMesh::from_raw_tracked(pos, indices, groups, colors, quads);
        if mask.iter().any(|&m| m != 0) {
            let mut m = vec![0u8; mask.len()];
            for (old, &new) in remap.iter().enumerate() {
                m[new as usize] = mask[old];
            }
            mesh.mask = m;
        }
        if !self.creases.is_empty() {
            let mut edges: Vec<(u32, u32)> = self
                .creases
                .iter()
                .filter_map(|&(a, b)| {
                    let (x, y) = (used[a as usize], used[b as usize]);
                    (x != u32::MAX && y != u32::MAX).then(|| key(remap[x as usize], remap[y as usize]))
                })
                .collect();
            edges.sort_unstable();
            edges.dedup();
            let bits = mesh.crease_bits(&edges);
            mesh.set_creases(bits);
        }
        let mut face_of = vec![u32::MAX; mesh.triangle_count()];
        for (old, &new) in tri_of_old.iter().enumerate() {
            if new != u32::MAX {
                face_of[new as usize] = tri_face[old];
            }
        }
        let vmap = used.iter().map(|&u| if u == u32::MAX { u32::MAX } else { remap[u as usize] }).collect();
        (mesh, face_of, vmap)
    }

    /// A new point at `p` that takes vertex `like`'s paint and mask.
    fn push_point(&mut self, p: Vec3, like: u32) -> u32 {
        self.pos.push(p);
        if !self.colors.is_empty() {
            self.colors.push(self.colors[like as usize]);
        }
        if !self.mask.is_empty() {
            self.mask.push(self.mask[like as usize]);
        }
        (self.pos.len() - 1) as u32
    }

    /// The points, paint, mask and creases of this mesh with no polygons yet.
    fn shell(&self) -> PolyMesh {
        PolyMesh { pos: self.pos.clone(), colors: self.colors.clone(), mask: self.mask.clone(), creases: self.creases.clone(), ..Default::default() }
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
            let id = out.push_point(self.pos[v as usize] + n * dist, v);
            copy.insert(v, id);
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
            let inner: Vec<u32> = fv.iter().map(|&v| out.push_point(self.pos[v as usize].lerp(c, t), v)).collect();
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
        let mut out = self.shell();
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
            out.push_point(self.pos[from as usize].lerp(self.pos[to as usize], t), from);
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

    /// The polygons on edge (a, b) and where the edge starts in each.
    fn faces_on(&self, a: u32, b: u32) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (f, fv) in self.faces.iter().enumerate() {
            let n = fv.len();
            for k in 0..n {
                if key(fv[k], fv[(k + 1) % n]) == key(a, b) {
                    out.push((f, k));
                }
            }
        }
        out
    }

    /// The polygons on edge (a, b).
    pub fn faces_on_edge(&self, a: u32, b: u32) -> Vec<u32> {
        self.faces_on(a, b).into_iter().map(|(f, _)| f as u32).collect()
    }

    /// The polygon with exactly these vertices (in any turn), if any.
    pub fn face_with(&self, verts: &[u32]) -> Option<u32> {
        let mut want = verts.to_vec();
        want.sort_unstable();
        self.faces.iter().position(|f| {
            let mut v = f.clone();
            v.sort_unstable();
            v == want
        }).map(|f| f as u32)
    }

    /// Bevel: the edge loop through (a, b) chamfered: a loop is cut on each side of it,
    /// `t` of the way along the edges that leave it, and the loop itself dissolved, so a
    /// flat strip of quads replaces the edge. Superseded by [`chamfer_edges`](Self::chamfer_edges),
    /// which its test compares against.
    #[cfg(test)]
    pub fn bevel(&self, a: u32, b: u32, t: f32) -> PolyMesh {
        let t = t.clamp(0.02, 0.45);
        let lp = self.edge_loop(a, b);
        let mut out = self.clone();
        for (f, k) in self.faces_on(a, b) {
            let fv = &self.faces[f];
            if fv.len() != 4 {
                return self.clone();
            }
            // The edge leaving the loop at the end of (a, b) that is `a` in this polygon.
            let n = fv.len();
            let (x, y) = (fv[k], fv[(k + 1) % n]);
            let (from, to) = if x == a { (x, fv[(k + n - 1) % n]) } else { (y, fv[(k + 2) % n]) };
            let _ = y;
            out = out.insert_loop(from, to, t);
        }
        out.dissolve(&lp)
    }

    /// Spin: the edge (a, b) turned one step round the two polygons on it (`forward`:
    /// with their order; else against it, which is how a mirror image of a forward spin
    /// turns).
    pub fn spin(&self, a: u32, b: u32, forward: bool) -> PolyMesh {
        let on = self.faces_on(a, b);
        if on.len() != 2 {
            return self.clone();
        }
        let ((f, k), (g, _)) = (on[0], on[1]);
        let fv = &self.faces[f];
        let (n, u, v) = (fv.len(), fv[k], fv[(k + 1) % fv.len()]);
        let f_rot: Vec<u32> = (0..n).map(|i| fv[(k + 1 + i) % n]).collect(); // v .. u
        let gv = &self.faces[g];
        let m = gv.len();
        let Some(j) = gv.iter().position(|&x| x == u) else { return self.clone() };
        let g_rot: Vec<u32> = (0..m).map(|i| gv[(j + i) % m]).collect(); // u .. v
        if g_rot.last() != Some(&v) || n < 3 || m < 3 {
            return self.clone();
        }
        // The polygon round both: v, x1 .. u, y1 .. ; the edge v–u (slots 0 and n - 1)
        // moves one slot on at both ends.
        let mut ring = f_rot.clone();
        ring.extend_from_slice(&g_rot[1..m - 1]);
        let len = ring.len();
        let (x1, y1) = if forward { (1usize, n) } else { (len - 1, n - 2) };
        let walk = |from: usize, to: usize| -> Vec<u32> {
            let mut out = vec![ring[from]];
            let mut i = from;
            while i != to {
                i = (i + 1) % len;
                out.push(ring[i]);
            }
            out
        };
        let first = walk(x1, y1);
        let second = walk(y1, x1);
        if first.len() < 3 || second.len() < 3 {
            return self.clone();
        }
        let mut out = self.clone();
        out.faces[f] = first;
        out.faces[g] = second;
        out
    }

    /// Bridge: polygons `f` and `g` (with as many sides) taken out and their borders joined
    /// by a tube of quads in a new polygroup, turned for the shortest joins.
    pub fn bridge(&self, f: u32, g: u32) -> Option<PolyMesh> {
        let (a, b) = (self.faces[f as usize].clone(), self.faces[g as usize].clone());
        let n = a.len();
        if f == g || b.len() != n || a.iter().any(|v| b.contains(v)) {
            return None;
        }
        let cost = |r: usize| -> f32 { (0..n).map(|k| self.pos[a[k] as usize].distance(self.pos[b[(r + n - k) % n] as usize])).sum() };
        let r = (0..n).min_by(|&x, &y| cost(x).total_cmp(&cost(y)))?;
        let mut out = self.delete(&[f, g]);
        let wall = out.next_group();
        for k in 0..n {
            let (b0, b1) = (b[(r + n - k) % n], b[(r + 2 * n - k - 1) % n]);
            out.faces.push(vec![a[k], a[(k + 1) % n], b1, b0]);
            out.groups.push(wall);
        }
        Some(out)
    }

    /// The edges round `faces` with another polygon on the far side: those of the faces
    /// that no other of them shares, and not on an open border.
    pub fn border_edges(&self, faces: &[u32]) -> Vec<(u32, u32)> {
        let set: HashSet<u32> = faces.iter().copied().collect();
        let edges = self.edges();
        let mut out = Vec::new();
        for &f in faces {
            let fv = &self.faces[f as usize];
            let n = fv.len();
            for k in 0..n {
                let (a, b) = (fv[k], fv[(k + 1) % n]);
                let on = &edges[&key(a, b)];
                if on.len() == 2 && !on.iter().any(|&(g, _)| g != f && set.contains(&g)) {
                    out.push((a, b));
                }
            }
        }
        out
    }

    /// Polygons round point `v` in turn, as (polygon, slot of `v`), each reached from the
    /// one before across the edge leaving `v` in it; `None` when the turn does not close
    /// (an open border at `v`).
    fn around(&self, v: u32, edges: &HashMap<(u32, u32), Vec<(u32, usize)>>, start: (u32, usize)) -> Option<Vec<(u32, usize)>> {
        let mut out = vec![start];
        let (mut f, mut k) = start;
        for _ in 0..self.faces.len() {
            let fv = &self.faces[f as usize];
            let q = fv[(k + 1) % fv.len()];
            let g = edges.get(&key(v, q))?.iter().find(|x| x.0 != f)?.0;
            if g == start.0 {
                return Some(out);
            }
            let j = self.faces[g as usize].iter().position(|&x| x == v)?;
            out.push((g, j));
            (f, k) = (g, j);
        }
        None
    }

    /// Bevel: `edges` (those between two polygons) chamfered off, `t` of the way along the
    /// edges that leave them. Each polygon on a chamfered edge pulls its corners there
    /// inward along its other edges (diagonally where both edges at a corner are
    /// chamfered), a quad takes each edge's place, and where chamfers meet round a point a
    /// polygon fills the gap; all new polygons in one new polygroup. A polygon's own
    /// border gives a smaller polygon with a rim (ZBrush polygon Bevel), a loop a flat
    /// strip (edge Bevel), one edge a single chamfer with a triangle at each end.
    pub fn chamfer_edges(&self, edges: &[(u32, u32)], t: f32) -> PolyMesh {
        let t = t.clamp(0.02, 0.45);
        let all = self.edges();
        let sel: HashSet<(u32, u32)> = edges.iter().map(|&(a, b)| key(a, b)).filter(|k| all.get(k).is_some_and(|on| on.len() == 2)).collect();
        if sel.is_empty() {
            return self.clone();
        }
        let mut out = self.shell();
        out.groups = self.groups.clone();
        let add = |out: &mut PolyMesh, v: u32, p: Vec3| -> u32 { out.push_point(p, v) };
        // Each polygon's copy of each of its corners. A corner slid along an edge is shared
        // by the two polygons on that edge, when both slide it; else the other polygon
        // gains it as a point on that edge, below.
        let mut along: HashMap<(u32, u32), u32> = HashMap::default();
        let mut corner: HashMap<(u32, usize), u32> = HashMap::default();
        for (fi, fv) in self.faces.iter().enumerate() {
            let n = fv.len();
            for k in 0..n {
                let (p, v, q) = (fv[(k + n - 1) % n], fv[k], fv[(k + 1) % n]);
                let (sp, sq) = (sel.contains(&key(p, v)), sel.contains(&key(v, q)));
                let pv = self.pos[v as usize];
                let id = match (sp, sq) {
                    (false, false) => v,
                    (true, false) => *along.entry((v, q)).or_insert_with(|| add(&mut out, v, pv.lerp(self.pos[q as usize], t))),
                    (false, true) => *along.entry((v, p)).or_insert_with(|| add(&mut out, v, pv.lerp(self.pos[p as usize], t))),
                    (true, true) => add(&mut out, v, pv + (self.pos[p as usize] - pv) * t + (self.pos[q as usize] - pv) * t),
                };
                corner.insert((fi as u32, k), id);
            }
        }
        for (fi, fv) in self.faces.iter().enumerate() {
            let n = fv.len();
            let mut nf = Vec::with_capacity(n + 2);
            for k in 0..n {
                let (v, q) = (fv[k], fv[(k + 1) % n]);
                let (cv, cq) = (corner[&(fi as u32, k)], corner[&(fi as u32, (k + 1) % n)]);
                nf.push(cv);
                if let Some(&c) = along.get(&(v, q)) {
                    if c != cv {
                        nf.push(c);
                    }
                }
                if let Some(&c) = along.get(&(q, v)) {
                    if c != cq {
                        nf.push(c);
                    }
                }
            }
            out.faces.push(nf);
        }
        let rim = self.next_group();
        // A quad where each edge was, wound against the polygons on it.
        let mut chamfered: Vec<(u32, u32)> = sel.iter().copied().collect();
        chamfered.sort_unstable();
        for e in chamfered {
            let on = &all[&e];
            let ((f1, k1), (f2, k2)) = (on[0], on[1]);
            let (n1, n2) = (self.faces[f1 as usize].len(), self.faces[f2 as usize].len());
            let (x1, y1) = (corner[&(f1, k1)], corner[&(f1, (k1 + 1) % n1)]);
            let (y2, x2) = (corner[&(f2, k2)], corner[&(f2, (k2 + 1) % n2)]);
            out.faces.push(vec![y1, x1, x2, y2]);
            out.groups.push(rim);
        }
        // Round each point of a chamfered edge: its corners in turn, a polygon when three
        // or more differ (a point kept by some polygon counts as one of them).
        let mut points: Vec<u32> = sel.iter().flat_map(|&(a, b)| [a, b]).collect();
        points.sort_unstable();
        points.dedup();
        for v in points {
            let Some(&(f0, _)) = all.iter().find_map(|(k, on)| (k.0 == v || k.1 == v).then_some(on)).and_then(|on| on.first()) else { continue };
            let Some(k0) = self.faces[f0 as usize].iter().position(|&x| x == v) else { continue };
            let Some(turn) = self.around(v, &all, (f0, k0)) else { continue };
            let mut ring: Vec<u32> = turn.iter().map(|c| corner[c]).collect();
            ring.dedup();
            while ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 {
                ring.reverse();
                out.faces.push(ring);
                out.groups.push(rim);
            }
        }
        out
    }

    /// Bevel of a point: the corner at `v` cut off, `t` of the way along every edge from
    /// it; each polygon round it gains a corner, and a polygon fills the gap (unless `v`
    /// is on an open border).
    pub fn chamfer_point(&self, v: u32, t: f32) -> PolyMesh {
        let t = t.clamp(0.02, 0.45);
        let all = self.edges();
        let Some(&(f0, _)) = all.iter().find_map(|(k, on)| (k.0 == v || k.1 == v).then_some(on)).and_then(|on| on.first()) else { return self.clone() };
        let Some(k0) = self.faces[f0 as usize].iter().position(|&x| x == v) else { return self.clone() };
        let turn = self.around(v, &all, (f0, k0));
        let mut out = self.shell();
        out.groups = self.groups.clone();
        let mut cut: HashMap<u32, u32> = HashMap::default();
        let pv = self.pos[v as usize];
        let mut m = |out: &mut PolyMesh, u: u32| -> u32 { *cut.entry(u).or_insert_with(|| out.push_point(pv.lerp(self.pos[u as usize], t), v)) };
        for fv in &self.faces {
            let n = fv.len();
            let mut nf = Vec::with_capacity(n + 1);
            for k in 0..n {
                if fv[k] == v {
                    nf.push(m(&mut out, fv[(k + n - 1) % n]));
                    nf.push(m(&mut out, fv[(k + 1) % n]));
                } else {
                    nf.push(fv[k]);
                }
            }
            out.faces.push(nf);
        }
        if let Some(turn) = turn {
            // The gap, wound like the polygons round it (the turn runs the other way).
            let mut ring: Vec<u32> = turn.iter().map(|&(f, k)| m(&mut out, self.faces[f as usize][(k + 1) % self.faces[f as usize].len()])).collect();
            ring.reverse();
            if ring.len() >= 3 {
                out.faces.push(ring);
                out.groups.push(self.next_group());
            }
        }
        out
    }

    /// Bridge two open edges with a polygon (a quad, or a triangle when they share a
    /// point), wound to fit the polygons they are on; `None` for an edge between two
    /// polygons or the same edge twice.
    pub fn bridge_edges(&self, first: (u32, u32), second: (u32, u32)) -> Option<PolyMesh> {
        if key(first.0, first.1) == key(second.0, second.1) {
            return None;
        }
        let run = |e: (u32, u32)| -> Option<(u32, u32)> {
            let on = self.faces_on(e.0, e.1);
            let [(f, k)] = on[..] else { return None };
            let fv = &self.faces[f];
            Some((fv[k], fv[(k + 1) % fv.len()]))
        };
        let ((x, y), (x2, y2)) = (run(first)?, run(second)?);
        let mut quad = vec![y, x, y2, x2];
        quad.dedup();
        while quad.len() > 1 && quad.first() == quad.last() {
            quad.pop();
        }
        if quad.len() < 3 {
            return None;
        }
        let mut out = self.clone();
        let g = out.next_group();
        out.faces.push(quad);
        out.groups.push(g);
        Some(out)
    }


    /// The points of `faces`, each once, sorted.
    pub fn points_of(&self, faces: &[u32]) -> Vec<u32> {
        let mut out: Vec<u32> = faces.iter().flat_map(|&f| self.faces[f as usize].iter().copied()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The edges of `faces`, each once as (low, high).
    pub fn edges_of(&self, faces: &[u32]) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = faces
            .iter()
            .flat_map(|&f| {
                let fv = &self.faces[f as usize];
                (0..fv.len()).map(move |k| key(fv[k], fv[(k + 1) % fv.len()]))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The points joined to `v` by an edge.
    pub fn neighbours(&self, v: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for fv in &self.faces {
            let n = fv.len();
            if let Some(k) = fv.iter().position(|&x| x == v) {
                out.push(fv[(k + 1) % n]);
                out.push(fv[(k + n - 1) % n]);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    #[cfg(test)]
    /// Scale: `points` moved towards or away from `centre` by `factor`.
    pub fn scaled(&self, points: &[u32], centre: Vec3, factor: f32) -> PolyMesh {
        let mut out = self.clone();
        for &v in points {
            out.pos[v as usize] = centre + (self.pos[v as usize] - centre) * factor;
        }
        out
    }

    /// Inflate: `points` pushed `dist` along their own normals (the mean of the normals of
    /// the polygons round each).
    pub fn inflated(&self, points: &[u32], dist: f32) -> PolyMesh {
        let set: HashSet<u32> = points.iter().copied().collect();
        let mut normal: HashMap<u32, Vec3> = HashMap::default();
        for f in 0..self.faces.len() as u32 {
            let n = self.normal(f);
            for &v in &self.faces[f as usize] {
                if set.contains(&v) {
                    *normal.entry(v).or_insert(Vec3::ZERO) += n;
                }
            }
        }
        let mut out = self.clone();
        for &v in points {
            out.pos[v as usize] += normal.get(&v).copied().unwrap_or(Vec3::ZERO).normalize_or(Vec3::ZERO) * dist;
        }
        out
    }

    /// Flip: `faces` wound the other way, so their normals turn round.
    pub fn flipped(&self, faces: &[u32]) -> PolyMesh {
        let mut out = self.clone();
        for &f in faces {
            out.faces[f as usize].reverse();
        }
        out
    }

    /// Split: each of `faces` gets a point in its middle and one on each of its edges, and
    /// becomes a quad per corner; a polygon beside it gains the point on the shared edge.
    pub fn split(&self, faces: &[u32]) -> PolyMesh {
        let set: HashSet<u32> = faces.iter().copied().collect();
        let mut out = self.shell();
        let mut mid: HashMap<(u32, u32), u32> = HashMap::default();
        let mut mid_of = |out: &mut PolyMesh, a: u32, b: u32| -> u32 {
            *mid.entry(key(a, b)).or_insert_with(|| out.push_point((self.pos[a as usize] + self.pos[b as usize]) * 0.5, a))
        };
        for (f, fv) in self.faces.iter().enumerate() {
            let n = fv.len();
            if set.contains(&(f as u32)) {
                let c = out.push_point(self.centre(f as u32), fv[0]);
                let m: Vec<u32> = (0..n).map(|k| mid_of(&mut out, fv[k], fv[(k + 1) % n])).collect();
                for k in 0..n {
                    out.faces.push(vec![fv[k], m[k], c, m[(k + n - 1) % n]]);
                    out.groups.push(self.groups[f]);
                }
            }
        }
        for (f, fv) in self.faces.iter().enumerate() {
            let n = fv.len();
            if set.contains(&(f as u32)) {
                continue;
            }
            let mut nf = Vec::with_capacity(n * 2);
            for k in 0..n {
                nf.push(fv[k]);
                if let Some(&m) = mid.get(&key(fv[k], fv[(k + 1) % n])) {
                    nf.push(m);
                }
            }
            out.faces.push(nf);
            out.groups.push(self.groups[f]);
        }
        out
    }

    /// Whether every one of `edges` is creased.
    pub fn all_creased(&self, edges: &[(u32, u32)]) -> bool {
        !edges.is_empty() && edges.iter().all(|&(a, b)| self.creases.contains(&key(a, b)))
    }

    /// Crease (`on`) or uncrease `edges`.
    pub fn creased(&self, edges: &[(u32, u32)], on: bool) -> PolyMesh {
        let mut out = self.clone();
        for &(a, b) in edges {
            if on {
                out.creases.insert(key(a, b));
            } else {
                out.creases.remove(&key(a, b));
            }
        }
        out
    }

    /// Whether every one of `points` is fully masked.
    pub fn all_masked(&self, points: &[u32]) -> bool {
        !points.is_empty() && !self.mask.is_empty() && points.iter().all(|&v| self.mask[v as usize] == 255)
    }

    /// Mask (`on`) or unmask `points`.
    pub fn masked(&self, points: &[u32], on: bool) -> PolyMesh {
        let mut out = self.clone();
        if out.mask.is_empty() {
            out.mask = vec![0; out.pos.len()];
        }
        for &v in points {
            out.mask[v as usize] = if on { 255 } else { 0 };
        }
        if out.mask.iter().all(|&m| m == 0) {
            out.mask = Vec::new();
        }
        out
    }

    /// Point `gone` welded onto `keep`, which goes to `at`; a polygon that loses a corner
    /// keeps its order, one left with fewer than three goes. Creases follow.
    pub fn welded(&self, keep: u32, gone: u32, at: Vec3) -> PolyMesh {
        if keep == gone {
            return self.clone();
        }
        let mut out = self.shell();
        out.pos[keep as usize] = at;
        for (f, fv) in self.faces.iter().enumerate() {
            let mut nf: Vec<u32> = fv.iter().map(|&v| if v == gone { keep } else { v }).collect();
            nf.dedup();
            while nf.len() > 1 && nf.first() == nf.last() {
                nf.pop();
            }
            if nf.len() >= 3 {
                out.faces.push(nf);
                out.groups.push(self.groups[f]);
            }
        }
        out.creases = self
            .creases
            .iter()
            .filter_map(|&(a, b)| {
                let (a, b) = (if a == gone { keep } else { a }, if b == gone { keep } else { b });
                (a != b).then(|| key(a, b))
            })
            .collect();
        out
    }

    /// Collapse: the edge's two points become one at its middle.
    pub fn collapse(&self, a: u32, b: u32) -> PolyMesh {
        self.welded(a, b, (self.pos[a as usize] + self.pos[b as usize]) * 0.5)
    }

    /// The rails of the loop edge (a, b): for each of its two points, the point beyond it
    /// on the left of a → b and the one on the right, from the polygons on the edge.
    /// `None` where the edge has no polygon on that side.
    pub fn rails(&self, a: u32, b: u32) -> [(Option<u32>, Option<u32>); 2] {
        let mut out = [(None, None); 2];
        for (f, k) in self.faces_on(a, b) {
            let fv = &self.faces[f];
            let n = fv.len();
            let (prev, next) = (fv[(k + n - 1) % n], fv[(k + 2) % n]);
            if fv[k] == a {
                // a → b runs with this polygon: it lies on the left.
                out[0].0 = Some(prev);
                out[1].0 = Some(next);
            } else {
                out[0].1 = Some(next);
                out[1].1 = Some(prev);
            }
        }
        out
    }

    /// Slide: the points of the loop `edges` moved `t` of the way towards the rail on the
    /// left of each edge (`t` > 0) or the right (`t` < 0).
    pub fn slide_loop(&self, edges: &[(u32, u32)], t: f32) -> PolyMesh {
        let mut to: HashMap<u32, u32> = HashMap::default();
        for &(a, b) in edges {
            let [ra, rb] = self.rails(a, b);
            for (v, (l, r)) in [(a, ra), (b, rb)] {
                if let Some(w) = if t >= 0.0 { l } else { r } {
                    to.entry(v).or_insert(w);
                }
            }
        }
        let t = t.abs().clamp(0.0, 0.95);
        let mut out = self.clone();
        for (v, w) in to {
            out.pos[v as usize] = self.pos[v as usize].lerp(self.pos[w as usize], t);
        }
        out
    }

    /// Extrude: each of `edges` that is open (one polygon) pulled out by `delta` as a new
    /// quad in a new polygroup; edges that share a point share its copy.
    pub fn extrude_edges(&self, edges: &[(u32, u32)], delta: Vec3) -> PolyMesh {
        let mut out = self.clone();
        let g = out.next_group();
        let mut copy: HashMap<u32, u32> = HashMap::default();
        let mut c = |out: &mut PolyMesh, v: u32| -> u32 { *copy.entry(v).or_insert_with(|| out.push_point(self.pos[v as usize] + delta, v)) };
        for &(a, b) in edges {
            let [(f, k)] = self.faces_on(a, b)[..] else { continue };
            let fv = &self.faces[f];
            let (x, y) = (fv[k], fv[(k + 1) % fv.len()]);
            let (cx, cy) = (c(&mut out, x), c(&mut out, y));
            out.faces.push(vec![y, x, cx, cy]);
            out.groups.push(g);
        }
        out
    }

    /// The open edges round the hole that open edge (a, b) is on, in turn (each as the
    /// polygon on it runs it); `None` for an edge between two polygons or a border that
    /// does not close.
    pub fn hole(&self, a: u32, b: u32) -> Option<Vec<(u32, u32)>> {
        let all = self.edges();
        if all.get(&key(a, b))?.len() != 1 {
            return None;
        }
        // Open edges run x → y in their polygon; the next one round the hole leaves y.
        let mut next: HashMap<u32, (u32, u32)> = HashMap::default();
        for (&(p, q), on) in &all {
            if on.len() == 1 {
                let (f, k) = on[0];
                let fv = &self.faces[f as usize];
                let (x, y) = (fv[k], fv[(k + 1) % fv.len()]);
                debug_assert_eq!(key(x, y), (p, q));
                if next.insert(y, (x, y)).is_some() {
                    return None;
                }
            }
        }
        let (f, k) = all[&key(a, b)][0];
        let fv = &self.faces[f as usize];
        let start = (fv[k], fv[(k + 1) % fv.len()]);
        let mut out = vec![start];
        let mut e = start;
        for _ in 0..next.len() {
            e = *next.get(&e.0)?;
            if e == start {
                return Some(out);
            }
            out.push(e);
        }
        None
    }

    /// Close Hole: the hole that open edge (a, b) is on filled with one polygon in a new
    /// polygroup.
    pub fn close_hole(&self, a: u32, b: u32) -> Option<PolyMesh> {
        let ring = self.hole(a, b)?;
        // The ring runs backwards along the open edges, so taking each edge's start
        // winds the polygon against them, like its neighbours.
        let face: Vec<u32> = ring.iter().map(|&(x, _)| x).collect();
        let mut check = face.clone();
        check.sort_unstable();
        check.dedup();
        if check.len() != face.len() || face.len() < 3 {
            return None;
        }
        let mut out = self.clone();
        let g = out.next_group();
        out.faces.push(face);
        out.groups.push(g);
        Some(out)
    }

    /// Delete a point: the polygons round `v` become one (wound like them); on an open
    /// border they are simply taken out. The point's creases go.
    pub fn dissolve_point(&self, v: u32) -> PolyMesh {
        let all = self.edges();
        let around: Vec<u32> = (0..self.faces.len() as u32).filter(|&f| self.faces[f as usize].contains(&v)).collect();
        if around.is_empty() {
            return self.clone();
        }
        let mut out = self.delete(&around);
        out.creases.retain(|&(a, b)| a != v && b != v);
        let f0 = around[0];
        let k0 = self.faces[f0 as usize].iter().position(|&x| x == v).unwrap();
        if let Some(turn) = self.around(v, &all, (f0, k0)) {
            // Each polygon's run from after `v` to before it; the turn goes against the
            // winding, so the runs join in reverse order.
            let mut face: Vec<u32> = Vec::new();
            for &(f, k) in turn.iter().rev() {
                let fv = &self.faces[f as usize];
                let n = fv.len();
                for i in 1..n {
                    let w = fv[(k + i) % n];
                    if face.last() != Some(&w) {
                        face.push(w);
                    }
                }
            }
            while face.len() > 1 && face.first() == face.last() {
                face.pop();
            }
            let mut check = face.clone();
            check.sort_unstable();
            check.dedup();
            if check.len() == face.len() && face.len() >= 3 {
                let g = self.groups[f0 as usize];
                out.faces.push(face);
                out.groups.push(g);
            }
        }
        out
    }

    #[cfg(test)]
    /// Slide a point: `v` moved `t` of the way along its edge to `to`.
    pub fn slid_point(&self, v: u32, to: u32, t: f32) -> PolyMesh {
        let mut out = self.clone();
        out.pos[v as usize] = self.pos[v as usize].lerp(self.pos[to as usize], t.clamp(0.0, 0.95));
        out
    }

    /// This mesh with `points` where they are told to go.
    pub fn with_points(&self, points: &[(u32, Vec3)]) -> PolyMesh {
        let mut out = self.clone();
        for &(v, p) in points {
            out.pos[v as usize] = p;
        }
        out
    }

    /// Move: where `points` go, shifted by `delta`.
    pub fn moved_points(&self, points: &[u32], delta: Vec3) -> Vec<(u32, Vec3)> {
        points.iter().map(|&v| (v, self.pos[v as usize] + delta)).collect()
    }

    /// Scale: where `points` go, moved towards or away from `centre` by `factor`.
    pub fn scaled_points(&self, points: &[u32], centre: Vec3, factor: f32) -> Vec<(u32, Vec3)> {
        points.iter().map(|&v| (v, centre + (self.pos[v as usize] - centre) * factor)).collect()
    }

    #[cfg(test)]
    /// Transpose: where `points` go under the map `m` (a Gizmo 3D drag).
    pub fn mapped_points(&self, points: &[u32], m: &Affine) -> Vec<(u32, Vec3)> {
        points.iter().map(|&v| (v, m.apply(self.pos[v as usize]))).collect()
    }

    /// Where the points of `faces` go under `m`, and under its mirror image in every
    /// other image of a symmetry: a mirrored point `w` (image `k` of a point of the target)
    /// goes to the image of where `m` takes its original.
    pub fn mapped_points_mirrored(&self, points: &[u32], m: &Affine, images: &crate::symmetry::Images, mirror: impl Fn(usize, u32) -> Option<u32>) -> Vec<(u32, Vec3)> {
        let mut done: HashSet<u32> = HashSet::default();
        let mut out = Vec::new();
        for &v in points {
            if done.insert(v) {
                out.push((v, m.apply(self.pos[v as usize])));
            }
        }
        for k in 1..images.len() {
            for &v in points {
                if let Some(w) = mirror(k, v) {
                    if done.insert(w) {
                        out.push((w, images.map(k, m.apply(images.unmap(k, self.pos[w as usize])))));
                    }
                }
            }
        }
        out
    }

    /// Spin Edges: the points round the border of `faces` each take the place of the next
    /// one round it with the polygons' winding (`forward`) or against it (the way a mirror
    /// image of a forward spin turns); points inside the region stay. A single polygon's
    /// corners turn one slot within its neighbours.
    pub fn spun_faces(&self, faces: &[u32], forward: bool) -> PolyMesh {
        let directed: HashSet<(u32, u32)> = faces
            .iter()
            .flat_map(|&f| {
                let fv = &self.faces[f as usize];
                (0..fv.len()).map(move |k| (fv[k], fv[(k + 1) % fv.len()]))
            })
            .collect();
        let mut next: HashMap<u32, u32> = HashMap::default();
        let mut border: Vec<(u32, u32)> = directed.iter().copied().filter(|&(a, b)| !directed.contains(&(b, a))).collect();
        border.sort_unstable();
        for (a, b) in border {
            let (v, w) = if forward { (a, b) } else { (b, a) };
            next.entry(v).or_insert(w);
        }
        let mut out = self.clone();
        for (&v, &w) in &next {
            out.pos[v as usize] = self.pos[w as usize];
        }
        out
    }

    /// Align: the points of `faces` flattened onto the plane through `at` with normal
    /// `n` (the clicked polygon's).
    pub fn aligned_faces(&self, faces: &[u32], at: Vec3, n: Vec3) -> PolyMesh {
        let n = n.normalize_or(Vec3::Z);
        let mut out = self.clone();
        for v in self.points_of(faces) {
            let p = self.pos[v as usize];
            out.pos[v as usize] = p - n * (p - at).dot(n);
        }
        out
    }

    /// The points of a run of `edges` in order along it, and whether it closes.
    fn chain(&self, edges: &[(u32, u32)]) -> (Vec<u32>, bool) {
        let mut link: HashMap<u32, Vec<u32>> = HashMap::default();
        for &(a, b) in edges {
            if a == b {
                continue;
            }
            let l = link.entry(a).or_default();
            if !l.contains(&b) {
                l.push(b);
            }
            let l = link.entry(b).or_default();
            if !l.contains(&a) {
                l.push(a);
            }
        }
        if link.is_empty() {
            return (Vec::new(), false);
        }
        let mut ends: Vec<u32> = link.iter().filter(|(_, l)| l.len() == 1).map(|(&v, _)| v).collect();
        ends.sort_unstable();
        let closed = ends.is_empty();
        let start = ends.first().copied().unwrap_or_else(|| *link.keys().min().unwrap());
        let mut order = vec![start];
        let (mut prev, mut cur) = (start, link[&start][0]);
        while cur != start && order.len() < link.len() {
            order.push(cur);
            let l = &link[&cur];
            let Some(&next) = l.iter().find(|&&w| w != prev) else { break };
            (prev, cur) = (cur, next);
        }
        (order, closed)
    }

    /// Align: the run of `edges` straightened: an open run's points onto the line through
    /// its two ends, a closed loop's onto the plane that fits it best.
    pub fn aligned_edges(&self, edges: &[(u32, u32)]) -> PolyMesh {
        let (order, closed) = self.chain(edges);
        let mut out = self.clone();
        if order.len() < 3 {
            return out;
        }
        if closed {
            let c = order.iter().map(|&v| self.pos[v as usize]).sum::<Vec3>() / order.len() as f32;
            let mut n = Vec3::ZERO;
            for k in 0..order.len() {
                n += self.pos[order[k] as usize].cross(self.pos[order[(k + 1) % order.len()] as usize]);
            }
            let n = n.normalize_or(Vec3::Y);
            for &v in &order {
                let p = self.pos[v as usize];
                out.pos[v as usize] = p - n * (p - c).dot(n);
            }
        } else {
            let (p, q) = (self.pos[order[0] as usize], self.pos[*order.last().unwrap() as usize]);
            let d = (q - p).normalize_or_zero();
            if d == Vec3::ZERO {
                return out;
            }
            for &v in &order {
                out.pos[v as usize] = p + d * (self.pos[v as usize] - p).dot(d);
            }
        }
        out
    }

    /// Insert Point on a polygon: a point at `p` joined to every corner of `f`, which
    /// becomes a fan of triangles (the first keeps its slot, the rest its polygroup).
    pub fn point_in_face(&self, f: u32, p: Vec3) -> PolyMesh {
        let fv = self.faces[f as usize].clone();
        let n = fv.len();
        let mut out = self.clone();
        let m = out.push_point(p, fv[0]);
        let g = self.groups[f as usize];
        out.faces[f as usize] = vec![fv[0], fv[1], m];
        for k in 1..n {
            out.faces.push(vec![fv[k], fv[(k + 1) % n], m]);
            out.groups.push(g);
        }
        out
    }

    /// Insert Point on an edge: a point `t` of the way from `a` to `b`, splitting the edge
    /// in every polygon on it (a quad becomes five-sided); a crease on it covers both halves.
    pub fn point_on_edge(&self, a: u32, b: u32, t: f32) -> PolyMesh {
        let mut out = self.clone();
        let m = out.push_point(self.pos[a as usize].lerp(self.pos[b as usize], t.clamp(0.0, 1.0)), a);
        for (f, k) in self.faces_on(a, b) {
            out.faces[f].insert(k + 1, m);
        }
        if out.creases.remove(&key(a, b)) {
            out.creases.insert(key(a, m));
            out.creases.insert(key(m, b));
        }
        out
    }

    /// Creases follow duplicated points: a creased edge is creased wherever it now runs,
    /// between the originals or their duplicates, and nowhere it no longer does.
    fn follow_creases(&mut self, dup: &HashMap<u32, Vec<u32>>) {
        if self.creases.is_empty() || dup.is_empty() {
            return;
        }
        let have: HashSet<(u32, u32)> = self.edges().into_keys().collect();
        let choices = |v: u32| -> Vec<u32> {
            let mut c = vec![v];
            if let Some(d) = dup.get(&v) {
                c.extend_from_slice(d);
            }
            c
        };
        let mut out: HashSet<(u32, u32)> = HashSet::default();
        for &(a, b) in &self.creases {
            for x in choices(a) {
                for y in choices(b) {
                    if have.contains(&key(x, y)) {
                        out.insert(key(x, y));
                    }
                }
            }
        }
        self.creases = out;
    }

    /// Unweld: `faces` come apart from the polygons round them: every point they share
    /// with another polygon is duplicated for them, so they are a shell of their own in
    /// the same place. Creases follow.
    pub fn unwelded_faces(&self, faces: &[u32]) -> PolyMesh {
        let set: HashSet<u32> = faces.iter().copied().collect();
        let mut outside: HashSet<u32> = HashSet::default();
        for (f, fv) in self.faces.iter().enumerate() {
            if !set.contains(&(f as u32)) {
                outside.extend(fv.iter().copied());
            }
        }
        let mut out = self.clone();
        let mut dup: HashMap<u32, Vec<u32>> = HashMap::default();
        let mut sorted = faces.to_vec();
        sorted.sort_unstable();
        for f in sorted {
            let fv = self.faces[f as usize].clone();
            let nf: Vec<u32> = fv
                .iter()
                .map(|&v| {
                    if !outside.contains(&v) {
                        return v;
                    }
                    if let Some(d) = dup.get(&v) {
                        return d[0];
                    }
                    let m = out.push_point(self.pos[v as usize], v);
                    dup.insert(v, vec![m]);
                    m
                })
                .collect();
            out.faces[f as usize] = nf;
        }
        out.follow_creases(&dup);
        out
    }

    /// Unweld: the mesh cut along the run of `edges` (an edge or a loop). At each point
    /// on the run the polygons on one side of it get a duplicate of the point, so the two
    /// sides come apart; at the run's ends only the polygon on that side of the last edge
    /// does, and a run that reaches an open border opens it. Creases follow.
    pub fn unwelded_edges(&self, edges: &[(u32, u32)]) -> PolyMesh {
        let (order, closed) = self.chain(edges);
        if order.len() < 2 {
            return self.clone();
        }
        let path: HashSet<(u32, u32)> = (0..order.len() - if closed { 0 } else { 1 }).map(|k| key(order[k], order[(k + 1) % order.len()])).collect();
        let all = self.edges();
        // Each point of the run: the polygons on it, with its slot in each.
        let on_run: HashSet<u32> = order.iter().copied().collect();
        let mut at: HashMap<u32, Vec<(u32, usize)>> = HashMap::default();
        for (f, fv) in self.faces.iter().enumerate() {
            for (k, &v) in fv.iter().enumerate() {
                if on_run.contains(&v) {
                    at.entry(v).or_default().push((f as u32, k));
                }
            }
        }
        // The side that comes away: the second polygon on the first edge, then every
        // polygon found on its side round each point along the run.
        let mut side: HashSet<u32> = HashSet::default();
        if let Some(l) = all.get(&key(order[0], order[1])) {
            if let Some(&(g, _)) = l.get(1) {
                side.insert(g);
            } else if let Some(&(g, _)) = l.first() {
                side.insert(g);
            }
        }
        let mut out = self.clone();
        let mut dup: HashMap<u32, Vec<u32>> = HashMap::default();
        for (i, &v) in order.iter().enumerate() {
            let Some(here) = at.get(&v) else { continue };
            let endpoint = !closed && (i == 0 || i == order.len() - 1);
            // The polygons round v in turn, from a border if there is one.
            let open_start = here.iter().copied().find(|&(f, k)| {
                let fv = &self.faces[f as usize];
                let p = fv[(k + fv.len() - 1) % fv.len()];
                all.get(&key(v, p)).is_some_and(|l| l.len() == 1)
            });
            let turn: Vec<(u32, usize)> = match open_start {
                Some(start) => {
                    let mut t = vec![start];
                    let (mut f, mut k) = start;
                    for _ in 0..here.len() {
                        let fv = &self.faces[f as usize];
                        let q = fv[(k + 1) % fv.len()];
                        let Some(g) = all.get(&key(v, q)).and_then(|l| l.iter().find(|x| x.0 != f)).map(|x| x.0) else { break };
                        let Some(j) = self.faces[g as usize].iter().position(|&x| x == v) else { break };
                        t.push((g, j));
                        (f, k) = (g, j);
                    }
                    t
                }
                None => match self.around(v, &all, here[0]) {
                    Some(t) => t,
                    None => continue,
                },
            };
            let crossing = |i: usize| -> bool {
                let (f, k) = turn[i];
                let fv = &self.faces[f as usize];
                path.contains(&key(v, fv[(k + 1) % fv.len()]))
            };
            let n = turn.len();
            let open = open_start.is_some();
            // Groups between crossings of the run; a closed turn starts after one.
            let first = if open { 0 } else { (0..n).find(|&i| crossing((i + n - 1) % n)).unwrap_or(0) };
            let mut groups: Vec<Vec<(u32, usize)>> = vec![Vec::new()];
            for s in 0..n {
                let i = (first + s) % n;
                groups.last_mut().unwrap().push(turn[i]);
                if s + 1 < n && crossing(i) {
                    groups.push(Vec::new());
                }
            }
            if endpoint || groups.len() < 2 {
                // The run ends here: only the side's polygons on the last edge come away.
                let take: Vec<(u32, usize)> = here.iter().copied().filter(|&(f, _)| side.contains(&f)).collect();
                if !take.is_empty() {
                    let m = out.push_point(self.pos[v as usize], v);
                    for (f, k) in take {
                        out.faces[f as usize][k] = m;
                    }
                    dup.entry(v).or_default().push(m);
                }
                continue;
            }
            for g in groups {
                if !g.iter().any(|(f, _)| side.contains(f)) {
                    continue;
                }
                let m = out.push_point(self.pos[v as usize], v);
                for &(f, k) in &g {
                    side.insert(f);
                    out.faces[f as usize][k] = m;
                }
                dup.entry(v).or_default().push(m);
            }
        }
        out.follow_creases(&dup);
        out
    }

    /// Equalize: each of `faces` made regular, with equal edges and angles in its own
    /// plane about its middle, turned to fit its corners best, all of them at the mean of
    /// their areas; a point shared by several goes to the mean of where they put it.
    pub fn equalized(&self, faces: &[u32]) -> PolyMesh {
        use std::f32::consts::TAU;
        let area = |f: u32| -> f32 {
            let fv = &self.faces[f as usize];
            let mut n = Vec3::ZERO;
            for k in 0..fv.len() {
                n += self.pos[fv[k] as usize].cross(self.pos[fv[(k + 1) % fv.len()] as usize]);
            }
            n.length() * 0.5
        };
        if faces.is_empty() {
            return self.clone();
        }
        let mean = faces.iter().map(|&f| area(f)).sum::<f32>() / faces.len() as f32;
        let mut want: HashMap<u32, (Vec3, f32)> = HashMap::default();
        for &f in faces {
            let fv = &self.faces[f as usize];
            let n = fv.len();
            let c = self.centre(f);
            let nrm = self.normal(f);
            let u = nrm.any_orthonormal_vector();
            let w = nrm.cross(u);
            // The turn that fits the corners best: the mean of their angles less the
            // regular polygon's.
            let (mut sx, mut sy) = (0.0f32, 0.0f32);
            for (k, &v) in fv.iter().enumerate() {
                let d = self.pos[v as usize] - c;
                let th = d.dot(w).atan2(d.dot(u)) - TAU * k as f32 / n as f32;
                sx += th.cos();
                sy += th.sin();
            }
            let phi0 = sy.atan2(sx);
            let r = (2.0 * mean / (n as f32 * (TAU / n as f32).sin())).sqrt();
            for (k, &v) in fv.iter().enumerate() {
                let phi = phi0 + TAU * k as f32 / n as f32;
                let p = c + (u * phi.cos() + w * phi.sin()) * r;
                let e = want.entry(v).or_insert((Vec3::ZERO, 0.0));
                e.0 += p;
                e.1 += 1.0;
            }
        }
        let mut out = self.clone();
        for (v, (sum, count)) in want {
            out.pos[v as usize] = sum / count;
        }
        out
    }

    #[cfg(test)]
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
    fn bevel_spin_and_bridge() {
        let pm = cube();
        // A cube edge loop (round the top's rim, one side) bevelled: a strip of quads.
        let top = (0..pm.faces.len() as u32).find(|&f| pm.normal(f).y > 0.9).unwrap();
        let fv = pm.faces[top as usize].clone();
        let rim = (0..4)
            .map(|k| (fv[k], fv[(k + 1) % 4]))
            .find(|&(x, y)| pm.pos[x as usize].y > 0.99 && pm.pos[y as usize].y > 0.99 && (pm.pos[x as usize].x.abs() > 0.99 && pm.pos[y as usize].x.abs() > 0.99 && (pm.pos[x as usize].x - pm.pos[y as usize].x).abs() < 1e-4))
            .unwrap();
        let bev = pm.bevel(rim.0, rim.1, 0.2);
        assert!(closed(&bev), "closed after the bevel");
        assert!(volume(&bev) < volume(&pm) - 0.01, "the edge was cut off: {} vs {}", volume(&bev), volume(&pm));
        // A closed loop (round the cube's middle) bevels into quads only.
        let eq = (0..pm.faces.len())
            .flat_map(|f| {
                let fv = pm.faces[f].clone();
                (0..4).map(move |k| (fv[k], fv[(k + 1) % 4]))
            })
            .find(|&(x, y)| pm.pos[x as usize].y.abs() < 1e-4 && pm.pos[y as usize].y.abs() < 1e-4)
            .unwrap();
        assert_eq!(pm.edge_loop(eq.0, eq.1).len(), 8);
        let band = pm.bevel(eq.0, eq.1, 0.3);
        assert!(closed(&band));
        assert!(band.faces.iter().all(|f| f.len() == 4), "{:?}", band.faces.iter().map(|f| f.len()).collect::<Vec<_>>());
        assert_eq!(band.faces.len(), 32);
        // Spin an edge between two quads: still closed, the same count.
        let (a, b) = (fv[0], fv[1]);
        let sp = pm.spin(a, b, true);
        assert!(closed(&pm.spin(a, b, false)));
        assert!(closed(&sp));
        assert_eq!(sp.faces.len(), pm.faces.len());
        assert_ne!(sp.faces, pm.faces);
        // Bridge two opposite quads of the cube: a tunnel, still closed.
        let bottom = (0..pm.faces.len() as u32).find(|&f| pm.normal(f).y < -0.9 && (pm.centre(f) - pm.centre(top) * Vec3::new(1.0, -1.0, 1.0)).length() < 1e-4).unwrap();
        let tunnel = pm.bridge(top, bottom).unwrap();
        assert!(closed(&tunnel));
        assert_eq!(tunnel.faces.len(), pm.faces.len() - 2 + 4);
        assert!(volume(&tunnel) < volume(&pm), "the tunnel goes through");
    }

    /// Every directed edge once, its reverse at most once (closed or with open borders).
    fn manifold(pm: &PolyMesh) -> bool {
        let mut count: HashMap<(u32, u32), i32> = HashMap::default();
        for f in &pm.faces {
            if f.len() < 3 {
                return false;
            }
            for k in 0..f.len() {
                *count.entry((f[k], f[(k + 1) % f.len()])).or_default() += 1;
            }
        }
        count.iter().all(|(&(a, b), &c)| a != b && c == 1 && count.get(&(b, a)).is_none_or(|&r| r == 1))
    }

    #[test]
    fn chamfers_bevel_polygons_edges_and_points_and_edges_bridge() {
        let pm = cube();
        let top = (0..pm.faces.len() as u32).find(|&f| pm.normal(f).y > 0.9).unwrap();
        // The top polygon bevelled: four rim quads, nothing to fill at a cube's corners.
        let rim = pm.border_edges(&[top]);
        assert_eq!(rim.len(), 4);
        let bev = pm.chamfer_edges(&rim, 0.25);
        assert!(closed(&bev));
        // Four rim quads, and a quad in the gap at each corner with four polygons round it
        // (three of them; the cube's corner has three polygons and needs none).
        assert_eq!(bev.faces.len(), 24 + 4 + 3);
        assert!(bev.faces[24..].iter().all(|f| f.len() == 4), "rim and gap quads");
        // A polygon beside a slid corner gains that point on its edge (the top polygon
        // across from the bevelled one gains two).
        assert_eq!(bev.faces[..24].iter().map(|f| f.len()).max(), Some(6));
        assert!(volume(&bev) < volume(&pm) - 0.01);
        // All four top polygons at once: their outer border only, the inner edges stay.
        let tops: Vec<u32> = (0..pm.faces.len() as u32).filter(|&f| pm.normal(f).y > 0.9).collect();
        assert_eq!(pm.border_edges(&tops).len(), 8);
        let all = pm.chamfer_edges(&pm.border_edges(&tops), 0.25);
        assert!(closed(&all));
        assert_eq!(all.faces.len(), 24 + 8);
        // One edge: a chamfer quad and a triangle at each end.
        let one = pm.chamfer_edges(&rim[..1], 0.2);
        assert!(closed(&one));
        assert_eq!(one.faces.len(), 24 + 1 + 2);
        assert_eq!(one.faces.iter().filter(|f| f.len() == 3).count(), 2);
        // A closed loop: the same flat strip as the loop bevel.
        let eq = (0..pm.faces.len())
            .flat_map(|f| {
                let fv = pm.faces[f].clone();
                (0..4).map(move |k| (fv[k], fv[(k + 1) % 4]))
            })
            .find(|&(x, y)| pm.pos[x as usize].y.abs() < 1e-4 && pm.pos[y as usize].y.abs() < 1e-4)
            .unwrap();
        let band = pm.chamfer_edges(&pm.edge_loop(eq.0, eq.1), 0.3);
        assert!(closed(&band));
        assert_eq!(band.faces.len(), 32);
        assert!(band.faces.iter().all(|f| f.len() == 4));
        // A polygon in a flat grid: the rim and a quad in each corner gap.
        let (p, i) = crate::primitive::Primitive::Plane.build(4, 1.0);
        let plane = PolyMesh::from_mesh(&SculptMesh::from_raw(p, i)).0;
        let mid = (0..plane.faces.len() as u32).find(|&f| plane.centre(f).length() < 0.4).unwrap();
        let flat = plane.chamfer_edges(&plane.border_edges(&[mid]), 0.3);
        assert!(manifold(&flat));
        assert_eq!(flat.faces.len(), 16 + 4 + 4);
        assert!(flat.faces[16..].iter().all(|f| f.len() == 4), "rim and gap quads");
        // A cube corner chamfered: three polygons gain a corner, a triangle fills the gap.
        let corner = (0..pm.pos.len() as u32).find(|&v| (pm.pos[v as usize] - Vec3::ONE).length() < 1e-4).unwrap();
        let cut = pm.chamfer_point(corner, 0.3);
        assert!(closed(&cut));
        assert_eq!(cut.faces.len(), 25);
        assert_eq!(cut.faces.iter().filter(|f| f.len() == 5).count(), 3);
        let reach = cut.faces.iter().flatten().map(|&v| cut.pos[v as usize]).fold(0.0f32, |m, p| m.max(p.x + p.y + p.z));
        assert!((reach - 2.7).abs() < 1e-4, "the corner is cut off: {reach}");
        // A border point of the plane: no gap to fill, the two polygons cut back.
        let edge_point = (0..plane.pos.len() as u32).find(|&v| (plane.pos[v as usize] - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-4).unwrap();
        let notched = plane.chamfer_point(edge_point, 0.3);
        assert!(manifold(&notched));
        assert_eq!(notched.faces.len(), 16);
        // Two opposite holes in the cube bridged edge by edge until it closes again.
        let bottom = (0..pm.faces.len() as u32).find(|&f| pm.normal(f).y < -0.9 && (pm.centre(f) - pm.centre(top) * Vec3::new(1.0, -1.0, 1.0)).length() < 1e-4).unwrap();
        let (tv, bv) = (pm.faces[top as usize].clone(), pm.faces[bottom as usize].clone());
        let mut holed = pm.delete(&[top, bottom]);
        assert!(holed.bridge_edges((tv[0], tv[1]), (tv[0], tv[1])).is_none(), "the same edge twice");
        let inner = (0..holed.faces.len() as u32).find(|&f| holed.normal(f).y > 0.9).unwrap();
        let iv = holed.faces[inner as usize].clone();
        assert!(holed.bridge_edges((iv[0], iv[1]), (tv[0], tv[1])).is_none(), "an edge between two polygons");
        // Each top edge to the bottom edge nearest it.
        for k in 0..4 {
            let (a, b) = (tv[k], tv[(k + 1) % 4]);
            let (c, d) = (0..4).map(|j| (bv[j], bv[(j + 1) % 4])).min_by(|x, y| {
                let near = |e: &(u32, u32)| (holed.pos[e.0 as usize] + holed.pos[e.1 as usize]).distance(holed.pos[a as usize] + holed.pos[b as usize]);
                near(x).total_cmp(&near(y))
            }).unwrap();
            holed = holed.bridge_edges((a, b), (c, d)).expect("open edges bridge");
            assert!(manifold(&holed));
        }
        assert!(closed(&holed), "four bridges close the tunnel");
        assert_eq!(holed.faces.len(), 24 - 2 + 4);
    }


    #[test]
    fn the_other_actions_move_scale_inflate_split_flip_crease_and_mask_polygons() {
        let c = cube();
        let top = (0..24).find(|&f| c.normal(f).y > 0.9).unwrap();
        let pts = c.points_of(&[top]);
        assert_eq!(pts.len(), 4);
        let s = c.scaled(&pts, c.centre(top), 0.5);
        assert!(closed(&s) && s.pos[pts[0] as usize].distance(c.centre(top)) < c.pos[pts[0] as usize].distance(c.centre(top)) * 0.51);
        let i = c.inflated(&pts, 0.1);
        assert!(closed(&i) && (i.centre(top).y - c.centre(top).y - 0.1).abs() < 0.03, "{}", i.centre(top).y - c.centre(top).y);
        let f = c.flipped(&[top]);
        assert!(!closed(&f) && closed(&f.flipped(&[top])));
        let sp = c.split(&[top]);
        assert_eq!(sp.faces.len(), 27);
        assert!(manifold(&sp) && sp.faces.iter().filter(|f| f.len() == 5).count() == 4, "{:?}", sp.faces.iter().map(|f| f.len()).collect::<Vec<_>>());
        assert_eq!(sp.pos.len(), 26 + 5);
        // Crease and mask survive the round trip through triangles, and toggle.
        let edges = c.edges_of(&[top]);
        let cr = c.creased(&edges, true);
        assert!(cr.all_creased(&edges) && !c.all_creased(&edges));
        let (m, _, vmap) = cr.to_mesh();
        assert_eq!(m.crease_edges().len(), 4);
        let back = PolyMesh::from_mesh(&m).0;
        assert_eq!(back.creases.len(), 4);
        assert!(cr.creased(&edges, false).creases.is_empty());
        let mk = c.masked(&pts, true);
        assert!(mk.all_masked(&pts) && !mk.all_masked(&[0, 1, 2, 3, 4, 5, 6, 7, 8]));
        let (m, _, vmap2) = mk.to_mesh();
        assert!(pts.iter().all(|&v| m.mask[vmap2[v as usize] as usize] == 255) && m.mask.iter().filter(|&&x| x == 255).count() == 4);
        assert!(PolyMesh::from_mesh(&m).0.all_masked(&(0..26).filter(|&v| m.mask[vmap2[v as usize] as usize] == 255).collect::<Vec<_>>()));
        assert!(mk.masked(&pts, false).mask.is_empty());
        let _ = vmap;
        // An extruded masked polygon's new points take the mask with them.
        let ex = mk.extrude(&[top], 0.2);
        assert!(ex.mask.len() == ex.pos.len() && ex.all_masked(&ex.points_of(&[top])));
    }

    #[test]
    fn edges_collapse_slide_extrude_and_close_holes_and_points_dissolve_slide_and_stitch() {
        let c = cube();
        let (a, b) = (c.faces[0][0], c.faces[0][1]);
        let col = c.collapse(a, b);
        assert!(manifold(&col));
        let (m, _, _) = col.to_mesh();
        assert_eq!(m.vertex_count(), 25);
        assert_eq!(col.faces.iter().filter(|f| f.len() == 3).count(), 2);
        // Slide the loop through an edge inside a face (its two polygons flat to each
        // other): every point of the loop moves towards its left rail and the mesh stays
        // closed; the other way goes the opposite way.
        let (fa, fb) = c
            .faces
            .iter()
            .flat_map(|fv| (0..4).map(move |k| (fv[k], fv[(k + 1) % 4])))
            .find(|&(x, y)| {
                let on = c.faces_on(x, y);
                on.len() == 2 && c.normal(on[0].0 as u32).dot(c.normal(on[1].0 as u32)) > 0.99
            })
            .unwrap();
        let lp = c.edge_loop(fa, fb);
        assert!(lp.len() >= 4, "{}", lp.len());
        let sl = c.slide_loop(&lp, 0.5);
        let [(left, _), _] = c.rails(fa, fb);
        let left = left.unwrap();
        assert!(closed(&sl) && (sl.pos[fa as usize] - c.pos[fa as usize].lerp(c.pos[left as usize], 0.5)).length() < 1e-6);
        assert!(lp.iter().all(|&(x, y)| sl.pos[x as usize] != c.pos[x as usize] && sl.pos[y as usize] != c.pos[y as usize]));
        let sr = c.slide_loop(&lp, -0.5);
        assert!((sr.pos[fa as usize] - c.pos[fa as usize]).dot(sl.pos[fa as usize] - c.pos[fa as usize]) < 0.0);
        // A hole: its edges run round it, extrude one out, close the rest.
        let open = c.delete(&[0]);
        let e = (c.faces[0][1], c.faces[0][0]);
        let ring = open.hole(e.0, e.1).unwrap();
        assert_eq!(ring.len(), 4);
        assert!(c.hole(e.0, e.1).is_none(), "an edge between polygons is on no hole");
        let ex = open.extrude_edges(&[e], Vec3::Y * 0.3);
        assert_eq!(ex.faces.len(), 24);
        assert!(manifold(&ex) && ex.pos.len() == 28);
        let cl = open.close_hole(e.0, e.1).unwrap();
        assert!(closed(&cl) && cl.faces.len() == 24 && cl.faces.last().unwrap().len() == 4);
        let two = c.delete(&[0, 1]);
        let e2 = two.faces.iter().find_map(|f| (0..f.len()).map(|k| (f[k], f[(k + 1) % f.len()])).find(|&(x, y)| two.faces_on(x, y).len() == 1)).unwrap();
        let cl2 = two.close_hole(e2.0, e2.1).unwrap();
        assert!(closed(&cl2) && cl2.faces.last().unwrap().len() == 6, "{}", cl2.faces.last().unwrap().len());
        // Delete a corner point: its three quads become one six-sided polygon.
        let corner = (0..26u32).find(|&v| c.neighbours(v).len() == 3).unwrap();
        let dp = c.dissolve_point(corner);
        assert!(manifold(&dp) && dp.faces.len() == 22 && dp.faces.last().unwrap().len() == 6, "{}", dp.faces.len());
        assert!(!dp.faces.iter().any(|f| f.contains(&corner)));
        // On a border the polygons simply go.
        let bp = open.dissolve_point(e.0);
        assert!(bp.faces.len() < open.faces.len() && !bp.faces.iter().any(|f| f.contains(&e.0)));
        // Slide a point along an edge; stitch welds two points, keeping the first.
        let nb = c.neighbours(a)[0];
        let sp = c.slid_point(a, nb, 0.5);
        assert!((sp.pos[a as usize] - (c.pos[a as usize] + c.pos[nb as usize]) * 0.5).length() < 1e-6);
        let st = c.welded(a, nb, c.pos[a as usize]);
        assert!(manifold(&st) && st.pos[a as usize] == c.pos[a as usize] && !st.faces.iter().any(|f| f.contains(&nb)));
        // Creases follow a weld and leave with a deleted point.
        let cr = c.creased(&[(a, nb), (nb, c.neighbours(nb)[1])], true);
        let w = cr.welded(a, nb, c.pos[a as usize]);
        assert!(w.creases.len() == 1 && !w.creases.iter().any(|&(x, y)| x == nb || y == nb));
        assert!(cr.dissolve_point(nb).creases.is_empty());
    }

    /// Every drag action that makes polygons moves its points linearly with its amount,
    /// which is what lets a drag show it without rebuilding: two samples predict a third.
    #[test]
    fn drag_actions_are_linear_in_their_amount() {
        let c = cube();
        let top = (0..24).find(|&f| c.normal(f).y > 0.9).unwrap();
        let fv = c.faces[top as usize].clone();
        let (fa, fb) = c
            .faces
            .iter()
            .flat_map(|fv| (0..4).map(move |k| (fv[k], fv[(k + 1) % 4])))
            .find(|&(x, y)| {
                let on = c.faces_on(x, y);
                on.len() == 2 && c.normal(on[0].0 as u32).dot(c.normal(on[1].0 as u32)) > 0.99
            })
            .unwrap();
        let lp = c.edge_loop(fa, fb);
        let corner = (0..26u32).find(|&v| c.neighbours(v).len() == 3).unwrap();
        let actions: Vec<(&str, Box<dyn Fn(f32) -> PolyMesh>, [f32; 3])> = vec![
            ("extrude", Box::new(|t| c.extrude(&[top], t)), [0.1, 0.4, -0.2]),
            ("inset", Box::new(|t| c.inset(&[top], t)), [0.1, 0.5, 0.8]),
            ("bevel", Box::new(|t| c.chamfer_edges(&c.border_edges(&[top]), t)), [0.05, 0.3, 0.4]),
            ("inflate", Box::new(|t| c.inflated(&c.points_of(&[top]), t)), [0.1, 0.3, -0.2]),
            ("insert", Box::new(|t| c.insert_loop(fv[0], fv[1], t)), [0.1, 0.5, 0.9]),
            ("slide", Box::new(|t| c.slide_loop(&lp, t)), [0.1, 0.5, 0.9]),
            ("edge bevel", Box::new(|t| c.chamfer_edges(&lp, t)), [0.05, 0.3, 0.4]),
            ("point bevel", Box::new(|t| c.chamfer_point(corner, t)), [0.05, 0.3, 0.4]),
        ];
        for (name, act, [t1, t2, t3]) in actions {
            let (p1, p2, p3) = (act(t1), act(t2), act(t3));
            assert!(p1.faces == p2.faces && p2.faces == p3.faces, "{name}: one topology");
            let k = (t3 - t1) / (t2 - t1);
            let err = p1.pos.iter().zip(&p2.pos).zip(&p3.pos).map(|((a, b), c)| (*a + (*b - *a) * k).distance(*c)).fold(0.0f32, f32::max);
            assert!(err < 1e-5, "{name}: predicted within {err}");
        }
    }

    /// Connected pieces of polygons (sharing a point).
    fn shells(pm: &PolyMesh) -> usize {
        let mut owner: Vec<u32> = (0..pm.pos.len() as u32).collect();
        fn find(owner: &mut [u32], v: u32) -> u32 {
            let mut r = v;
            while owner[r as usize] != r {
                r = owner[r as usize];
            }
            let mut v = v;
            while owner[v as usize] != r {
                let n = owner[v as usize];
                owner[v as usize] = r;
                v = n;
            }
            r
        }
        for f in &pm.faces {
            for k in 1..f.len() {
                let (a, b) = (find(&mut owner, f[0]), find(&mut owner, f[k]));
                owner[a as usize] = b;
            }
        }
        let used: HashSet<u32> = pm.faces.iter().flatten().copied().collect();
        let roots: HashSet<u32> = used.iter().map(|&v| find(&mut owner, v)).collect();
        roots.len()
    }

    #[test]
    fn spin_align_insert_point_unweld_equalize_and_transpose() {
        let c = cube();
        let top = (0..24).find(|&f| c.normal(f).y > 0.9).unwrap();
        let fv = c.faces[top as usize].clone();
        // Spin Edges: each corner takes the next one's place; against the winding the one
        // before's, which brings them back.
        let sp = c.spun_faces(&[top], true);
        assert!(closed(&sp) && sp.faces == c.faces);
        for k in 0..4 {
            assert_eq!(sp.pos[fv[k] as usize], c.pos[fv[(k + 1) % 4] as usize]);
        }
        assert!(sp.spun_faces(&[top], false).pos == c.pos);
        // Over a ring of quads only its two border loops move, one slot each.
        let ring = c.target(top, PolyTarget::Loop, Some((fv[1], fv[2])));
        assert_eq!(ring.len(), 8);
        let spr = c.spun_faces(&ring, true);
        let moved = (0..26u32).filter(|&v| spr.pos[v as usize] != c.pos[v as usize]).count();
        assert_eq!(moved, 16, "every point on the ring's two borders");
        // Align: a corner lifted off the top comes back into the polygon's plane.
        let mut bent = c.clone();
        bent.pos[fv[0] as usize].y += 0.3;
        let al = bent.aligned_faces(&[top], bent.centre(top), bent.normal(top));
        let p: Vec<Vec3> = fv.iter().map(|&v| al.pos[v as usize]).collect();
        assert!((p[1] - p[0]).cross(p[2] - p[0]).dot(p[3] - p[0]).abs() < 1e-5);
        assert!(al.pos[fv[0] as usize] != bent.pos[fv[0] as usize]);
        // Align a loop: the equator loop with one point pushed out flattens back; an open
        // run of two edges puts its middle point on the line through its ends.
        let (fa, fb) = c
            .faces
            .iter()
            .flat_map(|fv| (0..4).map(move |k| (fv[k], fv[(k + 1) % 4])))
            .find(|&(x, y)| {
                let on = c.faces_on(x, y);
                on.len() == 2 && c.normal(on[0].0 as u32).dot(c.normal(on[1].0 as u32)) > 0.99
            })
            .unwrap();
        let lp = c.edge_loop(fa, fb);
        assert_eq!(lp.len(), 8);
        let mut bl = c.clone();
        let axis = (0..3).find(|&i| lp.iter().all(|&(x, y)| (c.pos[x as usize][i] - c.pos[y as usize][i]).abs() < 1e-6)).unwrap();
        bl.pos[fa as usize][axis] += 0.2;
        let la = bl.aligned_edges(&lp);
        assert!(la.pos[fa as usize].distance(c.pos[fa as usize]) < 0.08, "{}", la.pos[fa as usize].distance(c.pos[fa as usize]));
        assert!(lp.iter().all(|&(x, _)| la.pos[x as usize].distance(c.pos[x as usize]) < 0.08));
        let (b, w) = (lp[1].0, lp[1].1);
        assert_eq!(b, lp[0].1, "the loop runs on from the edge");
        let mut bo = c.clone();
        bo.pos[b as usize][axis] += 0.2;
        let lo = bo.aligned_edges(&lp[..2]);
        let (pa, pb, pw) = (lo.pos[lp[0].0 as usize], lo.pos[b as usize], lo.pos[w as usize]);
        assert!((pb - pa).cross(pw - pa).length() < 1e-5 && pb != bo.pos[b as usize]);
        // Insert Point: in the polygon, four triangles round it; on an edge, two five-sided
        // polygons, and a crease on the edge covers both halves.
        let ip = c.point_in_face(top, c.centre(top));
        assert!(manifold(&ip) && ip.faces.len() == 27 && ip.pos.len() == 27);
        assert!(ip.faces.iter().filter(|f| f.contains(&26)).count() == 4 && ip.faces.iter().filter(|f| f.contains(&26)).all(|f| f.len() == 3));
        assert!((volume(&ip) - volume(&c)).abs() < 1e-5);
        let (a, b2) = (fv[0], fv[1]);
        let ep = c.creased(&[(a, b2)], true).point_on_edge(a, b2, 0.5);
        assert!(manifold(&ep) && ep.faces.len() == 24 && ep.pos.len() == 27 && ep.faces.iter().filter(|f| f.len() == 5).count() == 2);
        assert!(ep.creases.len() == 2 && ep.creases.contains(&key(a, 26)) && ep.creases.contains(&key(26, b2)));
        assert!((ep.pos[26] - (c.pos[a as usize] + c.pos[b2 as usize]) * 0.5).length() < 1e-6);
        // Unweld a polygon: it floats on four new points of its own, the rest open there.
        let uw = c.unwelded_faces(&[top]);
        assert!(manifold(&uw) && !closed(&uw) && uw.pos.len() == 30 && uw.faces[top as usize].iter().all(|&v| v >= 26));
        assert!(uw.faces.iter().enumerate().filter(|(f, _)| *f != top as usize).all(|(_, f)| f.iter().all(|&v| v < 26)));
        assert_eq!(shells(&uw), 2);
        // Unweld the equator loop: the cube comes apart into two halves, creases on both
        // sides of the cut; one edge alone: its two polygons no longer share it.
        let ue = c.creased(&lp, true).unwelded_edges(&lp);
        assert!(manifold(&ue) && ue.pos.len() == 34 && shells(&ue) == 2, "{} points, {} shells", ue.pos.len(), shells(&ue));
        assert_eq!(ue.creases.len(), 16);
        let u1 = c.unwelded_edges(&[(fa, fb)]);
        assert_eq!(u1.pos.len(), 28);
        assert_eq!(u1.faces.iter().filter(|f| f.contains(&26) && f.contains(&27)).count(), 1);
        assert_eq!(u1.faces.iter().filter(|f| f.contains(&fa) && f.contains(&fb)).count(), 1);
        assert!(manifold(&u1));
        // Equalize: a skewed top becomes a square of the same area; over the whole cube
        // (squares already) nothing moves.
        let mut sk = c.clone();
        let (u, v) = ((c.pos[fv[1] as usize] - c.pos[fv[0] as usize]) * 0.3, (c.pos[fv[3] as usize] - c.pos[fv[0] as usize]) * 0.2);
        sk.pos[fv[0] as usize] += u + v;
        let area = |pm: &PolyMesh, f: u32| {
            let fv = &pm.faces[f as usize];
            (0..4).map(|k| pm.pos[fv[k] as usize].cross(pm.pos[fv[(k + 1) % 4] as usize])).sum::<Vec3>().length() * 0.5
        };
        let eq = sk.equalized(&[top]);
        let edge = |k: usize| eq.pos[fv[k] as usize].distance(eq.pos[fv[(k + 1) % 4] as usize]);
        assert!((0..4).all(|k| (edge(k) - edge(0)).abs() < 1e-4), "{:?}", (0..4).map(edge).collect::<Vec<_>>());
        assert!((area(&eq, top) - area(&sk, top)).abs() < 1e-4);
        assert!((eq.centre(top) - sk.centre(top)).length() < 1e-5);
        let all: Vec<u32> = (0..24).collect();
        let same = c.equalized(&all);
        assert!(same.pos.iter().zip(&c.pos).all(|(a, b)| a.distance(*b) < 1e-4));
        // Transpose: the points under a map, a mirror image under its mirror image.
        let m = Affine::translation(Vec3::new(0.0, 0.5, 0.0));
        let pts = c.mapped_points(&c.points_of(&[top]), &m);
        assert!(pts.len() == 4 && pts.iter().all(|&(v, p)| (p - c.pos[v as usize] - Vec3::Y * 0.5).length() < 1e-6));
        let images = crate::symmetry::Symmetry { x: true, ..Default::default() }.images();
        let mirror = |k: usize, v: u32| -> Option<u32> {
            let want = images.map(k, c.pos[v as usize]);
            (0..26u32).find(|&w| c.pos[w as usize].distance(want) < 1e-5)
        };
        let side = (0..24).find(|&f| c.normal(f).x > 0.9).unwrap();
        let mm = c.mapped_points_mirrored(&c.points_of(&[side]), &Affine::translation(Vec3::X * 0.5), &images, mirror);
        assert_eq!(mm.len(), 8);
        assert!(mm.iter().all(|&(v, p)| (p - c.pos[v as usize]).x.abs() > 0.49 && (p - c.pos[v as usize]).x.signum() == c.pos[v as usize].x.signum()));
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

#[cfg(test)]
mod quad_tests {
    use super::*;

    #[test]
    fn zmodeler_keeps_quads_however_long_and_remembers_the_ones_it_makes() {
        // A plane stretched 4:1: each quad's diagonal is barely longer than its long side,
        // so edge lengths alone would not pair the triangles.
        let (p, i) = crate::primitive::Primitive::Plane.build(2, 1.0);
        let p: Vec<Vec3> = p.into_iter().map(|q| Vec3::new(q.x * 4.0, q.y, q.z)).collect();
        let guessed = PolyMesh::from_mesh(&SculptMesh::from_raw(p.clone(), i.clone())).0;
        assert_eq!(guessed.faces.len(), 8, "guessed: triangles");
        let known = SculptMesh::from_raw_paired(p, i, Vec::new());
        let (pm, _) = PolyMesh::from_mesh(&known);
        assert_eq!(pm.faces.len(), 4, "known: quads");
        // An edge loop through them: the new quads are known too, through to_mesh.
        let f = &pm.faces[0];
        let looped = pm.insert_loop(f[0], f[1], 0.5);
        let (mesh, _, _) = looped.to_mesh();
        assert_eq!(crate::mesh::quad_tests::all_paired(&mesh), mesh.triangle_count());
        assert_eq!(PolyMesh::from_mesh(&mesh).0.faces.len(), looped.faces.len());
    }
}

