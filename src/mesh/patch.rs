//! A mesh rebuilt after a local change (Sculptris Pro): only the chunks the change touches
//! are put together again; every other chunk is copied with its vertex and triangle
//! numbers shifted, its tables mapped rather than rebuilt. The result is the same mesh
//! [`SculptMesh::from_assigned`] would make from the same vertices in the same chunks, at
//! the cost of a few copies instead of a rebuild.

use super::*;
use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};

/// A vertex of a triangle in a [`Patch`]: one the mesh has, or the n-th new one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VRef {
    Old(u32),
    New(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct NewVertex {
    pub pos: Vec3,
    /// The chunk it joins (its parent's).
    pub chunk: u32,
    pub mask: u8,
    pub color: [u8; 4],
}

/// A local change: vertices moved, removed and added, triangles removed and added. Every
/// vertex that moves, goes or is used by a new triangle must be a vertex of a removed
/// triangle, and a removed vertex may be used by no triangle that stays.
#[derive(Default)]
pub struct Patch {
    pub moved: Vec<(u32, Vec3)>,
    pub dead: Vec<u32>,
    pub new_verts: Vec<NewVertex>,
    pub removed_tris: Vec<u32>,
    pub new_tris: Vec<([VRef; 3], u16)>,
}

/// A chunk of the new mesh: an old chunk copied, or one put together from these vertices.
enum Piece {
    Copy(u32),
    Built(Vec<VRef>),
}

/// The triangles of one built piece, in new numbers, before border-first ordering.
#[derive(Default)]
struct PieceTris {
    tris: Vec<[u32; 3]>,
    groups: Vec<u16>,
    hidden: Vec<u8>,
    /// Old triangle number, or `u32::MAX` with the new triangle's number in `new_id`.
    old: Vec<u32>,
    new_id: Vec<u32>,
}

/// The parts one after another, copied in parallel.
fn concat<T: Copy + Send + Sync>(parts: &[Vec<T>]) -> Vec<T> {
    let total: usize = parts.iter().map(Vec::len).sum();
    let mut out: Vec<T> = Vec::with_capacity(total);
    {
        let mut rest = &mut out.spare_capacity_mut()[..total];
        let mut slices = Vec::with_capacity(parts.len());
        for part in parts {
            let (a, b) = rest.split_at_mut(part.len());
            slices.push(a);
            rest = b;
        }
        slices.into_par_iter().zip(parts.par_iter()).for_each(|(dst, src)| {
            for (d, s) in dst.iter_mut().zip(src) {
                d.write(*s);
            }
        });
    }
    // SAFETY: every one of the `total` slots was written above.
    unsafe { out.set_len(total) };
    out
}

/// CSR offsets from per-piece counts: 0, then the running sum, in parallel by piece.
fn offsets(counts: &[Vec<u32>]) -> Vec<u32> {
    let sums: Vec<u32> = counts.par_iter().map(|c| c.iter().sum()).collect();
    let mut base = Vec::with_capacity(counts.len());
    let mut acc = 0u32;
    for s in &sums {
        base.push(acc);
        acc += s;
    }
    let parts: Vec<Vec<u32>> = counts
        .par_iter()
        .zip(base.par_iter())
        .map(|(c, &b)| {
            let mut run = b;
            c.iter()
                .map(|&k| {
                    run += k;
                    run
                })
                .collect()
        })
        .collect();
    let mut out = Vec::with_capacity(1 + parts.iter().map(Vec::len).sum::<usize>());
    out.push(0);
    out.extend(concat(&parts));
    out
}

impl SculptMesh {
    /// This mesh with `p` applied.
    #[cfg(test)]
    pub fn patched(&self, p: &Patch) -> SculptMesh {
        self.patched_tracked(p, &[]).0
    }

    /// [`patched`](Self::patched), also saying where things came from: for each new chunk
    /// the old chunk it is a copy of (`None` for rebuilt ones), and `carry` (a value per old
    /// vertex, empty for none) carried to the new vertices (`u32::MAX` for new ones).
    pub fn patched_tracked(&self, p: &Patch, carry: &[u32]) -> (SculptMesh, Vec<Option<u32>>, Vec<u32>) {
        let carrying = !carry.is_empty();
        let nc = self.chunks.len();
        let removed: HashSet<u32> = p.removed_tris.iter().copied().collect();
        let dead: HashSet<u32> = p.dead.iter().copied().collect();
        let moved: HashMap<u32, Vec3> = p.moved.iter().copied().collect();
        // Old vertices whose triangles change.
        let mut touched: HashSet<u32> = HashSet::default();
        let mut affected = vec![false; nc];
        for &t in &p.removed_tris {
            for &v in &self.indices[3 * t as usize..3 * t as usize + 3] {
                touched.insert(v);
                affected[self.vert_chunk[v as usize] as usize] = true;
            }
        }
        let mut new_in: HashMap<u32, Vec<u32>> = HashMap::default();
        for (i, nv) in p.new_verts.iter().enumerate() {
            affected[nv.chunk as usize] = true;
            new_in.entry(nv.chunk).or_default().push(i as u32);
        }
        let pos_of = |r: VRef| match r {
            VRef::Old(v) => moved.get(&v).copied().unwrap_or(self.positions[v as usize]),
            VRef::New(i) => p.new_verts[i as usize].pos,
        };

        // The new chunks, in the old order; a grown chunk is split, an emptied one goes.
        let mut pieces: Vec<Piece> = Vec::with_capacity(nc + 8);
        let mut piece_of = vec![u32::MAX; nc];
        let mut built_of: HashMap<u32, (usize, usize)> = HashMap::default();
        for c in 0..nc {
            if !affected[c] {
                piece_of[c] = pieces.len() as u32;
                pieces.push(Piece::Copy(c as u32));
                continue;
            }
            let ch = &self.chunks[c];
            let mut list: Vec<VRef> = (ch.vert_start..ch.vert_end).filter(|v| !dead.contains(v)).map(VRef::Old).collect();
            if let Some(news) = new_in.get(&(c as u32)) {
                list.extend(news.iter().map(|&i| VRef::New(i)));
            }
            let first = pieces.len();
            if list.len() > CHUNK_TARGET {
                let pts: Vec<Vec3> = list.iter().map(|&r| pos_of(r)).collect();
                for g in partition_ids((0..list.len() as u32).collect(), &pts) {
                    pieces.push(Piece::Built(g.into_iter().map(|i| list[i as usize]).collect()));
                }
            } else if !list.is_empty() {
                pieces.push(Piece::Built(list));
            }
            built_of.insert(c as u32, (first, pieces.len()));
        }
        let np = pieces.len();
        let mut vstart = Vec::with_capacity(np + 1);
        vstart.push(0u32);
        for piece in &pieces {
            let len = match piece {
                Piece::Copy(c) => self.chunks[*c as usize].vert_end - self.chunks[*c as usize].vert_start,
                Piece::Built(l) => l.len() as u32,
            };
            vstart.push(vstart.last().unwrap() + len);
        }
        // Where each affected chunk's old vertices went, densely: `aoff[c]` + offset in c.
        let mut aoff = vec![u32::MAX; nc];
        let mut asize = 0u32;
        for c in 0..nc {
            if affected[c] {
                aoff[c] = asize;
                asize += self.chunks[c].vert_end - self.chunks[c].vert_start;
            }
        }
        let mut amap = vec![u32::MAX; asize as usize];
        let mut nmap = vec![u32::MAX; p.new_verts.len()];
        for (i, piece) in pieces.iter().enumerate() {
            if let Piece::Built(list) = piece {
                for (k, r) in list.iter().enumerate() {
                    let id = vstart[i] + k as u32;
                    match *r {
                        VRef::Old(v) => {
                            let c = self.vert_chunk[v as usize] as usize;
                            amap[(aoff[c] + v - self.chunks[c].vert_start) as usize] = id;
                        }
                        VRef::New(j) => nmap[j as usize] = id,
                    }
                }
            }
        }
        // Old chunk → (old first vertex, new first vertex), or the dense table for an
        // affected one.
        let vshift: Vec<(u32, u32)> = (0..nc)
            .map(|c| if affected[c] { (self.chunks[c].vert_start, u32::MAX) } else { (self.chunks[c].vert_start, vstart[piece_of[c] as usize]) })
            .collect();
        let vmap = |v: u32| -> u32 {
            let c = self.vert_chunk[v as usize] as usize;
            let (old, new) = vshift[c];
            if new != u32::MAX { v - old + new } else { amap[(aoff[c] + v - old) as usize] }
        };
        let rmap = |r: VRef| match r {
            VRef::Old(v) => vmap(v),
            VRef::New(j) => nmap[j as usize],
        };
        let piece_at = |id: u32| vstart.partition_point(|&s| s <= id) - 1;

        // Per-vertex arrays.
        let masked = !self.mask.is_empty();
        let painted = !self.colors.is_empty();
        let carried: Vec<Vec<u32>> = if carrying {
            pieces
                .par_iter()
                .map(|piece| match piece {
                    Piece::Copy(c) => {
                        let (s, e) = self.chunks[*c as usize].range();
                        carry[s..e].to_vec()
                    }
                    Piece::Built(list) => list
                        .iter()
                        .map(|&r| match r {
                            VRef::Old(v) => carry[v as usize],
                            VRef::New(_) => u32::MAX,
                        })
                        .collect(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let carried = concat(&carried);
        let origins: Vec<Option<u32>> = pieces
            .iter()
            .map(|piece| match piece {
                Piece::Copy(c) => Some(*c),
                Piece::Built(_) => None,
            })
            .collect();
        let per_piece: Vec<(Vec<Vec3>, Vec<u8>, Vec<[u8; 4]>)> = pieces
            .par_iter()
            .map(|piece| match piece {
                Piece::Copy(c) => {
                    let (s, e) = self.chunks[*c as usize].range();
                    (
                        self.positions[s..e].to_vec(),
                        if masked { self.mask[s..e].to_vec() } else { Vec::new() },
                        if painted { self.colors[s..e].to_vec() } else { Vec::new() },
                    )
                }
                Piece::Built(list) => {
                    let pos = list.iter().map(|&r| pos_of(r)).collect();
                    let attr = |r: VRef| match r {
                        VRef::Old(v) => (self.mask.get(v as usize).copied().unwrap_or(0), self.colors.get(v as usize).copied().unwrap_or([0; 4])),
                        VRef::New(i) => (p.new_verts[i as usize].mask, p.new_verts[i as usize].color),
                    };
                    (
                        pos,
                        if masked { list.iter().map(|&r| attr(r).0).collect() } else { Vec::new() },
                        if painted { list.iter().map(|&r| attr(r).1).collect() } else { Vec::new() },
                    )
                }
            })
            .collect();
        let (pp, rest): (Vec<Vec<Vec3>>, Vec<(Vec<u8>, Vec<[u8; 4]>)>) = per_piece.into_iter().map(|(a, b, c)| (a, (b, c))).unzip();
        let (mm, cc): (Vec<Vec<u8>>, Vec<Vec<[u8; 4]>>) = rest.into_iter().unzip();
        let positions = concat(&pp);
        let mask = concat(&mm);
        let colors = concat(&cc);
        let vc: Vec<Vec<u32>> = pp.par_iter().enumerate().map(|(i, part)| vec![i as u32; part.len()]).collect();
        let vert_chunk = concat(&vc);

        // Triangles of the built pieces: what their old chunks kept plus the new ones,
        // each owned by the piece of its first vertex.
        let grouped = !self.groups.is_empty() || p.new_tris.iter().any(|t| t.1 != 0);
        let hidden_any = !self.hidden.is_empty();
        let mut new_by_chunk: HashMap<u32, Vec<u32>> = HashMap::default();
        for (j, (tri, _)) in p.new_tris.iter().enumerate() {
            let c = match tri[0] {
                VRef::Old(v) => self.vert_chunk[v as usize],
                VRef::New(i) => p.new_verts[i as usize].chunk,
            };
            new_by_chunk.entry(c).or_default().push(j as u32);
        }
        let mut built: HashMap<usize, PieceTris> = HashMap::default();
        for (&c, &(first, last)) in &built_of {
            for i in first..last {
                built.insert(i, PieceTris::default());
            }
            let mut put = |tri: [u32; 3], group: u16, hidden: u8, old: u32, new_id: u32| {
                let i = piece_at(tri[0]);
                if let Some(pt) = built.get_mut(&i) {
                    pt.tris.push(tri);
                    pt.groups.push(group);
                    pt.hidden.push(hidden);
                    pt.old.push(old);
                    pt.new_id.push(new_id);
                }
            };
            let ch = &self.chunks[c as usize];
            for t in ch.tris() {
                if removed.contains(&t) {
                    continue;
                }
                let tri = [0, 1, 2].map(|k| vmap(self.indices[3 * t as usize + k]));
                put(tri, self.group(t), self.hidden.get(t as usize).copied().unwrap_or(0), t, 0);
            }
            if let Some(js) = new_by_chunk.get(&c) {
                for &j in js {
                    let (tri, g) = p.new_tris[j as usize];
                    put(tri.map(rmap), g, 0, u32::MAX, j);
                }
            }
        }
        // Border triangles first in each built piece.
        for (&i, pt) in built.iter_mut() {
            let (s, e) = (vstart[i], vstart[i + 1]);
            let crosses: Vec<bool> = pt.tris.iter().map(|t| crosses_chunk(t, s, e)).collect();
            let order: Vec<usize> = (0..pt.tris.len()).filter(|&k| crosses[k]).chain((0..pt.tris.len()).filter(|&k| !crosses[k])).collect();
            fn take<T: Copy>(v: &[T], order: &[usize]) -> Vec<T> {
                order.iter().map(|&k| v[k]).collect()
            }
            *pt = PieceTris {
                tris: take(&pt.tris, &order),
                groups: take(&pt.groups, &order),
                hidden: take(&pt.hidden, &order),
                old: take(&pt.old, &order),
                new_id: take(&pt.new_id, &order),
            };
        }
        let mut tstart = Vec::with_capacity(np + 1);
        let mut tborder = Vec::with_capacity(np);
        tstart.push(0u32);
        for (i, piece) in pieces.iter().enumerate() {
            let s = *tstart.last().unwrap();
            let (len, border) = match piece {
                Piece::Copy(c) => {
                    let ch = &self.chunks[*c as usize];
                    (ch.tri_end - ch.tri_start, ch.tri_border - ch.tri_start)
                }
                Piece::Built(_) => {
                    let pt = &built[&i];
                    (pt.tris.len() as u32, pt.tris.iter().filter(|t| crosses_chunk(*t, vstart[i], vstart[i + 1])).count() as u32)
                }
            };
            tborder.push(s + border);
            tstart.push(s + len);
        }
        let nt = *tstart.last().unwrap() as usize;
        // Old and new triangle numbers of the built pieces.
        // Likewise for the triangles of the affected chunks.
        let mut toff = vec![u32::MAX; nc];
        let mut tsize = 0u32;
        for c in 0..nc {
            if affected[c] {
                toff[c] = tsize;
                tsize += self.chunks[c].tri_end - self.chunks[c].tri_start;
            }
        }
        let mut tmap = vec![u32::MAX; tsize as usize];
        let mut new_tri_id = vec![u32::MAX; p.new_tris.len()];
        for (&i, pt) in &built {
            for k in 0..pt.tris.len() {
                let id = tstart[i] + k as u32;
                if pt.old[k] != u32::MAX {
                    let t = pt.old[k];
                    let c = self.vert_chunk[self.indices[3 * t as usize] as usize] as usize;
                    tmap[(toff[c] + t - self.chunks[c].tri_start) as usize] = id;
                } else {
                    new_tri_id[pt.new_id[k] as usize] = id;
                }
            }
        }
        let tri_starts: Vec<u32> = self.chunks.iter().map(|ch| ch.tri_start).collect();
        let tshift: Vec<(u32, u32)> = (0..nc)
            .map(|c| if affected[c] { (self.chunks[c].tri_start, u32::MAX) } else { (self.chunks[c].tri_start, tstart[piece_of[c] as usize]) })
            .collect();
        let tmap_f = |t: u32| -> u32 {
            // The owning chunk: the last one starting at or before t (empty chunks share a
            // start with the next, so take the last).
            let c = tri_starts.partition_point(|&s| s <= t) - 1;
            let (old, new) = tshift[c];
            if new != u32::MAX { t - old + new } else { tmap[(toff[c] + t - old) as usize] }
        };
        let per_piece: Vec<(Vec<u32>, Vec<u16>, Vec<u8>)> = pieces
            .par_iter()
            .enumerate()
            .map(|(i, piece)| match piece {
                Piece::Copy(c) => {
                    let ch = &self.chunks[*c as usize];
                    let (s, e) = (ch.tri_start as usize, ch.tri_end as usize);
                    (
                        self.indices[3 * s..3 * e].iter().map(|&v| vmap(v)).collect(),
                        if grouped { (s..e).map(|t| self.group(t as u32)).collect() } else { Vec::new() },
                        if hidden_any { self.hidden[s..e].to_vec() } else { Vec::new() },
                    )
                }
                Piece::Built(_) => {
                    let pt = &built[&i];
                    (
                        pt.tris.iter().flatten().copied().collect(),
                        if grouped { pt.groups.clone() } else { Vec::new() },
                        if hidden_any { pt.hidden.clone() } else { Vec::new() },
                    )
                }
            })
            .collect();
        let (ii, rest): (Vec<Vec<u32>>, Vec<(Vec<u16>, Vec<u8>)>) = per_piece.into_iter().map(|(a, b, c)| (a, (b, c))).unzip();
        let (gg, hh): (Vec<Vec<u16>>, Vec<Vec<u8>>) = rest.into_iter().unzip();
        let indices = concat(&ii);
        let groups = concat(&gg);
        let hidden = concat(&hh);
        debug_assert_eq!(indices.len(), 3 * nt);

        // Tables: vertices whose triangles did not change map their lists; the others are
        // listed again from their triangles.
        let mut new_tris_of: HashMap<VRef, Vec<u32>> = HashMap::default();
        for (j, (tri, _)) in p.new_tris.iter().enumerate() {
            for &r in tri {
                new_tris_of.entry(r).or_default().push(new_tri_id[j]);
            }
        }
        let lists = |r: VRef| -> (Vec<u32>, Vec<u32>) {
            let (old_vt, old_adj): (Vec<u32>, Vec<u32>) = match r {
                VRef::Old(v) if !touched.contains(&v) => {
                    let vt: Vec<u32> = self.incident(v).iter().map(|&t| tmap_f(t)).collect();
                    let adj: Vec<u32> = self.neighbors(v).iter().map(|&w| vmap(w)).collect();
                    return (vt, adj);
                }
                VRef::Old(v) => (self.incident(v).iter().filter(|t| !removed.contains(t)).map(|&t| tmap_f(t)).collect(), Vec::new()),
                VRef::New(_) => (Vec::new(), Vec::new()),
            };
            let mut vt = old_vt;
            if let Some(ts) = new_tris_of.get(&r) {
                vt.extend_from_slice(ts);
            }
            vt.sort_unstable();
            let me = rmap(r);
            let mut adj = old_adj;
            for &t in &vt {
                for &w in &indices[3 * t as usize..3 * t as usize + 3] {
                    if w != me {
                        adj.push(w);
                    }
                }
            }
            adj.sort_unstable();
            adj.dedup();
            (vt, adj)
        };
        let per_piece: Vec<(Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>)> = pieces
            .par_iter()
            .map(|piece| match piece {
                // Whole slices of the old tables, mapped.
                Piece::Copy(c) => {
                    let (s, e) = self.chunks[*c as usize].range();
                    let (vs, ve) = (self.vt_offsets[s] as usize, self.vt_offsets[e] as usize);
                    let (as_, ae) = (self.adj_offsets[s] as usize, self.adj_offsets[e] as usize);
                    (
                        (s..e).map(|v| self.vt_offsets[v + 1] - self.vt_offsets[v]).collect(),
                        self.vt_tris[vs..ve].iter().map(|&t| tmap_f(t)).collect(),
                        (s..e).map(|v| self.adj_offsets[v + 1] - self.adj_offsets[v]).collect(),
                        self.adj_verts[as_..ae].iter().map(|&w| vmap(w)).collect(),
                    )
                }
                Piece::Built(list) => {
                    let (mut vt_n, mut vt, mut adj_n, mut adj) = (Vec::with_capacity(list.len()), Vec::new(), Vec::with_capacity(list.len()), Vec::new());
                    for &r in list {
                        let (a, b) = lists(r);
                        vt_n.push(a.len() as u32);
                        vt.extend(a);
                        adj_n.push(b.len() as u32);
                        adj.extend(b);
                    }
                    (vt_n, vt, adj_n, adj)
                }
            })
            .collect();
        let (vn, rest): (Vec<Vec<u32>>, Vec<(Vec<u32>, Vec<u32>, Vec<u32>)>) = per_piece.into_iter().map(|(a, b, c, d)| (a, (b, c, d))).unzip();
        let (vt, rest): (Vec<Vec<u32>>, Vec<(Vec<u32>, Vec<u32>)>) = rest.into_iter().map(|(b, c, d)| (b, (c, d))).unzip();
        let (an, av): (Vec<Vec<u32>>, Vec<Vec<u32>>) = rest.into_iter().unzip();
        let vt_offsets = offsets(&vn);
        let adj_offsets = offsets(&an);
        let vt_tris = concat(&vt);
        let adj_verts = concat(&av);

        // Chunks: copied ones keep their bounds and borrowed vertices (mapped) and their
        // neighbours unless one of those was rebuilt; built ones are worked out afresh.
        let chunks: Vec<Chunk> = (0..np)
            .map(|i| {
                let mut ch = Chunk::new(vstart[i], vstart[i + 1], tstart[i], tborder[i], tstart[i + 1]);
                if let Piece::Copy(c) = pieces[i] {
                    let old = &self.chunks[c as usize];
                    ch.aabb = old.aabb;
                    let mut ext: Vec<u32> = old.external.iter().map(|&v| vmap(v)).collect();
                    ext.sort_unstable();
                    ch.external = ext;
                    if old.neighbors.iter().all(|&o| !affected[o as usize]) {
                        let mut nb: Vec<u32> = old.neighbors.iter().map(|&o| piece_of[o as usize]).collect();
                        nb.sort_unstable();
                        ch.neighbors = nb;
                    } else {
                        ch.stamp = u32::MAX;
                    }
                } else {
                    ch.stamp = u32::MAX;
                }
                ch
            })
            .collect();
        let mut mesh = Self::assemble_parts(positions, indices, chunks, vert_chunk, adj_offsets, adj_verts, vt_offsets, vt_tris);
        let redo: Vec<usize> = (0..np).filter(|&i| mesh.chunks[i].stamp == u32::MAX).collect();
        let links: Vec<(usize, (Vec<u32>, Vec<u32>))> = redo.par_iter().map(|&i| (i, mesh.chunk_links(i))).collect();
        for (i, (external, nbs)) in links {
            let ch = &mut mesh.chunks[i];
            ch.stamp = 0;
            ch.neighbors = nbs;
            if matches!(pieces[i], Piece::Built(_)) {
                ch.external = external;
            }
        }
        for &i in &redo {
            if matches!(pieces[i], Piece::Built(_)) {
                let aabb = chunk_bounds(&mesh.chunks[i], &mesh.positions);
                mesh.chunks[i].aabb = aabb;
            }
        }
        mesh.mask = mask;
        mesh.colors = colors;
        if grouped {
            mesh.set_groups(groups);
        }
        if hidden_any {
            mesh.set_hidden(hidden);
        }
        (mesh, origins, carried)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same surface: positions per vertex, and every triangle as positions.
    fn same_surface(a: &SculptMesh, b: &SculptMesh) {
        assert_eq!(a.vertex_count(), b.vertex_count());
        assert_eq!(a.triangle_count(), b.triangle_count());
        let key = |m: &SculptMesh, t: &[u32]| {
            let mut k: Vec<[u32; 3]> = t.iter().map(|&v| m.positions[v as usize].to_array().map(f32::to_bits)).collect();
            let i = (0..3).min_by_key(|&i| k[i]).unwrap();
            k.rotate_left(i);
            k
        };
        let mut ta: Vec<_> = a.indices.chunks_exact(3).map(|t| key(a, t)).collect();
        let mut tb: Vec<_> = b.indices.chunks_exact(3).map(|t| key(b, t)).collect();
        ta.sort();
        tb.sort();
        assert!(ta == tb, "same triangles");
    }

    /// Everything the mesh derives is consistent with its arrays.
    fn consistent(m: &SculptMesh) {
        let rebuilt = SculptMesh::from_chunked(m.positions.clone(), m.indices.clone(), &m.chunk_ranges());
        assert_eq!(m.vert_chunk, rebuilt.vert_chunk);
        for v in 0..m.vertex_count() as u32 {
            let (mut a, mut b) = (m.neighbors(v).to_vec(), rebuilt.neighbors(v).to_vec());
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "neighbours of {v}");
            let (mut a, mut b) = (m.incident(v).to_vec(), rebuilt.incident(v).to_vec());
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "triangles of {v}");
        }
        for (x, y) in m.chunks.iter().zip(&rebuilt.chunks) {
            assert_eq!((x.tri_start, x.tri_border, x.tri_end), (y.tri_start, y.tri_border, y.tri_end));
            assert_eq!(x.external, y.external);
            assert_eq!(x.neighbors, y.neighbors);
            assert_eq!(x.aabb.min, y.aabb.min);
            assert_eq!(x.aabb.max, y.aabb.max);
        }
        for ch in &m.chunks {
            for t in ch.tris() {
                let v = m.indices[3 * t as usize];
                assert!(v >= ch.vert_start && v < ch.vert_end, "triangle {t} owned by its first vertex's chunk");
            }
        }
    }

    #[test]
    fn a_patch_gives_the_mesh_a_rebuild_would() {
        let (p, i) = cube_sphere(40, 1.0);
        let mut mesh = SculptMesh::from_raw(p, i);
        mesh.set_groups((0..mesh.triangle_count() as u32).map(|t| (t % 3) as u16).collect());
        mesh.mask = (0..mesh.vertex_count()).map(|v| (v % 7) as u8).collect();
        // Split every triangle near +Z into three around its centre, and drop one vertex
        // by fanning its ring from a neighbour.
        let near: Vec<u32> = (0..mesh.triangle_count() as u32)
            .filter(|&t| mesh.positions[mesh.indices[3 * t as usize] as usize].z > 0.9)
            .collect();
        let mut patch = Patch::default();
        for &t in &near {
            let tri = [0, 1, 2].map(|k| mesh.indices[3 * t as usize + k]);
            let c = tri.iter().map(|&v| mesh.positions[v as usize]).sum::<Vec3>() / 3.0;
            let j = patch.new_verts.len() as u32;
            patch.new_verts.push(NewVertex { pos: c, chunk: mesh.vert_chunk[tri[0] as usize], mask: 3, color: [0; 4] });
            for k in 0..3 {
                patch.new_tris.push(([VRef::Old(tri[k]), VRef::Old(tri[(k + 1) % 3]), VRef::New(j)], mesh.group(t)));
            }
            patch.removed_tris.push(t);
        }
        // Move one vertex of the region.
        let v0 = mesh.indices[3 * near[0] as usize];
        patch.moved.push((v0, mesh.positions[v0 as usize] + Vec3::Z * 0.01));
        // Remove the vertex nearest -Z: its ring is fanned from one of its neighbours.
        let gone_v = (0..mesh.vertex_count() as u32).min_by(|&a, &b| mesh.positions[a as usize].z.total_cmp(&mesh.positions[b as usize].z)).unwrap();
        let next: HashMap<u32, u32> = mesh
            .incident(gone_v)
            .iter()
            .map(|&t| {
                let tri = [0, 1, 2].map(|k| mesh.indices[3 * t as usize + k]);
                let i = tri.iter().position(|&x| x == gone_v).unwrap();
                (tri[(i + 1) % 3], tri[(i + 2) % 3])
            })
            .collect();
        let start = *next.keys().next().unwrap();
        let mut ring = vec![start];
        while ring.len() < next.len() {
            ring.push(next[ring.last().unwrap()]);
        }
        for k in 1..ring.len() - 1 {
            patch.new_tris.push(([VRef::Old(ring[0]), VRef::Old(ring[k]), VRef::Old(ring[k + 1])], mesh.group(mesh.incident(gone_v)[0])));
        }
        patch.removed_tris.extend_from_slice(mesh.incident(gone_v));
        patch.dead.push(gone_v);

        let patched = mesh.patched(&patch);
        // The same thing the slow way.
        let n0 = mesh.vertex_count();
        let mut positions = mesh.positions.clone();
        for &(v, p) in &patch.moved {
            positions[v as usize] = p;
        }
        positions.extend(patch.new_verts.iter().map(|v| v.pos));
        let mut vert_chunk = mesh.vert_chunk.clone();
        vert_chunk.extend(patch.new_verts.iter().map(|v| v.chunk));
        let mut indices = Vec::new();
        let mut groups = Vec::new();
        let gone: HashSet<u32> = patch.removed_tris.iter().copied().collect();
        for t in 0..mesh.triangle_count() as u32 {
            if !gone.contains(&t) {
                indices.extend_from_slice(&mesh.indices[3 * t as usize..3 * t as usize + 3]);
                groups.push(mesh.group(t));
            }
        }
        for (tri, g) in &patch.new_tris {
            indices.extend(tri.map(|r| match r {
                VRef::Old(v) => v,
                VRef::New(j) => n0 as u32 + j,
            }));
            groups.push(*g);
        }
        let mut mask = mesh.mask.clone();
        mask.extend(patch.new_verts.iter().map(|v| v.mask));
        // Without the dead vertex.
        let d = gone_v as usize;
        positions.remove(d);
        vert_chunk.remove(d);
        mask.remove(d);
        for i in &mut indices {
            assert_ne!(*i as usize, d);
            if *i as usize > d {
                *i -= 1;
            }
        }
        let slow = SculptMesh::from_assigned(positions, indices, &vert_chunk, mesh.chunks.len(), groups, Vec::new(), mask, Vec::new());
        same_surface(&patched, &slow);
        consistent(&patched);
        assert_eq!(patched.group_count(), 3);
        // Masks and groups went with their vertices and triangles.
        let mask_at = |m: &SculptMesh, p: Vec3| m.positions.iter().position(|q| *q == p).map(|v| m.mask[v]);
        for v in (0..n0).step_by(97).filter(|&v| v != d) {
            let p = patch.moved.iter().find(|m| m.0 == v as u32).map_or(mesh.positions[v], |m| m.1);
            assert_eq!(mask_at(&patched, p), Some(mesh.mask[v]));
        }
    }

    #[test]
    fn a_chunk_that_grows_past_the_limit_splits() {
        let (p, i) = cube_sphere(30, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        // Subdivide every triangle of chunk 0 into three, over and over, by adding centres.
        let c0: Vec<u32> = mesh.chunks[0].tris().collect();
        let mut patch = Patch::default();
        for &t in &c0 {
            let tri = [0, 1, 2].map(|k| mesh.indices[3 * t as usize + k]);
            let c = tri.iter().map(|&v| mesh.positions[v as usize]).sum::<Vec3>() / 3.0;
            let j = patch.new_verts.len() as u32;
            patch.new_verts.push(NewVertex { pos: c, chunk: 0, mask: 0, color: [0; 4] });
            for k in 0..3 {
                patch.new_tris.push(([VRef::Old(tri[k]), VRef::Old(tri[(k + 1) % 3]), VRef::New(j)], 0));
            }
            patch.removed_tris.push(t);
        }
        let n0 = (mesh.chunks[0].vert_end - mesh.chunks[0].vert_start) as usize;
        let patched = mesh.patched(&patch);
        consistent(&patched);
        assert_eq!(patched.vertex_count(), mesh.vertex_count() + patch.new_verts.len());
        if n0 + patch.new_verts.len() > CHUNK_TARGET {
            assert!(patched.chunks.len() > mesh.chunks.len());
        }
        assert!(patched.chunks.iter().all(|ch| (ch.vert_end - ch.vert_start) as usize <= CHUNK_TARGET));
    }
}
