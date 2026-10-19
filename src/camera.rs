use glam::{Mat4, Vec3, Vec4};

/// Right-handed view matrix looking from `eye` towards `center`.
pub fn look_at_rh(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    let f = (center - eye).normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols(
        Vec4::new(s.x, u.x, -f.x, 0.0),
        Vec4::new(s.y, u.y, -f.y, 0.0),
        Vec4::new(s.z, u.z, -f.z, 0.0),
        Vec4::new(-eye.dot(s), -eye.dot(u), eye.dot(f), 1.0),
    )
}

/// Right-handed perspective projection with a 0..1 depth range (wgpu convention).
pub fn perspective_rh(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    let r = far / (near - far);
    Mat4::from_cols(
        Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        Vec4::new(0.0, f, 0.0, 0.0),
        Vec4::new(0.0, 0.0, r, -1.0),
        Vec4::new(0.0, 0.0, r * near, 0.0),
    )
}

/// Orthographic projection with a 0..1 depth range, symmetric about the view axis.
pub fn orthographic_rh(half_w: f32, half_h: f32, near: f32, far: f32) -> Mat4 {
    let r = 1.0 / (near - far);
    Mat4::from_cols(
        Vec4::new(1.0 / half_w, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 1.0 / half_h, 0.0, 0.0),
        Vec4::new(0.0, 0.0, r, 0.0),
        Vec4::new(0.0, 0.0, r * near, 1.0),
    )
}

/// Pitch limit: exactly straight down / straight up. The up vector follows the orbit
/// (see [`Camera::up`]), so the poles are not degenerate.
pub const MAX_PITCH: f32 = std::f32::consts::FRAC_PI_2;

/// Orbit camera around a target point.
pub struct Camera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
    /// Orthographic instead of perspective. The visible height at the target stays the
    /// same when toggling, so the model does not jump.
    pub ortho: bool,
    /// Rotation about the view axis, radians (ZBrush: Shift, click, release Shift, drag).
    pub roll: f32,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: 0.6,
            pitch: 0.35,
            distance: 3.2,
            fov_y: 45f32.to_radians(),
            aspect: 1.0,
            near: 0.01,
            far: 100.0,
            ortho: false,
            roll: 0.0,
        }
    }

    pub fn eye(&self) -> Vec3 {
        let dir = Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        );
        self.target + dir * self.distance
    }

    pub fn forward(&self) -> Vec3 {
        (self.target - self.eye()).normalize()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(self.up()).normalize()
    }

    /// Screen-up in world space before roll: the derivative of the eye direction with
    /// respect to pitch. Equals +Y at pitch 0 and stays well defined at the poles, where
    /// a fixed world up would make the view matrix degenerate.
    fn up_unrolled(&self) -> Vec3 {
        Vec3::new(
            -self.pitch.sin() * self.yaw.sin(),
            self.pitch.cos(),
            -self.pitch.sin() * self.yaw.cos(),
        )
    }

    /// Screen-up in world space, including roll.
    pub fn up(&self) -> Vec3 {
        let u = self.up_unrolled();
        if self.roll == 0.0 {
            return u;
        }
        let r = self.forward().cross(u);
        u * self.roll.cos() + r * self.roll.sin()
    }

    /// Turn about the view axis. Positive rolls the scene counter-clockwise on screen.
    pub fn roll_by(&mut self, angle: f32) {
        self.roll = (self.roll - angle).rem_euclid(std::f32::consts::TAU);
    }

    pub fn view(&self) -> Mat4 {
        look_at_rh(self.eye(), self.target, self.up())
    }

    /// Half of the visible height at the target plane.
    pub fn half_height_at_target(&self) -> f32 {
        self.distance * (self.fov_y * 0.5).tan()
    }

    pub fn proj(&self) -> Mat4 {
        let aspect = self.aspect.max(0.01);
        if self.ortho {
            let h = self.half_height_at_target();
            orthographic_rh(h * aspect, h, self.near, self.far)
        } else {
            perspective_rh(self.fov_y, aspect, self.near, self.far)
        }
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj() * self.view()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Snap to the nearest axis view (ZBrush: Shift while rotating): yaw to a multiple
    /// of 90°, pitch to front/side level, straight up or straight down.
    pub fn snap_to_axis(&mut self) {
        let q = std::f32::consts::FRAC_PI_2;
        self.yaw = (self.yaw / q).round() * q;
        self.pitch = (self.pitch / q).round().clamp(-1.0, 1.0) * q;
        self.roll = 0.0;
    }

    pub fn pan(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        // Move the target so that the point under the cursor follows the mouse.
        let world_per_pixel = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_h.max(1.0);
        self.target -= self.right() * dx * world_per_pixel;
        self.target += self.up() * dy * world_per_pixel;
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * (1.0 - scroll * 0.1)).clamp(0.05, 50.0);
    }

    /// Zoom like [`zoom`](Self::zoom) but keep the point under the cursor fixed on screen:
    /// the target slides towards where the cursor `ray` crosses the plane through the
    /// target. Works for both projections (in orthographic the ray origin carries the offset).
    pub fn zoom_towards(&mut self, scroll: f32, ray: (Vec3, Vec3)) {
        let old = self.distance;
        self.zoom(scroll);
        let k = self.distance / old;
        let fwd = self.forward();
        let (o, d) = ray;
        let denom = d.dot(fwd);
        if denom > 1e-4 {
            let under_cursor = o + d * ((self.target - o).dot(fwd) / denom);
            self.target += (under_cursor - self.target) * (1.0 - k);
        }
    }

    /// Drag zoom: dragging down or right moves the eye closer.
    pub fn dolly(&mut self, dx: f32, dy: f32) {
        let factor = (-(dx + dy) * 0.005).exp();
        self.distance = (self.distance * factor).clamp(0.05, 50.0);
    }

    /// World-space ray through a pixel position. `x`, `y` in pixels, origin top-left.
    pub fn ray(&self, x: f32, y: f32, width: f32, height: f32) -> (Vec3, Vec3) {
        let ndc_x = (x / width.max(1.0)) * 2.0 - 1.0;
        let ndc_y = 1.0 - (y / height.max(1.0)) * 2.0;
        let inv = self.view_proj().inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        (near, (far - near).normalize())
    }

    /// Size of one pixel in world units at the given distance from the eye.
    pub fn pixel_size_at(&self, world_pos: Vec3, viewport_h: f32) -> f32 {
        if self.ortho {
            return 2.0 * self.half_height_at_target() / viewport_h.max(1.0);
        }
        let d = (world_pos - self.eye()).dot(self.forward()).max(self.near);
        2.0 * d * (self.fov_y * 0.5).tan() / viewport_h.max(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixel position of a world point, origin top-left.
    fn project(cam: &Camera, p: Vec3, w: f32, h: f32) -> (f32, f32) {
        let clip = cam.view_proj() * p.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        ((ndc.x + 1.0) * 0.5 * w, (1.0 - ndc.y) * 0.5 * h)
    }

    #[test]
    fn zoom_towards_keeps_point_under_cursor() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        let (w, h) = (1600.0, 1000.0);
        let (x, y) = (1200.0, 300.0);
        // The point under the cursor on the target plane.
        let (o, d) = cam.ray(x, y, w, h);
        let fwd = cam.forward();
        let p = o + d * ((cam.target - o).dot(fwd) / d.dot(fwd));
        for scroll in [3.0, -2.0, 1.0] {
            let ray = cam.ray(x, y, w, h);
            cam.zoom_towards(scroll, ray);
            let (px, py) = project(&cam, p, w, h);
            assert!((px - x).abs() < 0.05 && (py - y).abs() < 0.05, "{px} {py}");
        }
    }

    #[test]
    fn ortho_zoom_towards_keeps_point_under_cursor() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        cam.ortho = true;
        let (w, h) = (1600.0, 1000.0);
        let (x, y) = (300.0, 800.0);
        let (o, d) = cam.ray(x, y, w, h);
        let fwd = cam.forward();
        let p = o + d * ((cam.target - o).dot(fwd) / d.dot(fwd));
        for scroll in [2.0, -3.0] {
            let ray = cam.ray(x, y, w, h);
            cam.zoom_towards(scroll, ray);
            let (px, py) = project(&cam, p, w, h);
            assert!((px - x).abs() < 0.05 && (py - y).abs() < 0.05, "{px} {py}");
        }
    }

    #[test]
    fn ortho_toggle_keeps_target_plane_scale() {
        let mut cam = Camera::new();
        cam.aspect = 1.6;
        let (w, h) = (1600.0, 1000.0);
        // A point on the target plane, off-centre.
        let p = cam.target + cam.right() * 0.7 + cam.up() * 0.4;
        let a = project(&cam, p, w, h);
        cam.ortho = true;
        let b = project(&cam, p, w, h);
        assert!((a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05, "{a:?} {b:?}");
    }

    #[test]
    fn snap_to_axis_rounds_to_quarter_turns() {
        let q = std::f32::consts::FRAC_PI_2;
        let mut cam = Camera::new();
        cam.yaw = 0.6;
        cam.pitch = 0.35;
        cam.snap_to_axis();
        assert!(cam.yaw.abs() < 1e-6 && cam.pitch.abs() < 1e-6);
        cam.yaw = 1.0;
        cam.pitch = 1.2;
        cam.snap_to_axis();
        assert!((cam.yaw - q).abs() < 1e-6 && (cam.pitch - q).abs() < 1e-6);
    }

    #[test]
    fn roll_turns_about_the_view_axis() {
        let mut cam = Camera::new();
        let fwd = cam.forward();
        let up0 = cam.up();
        cam.roll_by(std::f32::consts::FRAC_PI_2);
        assert!((cam.forward() - fwd).length() < 1e-6);
        assert!(cam.up().dot(up0).abs() < 1e-5, "up should be perpendicular after 90°");
        assert!(cam.up().dot(fwd).abs() < 1e-5);
        assert!(cam.right().dot(cam.up()).abs() < 1e-5);
        cam.snap_to_axis();
        assert_eq!(cam.roll, 0.0);
    }

    #[test]
    fn view_is_finite_at_the_poles() {
        for pitch in [MAX_PITCH, -MAX_PITCH] {
            let mut cam = Camera::new();
            cam.pitch = pitch;
            let m = cam.view_proj();
            assert!(m.to_cols_array().iter().all(|v| v.is_finite()));
            let (o, d) = cam.ray(10.0, 10.0, 100.0, 100.0);
            assert!(o.is_finite() && d.is_finite());
        }
    }

    #[test]
    fn dolly_down_moves_closer() {
        let mut cam = Camera::new();
        let d0 = cam.distance;
        cam.dolly(0.0, 40.0);
        assert!(cam.distance < d0);
        cam.dolly(-80.0, 0.0);
        assert!(cam.distance > d0);
    }
}

#[cfg(test)]
mod raycast_probe {
    use super::*;
    use crate::mesh::{SculptMesh, cube_sphere};

    #[test]
    fn front_view_rays_hit_the_sphere_everywhere_inside_its_outline() {
        let (p, i) = cube_sphere(60, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let (w, h) = (1020.0, 860.0);
        for (yaw, pitch) in [(0.0, 0.0), (0.6, 0.35), (std::f32::consts::FRAC_PI_2, 0.0)] {
            let mut cam = Camera::new();
            cam.aspect = w / h;
            cam.yaw = yaw;
            cam.pitch = pitch;
            let mut misses = Vec::new();
            for (x, y) in [(295.0, 330.0), (510.0, 430.0), (330.0, 610.0), (700.0, 300.0), (400.0, 200.0)] {
                let (o, d) = cam.ray(x, y, w, h);
                // Analytic sphere test for the expectation.
                let b = o.dot(d);
                let c = o.length_squared() - 1.0;
                let expect = b * b - c > 0.0 && -b > 0.0;
                let got = mesh.raycast(o, d).is_some();
                if expect != got {
                    misses.push((x, y, expect, got, d));
                }
            }
            assert!(misses.is_empty(), "yaw {yaw} pitch {pitch}: {misses:?}");
        }
    }
}
