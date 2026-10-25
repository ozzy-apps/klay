//! Chunked triangle mesh for sculpting.
//!
//! Vertices are stored structure-of-arrays and are physically sorted by chunk, so every
//! chunk owns a contiguous vertex range, and the index buffer is grouped the same way, so
//! every chunk also owns a contiguous triangle range. All heavy operations (selection,
//! position writes, bounds) run in parallel over chunks, and a brush stroke only touches the
//! chunks under it plus their immediate neighbours. Normals are not stored on the CPU: the
//! GPU recomputes them for dirty chunks each frame, and brushes evaluate them on demand.

use glam::Vec3;
use rayon::prelude::*;
#[cfg(test)]
use std::io::Write;

pub use crate::primitive::cube_sphere;

/// Target number of vertices per chunk.
pub const CHUNK_TARGET: usize = 4096;

mod patch;
pub use patch::{NewVertex, Patch, VRef};

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn empty() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    pub fn grow(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn intersects_sphere(&self, c: Vec3, r: f32) -> bool {
        let q = c.clamp(self.min, self.max);
        (q - c).length_squared() <= r * r
    }

    /// Slab test. Returns entry distance along the ray if it hits.
    pub fn ray_hit(&self, o: Vec3, inv_d: Vec3) -> Option<f32> {
        let t0 = (self.min - o) * inv_d;
        let t1 = (self.max - o) * inv_d;
        let tmin = t0.min(t1).max_element().max(0.0);
        let tmax = t0.max(t1).min_element();
        if tmax >= tmin { Some(tmin) } else { None }
    }
}

pub struct Chunk {
    /// Owned vertex range `[vert_start, vert_end)`.
    pub vert_start: u32,
    pub vert_end: u32,
    /// Owned triangle range `[tri_start, tri_end)`: the triangles whose first vertex this
    /// chunk owns. The index buffer is grouped by chunk, so they are contiguous, and within
    /// the range the triangles that touch a vertex of another chunk come first, ending at
    /// `tri_border`.
    pub tri_start: u32,
    pub tri_border: u32,
    pub tri_end: u32,
    /// Bounds of every vertex referenced by the owned triangles.
    pub aabb: Aabb,
    /// Positions changed: re-upload the owned range.
    pub dirty_pos: bool,
    /// Some vertex normal in this chunk may have changed: GPU recompute.
    pub dirty_nrm: bool,
    /// Mask values of the owned range changed: re-upload them.
    pub dirty_mask: bool,
    pub dirty_color: bool,
    /// Chunks that share at least one edge with this one.
    pub neighbors: Vec<u32>,
    /// Vertices referenced by `tris` but owned by another chunk.
    pub external: Vec<u32>,
    stamp: u32,
}

impl Chunk {
    pub(crate) fn new(vert_start: u32, vert_end: u32, tri_start: u32, tri_border: u32, tri_end: u32) -> Self {
        Self {
            vert_start,
            vert_end,
            tri_start,
            tri_border,
            tri_end,
            aabb: Aabb::empty(),
            dirty_pos: true,
            dirty_nrm: true,
            dirty_mask: false,
            dirty_color: false,
            neighbors: Vec::new(),
            external: Vec::new(),
            stamp: 0,
        }
    }

    pub fn range(&self) -> (usize, usize) {
        (self.vert_start as usize, self.vert_end as usize)
    }

    /// Ids of the owned triangles.
    pub fn tris(&self) -> std::ops::Range<u32> {
        self.tri_start..self.tri_end
    }

    /// Ids of the owned triangles that have a vertex in another chunk.
    pub fn border_tris(&self) -> std::ops::Range<u32> {
        self.tri_start..self.tri_border
    }
}

/// Packed unit normal, uploaded as `Snorm8x4`.
pub type PackedNormal = [i8; 4];

pub fn pack_normal(n: Vec3) -> PackedNormal {
    let q = (n * 127.0).round();
    [q.x as i8, q.y as i8, q.z as i8, 0]
}

pub fn unpack_normal(p: PackedNormal) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) * (1.0 / 127.0)
}

/// A contiguous run of a `Selection` that belongs to one chunk.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub chunk: u32,
    pub start: u32,
    pub end: u32,
}

/// Vertices under a brush, grouped by chunk (groups ascend by chunk id).
pub struct Selection {
    pub verts: Vec<u32>,
    pub dist: Vec<f32>,
    pub groups: Vec<Group>,
    /// Which symmetry image of the brush selected each vertex (0: the brush itself; see
    /// [`crate::symmetry::Images`]). Empty without symmetry (every vertex is image 0).
    pub image: Vec<u8>,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.verts.len()
    }

    /// +1 for vertices selected by the brush itself, -1 for those of another image.
    pub fn side(&self, i: usize) -> f32 {
        if self.image(i) == 0 { 1.0 } else { -1.0 }
    }

    /// The symmetry image vertex `i` belongs to (0: the brush itself).
    #[inline]
    pub fn image(&self, i: usize) -> usize {
        if self.image.is_empty() { 0 } else { self.image[i] as usize }
    }

    pub fn is_empty(&self) -> bool {
        self.verts.is_empty()
    }

    pub fn chunks(&self) -> Vec<u32> {
        self.groups.iter().map(|g| g.chunk).collect()
    }

    /// Evaluate `f(index, vertex)` for every selected vertex in parallel.
    pub fn par_map<T: Send, F: Fn(usize, u32) -> T + Sync>(&self, f: F) -> Vec<T> {
        self.verts.par_iter().enumerate().map(|(i, &v)| f(i, v)).collect()
    }

    /// Like `par_map`, but reuses `out` to avoid reallocating per dab.
    pub fn par_map_into<T: Send, F: Fn(usize, u32) -> T + Sync>(&self, out: &mut Vec<T>, f: F) {
        self.verts
            .par_iter()
            .enumerate()
            .map(|(i, &v)| f(i, v))
            .collect_into_vec(out);
    }
}
/// A leading part of a mesh (see [`SculptMesh::extent`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub verts: usize,
    pub tris: usize,
    pub chunks: usize,
}


pub struct SculptMesh {
    pub positions: Vec<Vec3>,
    /// Three indices per triangle.
    pub indices: Vec<u32>,
    pub chunks: Vec<Chunk>,
    /// Owning chunk per vertex.
    pub vert_chunk: Vec<u32>,
    /// CSR adjacency: vertex -> neighbouring vertices.
    pub adj_offsets: Vec<u32>,
    pub adj_verts: Vec<u32>,
    /// CSR adjacency: vertex -> incident triangles.
    pub vt_offsets: Vec<u32>,
    pub vt_tris: Vec<u32>,
    /// Mask per vertex, 0 = free to edit, 255 = fully masked (ZBrush: Ctrl). Empty when
    /// nothing is masked. Only the shown level of a stack carries it (see
    /// [`crate::multires::Multires::set_active`]).
    pub mask: Vec<u8>,
    /// Premultiplied sRGB polypaint; empty while no vertex has been painted.
    pub colors: Vec<[u8; 4]>,
    /// Polygroup per triangle (ZBrush: PolyGroups). Empty while the whole mesh is group 0.
    pub groups: Vec<u16>,
    /// 1 per hidden triangle (ZBrush: visibility). Empty while nothing is hidden.
    pub hidden: Vec<u8>,
    /// 1 per vertex with at least one visible triangle: the vertices brushes may move.
    /// Empty while nothing is hidden.
    pub shown: Vec<u8>,
    /// Bumped whenever `hidden` / `groups` change, so the renderer knows to re-upload.
    pub vis_version: u32,
    pub group_version: u32,
    /// Distinct groups and hidden triangles, kept with the arrays.
    group_count: usize,
    hidden_count: usize,
    stamp_id: u32,
}

pub struct RayHit {
    #[allow(dead_code)]
    pub t: f32,
    pub point: Vec3,
    pub normal: Vec3,
    /// Triangle hit.
    pub tri: u32,
}

impl SculptMesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Area-weighted normal computed from the current positions.
    pub fn normal_at(&self, v: u32) -> Vec3 {
        vertex_normal(&self.positions, &self.indices, &self.vt_offsets, &self.vt_tris, v)
    }

    /// Packed normals for every vertex (used by export; the viewport computes its own).
    pub fn compute_all_normals(&self) -> Vec<PackedNormal> {
        (0..self.positions.len() as u32)
            .into_par_iter()
            .map(|v| pack_normal(self.normal_at(v)))
            .collect()
    }

    pub fn neighbors(&self, v: u32) -> &[u32] {
        let (a, b) = (self.adj_offsets[v as usize], self.adj_offsets[v as usize + 1]);
        &self.adj_verts[a as usize..b as usize]
    }

    /// Triangles incident to `v`.
    pub fn incident(&self, v: u32) -> &[u32] {
        let (a, b) = (self.vt_offsets[v as usize], self.vt_offsets[v as usize + 1]);
        &self.vt_tris[a as usize..b as usize]
    }

    /// Number of edges (each counted once).
    pub fn edge_count(&self) -> usize {
        self.adj_verts.len() / 2
    }

    /// Build a chunked mesh from raw positions and triangle indices.
    pub fn from_raw(positions: Vec<Vec3>, indices: Vec<u32>) -> Self {
        Self::from_raw_with_groups(positions, indices, Vec::new())
    }

    /// [`from_raw`](Self::from_raw) with a polygroup per triangle (or none), kept with its
    /// triangle when they are regrouped by chunk.
    pub fn from_raw_with_groups(positions: Vec<Vec3>, indices: Vec<u32>, tri_groups: Vec<u16>) -> Self {
        Self::from_raw_painted(positions, indices, tri_groups, Vec::new())
    }

    /// Repartition geometry and vertex paint together, preserving their correspondence.
    pub fn from_raw_painted(positions: Vec<Vec3>, indices: Vec<u32>, tri_groups: Vec<u16>, colors: Vec<[u8; 4]>) -> Self {
        // 1. Partition vertices into spatial chunks by recursive median split.
        let groups = partition_vertices(&positions);
        Self::from_partition(groups, positions, indices, tri_groups, colors, Vec::new(), Vec::new())
    }

    /// [`from_raw_painted`](Self::from_raw_painted) with a tag per triangle (which
    /// operand of a boolean it came from), returned in the mesh's triangle order.
    pub fn from_raw_tagged(positions: Vec<Vec3>, indices: Vec<u32>, tri_groups: Vec<u16>, colors: Vec<[u8; 4]>, tags: Vec<u8>) -> (Self, Vec<u8>) {
        let t = indices.len() / 3;
        let groups = partition_vertices(&positions);
        // The tags ride along as the hidden flags, then are taken back out.
        let mut mesh = Self::from_partition(groups, positions, indices, tri_groups, colors, Vec::new(), tags);
        let mut tags = std::mem::take(&mut mesh.hidden);
        mesh.set_hidden(Vec::new());
        if tags.is_empty() {
            tags = vec![0; t];
        }
        (mesh, tags)
    }

    /// Build a mesh whose vertices already have a chunk each (`vert_chunk`, below
    /// `chunk_count`): every chunk keeps its vertices, one grown past [`CHUNK_TARGET`] is
    /// split, empty ones go. Much quicker than [`from_raw`](Self::from_raw) for a mesh that
    /// changed in a few places. Per-vertex `mask` and `colors` and
    /// per-triangle `tri_groups` and `hidden` (each may be empty) are carried along.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn from_assigned(
        positions: Vec<Vec3>,
        indices: Vec<u32>,
        vert_chunk: &[u32],
        chunk_count: usize,
        tri_groups: Vec<u16>,
        colors: Vec<[u8; 4]>,
        mask: Vec<u8>,
        hidden: Vec<u8>,
    ) -> Self {
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); chunk_count];
        for (v, &c) in vert_chunk.iter().enumerate() {
            members[c as usize].push(v as u32);
        }
        let mut groups = Vec::with_capacity(chunk_count + 8);
        for m in members {
            if m.len() > CHUNK_TARGET {
                groups.extend(partition_ids(m, &positions));
            } else if !m.is_empty() {
                groups.push(m);
            }
        }
        Self::from_partition(groups, positions, indices, tri_groups, colors, mask, hidden)
    }

    /// The shared tail of the constructors: `groups` lists every vertex once, chunk by
    /// chunk.
    fn from_partition(
        groups: Vec<Vec<u32>>,
        positions: Vec<Vec3>,
        indices: Vec<u32>,
        mut tri_groups: Vec<u16>,
        colors: Vec<[u8; 4]>,
        mask: Vec<u8>,
        mut hidden: Vec<u8>,
    ) -> Self {
        let n = positions.len();
        assert!(colors.is_empty() || colors.len() == n);
        assert!(mask.is_empty() || mask.len() == n);

        // 2. Reorder vertices so each chunk is contiguous.
        let mut remap = vec![0u32; n];
        let mut new_positions = Vec::with_capacity(n);
        let mut new_colors = Vec::with_capacity(colors.len());
        let mut new_mask = Vec::with_capacity(mask.len());
        let mut vert_chunk = Vec::with_capacity(n);
        let mut chunks = Vec::with_capacity(groups.len());
        for (ci, g) in groups.iter().enumerate() {
            let start = new_positions.len() as u32;
            for &old in g {
                remap[old as usize] = new_positions.len() as u32;
                new_positions.push(positions[old as usize]);
                if !colors.is_empty() { new_colors.push(colors[old as usize]); }
                if !mask.is_empty() { new_mask.push(mask[old as usize]); }
                vert_chunk.push(ci as u32);
            }
            chunks.push(Chunk::new(start, new_positions.len() as u32, 0, 0, 0));
        }
        let positions = new_positions;
        let indices: Vec<u32> = indices.into_par_iter().map(|i| remap[i as usize]).collect();

        // 3. Group the triangles by the chunk owning their first vertex, border ones first.
        let indices = group_triangles(indices, &mut chunks, &mut tri_groups, &mut hidden);

        let (adj_offsets, adj_verts, vt_offsets, vt_tris) = build_csr(n, &indices);
        let mut mesh = Self::assemble(positions, indices, chunks, vert_chunk, adj_offsets, adj_verts, vt_offsets, vt_tris);
        mesh.colors = new_colors;
        mesh.mask = new_mask;
        if !tri_groups.is_empty() {
            mesh.set_groups(tri_groups);
        }
        if !hidden.is_empty() {
            mesh.set_hidden(hidden);
        }
        mesh
    }

    /// Whether some edge is shared by more than two triangles (subdivision needs a
    /// two-manifold mesh, possibly with boundaries).
    pub fn edges_over_two_triangles(&self) -> bool {
        (0..self.vertex_count() as u32).into_par_iter().any(|v| {
            self.neighbors(v).iter().filter(|&&w| w > v).any(|&w| {
                self.incident(v)
                    .iter()
                    .filter(|&&t| self.indices[3 * t as usize..3 * t as usize + 3].contains(&w))
                    .count()
                    > 2
            })
        })
    }

    /// Owned ranges of every chunk: `[vert_start, vert_end, tri_start, tri_border, tri_end]`.
    /// With the positions and the index buffer this is all [`from_chunked`](Self::from_chunked)
    /// needs to rebuild the mesh exactly.
    pub fn chunk_ranges(&self) -> Vec<[u32; 5]> {
        self.chunks
            .iter()
            .map(|c| [c.vert_start, c.vert_end, c.tri_start, c.tri_border, c.tri_end])
            .collect()
    }

    /// Rebuild a mesh from arrays that are already in chunk order (as kept by
    /// [`chunk_ranges`](Self::chunk_ranges)), with the same vertex order, chunks and tables as
    /// the mesh they came from. Unlike [`from_raw`](Self::from_raw) nothing is re-partitioned.
    pub fn from_chunked(positions: Vec<Vec3>, indices: Vec<u32>, ranges: &[[u32; 5]]) -> Self {
        let n = positions.len();
        let mut vert_chunk = vec![0u32; n];
        let chunks: Vec<Chunk> = ranges
            .iter()
            .enumerate()
            .map(|(ci, r)| {
                vert_chunk[r[0] as usize..r[1] as usize].fill(ci as u32);
                Chunk::new(r[0], r[1], r[2], r[3], r[4])
            })
            .collect();
        let (adj_offsets, adj_verts, vt_offsets, vt_tris) = build_csr(n, &indices);
        Self::assemble(positions, indices, chunks, vert_chunk, adj_offsets, adj_verts, vt_offsets, vt_tris)
    }

    /// How much of the mesh comes first: its first `verts` vertices, `tris` triangles and
    /// `chunks` chunks. Parts added with [`appended`](Self::appended) go after it.
    pub fn extent(&self) -> Extent {
        Extent { verts: self.vertex_count(), tris: self.triangle_count(), chunks: self.chunks.len() }
    }

    /// The first `keep` of this mesh followed by `part` (a separate piece of geometry, such
    /// as a CurveTube, with its own polygroups) as chunks of its own; with no part, just the
    /// first `keep`. What is kept is unchanged: same vertex order, chunks, mask, paint,
    /// groups and hidden faces, so the part can later be swapped by cutting back to `keep`.
    #[cfg(test)]
    pub fn appended(&self, keep: Extent, part: Option<&SculptMesh>) -> SculptMesh {
        let ranges = self.chunk_ranges();
        assert!(keep.chunks <= ranges.len());
        let (kv, kt) = (keep.verts as u32, keep.tris as u32);
        assert_eq!(ranges[..keep.chunks].last().map_or(0, |r| r[1]), kv, "keep ends at a chunk");
        assert_eq!(ranges[..keep.chunks].last().map_or(0, |r| r[4]), kt, "keep ends at a chunk");
        let (pv, pt) = part.map_or((0, 0), |p| (p.vertex_count(), p.triangle_count()));
        let mut positions = Vec::with_capacity(keep.verts + pv);
        positions.extend_from_slice(&self.positions[..keep.verts]);
        let mut indices = Vec::with_capacity(3 * (keep.tris + pt));
        indices.extend_from_slice(&self.indices[..3 * keep.tris]);
        let mut all = ranges[..keep.chunks].to_vec();
        if let Some(part) = part {
            positions.extend_from_slice(&part.positions);
            indices.extend(part.indices.iter().map(|&i| i + kv));
            all.extend(part.chunk_ranges().iter().map(|r| [r[0] + kv, r[1] + kv, r[2] + kt, r[3] + kt, r[4] + kt]));
        }
        let mut mesh = Self::from_chunked(positions, indices, &all);
        // Per-vertex and per-triangle data of both, an empty array standing for defaults.
        fn join<T: Copy>(a: &[T], keep: usize, b: Option<&[T]>, len: usize, none: T) -> Vec<T> {
            let b = b.unwrap_or(&[]);
            if a.is_empty() && b.is_empty() {
                return Vec::new();
            }
            let mut out = Vec::with_capacity(keep + len);
            if a.is_empty() { out.resize(keep, none) } else { out.extend_from_slice(&a[..keep]) }
            if b.is_empty() { out.resize(keep + len, none) } else { out.extend_from_slice(b) }
            out
        }
        mesh.mask = join(&self.mask, keep.verts, part.map(|p| &p.mask[..]), pv, 0);
        mesh.colors = join(&self.colors, keep.verts, part.map(|p| &p.colors[..]), pv, [0; 4]);
        mesh.set_groups(join(&self.groups, keep.tris, part.map(|p| &p.groups[..]), pt, 0));
        mesh.set_hidden(join(&self.hidden, keep.tris, part.map(|p| &p.hidden[..]), pt, 0));
        mesh
    }

    /// Everything after `keep` as a mesh of its own (what [`appended`](Self::appended)
    /// added), or `None` when nothing follows it.
    pub fn tail(&self, keep: Extent) -> Option<SculptMesh> {
        if keep.chunks >= self.chunks.len() {
            return None;
        }
        let (kv, kt) = (keep.verts as u32, keep.tris as u32);
        let ranges: Vec<[u32; 5]> = self.chunk_ranges()[keep.chunks..]
            .iter()
            .map(|r| [r[0] - kv, r[1] - kv, r[2] - kt, r[3] - kt, r[4] - kt])
            .collect();
        let positions = self.positions[keep.verts..].to_vec();
        let indices = self.indices[3 * keep.tris..].iter().map(|&i| i - kv).collect();
        let mut mesh = Self::from_chunked(positions, indices, &ranges);
        let tail = |v: &[u8], from: usize| if v.is_empty() { Vec::new() } else { v[from..].to_vec() };
        mesh.mask = tail(&self.mask, keep.verts);
        mesh.colors = if self.colors.is_empty() { Vec::new() } else { self.colors[keep.verts..].to_vec() };
        mesh.set_groups(if self.groups.is_empty() { Vec::new() } else { self.groups[keep.tris..].to_vec() });
        mesh.set_hidden(tail(&self.hidden, keep.tris));
        Some(mesh)
    }

    /// [`appended`](Self::appended) in place: everything after `keep` is replaced by `part`
    /// (or removed), and what was there is returned. The kept part's arrays and tables are
    /// only cut and extended, never rebuilt, so this costs about the size of the parts, not
    /// of the model (a CurveTube is bent this way every frame).
    pub fn swap_tail(&mut self, keep: Extent, part: Option<&SculptMesh>) -> Option<SculptMesh> {
        assert!(keep.chunks <= self.chunks.len());
        let (kv, kt, kc) = (keep.verts, keep.tris, keep.chunks);
        assert_eq!(self.chunks[..kc].last().map_or(0, |c| c.vert_end as usize), kv, "keep ends at a chunk");
        assert_eq!(self.chunks[..kc].last().map_or(0, |c| c.tri_end as usize), kt, "keep ends at a chunk");
        let old = self.tail(keep);
        self.positions.truncate(kv);
        self.indices.truncate(3 * kt);
        self.chunks.truncate(kc);
        self.vert_chunk.truncate(kv);
        self.adj_offsets.truncate(kv + 1);
        self.adj_verts.truncate(self.adj_offsets[kv] as usize);
        self.vt_offsets.truncate(kv + 1);
        self.vt_tris.truncate(self.vt_offsets[kv] as usize);
        self.mask.truncate(kv);
        self.colors.truncate(kv);
        self.shown.truncate(kv);
        let mut groups = std::mem::take(&mut self.groups);
        groups.truncate(kt);
        let mut hidden = std::mem::take(&mut self.hidden);
        hidden.truncate(kt);
        if let Some(p) = part {
            let (v, t, c) = (kv as u32, kt as u32, kc as u32);
            let (pv, pt) = (p.vertex_count(), p.triangle_count());
            self.positions.extend_from_slice(&p.positions);
            self.indices.extend(p.indices.iter().map(|&i| i + v));
            self.chunks.extend(p.chunks.iter().map(|ch| Chunk {
                vert_start: ch.vert_start + v,
                vert_end: ch.vert_end + v,
                tri_start: ch.tri_start + t,
                tri_border: ch.tri_border + t,
                tri_end: ch.tri_end + t,
                aabb: ch.aabb,
                dirty_pos: true,
                dirty_nrm: true,
                dirty_mask: true,
                dirty_color: true,
                neighbors: ch.neighbors.iter().map(|&n| n + c).collect(),
                external: ch.external.iter().map(|&e| e + v).collect(),
                stamp: 0,
            }));
            self.vert_chunk.extend(p.vert_chunk.iter().map(|&ch| ch + c));
            let (adj_end, vt_end) = (self.adj_offsets[kv], self.vt_offsets[kv]);
            self.adj_offsets.extend(p.adj_offsets[1..].iter().map(|&o| o + adj_end));
            self.adj_verts.extend(p.adj_verts.iter().map(|&w| w + v));
            self.vt_offsets.extend(p.vt_offsets[1..].iter().map(|&o| o + vt_end));
            self.vt_tris.extend(p.vt_tris.iter().map(|&x| x + t));
            // Per-vertex and per-triangle data, an empty array standing for defaults.
            fn extend<T: Copy>(a: &mut Vec<T>, keep: usize, b: &[T], len: usize, none: T) {
                if a.is_empty() && b.is_empty() {
                    return;
                }
                a.resize(keep, none);
                if b.is_empty() { a.resize(keep + len, none) } else { a.extend_from_slice(b) }
            }
            extend(&mut self.mask, kv, &p.mask, pv, 0);
            extend(&mut self.colors, kv, &p.colors, pv, [0; 4]);
            extend(&mut groups, kt, &p.groups, pt, 0);
            extend(&mut hidden, kt, &p.hidden, pt, 0);
        }
        self.set_groups(groups);
        self.set_hidden(hidden);
        old
    }

    /// The highest polygroup in use, 0 for none.
    pub fn max_group(&self) -> u16 {
        self.groups.par_iter().copied().max().unwrap_or(0)
    }
}

/// Vertex -> vertex and vertex -> triangle CSR tables of an index buffer:
/// `(adj_offsets, adj_verts, vt_offsets, vt_tris)`. Deterministic, so a mesh rebuilt from
/// the same index buffer gets the same tables.
fn build_csr(n: usize, indices: &[u32]) -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>) {
    {
        // 4. Vertex -> triangle CSR.
        let mut vt_offsets = vec![0u32; n + 1];
        for &i in indices {
            vt_offsets[i as usize + 1] += 1;
        }
        for i in 0..n {
            vt_offsets[i + 1] += vt_offsets[i];
        }
        let mut fill = vt_offsets.clone();
        let mut vt_tris = vec![0u32; indices.len()];
        for (t, tri) in indices.chunks_exact(3).enumerate() {
            for &v in tri {
                let slot = &mut fill[v as usize];
                vt_tris[*slot as usize] = t as u32;
                *slot += 1;
            }
        }

        // 5. Vertex -> vertex CSR, built in parallel blocks then concatenated.
        const BLOCK: usize = 8192;
        let blocks: Vec<Vec<u32>> = (0..n.div_ceil(BLOCK))
            .into_par_iter()
            .map(|b| {
                let mut out = Vec::with_capacity(BLOCK * 7);
                let mut buf: Vec<u32> = Vec::with_capacity(16);
                for v in (b * BLOCK)..((b + 1) * BLOCK).min(n) {
                    buf.clear();
                    let (ta, tb) = (vt_offsets[v] as usize, vt_offsets[v + 1] as usize);
                    for &t in &vt_tris[ta..tb] {
                        for &w in &indices[3 * t as usize..3 * t as usize + 3] {
                            if w != v as u32 {
                                buf.push(w);
                            }
                        }
                    }
                    buf.sort_unstable();
                    buf.dedup();
                    out.push(buf.len() as u32);
                    out.extend_from_slice(&buf);
                }
                out
            })
            .collect();
        let mut adj_offsets = Vec::with_capacity(n + 1);
        let mut adj_verts = Vec::with_capacity(n * 6);
        adj_offsets.push(0u32);
        for block in &blocks {
            let mut i = 0;
            while i < block.len() {
                let cnt = block[i] as usize;
                adj_verts.extend_from_slice(&block[i + 1..i + 1 + cnt]);
                adj_offsets.push(adj_verts.len() as u32);
                i += 1 + cnt;
            }
        }
        drop(blocks);
        (adj_offsets, adj_verts, vt_offsets, vt_tris)
    }
}

impl SculptMesh {

    /// Build a mesh from arrays that are already in chunk order: positions and `vert_chunk`
    /// sorted so every chunk owns a contiguous range, the index buffer grouped so each chunk
    /// owns the contiguous range of triangles whose first vertex it owns (those touching
    /// another chunk first), and both CSR tables complete. Chunk neighbours, borrowed
    /// vertices and bounds are derived here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble(
        positions: Vec<Vec3>,
        indices: Vec<u32>,
        chunks: Vec<Chunk>,
        vert_chunk: Vec<u32>,
        adj_offsets: Vec<u32>,
        adj_verts: Vec<u32>,
        vt_offsets: Vec<u32>,
        vt_tris: Vec<u32>,
    ) -> Self {
        let mut mesh = Self::assemble_parts(positions, indices, chunks, vert_chunk, adj_offsets, adj_verts, vt_offsets, vt_tris);

        // Chunk neighbours and external vertices (referenced by our triangles, owned elsewhere).
        let info: Vec<(Vec<u32>, Vec<u32>)> = (0..mesh.chunks.len()).into_par_iter().map(|ci| mesh.chunk_links(ci))
            .collect();
        for (ch, (external, nbs)) in mesh.chunks.iter_mut().zip(info) {
            ch.external = external;
            ch.neighbors = nbs;
        }

        // Chunk bounds.
        let Self { chunks, positions, .. } = &mut mesh;
        chunks.par_iter_mut().for_each(|ch| ch.aabb = chunk_bounds(ch, positions));
        mesh
    }

    /// [`assemble`](Self::assemble) without working out chunk neighbours, borrowed vertices
    /// and bounds (the caller sets them).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble_parts(
        positions: Vec<Vec3>,
        indices: Vec<u32>,
        chunks: Vec<Chunk>,
        vert_chunk: Vec<u32>,
        adj_offsets: Vec<u32>,
        adj_verts: Vec<u32>,
        vt_offsets: Vec<u32>,
        vt_tris: Vec<u32>,
    ) -> Self {
        Self {
            positions,
            indices,
            chunks,
            vert_chunk,
            adj_offsets,
            adj_verts,
            vt_offsets,
            vt_tris,
            mask: Vec::new(),
            colors: Vec::new(),
            groups: Vec::new(),
            hidden: Vec::new(),
            shown: Vec::new(),
            vis_version: 0,
            group_version: 0,
            group_count: 1,
            hidden_count: 0,
            stamp_id: 0,
        }
    }

    /// Chunk `ci`'s borrowed vertices (referenced by its triangles, owned by other chunks)
    /// and its neighbouring chunks (sharing an edge), both sorted.
    pub(crate) fn chunk_links(&self, ci: usize) -> (Vec<u32>, Vec<u32>) {
        let ch = &self.chunks[ci];
        let mut nbs = Vec::new();
        for v in ch.vert_start..ch.vert_end {
            for &w in self.neighbors(v) {
                let cw = self.vert_chunk[w as usize];
                if cw != ci as u32 {
                    nbs.push(cw);
                }
            }
        }
        nbs.sort_unstable();
        nbs.dedup();
        let mut external = Vec::new();
        for t in ch.border_tris() {
            for &v in &self.indices[3 * t as usize..3 * t as usize + 3] {
                if v < ch.vert_start || v >= ch.vert_end {
                    external.push(v);
                }
            }
        }
        external.sort_unstable();
        external.dedup();
        (external, nbs)
    }

    /// Vertices within `r` of `c`, grouped by chunk. Parallel over chunks.
    pub fn select_sphere(&self, c: Vec3, r: f32) -> Selection {
        let r2 = r * r;
        let shown = &self.shown;
        let per_chunk: Vec<(u32, Vec<(u32, f32)>)> = self
            .chunks
            .par_iter()
            .enumerate()
            .filter(|(_, ch)| ch.aabb.intersects_sphere(c, r))
            .map(|(ci, ch)| {
                let hits: Vec<(u32, f32)> = (ch.vert_start..ch.vert_end)
                    .filter_map(|v| {
                        // Vertices of hidden triangles only are left alone.
                        if !shown.is_empty() && shown[v as usize] == 0 {
                            return None;
                        }
                        let d2 = (self.positions[v as usize] - c).length_squared();
                        if d2 <= r2 { Some((v, d2.sqrt())) } else { None }
                    })
                    .collect();
                (ci as u32, hits)
            })
            .filter(|(_, hits)| !hits.is_empty())
            .collect();
        // Flatten in parallel: each chunk's hits go to a precomputed range.
        let mut groups = Vec::with_capacity(per_chunk.len());
        let mut total = 0u32;
        for (chunk, hits) in &per_chunk {
            groups.push(Group {
                chunk: *chunk,
                start: total,
                end: total + hits.len() as u32,
            });
            total += hits.len() as u32;
        }
        let mut verts = vec![0u32; total as usize];
        let mut dist = vec![0f32; total as usize];
        let ranges = || groups.iter().map(|g| (g.start as usize, g.end as usize));
        let vslices = split_ranges(&mut verts, ranges());
        let dslices = split_ranges(&mut dist, ranges());
        vslices
            .into_par_iter()
            .zip(dslices.into_par_iter())
            .zip(per_chunk.par_iter())
            .for_each(|((vs, ds), (_, hits))| {
                for (i, (v, d)) in hits.iter().enumerate() {
                    vs[i] = *v;
                    ds[i] = *d;
                }
            });
        Selection { verts, dist, groups, image: Vec::new() }
    }

    /// Vertices within `r` of any image of `c` under `images` (symmetry). Each vertex is
    /// listed once, with the distance to the nearest image and which image that is (see
    /// [`Selection::image`]; the brush itself wins a tie), so images never race on a vertex.
    /// Kept separate from [`select_sphere`](Self::select_sphere) so the common path stays
    /// as tight as it is.
    pub fn select_sphere_images(&self, c: Vec3, r: f32, images: &crate::symmetry::Images) -> Selection {
        let centres = images.of(c);
        let r2 = r * r;
        let shown = &self.shown;
        let per_chunk: Vec<(u32, Vec<(u32, f32, u8)>)> = self
            .chunks
            .par_iter()
            .enumerate()
            .filter(|(_, ch)| centres.iter().any(|&m| ch.aabb.intersects_sphere(m, r)))
            .map(|(ci, ch)| {
                let near: Vec<(usize, Vec3)> = centres
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, m)| ch.aabb.intersects_sphere(*m, r))
                    .collect();
                let hits: Vec<(u32, f32, u8)> = (ch.vert_start..ch.vert_end)
                    .filter_map(|v| {
                        if !shown.is_empty() && shown[v as usize] == 0 {
                            return None;
                        }
                        let p = self.positions[v as usize];
                        let (k, d2) = near
                            .iter()
                            .map(|&(k, m)| (k, (p - m).length_squared()))
                            .fold((usize::MAX, f32::INFINITY), |best, x| if x.1 < best.1 { x } else { best });
                        if d2 <= r2 { Some((v, d2.sqrt(), k as u8)) } else { None }
                    })
                    .collect();
                (ci as u32, hits)
            })
            .filter(|(_, hits)| !hits.is_empty())
            .collect();
        let mut groups = Vec::with_capacity(per_chunk.len());
        let mut total = 0u32;
        for (chunk, hits) in &per_chunk {
            groups.push(Group {
                chunk: *chunk,
                start: total,
                end: total + hits.len() as u32,
            });
            total += hits.len() as u32;
        }
        let mut verts = vec![0u32; total as usize];
        let mut dist = vec![0f32; total as usize];
        let mut image = vec![0u8; total as usize];
        let ranges = || groups.iter().map(|g| (g.start as usize, g.end as usize));
        let vslices = split_ranges(&mut verts, ranges());
        let dslices = split_ranges(&mut dist, ranges());
        let islices = split_ranges(&mut image, ranges());
        vslices
            .into_par_iter()
            .zip(dslices.into_par_iter())
            .zip(islices.into_par_iter())
            .zip(per_chunk.par_iter())
            .for_each(|(((vs, ds), is), (_, hits))| {
                for (i, (v, d, k)) in hits.iter().enumerate() {
                    vs[i] = *v;
                    ds[i] = *d;
                    is[i] = *k;
                }
            });
        Selection { verts, dist, groups, image }
    }

    /// Write new positions for a selection, in parallel per chunk.
    pub fn scatter_positions(&mut self, sel: &Selection, new: &[Vec3]) {
        debug_assert_eq!(new.len(), sel.len());
        self.scatter_positions_with(sel, |i, _| new[i]);
    }

    /// Write `f(index, vertex)` as the new position of every selected vertex, in parallel per
    /// chunk. `f` cannot read mesh positions (they are being written); use it when the result
    /// depends only on stroke state, e.g. Grab.
    pub fn scatter_positions_with<F: Fn(usize, u32) -> Vec3 + Sync>(&mut self, sel: &Selection, f: F) {
        let Self { positions, chunks, .. } = self;
        let slices = split_ranges(positions, sel.groups.iter().map(|g| chunks[g.chunk as usize].range()));
        slices
            .into_par_iter()
            .zip(sel.groups.par_iter())
            .for_each(|(slice, g)| {
                let base = chunks[g.chunk as usize].vert_start;
                for i in g.start as usize..g.end as usize {
                    let v = sel.verts[i];
                    slice[(v - base) as usize] = f(i, v);
                }
            });
    }

    /// Copy the owned positions of each chunk, in parallel.
    pub fn snapshot_chunks(&self, chunks: &[u32]) -> Vec<(u32, Vec<Vec3>)> {
        chunks
            .par_iter()
            .map(|&c| {
                let (s, e) = self.chunks[c as usize].range();
                (c, self.positions[s..e].to_vec())
            })
            .collect()
    }

    /// Restore owned positions for whole chunks (sorted by chunk id), in parallel.
    pub fn restore_chunks(&mut self, saved: &[(u32, Vec<Vec3>)]) {
        let Self { positions, chunks, .. } = self;
        let slices = split_ranges(positions, saved.iter().map(|(c, _)| chunks[*c as usize].range()));
        slices
            .into_par_iter()
            .zip(saved.par_iter())
            .for_each(|(slice, (_, data))| slice.copy_from_slice(data));
    }

    /// After positions in `changed` chunks were modified: refresh bounds of those chunks and
    /// their neighbours, and flag them for upload / GPU normal recompute.
    pub fn finish_edit(&mut self, changed: &[u32]) {
        self.stamp_id = self.stamp_id.wrapping_add(1);
        if self.stamp_id == 0 {
            for ch in &mut self.chunks {
                ch.stamp = 0;
            }
            self.stamp_id = 1;
        }
        let id = self.stamp_id;
        // Stamp changed chunks and their neighbours (a neighbour's border normals and bounds
        // depend on our vertices).
        for &c in changed {
            self.chunks[c as usize].stamp = id;
        }
        let nbs: Vec<u32> = changed
            .iter()
            .flat_map(|&c| self.chunks[c as usize].neighbors.iter().copied())
            .collect();
        for c in nbs {
            self.chunks[c as usize].stamp = id;
        }
        let Self { positions, chunks, .. } = self;
        chunks
            .par_iter_mut()
            .filter(|ch| ch.stamp == id)
            .for_each(|ch| {
                ch.aabb = chunk_bounds(ch, positions);
                ch.dirty_nrm = true;
            });
        for &c in changed {
            self.chunks[c as usize].dirty_pos = true;
        }
    }

    /// Every vertex was moved by `f`, a map that keeps the order along each axis (a
    /// translation and a positive scale): each chunk's box maps exactly through `f`, so it
    /// needs no refit over the vertices. Marks every chunk for upload and normals.
    pub fn finish_monotone_edit(&mut self, f: impl Fn(Vec3) -> Vec3 + Sync) {
        self.chunks.par_iter_mut().for_each(|ch| {
            ch.aabb = Aabb {
                min: f(ch.aabb.min),
                max: f(ch.aabb.max),
            };
            ch.dirty_pos = true;
            ch.dirty_nrm = true;
        });
    }

    /// Per-chunk flags (1 = recompute normals) for `chunks` and their neighbours.
    pub fn normal_flags(&self, chunks: &[u32]) -> Vec<u32> {
        let mut flags = vec![0u32; self.chunks.len()];
        for &c in chunks {
            flags[c as usize] = 1;
            for &n in &self.chunks[c as usize].neighbors {
                flags[n as usize] = 1;
            }
        }
        flags
    }

    /// Nearest ray hit against the mesh, using chunk bounds for culling.
    pub fn raycast(&self, o: Vec3, d: Vec3) -> Option<RayHit> {
        self.raycast_in(o, d, self.chunks.len())
    }

    /// [`raycast`](Self::raycast) against the first `chunks` chunks only (a part added
    /// after them, such as the CurveTube being bent, is left out).
    pub fn raycast_in(&self, o: Vec3, d: Vec3, chunks: usize) -> Option<RayHit> {
        let inv_d = Vec3::new(1.0 / d.x, 1.0 / d.y, 1.0 / d.z);
        let mut candidates: Vec<(f32, usize)> = self.chunks[..chunks.min(self.chunks.len())]
            .iter()
            .enumerate()
            .filter_map(|(i, ch)| ch.aabb.ray_hit(o, inv_d).map(|t| (t, i)))
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut best: Option<(f32, u32)> = None;
        for (t_entry, ci) in candidates {
            if let Some((bt, _)) = best {
                if t_entry > bt {
                    break;
                }
            }
            for t in self.chunks[ci].tris() {
                if !self.hidden.is_empty() && self.hidden[t as usize] != 0 {
                    continue;
                }
                let i = 3 * t as usize;
                let a = self.positions[self.indices[i] as usize];
                let b = self.positions[self.indices[i + 1] as usize];
                let c = self.positions[self.indices[i + 2] as usize];
                if let Some(hit_t) = ray_triangle(o, d, a, b, c) {
                    if best.is_none_or(|(bt, _)| hit_t < bt) {
                        best = Some((hit_t, t));
                    }
                }
            }
        }
        best.map(|(t, tri)| {
            let i = 3 * tri as usize;
            let a = self.positions[self.indices[i] as usize];
            let b = self.positions[self.indices[i + 1] as usize];
            let c = self.positions[self.indices[i + 2] as usize];
            RayHit {
                t,
                point: o + d * t,
                normal: (b - a).cross(c - a).normalize_or(Vec3::Y),
                tri,
            }
        })
    }

    /// Whether triangle `t` is drawn.
    #[inline]
    pub fn visible(&self, t: u32) -> bool {
        self.hidden.is_empty() || self.hidden[t as usize] == 0
    }

    /// Polygroup of triangle `t`.
    #[inline]
    pub fn group(&self, t: u32) -> u16 {
        if self.groups.is_empty() { 0 } else { self.groups[t as usize] }
    }

    pub fn hidden_count(&self) -> usize {
        self.hidden_count
    }

    pub fn group_count(&self) -> usize {
        self.group_count
    }

    /// Replace the hidden flags (one per triangle, or empty for none). All-visible is
    /// stored as empty so a mesh with nothing hidden costs nothing anywhere. Refreshes the
    /// per-vertex `shown` flags.
    pub fn set_hidden(&mut self, mut hidden: Vec<u8>) {
        let count = hidden.par_iter().filter(|&&h| h != 0).count();
        if count == 0 {
            hidden = Vec::new();
        }
        debug_assert!(hidden.is_empty() || hidden.len() == self.triangle_count());
        self.shown = if hidden.is_empty() {
            Vec::new()
        } else {
            (0..self.vertex_count())
                .into_par_iter()
                .map(|v| {
                    let (a, b) = (self.vt_offsets[v] as usize, self.vt_offsets[v + 1] as usize);
                    self.vt_tris[a..b].iter().any(|&t| hidden[t as usize] == 0) as u8
                })
                .collect()
        };
        self.hidden = hidden;
        self.hidden_count = count;
        self.vis_version = self.vis_version.wrapping_add(1);
    }

    /// Replace the polygroups (one per triangle, or empty for all group 0). A single group
    /// 0 is stored as empty.
    pub fn set_groups(&mut self, mut groups: Vec<u16>) {
        debug_assert!(groups.is_empty() || groups.len() == self.triangle_count());
        let seen = groups
            .par_chunks(1 << 16)
            .fold(
                || vec![0u64; 1024],
                |mut acc, block| {
                    for &g in block {
                        acc[g as usize >> 6] |= 1 << (g & 63);
                    }
                    acc
                },
            )
            .reduce(
                || vec![0u64; 1024],
                |mut a, b| {
                    a.iter_mut().zip(b).for_each(|(x, y)| *x |= y);
                    a
                },
            );
        let count: usize = seen.iter().map(|w| w.count_ones() as usize).sum();
        if groups.is_empty() || (count == 1 && groups[0] == 0) {
            groups = Vec::new();
        }
        self.group_count = count.max(1);
        self.groups = groups;
        self.group_version = self.group_version.wrapping_add(1);
    }

    /// The index buffer of the visible triangles, in order; `None` while nothing is hidden.
    pub fn visible_indices(&self) -> Option<Vec<u32>> {
        if self.hidden.is_empty() {
            return None;
        }
        Some(
            self.indices
                .par_chunks_exact(3)
                .zip(self.hidden.par_iter())
                .filter(|(_, h)| **h == 0)
                .flat_map_iter(|(t, _)| t.iter().copied())
                .collect(),
        )
    }

    /// Polygroup of every drawn (visible) triangle in draw order, two per word.
    pub fn drawn_groups_packed(&self) -> Vec<u32> {
        let drawn: Vec<u16> = (0..self.triangle_count() as u32)
            .into_par_iter()
            .filter(|&t| self.visible(t))
            .map(|t| self.group(t))
            .collect();
        drawn
            .par_chunks(2)
            .map(|p| p[0] as u32 | (p.get(1).copied().unwrap_or(0) as u32) << 16)
            .collect()
    }

    /// Bounds of the vertices that are shown (all of them while nothing is hidden).
    pub fn visible_bounds(&self) -> Aabb {
        if self.shown.is_empty() {
            return self.bounds();
        }
        self.positions
            .par_iter()
            .zip(self.shown.par_iter())
            .filter(|(_, s)| **s != 0)
            .fold(Aabb::empty, |mut b, (p, _)| {
                b.grow(*p);
                b
            })
            .reduce(Aabb::empty, |mut a, b| {
                a.grow(b.min);
                a.grow(b.max);
                a
            })
    }

    pub fn bounds(&self) -> Aabb {
        let mut aabb = Aabb::empty();
        for ch in &self.chunks {
            aabb.grow(ch.aabb.min);
            aabb.grow(ch.aabb.max);
        }
        aabb
    }

    #[cfg(test)]
    pub fn export_obj(&self, path: &str) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, file);
        writeln!(w, "# Klay export: {} vertices, {} triangles", self.vertex_count(), self.triangle_count())?;
        self.write_obj(&mut w, "Klay", 0)?;
        w.flush()
    }

    /// Append this mesh to an OBJ stream as object `name`, with normals. `first` is the
    /// number of vertices already written (OBJ indices are global and 1-based).
    pub fn write_obj<W: std::io::Write>(&self, w: &mut W, name: &str, first: usize) -> std::io::Result<()> {
        writeln!(w, "o {name}")?;
        for p in &self.positions {
            writeln!(w, "v {} {} {}", p.x, p.y, p.z)?;
        }
        for n in self.compute_all_normals() {
            let n = unpack_normal(n);
            writeln!(w, "vn {} {} {}", n.x, n.y, n.z)?;
        }
        let first = first as u64 + 1;
        // Polygroups become OBJ groups, as ZBrush exports them.
        let mut group = None;
        for (t, tri) in self.indices.chunks_exact(3).enumerate() {
            if !self.groups.is_empty() && group != Some(self.groups[t]) {
                group = Some(self.groups[t]);
                writeln!(w, "g {name}_{}", self.groups[t])?;
            }
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|v| v as u64 + first);
            writeln!(w, "f {a}//{a} {b}//{b} {c}//{c}")?;
        }
        Ok(())
    }
}

/// Area-weighted vertex normal from the current positions.
pub(crate) fn vertex_normal(positions: &[Vec3], indices: &[u32], vt_offsets: &[u32], vt_tris: &[u32], v: u32) -> Vec3 {
    let mut n = Vec3::ZERO;
    let (ta, tb) = (vt_offsets[v as usize] as usize, vt_offsets[v as usize + 1] as usize);
    for &t in &vt_tris[ta..tb] {
        let i = 3 * t as usize;
        let a = positions[indices[i] as usize];
        let b = positions[indices[i + 1] as usize];
        let c = positions[indices[i + 2] as usize];
        n += (b - a).cross(c - a);
    }
    n.normalize_or(Vec3::Y)
}

/// Whether triangle `tri` has a vertex outside `[vert_start, vert_end)`.
pub(crate) fn crosses_chunk(tri: &[u32], vert_start: u32, vert_end: u32) -> bool {
    tri.iter().any(|&v| v < vert_start || v >= vert_end)
}

/// Reorder `indices` so the triangles whose first vertex lies in chunk `c` form one
/// contiguous range with the ones that cross into another chunk first, keeping the input
/// order within each part, and record the ranges in `chunks[c]`. `groups` (one per
/// triangle, or empty) is reordered the same way.
fn group_triangles(indices: Vec<u32>, chunks: &mut [Chunk], groups: &mut Vec<u16>, hidden: &mut Vec<u8>) -> Vec<u32> {
    // Chunk and border flag per triangle, then counting sort by (chunk, !border).
    let owner: Vec<(u32, bool)> = indices
        .par_chunks_exact(3)
        .map(|tri| {
            let c = chunks.partition_point(|ch| ch.vert_end <= tri[0]) as u32;
            let ch = &chunks[c as usize];
            (c, crosses_chunk(tri, ch.vert_start, ch.vert_end))
        })
        .collect();
    let mut start = vec![0u32; 2 * chunks.len() + 1];
    for &(c, border) in &owner {
        start[2 * c as usize + !border as usize + 1] += 1;
    }
    for i in 0..2 * chunks.len() {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut out = vec![0u32; indices.len()];
    let mut out_groups = vec![0u16; groups.len()];
    let mut out_hidden = vec![0u8; hidden.len()];
    for (t, (tri, &(c, border))) in indices.chunks_exact(3).zip(&owner).enumerate() {
        let slot = &mut fill[2 * c as usize + !border as usize];
        let at = 3 * *slot as usize;
        out[at..at + 3].copy_from_slice(tri);
        if !groups.is_empty() {
            out_groups[*slot as usize] = groups[t];
        }
        if !hidden.is_empty() {
            out_hidden[*slot as usize] = hidden[t];
        }
        *slot += 1;
    }
    *groups = out_groups;
    *hidden = out_hidden;
    for (c, ch) in chunks.iter_mut().enumerate() {
        ch.tri_start = start[2 * c];
        ch.tri_border = start[2 * c + 1];
        ch.tri_end = start[2 * c + 2];
    }
    out
}

/// Bounds of a chunk's triangles: its own contiguous vertex range plus the few vertices it
/// borrows from neighbouring chunks.
fn chunk_bounds(ch: &Chunk, positions: &[Vec3]) -> Aabb {
    let mut aabb = Aabb::empty();
    let (s, e) = ch.range();
    for p in &positions[s..e] {
        aabb.grow(*p);
    }
    for &v in &ch.external {
        aabb.grow(positions[v as usize]);
    }
    aabb
}

/// Split a slice into disjoint mutable sub-slices for ascending, non-overlapping ranges.
pub(crate) fn split_ranges<'a, T>(
    data: &'a mut [T],
    ranges: impl Iterator<Item = (usize, usize)>,
) -> Vec<&'a mut [T]> {
    let mut out = Vec::new();
    let mut rest = data;
    let mut consumed = 0;
    for (s, e) in ranges {
        debug_assert!(s >= consumed && e >= s);
        let (_, tail) = std::mem::take(&mut rest).split_at_mut(s - consumed);
        let (mid, tail) = tail.split_at_mut(e - s);
        out.push(mid);
        rest = tail;
        consumed = e;
    }
    out
}

/// Möller–Trumbore ray/triangle intersection. Returns distance along the ray.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    if t > 1e-6 { Some(t) } else { None }
}

/// Recursive median split of all vertices along the longest axis of their bounds.
fn partition_vertices(positions: &[Vec3]) -> Vec<Vec<u32>> {
    partition_ids((0..positions.len() as u32).collect(), positions)
}

/// Recursive median split of `ids` (indices into `positions`) into groups of at most
/// [`CHUNK_TARGET`] vertices. An empty input gives no groups.
pub(crate) fn partition_ids(ids: Vec<u32>, positions: &[Vec3]) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    if ids.is_empty() {
        return out;
    }
    let mut stack = vec![ids];
    while let Some(mut ids) = stack.pop() {
        if ids.len() <= CHUNK_TARGET {
            out.push(ids);
            continue;
        }
        let mut aabb = Aabb::empty();
        for &i in &ids {
            aabb.grow(positions[i as usize]);
        }
        let ext = aabb.max - aabb.min;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = ids.len() / 2;
        ids.select_nth_unstable_by(mid, |&a, &b| {
            positions[a as usize][axis].total_cmp(&positions[b as usize][axis])
        });
        let right = ids.split_off(mid);
        stack.push(ids);
        stack.push(right);
    }
    out
}

