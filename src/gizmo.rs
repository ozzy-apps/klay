//! Gizmo 3D (ZBrush: Y in Move / Scale / Rotate mode): a manipulator at the centre of the
//! active subtool, or wherever it was put. Its arrows move along its X, Y and Z, the boxes
//! on them squash or stretch along that axis, its rings turn about them, the outer ring
//! turns about the view axis, the white circle moves freely in the view plane and the
//! yellow square in the middle scales evenly. Sizes are in screen pixels so it stays the
//! same size as you zoom; a drag gives an [`Affine`] that Transpose previews and applies.
//!
//! Where it stands and how it is turned is a [`Place`]: Alt + click on the model puts it
//! there, turned to the surface (its Y along the normal), and Alt + dragging a handle moves
//! or turns the gizmo alone (ZBrush).

use crate::transform::{Affine, View};
use glam::{Quat, Vec2, Vec3};

/// Screen sizes, pixels.
pub const ARROW: f32 = 95.0;
pub const RING: f32 = 70.0;
pub const VIEW_RING: f32 = 108.0;
pub const SCALE_BOX: f32 = 7.0;
/// The axis scale boxes: half size and distance from the centre, pixels.
pub const AXIS_BOX: f32 = 4.5;
pub const AXIS_BOX_AT: f32 = 48.0;
pub const FREE_RING: f32 = 17.0;
/// How near the cursor must be to a handle, pixels.
const GRAB: f32 = 7.0;

pub const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

/// Where the gizmo stands and how it is turned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub centre: Vec3,
    pub rotation: Quat,
}

impl Place {
    pub fn at(centre: Vec3) -> Self {
        Self { centre, rotation: Quat::IDENTITY }
    }

    /// Its axis `a` in the world.
    pub fn axis(&self, a: usize) -> Vec3 {
        self.rotation * AXES[a]
    }

    /// Where it stands after the model under it is mapped by `m`: carried along and turned
    /// with it.
    pub fn mapped(&self, m: &Affine) -> Self {
        Self { centre: m.apply(self.centre), rotation: (m.rotation * self.rotation).normalize() }
    }

    /// On the surface at `point`, its Y along `normal` and its X as near the world's as it
    /// can be (ZBrush: Alt + click on the model).
    pub fn on_surface(point: Vec3, normal: Vec3) -> Self {
        let y = normal.normalize_or(Vec3::Y);
        Self { centre: point, rotation: Quat::from_rotation_arc(Vec3::Y, y) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Move along axis 0 (X), 1 (Y) or 2 (Z).
    Move(usize),
    /// Squash or stretch along axis 0, 1 or 2.
    Stretch(usize),
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
pub fn ring(view: &View, g: &Place, axis: Option<usize>, px: f32) -> Vec<Vec2> {
    let centre = g.centre;
    let r = px * pixel(view, centre);
    let n = match axis {
        Some(a) => g.axis(a),
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
pub fn arrow_tip(view: &View, g: &Place, a: usize) -> Vec2 {
    view.project(g.centre + g.axis(a) * ARROW * pixel(view, g.centre))
}

/// Screen centre of the scale box on axis `a`.
pub fn axis_box(view: &View, g: &Place, a: usize) -> Vec2 {
    view.project(g.centre + g.axis(a) * AXIS_BOX_AT * pixel(view, g.centre))
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
pub fn hit(view: &View, g: &Place, cursor: Vec2) -> Option<Handle> {
    let c = view.project(g.centre);
    let d = cursor.distance(c);
    if d <= SCALE_BOX + 2.0 {
        return Some(Handle::Scale);
    }
    let boxes = (0..3)
        .map(|a| (a, (cursor - axis_box(view, g, a)).abs().max_element()))
        .filter(|(_, dist)| *dist <= AXIS_BOX + 3.0)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((a, _)) = boxes {
        return Some(Handle::Stretch(a));
    }
    if (d - FREE_RING).abs() <= GRAB * 0.8 {
        return Some(Handle::Free);
    }
    let arrow = (0..3)
        .map(|a| (a, segment_distance(cursor, c, arrow_tip(view, g, a))))
        .filter(|(_, dist)| *dist <= GRAB)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((a, _)) = arrow {
        if d > FREE_RING + GRAB * 0.8 {
            return Some(Handle::Move(a));
        }
    }
    let rings = (0..3)
        .map(|a| (a, polyline_distance(cursor, &ring(view, g, Some(a), RING))))
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
pub fn drag_map(view: &View, g: &Place, handle: Handle, press: Vec2, to: Vec2) -> Affine {
    let centre = g.centre;
    let c = view.project(centre);
    match handle {
        Handle::Move(a) => {
            // Along the axis' screen direction, at its screen speed.
            // Screen pixels per world unit along the axis, measured over the arrow's length.
            let len = ARROW * pixel(view, centre);
            let s = (view.project(centre + g.axis(a) * len) - c) / len;
            if s.length() < 1e-3 {
                return Affine::IDENTITY;
            }
            let t = (to - press).dot(s) / s.length_squared();
            Affine::translation(g.axis(a) * t)
        }
        Handle::Stretch(a) => {
            // Out along the axis' screen direction stretches, in squashes: twice or half
            // per 100 pixels.
            let dir = (axis_box(view, g, a) - c).normalize_or_zero();
            if dir == Vec2::ZERO {
                return Affine::IDENTITY;
            }
            let k = ((to - press).dot(dir) / 100.0).exp2().clamp(crate::transform::MIN_SCALE, crate::transform::MAX_SCALE);
            Affine::stretching(centre, g.axis(a), k)
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
            let axis = g.axis(a);
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
        for g in [Place::at(Vec3::ZERO), Place { centre: Vec3::new(0.1, 0.2, 0.0), rotation: Quat::from_euler(glam::EulerRot::XYZ, 0.4, -0.3, 0.2) }] {
            let c = &g;
            let centre = v.project(g.centre);
            assert_eq!(hit(&v, c, centre), Some(Handle::Scale));
            assert_eq!(hit(&v, c, centre + Vec2::new(FREE_RING, 0.0)), Some(Handle::Free));
            for a in 0..3 {
                let tip = arrow_tip(&v, c, a);
                assert_eq!(hit(&v, c, centre + (tip - centre) * 0.85), Some(Handle::Move(a)), "arrow {a}");
                assert_eq!(hit(&v, c, axis_box(&v, c, a)), Some(Handle::Stretch(a)), "box {a}");
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
    }

    #[test]
    fn drags_move_turn_and_scale_as_the_handles_say() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        let v = view(&cam);
        let c = Vec3::new(0.2, 0.1, 0.0);
        let g = Place::at(c);
        let centre = v.project(c);
        // Dragging the X arrow by its own screen length moves by its world length.
        let tip = arrow_tip(&v, &g, 0);
        let m = drag_map(&v, &g, Handle::Move(0), tip, tip + (tip - centre));
        let world = ARROW * cam.pixel_size_at(c, 1000.0);
        assert!((m.translate - Vec3::X * world).length() < world * 0.05, "{:?} vs {world}", m.translate);
        // Up-right doubles the size.
        let m = drag_map(&v, &g, Handle::Scale, centre, centre + Vec2::new(100.0, -100.0));
        assert!((m.scale - 2.0).abs() < 1e-4 && (m.apply(c) - c).length() < 1e-5);
        // A Y ring drag turns about Y only, and the centre stays put.
        let on_ring = ring(&v, &g, Some(1), RING)[16];
        let m = drag_map(&v, &g, Handle::Rotate(1), on_ring, on_ring + Vec2::new(30.0, 0.0));
        let (axis, angle) = m.rotation.to_axis_angle();
        assert!(angle.abs() > 0.05 && axis.cross(Vec3::Y).length() < 1e-3, "{axis:?} {angle}");
        assert!((m.apply(c) - c).length() < 1e-5);
        // The Y box dragged inward 100 pixels squashes Y to half, X and Z kept.
        let b = axis_box(&v, &g, 1);
        let m = drag_map(&v, &g, Handle::Stretch(1), b, b + (centre - b).normalize() * 100.0);
        assert!((m.apply(c + Vec3::Y) - (c + Vec3::Y * 0.5)).length() < 1e-4, "{:?}", m.apply(c + Vec3::Y));
        assert!((m.apply(c + Vec3::X) - (c + Vec3::X)).length() < 1e-5);
        // A turned gizmo moves along its own axes, and follows the map it made.
        let turned = Place { centre: c, rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2) };
        let tip = arrow_tip(&v, &turned, 0);
        let m = drag_map(&v, &turned, Handle::Move(0), tip, tip + (tip - centre));
        assert!(m.translate.normalize().dot(Vec3::Y) > 0.999, "{:?}", m.translate);
        let spun = Affine::rotation_about(c, Quat::from_rotation_x(0.5));
        assert!((g.mapped(&spun).axis(1) - Quat::from_rotation_x(0.5) * Vec3::Y).length() < 1e-5);
        // On the surface: Y along the normal.
        let n = Vec3::new(1.0, 1.0, 0.0).normalize();
        assert!((Place::on_surface(c, n).axis(1) - n).length() < 1e-5);
    }
}
