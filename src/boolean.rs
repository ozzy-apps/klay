//! Booleans between subtools (ZBrush: SubTool › Boolean, Live Boolean, Make Boolean Mesh).
//!
//! Every visible subtool, top to bottom, joins the result of the ones above it: **Add**
//! (union), **Subtract** (carves itself out) or **Intersect** (keeps only what both hold).
//! Live Boolean shows the result as you work: each operand's inside is kept as a small
//! distance volume ([`crate::wedge::distance_volume`]) and the viewport draws only the
//! surface points on the result's boundary (see `shader.wgsl`). Make Boolean Mesh builds
//! the result as one new closed mesh with Remesh's exact winding-number scan, a rule per
//! grid line instead of the union, and sharp creases where operands meet.

use crate::mesh::SculptMesh;
use glam::Vec3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BoolOp {
    #[default]
    Add,
    Subtract,
    Intersect,
}

impl BoolOp {
    pub const ALL: [Self; 3] = [Self::Add, Self::Subtract, Self::Intersect];

    pub fn name(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
        }
    }

    /// Mark beside a subtool's name.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Intersect => "∩",
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(c: u8) -> Self {
        Self::ALL.get(c as usize).copied().unwrap_or_default()
    }
}

/// Whether a point is inside the result, given whether it is inside each operand.
pub fn fold(ops: &[BoolOp], inside: impl Fn(usize) -> bool) -> bool {
    let mut r = false;
    for (i, op) in ops.iter().enumerate() {
        let a = inside(i);
        r = match op {
            BoolOp::Add => r || a,
            BoolOp::Subtract => r && !a,
            BoolOp::Intersect => r && a,
        };
    }
    r
}

/// Make Boolean Mesh: `parts` in order (mesh and how it joins), as one closed mesh at
/// Remesh resolution `resolution`. Each part keeps its polygroups (renumbered so parts
/// stay apart) and its polypaint.
pub fn make_mesh(parts: &[(&SculptMesh, BoolOp)], resolution: u32) -> Result<(SculptMesh, crate::wedge::Stats), String> {
    if parts.is_empty() {
        return Err("no visible subtools".into());
    }
    let painted = parts.iter().any(|(m, _)| !m.colors.is_empty());
    let (mut positions, mut indices, mut groups, mut colors, mut tags) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut next_group = 0u16;
    for (k, (m, _)) in parts.iter().enumerate() {
        let base = positions.len() as u32;
        positions.extend_from_slice(&m.positions);
        indices.extend(m.indices.iter().map(|&i| i + base));
        let first = next_group;
        for t in 0..m.triangle_count() as u32 {
            let g = first.saturating_add(m.group(t));
            groups.push(g);
            next_group = next_group.max(g.saturating_add(1));
        }
        if painted {
            if m.colors.is_empty() {
                colors.extend(std::iter::repeat_n([0u8; 4], m.vertex_count()));
            } else {
                colors.extend_from_slice(&m.colors);
            }
        }
        tags.extend(std::iter::repeat_n(k as u8, m.triangle_count()));
    }
    let (merged, owner) = SculptMesh::from_raw_tagged(positions, indices, groups, colors, tags);
    let ops: Vec<BoolOp> = parts.iter().map(|p| p.1).collect();
    let settings = crate::wedge::Settings { resolution, blur: 0, project: false };
    let out = crate::wedge::remesh_with(&merged, &settings, Some(&crate::wedge::Csg { owner: &owner, ops: &ops }))?;
    if out.indices.is_empty() {
        return Err("the result is empty".into());
    }
    let colors = if painted { out.colors } else { Vec::new() };
    Ok((SculptMesh::from_raw_painted(out.positions, out.indices, out.groups, colors), out.stats))
}

/// The world → volume map of a [`crate::wedge::Volume`]: texture coordinates (0..1 over
/// its `n` points, at point centres) of a world point.
pub fn volume_box(v: &crate::wedge::Volume) -> (Vec3, Vec3) {
    // Point i sits at origin + h i; texel centre i at (i + 0.5) / n.
    let size = v.h * v.n as f32;
    (v.origin - Vec3::splat(v.h * 0.5), Vec3::splat(1.0 / size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::cube_sphere;

    fn ball(r: f32, at: Vec3, seg: u32) -> SculptMesh {
        let (p, i) = cube_sphere(seg, r);
        SculptMesh::from_raw(p.into_iter().map(|q| q + at).collect(), i)
    }

    fn volume(m: &SculptMesh) -> f32 {
        m.indices
            .chunks_exact(3)
            .map(|t| m.positions[t[0] as usize].dot(m.positions[t[1] as usize].cross(m.positions[t[2] as usize])) / 6.0)
            .sum()
    }

    #[test]
    fn fold_applies_the_operations_in_order() {
        use BoolOp::*;
        assert!(fold(&[Add, Subtract], |i| [true, false][i]));
        assert!(!fold(&[Add, Subtract], |i| [true, true][i]));
        assert!(fold(&[Add, Intersect], |i| [true, true][i]));
        assert!(!fold(&[Add, Intersect], |i| [true, false][i]));
        assert!(fold(&[Add, Add, Subtract], |i| [false, true, false][i]));
        assert!(!fold(&[Subtract], |_| true));
    }

    #[test]
    fn make_mesh_subtracts_and_intersects() {
        use std::f32::consts::PI;
        let a = ball(1.0, Vec3::ZERO, 24);
        let b = ball(0.6, Vec3::new(0.9, 0.0, 0.0), 24);
        let res = 160;
        let (sub, _) = make_mesh(&[(&a, BoolOp::Add), (&b, BoolOp::Subtract)], res).unwrap();
        let (int, _) = make_mesh(&[(&a, BoolOp::Add), (&b, BoolOp::Intersect)], res).unwrap();
        let (uni, _) = make_mesh(&[(&a, BoolOp::Add), (&b, BoolOp::Add)], res).unwrap();
        let (va, vb) = (4.0 / 3.0 * PI, 4.0 / 3.0 * PI * 0.216);
        let (vs, vi, vu) = (volume(&sub), volume(&int), volume(&uni));
        // The pieces add up: A − B + A ∩ B = A, and A ∪ B = A + B − A ∩ B.
        assert!((vs + vi - va).abs() < va * 0.03, "{vs} + {vi} vs {va}");
        assert!((vu - (va + vb - vi)).abs() < va * 0.03, "{vu}");
        assert!(vi > 0.05 && vs < va - 0.05);
        // Nothing of the subtracted ball's middle is left, and the bite is where it was.
        assert!(sub.positions.iter().all(|p| p.distance(Vec3::new(0.9, 0.0, 0.0)) > 0.55));
        assert!(int.positions.iter().all(|p| p.x > 0.25));
        assert!(crate::wedge::closed(&sub) && crate::wedge::closed(&int));
    }

    #[test]
    fn distance_volume_is_negative_inside_and_crosses_at_the_surface() {
        let m = ball(1.0, Vec3::new(0.5, 0.0, 0.0), 24);
        let v = crate::wedge::distance_volume(&m, 64);
        let (lo, inv) = volume_box(&v);
        let sample = |p: Vec3| {
            // Nearest point.
            let g = ((p - lo) * inv * v.n as f32 - Vec3::splat(0.5)).round();
            let (i, j, k) = (g.x as usize, g.y as usize, g.z as usize);
            v.data[i + v.n * (j + v.n * k)]
        };
        assert!(sample(Vec3::new(0.5, 0.0, 0.0)) < 10, "deep inside");
        assert!(sample(Vec3::new(0.5, 0.0, 1.07)) > 200, "outside: {}", sample(Vec3::new(0.5, 0.0, 1.07)));
        let s = sample(Vec3::new(0.5, 1.0, 0.0)) as f32;
        assert!((s - 127.5).abs() < 50.0, "near the surface: {s}");
    }
}
