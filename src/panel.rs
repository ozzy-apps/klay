//! Panel Loops (ZBrush: Geometry › EdgeLoop › Panel Loops): every visible polygroup becomes a
//! panel of its own, a closed slab `thickness` thick with a rounded rim of `loops` rows, so
//! the model reads as panels with grooves between them (armour, car bodies, hard-surface
//! parts). Hidden polygons stay as they are.
//!
//! Each panel is its group's triangles offset along their normals: a top layer and a bottom
//! layer turned inside out, joined at the group's border by a wall. **Elevation** places the
//! slab: 100 grows it outward from the surface, -100 inward, 0 centres it. **Bevel** rounds
//! the rim: the wall bulges out by up to half the thickness and the top and bottom borders
//! are pulled in by that much, as a rounded rectangle seen in section.

use crate::mesh::SculptMesh;
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Slab thickness as a share of the model's size.
    pub thickness: f32,
    /// Rows in the rim, 1 to 8.
    pub loops: u32,
    /// Roundness of the rim, 0 (square) to 1 (half round).
    pub bevel: f32,
    /// -100 inward, 0 centred, 100 outward.
    pub elevation: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { thickness: 0.02, loops: 4, bevel: 0.5, elevation: 100.0 }
    }
}

/// The model with every visible polygroup made a panel, or `None` when no visible group
/// has a border to make a rim from (one closed group).
pub fn panels(mesh: &SculptMesh, s: &Settings) -> Option<SculptMesh> {
    let size = {
        let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)));
        (hi - lo).max_element().max(1e-6)
    };
    let t = s.thickness.max(0.0) * size;
    let loops = s.loops.clamp(1, 8);
    let e = (s.elevation / 100.0).clamp(-1.0, 1.0);
    let half = t * 0.5;
    let r = s.bevel.clamp(0.0, 1.0) * half;

    // Visible triangles by group; hidden ones are copied.
    let mut by_group: std::collections::BTreeMap<u16, Vec<u32>> = Default::default();
    let mut hidden = Vec::new();
    for tri in 0..mesh.triangle_count() as u32 {
        if mesh.visible(tri) {
            by_group.entry(mesh.group(tri)).or_default().push(tri);
        } else {
            hidden.push(tri);
        }
    }
    let mut next_group = by_group.keys().copied().max().unwrap_or(0).max(hidden.iter().map(|&t| mesh.group(t)).max().unwrap_or(0));
    let (mut positions, mut indices, mut groups) = (Vec::new(), Vec::new(), Vec::new());
    let mut any_rim = false;

    for (&group, tris) in &by_group {
        // The patch: its own vertices.
        let mut local = std::collections::HashMap::new();
        let mut verts: Vec<u32> = Vec::new();
        let patch: Vec<[u32; 3]> = tris
            .iter()
            .map(|&t| {
                std::array::from_fn(|k| {
                    let v = mesh.indices[3 * t as usize + k];
                    *local.entry(v).or_insert_with(|| {
                        verts.push(v);
                        verts.len() as u32 - 1
                    })
                })
            })
            .collect();
        let n = verts.len();
        let p: Vec<Vec3> = verts.iter().map(|&v| mesh.positions[v as usize]).collect();
        let mut normal = vec![Vec3::ZERO; n];
        for f in &patch {
            let face = (p[f[1] as usize] - p[f[0] as usize]).cross(p[f[2] as usize] - p[f[0] as usize]);
            for &i in f {
                normal[i as usize] += face;
            }
        }
        let normal: Vec<Vec3> = normal.into_iter().map(|x| x.normalize_or(Vec3::Y)).collect();
        // Border edges in triangle winding: the patch lies to their left.
        let mut count: std::collections::HashMap<(u32, u32), u32> = Default::default();
        for f in &patch {
            for k in 0..3 {
                let (a, b) = (f[k], f[(k + 1) % 3]);
                *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        let border: Vec<(u32, u32)> = patch
            .iter()
            .flat_map(|f| (0..3).map(move |k| (f[k], f[(k + 1) % 3])))
            .filter(|&(a, b)| count[&(a.min(b), a.max(b))] == 1)
            .collect();
        any_rim |= !border.is_empty();
        // Inward direction at each border vertex, along the surface.
        let mut inward = vec![Vec3::ZERO; n];
        for &(a, b) in &border {
            let d = p[b as usize] - p[a as usize];
            for v in [a, b] {
                inward[v as usize] += normal[v as usize].cross(d);
            }
        }
        let inward: Vec<Vec3> = inward.into_iter().map(|w| w.normalize_or_zero()).collect();
        let on_border = |v: usize| inward[v] != Vec3::ZERO;
        // A point of the rim's section at height `y` (-half bottom to half top) about the
        // slab's middle: pulled in where the rounded corners are.
        let mid = |v: usize| p[v] + normal[v] * (half * e);
        let section = |v: usize, y: f32| {
            let corner = (y.abs() - (half - r)).max(0.0);
            let pull = if r > 0.0 { r - (r * r - corner * corner).max(0.0).sqrt() } else { 0.0 };
            mid(v) + normal[v] * y + inward[v] * pull
        };
        let base = positions.len() as u32;
        // Bottom layer, then top layer.
        for v in 0..n {
            positions.push(if on_border(v) { section(v, -half) } else { mid(v) - normal[v] * half });
        }
        for v in 0..n {
            positions.push(if on_border(v) { section(v, half) } else { mid(v) + normal[v] * half });
        }
        let (bottom, top) = (base, base + n as u32);
        let (bottom_group, rim_group) = (next_group.wrapping_add(1), next_group.wrapping_add(2));
        next_group = next_group.wrapping_add(2);
        for f in &patch {
            indices.extend_from_slice(&[top + f[0], top + f[1], top + f[2]]);
            groups.push(group);
            indices.extend_from_slice(&[bottom + f[0], bottom + f[2], bottom + f[1]]);
            groups.push(bottom_group);
        }
        // Rows of the rim between the bottom (row 0) and top (row `loops`) borders.
        let mut row: std::collections::HashMap<(u32, u32), u32> = Default::default();
        let mut at = |v: u32, k: u32, positions: &mut Vec<Vec3>| -> u32 {
            if k == 0 {
                return bottom + v;
            }
            if k == loops {
                return top + v;
            }
            *row.entry((v, k)).or_insert_with(|| {
                let y = -half + t * k as f32 / loops as f32;
                positions.push(section(v as usize, y));
                positions.len() as u32 - 1
            })
        };
        for &(a, b) in &border {
            for k in 0..loops {
                let (a_lo, b_lo) = (at(a, k, &mut positions), at(b, k, &mut positions));
                let (a_up, b_up) = (at(a, k + 1, &mut positions), at(b, k + 1, &mut positions));
                indices.extend_from_slice(&[b_up, a_up, a_lo]);
                indices.extend_from_slice(&[b_up, a_lo, b_lo]);
                groups.push(rim_group);
                groups.push(rim_group);
            }
        }
    }
    if !any_rim || t <= 0.0 {
        return None;
    }
    // Hidden polygons as they were.
    if !hidden.is_empty() {
        let mut local = std::collections::HashMap::new();
        for &tri in &hidden {
            for k in 0..3 {
                let v = mesh.indices[3 * tri as usize + k];
                let i = *local.entry(v).or_insert_with(|| {
                    positions.push(mesh.positions[v as usize]);
                    positions.len() as u32 - 1
                });
                indices.push(i);
            }
            groups.push(mesh.group(tri));
        }
    }
    let mut out = SculptMesh::from_raw_painted(positions, indices, groups, Vec::new());
    if !hidden.is_empty() {
        let n = out.triangle_count();
        let first_hidden = n - hidden.len();
        out.set_hidden((0..n).map(|t| (t >= first_hidden) as u8).collect());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_group_plane() -> SculptMesh {
        let (p, i) = crate::primitive::Primitive::Plane.build(8, 1.0);
        let mut m = SculptMesh::from_raw(p, i);
        let groups = m
            .indices
            .chunks_exact(3)
            .map(|t| ((m.positions[t[0] as usize] + m.positions[t[1] as usize] + m.positions[t[2] as usize]).x > 0.0) as u16)
            .collect();
        m.set_groups(groups);
        m
    }

    /// Edges used by other than two triangles (a closed surface has none).
    fn open_edges(m: &SculptMesh) -> usize {
        crate::crease::edges(m).iter().filter(|e| e.3 == u32::MAX).count()
    }

    #[test]
    fn each_group_becomes_a_closed_panel_with_a_rounded_rim() {
        let m = two_group_plane();
        let s = Settings { thickness: 0.05, loops: 4, bevel: 1.0, elevation: 100.0 };
        let out = panels(&m, &s).unwrap();
        assert_eq!(open_edges(&out), 0, "every panel is closed");
        assert!(out.group_count() >= 2 * 3, "top, bottom and rim of two panels");
        // Elevation 100: the slab sits on the surface's normal side; the plane is flat
        // along one axis.
        let ext = |m: &SculptMesh, k: usize| {
            let v: Vec<f32> = m.positions.iter().map(|p| p[k]).collect();
            (v.iter().copied().fold(f32::INFINITY, f32::min), v.iter().copied().fold(f32::NEG_INFINITY, f32::max))
        };
        let axis = (0..3).find(|&k| { let (a, b) = ext(&m, k); b - a < 1e-6 }).unwrap();
        let size = (0..3).map(|k| { let (a, b) = ext(&m, k); b - a }).fold(0.0, f32::max);
        let (lo, hi) = ext(&out, axis);
        assert!((hi - lo - 0.05 * size).abs() < 1e-3, "{lo} {hi}");
        assert!(lo.abs() < 1e-5 || hi.abs() < 1e-5, "one face of the slab stays on the surface");
        // With the full bevel the far face's border is pulled in: narrower than the plane.
        let far = if hi.abs() < 1e-5 { lo } else { hi };
        let other = (axis + 1) % 3;
        let top: Vec<f32> = out.positions.iter().filter(|p| (p[axis] - far).abs() < 1e-5).map(|p| p[other]).collect();
        let width = top.iter().copied().fold(f32::NEG_INFINITY, f32::max) - top.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(width < size - 0.02 * size, "{width} {size}");
    }

    #[test]
    fn one_closed_group_has_no_rim_and_hidden_parts_stay() {
        let (p, i) = crate::mesh::cube_sphere(4, 1.0);
        let sphere = SculptMesh::from_raw(p, i);
        assert!(panels(&sphere, &Settings::default()).is_none());
        let mut m = two_group_plane();
        let hidden: Vec<u8> = (0..m.triangle_count() as u32).map(|t| (m.group(t) == 1) as u8).collect();
        let n_hidden = hidden.iter().filter(|&&h| h == 1).count();
        m.set_hidden(hidden);
        let out = panels(&m, &Settings::default()).unwrap();
        assert_eq!(out.hidden_count(), n_hidden);
    }
}
