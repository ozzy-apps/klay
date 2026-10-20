//! Moving, scaling and rotating a whole subtool: the map itself and the ZBrush Transpose
//! action line that drives it.
//!
//! In Move / Scale / Rotate mode (W / E / R) a left drag on the active subtool draws the
//! action line: a start circle on the surface under the press, a middle circle and an end
//! circle. Dragging inside a circle then acts on the subtool:
//!
//! - Move: the middle circle moves the subtool in the view plane.
//! - Scale: the end circle scales about the start by the ratio of the cursor's distance to
//!   the start over the press's (the start circle scales about the end).
//! - Rotate: the end circle rotates about the start around the view axis by the angle the
//!   cursor turns through (the start circle rotates about the end).
//!
//! Dragging a circle's outer ring moves that end of the line, dragging the line between the
//! circles moves the whole line; neither touches the model.

use crate::camera::Camera;
use glam::{Mat4, Quat, Vec2, Vec3};

/// The map `p -> center + scale * rotation (p - center) + translate`: a rotation and a
/// uniform scale about `center`, then a translation. Loop subdivision weights sum to one, so
/// subdivision commutes with it: every level of a stack maps exactly by mapping positions
/// and turning the offsets by `scale * rotation`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub center: Vec3,
    pub rotation: Quat,
    pub scale: f32,
    pub translate: Vec3,
}

impl Affine {
    pub const IDENTITY: Self = Self {
        center: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: 1.0,
        translate: Vec3::ZERO,
    };

    pub fn translation(t: Vec3) -> Self {
        Self {
            translate: t,
            ..Self::IDENTITY
        }
    }

    pub fn scaling(center: Vec3, scale: f32) -> Self {
        Self {
            center,
            scale,
            ..Self::IDENTITY
        }
    }

    pub fn rotation_about(center: Vec3, rotation: Quat) -> Self {
        Self {
            center,
            rotation,
            ..Self::IDENTITY
        }
    }

    pub fn apply(&self, p: Vec3) -> Vec3 {
        self.center + self.apply_vector(p - self.center) + self.translate
    }

    /// The linear part: how offsets and other directions map.
    pub fn apply_vector(&self, v: Vec3) -> Vec3 {
        if self.rotation == Quat::IDENTITY {
            v * self.scale
        } else {
            (self.rotation * v) * self.scale
        }
    }

    /// True when the linear part is the identity (a pure translation or nothing).
    pub fn is_translation(&self) -> bool {
        self.scale == 1.0 && self.rotation == Quat::IDENTITY
    }

    pub fn is_identity(&self) -> bool {
        self.is_translation() && self.translate == Vec3::ZERO
    }

    /// The exact inverse, in the same form: `p = (c + t) + (1/s) R⁻¹ (p' - (c + t)) - t`.
    pub fn inverse(&self) -> Self {
        Self {
            center: self.center + self.translate,
            rotation: self.rotation.inverse(),
            scale: 1.0 / self.scale,
            translate: -self.translate,
        }
    }

    /// The same map as a matrix, for the vertex shader.
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_translation(self.center + self.translate)
            * Mat4::from_scale_rotation_translation(Vec3::splat(self.scale), self.rotation, Vec3::ZERO)
            * Mat4::from_translation(-self.center)
    }
}

/// Scale factors a single Scale drag can reach.
pub const MIN_SCALE: f32 = 0.01;
pub const MAX_SCALE: f32 = 100.0;

/// The camera as the canvas shows it: pixel coordinates with the origin at the canvas's
/// top-left corner.
pub struct View<'a> {
    pub camera: &'a Camera,
    pub size: Vec2,
}

impl View<'_> {
    pub fn project(&self, p: Vec3) -> Vec2 {
        let clip = self.camera.view_proj() * p.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        Vec2::new((ndc.x + 1.0) * 0.5 * self.size.x, (1.0 - ndc.y) * 0.5 * self.size.y)
    }

    pub fn ray(&self, px: Vec2) -> (Vec3, Vec3) {
        self.camera.ray(px.x, px.y, self.size.x, self.size.y)
    }

    /// The point under pixel `px` on the plane through `through` that faces the camera.
    pub fn unproject_at(&self, px: Vec2, through: Vec3) -> Vec3 {
        let (o, d) = self.ray(px);
        let f = self.camera.forward();
        let denom = d.dot(f);
        if denom.abs() < 1e-6 {
            return through;
        }
        o + d * ((through - o).dot(f) / denom)
    }
}

/// Move: the point under `from` on the view plane through `anchor` follows the cursor to `to`.
pub fn move_map(view: &View, anchor: Vec3, from: Vec2, to: Vec2) -> Affine {
    Affine::translation(view.unproject_at(to, anchor) - view.unproject_at(from, anchor))
}

/// Scale about `pivot` by how much farther from it (on screen) the cursor is than the press.
pub fn scale_map(view: &View, pivot: Vec3, from: Vec2, to: Vec2) -> Affine {
    let c = view.project(pivot);
    let (a, b) = ((from - c).length(), (to - c).length());
    if a < 1.0 {
        return Affine::IDENTITY;
    }
    Affine::scaling(pivot, (b / a).clamp(MIN_SCALE, MAX_SCALE))
}

/// Rotate about `pivot` around the line of sight through it, by the angle the cursor turned
/// through about the pivot's pixel, so the press point follows the cursor around.
pub fn rotate_map(view: &View, pivot: Vec3, from: Vec2, to: Vec2) -> Affine {
    let c = view.project(pivot);
    let (a, b) = (from - c, to - c);
    if a.length() < 1.0 || b.length() < 1.0 {
        return Affine::IDENTITY;
    }
    // Screen y points down, so a positive angle is clockwise on screen, which is a positive
    // turn about an axis pointing away from the eye.
    let angle = a.perp_dot(b).atan2(a.dot(b));
    let axis = view.ray(c).1;
    Affine::rotation_about(pivot, Quat::from_axis_angle(axis, angle))
}

/// Shift while drawing the line: snap its direction on screen to 45° steps.
pub fn snap_45(from: Vec2, to: Vec2) -> Vec2 {
    let d = to - from;
    let len = d.length();
    if len < 1e-3 {
        return to;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (d.y.atan2(d.x) / step).round() * step;
    from + Vec2::new(angle.cos(), angle.sin()) * len
}

/// The action line, in world space so it stays on the model while the view turns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionLine {
    pub start: Vec3,
    pub end: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Start,
    Middle,
    End,
}

impl ActionLine {
    pub fn mid(&self) -> Vec3 {
        (self.start + self.end) * 0.5
    }

    pub fn point(&self, part: Part) -> Vec3 {
        match part {
            Part::Start => self.start,
            Part::Middle => self.mid(),
            Part::End => self.end,
        }
    }

    pub fn mapped(&self, m: &Affine) -> Self {
        Self {
            start: m.apply(self.start),
            end: m.apply(self.end),
        }
    }
}

/// Radii of the circles drawn on the action line, in points: inside `INNER` acts on the
/// model, between `INNER` and `OUTER` grabs that end of the line.
pub const INNER: f32 = 7.0;
pub const OUTER: f32 = 13.0;
/// How close to the line (in points) a press grabs the line itself.
const LINE_GRAB: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Inside a circle: act on the model.
    Inner(Part),
    /// On a circle's outer ring: move that point of the line.
    Ring(Part),
    /// On the line between the circles: move the whole line.
    Line,
}

/// What a press at `cursor` (canvas pixels) grabs. `ppp` is pixels per point.
pub fn hit_test(view: &View, line: &ActionLine, cursor: Vec2, ppp: f32) -> Option<Hit> {
    let parts = [Part::Start, Part::Middle, Part::End];
    let (part, dist) = parts
        .iter()
        .map(|&p| (p, (view.project(line.point(p)) - cursor).length()))
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    if dist <= INNER * ppp {
        return Some(Hit::Inner(part));
    }
    if dist <= OUTER * ppp {
        return Some(Hit::Ring(part));
    }
    let (a, b) = (view.project(line.start), view.project(line.end));
    let ab = b - a;
    let t = ((cursor - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    ((a + ab * t - cursor).length() <= LINE_GRAB * ppp).then_some(Hit::Line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(ortho: bool) -> Camera {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        cam.ortho = ortho;
        cam
    }

    fn sample() -> Affine {
        Affine {
            center: Vec3::new(0.3, -0.2, 0.5),
            rotation: Quat::from_axis_angle(Vec3::new(1.0, 2.0, -0.5).normalize(), 0.9),
            scale: 1.7,
            translate: Vec3::new(-0.4, 1.1, 0.25),
        }
    }

    #[test]
    fn affine_inverse_and_matrix_agree_with_apply() {
        let m = sample();
        let inv = m.inverse();
        let mat = m.matrix();
        for p in [Vec3::ZERO, Vec3::new(1.0, -2.0, 3.0), Vec3::new(-0.7, 0.1, 0.4)] {
            let q = m.apply(p);
            assert!((inv.apply(q) - p).length() < 1e-5, "inverse");
            assert!((mat.transform_point3(p) - q).length() < 1e-5, "matrix");
            assert!((inv.inverse().apply(p) - q).length() < 1e-5, "inverse of inverse");
        }
        // Distances scale by `scale`, and the map is a similarity (angles kept).
        let (a, b, c) = (Vec3::X, Vec3::Y * 2.0, Vec3::new(0.5, 0.5, -1.0));
        let d0 = (b - a).length();
        let d1 = (m.apply(b) - m.apply(a)).length();
        assert!((d1 / d0 - 1.7).abs() < 1e-5);
        let cos0 = (b - a).normalize().dot((c - a).normalize());
        let cos1 = (m.apply(b) - m.apply(a)).normalize().dot((m.apply(c) - m.apply(a)).normalize());
        assert!((cos0 - cos1).abs() < 1e-5);
        assert!(Affine::IDENTITY.is_identity());
        assert!(Affine::translation(Vec3::X).is_translation());
        assert!(!m.is_translation());
    }

    #[test]
    fn move_follows_the_cursor_on_the_anchor_plane() {
        for ortho in [false, true] {
            let cam = camera(ortho);
            let view = View { camera: &cam, size: Vec2::new(1600.0, 1000.0) };
            let anchor = Vec3::new(0.2, 0.1, -0.3);
            let from = view.project(anchor);
            let to = from + Vec2::new(140.0, -60.0);
            let m = move_map(&view, anchor, from, to);
            assert!(m.is_translation());
            let got = view.project(m.apply(anchor));
            assert!((got - to).length() < 0.05, "ortho {ortho}: {got} vs {to}");
            // The move stays in the view plane.
            assert!(m.translate.dot(cam.forward()).abs() < 1e-5);
        }
    }

    #[test]
    fn scale_is_the_ratio_of_distances_to_the_pivot() {
        for ortho in [false, true] {
            let cam = camera(ortho);
            let view = View { camera: &cam, size: Vec2::new(1600.0, 1000.0) };
            let pivot = Vec3::new(0.0, 0.2, 0.1);
            let c = view.project(pivot);
            let m = scale_map(&view, pivot, c + Vec2::new(50.0, 0.0), c + Vec2::new(0.0, 125.0));
            assert!((m.scale - 2.5).abs() < 1e-4, "{}", m.scale);
            assert_eq!(m.apply(pivot), pivot);
            // Clamped, and a press on the pivot itself does nothing.
            let tiny = scale_map(&view, pivot, c + Vec2::X * 100.0, c);
            assert_eq!(tiny.scale, MIN_SCALE);
            assert!(scale_map(&view, pivot, c, c + Vec2::X * 30.0).is_identity());
        }
    }

    #[test]
    fn rotate_turns_the_press_point_towards_the_cursor() {
        let cam = camera(true);
        let view = View { camera: &cam, size: Vec2::new(1600.0, 1000.0) };
        let pivot = Vec3::new(0.1, 0.0, 0.2);
        let c = view.project(pivot);
        let from = c + Vec2::new(120.0, 0.0);
        let end = view.unproject_at(from, pivot);
        for degrees in [30.0f32, 90.0, -45.0, 170.0] {
            let a = degrees.to_radians();
            let to = c + Vec2::new(a.cos(), a.sin()) * 80.0;
            let m = rotate_map(&view, pivot, from, to);
            assert_eq!(m.scale, 1.0);
            assert!((m.apply(pivot) - pivot).length() < 1e-6);
            // In orthographic the press point lands exactly on the ray towards the cursor,
            // at its old distance.
            let got = view.project(m.apply(end)) - c;
            let want = Vec2::new(a.cos(), a.sin()) * 120.0;
            assert!((got - want).length() < 0.05, "{degrees}°: {got} vs {want}");
        }
        // In perspective the direction still follows the cursor.
        let cam = camera(false);
        let view = View { camera: &cam, size: Vec2::new(1600.0, 1000.0) };
        let c = view.project(pivot);
        let from = c + Vec2::new(120.0, 0.0);
        let end = view.unproject_at(from, pivot);
        let to = c + Vec2::new(0.0, 90.0);
        let got = (view.project(rotate_map(&view, pivot, from, to).apply(end)) - c).normalize();
        assert!(got.dot(Vec2::Y) > 0.999, "{got}");
    }

    #[test]
    fn line_hit_test_finds_circles_rings_and_the_line() {
        let cam = camera(false);
        let view = View { camera: &cam, size: Vec2::new(1600.0, 1000.0) };
        let start = view.unproject_at(Vec2::new(600.0, 500.0), Vec3::ZERO);
        let line = ActionLine {
            start,
            end: view.unproject_at(Vec2::new(900.0, 500.0), start),
        };
        let at = |x: f32, y: f32| hit_test(&view, &line, Vec2::new(x, y), 1.0);
        assert_eq!(at(602.0, 503.0), Some(Hit::Inner(Part::Start)));
        assert_eq!(at(750.0, 495.0), Some(Hit::Inner(Part::Middle)));
        assert_eq!(at(910.0, 500.0), Some(Hit::Ring(Part::End)));
        assert_eq!(at(680.0, 503.0), Some(Hit::Line));
        assert_eq!(at(680.0, 520.0), None);
        assert_eq!(at(1000.0, 500.0), None);
        // Mapping the line maps both ends.
        let moved = line.mapped(&Affine::translation(Vec3::Y));
        assert_eq!(moved.start, line.start + Vec3::Y);
        assert_eq!(moved.end, line.end + Vec3::Y);
    }

    #[test]
    fn shift_snaps_the_line_to_45_degrees() {
        let from = Vec2::new(100.0, 100.0);
        let got = snap_45(from, Vec2::new(200.0, 110.0));
        assert!((got.y - 100.0).abs() < 1e-3 && got.x > 199.0);
        let got = snap_45(from, Vec2::new(170.0, 160.0)) - from;
        assert!((got.x - got.y).abs() < 1e-3);
    }
}
