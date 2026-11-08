//! Creased edges (ZBrush: Tool › Geometry › Crease) and the edge lines ZRemesher keeps.
//!
//! A crease is a tag on an edge (see [`SculptMesh::creases`]): Divide smooths a creased
//! edge as if it were a border, so it stays sharp at every new level, and ZRemesher's
//! KeepCreases runs an edge loop along it. Crease PG creases every polygroup border, Crease
//! the border of the visible part, and UnCrease takes the creases off the visible part.
//! ZRemesher's Detect Edges finds sharp edges by angle with [`sharp_edges`].

use crate::mesh::SculptMesh;
use glam::Vec3;
use rayon::prelude::*;

const NONE: u32 = u32::MAX;

/// Geometry › Crease buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreaseOp {
    /// Crease PG: every polygroup border.
    Groups,
    /// Crease: the border of the visible part.
    Visible,
    /// UnCrease: the creases of the visible part go.
    Uncrease,
}

impl CreaseOp {
    pub fn name(self) -> &'static str {
        match self {
            Self::Groups => "Crease PG",
            Self::Visible => "Crease",
            Self::Uncrease => "UnCrease",
        }
    }

    /// The crease bits `mesh` gets.
    pub fn bits(self, mesh: &SculptMesh) -> Vec<u8> {
        match self {
            Self::Groups => crease_pg(mesh),
            Self::Visible => crease_visible(mesh),
            Self::Uncrease => uncrease(mesh),
        }
    }
}

/// Every edge once, as `(low, high, triangle, other triangle or NONE)`.
pub(crate) fn edges(mesh: &SculptMesh) -> Vec<(u32, u32, u32, u32)> {
    mesh.indices
        .par_chunks_exact(3)
        .enumerate()
        .flat_map_iter(|(t, tri)| {
            let t = t as u32;
            (0..3).filter_map(move |k| {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let other = mesh
                    .incident(a)
                    .iter()
                    .copied()
                    .find(|&u| u != t && mesh.indices[3 * u as usize..3 * u as usize + 3].contains(&b))
                    .unwrap_or(NONE);
                (other == NONE || t < other).then_some((a.min(b), a.max(b), t, other))
            })
        })
        .collect()
}

fn sorted(mut e: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    e.par_sort_unstable();
    e.dedup();
    e
}

/// Union of two sorted edge lists.
fn union(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    sorted(a.iter().chain(b).copied().collect())
}

/// Polygroup borders between triangles of which at least one is visible.
pub fn polygroup_borders(mesh: &SculptMesh) -> Vec<(u32, u32)> {
    sorted(
        edges(mesh)
            .into_par_iter()
            .filter(|&(_, _, t, u)| u != NONE && mesh.group(t) != mesh.group(u) && (mesh.visible(t) || mesh.visible(u)))
            .map(|(a, b, _, _)| (a, b))
            .collect(),
    )
}

/// Edges between a visible and a hidden triangle.
pub fn visible_border(mesh: &SculptMesh) -> Vec<(u32, u32)> {
    if mesh.hidden.is_empty() {
        return Vec::new();
    }
    sorted(
        edges(mesh)
            .into_par_iter()
            .filter(|&(_, _, t, u)| u != NONE && mesh.visible(t) != mesh.visible(u))
            .map(|(a, b, _, _)| (a, b))
            .collect(),
    )
}

/// Crease PG: crease bits with every polygroup border added to `mesh`'s creases.
pub fn crease_pg(mesh: &SculptMesh) -> Vec<u8> {
    mesh.crease_bits(&union(&mesh.crease_edges(), &polygroup_borders(mesh)))
}

/// Crease: crease bits with the border of the visible part added.
pub fn crease_visible(mesh: &SculptMesh) -> Vec<u8> {
    mesh.crease_bits(&union(&mesh.crease_edges(), &visible_border(mesh)))
}

/// UnCrease: crease bits without the creases of edges on visible triangles (all of them
/// when nothing is hidden).
pub fn uncrease(mesh: &SculptMesh) -> Vec<u8> {
    if mesh.hidden.is_empty() {
        return Vec::new();
    }
    let keep: Vec<(u32, u32)> = mesh.crease_edges();
    let on_visible = sorted(
        edges(mesh)
            .into_par_iter()
            .filter(|&(_, _, t, u)| mesh.visible(t) || (u != NONE && mesh.visible(u)))
            .map(|(a, b, _, _)| (a, b))
            .collect(),
    );
    let keep: Vec<(u32, u32)> = keep.into_iter().filter(|e| on_visible.binary_search(e).is_err()).collect();
    mesh.crease_bits(&keep)
}

/// Edges where the surface turns by more than `angle` degrees.
pub fn sharp_edges(mesh: &SculptMesh, angle: f32) -> Vec<(u32, u32)> {
    let cos = angle.to_radians().cos();
    let p = &mesh.positions;
    let normal = |t: u32| {
        let i = 3 * t as usize;
        let (a, b, c) = (p[mesh.indices[i] as usize], p[mesh.indices[i + 1] as usize], p[mesh.indices[i + 2] as usize]);
        (b - a).cross(c - a).normalize_or_zero()
    };
    sorted(
        edges(mesh)
            .into_par_iter()
            .filter(|&(_, _, t, u)| {
                if u == NONE {
                    return false;
                }
                let (n, m) = (normal(t), normal(u));
                n != Vec3::ZERO && m != Vec3::ZERO && n.dot(m) < cos
            })
            .map(|(a, b, _, _)| (a, b))
            .collect(),
    )
}

/// Edges as lines of vertices: each line runs between vertices where the lines do not
/// simply continue (ends and junctions), or around a closed loop (first vertex repeated
/// at the end). Also returns the junctions (vertices on three or more of the edges).
pub fn chains(edges: &[(u32, u32)]) -> (Vec<Vec<u32>>, Vec<u32>) {
    use crate::fx::FxHashMap as HashMap;
    let mut nb: HashMap<u32, Vec<u32>> = HashMap::default();
    for &(a, b) in edges {
        nb.entry(a).or_default().push(b);
        nb.entry(b).or_default().push(a);
    }
    let mut keys: Vec<u32> = nb.keys().copied().collect();
    keys.sort_unstable();
    let junctions: Vec<u32> = keys.iter().copied().filter(|v| nb[v].len() >= 3).collect();
    let mut used: crate::fx::FxHashSet<(u32, u32)> = Default::default();
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut out = Vec::new();
    let walk = |start: u32, next: u32, used: &mut crate::fx::FxHashSet<(u32, u32)>| {
        let mut line = vec![start];
        let (mut prev, mut at) = (start, next);
        used.insert(key(start, next));
        loop {
            line.push(at);
            if at == start || nb[&at].len() != 2 {
                break;
            }
            let Some(&n) = nb[&at].iter().find(|&&w| w != prev && !used.contains(&key(at, w))) else { break };
            used.insert(key(at, n));
            prev = at;
            at = n;
        }
        line
    };
    // Lines from ends and junctions first, then what is left: closed loops.
    for &v in &keys {
        if nb[&v].len() != 2 {
            for &w in &nb[&v].clone() {
                if !used.contains(&key(v, w)) {
                    out.push(walk(v, w, &mut used));
                }
            }
        }
    }
    for &v in &keys {
        for &w in &nb[&v].clone() {
            if !used.contains(&key(v, w)) {
                out.push(walk(v, w, &mut used));
            }
        }
    }
    (out, junctions)
}

/// Lines of `edges` at least `min_length` long, as points of `mesh`, and the junction points
/// of the lines kept.
pub fn lines(mesh: &SculptMesh, edges: &[(u32, u32)], min_length: f32) -> (Vec<Vec<Vec3>>, Vec<Vec3>) {
    let (chains, junctions) = chains(edges);
    let p = &mesh.positions;
    let kept: Vec<Vec<u32>> = chains
        .into_iter()
        .filter(|c| c.windows(2).map(|w| p[w[0] as usize].distance(p[w[1] as usize])).sum::<f32>() >= min_length)
        .collect();
    let corners = junctions
        .into_iter()
        .filter(|j| kept.iter().filter(|c| c.first() == Some(j) || c.last() == Some(j)).count() >= 2)
        .map(|j| p[j as usize])
        .collect();
    (kept.into_iter().map(|c| c.into_iter().map(|v| p[v as usize]).collect()).collect(), corners)
}

/// The lines of `edges` (see [`chains`]) fit to remesh at `quad`: short chains go (open ones
/// under 4 quads, closed ones under 6), and so does a chain that mostly runs along a longer
/// kept chain of its own kind (70% of its vertices within half a quad of an open or closed
/// chain, like itself, over 30% longer: the noise beside a real edge on a dense model). A
/// fillet's two parallel edges, alike in length, both stay. Returns the edges kept and the
/// closed loops among them, each its vertices round once (the first not repeated).
pub fn clean_lines(mesh: &SculptMesh, edges: &[(u32, u32)], quad: f32) -> (Vec<(u32, u32)>, Vec<Vec<u32>>) {
    use crate::fx::FxHashMap as HashMap;
    let p = &mesh.positions;
    let length = |c: &[u32]| c.windows(2).map(|w| p[w[0] as usize].distance(p[w[1] as usize])).sum::<f32>();
    // The chains long enough, (vertices, closed, length), the longest first.
    let mut cs: Vec<(Vec<u32>, bool, f32)> = chains(edges)
        .0
        .into_iter()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let closed = c.len() > 3 && c.first() == c.last();
            let l = length(&c);
            (c, closed, l)
        })
        .filter(|&(_, closed, l)| l >= quad * if closed { 6.0 } else { 4.0 })
        .collect();
    cs.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    // The kept chains' segments by the cells (half a quad) their boxes cover.
    let cell = (quad * 0.5).max(1e-6);
    let key = |q: Vec3| {
        let c = (q / cell).floor();
        (c.x as i32, c.y as i32, c.z as i32)
    };
    let mut grid: HashMap<(i32, i32, i32), Vec<(u32, u32, u32)>> = HashMap::default();
    let seg_dist = |q: Vec3, a: Vec3, b: Vec3| -> f32 {
        let ab = b - a;
        let t = ((q - a).dot(ab) / ab.length_squared().max(1e-24)).clamp(0.0, 1.0);
        q.distance(a + ab * t)
    };
    let mut kept: Vec<usize> = Vec::new();
    for i in 0..cs.len() {
        let (c, closed, l) = (&cs[i].0, cs[i].1, cs[i].2);
        let own: &[u32] = if closed { &c[..c.len() - 1] } else { c };
        // How many of its vertices lie along each longer kept chain of its kind.
        let mut along: HashMap<u32, usize> = HashMap::default();
        for &v in own {
            let q = p[v as usize];
            let k = key(q);
            let mut near: Vec<u32> = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &(ci, a, b) in grid.get(&(k.0 + dx, k.1 + dy, k.2 + dz)).into_iter().flatten() {
                            let other = &cs[ci as usize];
                            if other.1 == closed && other.2 > l / 0.7 && !near.contains(&ci) && seg_dist(q, p[a as usize], p[b as usize]) <= quad * 0.5 {
                                near.push(ci);
                            }
                        }
                    }
                }
            }
            for ci in near {
                *along.entry(ci).or_default() += 1;
            }
        }
        if along.values().any(|&n| n * 10 >= own.len() * 7) {
            continue;
        }
        kept.push(i);
        for w in c.windows(2) {
            let (a, b) = (p[w[0] as usize], p[w[1] as usize]);
            let (ka, kb) = (key(a), key(b));
            for x in ka.0.min(kb.0)..=ka.0.max(kb.0) {
                for y in ka.1.min(kb.1)..=ka.1.max(kb.1) {
                    for z in ka.2.min(kb.2)..=ka.2.max(kb.2) {
                        grid.entry((x, y, z)).or_default().push((i as u32, w[0], w[1]));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut loops = Vec::new();
    for &i in &kept {
        let c = &cs[i].0;
        out.extend(c.windows(2).map(|w| (w[0].min(w[1]), w[0].max(w[1]))));
        if cs[i].1 {
            loops.push(c[..c.len() - 1].to_vec());
        }
    }
    (sorted(out), loops)
}

/// The edges of the lines of `edges` (see [`chains`]) at least `min_length` long.
pub fn long_lines_only(mesh: &SculptMesh, edges: &[(u32, u32)], min_length: f32) -> Vec<(u32, u32)> {
    let p = &mesh.positions;
    let mut out = Vec::new();
    for c in chains(edges).0 {
        if c.windows(2).map(|w| p[w[0] as usize].distance(p[w[1] as usize])).sum::<f32>() >= min_length {
            out.extend(c.windows(2).map(|w| (w[0].min(w[1]), w[0].max(w[1]))));
        }
    }
    sorted(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(seg: u32) -> SculptMesh {
        let (p, i) = crate::primitive::box_surface([seg; 3], |p| p);
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn a_cube_has_twelve_sharp_edges_meeting_at_eight_corners() {
        let m = cube(6);
        let e = sharp_edges(&m, 40.0);
        assert_eq!(e.len(), 12 * 6, "{}", e.len());
        let (lines, corners) = lines(&m, &e, 0.5);
        assert_eq!(lines.len(), 12);
        assert_eq!(corners.len(), 8);
    }

    #[test]
    fn clean_lines_keeps_the_cubes_edges_and_a_ring_and_drops_short_or_doubled_lines() {
        let m = cube(6);
        let e = sharp_edges(&m, 40.0);
        // Twelve open edges of ten quads: all kept, no closed loop.
        let (kept, loops) = clean_lines(&m, &e, 0.2);
        assert_eq!(kept.len(), 12 * 6);
        assert!(loops.is_empty());
        // At a quad of 0.6 each edge is 3.3 quads: too short, all gone.
        assert!(clean_lines(&m, &e, 0.6).0.is_empty());
        // A polygroup border round a sphere's equator is one closed loop (on the cube it
        // would meet the vertical edges, and a line through a junction is not a loop).
        let (p, i) = crate::mesh::cube_sphere(30, 1.0);
        let mut g = SculptMesh::from_raw(p, i);
        let groups: Vec<u16> = (0..g.triangle_count() as u32)
            .map(|t| {
                let i = 3 * t as usize;
                let c = (g.positions[g.indices[i] as usize] + g.positions[g.indices[i + 1] as usize] + g.positions[g.indices[i + 2] as usize]) / 3.0;
                (c.y > 0.0) as u16
            })
            .collect();
        g.set_groups(groups);
        let border = polygroup_borders(&g);
        assert_eq!(border.len(), 120);
        let (kept, loops) = clean_lines(&g, &border, 0.2);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].len(), 120);
        assert_eq!(kept.len(), 120);
        // At a quad of 1.2 the ring (6.3 long) is under six quads: dropped.
        assert!(clean_lines(&g, &border, 1.2).0.is_empty());
        // A line beside a long edge: on a finer cube, the row of vertices one segment (1/12)
        // below the top edge, from z = -7/12 to 7/12 (1.17 long, not touching another edge).
        let m = cube(24);
        let e = sharp_edges(&m, 40.0);
        let p = &m.positions;
        let beside: Vec<u32> = (0..p.len() as u32).filter(|&v| p[v as usize].x > 0.999 && (p[v as usize].y - 11.0 / 12.0).abs() < 1e-5 && p[v as usize].z.abs() < 7.0 / 12.0 + 1e-5).collect();
        assert_eq!(beside.len(), 15);
        let mut noisy = e.clone();
        noisy.extend(beside.windows(2).map(|w| (w[0].min(w[1]), w[0].max(w[1]))));
        let noisy = sorted(noisy);
        // Quad 0.25: the row is 4.7 quads long, a third of a quad from the edge (8 quads,
        // over 30% longer): the noise beside a real edge, dropped.
        let (kept, _) = clean_lines(&m, &noisy, 0.25);
        assert_eq!(kept.len(), 12 * 24);
        // Quad 0.15: 0.56 quad away, a line of its own, kept.
        let (kept, _) = clean_lines(&m, &noisy, 0.15);
        assert_eq!(kept.len(), 12 * 24 + 14);
    }

    #[test]
    fn crease_pg_creases_group_borders_and_uncrease_takes_them_off() {
        let mut m = cube(4);
        let groups: Vec<u16> = (0..m.triangle_count() as u32)
            .map(|t| {
                let i = 3 * t as usize;
                let c = (m.positions[m.indices[i] as usize] + m.positions[m.indices[i + 1] as usize] + m.positions[m.indices[i + 2] as usize]) / 3.0;
                (c.y > 0.0) as u16
            })
            .collect();
        m.set_groups(groups);
        let bits = crease_pg(&m);
        m.set_creases(bits);
        let e = m.crease_edges();
        assert_eq!(e.len(), 4 * 4, "a ring around the middle of the cube");
        assert!(e.iter().all(|&(a, b)| m.positions[a as usize].y.abs() < 1e-5 && m.positions[b as usize].y.abs() < 1e-5));
        let bits = uncrease(&m);
        m.set_creases(bits);
        assert!(m.creases.is_empty());
    }
}
