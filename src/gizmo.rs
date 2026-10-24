//! Gizmo 3D (ZBrush: Y in Move / Scale / Rotate mode): a manipulator at the centre of the
//! active subtool. Its arrows move along X, Y and Z, its rings turn about them, the outer
//! ring turns about the view axis, the white circle moves freely in the view plane and the
//! yellow square in the middle scales evenly. Sizes are in screen pixels so it stays the
//! same size as you zoom; a drag gives an [`Affine`] that Transpose previews and applies.

use crate::transform::{Affine, View};
use glam::{Quat, Vec2, Vec3};

/// Screen sizes, pixels.
pub const ARROW: f32 = 95.0;
pub const RING: f32 = 70.0;
pub const VIEW_RING: f32 = 108.0;
pub const SCALE_BOX: f32 = 7.0;
pub const FREE_RING: f32 = 17.0;
/// How near the cursor must be to a handle, pixels.
const GRAB: f32 = 7.0;

pub const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Move along axis 0 (X), 1 (Y) or 2 (Z).
    Move(usize),
    /// Turn about axis 0, 1 or 2.
    Rotate(usize),
    /// Turn about the view axis.
    RotateView,
    /// Move in the view plane.
    Free,
    /// Scale evenly.
    Scale,
}

/// World size of one screen pixel at the gizmo's centre.
fn pixel(view: &View, centre: Vec3) -> f32 {
    view.camera.pixel_size_at(centre, view.size.y)
}

/// Screen polyline of the ring about axis `a` (or the view axis), radius `px` pixels.
pub fn ring(view: &View, centre: Vec3, axis: Option<usize>, px: f32) -> Vec<Vec2> {
    let r = px * pixel(view, centre);
    let n = match axis {
        Some(a) => AXES[a],
        None => -view.camera.forward(),
    };
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    (0..=64)
        .map(|k| {
            let t = std::f32::consts::TAU * k as f32 / 64.0;
            view.project(centre + (u * t.cos() + v * t.sin()) * r)
        })
        .collect()
}

/// Screen tip of the arrow for axis `a`.
pub fn arrow_tip(view: &View, centre: Vec3, a: usize) -> Vec2 {
    view.project(centre + AXES[a] * ARROW * pixel(view, centre))
}

fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn polyline_distance(p: Vec2, line: &[Vec2]) -> f32 {
    line.windows(2).map(|w| segment_distance(p, w[0], w[1])).fold(f32::INFINITY, f32::min)
}

/// The handle under `cursor` (canvas pixels), if any. Inner handles win.
pub fn hit(view: &View, centre: Vec3, cursor: Vec2) -> Option<Handle> {
    let c = view.project(centre);
    let d = cursor.distance(c);
    if d <= SCALE_BOX + 2.0 {
        return Some(Handle::Scale);
    }
    if (d - FREE_RING).abs() <= GRAB * 0.8 {
        return Some(Handle::Free);
    }
    let arrow = (0..3)
        .map(|a| (a, segment_distance(cursor, c, arrow_tip(view, centre, a))))
        .filter(|(_, dist)| *dist <= GRAB)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((a, _)) = arrow {
        if d > FREE_RING + GRAB * 0.8 {
            return Some(Handle::Move(a));
        }
    }
    let rings = (0..3)
        .map(|a| (a, polyline_distance(cursor, &ring(view, centre, Some(a), RING))))
        .filter(|(_, dist)| *dist <= GRAB)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((a, _)) = rings {
        return Some(Handle::Rotate(a));
    }
    if (d - VIEW_RING).abs() <= GRAB {
        return Some(Handle::RotateView);
    }
    None
}

/// The map a drag of `handle` from `press` to `to` (canvas pixels) makes.
pub fn drag_map(view: &View, centre: Vec3, handle: Handle, press: Vec2, to: Vec2) -> Affine {
    let c = view.project(centre);
    match handle {
        Handle::Move(a) => {
            // Along the axis' screen direction, at its screen speed.
            // Screen pixels per world unit along the axis, measured over the arrow's length.
            let len = ARROW * pixel(view, centre);
            let s = (view.project(centre + AXES[a] * len) - c) / len;
            if s.length() < 1e-3 {
                return Affine::IDENTITY;
            }
            let t = (to - press).dot(s) / s.length_squared();
            Affine::translation(AXES[a] * t)
        }
        Handle::Free => crate::transform::move_map(view, centre, press, to),
        Handle::Scale => {
            // Up and right grow, down and left shrink: twice the size per 200 pixels.
            let d = to - press;
            let k = ((d.x - d.y) / 200.0).exp2().clamp(crate::transform::MIN_SCALE, crate::transform::MAX_SCALE);
            Affine::scaling(centre, k)
        }
        Handle::RotateView => crate::transform::rotate_map(view, centre, press, to),
        Handle::Rotate(a) => {
            // Along the ring where it was grabbed: the drag along the ring's screen tangent
            // there, over the ring's screen speed per radian.
            let axis = AXES[a];
            let r = RING * pixel(view, centre);
            let w = view.unproject_at(press, centre) - centre;
            let w = (w - axis * w.dot(axis)).normalize_or(axis.any_orthonormal_vector());
            let q = centre + w * r;
            let eps = 0.01;
            let step = view.project(q + axis.cross(w) * r * eps) - view.project(q);
            let speed = step.length() / eps;
            if speed < 1.0 {
                return Affine::IDENTITY;
            }
            let angle = (to - press).dot(step.normalize()) / speed;
            Affine::rotation_about(centre, Quat::from_axis_angle(axis, angle))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn view(camera: &Camera) -> View<'_> {
        View { camera, size: Vec2::new(1600.0, 1000.0) }
    }

    #[test]
    fn handles_are_found_where_they_are_drawn() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        cam.yaw = 0.5;
        cam.pitch = 0.3;
        let v = view(&cam);
        let c = Vec3::ZERO;
        let centre = v.project(c);
        assert_eq!(hit(&v, c, centre), Some(Handle::Scale));
        assert_eq!(hit(&v, c, centre + Vec2::new(FREE_RING, 0.0)), Some(Handle::Free));
        for a in 0..3 {
            let tip = arrow_tip(&v, c, a);
            assert_eq!(hit(&v, c, centre + (tip - centre) * 0.85), Some(Handle::Move(a)), "arrow {a}");
        }
        // A point on a ring away from the arrows.
        for a in 0..3 {
            let pts = ring(&v, c, Some(a), RING);
            let found = pts.iter().any(|&p| hit(&v, c, p) == Some(Handle::Rotate(a)));
            assert!(found, "ring {a}");
        }
        assert_eq!(hit(&v, c, centre + Vec2::new(0.0, -VIEW_RING)), Some(Handle::RotateView));
        assert_eq!(hit(&v, c, centre + Vec2::new(300.0, 300.0)), None);
    }

    #[test]
    fn drags_move_turn_and_scale_as_the_handles_say() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        let v = view(&cam);
        let c = Vec3::new(0.2, 0.1, 0.0);
        let centre = v.project(c);
        // Dragging the X arrow by its own screen length moves by its world length.
        let tip = arrow_tip(&v, c, 0);
        let m = drag_map(&v, c, Handle::Move(0), tip, tip + (tip - centre));
        let world = ARROW * cam.pixel_size_at(c, 1000.0);
        assert!((m.translate - Vec3::X * world).length() < world * 0.05, "{:?} vs {world}", m.translate);
        // Up-right doubles the size.
        let m = drag_map(&v, c, Handle::Scale, centre, centre + Vec2::new(100.0, -100.0));
        assert!((m.scale - 2.0).abs() < 1e-4 && (m.apply(c) - c).length() < 1e-5);
        // A Y ring drag turns about Y only, and the centre stays put.
        let on_ring = ring(&v, c, Some(1), RING)[16];
        let m = drag_map(&v, c, Handle::Rotate(1), on_ring, on_ring + Vec2::new(30.0, 0.0));
        let (axis, angle) = m.rotation.to_axis_angle();
        assert!(angle.abs() > 0.05 && axis.cross(Vec3::Y).length() < 1e-3, "{axis:?} {angle}");
        assert!((m.apply(c) - c).length() < 1e-5);
    }
}
