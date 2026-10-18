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
