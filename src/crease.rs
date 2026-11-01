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
fn edges(mesh: &SculptMesh) -> Vec<(u32, u32, u32, u32)> {
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
