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
use glam::Vec3;
use rayon::prelude::*;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

const NONE: u32 = u32::MAX;

/// How a child vertex is smoothed from its parent level. `[p, NONE, _, _]` is the even
/// vertex above parent vertex `p` (its ring comes from the parent's adjacency); `[a, b, c, d]`
/// is the odd vertex on parent edge `a-b` with opposite vertices `c` and `d` (`d == NONE` on a
/// boundary edge).
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
        mesh + link + offsets
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

    // 2. Odd stencils in edge order, filled per parent chunk.
    let mut odd = vec![[NONE; 4]; e_count];
    {
        let ranges = pc.iter().map(|ch| {
            (
                edge_base[ch.vert_start as usize] as usize,
                edge_base[ch.vert_end as usize] as usize,
            )
        });
        split_ranges(&mut odd, ranges)
            .into_par_iter()
            .zip(pc.par_iter())
            .for_each(|(slice, ch)| {
                let mut k = 0;
                for v in ch.vert_start..ch.vert_end {
                    for &w in parent.neighbors(v) {
                        if w <= v {
                            continue;
                        }
                        let mut s = [v, w, NONE, NONE];
                        for (i, (_, _, _, o)) in edge_tris(parent, v, w).enumerate() {
                            s[2 + i] = o;
                        }
                        slice[k] = s;
                        k += 1;
                    }
                }
            });
    }

    // 3. Smoothed positions in natural order.
    let mut pos_nat = vec![Vec3::ZERO; n_count];
    {
        let (even, odd_pos) = pos_nat.split_at_mut(v_count);
        even.par_iter_mut()
            .enumerate()
            .for_each(|(v, p)| *p = smooth(parent, [v as u32, NONE, 0, 0]));
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
                        [nat, NONE, 0, 0]
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
    let borders: Vec<u32> = {
        let tri_ranges = chunk_tri
            .windows(2)
            .map(|w| (3 * w[0] as usize, 3 * w[1] as usize));
        split_ranges(&mut indices, tri_ranges)
            .into_par_iter()
            .zip(owned.par_iter())
            .zip(ranges().collect::<Vec<_>>().into_par_iter())
            .zip(chunk_tri.par_iter())
            .map(|(((out, o), (s, e)), &start)| {
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

    let mesh = SculptMesh::assemble(
        positions,
        indices,
        chunks,
        vert_chunk,
        adj_offsets,
        adj_verts,
        vt_offsets,
        vt_tris,
    );
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
}

#[derive(Clone)]
struct PackedLevel {
    labels: Vec<u8>,
    offsets: Vec<Vec<Vec3>>,
}

impl Packed {
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
        self.positions.len() * 12 + self.indices.len() * 4 + self.ranges.len() * 20 + levels
    }

    /// Vertex count per level, lowest first.
    pub fn counts(&self) -> &[usize] {
        &self.counts
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// Serialise in the project file layout (see [`crate::project`]). Offsets are written as
    /// three blocks (chunk ids, lengths, data) rather than one record per chunk.
    pub fn write_to<W: std::io::Write>(&self, out: &mut crate::project::Out<W>) -> std::io::Result<()> {
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
        Ok(())
    }

    /// Read what [`write_to`](Self::write_to) wrote. Sizes are checked here; topology in
    /// [`try_unpack`](Self::try_unpack).
    pub fn read_from<R: std::io::Read>(input: &mut crate::project::In<R>) -> Result<Self, crate::project::Fail> {
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
        Ok(Packed {
            positions,
            indices,
            ranges,
            levels,
            active,
            budget,
            counts,
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
        Ok(Multires {
            levels,
            active,
            budget,
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
}

impl Multires {
    pub fn new(mesh: SculptMesh, budget: usize) -> Self {
        Self {
            levels: vec![Level::base(mesh)],
            active: 0,
            budget,
        }
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

    #[cfg(test)]
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
        let (mesh, link) = subdivide(&self.levels[top].mesh);
        let n = mesh.chunks.len();
        self.levels.push(Level {
            mesh,
            link: Some(link),
            offsets: vec![Vec::new(); n],
            stale: vec![false; n],
        });
        self.levels[top].stale.iter_mut().for_each(|s| *s = false);
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
        self.active = level;
        synced
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
    pub fn pack(self) -> Packed {
        let counts = self.vertex_counts();
        let Multires {
            levels,
            active,
            budget,
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
        }
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
            indices: m.indices.clone(),
            level,
            bounds,
        }
    }

    /// Move and scale every level: `p -> c + s (p - c) + t`. Loop's weights sum to one, so
    /// smoothing commutes with this map and the offsets only scale by `s`; the stack stays
    /// exactly consistent, stale levels included.
    pub fn transform(&mut self, c: Vec3, s: f32, t: Vec3) {
        for lvl in &mut self.levels {
            lvl.mesh
                .positions
                .par_iter_mut()
                .for_each(|p| *p = c + (*p - c) * s + t);
            if s != 1.0 {
                lvl.offsets
                    .par_iter_mut()
                    .for_each(|o| o.iter_mut().for_each(|d| *d *= s));
            }
            let all: Vec<u32> = (0..lvl.mesh.chunks.len() as u32).collect();
            lvl.mesh.finish_edit(&all);
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
        nudge(&mut mr, Vec3::Z, Vec3::Z * 0.2);
        mr.set_active(0);
        nudge(&mut mr, Vec3::X, Vec3::X * 0.2);
        let (c, s, t) = (Vec3::new(0.1, 0.2, 0.3), 1.7, Vec3::new(-0.5, 0.25, 2.0));
        let mut expect = mr.pack_copy().unpack();
        expect.sync_to(1);
        let want: Vec<Vec3> = expect.levels[1]
            .mesh
            .positions
            .iter()
            .map(|&p| c + (p - c) * s + t)
            .collect();
        mr.transform(c, s, t);
        mr.set_active(1);
        let worst = mr
            .mesh()
            .positions
            .iter()
            .zip(&want)
            .map(|(p, q)| (*p - *q).length())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
        // The exact inverse brings it back.
        mr.transform(c + t, 1.0 / s, -t);
        let back: Vec<Vec3> = expect.levels[1].mesh.positions.clone();
        let worst = mr
            .mesh()
            .positions
            .iter()
            .zip(&back)
            .map(|(p, q)| (*p - *q).length())
            .fold(0.0f32, f32::max);
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
}
