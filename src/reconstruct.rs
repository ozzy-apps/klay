//! Reconstruct Subdiv (ZBrush: Tool › Geometry › Reconstruct Subdiv): find the coarser mesh a
//! mesh was divided from.
//!
//! Divide cuts every triangle into four: one in the middle, joining the midpoints of its
//! edges, and three at its corners. So the vertices of a divided mesh are of two kinds:
//! *even* ones, where the coarser mesh had a vertex, all of whose neighbours are midpoints,
//! and *odd* ones, the midpoints, each with exactly two even neighbours on opposite sides
//! (six neighbours inside, four on a border). Starting from one vertex taken to be even,
//! that rule labels the whole connected part; a few starting guesses are tried, and the
//! labelling is accepted only if every triangle is a middle one (three odd corners) or a
//! corner one (one even), and every middle triangle has a corner triangle across each
//! edge. The coarser triangles are then read off the middle triangles.

use crate::mesh::SculptMesh;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Label {
    Unknown,
    Even,
    Odd,
}

/// The coarser mesh found under a divided one.
#[derive(Debug)]
pub struct Coarse {
    /// Per coarse vertex: the vertex of the divided mesh above it.
    pub even: Vec<u32>,
    /// Per vertex of the divided mesh: its coarse vertex and `NONE`, or for a midpoint the
    /// two coarse vertices of its edge.
    pub of: Vec<(u32, u32)>,
    /// Coarse triangles, three coarse vertex ids each.
    pub indices: Vec<u32>,
    /// Per coarse triangle: the middle triangle of the divided mesh inside it.
    pub centre: Vec<u32>,
}

/// The neighbours of `v` in turn around it, and whether they close a loop (inside) or not
/// (on a border, from one border neighbour to the other). `None` where the surface around
/// `v` is not a single fan.
fn fan(mesh: &SculptMesh, v: u32) -> Option<(Vec<u32>, bool)> {
    let tris = mesh.incident(v);
    let mut next: Vec<(u32, u32)> = Vec::with_capacity(tris.len());
    for &t in tris {
        let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
        let k = tri.iter().position(|&x| x == v)?;
        next.push((tri[(k + 1) % 3], tri[(k + 2) % 3]));
    }
    // Start where nothing leads in (a border), else anywhere.
    let start = next.iter().map(|e| e.0).find(|&a| !next.iter().any(|e| e.1 == a)).unwrap_or(next[0].0);
    let mut out = vec![start];
    let mut at = start;
    loop {
        let Some(&(_, b)) = next.iter().find(|e| e.0 == at) else { break };
        if b == start {
            return (out.len() == next.len()).then_some((out, true));
        }
        if out.len() > next.len() {
            return None;
        }
        out.push(b);
        at = b;
    }
    (out.len() == next.len() + 1).then_some((out, false))
}

/// Label the connected part of `seed`, which is taken to be even. Returns the vertices
/// labelled, or `None` (with the labels of the part cleared) if the rule breaks.
fn label_from(mesh: &SculptMesh, seed: u32, labels: &mut [Label]) -> Option<Vec<u32>> {
    let mut seen = vec![seed];
    labels[seed as usize] = Label::Even;
    let mut queue = std::collections::VecDeque::from([seed]);
    let ok = (|| {
        while let Some(v) = queue.pop_front() {
            let want: Vec<(u32, Label)> = if labels[v as usize] == Label::Even {
                mesh.neighbors(v).iter().map(|&w| (w, Label::Odd)).collect()
            } else {
                let (ring, closed) = fan(mesh, v)?;
                let evens = if closed {
                    if ring.len() != 6 {
                        return None;
                    }
                    let i = ring.iter().position(|&w| labels[w as usize] == Label::Even)?;
                    [i, (i + 3) % 6]
                } else {
                    if ring.len() != 4 {
                        return None;
                    }
                    [0, 3]
                };
                ring.iter().enumerate().map(|(k, &w)| (w, if evens.contains(&k) { Label::Even } else { Label::Odd })).collect()
            };
            for (w, l) in want {
                match labels[w as usize] {
                    Label::Unknown => {
                        labels[w as usize] = l;
                        seen.push(w);
                        queue.push_back(w);
                    }
                    x if x != l => return None,
                    _ => {}
                }
            }
        }
        Some(())
    })();
    if ok.is_none() {
        for &v in &seen {
            labels[v as usize] = Label::Unknown;
        }
        return None;
    }
    Some(seen)
}

/// The coarser mesh `mesh` was divided from, or why there is none.
pub fn coarse(mesh: &SculptMesh) -> Result<Coarse, String> {
    let n = mesh.vertex_count();
    let nt = mesh.triangle_count();
    if nt < 4 || nt % 4 != 0 {
        return Err("the mesh is not divided: its triangles do not come in fours".into());
    }
    let mut labels = vec![Label::Unknown; n];
    let not_divided = || "the mesh is not divided evenly (every triangle cut into four)".to_string();
    for v0 in 0..n as u32 {
        if labels[v0 as usize] != Label::Unknown || mesh.neighbors(v0).is_empty() {
            continue;
        }
        // Starting guesses: a vertex a midpoint cannot be (valence other than six inside,
        // four on a border) is even for sure; otherwise the corners of a triangle and of its
        // neighbours, one of which is even unless it is a middle triangle.
        let mut part = vec![v0];
        let mut k = 0;
        let mut in_part = std::collections::HashSet::from([v0]);
        while k < part.len() {
            for &w in mesh.neighbors(part[k]) {
                if in_part.insert(w) {
                    part.push(w);
                }
            }
            k += 1;
        }
        let sure = part.iter().copied().find(|&v| match fan(mesh, v) {
            Some((r, true)) => r.len() != 6,
            Some((r, false)) => r.len() != 4,
            None => false,
        });
        let mut seeds: Vec<u32> = sure.into_iter().collect();
        if let Some(&t) = mesh.incident(v0).first() {
            let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
            seeds.extend_from_slice(tri);
            for &c in tri {
                for &u in mesh.incident(c) {
                    seeds.extend_from_slice(&mesh.indices[3 * u as usize..3 * u as usize + 3]);
                }
            }
        }
        seeds.dedup();
        let mut done = false;
        for s in seeds {
            if let Some(seen) = label_from(mesh, s, &mut labels) {
                if part_is_divided(mesh, &seen, &labels) {
                    done = true;
                    break;
                }
                for &v in &seen {
                    labels[v as usize] = Label::Unknown;
                }
            }
        }
        if !done {
            return Err(not_divided());
        }
    }
    // Coarse vertices, and each midpoint's edge.
    let mut id = vec![NONE; n];
    let mut even = Vec::new();
    for v in 0..n {
        if labels[v] == Label::Even {
            id[v] = even.len() as u32;
            even.push(v as u32);
        }
    }
    let mut of = vec![(NONE, NONE); n];
    for v in 0..n {
        match labels[v] {
            Label::Even => of[v] = (id[v], NONE),
            Label::Odd => {
                let e: Vec<u32> = mesh.neighbors(v as u32).iter().filter(|&&w| labels[w as usize] == Label::Even).map(|&w| id[w as usize]).collect();
                if e.len() != 2 {
                    return Err(not_divided());
                }
                of[v] = (e[0].min(e[1]), e[0].max(e[1]));
            }
            Label::Unknown => {}
        }
    }
    // Coarse triangles from the middle ones: across the middle triangle's edge k is the
    // corner triangle at the coarse triangle's vertex k + 1.
    let mut indices = Vec::with_capacity(nt / 4 * 3);
    let mut centre = Vec::with_capacity(nt / 4);
    for t in 0..nt as u32 {
        let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
        if tri.iter().any(|&v| labels[v as usize] != Label::Odd) {
            continue;
        }
        let mut e = [NONE; 3];
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let across = mesh.incident(a).iter().copied().find(|&u| u != t && mesh.indices[3 * u as usize..3 * u as usize + 3].contains(&b)).ok_or_else(not_divided)?;
            let c = mesh.indices[3 * across as usize..3 * across as usize + 3].iter().copied().find(|&x| labels[x as usize] == Label::Even).ok_or_else(not_divided)?;
            e[k] = id[c as usize];
        }
        indices.extend_from_slice(&[e[2], e[0], e[1]]);
        centre.push(t);
    }
    if centre.len() * 4 != nt {
        return Err(not_divided());
    }
    Ok(Coarse { even, of, indices, centre })
}

/// Whether the labels of a connected part make every triangle a middle or a corner one, and
/// every middle triangle surrounded by corner ones.
fn part_is_divided(mesh: &SculptMesh, part: &[u32], labels: &[Label]) -> bool {
    let mut tris: Vec<u32> = part.iter().flat_map(|&v| mesh.incident(v).iter().copied()).collect();
    tris.sort_unstable();
    tris.dedup();
    let evens = |t: u32| mesh.indices[3 * t as usize..3 * t as usize + 3].iter().filter(|&&v| labels[v as usize] == Label::Even).count();
    let mut middles = 0;
    for &t in &tris {
        match evens(t) {
            0 => {
                middles += 1;
                let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
                for k in 0..3 {
                    let (a, b) = (tri[k], tri[(k + 1) % 3]);
                    match mesh.incident(a).iter().copied().find(|&u| u != t && mesh.indices[3 * u as usize..3 * u as usize + 3].contains(&b)) {
                        Some(u) if evens(u) == 1 => {}
                        _ => return false,
                    }
                }
            }
            1 => {}
            _ => return false,
        }
    }
    middles * 4 == tris.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_divided_sphere_and_plane_give_back_their_coarse_meshes() {
        for (p, i) in [crate::mesh::cube_sphere(5, 1.0), crate::primitive::Primitive::Plane.build(6, 1.0)] {
            let coarse_mesh = SculptMesh::from_raw(p, i);
            let (fine, _) = crate::multires::subdivide(&coarse_mesh);
            let c = coarse(&fine).unwrap();
            assert_eq!(c.even.len(), coarse_mesh.vertex_count());
            assert_eq!(c.indices.len(), coarse_mesh.indices.len());
            let back: std::collections::HashSet<[u32; 3]> = c.indices.chunks_exact(3).map(|t| {
                let mut k = [t[0], t[1], t[2]];
                k.sort();
                k
            }).collect();
            assert_eq!(back.len(), coarse_mesh.triangle_count());
        }
    }

    #[test]
    fn an_undivided_mesh_is_refused() {
        // Five cells along each edge of the cube cannot have come from halving.
        let (p, i) = crate::mesh::cube_sphere(5, 1.0);
        assert!(coarse(&SculptMesh::from_raw(p, i)).is_err());
    }
}
