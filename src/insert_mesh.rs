//! Insert Mesh (ZBrush: the IMM brushes, IMM Primitives): drag on the model to add a mesh
//! there. It stands on the surface (its Y along the normal, its centre on the point under
//! the press), the drag sets its radius, and it is as dense as the model around it so it
//! sculpts alike. It goes into the active subtool as a polygroup of its own; with symmetry
//! every mirror and radial copy gets one too.

use crate::mesh::SculptMesh;
use crate::primitive::Primitive;
use crate::symmetry::Symmetry;
use glam::{Quat, Vec3};

/// The meshes the brush inserts (ZBrush: the IMM Primitives picker).
pub const SHAPES: [Primitive; 4] = [Primitive::Sphere, Primitive::Cube, Primitive::Cylinder, Primitive::Ring];

/// Most segments an inserted mesh gets, so a big drag on a dense model stays quick.
const MAX_SEGMENTS: u32 = 120;

/// Segments for `shape` at `radius` so its edges come out about `edge` long.
pub fn segments_for(shape: Primitive, radius: f32, edge: f32) -> u32 {
    let per = radius / edge.max(1e-6);
    let n = match shape {
        // Edges across a cube face of 2r in n steps.
        Primitive::Sphere | Primitive::Cube | Primitive::Plane => 2.0 * per,
        // A quarter turn of the wall (cylinder) or the tube's turn (ring: tube radius r/4).
        Primitive::Cylinder | Primitive::Ring => std::f32::consts::FRAC_PI_2 * per,
    };
    let (lo, hi) = shape.segment_range();
    (n.round() as u32).clamp(lo, hi.min(MAX_SEGMENTS))
}

/// Mean edge length of `mesh`, from an even sample of its triangles.
pub fn mean_edge(mesh: &SculptMesh) -> f32 {
    let tris = mesh.triangle_count();
    let step = (tris / 4096).max(1);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for t in (0..tris).step_by(step) {
        let i = &mesh.indices[3 * t..3 * t + 3];
        let (a, b, c) = (mesh.positions[i[0] as usize], mesh.positions[i[1] as usize], mesh.positions[i[2] as usize]);
        sum += (a.distance(b) + b.distance(c) + c.distance(a)) as f64 / 3.0;
        n += 1;
    }
    if n == 0 { 0.05 } else { (sum / n as f64) as f32 }
}

/// `shape` with `segments`, radius `radius`, centred on `at` with its Y along `normal`,
/// and its copies for `symmetry`, all in polygroup `group`.
pub fn inserted(shape: Primitive, segments: u32, at: Vec3, normal: Vec3, radius: f32, symmetry: Symmetry, group: u16) -> SculptMesh {
    let (base, tris) = shape.build(segments, radius.max(1e-5));
    let turn = Quat::from_rotation_arc(Vec3::Y, normal.normalize_or(Vec3::Y));
    let placed: Vec<Vec3> = base.iter().map(|&p| at + turn * p).collect();
    let images = symmetry.images();
    let (mut positions, mut indices) = (Vec::with_capacity(placed.len() * images.len()), Vec::with_capacity(tris.len() * images.len()));
    for k in 0..images.len() {
        let first = positions.len() as u32;
        positions.extend(placed.iter().map(|&p| images.map(k, p)));
        // A mirror image turns the triangles inside out; put their order back.
        let flip = images.mirrors(k);
        indices.extend(tris.chunks_exact(3).flat_map(|t| {
            let t = if flip { [t[0], t[2], t[1]] } else { [t[0], t[1], t[2]] };
            t.map(|i| i + first)
        }));
    }
    let groups = vec![group; indices.len() / 3];
    SculptMesh::from_raw_with_groups(positions, indices, groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_meshes_stand_on_the_surface_and_mirror_outward() {
        let n = Vec3::new(0.0, 0.0, 1.0);
        let m = inserted(Primitive::Cylinder, 6, Vec3::new(0.3, 0.0, 1.0), n, 0.2, Symmetry::default(), 7);
        let (lo, hi) = m.positions.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        // The cylinder's axis runs along the normal (Z), centred on the point.
        assert!(((lo + hi) * 0.5 - Vec3::new(0.3, 0.0, 1.0)).length() < 1e-4, "{lo} {hi}");
        assert!((hi.z - lo.z - 0.4).abs() < 1e-4);
        assert!(m.groups.iter().all(|&g| g == 7));
        // X symmetry: a second copy at -X, still facing outward (positive volume).
        let sym = Symmetry { x: true, ..Symmetry::default() };
        let m = inserted(Primitive::Sphere, 4, Vec3::new(0.5, 0.0, 0.0), Vec3::X, 0.1, sym, 1);
        assert!(m.positions.iter().any(|p| p.x < -0.4));
        let volume: f32 = m
            .indices
            .chunks_exact(3)
            .map(|t| m.positions[t[0] as usize].dot(m.positions[t[1] as usize].cross(m.positions[t[2] as usize])) / 6.0)
            .sum();
        assert!(volume > 0.0, "{volume}");
        // About the model's density, within the cap.
        assert_eq!(segments_for(Primitive::Sphere, 0.1, 0.02), 10);
        assert_eq!(segments_for(Primitive::Sphere, 10.0, 0.001), MAX_SEGMENTS);
    }
}
