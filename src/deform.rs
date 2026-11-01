//! Tool › Deformation shapes, ZBrush style: whole-subtool changes of the shown level that
//! are not a plain move or scale (those are `subtool::deform_map`). Each slider is released at
//! a value in -100..=100; masked vertices stay (partly masked ones move part of the way) and
//! vertices of hidden polygons only are left alone, as with brushes.

use crate::mesh::SculptMesh;
use crate::subtool::Deform;
use glam::Vec3;
use rayon::prelude::*;

/// The shown level's positions after `kind` at `value`, or `None` when `kind` is a map
/// (Offset, Size) or nothing would move.
pub fn shaped(mesh: &SculptMesh, kind: Deform, value: f32) -> Option<Vec<Vec3>> {
    let t = (value / 100.0).clamp(-1.0, 1.0);
    if t.abs() < 0.005 || kind.is_map() || mesh.vertex_count() == 0 {
        return None;
    }
    let (lo, hi) = mesh
        .positions
        .par_iter()
        .fold(|| (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)))
        .reduce(|| (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |x, y| (x.0.min(y.0), x.1.max(y.1)));
    let centre = (lo + hi) * 0.5;
    let extent = (hi - lo).max_element().max(1e-6);
    let half = ((hi - lo) * 0.5).max(Vec3::splat(1e-6));
    let moved: Vec<Vec3> = match kind {
        Deform::Smooth | Deform::Polish => {
            // Laplacian passes; Polish alternates a shrinking and an inflating pass (Taubin),
            // so it evens the surface without losing volume.
            let passes = (t.abs() * 10.0).ceil() as usize;
            let mut p = mesh.positions.clone();
            for _ in 0..passes {
                p = relax(mesh, &p, 0.5);
                if kind == Deform::Polish {
                    p = relax(mesh, &p, -0.53);
                }
            }
            p
        }
        Deform::PolishFeatures => {
            // Polish, with the vertices on feature lines (polygroup borders, creases, open
            // borders) relaxed only along their line, and the ends and corners of the lines
            // held: the features stay crisp while the surface between them evens out.
            let lines = feature_neighbours(mesh);
            let passes = (t.abs() * 10.0).ceil() as usize;
            let mut p = mesh.positions.clone();
            for _ in 0..passes {
                p = relax_along(mesh, &lines, &p, 0.5);
                p = relax_along(mesh, &lines, &p, -0.53);
            }
            p
        }
        Deform::Inflate => {
            let amount = t * extent * 0.05;
            (0..mesh.vertex_count() as u32).into_par_iter().map(|v| mesh.positions[v as usize] + mesh.normal_at(v) * amount).collect()
        }
        Deform::Spherize => {
            let radius = mesh.positions.par_iter().map(|p| p.distance(centre)).sum::<f32>() / mesh.vertex_count() as f32;
            mesh.positions
                .par_iter()
                .map(|&p| {
                    let on_sphere = centre + (p - centre).normalize_or(Vec3::Y) * radius;
                    p + (on_sphere - p) * t
                })
                .collect()
        }
        Deform::Noise => {
            let (amount, scale) = (t * extent * 0.02, 12.0 / extent);
            (0..mesh.vertex_count() as u32)
                .into_par_iter()
                .map(|v| {
                    let p = mesh.positions[v as usize];
                    p + mesh.normal_at(v) * (noise(p * scale) * amount)
                })
                .collect()
        }
        Deform::Twist => mesh
            .positions
            .par_iter()
            .map(|&p| {
                // Up to half a turn each way from the middle, about Y through the centre.
                let along = (p.y - centre.y) / half.y;
                let (s, c) = (along * t * std::f32::consts::FRAC_PI_2).sin_cos();
                let d = p - centre;
                centre + Vec3::new(d.x * c + d.z * s, d.y, -d.x * s + d.z * c)
            })
            .collect(),
        Deform::Taper => mesh
            .positions
            .par_iter()
            .map(|&p| {
                // Wider towards the top for positive values, about the Y axis through the centre.
                let along = (p.y - centre.y) / half.y;
                let k = (1.0 + along * t * 0.75).max(0.0);
                Vec3::new(centre.x + (p.x - centre.x) * k, p.y, centre.z + (p.z - centre.z) * k)
            })
            .collect(),
        Deform::OffsetX | Deform::OffsetY | Deform::OffsetZ | Deform::Size => unreachable!(),
    };
    held(mesh, moved)
}

/// `moved` with masks holding vertices back and hidden-only vertices staying, or `None`
/// when nothing moves.
fn held(mesh: &SculptMesh, moved: Vec<Vec3>) -> Option<Vec<Vec3>> {
    let out: Vec<Vec3> = (0..mesh.vertex_count())
        .into_par_iter()
        .map(|v| {
            let p = mesh.positions[v];
            if !mesh.shown.is_empty() && mesh.shown[v] == 0 {
                return p;
            }
            p + (moved[v] - p) * crate::mask::freedom(&mesh.mask, v as u32)
        })
        .collect();
    (out != mesh.positions).then_some(out)
}

/// One pass of neighbour averaging `k` of the way (negative `k` pushes away: Taubin's
/// inflating pass).
fn relax(mesh: &SculptMesh, p: &[Vec3], k: f32) -> Vec<Vec3> {
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let nb = mesh.neighbors(v);
            let own = p[v as usize];
            if nb.is_empty() {
                return own;
            }
            let mean = nb.iter().map(|&n| p[n as usize]).sum::<Vec3>() / nb.len() as f32;
            own + (mean - own) * k
        })
        .collect()
}

/// Per vertex, its neighbours along feature lines (polygroup borders, creased edges, open
/// borders); `None` for a vertex off every line.
fn feature_neighbours(mesh: &SculptMesh) -> Vec<Option<Vec<u32>>> {
    let creased = mesh.crease_edges();
    let mut out: Vec<Option<Vec<u32>>> = vec![None; mesh.vertex_count()];
    for (a, b, t, u) in crate::crease::edges(mesh) {
        let feature = u == u32::MAX || mesh.group(t) != mesh.group(u) || creased.binary_search(&(a, b)).is_ok();
        if feature {
            out[a as usize].get_or_insert_with(Vec::new).push(b);
            out[b as usize].get_or_insert_with(Vec::new).push(a);
        }
    }
    out
}

/// [`relax`], except that a vertex on two feature lines' edges moves toward the mean of
/// those two neighbours only, and one at a line's end or where lines meet stays.
fn relax_along(mesh: &SculptMesh, lines: &[Option<Vec<u32>>], p: &[Vec3], k: f32) -> Vec<Vec3> {
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let own = p[v as usize];
            let nb: &[u32] = match &lines[v as usize] {
                None => mesh.neighbors(v),
                Some(l) if l.len() == 2 => l,
                Some(_) => return own,
            };
            if nb.is_empty() {
                return own;
            }
            let mean = nb.iter().map(|&n| p[n as usize]).sum::<Vec3>() / nb.len() as f32;
            own + (mean - own) * k
        })
        .collect()
}

/// Deformation › Smart ReSym (ZBrush): make the shown level symmetric across the plane
/// through the origin perpendicular to `axis` (0 X, 1 Y, 2 Z), the positive side copied
/// onto the negative one (the other way with `from_negative`). Each vertex on the side
/// being replaced goes to the mirror image of the nearest point of the other side's
/// surface facing the same way, so the two sides need not have matching vertices, and the
/// topology does not change. Vertices on the plane stay on it. Masks hold, as with the other
/// deformations.
pub fn smart_resym(mesh: &SculptMesh, axis: usize, from_negative: bool) -> Option<Vec<Vec3>> {
    let sign = if from_negative { -1.0 } else { 1.0 };
    let mirror = |mut q: Vec3| {
        q[axis] = -q[axis];
        q
    };
    let (lo, hi) = mesh
        .positions
        .par_iter()
        .fold(|| (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)))
        .reduce(|| (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |x, y| (x.0.min(y.0), x.1.max(y.1)));
    let tol = (hi - lo).max_element().max(1e-6) * 1e-5;
    // The kept side: triangles whose centre is on it (or on the plane).
    let p = &mesh.positions;
    let tri: Vec<[u32; 3]> = mesh
        .indices
        .chunks_exact(3)
        .filter(|t| (p[t[0] as usize][axis] + p[t[1] as usize][axis] + p[t[2] as usize][axis]) * sign >= -3.0 * tol)
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    if tri.is_empty() {
        return None;
    }
    let kept = crate::project_all::Sources::from_triangles(p.clone(), tri);
    let moved: Vec<Vec3> = (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let q = p[v as usize];
            let side = q[axis] * sign;
            if side > tol {
                return q;
            }
            if side >= -tol {
                let mut on = q;
                on[axis] = 0.0;
                return on;
            }
            let image = mirror(q);
            let facing = mirror(mesh.normal_at(v));
            kept.closest(image, facing).or_else(|| kept.closest(image, Vec3::ZERO)).map_or(q, |(x, _)| mirror(x))
        })
        .collect();
    held(mesh, moved)
}

/// Smooth value noise in -1..1.
fn noise(p: Vec3) -> f32 {
    let hash = |i: glam::IVec3| -> f32 {
        let mut h = (i.x as u32).wrapping_mul(0x8da6_b343) ^ (i.y as u32).wrapping_mul(0xd816_3841) ^ (i.z as u32).wrapping_mul(0xcb1a_b31f);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h & 0xffff) as f32 / 32767.5 - 1.0
    };
    let i = p.floor();
    let f = p - i;
    let s = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let i = i.as_ivec3();
    let corner = |dx: i32, dy: i32, dz: i32| hash(i + glam::IVec3::new(dx, dy, dz));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), s.x);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), s.x);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), s.x);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), s.x);
    lerp(lerp(x00, x10, s.y), lerp(x01, x11, s.y), s.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(16, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn mean_radius(p: &[Vec3]) -> f32 {
        p.iter().map(|q| q.length()).sum::<f32>() / p.len() as f32
    }

    #[test]
    fn every_shape_changes_the_surface_and_respects_masks() {
        for kind in Deform::ALL.into_iter().filter(|k| !k.is_map()) {
            let mut m = sphere();
            assert!(shaped(&m, kind, 0.0).is_none());
            let out = shaped(&m, kind, 60.0).unwrap_or_else(|| panic!("{kind:?} moved nothing"));
            assert!(out.iter().all(|p| p.is_finite()), "{kind:?}");
            m.mask = vec![255; m.vertex_count()];
            assert!(shaped(&m, kind, 60.0).is_none(), "{kind:?} moved masked clay");
        }
    }

    #[test]
    fn shapes_do_what_their_names_say() {
        let m = sphere();
        let r = mean_radius(&m.positions);
        assert!(mean_radius(&shaped(&m, Deform::Inflate, 50.0).unwrap()) > r);
        assert!(mean_radius(&shaped(&m, Deform::Inflate, -50.0).unwrap()) < r);
        // Smooth shrinks a sphere; Polish keeps it nearly as large.
        let (smooth, polish) = (mean_radius(&shaped(&m, Deform::Smooth, 100.0).unwrap()), mean_radius(&shaped(&m, Deform::Polish, 100.0).unwrap()));
        assert!(smooth < r * 0.99 && (polish - r).abs() < (smooth - r).abs() * 0.5, "{r} {smooth} {polish}");
        // A cube spherizes toward a round shape.
        let (p, i, _) = crate::primitive::Settings { kind: crate::primitive::Primitive::Cube, ..Default::default() }.build(1.0);
        let cube = SculptMesh::from_raw(p, i);
        let spread = |p: &[Vec3]| {
            let r = mean_radius(p);
            p.iter().map(|q| (q.length() - r).abs()).fold(0.0, f32::max) / r
        };
        assert!(spread(&shaped(&cube, Deform::Spherize, 100.0).unwrap()) < 0.01);
        // Twist turns the top one way and the bottom the other; Taper widens the top.
        let twisted = shaped(&m, Deform::Twist, 50.0).unwrap();
        let top = (0..m.vertex_count()).max_by(|&a, &b| (m.positions[a].y + m.positions[a].x * 0.01).total_cmp(&(m.positions[b].y + m.positions[b].x * 0.01))).unwrap();
        let ring = (0..m.vertex_count()).find(|&v| m.positions[v].y > 0.6 && m.positions[v].x > 0.5).unwrap();
        assert!((twisted[ring].y - m.positions[ring].y).abs() < 1e-6 && twisted[ring].distance(m.positions[ring]) > 0.05);
        assert!(twisted[top].distance(m.positions[top]) < 0.05);
        let tapered = shaped(&m, Deform::Taper, 60.0).unwrap();
        assert!(tapered[ring].truncate().length() > m.positions[ring].truncate().length());
        // Noise is deterministic.
        assert_eq!(shaped(&m, Deform::Noise, 40.0), shaped(&m, Deform::Noise, 40.0));
    }

    /// A cube with one polygroup per face.
    fn grouped_cube() -> SculptMesh {
        let (p, i) = crate::primitive::box_surface([6; 3], |p| p);
        let mut m = SculptMesh::from_raw(p, i);
        let groups: Vec<u16> = m
            .indices
            .chunks_exact(3)
            .map(|t| {
                let c = (m.positions[t[0] as usize] + m.positions[t[1] as usize] + m.positions[t[2] as usize]) / 3.0;
                let a = c.abs();
                let axis = if a.x >= a.y && a.x >= a.z { 0 } else if a.y >= a.z { 1 } else { 2 };
                (2 * axis + (c[axis] > 0.0) as usize) as u16
            })
            .collect();
        m.set_groups(groups);
        m
    }

    #[test]
    fn polish_by_features_keeps_group_borders_and_corners_crisp() {
        let m = grouped_cube();
        let half = m.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max);
        let on_edge = |p: Vec3| [p.x, p.y, p.z].iter().filter(|c| (c.abs() - half).abs() < 1e-5).count() >= 2;
        let corner = |p: Vec3| [p.x, p.y, p.z].iter().all(|c| (c.abs() - half).abs() < 1e-5);
        let out = shaped(&m, Deform::PolishFeatures, 100.0).unwrap();
        for (a, b) in m.positions.iter().zip(&out) {
            if corner(*a) {
                assert!(a.distance(*b) < 1e-6, "a corner moved");
            } else if on_edge(*a) {
                assert!(on_edge(*b), "an edge vertex left its edge: {a} -> {b}");
            }
        }
        // Plain Polish rounds the same cube's corners off.
        let plain = shaped(&m, Deform::Polish, 100.0).unwrap();
        assert!(m.positions.iter().zip(&plain).any(|(a, b)| corner(*a) && a.distance(*b) > 0.01));
    }

    #[test]
    fn smart_resym_mirrors_one_side_onto_the_other_without_matching_vertices() {
        // A sphere turned a little, so no vertex has a mirror partner, with a bump on +X only.
        let mut m = sphere();
        let turn = glam::Quat::from_rotation_y(0.21) * glam::Quat::from_rotation_z(0.13);
        let bumped: Vec<Vec3> = m.positions.iter().map(|&p| {
            let p = turn * p;
            p * (1.0 + 0.2 * (-(p - Vec3::new(0.8, 0.3, 0.3)).length_squared() * 12.0).exp())
        }).collect();
        m.positions = bumped;
        let out = smart_resym(&m, 0, false).unwrap();
        // The +X side is untouched; every −X vertex lies on the mirror of the +X surface.
        let kept: Vec<[u32; 3]> = m.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        let surface = crate::project_all::Sources::from_triangles(m.positions.clone(), kept);
        for (a, b) in m.positions.iter().zip(&out) {
            if a.x > 1e-4 {
                assert_eq!(a, b);
            } else {
                let image = Vec3::new(-b.x, b.y, b.z);
                let (x, _) = surface.closest(image, Vec3::ZERO).unwrap();
                assert!(x.distance(image) < 1e-4, "{a} -> {b}, {}", x.distance(image));
            }
        }
        // The bump now shows on −X too.
        let r = |p: &[Vec3]| p.iter().filter(|q| q.x < -0.5).map(|q| q.length()).fold(0.0, f32::max);
        assert!(r(&out) > r(&m.positions) + 0.1, "{} {}", r(&out), r(&m.positions));
        // From −X instead keeps −X.
        let back = smart_resym(&m, 0, true).unwrap();
        assert!(m.positions.iter().zip(&back).all(|(a, b)| a.x > -1e-4 || a == b));
    }
}
