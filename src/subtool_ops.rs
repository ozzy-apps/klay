//! SubTool › Split, Merge and Extract (ZBrush): whole meshes made from parts of others. They
//! work on subtools without subdivision levels and build new meshes, so polygroups and
//! polypaint come along; masks and hidden polygons do not (the vertices are reordered into
//! new chunks).

use crate::mesh::SculptMesh;
use glam::Vec3;

/// The triangles of `mesh` that `keep` accepts, as a new mesh (vertices renumbered, groups and
/// paint kept), or `None` when it accepts none.
pub fn part(mesh: &SculptMesh, keep: impl Fn(u32) -> bool) -> Option<SculptMesh> {
    let mut map = vec![u32::MAX; mesh.vertex_count()];
    let (mut positions, mut colors, mut indices, mut groups) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let painted = !mesh.colors.is_empty();
    for t in 0..mesh.triangle_count() as u32 {
        if !keep(t) {
            continue;
        }
        for k in 0..3 {
            let v = mesh.indices[3 * t as usize + k] as usize;
            if map[v] == u32::MAX {
                map[v] = positions.len() as u32;
                positions.push(mesh.positions[v]);
                if painted {
                    colors.push(mesh.colors[v]);
                }
            }
            indices.push(map[v]);
        }
        groups.push(mesh.group(t));
    }
    (!indices.is_empty()).then(|| SculptMesh::from_raw_painted(positions, indices, groups, colors))
}

/// Two meshes as one (SubTool › Merge Down). `b`'s polygroups are renumbered after `a`'s so
/// both keep their own; paint is kept where either has it.
pub fn merged(a: &SculptMesh, b: &SculptMesh) -> SculptMesh {
    let shift = a.groups.iter().copied().max().map_or(1, |g| g + 1);
    let mut positions = a.positions.clone();
    positions.extend_from_slice(&b.positions);
    let base = a.vertex_count() as u32;
    let mut indices = a.indices.clone();
    indices.extend(b.indices.iter().map(|i| i + base));
    let mut groups: Vec<u16> = (0..a.triangle_count() as u32).map(|t| a.group(t)).collect();
    groups.extend((0..b.triangle_count() as u32).map(|t| b.group(t).saturating_add(shift)));
    let colors = if a.colors.is_empty() && b.colors.is_empty() {
        Vec::new()
    } else {
        let fill = |m: &SculptMesh| if m.colors.is_empty() { vec![[0; 4]; m.vertex_count()] } else { m.colors.clone() };
        let mut c = fill(a);
        c.extend(fill(b));
        c
    };
    SculptMesh::from_raw_painted(positions, indices, groups, colors)
}

/// SubTool › Extract: the masked surface (triangles whose corners are masked on average) as a
/// closed shell `thickness` thick, grown outward from the surface along its normals, with
/// walls along its open edges. One polygroup.
pub fn extracted(mesh: &SculptMesh, thickness: f32) -> Option<SculptMesh> {
    if mesh.mask.is_empty() {
        return None;
    }
    let masked = |t: u32| {
        let i = 3 * t as usize;
        mesh.visible(t) && mesh.indices[i..i + 3].iter().map(|&v| mesh.mask[v as usize] as u32).sum::<u32>() > 3 * 127
    };
    let mut map = vec![u32::MAX; mesh.vertex_count()];
    let (mut inner, mut tris) = (Vec::new(), Vec::new());
    for t in 0..mesh.triangle_count() as u32 {
        if !masked(t) {
            continue;
        }
        let mut tri = [0u32; 3];
        for k in 0..3 {
            let v = mesh.indices[3 * t as usize + k] as usize;
            if map[v] == u32::MAX {
                map[v] = inner.len() as u32;
                inner.push(v as u32);
            }
            tri[k] = map[v];
        }
        tris.push(tri);
    }
    if tris.is_empty() {
        return None;
    }
    let n = inner.len() as u32;
    // Normals of the piece itself, so its rim does not lean with the polygons left behind.
    let mut normals = vec![Vec3::ZERO; inner.len()];
    for t in &tris {
        let [a, b, c] = t.map(|i| mesh.positions[inner[i as usize] as usize]);
        let face = (b - a).cross(c - a);
        for &i in t {
            normals[i as usize] += face;
        }
    }
    let mut positions: Vec<Vec3> = inner.iter().map(|&v| mesh.positions[v as usize]).collect();
    let outer: Vec<Vec3> = positions.iter().zip(&normals).map(|(p, nrm)| *p + nrm.normalize_or_zero() * thickness).collect();
    positions.extend(outer);
    // Outer layer as it was, inner layer turned inside out, walls along the open edges.
    let mut indices = Vec::with_capacity(tris.len() * 6);
    for t in &tris {
        indices.extend_from_slice(&[t[0] + n, t[1] + n, t[2] + n]);
        indices.extend_from_slice(&[t[0], t[2], t[1]]);
    }
    let mut edges: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if edges[&(a.min(b), a.max(b))] == 1 {
                // The open edge a → b of the outer layer meets its wall as b → a.
                indices.extend_from_slice(&[b + n, a + n, a]);
                indices.extend_from_slice(&[b + n, a, b]);
            }
        }
    }
    let groups = vec![0u16; indices.len() / 3];
    Some(SculptMesh::from_raw_painted(positions, indices, groups, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(10, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// Signed volume (closed, outward-facing meshes are positive).
    fn volume(m: &SculptMesh) -> f32 {
        m.indices.chunks(3).map(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|v| m.positions[v as usize]);
            a.dot(b.cross(c)) / 6.0
        }).sum()
    }

    #[test]
    fn parts_and_merges_keep_triangles_and_groups() {
        let mut m = sphere();
        let groups: Vec<u16> = (0..m.triangle_count() as u32).map(|t| (t % 3) as u16).collect();
        m.set_groups(groups);
        let two = part(&m, |t| m.group(t) == 2).unwrap();
        assert_eq!(two.triangle_count(), (0..m.triangle_count()).filter(|t| t % 3 == 2).count());
        assert!(part(&m, |_| false).is_none());
        let both = merged(&m, &sphere());
        assert_eq!(both.triangle_count(), 2 * m.triangle_count());
        assert_eq!(both.group_count(), 4, "the second sphere's group comes after the first's three");
        assert!((volume(&both) - 2.0 * volume(&m)).abs() < 1e-3);
    }

    #[test]
    fn extract_makes_a_closed_outward_shell_of_the_mask() {
        let mut m = sphere();
        assert!(extracted(&m, 0.05).is_none(), "nothing masked");
        m.mask = m.positions.iter().map(|p| if p.z > 0.5 { 255 } else { 0 }).collect();
        let shell = extracted(&m, 0.05).unwrap();
        // Closed: every edge is shared by exactly two triangles.
        let mut edges = std::collections::HashMap::new();
        for t in shell.indices.chunks(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_insert(0u32) += 1;
            }
        }
        assert!(edges.values().all(|&c| c == 2));
        // Outward, and about the masked area times the thickness.
        let v = volume(&shell);
        let area = 2.0 * std::f32::consts::PI * (1.0 - 0.5); // a spherical cap above z = 0.5
        assert!(v > 0.0 && (v - area * 0.05).abs() < area * 0.05 * 0.35, "{v} vs {}", area * 0.05);
    }
}
