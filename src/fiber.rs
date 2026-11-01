//! FiberMesh (ZBrush: Tool › FiberMesh): fibres grown from the surface, for hair, fur, grass
//! and bristles. They are previewed on the model and become a subtool of their own with
//! **Accept**, to sculpt (groom) like any other.
//!
//! Roots are spread over the masked part of the model (all of the visible part when nothing
//! is masked) by area, with a fixed seed so the same settings give the same fibres. Each
//! fibre leaves along the surface normal and bends under **Gravity** and **Curl** over its
//! **Segments**; it is a tube of **Sides** sides tapering from **Base** to **Tip** width, or
//! a flat ribbon facing the view's side with one side. Lengths vary by **Length variation**.

use crate::mesh::SculptMesh;
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// How many fibres.
    pub count: u32,
    /// Length as a share of the model's size.
    pub length: f32,
    /// How much shorter a fibre may be, 0 to 1.
    pub length_variation: f32,
    /// Segments along a fibre (1 to 16).
    pub segments: u32,
    /// Pull downward (-1 up to 1 down): fibres droop with distance from the root.
    pub gravity: f32,
    /// Turns a fibre makes about its root's normal over its length.
    pub curl: f32,
    /// Widths at the root and the tip, as shares of the length.
    pub base_width: f32,
    pub tip_width: f32,
    /// 1 for a flat ribbon, 3 to 8 for a tube.
    pub sides: u32,
    /// Random lean of each fibre away from the normal, 0 to 1.
    pub spread: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { count: 2000, length: 0.12, length_variation: 0.3, segments: 6, gravity: 0.3, curl: 0.0, base_width: 0.04, tip_width: 0.0, sides: 4, spread: 0.2 }
    }
}

/// The largest fibre count offered.
pub const MAX_FIBERS: u32 = 200_000;

fn hash(mut x: u32) -> f32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x & 0xff_ffff) as f32 / 16_777_216.0
}

/// The fibres grown on `mesh` as `s` says, one polygroup per fibre (wrapping after 4096),
/// or `None` when there is no surface to grow on.
pub fn grow(mesh: &SculptMesh, s: &Settings) -> Option<SculptMesh> {
    let masked = mesh.mask.iter().any(|&m| m > 127);
    let tris: Vec<u32> = (0..mesh.triangle_count() as u32)
        .filter(|&t| mesh.visible(t))
        .filter(|&t| !masked || mesh.indices[3 * t as usize..3 * t as usize + 3].iter().all(|&v| mesh.mask[v as usize] > 127))
        .collect();
    let p = &mesh.positions;
    let corner = |t: u32, k: usize| mesh.indices[3 * t as usize + k];
    let area = |t: u32| (p[corner(t, 1) as usize] - p[corner(t, 0) as usize]).cross(p[corner(t, 2) as usize] - p[corner(t, 0) as usize]).length() * 0.5;
    let mut cumulative = Vec::with_capacity(tris.len());
    let mut total = 0.0f64;
    for &t in &tris {
        total += area(t) as f64;
        cumulative.push(total);
    }
    if total <= 0.0 {
        return None;
    }
    let (lo, hi) = p.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &q| (a.min(q), b.max(q)));
    let size = (hi - lo).max_element().max(1e-6);
    let segments = s.segments.clamp(1, 16) as usize;
    let sides = if s.sides <= 1 { 1 } else { s.sides.clamp(3, 8) as usize };
    let count = s.count.clamp(1, MAX_FIBERS);
    let (mut out_p, mut out_i, mut groups) = (Vec::new(), Vec::new(), Vec::new());
    for f in 0..count {
        // A root: a triangle by area, a point in it.
        let r = hash(f.wrapping_mul(3)) as f64 * total;
        let k = cumulative.partition_point(|&c| c < r).min(tris.len() - 1);
        let t = tris[k];
        let (mut a, mut b) = (hash(f.wrapping_mul(3) + 1), hash(f.wrapping_mul(3) + 2));
        if a + b > 1.0 {
            (a, b) = (1.0 - a, 1.0 - b);
        }
        let w = [1.0 - a - b, a, b];
        let root: Vec3 = (0..3).map(|k| p[corner(t, k) as usize] * w[k]).sum();
        let normal = (0..3).map(|k| mesh.normal_at(corner(t, k)) * w[k]).sum::<Vec3>().normalize_or(Vec3::Y);
        let len = s.length * size * (1.0 - s.length_variation.clamp(0.0, 1.0) * hash(f ^ 0xa5a5_a5a5));
        // Leaning a little at random, then bending.
        let t1 = normal.any_orthonormal_vector();
        let t2 = normal.cross(t1);
        let phi = hash(f ^ 0x3c3c) * std::f32::consts::TAU;
        let lean = (t1 * phi.cos() + t2 * phi.sin()) * s.spread.clamp(0.0, 1.0);
        let mut dir = (normal + lean).normalize();
        let step = len / segments as f32;
        let mut points = vec![root];
        let mut tangents = vec![dir];
        for k in 1..=segments {
            let u = k as f32 / segments as f32;
            let curl = if s.curl != 0.0 {
                let angle = s.curl * std::f32::consts::TAU / segments as f32;
                glam::Quat::from_axis_angle(normal, angle)
            } else {
                glam::Quat::IDENTITY
            };
            dir = (curl * dir + Vec3::NEG_Y * (s.gravity * 1.5 * u / segments as f32)).normalize();
            points.push(points[k - 1] + dir * step);
            tangents.push(dir);
        }
        let width = |k: usize| {
            let u = k as f32 / segments as f32;
            len * (s.base_width + (s.tip_width - s.base_width) * u)
        };
        let base = out_p.len() as u32;
        let group = (f % 4096) as u16;
        if sides == 1 {
            // A ribbon, flat across the lean's side.
            for (k, (q, tg)) in points.iter().zip(&tangents).enumerate() {
                let across = tg.cross(t2).normalize_or(t1) * (width(k) * 0.5).max(len * 1e-3);
                out_p.extend_from_slice(&[*q - across, *q + across]);
            }
            for k in 0..segments as u32 {
                let (a0, a1, b0, b1) = (base + 2 * k, base + 2 * k + 1, base + 2 * k + 2, base + 2 * k + 3);
                out_i.extend_from_slice(&[a0, a1, b1, a0, b1, b0]);
                groups.extend_from_slice(&[group, group]);
            }
        } else {
            // A tube: a ring per point, the base capped, a tip point when it narrows to
            // nothing.
            let pointy = s.tip_width <= 0.0;
            let rings = if pointy { segments } else { segments + 1 };
            for k in 0..rings {
                let tg = tangents[k];
                let u = tg.any_orthonormal_vector();
                let v = tg.cross(u);
                let r = (width(k) * 0.5).max(len * 1e-3);
                for j in 0..sides {
                    let a = j as f32 / sides as f32 * std::f32::consts::TAU;
                    out_p.push(points[k] + (u * a.cos() + v * a.sin()) * r);
                }
            }
            let sides = sides as u32;
            for k in 0..rings as u32 - 1 {
                for j in 0..sides {
                    let (a0, a1) = (base + k * sides + j, base + k * sides + (j + 1) % sides);
                    let (b0, b1) = (a0 + sides, a1 + sides);
                    out_i.extend_from_slice(&[a0, a1, b1, a0, b1, b0]);
                    groups.extend_from_slice(&[group, group]);
                }
            }
            let last = base + (rings as u32 - 1) * sides;
            let tip = out_p.len() as u32;
            out_p.push(if pointy { points[segments] } else { points[segments] + tangents[segments] * 1e-4 });
            for j in 0..sides {
                out_i.extend_from_slice(&[last + j, last + (j + 1) % sides, tip]);
                groups.push(group);
            }
            let cap = out_p.len() as u32;
            out_p.push(root);
            for j in 0..sides {
                out_i.extend_from_slice(&[base + (j + 1) % sides, base + j, cap]);
                groups.push(group);
            }
        }
    }
    Some(SculptMesh::from_raw_with_groups(out_p, out_i, groups))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(12, 1.0);
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn tubes_are_closed_and_grow_outward_on_the_mask_only() {
        let mut m = sphere();
        // Mask the top only: every root is up there.
        m.mask = m.positions.iter().map(|p| if p.y > 0.5 { 255 } else { 0 }).collect();
        let s = Settings { count: 300, gravity: 0.0, spread: 0.0, ..Default::default() };
        let f = grow(&m, &s).unwrap();
        assert!(crate::wedge::closed(&f), "each tube is capped and pointed");
        let per = (s.segments as usize) * 4 + 2;
        assert_eq!(f.vertex_count(), 300 * per);
        // (Meshes keep their vertices in chunk order, so look at all of them.) Every
        // vertex is on or outside the sphere, near the masked top, and the fibres reach out.
        assert!(f.positions.iter().all(|q| q.length() > 0.97 && q.y > 0.4));
        assert!(f.positions.iter().map(|q| q.length()).fold(0.0, f32::max) > 1.15);
        // The same settings give the same fibres.
        assert_eq!(grow(&m, &s).unwrap().positions, f.positions);
    }

    #[test]
    fn gravity_droops_and_ribbons_are_flat_strips() {
        let m = sphere();
        let up = Settings { count: 50, gravity: 0.0, spread: 0.0, ..Default::default() };
        let down = Settings { gravity: 1.0, ..up };
        let mean_y = |f: &SculptMesh| f.positions.iter().map(|q| q.y).sum::<f32>() / f.vertex_count() as f32;
        assert!(mean_y(&grow(&m, &down).unwrap()) < mean_y(&grow(&m, &up).unwrap()));
        let ribbon = grow(&m, &Settings { sides: 1, ..up }).unwrap();
        assert_eq!(ribbon.vertex_count(), 50 * 2 * (up.segments as usize + 1));
    }
}
