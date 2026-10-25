//! Multiresolution subdivision levels, ZBrush style.
//!
//! Level 0 is the base mesh. Every level above it is the Loop subdivision of the level below
//! (1:4 per triangle) plus a per-vertex offset: `P[k] = smooth(P[k-1]) + D[k]`. Sculpting at
//! the active level edits its positions; the offsets of that level are refreshed for the
//! touched chunks when the stroke ends. Levels above the active one are synced lazily: edits
//! mark their chunks stale, and stepping up re-smooths the dependent child chunks and
//! re-applies their offsets, one level at a time. Edits never flow downwards.
//!
//! Subdivision keeps chunk locality: each parent chunk's children (its vertices plus the
//! midpoints of the edges it owns) are split spatially into child chunks, and every array of
//! the child mesh is produced in parallel over chunks.

use crate::mesh::{
    Aabb, CHUNK_TARGET, Chunk, PackedNormal, SculptMesh, crosses_chunk, pack_normal, partition_ids,
    split_ranges, vertex_normal,
};
use crate::polygroup::{GroupRuns, flags_of_runs, runs_of_flags};
use crate::transform::{Affine, Partial};
use glam::Vec3;
use rayon::prelude::*;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering::Relaxed};

const NONE: u32 = u32::MAX;

/// Flag on a [`Multires::tri_parents`] entry: the triangle is the centre child.
const CENTRE: u32 = 1 << 31;

/// How a child vertex is smoothed from its parent level. `[p, NONE, NONE, NONE]` is the even
/// vertex above interior parent vertex `p` (its ring comes from the parent's adjacency);
/// `[p, NONE, a, b]` is the even vertex above a border vertex with border neighbours `a` and
/// `b` (a corner, which stays put, has `a == b == p`); `[a, b, c, d]` is the odd vertex on
/// parent edge `a-b` with opposite vertices `c` and `d` (`d == NONE` on a border edge).
type Stencil = [u32; 4];

/// Chunk positions saved for undo: at level 0 these are positions, above it offsets.
pub type ChunkData = Vec<(u32, Vec<Vec3>)>;

/// Connection from a level to the one below it.
pub struct Link {
    /// Per child vertex.
    stencil: Vec<Stencil>,
    /// Parent chunk -> child chunks with at least one vertex whose stencil reads it.
    dependants: Vec<Vec<u32>>,
    /// Which child chunk of its parent chunk each child vertex went to, in natural order per
    /// parent chunk (see [`subdivide_with`]). One byte per vertex; it is what lets a parked
    /// level be rebuilt with the same vertex order without keeping the level itself.
    labels: Vec<u8>,
}

pub struct Level {
    pub mesh: SculptMesh,
    /// `None` for the base level.
    link: Option<Link>,
    /// Per chunk: offsets of the owned vertices from their smoothed positions. Empty while
    /// nothing was ever sculpted in that chunk at this level (all zero).
    offsets: Vec<Vec<Vec3>>,
    /// Chunks whose positions changed since the level above was last synced.
    stale: Vec<bool>,
}

impl Level {
    fn base(mesh: SculptMesh) -> Self {
        let n = mesh.chunks.len();
        Self {
            mesh,
            link: None,
            offsets: vec![Vec::new(); n],
            stale: vec![false; n],
        }
    }

    /// Approximate memory in bytes.
    pub fn bytes(&self) -> usize {
        let m = &self.mesh;
        let mesh = m.positions.len() * 16
            + m.indices.len() * 4
            + (m.adj_verts.len() + m.vt_tris.len() + m.adj_offsets.len() + m.vt_offsets.len()) * 4;
        let link = self.link.as_ref().map_or(0, |l| l.stencil.len() * 17);
        let offsets: usize = self.offsets.iter().map(|o| o.len() * 12).sum();
        mesh + link + offsets + m.colors.len() * 4
    }
}

/// Loop's weight for the ring of an even vertex of valence `n`.
fn loop_beta(n: usize) -> f32 {
    static TABLE: LazyLock<[f32; 64]> = LazyLock::new(|| std::array::from_fn(loop_beta_exact));
    if n < 64 { TABLE[n] } else { loop_beta_exact(n) }
}

fn loop_beta_exact(n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let t = 0.375 + 0.25 * (std::f32::consts::TAU / n as f32).cos();
    (0.625 - t * t) / n as f32
}

/// Smoothed position of a child vertex from its parent level.
#[inline]
fn smooth(parent: &SculptMesh, s: Stencil) -> Vec3 {
    let p = &parent.positions;
    if s[1] == NONE {
        let v = s[0] as usize;
        if s[2] != NONE {
            // Loop's boundary rule: the border is smoothed as a curve of its own.
            return p[v] * 0.75 + (p[s[2] as usize] + p[s[3] as usize]) * 0.125;
        }
        let nb = parent.neighbors(s[0]);
        let n = nb.len();
        if n == 0 {
            return p[v];
        }
        let beta = loop_beta(n);
        let mut sum = Vec3::ZERO;
        for &w in nb {
            sum += p[w as usize];
        }
        p[v] * (1.0 - n as f32 * beta) + sum * beta
    } else if s[3] == NONE {
        (p[s[0] as usize] + p[s[1] as usize]) * 0.5
    } else {
        (p[s[0] as usize] + p[s[1] as usize]) * 0.375
            + (p[s[2] as usize] + p[s[3] as usize]) * 0.125
    }
}

/// Per-vertex values (mask, transform weights) of a parent level carried to its child:
/// an even vertex takes its parent's value, an odd vertex the mean of its edge's ends.
/// Simpler than Loop's weights on purpose: it keeps a hard mask edge hard.
fn carry_up<T: Copy + Send + Sync>(link: &Link, parent: &[T], mid: impl Fn(T, T) -> T + Sync) -> Vec<T> {
    link.stencil
        .par_iter()
        .map(|s| {
            if s[1] == NONE {
                parent[s[0] as usize]
            } else {
                mid(parent[s[0] as usize], parent[s[1] as usize])
            }
        })
        .collect()
}

/// Per-vertex values of a child level carried to its parent: each parent vertex takes the
/// value of its even child.
fn carry_down<T: Copy + Send + Sync>(link: &Link, child: &[T], parent_count: usize) -> Vec<T> {
    // Every parent vertex has exactly one even child; find them in parallel.
    let even: Vec<AtomicU32> = (0..parent_count).map(|_| AtomicU32::new(0)).collect();
    link.stencil.par_iter().enumerate().for_each(|(i, s)| {
        if s[1] == NONE {
            even[s[0] as usize].store(i as u32, Relaxed);
        }
    });
    even.par_iter().map(|e| child[e.load(Relaxed) as usize]).collect()
}

fn mid_u8(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16 + 1) / 2) as u8
}

#[cfg(test)]
mod paint_tests {
    use super::*;
    use crate::sculpt::{Brush,BrushKind,Stroke,History,UndoStep};
    #[test]
    fn paint_survives_levels_parking_files_and_exact_undo() {
        let (p,i)=crate::mesh::cube_sphere(8,1.0);let mut doc=Multires::new(SculptMesh::from_raw(p,i),100_000);
        doc.divide();doc.divide();let level=doc.active();let positions=doc.mesh().positions.clone();
        let before:Vec<_>=doc.levels.iter().map(|l|l.mesh.colors.clone()).collect();
        let brush=Brush{kind:BrushKind::Paint,paint:true,sculpt:false,radius:0.6,color:[255,50,20],..Brush::default()};
        let mut stroke=Stroke::begin(false,false,false);
        stroke.dab(doc.mesh_mut(),&brush,Vec3::Z,Vec3::Z,-Vec3::Z,(Vec3::Z*5.0,-Vec3::Z));
        let paint=stroke.take_paint();assert!(stroke.end(doc.mesh_mut()).is_none());
        let undo=doc.finish_paint(level,paint);assert_eq!(doc.mesh().positions,positions);
        assert!(doc.levels.iter().all(|l|l.mesh.colors.iter().any(|c|c[3]>0)));
        let after:Vec<_>=doc.levels.iter().map(|l|l.mesh.colors.clone()).collect();
        let mut history=History::new(10);history.push(UndoStep::Paint(undo));history.undo(&mut doc);
        for (l,before) in doc.levels.iter().zip(&before) {assert!(l.mesh.colors.iter().all(|c|*c==[0;4]));assert!(before.is_empty());}
        history.redo(&mut doc);assert_eq!(doc.levels.iter().map(|l|l.mesh.colors.clone()).collect::<Vec<_>>(),after);
        doc.set_active(0);doc.set_active(level);assert_eq!(doc.mesh().colors,after[level]);
        let packed=doc.pack_copy();let back=packed.clone().unpack();assert_eq!(back.mesh().colors,after[level]);
        let bytes=crate::project::write_to(Vec::new(),&[("Paint",true,&packed)],0).unwrap();
        let project=crate::project::read_from(&bytes[..],bytes.len() as u64).unwrap();let back=project.tools.into_iter().next().unwrap().packed.try_unpack().unwrap();
        assert_eq!(back.levels.iter().map(|l|l.mesh.colors.clone()).collect::<Vec<_>>(),after);
        for version in 1..=3 {
            let bytes=crate::project::write_version(Vec::new(),&[("Legacy",true,&packed)],0,version).unwrap();
            let old=crate::project::read_from(&bytes[..],bytes.len() as u64).unwrap().tools.remove(0).packed.try_unpack().unwrap();
            assert!(old.levels.iter().all(|l|l.mesh.colors.is_empty()));
        }
    }
    #[test]
    fn editing_lower_paint_keeps_upper_detail_and_undo_restores_every_level() {
        let (p,i)=crate::mesh::cube_sphere(6,1.0);let mut doc=Multires::new(SculptMesh::from_raw(p,i),100_000);doc.divide();doc.divide();
        let top=doc.top();crate::paint::ensure(doc.mesh_mut());
        for (i,c) in doc.mesh_mut().colors.iter_mut().enumerate(){*c=[(i%80) as u8,0,0,200];}
        let before=doc.mesh().colors.clone();doc.set_active(0);
        let ids=vec![0];let old=crate::paint::snapshot(doc.mesh(),&ids);crate::paint::ensure(doc.mesh_mut());
        let (a,b)=doc.mesh().chunks[0].range();for c in &mut doc.mesh_mut().colors[a..b]{*c=[0,80,0,100];}
        let undo=doc.finish_paint(0,old);assert_ne!(doc.levels[top].mesh.colors,before);
        let _=doc.restore_paint(undo);assert_eq!(doc.levels[top].mesh.colors,before);
    }
}

/// Parent chunks a child vertex's stencil reads.
fn stencil_chunks(parent: &SculptMesh, s: Stencil, out: &mut Vec<u32>) {
    if s[1] == NONE {
        out.push(parent.vert_chunk[s[0] as usize]);
        for &w in parent.neighbors(s[0]) {
            out.push(parent.vert_chunk[w as usize]);
        }
    } else {
        for &v in &s {
            if v != NONE {
                out.push(parent.vert_chunk[v as usize]);
            }
        }
    }
}

/// Triangles of `parent` that contain edge `a-b`, as `(triangle, corner of a, corner of b,
/// opposite vertex)`, at most two of them, in the order of `a`'s incidence list.
fn edge_tris(
    parent: &SculptMesh,
    a: u32,
    b: u32,
) -> impl Iterator<Item = (u32, u32, u32, u32)> + '_ {
    parent
        .incident(a)
        .iter()
        .filter_map(move |&t| {
            let tri = &parent.indices[3 * t as usize..3 * t as usize + 3];
            let ia = tri.iter().position(|&x| x == a)? as u32;
            let ib = tri.iter().position(|&x| x == b)? as u32;
            let o = *tri.iter().find(|&&x| x != a && x != b)?;
            Some((t, ia, ib, o))
        })
        .take(2)
}

/// Triangles of `parent` whose first edge is `a-b` (`a < b`), in the order of `a`'s
/// incidence list: the parent triangles whose centre child the odd vertex of `a-b` owns.
fn centre_tris(parent: &SculptMesh, a: u32, b: u32) -> impl Iterator<Item = u32> + '_ {
    edge_tris(parent, a, b)
        .filter(|&(_, ia, ib, _)| ia + ib == 1)
        .map(|(t, _, _, _)| t)
}

/// Loop-subdivide `parent` once. Returns the child mesh and its link to the parent.
pub fn subdivide(parent: &SculptMesh) -> (SculptMesh, Link) {
    subdivide_with(parent, None)
}

/// Loop subdivision of `parent`. With `replay`, the child chunks are formed from the given
/// labels (from an earlier subdivision of the same topology) instead of a spatial split, so
/// the result has exactly the vertex order and chunks of that earlier level, whatever the
/// parent's positions are now.
pub fn subdivide_with(parent: &SculptMesh, replay: Option<&[u8]>) -> (SculptMesh, Link) {
    let v_count = parent.vertex_count();
    let pc = &parent.chunks;

    // 1. Edge numbering: edge `v-w` with `v < w` belongs to `v`, and v's edges follow those of
    // all lower vertices. Since vertices are sorted by chunk, each chunk owns an edge range.
    let mut edge_base = vec![0u32; v_count + 1];
    edge_base[1..]
        .par_iter_mut()
        .enumerate()
        .for_each(|(v, e)| {
            *e = parent
                .neighbors(v as u32)
                .iter()
                .filter(|&&w| w > v as u32)
                .count() as u32;
        });
    for v in 0..v_count {
        edge_base[v + 1] += edge_base[v];
    }
    let e_count = edge_base[v_count] as usize;
    let n_count = v_count + e_count;
    let edge_id = |a: u32, b: u32| -> u32 {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        let k = parent
            .neighbors(lo)
            .iter()
            .filter(|&&w| w > lo)
            .position(|&w| w == hi)
            .expect("edge of a known neighbour");
        edge_base[lo as usize] + k as u32
    };
    // Natural ids: even vertex `v` is `v`, odd vertex of edge `e` is `v_count + e`.
    let odd_nat = |a: u32, b: u32| (v_count as u32) + edge_id(a, b);

    // 2. Odd stencils in edge order, filled per parent chunk, collecting the edges on a
    // border (those with one triangle; a closed mesh has none).
    let mut odd = vec![[NONE; 4]; e_count];
    let border_edges: Vec<(u32, u32)> = {
        let ranges = pc.iter().map(|ch| {
            (
                edge_base[ch.vert_start as usize] as usize,
                edge_base[ch.vert_end as usize] as usize,
            )
        });
        split_ranges(&mut odd, ranges)
            .into_par_iter()
            .zip(pc.par_iter())
            .flat_map_iter(|(slice, ch)| {
                let mut k = 0;
                let mut border = Vec::new();
                for v in ch.vert_start..ch.vert_end {
                    for &w in parent.neighbors(v) {
                        if w <= v {
                            continue;
                        }
                        let mut s = [v, w, NONE, NONE];
                        for (i, (_, _, _, o)) in edge_tris(parent, v, w).enumerate() {
                            s[2 + i] = o;
                        }
                        if s[3] == NONE {
                            border.push((v, w));
                        }
                        slice[k] = s;
                        k += 1;
                    }
                }
                border
            })
            .collect()
    };

    // Even stencils. On a mesh with a border every border vertex gets Loop's boundary rule:
    // it follows its two border neighbours, and one that touches a single triangle is a
    // corner and stays put (so does a vertex where several borders meet). Topology alone
    // decides, so a replay gets the same stencils whatever the positions are now.
    let border: Option<Vec<Stencil>> = (!border_edges.is_empty()).then(|| {
        let mut st: Vec<Stencil> = (0..v_count as u32)
            .into_par_iter()
            .map(|v| [v, NONE, NONE, NONE])
            .collect();
        for &(a, b) in &border_edges {
            for (v, w) in [(a, b), (b, a)] {
                let s = &mut st[v as usize];
                if s[2] == NONE {
                    s[2] = w;
                } else if s[3] == NONE {
                    s[3] = w;
                } else {
                    *s = [v, NONE, v, v];
                }
            }
        }
        for &(a, b) in &border_edges {
            for v in [a, b] {
                let s = &mut st[v as usize];
                if s[3] == NONE || parent.incident(v).len() == 1 {
                    *s = [v, NONE, v, v];
                }
            }
        }
        st
    });
    drop(border_edges);
    let even_stencil = |v: u32| -> Stencil {
        match &border {
            Some(b) => b[v as usize],
            None => [v, NONE, NONE, NONE],
        }
    };

    // 3. Smoothed positions in natural order.
    let mut pos_nat = vec![Vec3::ZERO; n_count];
    {
        let (even, odd_pos) = pos_nat.split_at_mut(v_count);
        even.par_iter_mut()
            .enumerate()
            .for_each(|(v, p)| *p = smooth(parent, even_stencil(v as u32)));
        odd_pos
            .par_iter_mut()
            .zip(odd.par_iter())
            .for_each(|(p, s)| *p = smooth(parent, *s));
    }

    // 4. Child chunks: each parent chunk's vertices and the midpoints of the edges it owns,
    // split spatially into groups of at most CHUNK_TARGET, each group in natural order. The
    // group of every child is kept as a one-byte label, laid out per parent chunk in natural
    // order (parent chunk `c` starts at `vert_start + edge_base[vert_start]`), so a replay
    // can rebuild the same groups from the topology alone.
    let label_range = |ch: &Chunk| {
        let (s, e) = ch.range();
        let (es, ee) = (edge_base[s] as usize, edge_base[e] as usize);
        (s + es, e + ee)
    };
    let children = |ch: &Chunk| {
        let (s, e) = ch.range();
        let (es, ee) = (edge_base[s] as usize, edge_base[e] as usize);
        (s as u32..e as u32).chain((v_count + es) as u32..(v_count + ee) as u32)
    };
    let mut labels = vec![0u8; n_count];
    let per_parent: Vec<Vec<Vec<u32>>> = match replay {
        Some(given) => {
            assert_eq!(given.len(), n_count, "labels belong to another topology");
            labels.copy_from_slice(given);
            pc.par_iter()
                .map(|ch| {
                    let (a, b) = label_range(ch);
                    let lab = &given[a..b];
                    let k = lab.iter().max().map_or(0, |&m| m as usize + 1);
                    let mut groups = vec![Vec::new(); k];
                    for (i, id) in children(ch).enumerate() {
                        groups[lab[i] as usize].push(id);
                    }
                    groups
                })
                .collect()
        }
        None => split_ranges(&mut labels, pc.iter().map(label_range))
            .into_par_iter()
            .zip(pc.par_iter())
            .map(|(lab, ch)| {
                let mut groups = partition_ids(children(ch).collect(), &pos_nat);
                assert!(groups.len() <= 256, "too many child chunks for a byte label");
                let (s, e) = ch.range();
                let es = edge_base[s] as usize;
                for (gi, g) in groups.iter_mut().enumerate() {
                    g.sort_unstable();
                    for &id in g.iter() {
                        let id = id as usize;
                        let at = if id < v_count { id - s } else { (e - s) + (id - v_count - es) };
                        lab[at] = gi as u8;
                    }
                }
                groups
            })
            .collect(),
    };
    let mut groups: Vec<Vec<u32>> = Vec::with_capacity(pc.len() * 4);
    for g in per_parent {
        groups.extend(g);
    }
    let c_count = groups.len();
    let mut chunk_start = Vec::with_capacity(c_count + 1);
    chunk_start.push(0u32);
    for g in &groups {
        chunk_start.push(chunk_start.last().unwrap() + g.len() as u32);
    }
    debug_assert_eq!(*chunk_start.last().unwrap() as usize, n_count);
    let ranges = || {
        chunk_start
            .windows(2)
            .map(|w| (w[0] as usize, w[1] as usize))
    };

    // 5. Natural -> final ids. Every natural id is in exactly one group, so the parallel
    // scatter never writes a slot twice.
    let remap: Vec<AtomicU32> = (0..n_count).map(|_| AtomicU32::new(0)).collect();
    groups
        .par_iter()
        .zip(chunk_start.par_iter())
        .for_each(|(g, &start)| {
            for (i, &nat) in g.iter().enumerate() {
                remap[nat as usize].store(start + i as u32, Relaxed);
            }
        });
    let rm = |nat: u32| remap[nat as usize].load(Relaxed);

    // 6. Per-vertex arrays in final order, and the CSR counts.
    let mut positions = vec![Vec3::ZERO; n_count];
    let mut vert_chunk = vec![0u32; n_count];
    let mut stencil = vec![[0u32; 4]; n_count];
    let mut adj_offsets = vec![0u32; n_count + 1];
    let mut vt_offsets = vec![0u32; n_count + 1];
    {
        let shifted = || ranges().map(|(s, e)| (s + 1, e + 1));
        split_ranges(&mut positions, ranges())
            .into_par_iter()
            .zip(split_ranges(&mut vert_chunk, ranges()))
            .zip(split_ranges(&mut stencil, ranges()))
            .zip(split_ranges(&mut adj_offsets, shifted()))
            .zip(split_ranges(&mut vt_offsets, shifted()))
            .zip(groups.par_iter())
            .enumerate()
            .for_each(|(ci, (((((p, vc), st), ac), tc), g))| {
                for (i, &nat) in g.iter().enumerate() {
                    p[i] = pos_nat[nat as usize];
                    vc[i] = ci as u32;
                    let s = if (nat as usize) < v_count {
                        even_stencil(nat)
                    } else {
                        odd[nat as usize - v_count]
                    };
                    st[i] = s;
                    if s[1] == NONE {
                        ac[i] = parent.neighbors(s[0]).len() as u32;
                        tc[i] = parent.incident(s[0]).len() as u32;
                    } else {
                        let opp = (s[2] != NONE) as u32 + (s[3] != NONE) as u32;
                        ac[i] = 2 + 2 * opp;
                        tc[i] = 3 * opp;
                    }
                }
            });
    }
    drop(pos_nat);
    drop(odd);
    drop(border);
    for i in 0..n_count {
        adj_offsets[i + 1] += adj_offsets[i];
        vt_offsets[i + 1] += vt_offsets[i];
    }

    // 7. The triangles each chunk owns, by natural child id. Child triangle `4t + k` is the
    // corner of parent triangle `t` at its k-th vertex (k < 3) or its centre (k == 3); its
    // first vertex is the even vertex above that corner, or the odd vertex on the
    // triangle's first edge, so a chunk owns the children whose first vertex it holds. The
    // lists are sorted so a chunk's range of the index buffer keeps the parent's triangle
    // order, which the vertex cache rewards (owner order costs 17% of frame time).
    let owned: Vec<Vec<u32>> = ranges()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(s, e)| {
            let mut out = Vec::with_capacity((e - s) * 2);
            for st in &stencil[s..e] {
                if st[1] == NONE {
                    let p = st[0];
                    for &t in parent.incident(p) {
                        let tri = &parent.indices[3 * t as usize..3 * t as usize + 3];
                        let corner = tri.iter().position(|&x| x == p).unwrap() as u32;
                        out.push(4 * t + corner);
                    }
                } else {
                    for t in centre_tris(parent, st[0], st[1]) {
                        out.push(4 * t + 3);
                    }
                }
            }
            out.sort_unstable();
            out
        })
        .collect();
    let mut chunk_tri = Vec::with_capacity(c_count + 1);
    chunk_tri.push(0u32);
    for o in &owned {
        chunk_tri.push(chunk_tri.last().unwrap() + o.len() as u32);
    }
    let t_count = parent.indices.len() / 3 * 4;
    debug_assert_eq!(*chunk_tri.last().unwrap() as usize, t_count);

    // 8. Triangles, four per parent triangle with the parent's winding, each chunk's range
    // in the order of its owned list except that the triangles reaching into another chunk
    // go first (the renderer walks only those for neighbouring chunks). The final id of
    // every natural child id goes into `tri_remap`; each child is owned by one chunk, so
    // the parallel scatter never writes a slot twice.
    let tri_remap: Vec<AtomicU32> = (0..t_count)
        .into_par_iter()
        .map(|_| AtomicU32::new(0))
        .collect();
    let mut indices = vec![0u32; 3 * t_count];
    // Polygroups and hidden flags are inherited from the parent triangle, when it has any.
    let mut child_groups = if parent.groups.is_empty() { Vec::new() } else { vec![0u16; t_count] };
    let mut child_hidden = if parent.hidden.is_empty() { Vec::new() } else { vec![0u8; t_count] };
    let borders: Vec<u32> = {
        let tri_ranges = chunk_tri
            .windows(2)
            .map(|w| (3 * w[0] as usize, 3 * w[1] as usize));
        let per_tri = || chunk_tri.windows(2).map(|w| (w[0] as usize, w[1] as usize));
        let group_out: Vec<Option<&mut [u16]>> = if child_groups.is_empty() {
            (0..c_count).map(|_| None).collect()
        } else {
            split_ranges(&mut child_groups, per_tri()).into_iter().map(Some).collect()
        };
        let hidden_out: Vec<Option<&mut [u8]>> = if child_hidden.is_empty() {
            (0..c_count).map(|_| None).collect()
        } else {
            split_ranges(&mut child_hidden, per_tri()).into_iter().map(Some).collect()
        };
        split_ranges(&mut indices, tri_ranges)
            .into_par_iter()
            .zip(owned.par_iter())
            .zip(ranges().collect::<Vec<_>>().into_par_iter())
            .zip(chunk_tri.par_iter())
            .zip(group_out.into_par_iter().zip(hidden_out.into_par_iter()))
            .map(|((((out, o), (s, e)), &start), (mut group_out, mut hidden_out))| {
                // The children of one parent triangle share its remapped corners and edge
                // midpoints, and are consecutive in the sorted list when one chunk owns
                // several of them.
                let mut last_t = NONE;
                let mut v = [0u32; 6]; // a, b, c, ab, bc, ca
                let mut tris: Vec<[u32; 3]> = Vec::with_capacity(o.len());
                let mut crosses: Vec<bool> = Vec::with_capacity(o.len());
                for &nat in o {
                    let (t, k) = (nat / 4, nat % 4);
                    if t != last_t {
                        let tri = &parent.indices[3 * t as usize..3 * t as usize + 3];
                        v = [
                            rm(tri[0]),
                            rm(tri[1]),
                            rm(tri[2]),
                            rm(odd_nat(tri[0], tri[1])),
                            rm(odd_nat(tri[1], tri[2])),
                            rm(odd_nat(tri[2], tri[0])),
                        ];
                        last_t = t;
                    }
                    let child = match k {
                        0 => [v[0], v[3], v[5]],
                        1 => [v[1], v[4], v[3]],
                        2 => [v[2], v[5], v[4]],
                        _ => [v[3], v[4], v[5]],
                    };
                    crosses.push(crosses_chunk(&child, s as u32, e as u32));
                    tris.push(child);
                }
                // Border triangles first, then the rest, each part in list order.
                let mut n = 0u32;
                let mut n_border = 0u32;
                for pass in [true, false] {
                    for (i, &nat) in o.iter().enumerate() {
                        if crosses[i] == pass {
                            let at = 3 * n as usize;
                            out[at..at + 3].copy_from_slice(&tris[i]);
                            tri_remap[nat as usize].store(start + n, Relaxed);
                            if let Some(g) = group_out.as_deref_mut() {
                                g[n as usize] = parent.groups[(nat / 4) as usize];
                            }
                            if let Some(h) = hidden_out.as_deref_mut() {
                                h[n as usize] = parent.hidden[(nat / 4) as usize];
                            }
                            n += 1;
                        }
                    }
                    if pass {
                        n_border = n;
                    }
                }
                n_border
            })
            .collect()
    };
    drop(owned);
    let tm = |nat: u32| tri_remap[nat as usize].load(Relaxed);

    // 9. Adjacency and incidence lists, filled per chunk.
    let mut adj_verts = vec![0u32; adj_offsets[n_count] as usize];
    let mut vt_tris = vec![0u32; vt_offsets[n_count] as usize];
    {
        let adj_ranges = ranges().map(|(s, e)| (adj_offsets[s] as usize, adj_offsets[e] as usize));
        let vt_ranges = ranges().map(|(s, e)| (vt_offsets[s] as usize, vt_offsets[e] as usize));
        split_ranges(&mut adj_verts, adj_ranges)
            .into_par_iter()
            .zip(split_ranges(&mut vt_tris, vt_ranges))
            .zip(ranges().collect::<Vec<_>>().into_par_iter())
            .for_each(|((adj, vt), (s, e))| {
                let (a0, t0) = (adj_offsets[s] as usize, vt_offsets[s] as usize);
                for j in s..e {
                    let st = stencil[j];
                    let adj =
                        &mut adj[adj_offsets[j] as usize - a0..adj_offsets[j + 1] as usize - a0];
                    let vt = &mut vt[vt_offsets[j] as usize - t0..vt_offsets[j + 1] as usize - t0];
                    if st[1] == NONE {
                        let p = st[0];
                        for (k, &w) in parent.neighbors(p).iter().enumerate() {
                            adj[k] = rm(odd_nat(p, w));
                        }
                        for (k, &t) in parent.incident(p).iter().enumerate() {
                            let tri = &parent.indices[3 * t as usize..3 * t as usize + 3];
                            let corner = tri.iter().position(|&x| x == p).unwrap() as u32;
                            vt[k] = tm(4 * t + corner);
                        }
                    } else {
                        let [a, b, c, d] = st;
                        adj[0] = rm(a);
                        adj[1] = rm(b);
                        let mut k = 2;
                        for o in [c, d] {
                            if o != NONE {
                                adj[k] = rm(odd_nat(a, o));
                                adj[k + 1] = rm(odd_nat(b, o));
                                k += 2;
                            }
                        }
                        for (k, (t, ia, ib, _)) in edge_tris(parent, a, b).enumerate() {
                            vt[3 * k] = tm(4 * t + ia);
                            vt[3 * k + 1] = tm(4 * t + ib);
                            vt[3 * k + 2] = tm(4 * t + 3);
                        }
                    }
                    adj.sort_unstable();
                }
            });
    }

    drop(tri_remap);

    // 10. Chunks: vertex range and the triangle range its vertices own.
    let chunks: Vec<Chunk> = ranges()
        .zip(chunk_tri.windows(2))
        .zip(&borders)
        .map(|(((s, e), w), &b)| Chunk::new(s as u32, e as u32, w[0], w[0] + b, w[1]))
        .collect();

    // 11. Which child chunks read which parent chunks.
    let deps: Vec<Vec<u32>> = ranges()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(s, e)| {
            let mut out = Vec::new();
            for st in &stencil[s..e] {
                stencil_chunks(parent, *st, &mut out);
            }
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect();
    let mut dependants = vec![Vec::new(); pc.len()];
    for (child, d) in deps.iter().enumerate() {
        for &p in d {
            dependants[p as usize].push(child as u32);
        }
    }

    let mut mesh = SculptMesh::assemble(
        positions,
        indices,
        chunks,
        vert_chunk,
        adj_offsets,
        adj_verts,
        vt_offsets,
        vt_tris,
    );
    if !child_groups.is_empty() {
        mesh.set_groups(child_groups);
    }
    if !child_hidden.is_empty() {
        mesh.set_hidden(child_hidden);
    }
    debug_assert!(
        mesh.chunks
            .iter()
            .all(|c| (c.vert_end - c.vert_start) as usize <= CHUNK_TARGET)
    );
    (
        mesh,
        Link {
            stencil,
            dependants,
            labels,
        },
    )
}

/// Lossless compact form of a level stack, kept for a parked subtool: the base mesh in chunk
/// order, and for each level above it the labels that replay its subdivision plus the
/// offsets of the chunks that were ever sculpted at that level. Adjacency, stencils and the
/// positions of upper levels are not kept; [`unpack`](Self::unpack) rebuilds them.
#[derive(Clone)]
pub struct Packed {
    positions: Vec<Vec3>,
    indices: Vec<u32>,
    ranges: Vec<[u32; 5]>,
    levels: Vec<PackedLevel>,
    active: usize,
    budget: usize,
    /// Vertex count per level, for display while parked.
    counts: Vec<usize>,
    /// Mask of the shown level (empty when nothing is masked).
    mask: Vec<u8>,
    /// Polypaint of every level. Empty entries mean unpainted clay.
    colors: Vec<Vec<[u8;4]>>,
    /// Polygroups and hidden triangles of every level, base first, run-length coded.
    marks: Vec<Marks>,
}

/// Polygroups and hidden triangles of one level in run-length form (see `crate::polygroup`).
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Marks {
    pub groups: GroupRuns,
    pub hidden: Vec<u32>,
}

impl Marks {
    fn of(mesh: &SculptMesh) -> Self {
        Self {
            groups: GroupRuns::of(&mesh.groups),
            hidden: runs_of_flags(&mesh.hidden),
        }
    }

    fn apply(&self, mesh: &mut SculptMesh) -> Result<(), String> {
        let n = mesh.triangle_count();
        let groups = self.groups.expand(n)?;
        let hidden = flags_of_runs(&self.hidden, n)?;
        if !mesh.groups.is_empty() || !groups.is_empty() {
            mesh.set_groups(groups);
        }
        if !mesh.hidden.is_empty() || !hidden.is_empty() {
            mesh.set_hidden(hidden);
        }
        Ok(())
    }

    fn bytes(&self) -> usize {
        self.groups.bytes() + self.hidden.len() * 4
    }
}

#[derive(Clone)]
struct PackedLevel {
    labels: Vec<u8>,
    offsets: Vec<Vec<Vec3>>,
}

impl Packed {
    pub fn write_colors<W:std::io::Write>(&self,out:&mut crate::project::Out<W>)->std::io::Result<()> {
        out.u32(self.counts.len() as u32)?;
        for l in 0..self.counts.len() {out.slice(self.colors.get(l).map_or(&[][..],|c|c.as_slice()))?;}
        Ok(())
    }

    pub fn read_colors<R:std::io::Read>(&mut self,input:&mut crate::project::In<R>)->Result<(),String> {
        let n=input.u32()? as usize;
        if n!=self.counts.len(){return Err("paint has a different number of levels".into());}
        let mut colors=Vec::with_capacity(n);
        for l in 0..n {
            let c:Vec<[u8;4]>=input.vec()?;
            if !c.is_empty() && c.len()!=self.counts[l] {return Err("paint does not match the level's vertices".into());}
            if c.par_iter().any(|c|c[..3].iter().any(|&v|v>c[3])) {return Err("invalid premultiplied paint color".into());}
            colors.push(c);
        }
        self.colors=colors;Ok(())
    }
    /// Memory held, in bytes.
    pub fn bytes(&self) -> usize {
        let levels: usize = self
            .levels
            .iter()
            .map(|l| {
                l.labels.len()
                    + l.offsets.len() * std::mem::size_of::<Vec<Vec3>>()
                    + l.offsets.iter().map(|o| o.len() * 12).sum::<usize>()
            })
            .sum();
        self.positions.len() * 12
            + self.indices.len() * 4
            + self.ranges.len() * 20
            + levels
            + self.mask.len()
            + self.colors.iter().map(|c|c.len()*4).sum::<usize>()
            + self.marks.iter().map(Marks::bytes).sum::<usize>()
    }

    #[cfg(test)]
    pub fn set_marks_for_test(&mut self, marks: Vec<Marks>) {
        self.marks = marks;
    }

    /// Serialise the polygroups and hidden triangles of every level (project version 3).
    pub fn write_marks<W: std::io::Write>(&self, out: &mut crate::project::Out<W>) -> std::io::Result<()> {
        out.u32(self.marks.len() as u32)?;
        for m in &self.marks {
            out.slice(&m.groups.lens)?;
            out.slice(&m.groups.values)?;
            out.slice(&m.hidden)?;
        }
        Ok(())
    }

    /// Read what [`write_marks`](Self::write_marks) wrote. Lengths are checked against the
    /// levels when the stack is rebuilt.
    pub fn read_marks<R: std::io::Read>(&mut self, input: &mut crate::project::In<R>) -> Result<(), crate::project::Fail> {
        let n = input.u32()? as usize;
        if n != self.levels.len() + 1 {
            return Err("polygroups are stored for a different number of levels".into());
        }
        self.marks = (0..n)
            .map(|_| {
                Ok(Marks {
                    groups: GroupRuns {
                        lens: input.vec()?,
                        values: input.vec()?,
                    },
                    hidden: input.vec()?,
                })
            })
            .collect::<Result<_, crate::project::Fail>>()?;
        Ok(())
    }


    /// Vertex count per level, lowest first.
    pub fn counts(&self) -> &[usize] {
        &self.counts
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// Serialise in the project file layout of `version` (see [`crate::project`]). Offsets
    /// are written as three blocks (chunk ids, lengths, data) rather than one record per
    /// chunk; the mask from version 2 on.
    pub fn write_to<W: std::io::Write>(&self, out: &mut crate::project::Out<W>, version: u32) -> std::io::Result<()> {
        out.u32(self.active as u32)?;
        out.u64(self.budget as u64)?;
        out.slice(&self.positions)?;
        out.slice(&self.indices)?;
        out.slice(&self.ranges)?;
        out.u32(self.levels.len() as u32)?;
        for l in &self.levels {
            out.slice(&l.labels)?;
            out.u64(l.offsets.len() as u64)?;
            let ids: Vec<u32> = (0..l.offsets.len() as u32)
                .filter(|&c| !l.offsets[c as usize].is_empty())
                .collect();
            let lens: Vec<u32> = ids.iter().map(|&c| l.offsets[c as usize].len() as u32).collect();
            out.slice(&ids)?;
            out.slice(&lens)?;
            out.u64(lens.iter().map(|&n| n as u64).sum())?;
            for &c in &ids {
                out.bytes(bytemuck::cast_slice(&l.offsets[c as usize]))?;
            }
        }
        if version >= 2 {
            out.slice(&self.mask)?;
        }
        Ok(())
    }

    /// Read what [`write_to`](Self::write_to) wrote. Sizes are checked here; topology in
    /// [`try_unpack`](Self::try_unpack).
    pub fn read_from<R: std::io::Read>(
        input: &mut crate::project::In<R>,
        version: u32,
    ) -> Result<Self, crate::project::Fail> {
        let active = input.u32()? as usize;
        let budget = input.u64()? as usize;
        let positions: Vec<Vec3> = input.vec()?;
        let indices: Vec<u32> = input.vec()?;
        let ranges: Vec<[u32; 5]> = input.vec()?;
        let n_levels = input.u32()? as usize;
        if n_levels > 16 {
            return Err(format!("{n_levels} subdivision levels is not plausible"));
        }
        let mut levels = Vec::with_capacity(n_levels);
        let mut counts = vec![positions.len()];
        for _ in 0..n_levels {
            let labels: Vec<u8> = input.vec()?;
            let n_chunks = input.u64()?;
            if n_chunks > labels.len() as u64 {
                return Err("a level lists more chunks than vertices".into());
            }
            let ids: Vec<u32> = input.vec()?;
            let lens: Vec<u32> = input.vec()?;
            if ids.len() != lens.len() || ids.iter().any(|&c| c as u64 >= n_chunks) {
                return Err("sculpted chunk table is inconsistent".into());
            }
            let total = input.u64()?;
            if total != lens.iter().map(|&n| n as u64).sum::<u64>() {
                return Err("sculpted chunk sizes do not add up".into());
            }
            let mut offsets = vec![Vec::new(); n_chunks as usize];
            for (&c, &n) in ids.iter().zip(&lens) {
                // Read each chunk's block straight into its own vector.
                let mut block = vec![Vec3::ZERO; n as usize];
                let size = n as u64 * 12;
                if size > input.left() {
                    return Err("the file ends early (truncated or not a Klay project)".into());
                }
                tmp_fill(input, bytemuck::cast_slice_mut(&mut block))?;
                offsets[c as usize] = block;
            }
            counts.push(labels.len());
            levels.push(PackedLevel { labels, offsets });
        }
        if active >= counts.len() {
            return Err("the shown level is not one of the stored levels".into());
        }
        // Version 1 files have no mask.
        let mask: Vec<u8> = if version >= 2 { input.vec()? } else { Vec::new() };
        if !mask.is_empty() && mask.len() != counts[active] {
            return Err("the stored mask does not match the shown level".into());
        }
        let marks = vec![Marks::default(); levels.len() + 1];
        Ok(Packed {
            positions,
            indices,
            ranges,
            levels,
            active,
            budget,
            counts,
            marks,
            mask,
            colors: Vec::new(),
        })
    }

    /// Rebuild the full level stack, checking everything subdivision relies on first: index
    /// bounds, the chunk table, two-manifold edges with distinct corners, and for every
    /// level the label table and the sculpted chunk sizes. Anything else that still goes
    /// wrong while rebuilding is reported instead of crashing.
    pub fn try_unpack(self) -> Result<Multires, String> {
        check_base(&self.positions, &self.indices, &self.ranges)?;
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || self.unpack_checked()));
        match checked {
            Ok(r) => r,
            Err(_) => Err("the stored mesh could not be rebuilt (corrupt data)".into()),
        }
    }

    /// Rebuild the full level stack: the base mesh as it was, then every level by replaying
    /// its subdivision and adding its offsets. Only for data this build produced; files go
    /// through [`try_unpack`](Self::try_unpack).
    /// Triangles of the base level.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn unpack(self) -> Multires {
        self.unpack_checked().expect("packed data from this session is consistent")
    }

    fn unpack_checked(self) -> Result<Multires, String> {
        let Packed {
            positions,
            indices,
            ranges,
            levels: packed,
            active,
            budget,
            marks,
            mask,
            colors,
            ..
        } = self;
        let mut levels = vec![Level::base(SculptMesh::from_chunked(positions, indices, &ranges))];
        if levels[0].mesh.edges_over_two_triangles() {
            return Err("the stored mesh has edges shared by more than two triangles".into());
        }
        for pl in packed {
            let parent = &levels.last().unwrap().mesh;
            check_labels(parent, &pl.labels)?;
            let (mut mesh, link) = subdivide_with(parent, Some(&pl.labels));
            if pl.offsets.len() != mesh.chunks.len()
                || pl.offsets.iter().zip(&mesh.chunks).any(|(o, ch)| {
                    !o.is_empty() && o.len() != (ch.vert_end - ch.vert_start) as usize
                })
            {
                return Err("sculpted offsets do not match the rebuilt level".into());
            }
            let sculpted: Vec<u32> = (0..pl.offsets.len() as u32)
                .filter(|&c| !pl.offsets[c as usize].is_empty())
                .collect();
            {
                let SculptMesh {
                    positions, chunks, ..
                } = &mut mesh;
                split_ranges(positions, sculpted.iter().map(|&c| chunks[c as usize].range()))
                    .into_par_iter()
                    .zip(sculpted.par_iter())
                    .for_each(|(slice, &c)| {
                        for (p, d) in slice.iter_mut().zip(&pl.offsets[c as usize]) {
                            *p += *d;
                        }
                    });
            }
            mesh.finish_edit(&sculpted);
            let n = mesh.chunks.len();
            levels.push(Level {
                mesh,
                link: Some(link),
                offsets: pl.offsets,
                stale: vec![false; n],
            });
        }
        // Groups and visibility as they were on every level (the replay inherited them
        // from below, which edits made at upper levels may differ from).
        for (lvl, m) in levels.iter_mut().zip(&marks) {
            m.apply(&mut lvl.mesh)?;
        }
        if active >= levels.len() || (!mask.is_empty() && mask.len() != levels[active].mesh.vertex_count()) {
            return Err("the stored mask does not match the shown level".into());
        }
        crate::mask::replace(&mut levels[active].mesh, mask);
        for (level,colors) in levels.iter_mut().zip(colors) {
            if !colors.is_empty() && colors.len()!=level.mesh.vertex_count(){return Err("paint count does not match the rebuilt level".into());}
            level.mesh.colors=colors;
            for c in &mut level.mesh.chunks {c.dirty_color=true;}
        }
        Ok(Multires {
            levels,
            active,
            budget,
            generation: next_generation(),
            layers: crate::layers::Layers::default(),
        })
    }
}

fn tmp_fill<R: std::io::Read>(input: &mut crate::project::In<R>, buf: &mut [u8]) -> Result<(), crate::project::Fail> {
    input.fill_raw(buf)
}

/// Checks on a stored base mesh that the rebuild relies on.
fn check_base(positions: &[Vec3], indices: &[u32], ranges: &[[u32; 5]]) -> Result<(), String> {
    let n = positions.len() as u32;
    if n == 0 || indices.is_empty() || indices.len() % 3 != 0 || ranges.is_empty() {
        return Err("the stored mesh is empty or malformed".into());
    }
    if indices.par_chunks_exact(3).any(|t| {
        t.iter().any(|&v| v >= n) || t[0] == t[1] || t[1] == t[2] || t[0] == t[2]
    }) {
        return Err("the stored triangles reference missing vertices or repeat a corner".into());
    }
    let tris = (indices.len() / 3) as u32;
    let (mut v, mut t) = (0u32, 0u32);
    for r in ranges {
        let [vs, ve, ts, tb, te] = *r;
        if vs != v || ve <= vs || ve - vs > CHUNK_TARGET as u32 || ts != t || tb < ts || te < tb {
            return Err("the stored chunk table is inconsistent".into());
        }
        v = ve;
        t = te;
    }
    if v != n || t != tris {
        return Err("the stored chunk table does not cover the mesh".into());
    }
    // Every chunk owns exactly the triangles whose first vertex it owns, border ones first.
    let bad = ranges.par_iter().any(|&[vs, ve, ts, tb, te]| {
        (ts..te).any(|t| {
            let tri = &indices[3 * t as usize..3 * t as usize + 3];
            let owned = tri[0] >= vs && tri[0] < ve;
            !owned || crosses_chunk(tri, vs, ve) != (t < tb)
        })
    });
    if bad {
        return Err("the stored triangles are not grouped by chunk".into());
    }
    Ok(())
}

/// Checks that `labels` can replay a subdivision of `parent`: one label per child, laid out
/// per parent chunk, each parent chunk's labels forming groups `0..k` with no empty group
/// and none larger than a chunk.
fn check_labels(parent: &SculptMesh, labels: &[u8]) -> Result<(), String> {
    let owned_edges = |v: u32| parent.neighbors(v).iter().filter(|&&w| w > v).count();
    let expect = parent.vertex_count() + parent.edge_count();
    if labels.len() != expect {
        return Err(format!(
            "a level has {} vertices where its parent subdivides into {expect}",
            labels.len()
        ));
    }
    let sizes: Vec<usize> = parent
        .chunks
        .par_iter()
        .map(|ch| (ch.vert_start..ch.vert_end).map(|v| 1 + owned_edges(v)).sum())
        .collect();
    let mut at = 0;
    for size in sizes {
        let lab = &labels[at..at + size];
        at += size;
        let mut groups = [0usize; 256];
        for &l in lab {
            groups[l as usize] += 1;
        }
        let k = lab.iter().max().map_or(0, |&m| m as usize + 1);
        if groups[..k].iter().any(|&g| g == 0 || g > CHUNK_TARGET) {
            return Err("a level's chunk labels are inconsistent".into());
        }
    }
    Ok(())
}

/// Draw-only copy of a parked subtool for the GPU (see [`Multires::stand_in`]).
pub struct StandIn {
    pub positions: Vec<Vec3>,
    pub normals: Vec<PackedNormal>,
    pub colors: Vec<[u8;4]>,
    pub indices: Vec<u32>,
    /// The level whose topology it uses.
    pub level: usize,
    pub bounds: Aabb,
}

/// The level stack.
pub struct Multires {
    levels: Vec<Level>,
    active: usize,
    /// Divide is refused when the next level would have more vertices than this.
    pub budget: usize,
    /// Unique per stack whose levels were built afresh (new, rebuilt, unpacked, or with
    /// levels deleted below), so the renderer knows its cached levels are someone else's.
    pub generation: u64,
    /// Tool › Layers, on the highest level (not packed: a parked subtool keeps them with
    /// its undo history).
    pub layers: crate::layers::Layers,
}

fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Relaxed)
}

impl Multires {
    pub fn new(mesh: SculptMesh, budget: usize) -> Self {
        Self {
            levels: vec![Level::base(mesh)],
            active: 0,
            budget,
            generation: next_generation(),
            layers: crate::layers::Layers::default(),
        }
    }

    /// Put `part` (or nothing) in place of everything after `keep` in the base mesh and
    /// return what was there (CurveTube adds its tube this way; its undo steps too). Only
    /// for a stack without upper levels; it counts as a new stack for the renderer.
    pub fn swap_part(&mut self, keep: crate::mesh::Extent, part: Option<&SculptMesh>) -> Option<SculptMesh> {
        let old = self.swap_part_in_place(keep, part);
        self.generation = next_generation();
        old
    }

    /// [`swap_part`](Self::swap_part) for a caller that updates the renderer's copy itself
    /// (`Gfx::swap_tail`): the stack keeps its generation.
    pub fn swap_part_in_place(&mut self, keep: crate::mesh::Extent, part: Option<&SculptMesh>) -> Option<SculptMesh> {
        assert_eq!(self.levels.len(), 1, "parts are added to a stack without levels");
        let level = &mut self.levels[0];
        let old = level.mesh.swap_tail(keep, part);
        let n = level.mesh.chunks.len();
        level.offsets.truncate(keep.chunks);
        level.offsets.resize(n, Vec::new());
        level.stale.truncate(keep.chunks);
        level.stale.resize(n, false);
        self.active = 0;
        old
    }

    /// Delete the levels above the shown one (ZBrush: Del Higher). Returns how many went.
    pub fn delete_higher(&mut self) -> usize {
        let n = self.levels.len() - 1 - self.active;
        self.levels.truncate(self.active + 1);
        n
    }

    /// Make the shown level the base, deleting the levels below it (ZBrush: Del Lower). Its
    /// sculpting is kept as plain positions. Returns how many went.
    pub fn delete_lower(&mut self) -> usize {
        let n = self.active;
        if n == 0 {
            return 0;
        }
        self.sync_to(self.active);
        let lvl = self.levels.drain(..n + 1).last().expect("the shown level exists");
        // Chunks edited here but not yet carried up stay marked.
        let mut base = Level::base(lvl.mesh);
        base.stale = lvl.stale;
        self.levels.insert(0, base);
        self.active = 0;
        self.generation = next_generation();
        n
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn count(&self) -> usize {
        self.levels.len()
    }

    pub fn top(&self) -> usize {
        self.levels.len() - 1
    }

    /// The mesh being displayed and sculpted.
    pub fn mesh(&self) -> &SculptMesh {
        &self.levels[self.active].mesh
    }

    pub fn mesh_mut(&mut self) -> &mut SculptMesh {
        &mut self.levels[self.active].mesh
    }

    pub fn level_mesh(&self, level: usize) -> &SculptMesh {
        &self.levels[level].mesh
    }

    /// Vertex count per level, lowest first.
    pub fn vertex_counts(&self) -> Vec<usize> {
        self.levels.iter().map(|l| l.mesh.vertex_count()).collect()
    }

    /// Vertices the level above the top one would have.
    pub fn next_vertex_count(&self) -> usize {
        let m = &self.levels[self.top()].mesh;
        m.vertex_count() + m.edge_count()
    }

    pub fn can_divide(&self) -> bool {
        self.next_vertex_count() <= self.budget
    }

    /// Approximate memory of all levels, in bytes.
    pub fn bytes(&self) -> usize {
        self.levels.iter().map(Level::bytes).sum()
    }

    /// Subdivide the top level and make the new level active. Returns false when the
    /// budget would be exceeded.
    pub fn divide(&mut self) -> bool {
        if !self.can_divide() {
            return false;
        }
        let top = self.top();
        self.sync_to(top);
        let (mut mesh, link) = subdivide(&self.levels[top].mesh);
        if !self.levels[top].mesh.colors.is_empty() {
            mesh.colors=carry_up(&link,&self.levels[top].mesh.colors,|a,b|std::array::from_fn(|i|mid_u8(a[i],b[i])));
            for c in &mut mesh.chunks {c.dirty_color=true;}
        }
        let n = mesh.chunks.len();
        self.levels.push(Level {
            mesh,
            link: Some(link),
            offsets: vec![Vec::new(); n],
            stale: vec![false; n],
        });
        self.levels[top].stale.iter_mut().for_each(|s| *s = false);
        self.move_mask(self.active, top + 1);
        self.active = top + 1;
        true
    }

    /// Change the displayed level, syncing upwards as needed. Returns the number of chunks
    /// that were re-smoothed on the way up.
    pub fn set_active(&mut self, level: usize) -> usize {
        let level = level.min(self.top());
        let synced = if level > self.active {
            self.sync_to(level)
        } else {
            0
        };
        self.move_mask(self.active, level);
        self.active = level;
        synced
    }

    /// Carry the mask from level `from` (where it is) to level `to`, one level at a time.
    /// Both meshes are marked for a mask upload: `to` gains it, `from` loses it.
    fn move_mask(&mut self, from: usize, to: usize) {
        if from == to || self.levels[from].mesh.mask.is_empty() {
            return;
        }
        let t = std::time::Instant::now();
        let mut mask = crate::mask::replace(&mut self.levels[from].mesh, Vec::new());
        let mut l = from;
        while l < to {
            mask = carry_up(self.levels[l + 1].link.as_ref().unwrap(), &mask, mid_u8);
            l += 1;
        }
        while l > to {
            let n = self.levels[l - 1].mesh.vertex_count();
            mask = carry_down(self.levels[l].link.as_ref().unwrap(), &mask, n);
            l -= 1;
        }
        crate::mask::replace(&mut self.levels[to].mesh, mask);
        log::debug!("mask carried from level {from} to {to} in {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
    }

    /// Carry a paint stroke through the stack while retaining detail on upper levels.
    /// Every changed level is snapshotted so undo is exact even when colors clamp.
    pub fn finish_paint(&mut self,level:usize,before:crate::paint::Chunks)->crate::paint::Snapshot {
        let mut delta=vec![[0i16;4];self.levels[level].mesh.vertex_count()];
        let mesh=&self.levels[level].mesh;
        for (c,old) in &before {
            let (a,_)=mesh.chunks[*c as usize].range();
            for (i,old) in old.iter().enumerate() {
                let now=crate::paint::at(mesh,a+i);
                delta[a+i]=std::array::from_fn(|k|now[k] as i16-old[k] as i16);
            }
        }
        let mut snapshot=crate::paint::Snapshot {active:level,levels:vec![(level,before)]};
        for l in (0..level).rev() {
            let n=self.levels[l].mesh.vertex_count();
            let colors=carry_down(self.levels[l+1].link.as_ref().unwrap(),&self.levels[l+1].mesh.colors,n);
            let mesh=&mut self.levels[l].mesh;
            let ids:Vec<_>=mesh.chunks.iter().enumerate().filter_map(|(i,c)| {
                let (a,b)=c.range();
                (a..b).any(|v|crate::paint::at(mesh,v)!=colors[v]).then_some(i as u32)
            }).collect();
            if !ids.is_empty() {
                let old=crate::paint::snapshot(mesh,&ids);
                mesh.colors=colors;
                for c in ids {mesh.chunks[c as usize].dirty_color=true;}
                snapshot.levels.push((l,old));
            }
        }
        for l in level+1..self.levels.len() {
            delta=carry_up(self.levels[l].link.as_ref().unwrap(),&delta,|a,b|std::array::from_fn(|k|((a[k] as f32+b[k] as f32)*0.5).round() as i16));
            let mesh=&mut self.levels[l].mesh;
            let ids:Vec<_>=mesh.chunks.iter().enumerate().filter_map(|(i,c)| {
                let (a,b)=c.range();delta[a..b].iter().any(|d|*d!=[0;4]).then_some(i as u32)
            }).collect();
            if !ids.is_empty() {
                let old=crate::paint::snapshot(mesh,&ids);crate::paint::ensure(mesh);
                mesh.colors.par_iter_mut().zip(delta.par_iter()).for_each(|(c,d)| {
                    let a=(c[3] as i16+d[3]).clamp(0,255) as u8;
                    for k in 0..3 {c[k]=(c[k] as i16+d[k]).clamp(0,a as i16) as u8;}
                    c[3]=a;
                });
                for c in ids {mesh.chunks[c as usize].dirty_color=true;}
                snapshot.levels.push((l,old));
            }
        }
        snapshot
    }

    pub fn restore_paint(&mut self,data:crate::paint::Snapshot)->crate::paint::Snapshot {
        self.set_active(data.active);
        let levels=data.levels.into_iter().map(|(l,c)|(l,crate::paint::restore(&mut self.levels[l].mesh,c))).collect();
        crate::paint::Snapshot {active:data.active,levels}
    }

    /// Put back mask data saved for `level` and return what undoes it. The shown level
    /// becomes `level` (the mask lives on the shown level).
    pub fn restore_mask(&mut self, level: usize, data: crate::mask::MaskData) -> crate::mask::MaskData {
        use crate::mask::MaskData;
        self.set_active(level);
        let mesh = &mut self.levels[level].mesh;
        match data {
            MaskData::Whole(mask) => MaskData::Whole(crate::mask::replace(mesh, mask)),
            MaskData::Chunks(chunks) => MaskData::Chunks(crate::mask::restore_chunks(mesh, chunks)),
        }
    }

    /// Bring every level up to `level` in line with the edits below it. Returns the number
    /// of chunks re-smoothed.
    pub fn sync_to(&mut self, level: usize) -> usize {
        (0..level.min(self.top())).map(|l| self.sync_level(l)).sum()
    }

    /// Make level `l + 1` consistent with the stale chunks of level `l`. Returns the number
    /// of child chunks recomputed.
    fn sync_level(&mut self, l: usize) -> usize {
        let (lower, upper) = self.levels.split_at_mut(l + 1);
        let parent = &mut lower[l];
        let child = &mut upper[0];
        let stale: Vec<u32> = parent
            .stale
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .map(|(i, _)| i as u32)
            .collect();
        if stale.is_empty() {
            return 0;
        }
        let link = child
            .link
            .as_ref()
            .expect("level above the base has a link");
        let mut targets: Vec<u32> = stale
            .iter()
            .flat_map(|&c| link.dependants[c as usize].iter().copied())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        let pm = &parent.mesh;
        let Level { mesh, offsets, .. } = child;
        let SculptMesh {
            positions, chunks, ..
        } = mesh;
        split_ranges(
            positions,
            targets.iter().map(|&t| chunks[t as usize].range()),
        )
        .into_par_iter()
        .zip(targets.par_iter())
        .for_each(|(slice, &t)| {
            let s = chunks[t as usize].vert_start as usize;
            let off = &offsets[t as usize];
            for (i, p) in slice.iter_mut().enumerate() {
                let base = smooth(pm, link.stencil[s + i]);
                *p = if off.is_empty() { base } else { base + off[i] };
            }
        });
        mesh.finish_edit(&targets);
        for &c in &stale {
            parent.stale[c as usize] = false;
        }
        for &t in &targets {
            child.stale[t as usize] = true;
        }
        targets.len()
    }

    /// Show the highest level (layers live there), returning whether it changed.
    fn show_top(&mut self) {
        let top = self.top();
        if self.active != top {
            self.set_active(top);
        }
    }

    /// Change the shape at the highest level by `f`, keeping its offsets right (as a stroke
    /// over every chunk would).
    fn reshape_top(&mut self, f: impl FnOnce(&mut SculptMesh, &mut crate::layers::Layers)) {
        self.show_top();
        let top = self.top();
        let all: Vec<u32> = (0..self.mesh().chunks.len() as u32).collect();
        let snapshot = self.mesh().snapshot_chunks(&all);
        let mut layers = std::mem::take(&mut self.layers);
        f(self.mesh_mut(), &mut layers);
        self.layers = layers;
        self.finish_stroke(top, snapshot);
    }

    /// Fold the recording layer's sculpting in (see [`crate::layers::Layers::freeze`]); only
    /// at the highest level, where it records.
    pub fn freeze_layers(&mut self) {
        if self.active == self.top() {
            let mut layers = std::mem::take(&mut self.layers);
            layers.freeze(&self.mesh().positions);
            self.layers = layers;
        }
    }

    /// Replace the layers with `target`. With `reshape` the shape follows (the layers' part
    /// of it becomes `target`'s); without, it stays as it is (Bake). Returns the state
    /// replaced, which undoes it with the same `reshape`.
    pub fn swap_layers(&mut self, target: crate::layers::Layers, reshape: bool) -> crate::layers::Layers {
        if reshape {
            let mut old = None;
            self.reshape_top(|mesh, layers| old = Some(layers.swap(mesh, target)));
            return old.unwrap();
        }
        self.show_top();
        self.freeze_layers();
        let mut target = target;
        if let Some((_, base)) = target.rec.as_mut() {
            *base = self.mesh().positions.clone();
        }
        std::mem::replace(&mut self.layers, target)
    }

    /// Set layer `index`'s intensity, the shape following; returns the old intensity.
    pub fn set_layer_intensity(&mut self, index: usize, intensity: f32) -> f32 {
        let mut old = intensity;
        self.reshape_top(|mesh, layers| {
            layers.freeze(&mesh.positions);
            let Some(layer) = layers.list.get_mut(index) else { return };
            old = layer.intensity;
            if layer.delta.len() == mesh.vertex_count() {
                let k = if layer.visible { intensity - layer.intensity } else { 0.0 };
                mesh.positions.par_iter_mut().zip(layer.delta.par_iter()).for_each(|(p, d)| *p += *d * k);
            }
            layer.intensity = intensity;
            if let Some((_, base)) = layers.rec.as_mut() {
                base.copy_from_slice(&mesh.positions);
            }
            let all: Vec<u32> = (0..mesh.chunks.len() as u32).collect();
            mesh.finish_edit(&all);
        });
        old
    }

    /// Record a finished stroke at `level`: `snapshot` holds the positions of the touched
    /// chunks before the stroke. Refreshes the level's offsets for those chunks and marks
    /// them stale for the level above. Returns the chunk data an undo needs (positions at
    /// level 0, offsets above it).
    pub fn finish_stroke(&mut self, level: usize, mut snapshot: ChunkData) -> ChunkData {
        if level == 0 {
            for (c, _) in &snapshot {
                self.levels[0].stale[*c as usize] = true;
            }
            return snapshot;
        }
        let (lower, upper) = self.levels.split_at_mut(level);
        let parent = &lower[level - 1].mesh;
        let lvl = &mut upper[0];
        let link = lvl.link.as_ref().unwrap();
        let mesh = &lvl.mesh;
        // One smoothing pass per touched chunk gives both the old offsets (for undo) and the
        // new ones.
        let new: Vec<(u32, Vec<Vec3>)> = snapshot
            .par_iter_mut()
            .map(|(c, saved)| {
                let (s, e) = mesh.chunks[*c as usize].range();
                let mut now = Vec::with_capacity(e - s);
                for i in 0..e - s {
                    let base = smooth(parent, link.stencil[s + i]);
                    saved[i] -= base;
                    now.push(mesh.positions[s + i] - base);
                }
                (*c, now)
            })
            .collect();
        for (c, off) in new {
            lvl.offsets[c as usize] = off;
            lvl.stale[c as usize] = true;
        }
        snapshot
    }

    /// Restore chunk data saved by [`finish_stroke`](Self::finish_stroke) or a previous
    /// restore, and return the data that undoes this restore. The displayed level is kept
    /// in sync.
    pub fn restore(&mut self, level: usize, mut saved: ChunkData) -> ChunkData {
        saved.sort_unstable_by_key(|(c, _)| *c);
        let ids: Vec<u32> = saved.iter().map(|(c, _)| *c).collect();
        let inverse = if level == 0 {
            let mesh = &mut self.levels[0].mesh;
            let inverse = mesh.snapshot_chunks(&ids);
            mesh.restore_chunks(&saved);
            mesh.finish_edit(&ids);
            inverse
        } else {
            // Offsets only mean something against a parent the level is in sync with.
            self.sync_to(level);
            let (lower, upper) = self.levels.split_at_mut(level);
            let parent = &lower[level - 1].mesh;
            let lvl = &mut upper[0];
            let link = lvl.link.as_ref().unwrap();
            let mut inverse = Vec::with_capacity(saved.len());
            for (c, data) in saved {
                let n = data.len();
                let old = std::mem::replace(&mut lvl.offsets[c as usize], data);
                inverse.push((
                    c,
                    if old.is_empty() {
                        vec![Vec3::ZERO; n]
                    } else {
                        old
                    },
                ));
            }
            let Level { mesh, offsets, .. } = lvl;
            let SculptMesh {
                positions, chunks, ..
            } = mesh;
            split_ranges(positions, ids.iter().map(|&c| chunks[c as usize].range()))
                .into_par_iter()
                .zip(ids.par_iter())
                .for_each(|(slice, &c)| {
                    let s = chunks[c as usize].vert_start as usize;
                    let off = &offsets[c as usize];
                    for (i, p) in slice.iter_mut().enumerate() {
                        *p = smooth(parent, link.stencil[s + i]) + off[i];
                    }
                });
            mesh.finish_edit(&ids);
            inverse
        };
        for &c in &ids {
            self.levels[level].stale[c as usize] = true;
        }
        if self.active > level {
            self.sync_to(self.active);
        }
        inverse
    }

    /// Remove the top level (undo of a divide). The base level stays.
    pub fn pop_level(&mut self) -> Option<Level> {
        if self.levels.len() <= 1 {
            return None;
        }
        let top = self.top();
        if self.active == top {
            self.move_mask(top, top - 1);
        }
        let level = self.levels.pop();
        self.active = self.active.min(self.top());
        level
    }

    /// Put back a level removed by [`pop_level`](Self::pop_level) (redo of a divide) and show it.
    pub fn push_level(&mut self, level: Level) {
        self.levels.push(level);
        let top = self.top();
        self.set_active(top);
    }

    /// Bounds of the displayed level.
    pub fn bounds(&self) -> Aabb {
        self.mesh().bounds()
    }

    /// The compact form of this stack (see [`Packed`]). Consumes the stack: this is what
    /// frees the memory of a subtool that is parked.
    pub fn pack(mut self) -> Packed {
        let counts = self.vertex_counts();
        let marks = self.marks();
        let colors=self.levels.iter_mut().map(|l|std::mem::take(&mut l.mesh.colors)).collect();
        let mask = std::mem::take(&mut self.levels[self.active].mesh.mask);
        let Multires {
            levels,
            active,
            budget,
            ..
        } = self;
        let mut it = levels.into_iter();
        let base = it.next().expect("a stack has a base level").mesh;
        let ranges = base.chunk_ranges();
        let SculptMesh {
            positions, indices, ..
        } = base;
        let levels = it
            .map(|l| PackedLevel {
                labels: l.link.expect("level above the base has a link").labels,
                offsets: l.offsets,
            })
            .collect();
        Packed {
            positions,
            indices,
            ranges,
            levels,
            active,
            budget,
            counts,
            marks,
            mask,
            colors,
        }
    }

    /// A compact copy, leaving this stack as it is (Duplicate).
    pub fn pack_copy(&self) -> Packed {
        let base = &self.levels[0].mesh;
        Packed {
            positions: base.positions.clone(),
            indices: base.indices.clone(),
            ranges: base.chunk_ranges(),
            levels: self.levels[1..]
                .iter()
                .map(|l| PackedLevel {
                    labels: l.link.as_ref().unwrap().labels.clone(),
                    offsets: l.offsets.clone(),
                })
                .collect(),
            active: self.active,
            budget: self.budget,
            counts: self.vertex_counts(),
            marks: self.marks(),
            mask: self.mesh().mask.clone(),
            colors:self.levels.iter().map(|l|l.mesh.colors.clone()).collect(),
        }
    }

    /// Polygroups and hidden triangles of every level, run-length coded.
    pub fn marks(&self) -> Vec<Marks> {
        self.levels.par_iter().map(|l| Marks::of(&l.mesh)).collect()
    }

    /// For every triangle of level `l` (above the base): the triangle of level `l - 1` it
    /// was cut from, with [`CENTRE`] set for the middle one of the four. Found from the
    /// stencils: a corner child holds one even vertex (above the parent's corner) and the
    /// midpoints of the two edges from it; the centre child holds three midpoints.
    fn tri_parents(&self, l: usize) -> Vec<u32> {
        let parent = &self.levels[l - 1].mesh;
        let child = &self.levels[l].mesh;
        let st = &self.levels[l].link.as_ref().expect("level above the base has a link").stencil;
        child
            .indices
            .par_chunks_exact(3)
            .map(|tri| {
                let even = tri.iter().find(|&&v| st[v as usize][1] == NONE);
                let (a, b, c, centre) = match even {
                    Some(&e) => {
                        let p = st[e as usize][0];
                        let mut ends = [NONE; 2];
                        let mut k = 0;
                        for &v in tri {
                            let s = st[v as usize];
                            if s[1] != NONE && k < 2 {
                                ends[k] = if s[0] == p { s[1] } else { s[0] };
                                k += 1;
                            }
                        }
                        (p, ends[0], ends[1], false)
                    }
                    None => {
                        let (s0, s1) = (st[tri[0] as usize], st[tri[1] as usize]);
                        let third = if s1[0] != s0[0] && s1[0] != s0[1] { s1[0] } else { s1[1] };
                        (s0[0], s0[1], third, true)
                    }
                };
                let t = parent
                    .incident(a)
                    .iter()
                    .copied()
                    .find(|&t| {
                        let pt = &parent.indices[3 * t as usize..3 * t as usize + 3];
                        pt.contains(&b) && pt.contains(&c)
                    })
                    .expect("a child triangle lies in a parent triangle");
                t | if centre { CENTRE } else { 0 }
            })
            .collect()
    }

    /// Hide triangles of `level` (one flag per triangle, empty for none) and carry the
    /// change to every other level: above, a triangle is hidden exactly when the one it was
    /// cut from is; below, when all four of its children are.
    pub fn set_hidden(&mut self, level: usize, hidden: Vec<u8>) {
        self.levels[level].mesh.set_hidden(hidden);
        for l in level + 1..self.levels.len() {
            let h = if self.levels[l - 1].mesh.hidden.is_empty() {
                Vec::new()
            } else {
                let parents = self.tri_parents(l);
                let above = &self.levels[l - 1].mesh.hidden;
                parents.par_iter().map(|&p| above[(p & !CENTRE) as usize]).collect()
            };
            self.levels[l].mesh.set_hidden(h);
        }
        for l in (0..level).rev() {
            let h = if self.levels[l + 1].mesh.hidden.is_empty() {
                Vec::new()
            } else {
                let parents = self.tri_parents(l + 1);
                let below = &self.levels[l + 1].mesh.hidden;
                let all: Vec<AtomicU8> = (0..self.levels[l].mesh.triangle_count())
                    .into_par_iter()
                    .map(|_| AtomicU8::new(1))
                    .collect();
                parents.par_iter().zip(below.par_iter()).for_each(|(&p, &h)| {
                    if h == 0 {
                        all[(p & !CENTRE) as usize].store(0, Relaxed);
                    }
                });
                all.into_iter().map(AtomicU8::into_inner).collect()
            };
            self.levels[l].mesh.set_hidden(h);
        }
    }

    /// Set the polygroups of `level` and carry them to every other level: above, a
    /// triangle takes the group of the one it was cut from; below, the group of its centre
    /// child.
    pub fn set_groups(&mut self, level: usize, groups: Vec<u16>) {
        self.levels[level].mesh.set_groups(groups);
        for l in level + 1..self.levels.len() {
            let g = if self.levels[l - 1].mesh.groups.is_empty() {
                Vec::new()
            } else {
                let parents = self.tri_parents(l);
                let above = &self.levels[l - 1].mesh.groups;
                parents.par_iter().map(|&p| above[(p & !CENTRE) as usize]).collect()
            };
            self.levels[l].mesh.set_groups(g);
        }
        for l in (0..level).rev() {
            let g = if self.levels[l + 1].mesh.groups.is_empty() {
                Vec::new()
            } else {
                let parents = self.tri_parents(l + 1);
                let below = &self.levels[l + 1].mesh.groups;
                let out: Vec<AtomicU16> = (0..self.levels[l].mesh.triangle_count())
                    .into_par_iter()
                    .map(|_| AtomicU16::new(0))
                    .collect();
                parents.par_iter().zip(below.par_iter()).for_each(|(&p, &g)| {
                    if p & CENTRE != 0 {
                        out[(p & !CENTRE) as usize].store(g, Relaxed);
                    }
                });
                out.into_iter().map(AtomicU16::into_inner).collect()
            };
            self.levels[l].mesh.set_groups(g);
        }
    }

    /// Hidden triangles of every level, run-length coded (for undo).
    pub fn visibility(&self) -> Vec<Vec<u32>> {
        self.levels.par_iter().map(|l| runs_of_flags(&l.mesh.hidden)).collect()
    }

    /// Put back what [`visibility`](Self::visibility) returned; returns the state it replaced.
    pub fn restore_visibility(&mut self, saved: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
        let now = self.visibility();
        for (lvl, runs) in self.levels.iter_mut().zip(&saved) {
            let n = lvl.mesh.triangle_count();
            lvl.mesh.set_hidden(flags_of_runs(runs, n).unwrap_or_default());
        }
        now
    }

    /// Polygroups of every level, run-length coded (for undo).
    pub fn group_runs(&self) -> Vec<GroupRuns> {
        self.levels.par_iter().map(|l| GroupRuns::of(&l.mesh.groups)).collect()
    }

    /// Put back what [`group_runs`](Self::group_runs) returned; returns the state it replaced.
    pub fn restore_groups(&mut self, saved: Vec<GroupRuns>) -> Vec<GroupRuns> {
        let now = self.group_runs();
        for (lvl, runs) in self.levels.iter_mut().zip(&saved) {
            let n = lvl.mesh.triangle_count();
            lvl.mesh.set_groups(runs.expand(n).unwrap_or_default());
        }
        now
    }

    /// A draw-only copy for when this subtool is parked: the topology of the highest level
    /// with at most `max_vertices` vertices, each vertex placed where its chain of even
    /// children lands on the top level, so the shape and large forms of the top level show.
    /// Normals are baked from those positions. Brings every level in sync first.
    pub fn stand_in(&mut self, max_vertices: usize) -> StandIn {
        let top = self.top();
        self.sync_to(top);
        let level = (0..=top)
            .rev()
            .find(|&l| self.levels[l].mesh.vertex_count() <= max_vertices)
            .unwrap_or(0);
        let n = self.levels[level].mesh.vertex_count();
        let mut ids: Vec<u32> = (0..n as u32).collect();
        for l in level + 1..=top {
            let link = self.levels[l].link.as_ref().unwrap();
            let mut even = vec![0u32; self.levels[l - 1].mesh.vertex_count()];
            for (child, st) in link.stencil.iter().enumerate() {
                if st[1] == NONE {
                    even[st[0] as usize] = child as u32;
                }
            }
            ids.par_iter_mut().for_each(|v| *v = even[*v as usize]);
        }
        let top_positions = &self.levels[top].mesh.positions;
        let positions: Vec<Vec3> = ids.par_iter().map(|&v| top_positions[v as usize]).collect();
        let top_colors=&self.levels[top].mesh.colors;
        let colors=if top_colors.is_empty(){Vec::new()}else{ids.par_iter().map(|&v|top_colors[v as usize]).collect()};
        drop(ids);
        let m = &self.levels[level].mesh;
        let normals = (0..n as u32)
            .into_par_iter()
            .map(|v| pack_normal(vertex_normal(&positions, &m.indices, &m.vt_offsets, &m.vt_tris, v)))
            .collect();
        let bounds = positions
            .par_iter()
            .fold(Aabb::empty, |mut b, &p| {
                b.grow(p);
                b
            })
            .reduce(Aabb::empty, |mut a, b| {
                a.grow(b.min);
                a.grow(b.max);
                a
            });
        StandIn {
            positions,
            normals,
            colors,
            // Hidden parts stay hidden while the subtool is parked.
            indices: m.visible_indices().unwrap_or_else(|| m.indices.clone()),
            level,
            bounds,
        }
    }

    /// Map the stack by `m` weighted per vertex by how free it is in `mask`, the mask of
    /// `level` (Transpose with a mask, ZBrush style). Level `level` is mapped exactly
    /// ([`Partial`] per vertex, what the preview shows), so fully masked vertices there do
    /// not move at all. The weights are carried to every other level: below it the base
    /// positions and the offsets are mapped part of the way, so the pose shows at lower
    /// levels too, and `level`'s own offsets take up the difference; above it the offsets
    /// turn with the part they sit on. The inverse map with the same mask undoes it.
    pub fn transform_masked(&mut self, m: &Affine, level: usize, mask: &[u8]) {
        debug_assert!(m.is_translation() || m.translate == Vec3::ZERO, "see Partial");
        let part = Partial::new(m);
        let top = self.top();
        self.sync_to(level);
        // The mask carried to every level, as the levels themselves carry it.
        let mut masks: Vec<Vec<u8>> = vec![Vec::new(); top + 1];
        masks[level] = mask.to_vec();
        for l in (0..level).rev() {
            let n = self.levels[l].mesh.vertex_count();
            masks[l] = carry_down(self.levels[l + 1].link.as_ref().unwrap(), &masks[l + 1], n);
        }
        for l in level + 1..=top {
            masks[l] = carry_up(self.levels[l].link.as_ref().unwrap(), &masks[l - 1], mid_u8);
        }
        let at = |m: &[u8], v: usize| crate::mask::freedom(m, v as u32);
        // The exact target at `level`.
        let ml = &masks[level];
        let target: Vec<Vec3> = self.levels[level]
            .mesh
            .positions
            .par_iter()
            .enumerate()
            .map(|(v, &p)| if ml[v] == 255 { p } else { part.at(at(ml, v)).apply(p) })
            .collect();
        if level > 0 {
            let base = &mut self.levels[0].mesh;
            let m0 = &masks[0];
            base.positions
                .par_iter_mut()
                .enumerate()
                .for_each(|(v, p)| *p = part.at(at(m0, v)).apply(*p));
            let all: Vec<u32> = (0..base.chunks.len() as u32).collect();
            base.finish_edit(&all);
        }
        for (l, lvl) in self.levels.iter_mut().enumerate().skip(1) {
            if l == level {
                continue;
            }
            let ml = &masks[l];
            let chunks = &lvl.mesh.chunks;
            lvl.offsets.par_iter_mut().enumerate().for_each(|(c, off)| {
                let s = chunks[c].vert_start as usize;
                for (i, d) in off.iter_mut().enumerate() {
                    *d = part.at(at(ml, s + i)).apply_vector(*d);
                }
            });
        }
        for lvl in &mut self.levels[..level] {
            lvl.stale.iter_mut().for_each(|s| *s = true);
        }
        if level > 0 {
            // Rebuild the levels below from the mapped base, then fit `level`'s offsets to
            // the target over the moved parent.
            self.sync_to(level - 1);
            let (lower, upper) = self.levels.split_at_mut(level);
            let parent = &lower[level - 1].mesh;
            let lvl = &mut upper[0];
            let link = lvl.link.as_ref().unwrap();
            let chunks = &lvl.mesh.chunks;
            lvl.offsets.par_iter_mut().enumerate().for_each(|(c, off)| {
                let (s, e) = chunks[c].range();
                let new: Vec<Vec3> = (s..e).map(|v| target[v] - smooth(parent, link.stencil[v])).collect();
                // A chunk that was never sculpted and did not move keeps no offsets.
                if !(off.is_empty() && new.iter().all(|d| *d == Vec3::ZERO)) {
                    *off = new;
                }
            });
            lower[level - 1].stale.iter_mut().for_each(|s| *s = false);
        }
        // Only chunks that moved are uploaded again; with most of a model masked (posing)
        // that is a small part of it.
        let lvl = &mut self.levels[level];
        let mesh = &lvl.mesh;
        let changed: Vec<u32> = (0..mesh.chunks.len() as u32)
            .into_par_iter()
            .filter(|&c| {
                let (s, e) = mesh.chunks[c as usize].range();
                mesh.positions[s..e] != target[s..e]
            })
            .collect();
        lvl.mesh.positions = target;
        lvl.mesh.finish_edit(&changed);
        lvl.stale.iter_mut().for_each(|s| *s = true);
        self.sync_to(self.active);
    }

    /// Map every level by `m` (see [`Affine`]). Loop's weights sum to one, so smoothing
    /// commutes with the map: positions map and offsets turn and scale with its linear part,
    /// which keeps the stack exactly consistent, stale levels included.
    pub fn transform(&mut self, m: &Affine) {
        for lvl in &mut self.levels {
            lvl.mesh
                .positions
                .par_iter_mut()
                .for_each(|p| *p = m.apply(*p));
            if !m.is_translation() {
                lvl.offsets
                    .par_iter_mut()
                    .for_each(|o| o.iter_mut().for_each(|d| *d = m.apply_vector(*d)));
            }
            if m.keeps_boxes() {
                lvl.mesh.finish_monotone_edit(|p| m.apply(p));
            } else {
                let all: Vec<u32> = (0..lvl.mesh.chunks.len() as u32).collect();
                lvl.mesh.finish_edit(&all);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::cube_sphere;
    use std::collections::HashSet;

    fn sphere(segments: u32) -> SculptMesh {
        let (p, i) = cube_sphere(segments, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn check_tables(m: &SculptMesh) {
        let n = m.vertex_count();
        // Adjacency and incidence rebuilt by brute force from the triangle list.
        let mut adj: Vec<HashSet<u32>> = vec![HashSet::new(); n];
        let mut vt: Vec<HashSet<u32>> = vec![HashSet::new(); n];
        for (t, tri) in m.indices.chunks_exact(3).enumerate() {
            for k in 0..3 {
                vt[tri[k] as usize].insert(t as u32);
                adj[tri[k] as usize].insert(tri[(k + 1) % 3]);
                adj[tri[k] as usize].insert(tri[(k + 2) % 3]);
            }
        }
        for v in 0..n as u32 {
            let got: HashSet<u32> = m.neighbors(v).iter().copied().collect();
            assert_eq!(got, adj[v as usize], "adjacency of {v}");
            assert!(
                m.neighbors(v).windows(2).all(|w| w[0] < w[1]),
                "adjacency sorted"
            );
            let got: HashSet<u32> = m.incident(v).iter().copied().collect();
            assert_eq!(got, vt[v as usize], "incidence of {v}");
        }
        // Chunk triangle ranges tile the index buffer in order, each holds exactly the
        // triangles whose first vertex the chunk owns, and the ones reaching into another
        // chunk come first.
        let mut next = 0;
        for (ci, ch) in m.chunks.iter().enumerate() {
            assert_eq!(ch.tri_start, next, "triangle ranges are contiguous");
            next = ch.tri_end;
            for t in ch.tris() {
                let tri = &m.indices[3 * t as usize..3 * t as usize + 3];
                assert_eq!(m.vert_chunk[tri[0] as usize], ci as u32);
                let crosses = tri.iter().any(|&v| m.vert_chunk[v as usize] != ci as u32);
                assert_eq!(crosses, t < ch.tri_border, "border triangles first");
            }
            for v in ch.vert_start..ch.vert_end {
                assert_eq!(m.vert_chunk[v as usize], ci as u32);
            }
            assert!((ch.vert_end - ch.vert_start) as usize <= CHUNK_TARGET);
        }
        assert_eq!(next as usize, m.triangle_count(), "every triangle is in a range");
    }

    #[test]
    fn subdivision_is_one_to_four_and_consistent() {
        let base = sphere(12);
        let (child, link) = subdivide(&base);
        assert_eq!(
            child.vertex_count(),
            base.vertex_count() + base.edge_count()
        );
        assert_eq!(child.triangle_count(), 4 * base.triangle_count());
        // Closed surface: Euler characteristic 2 at both levels.
        let euler = |m: &SculptMesh| {
            m.vertex_count() as i64 - m.edge_count() as i64 + m.triangle_count() as i64
        };
        assert_eq!(euler(&base), 2);
        assert_eq!(euler(&child), 2);
        check_tables(&child);
        assert_eq!(link.stencil.len(), child.vertex_count());
        assert_eq!(link.dependants.len(), base.chunks.len());
        // Loop subdivision of a unit sphere stays close to it and shrinks slightly.
        let r: Vec<f32> = child.positions.iter().map(|p| p.length()).collect();
        let mean = r.iter().sum::<f32>() / r.len() as f32;
        assert!(mean < 1.0 && mean > 0.98, "mean radius {mean}");
        assert!(r.iter().all(|&x| x > 0.97 && x <= 1.0001));
        // Outward winding kept: every triangle's normal points away from the origin.
        for tri in child.indices.chunks_exact(3) {
            let (a, b, c) = (
                child.positions[tri[0] as usize],
                child.positions[tri[1] as usize],
                child.positions[tri[2] as usize],
            );
            assert!((b - a).cross(c - a).dot(a) > 0.0);
        }
    }

    #[test]
    fn second_level_nests_in_chunks() {
        let base = sphere(40);
        let (l1, _) = subdivide(&base);
        check_tables(&l1);
        let (l2, _) = subdivide(&l1);
        assert_eq!(l2.vertex_count(), l1.vertex_count() + l1.edge_count());
        check_tables(&l2);
        // Same surface, same bounds.
        let (b0, b2) = (base.bounds(), l2.bounds());
        assert!((b0.max - b2.max).length() < 0.02 && (b0.min - b2.min).length() < 0.02);
    }

    #[test]
    fn divide_respects_budget() {
        let mut mr = Multires::new(sphere(8), 2000);
        let next = mr.next_vertex_count();
        assert!(next > 1000 && next < 2000);
        assert!(mr.divide());
        assert_eq!(mr.count(), 2);
        assert_eq!(mr.active(), 1);
        assert!(!mr.can_divide());
        assert!(!mr.divide());
        assert_eq!(mr.count(), 2);
    }

    /// Move the vertex nearest `p` at the active level by `delta` through the stroke path.
    fn nudge(mr: &mut Multires, p: Vec3, delta: Vec3) -> ChunkData {
        let level = mr.active();
        let mesh = mr.mesh_mut();
        let v = (0..mesh.vertex_count() as u32)
            .min_by(|&a, &b| {
                (mesh.positions[a as usize] - p)
                    .length_squared()
                    .total_cmp(&(mesh.positions[b as usize] - p).length_squared())
            })
            .unwrap();
        let c = mesh.vert_chunk[v as usize];
        let snapshot = mesh.snapshot_chunks(&[c]);
        mesh.positions[v as usize] += delta;
        mesh.finish_edit(&[c]);
        mr.finish_stroke(level, snapshot)
    }

    #[test]
    fn lower_level_edits_propagate_and_detail_survives() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        assert!(mr.divide());
        assert!(mr.divide());
        let top0 = mr.mesh().positions.clone();
        // Detail at the top level.
        let d = Vec3::new(0.0, 0.0, 0.3);
        nudge(&mut mr, Vec3::Z, d);
        let top1 = mr.mesh().positions.clone();
        let moved = top1.iter().zip(&top0).filter(|(a, b)| a != b).count();
        assert_eq!(moved, 1);
        // Edit the base in the same area and come back up.
        mr.set_active(0);
        assert_eq!(mr.active(), 0);
        let base_before = mr.mesh().positions.clone();
        nudge(&mut mr, Vec3::Z, Vec3::new(0.0, 0.0, 0.5));
        mr.set_active(2);
        let top2 = &mr.mesh().positions;
        // The base edit reached the top: the region near +Z moved outwards...
        let near: Vec<usize> = (0..top2.len()).filter(|&i| top0[i].z > 0.9).collect();
        assert!(!near.is_empty());
        assert!(near.iter().any(|&i| top2[i].z > top1[i].z + 0.01));
        // ...the far side did not...
        assert!(
            (0..top2.len())
                .filter(|&i| top0[i].z < 0.0)
                .all(|i| top2[i] == top1[i])
        );
        // ...and the detail offset is still exactly on top of the smoothed surface.
        let lvl2 = &mr.levels[2];
        let link = lvl2.link.as_ref().unwrap();
        let vd = (0..top1.len()).find(|&i| top1[i] != top0[i]).unwrap();
        let smooth_now = smooth(&mr.levels[1].mesh, link.stencil[vd]);
        let c = lvl2.mesh.vert_chunk[vd] as usize;
        let i = vd - lvl2.mesh.chunks[c].vert_start as usize;
        assert!((lvl2.offsets[c][i] - d).length() < 1e-5);
        assert!((top2[vd] - (smooth_now + d)).length() < 1e-5);
        // Stepping down shows the lower level untouched by the top-level detail.
        mr.set_active(0);
        let base_now = &mr.mesh().positions;
        let changed = base_now
            .iter()
            .zip(&base_before)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 1);
    }

    /// Every level of `b` matches `a`: same topology, chunks and tables, positions within
    /// float noise, same offsets and labels.
    fn assert_same_stack(a: &Multires, b: &Multires) {
        assert_eq!(a.count(), b.count());
        assert_eq!(a.active(), b.active());
        for (la, lb) in a.levels.iter().zip(&b.levels) {
            let (ma, mb) = (&la.mesh, &lb.mesh);
            assert_eq!(ma.indices, mb.indices, "index buffer");
            assert_eq!(ma.chunk_ranges(), mb.chunk_ranges(), "chunk ranges");
            assert_eq!(ma.adj_offsets, mb.adj_offsets);
            assert_eq!(ma.adj_verts, mb.adj_verts);
            assert_eq!(ma.vt_tris, mb.vt_tris);
            let worst = ma
                .positions
                .iter()
                .zip(&mb.positions)
                .map(|(p, q)| (*p - *q).length())
                .fold(0.0f32, f32::max);
            assert!(worst < 1e-5, "positions differ by {worst}");
            assert_eq!(la.offsets.len(), lb.offsets.len());
            for (oa, ob) in la.offsets.iter().zip(&lb.offsets) {
                assert_eq!(oa, ob, "offsets are carried over exactly");
            }
            match (&la.link, &lb.link) {
                (Some(x), Some(y)) => {
                    assert_eq!(x.labels, y.labels);
                    assert_eq!(x.stencil, y.stencil);
                    assert_eq!(x.dependants, y.dependants);
                }
                (None, None) => {}
                _ => panic!("link mismatch"),
            }
        }
    }

    #[test]
    fn pack_and_unpack_rebuild_the_exact_stack() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        nudge(&mut mr, Vec3::X, Vec3::X * 0.2);
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Y, Vec3::Y * 0.1);
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.05);
        // A base edit after dividing: the upper levels are now stale, and a re-divide would
        // split chunks differently from the positions they were made from.
        mr.set_active(0);
        nudge(&mut mr, -Vec3::X, -Vec3::X * 0.3);
        mr.set_active(1);
        let mut reference = mr.pack_copy().unpack();
        reference.sync_to(reference.top());
        let packed = mr.pack();
        assert!(packed.bytes() > 0);
        let mut back = packed.unpack();
        back.sync_to(back.top());
        assert_eq!(back.active(), 1);
        assert_same_stack(&reference, &back);
        // And against a stack that was never packed.
        let mut live = Multires::new(sphere(10), usize::MAX);
        nudge(&mut live, Vec3::X, Vec3::X * 0.2);
        assert!(live.divide());
        nudge(&mut live, Vec3::Y, Vec3::Y * 0.1);
        assert!(live.divide());
        nudge(&mut live, Vec3::Z, Vec3::Z * 0.05);
        live.set_active(0);
        nudge(&mut live, -Vec3::X, -Vec3::X * 0.3);
        live.set_active(1);
        live.sync_to(live.top());
        assert_same_stack(&live, &back);
        for l in 0..back.count() {
            check_tables(&back.levels[l].mesh);
        }
    }

    #[test]
    fn packed_form_is_much_smaller_than_the_stack() {
        let mut mr = Multires::new(sphere(24), usize::MAX);
        assert!(mr.divide());
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.05);
        let full = mr.bytes();
        let packed = mr.pack().bytes();
        assert!(packed * 10 < full, "packed {packed} vs full {full}");
    }

    #[test]
    fn stand_in_samples_the_top_level() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        assert!(mr.divide());
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.4);
        let base_n = mr.levels[0].mesh.vertex_count();
        let si = mr.stand_in(base_n);
        assert_eq!(si.level, 0);
        assert_eq!(si.positions.len(), base_n);
        assert_eq!(si.indices, mr.levels[0].mesh.indices);
        // Every stand-in vertex is a vertex of the top level.
        let top: HashSet<[u32; 3]> = mr.levels[2]
            .mesh
            .positions
            .iter()
            .map(|p| p.to_array().map(f32::to_bits))
            .collect();
        assert!(si.positions.iter().all(|p| top.contains(&p.to_array().map(f32::to_bits))));
        // A budget that fits the top level gives the top level itself.
        let all = mr.stand_in(usize::MAX);
        assert_eq!(all.level, 2);
        assert_eq!(all.positions, mr.levels[2].mesh.positions);
    }

    #[test]
    fn transform_keeps_the_stack_consistent() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        assert!(mr.divide());
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.2);
        mr.set_active(1);
        nudge(&mut mr, Vec3::Y, Vec3::Y * 0.15);
        mr.set_active(0);
        nudge(&mut mr, Vec3::X, Vec3::X * 0.2);
        let worst = |a: &[Vec3], b: &[Vec3]| {
            a.iter().zip(b).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max)
        };
        let maps = [
            Affine::scaling(Vec3::new(0.1, 0.2, 0.3), 1.7),
            Affine::translation(Vec3::new(-0.5, 0.25, 2.0)),
            Affine {
                center: Vec3::new(0.1, -0.3, 0.2),
                rotation: glam::Quat::from_axis_angle(Vec3::new(0.3, 1.0, -0.2).normalize(), 1.1),
                scale: 0.6,
                translate: Vec3::new(0.4, 0.0, -1.5),
                ..Affine::IDENTITY
            },
            Affine::stretching(Vec3::new(0.1, 0.0, 0.2), Vec3::new(1.0, 0.5, 0.2), 0.3),
            Affine::stretching(Vec3::ZERO, Vec3::Y, 2.5),
        ];
        for m in maps {
            // Level 2 is stale (edits at levels 0 and 1 are not synced up yet): the map has
            // to commute with the lazy re-smoothing.
            let mut expect = mr.pack_copy().unpack();
            expect.sync_to(2);
            let before: Vec<Vec3> = expect.levels[2].mesh.positions.clone();
            let want: Vec<Vec3> = before.iter().map(|&p| m.apply(p)).collect();
            mr.transform(&m);
            mr.set_active(2);
            assert!(worst(&mr.mesh().positions, &want) < 1e-4, "{m:?}");
            // Every level matches a fresh subdivision of the mapped base plus mapped offsets.
            let mut fresh = mr.pack_copy().unpack();
            fresh.sync_to(2);
            for l in 0..3 {
                let a = &mr.level_mesh(l).positions;
                let b = &fresh.level_mesh(l).positions;
                assert!(worst(a, b) < 1e-4, "level {l}");
            }
            // Chunk boxes match a refit over the vertices (the scale and move skip it).
            for l in 0..3 {
                let mesh = mr.level_mesh(l);
                for ch in &mesh.chunks {
                    let mut b = crate::mesh::Aabb::empty();
                    for v in ch.vert_start..ch.vert_end {
                        b.grow(mesh.positions[v as usize]);
                    }
                    assert!(ch.aabb.min.cmple(b.min).all() && ch.aabb.max.cmpge(b.max).all());
                }
            }
            // The exact inverse brings it back.
            mr.transform(&m.inverse());
            assert!(worst(&mr.mesh().positions, &before) < 1e-4, "inverse of {m:?}");
            mr.set_active(0);
        }
    }

    #[test]
    fn the_mask_follows_the_shown_level_and_survives_packing() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        assert!(mr.divide());
        assert!(mr.divide());
        mr.set_active(0);
        let half: Vec<u8> = mr.mesh().positions.iter().map(|p| if p.x > 0.0 { 255 } else { 0 }).collect();
        crate::mask::replace(mr.mesh_mut(), half.clone());
        mr.set_active(2);
        assert!(mr.level_mesh(0).mask.is_empty(), "only the shown level carries it");
        let top = mr.mesh();
        assert_eq!(top.mask.len(), top.vertex_count());
        for (p, &m) in top.positions.iter().zip(&top.mask) {
            if p.x > 0.15 {
                assert_eq!(m, 255);
            } else if p.x < -0.15 {
                assert_eq!(m, 0);
            }
        }
        // Down again: even vertices carry their parent's value, so it comes back exactly.
        mr.set_active(0);
        assert_eq!(mr.mesh().mask, half);
        // Packing keeps it with the shown level.
        mr.set_active(1);
        let shown = mr.mesh().mask.clone();
        let back = mr.pack_copy().unpack();
        assert_eq!(back.active(), 1);
        assert_eq!(back.mesh().mask, shown);
        // Undoing a divide while its level is shown carries the mask down.
        mr.set_active(2);
        mr.pop_level();
        assert_eq!(mr.active(), 1);
        assert_eq!(mr.mesh().mask, shown);
    }

    #[test]
    fn masked_transform_holds_the_mask_and_undoes() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        assert!(mr.divide());
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.2);
        assert!(mr.divide());
        let before = mr.mesh().positions.clone();
        let mask: Vec<u8> = before.iter().map(|p| if p.x < 0.0 { 255 } else { 0 }).collect();
        crate::mask::replace(mr.mesh_mut(), mask.clone());
        let m = Affine::rotation_about(Vec3::ZERO, glam::Quat::from_rotation_y(0.7));
        mr.transform_masked(&m, 2, &mask);
        let after = mr.mesh().positions.clone();
        for (p, q) in before.iter().zip(&after) {
            if p.x < 0.0 {
                assert_eq!(p, q, "masked vertices stay exactly");
            } else {
                assert!((m.apply(*p) - *q).length() < 1e-5, "free vertices turn with the map");
            }
        }
        // Every level moved, not only the shown one.
        mr.set_active(0);
        assert!(mr.mesh().positions.iter().any(|p| p.x > 0.3));
        mr.set_active(2);
        mr.transform_masked(&m.inverse(), 2, &mask);
        let worst = mr.mesh().positions.iter().zip(&before).map(|(p, q)| (*p - *q).length()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn undo_works_at_every_level() {
        let mut mr = Multires::new(sphere(10), usize::MAX);
        let base0 = mr.mesh().positions.clone();
        let undo_a = nudge(&mut mr, Vec3::X, Vec3::X * 0.2);
        assert!(mr.divide());
        let l1_after_a = mr.mesh().positions.clone();
        let undo_b = nudge(&mut mr, Vec3::X, Vec3::X * 0.1);
        assert_ne!(mr.mesh().positions, l1_after_a);
        // Undo the level-1 stroke: offsets return to zero, so the level is the plain
        // subdivision of the (still edited) base.
        let redo_b = mr.restore(1, undo_b);
        assert_eq!(mr.mesh().positions, l1_after_a);
        // Undo the level-0 stroke while level 1 is displayed: the display follows.
        let redo_a = mr.restore(0, undo_a);
        assert_eq!(mr.level_mesh(0).positions, base0);
        let (fresh, _) = subdivide(mr.level_mesh(0));
        assert_eq!(mr.mesh().positions, fresh.positions);
        // Redo both.
        mr.restore(0, redo_a);
        assert_eq!(mr.mesh().positions, l1_after_a);
        mr.restore(1, redo_b);
        assert_ne!(mr.mesh().positions, l1_after_a);
        // Undo / redo of the divide itself.
        let saved = mr.pop_level().unwrap();
        assert_eq!(mr.count(), 1);
        assert_eq!(mr.active(), 0);
        let n = saved.mesh.vertex_count();
        mr.push_level(saved);
        assert_eq!(mr.count(), 2);
        assert_eq!(mr.active(), 1);
        assert_eq!(mr.mesh().vertex_count(), n);
    }

    #[test]
    fn every_child_triangle_finds_its_parent() {
        let mut mr = Multires::new(sphere(6), usize::MAX);
        assert!(mr.divide() && mr.divide());
        for l in 1..3 {
            let parents = mr.tri_parents(l);
            let parent = &mr.levels[l - 1].mesh;
            let child = &mr.levels[l].mesh;
            let st = &mr.levels[l].link.as_ref().unwrap().stencil;
            let mut kids = vec![(0u32, 0u32); parent.triangle_count()];
            for (c, &p) in parents.iter().enumerate() {
                let t = (p & !CENTRE) as usize;
                kids[t].0 += 1;
                kids[t].1 += (p & CENTRE != 0) as u32;
                // Every vertex of the child comes from the parent triangle's corners.
                let pt = &parent.indices[3 * t..3 * t + 3];
                for &v in &child.indices[3 * c..3 * c + 3] {
                    let s = st[v as usize];
                    assert!(pt.contains(&s[0]));
                    if s[1] != NONE {
                        assert!(pt.contains(&s[1]));
                    }
                }
            }
            assert!(kids.iter().all(|&k| k == (4, 1)), "four children, one of them the centre");
        }
    }

    #[test]
    fn groups_and_visibility_carry_across_levels() {
        let mut mr = Multires::new(sphere(8), usize::MAX);
        // Groups and hidden triangles made at the base ride along when dividing.
        let n0 = mr.mesh().triangle_count();
        let base_groups: Vec<u16> = (0..n0).map(|t| (t % 5) as u16).collect();
        mr.set_groups(0, base_groups.clone());
        let base_hidden: Vec<u8> = (0..n0).map(|t| (t % 7 == 0) as u8).collect();
        mr.set_hidden(0, base_hidden.clone());
        assert!(mr.divide() && mr.divide());
        for l in 1..3 {
            let parents = mr.tri_parents(l);
            let (p, c) = (&mr.levels[l - 1].mesh, &mr.levels[l].mesh);
            for (t, &pt) in parents.iter().enumerate() {
                let pt = (pt & !CENTRE) as usize;
                assert_eq!(c.groups[t], p.groups[pt]);
                assert_eq!(c.hidden[t], p.hidden[pt]);
            }
            assert_eq!(c.group_count(), 5);
            assert_eq!(c.hidden_count(), p.hidden_count() * 4);
        }
        // Vertices of hidden triangles only are not shown; the others are.
        let top = &mr.levels[2].mesh;
        for v in 0..top.vertex_count() {
            let any_visible = top.incident(v as u32).iter().any(|&t| top.hidden[t as usize] == 0);
            assert_eq!(top.shown[v] != 0, any_visible);
        }

        // A change at the middle level: exact above, approximate below.
        let before = mr.visibility();
        let n1 = mr.levels[1].mesh.triangle_count();
        let mid: Vec<u8> = (0..n1).map(|t| (t < n1 / 2) as u8).collect();
        mr.set_hidden(1, mid.clone());
        assert_eq!(mr.levels[1].mesh.hidden, mid);
        let parents = mr.tri_parents(2);
        for (t, &p) in parents.iter().enumerate() {
            assert_eq!(mr.levels[2].mesh.hidden[t], mid[(p & !CENTRE) as usize]);
        }
        let parents = mr.tri_parents(1);
        let mut all = vec![true; n0];
        for (t, &p) in parents.iter().enumerate() {
            all[(p & !CENTRE) as usize] &= mid[t] != 0;
        }
        let want: Vec<u8> = all.iter().map(|&a| a as u8).collect();
        assert_eq!(mr.levels[0].mesh.hidden, want);
        // Undo puts every level back exactly, including the lossy one below.
        let after = mr.restore_visibility(before);
        assert_eq!(mr.levels[0].mesh.hidden, base_hidden);
        mr.restore_visibility(after);
        assert_eq!(mr.levels[1].mesh.hidden, mid);
        // Showing everything empties the arrays.
        mr.set_hidden(2, Vec::new());
        assert!(mr.levels.iter().all(|l| l.mesh.hidden.is_empty() && l.mesh.shown.is_empty()));

        // Groups changed at the top: the base takes the centre child's group.
        let n2 = mr.levels[2].mesh.triangle_count();
        let top_groups: Vec<u16> = (0..n2).map(|t| (t / 100) as u16).collect();
        let saved = mr.group_runs();
        mr.set_groups(2, top_groups.clone());
        let parents = mr.tri_parents(2);
        for (t, &p) in parents.iter().enumerate() {
            if p & CENTRE != 0 {
                assert_eq!(mr.levels[1].mesh.groups[(p & !CENTRE) as usize], top_groups[t]);
            }
        }
        mr.restore_groups(saved);
        assert_eq!(mr.levels[0].mesh.groups, base_groups);
    }

    #[test]
    fn packing_keeps_groups_and_visibility_of_every_level() {
        let mut mr = Multires::new(sphere(8), usize::MAX);
        assert!(mr.divide());
        let n1 = mr.mesh().triangle_count();
        // Made at level 1, so level 1 differs from what level 0 alone would give it.
        mr.set_groups(1, (0..n1).map(|t| (t % 3) as u16).collect());
        mr.set_hidden(1, (0..n1).map(|t| (t % 11 == 0) as u8).collect());
        let want = mr.marks();
        assert!(!want[1].groups.lens.is_empty() && !want[1].hidden.is_empty());
        let back = mr.pack_copy().unpack();
        assert_eq!(back.marks(), want);
        assert_eq!(back.levels[1].mesh.shown, mr.levels[1].mesh.shown);
        // The stand-in leaves hidden triangles out.
        let si = mr.stand_in(usize::MAX);
        assert_eq!(si.indices.len() / 3, n1 - mr.mesh().hidden_count());
    }
}
