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
        self.forward().cross(Vec3::Y).normalize()
    }

    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward()).normalize()
    }

    pub fn view(&self) -> Mat4 {
        look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    pub fn proj(&self) -> Mat4 {
        perspective_rh(self.fov_y, self.aspect.max(0.01), self.near, self.far)
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj() * self.view()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-1.55, 1.55);
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
    /// the target slides towards where `ray_dir` crosses the plane through the target.
    pub fn zoom_towards(&mut self, scroll: f32, ray_dir: Vec3) {
        let old = self.distance;
        self.zoom(scroll);
        let k = self.distance / old;
        let fwd = self.forward();
        let denom = ray_dir.dot(fwd);
        if denom > 1e-4 {
            // Eye uses the old distance: recompute it from the target and the old value.
            let eye = self.target - fwd * old;
            let under_cursor = eye + ray_dir * (old / denom);
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
            let (_, d) = cam.ray(x, y, w, h);
            cam.zoom_towards(scroll, d);
            let (px, py) = project(&cam, p, w, h);
            assert!((px - x).abs() < 0.05 && (py - y).abs() < 0.05, "{px} {py}");
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
