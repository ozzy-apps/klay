//! CurveTube (ZBrush: Brush › CurveTube): geometry along a curve.
//!
//! The curve is drawn on the model like any curve stroke (see `curve.rs`). On release a
//! closed tube is built around it and added to the active subtool as a polygroup of its
//! own, the way ZBrush's curve insert brushes add a mesh along the curve: dreadlocks,
//! cables, straps, piping. The tube's centre line runs on the surface (its root sits half
//! inside the model), its rings are as far apart as its sides are wide, it is turned along
//! the curve without twisting (each ring's frame is the previous one carried along), and
//! both ends are closed with round caps. Taper narrows it toward the end of the curve.

use crate::curve::Curve;
use crate::mesh::SculptMesh;
use glam::Vec3;

/// Shape of the tubes a curve makes (Stroke › Curve, for CurveTube).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tube {
    /// Radius at the start, in world units (the brush's Draw Size).
    pub radius: f32,
    /// 0 keeps the radius, 1 narrows it to a point at the end.
    pub taper: f32,
    /// Vertices around each ring.
    pub sides: u32,
}

impl Tube {
    pub const MIN_SIDES: u32 = 3;
    pub const MAX_SIDES: u32 = 32;
}

/// One closed tube around `points` (at least two, in order), starting with its frame's
/// first axis along `up` (the surface normal at the start). Returns positions and
/// outward-facing triangles; empty when the line has no length.
pub fn tube(points: &[Vec3], up: Vec3, shape: Tube) -> (Vec<Vec3>, Vec<u32>) {
    let sides = shape.sides.clamp(Tube::MIN_SIDES, Tube::MAX_SIDES) as usize;
    let r0 = shape.radius.max(1e-6);
    let taper = shape.taper.clamp(0.0, 1.0);
    // Even rings, about as far apart as one side is wide, so the faces come out square.
    let step = (std::f32::consts::TAU * r0 / sides as f32).max(1e-6);
    let mut length = 0.0;
    for w in points.windows(2) {
        length += w[0].distance(w[1]);
    }
    if points.len() < 2 || length <= step * 0.05 {
        return (Vec::new(), Vec::new());
    }
    let rings_n = ((length / step).round() as usize).max(1) + 1;
    let centres = resample_n(points, length, rings_n);
    let n = centres.len();
    let tangents: Vec<Vec3> = (0..n)
        .map(|i| (centres[(i + 1).min(n - 1)] - centres[i.saturating_sub(1)]).normalize_or(Vec3::X))
        .collect();
    // Rotation-minimizing frames: the first axis starts along the surface normal and is
    // carried from ring to ring by the rotation between their tangents.
    let mut u = perpendicular(up, tangents[0]);
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            u = glam::Quat::from_rotation_arc(tangents[i - 1], tangents[i]) * u;
            u = perpendicular(u, tangents[i]);
        }
        frames.push((u, tangents[i].cross(u)));
    }
    let radius = |s: f32| r0 * (1.0 - taper * (s / length).clamp(0.0, 1.0)).max(0.02);
    // Rings: (centre, u, v, radius), with round caps added before the first and after the
    // last ring (quarter circles in `cap` steps), then one pole at each end.
    let cap = (sides / 4).max(1);
    let mut rings: Vec<(Vec3, Vec3, Vec3, f32)> = Vec::with_capacity(n + 2 * cap);
    let (start_r, end_r) = (radius(0.0), radius(length));
    let quarter = |k: usize| (k as f32 / (cap + 1) as f32) * std::f32::consts::FRAC_PI_2;
    for k in (1..=cap).rev() {
        let a = quarter(k);
        rings.push((centres[0] - tangents[0] * start_r * a.sin(), frames[0].0, frames[0].1, start_r * a.cos()));
    }
    for i in 0..n {
        rings.push((centres[i], frames[i].0, frames[i].1, radius(length * i as f32 / (n - 1) as f32)));
    }
    for k in 1..=cap {
        let a = quarter(k);
        rings.push((centres[n - 1] + tangents[n - 1] * end_r * a.sin(), frames[n - 1].0, frames[n - 1].1, end_r * a.cos()));
    }
    let mut positions = Vec::with_capacity(rings.len() * sides + 2);
    for &(c, u, v, r) in &rings {
        for j in 0..sides {
            let (s, co) = (std::f32::consts::TAU * j as f32 / sides as f32).sin_cos();
            positions.push(c + (u * co + v * s) * r);
        }
    }
    let first_pole = positions.len() as u32;
    positions.push(centres[0] - tangents[0] * start_r);
    positions.push(centres[n - 1] + tangents[n - 1] * end_r);
    let at = |k: usize, j: usize| (k * sides + j % sides) as u32;
    let mut indices = Vec::with_capacity(6 * sides * rings.len());
    for k in 0..rings.len() - 1 {
        for j in 0..sides {
            // With u, v, t right-handed these face outward.
            let (a, b, c, d) = (at(k, j), at(k, j + 1), at(k + 1, j), at(k + 1, j + 1));
            indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    let last = rings.len() - 1;
    for j in 0..sides {
        indices.extend_from_slice(&[first_pole, at(0, j + 1), at(0, j)]);
        indices.extend_from_slice(&[first_pole + 1, at(last, j), at(last, j + 1)]);
    }
    (positions, indices)
}

/// Every tube of a curve: one per piece, and with `symmetry` one per piece of its mirror
/// image across X = 0, as one mesh in polygroup `group`.
pub fn tubes(curve: &Curve, shape: Tube, symmetry: bool, group: u16) -> Option<SculptMesh> {
    let mirrored = symmetry.then(|| curve.mirrored());
    let (mut positions, mut indices) = (Vec::new(), Vec::new());
    for c in std::iter::once(curve).chain(mirrored.as_ref()) {
        for piece in &c.pieces {
            if piece.len() < 2 {
                continue;
            }
            let (p, i) = tube(&c.points[piece.clone()], c.normals[piece.start], shape);
            let base = positions.len() as u32;
            positions.extend(p);
            indices.extend(i.into_iter().map(|i| i + base));
        }
    }
    if positions.is_empty() {
        return None;
    }
    let groups = vec![group; indices.len() / 3];
    Some(SculptMesh::from_raw_with_groups(positions, indices, groups))
}

/// `a` made perpendicular to the unit vector `t` and unit length (any perpendicular when
/// `a` runs along `t`).
fn perpendicular(a: Vec3, t: Vec3) -> Vec3 {
    let p = a - t * a.dot(t);
    if p.length_squared() > 1e-10 {
        p.normalize()
    } else {
        t.any_orthonormal_vector()
    }
}

/// `n` points evenly spread by arc length along a polyline of total `length`, both ends kept.
fn resample_n(points: &[Vec3], length: f32, n: usize) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(n);
    let (mut seg, mut seg_start) = (0usize, 0.0f32);
    for k in 0..n {
        let s = length * k as f32 / (n - 1) as f32;
        while seg + 2 < points.len() && seg_start + points[seg].distance(points[seg + 1]) < s {
            seg_start += points[seg].distance(points[seg + 1]);
            seg += 1;
        }
        let (a, b) = (points[seg], points[seg + 1]);
        let len = a.distance(b);
        let f = if len > 0.0 { ((s - seg_start) / len).clamp(0.0, 1.0) } else { 0.0 };
        out.push(a.lerp(b, f));
    }
    *out.last_mut().unwrap() = *points.last().unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Tube {
        Tube { radius: 0.1, taper: 0.0, sides: 8 }
    }

    /// Signed volume of a closed triangle mesh (positive when it faces outward).
    fn volume(p: &[Vec3], i: &[u32]) -> f32 {
        i.chunks_exact(3).map(|t| p[t[0] as usize].dot(p[t[1] as usize].cross(p[t[2] as usize])) / 6.0).sum()
    }

    #[test]
    fn a_straight_tube_is_closed_round_and_faces_out() {
        let line: Vec<Vec3> = (0..=10).map(|i| Vec3::new(i as f32 * 0.1, 0.0, 0.0)).collect();
        let (p, i) = tube(&line, Vec3::Y, shape());
        // Every edge is shared by exactly two triangles, in opposite directions: closed and
        // consistently wound.
        let mut edges = std::collections::HashMap::new();
        for t in i.chunks_exact(3) {
            for k in 0..3 {
                *edges.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
            }
        }
        for (&(a, b), &n) in &edges {
            assert_eq!(n, 1, "edge {a}-{b} twice the same way");
            assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}-{b} open");
        }
        // A cylinder of length 1 with two half-sphere caps; an octagon's area is 0.9 of
        // the circle's.
        let r: f32 = 0.1;
        let expect = std::f32::consts::PI * r * r * 1.0 + 4.0 / 3.0 * std::f32::consts::PI * r.powi(3);
        let v = volume(&p, &i);
        assert!(v > 0.0, "faces inward");
        assert!((v / expect - 0.9).abs() < 0.06, "volume {v} vs {expect}");
        // The middle rings lie on the radius, ends run past the curve by one radius.
        assert!(p.iter().all(|q| (q.y * q.y + q.z * q.z).sqrt() <= r + 1e-4));
        let (lo, hi) = p.iter().fold((f32::MAX, f32::MIN), |(lo, hi), q| (lo.min(q.x), hi.max(q.x)));
        assert!((lo + r).abs() < 1e-4 && (hi - 1.0 - r).abs() < 1e-4, "{lo} {hi}");
        // Faces are about square: rings one side's width apart (rings 0 and 1 are the
        // start cap's).
        let side = std::f32::consts::TAU * r / 8.0;
        let gap = p[16].distance(p[24]);
        assert!(gap > side * 0.8 && gap < side * 1.3, "{gap} vs {side}");
    }

    #[test]
    fn taper_narrows_to_the_end_and_the_frame_does_not_twist() {
        // A quarter circle in the XZ plane, starting with the frame along +Y.
        let arc: Vec<Vec3> = (0..=40)
            .map(|d| {
                let a = (d as f32 * 90.0 / 40.0).to_radians();
                Vec3::new(a.sin(), 0.0, 1.0 - a.cos())
            })
            .collect();
        let s = Tube { taper: 1.0, ..shape() };
        let (p, i) = tube(&arc, Vec3::Y, s);
        assert!(volume(&p, &i) > 0.0);
        let rings = (p.len() - 2) / 8;
        let width = |k: usize| p[k * 8].distance(p[k * 8 + 4]);
        assert!(width(2) > 0.19 && width(rings - 3) < 0.04, "{} {}", width(2), width(rings - 3));
        // The curve turns about Y only, so vertex 0 of every ring stays straight up.
        for k in 0..rings {
            let c = (p[k * 8] + p[k * 8 + 4]) * 0.5;
            let up = (p[k * 8] - c).normalize_or_zero();
            assert!(up.y > 0.999 || (p[k * 8] - c).length() < 1e-3, "ring {k} turned: {up:?}");
        }
    }

    #[test]
    fn a_curve_makes_one_tube_per_piece_and_its_mirror() {
        let hits: Vec<_> = (0..=20).map(|i| Some((Vec3::new(0.2 + i as f32 * 0.02, 0.0, 0.0), Vec3::Y))).collect();
        let curve = Curve::from_hits(&hits, 0.01);
        let one = tubes(&curve, shape(), false, 1).unwrap();
        let two = tubes(&curve, shape(), true, 1).unwrap();
        assert_eq!(two.vertex_count(), 2 * one.vertex_count());
        assert!(two.positions.iter().any(|p| p.x < -0.2));
        // The mirror image is closed and faces outward as well.
        assert!(volume(&two.positions, &two.indices) > 1.9 * volume(&one.positions, &one.indices));
        assert!(tubes(&Curve::default(), shape(), true, 1).is_none());
    }

    #[test]
    fn a_tube_appends_to_a_mesh_as_its_own_polygroup_and_cuts_back() {
        let (p, i, g) = crate::primitive::Settings::default().build(1.0);
        let mut base = SculptMesh::from_raw_with_groups(p, i, g);
        crate::paint::ensure(&mut base);
        base.colors[3] = [9, 9, 9, 255];
        base.mask = vec![0; base.vertex_count()];
        base.mask[5] = 200;
        let keep = base.extent();
        let hits: Vec<_> = (0..=20).map(|i| Some((Vec3::new(-0.3 + i as f32 * 0.03, 1.0, 0.0), Vec3::Y))).collect();
        let group = base.max_group() + 1;
        let part = tubes(&Curve::from_hits(&hits, 0.01), shape(), false, group).unwrap();
        let joined = base.appended(keep, Some(&part));
        assert_eq!(joined.vertex_count(), keep.verts + part.vertex_count());
        assert_eq!(joined.positions[..keep.verts], base.positions[..]);
        assert_eq!(joined.indices[..3 * keep.tris], base.indices[..]);
        assert_eq!((joined.colors[3], joined.mask[5]), ([9, 9, 9, 255], 200));
        assert_eq!(joined.group_count(), base.group_count() + 1);
        assert!((keep.tris..joined.triangle_count()).all(|t| joined.group(t as u32) == group));
        // The model's own raycast ignores the tube; the full one hits it.
        let (o, d) = (Vec3::new(0.0, 3.0, 0.0), -Vec3::Y);
        let top = joined.raycast(o, d).unwrap();
        assert!((top.point.y - 1.1).abs() < 0.02, "{}", top.point);
        let model = joined.raycast_in(o, d, keep.chunks).unwrap();
        assert!(model.point.y < 1.0 + 1e-3, "{}", model.point);
        // The tail is the part again; cutting back and appending it gives the same mesh.
        let tail = joined.tail(keep).unwrap();
        assert_eq!((tail.positions.clone(), tail.indices.clone()), (part.positions.clone(), part.indices.clone()));
        let again = joined.appended(keep, Some(&tail));
        assert_eq!(again.positions, joined.positions);
        assert_eq!(again.indices, joined.indices);
        assert_eq!(again.groups, joined.groups);
        assert_eq!(again.chunk_ranges(), joined.chunk_ranges());
        let cut = joined.appended(keep, None);
        assert_eq!((cut.positions.clone(), cut.indices.clone(), cut.groups.clone()), (base.positions.clone(), base.indices.clone(), base.groups.clone()));
        assert!(cut.tail(keep).is_none());
        // In place, the same as building it afresh.
        let mut inplace = joined.appended(keep, None);
        assert!(inplace.swap_tail(keep, Some(&part)).is_none());
        assert_eq!((&inplace.positions, &inplace.indices, &inplace.groups, &inplace.mask, &inplace.colors), (&joined.positions, &joined.indices, &joined.groups, &joined.mask, &joined.colors));
        assert_eq!((&inplace.adj_offsets, &inplace.adj_verts, &inplace.vt_offsets, &inplace.vt_tris, &inplace.vert_chunk), (&joined.adj_offsets, &joined.adj_verts, &joined.vt_offsets, &joined.vt_tris, &joined.vert_chunk));
        assert_eq!(inplace.chunk_ranges(), joined.chunk_ranges());
        for (a, b) in inplace.chunks.iter().zip(&joined.chunks) {
            assert_eq!((&a.neighbors, &a.external, a.aabb.min, a.aabb.max), (&b.neighbors, &b.external, b.aabb.min, b.aabb.max));
        }
        assert_eq!(inplace.group_count(), joined.group_count());
        let back = inplace.swap_tail(keep, None).unwrap();
        assert_eq!((back.positions, back.groups), (part.positions.clone(), part.groups.clone()));
        assert_eq!((&inplace.positions, &inplace.adj_verts, &inplace.vt_tris), (&base.positions, &base.adj_verts, &base.vt_tris));
        // As undo steps on a stack: add, take away, put back.
        let mut doc = crate::multires::Multires::new(base, usize::MAX);
        assert!(doc.swap_part(keep, Some(&part)).is_none());
        assert_eq!(doc.mesh().vertex_count(), joined.vertex_count());
        let removed = doc.swap_part(keep, None).unwrap();
        assert_eq!(doc.mesh().vertex_count(), keep.verts);
        doc.swap_part(keep, Some(&removed));
        assert_eq!(doc.mesh().positions, joined.positions);
    }
}
