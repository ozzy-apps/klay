//! Reference image (ZBrush: Image Plane): a picture on a plane in the scene, to model
//! against. The plane sits behind the model as seen from the front, the side or the top
//! ([`View`]), or free: behind the model as seen from the camera when Free is chosen, and
//! fixed in the world from then on, so the camera turns round it. The renderer draws it
//! blended at its opacity (`render.rs`, `vs_reference`); the project saves it with its
//! picture ([`crate::project::SavedReference`]).

use crate::camera::Camera;
use crate::mesh::Aabb;
use crate::spotlight::Picture;
use glam::Vec3;
use std::sync::Arc;

/// Which way the plane faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Behind the model, facing +Z (the front view).
    Front,
    /// To the model's left, facing +X (seen from the right side).
    Side,
    /// Under the model, facing +Y (seen from above; the picture's top points away).
    Top,
    /// Behind the model as the camera saw it when Free was chosen.
    Free,
}

impl View {
    pub const ALL: [View; 4] = [View::Front, View::Side, View::Top, View::Free];

    pub fn label(self) -> &'static str {
        match self {
            View::Front => "Front",
            View::Side => "Side",
            View::Top => "Top",
            View::Free => "Free",
        }
    }

    /// Its code in a project file.
    pub fn code(self) -> u8 {
        View::ALL.iter().position(|&v| v == self).unwrap() as u8
    }

    pub fn from_code(code: u8) -> Self {
        View::ALL.get(code as usize).copied().unwrap_or(View::Front)
    }
}

/// Where the plane is: its centre and the picture's right and up in the world (unit and
/// perpendicular); the picture faces `right × up`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub center: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

impl Pose {
    /// The way the picture faces.
    #[cfg(test)]
    pub fn normal(&self) -> Vec3 {
        self.right.cross(self.up)
    }

    /// The plane for `view`, just behind the model's `bounds` (for Free: behind it as
    /// `camera` sees it).
    pub fn place(view: View, bounds: &Aabb, camera: &Camera) -> Self {
        let (c, ext) = if bounds.min.cmple(bounds.max).all() && bounds.min.is_finite() && bounds.max.is_finite() {
            ((bounds.min + bounds.max) * 0.5, bounds.max - bounds.min)
        } else {
            (Vec3::ZERO, Vec3::splat(2.0))
        };
        let margin = 0.05 * ext.max_element().max(0.1);
        let (right, up) = match view {
            View::Front => (Vec3::X, Vec3::Y),
            View::Side => (-Vec3::Z, Vec3::Y),
            View::Top => (Vec3::X, -Vec3::Z),
            View::Free => {
                let r = camera.right();
                (r, (-camera.forward()).cross(r).normalize())
            }
        };
        let n = right.cross(up);
        // How far the model reaches behind its centre, the way the plane faces.
        let reach = (ext * 0.5).dot(n.abs());
        Self { center: c - n * (reach + margin), right, up }
    }

    /// The plane's corners for a picture `size` wide and `aspect` (height / width) tall:
    /// top left, top right, bottom right, bottom left, as the picture reads from the front.
    pub fn corners(&self, size: f32, aspect: f32) -> [Vec3; 4] {
        let (hw, hh) = (self.right * (size * 0.5), self.up * (size * aspect * 0.5));
        [self.center - hw + hh, self.center + hw + hh, self.center + hw - hh, self.center - hw - hh]
    }
}

/// The reference image as the interface holds it.
pub struct Reference {
    pub picture: Arc<Picture>,
    pub name: String,
    pub view: View,
    pub opacity: f32,
    /// The plane's width, world units.
    pub size: f32,
    /// The plane stays where it is: the view, Size and placing again are disabled.
    pub locked: bool,
    pub shown: bool,
    pub pose: Pose,
}

impl Reference {
    /// `picture` on a Front plane behind the model's `bounds`, half transparent and one
    /// and a half times as wide as the model's longest side.
    pub fn new(picture: Arc<Picture>, name: String, bounds: &Aabb, camera: &Camera) -> Self {
        let ext = bounds.max - bounds.min;
        let size = if ext.is_finite() && ext.max_element() > 0.0 { 1.5 * ext.max_element() } else { 3.0 };
        Self { picture, name, view: View::Front, opacity: 0.5, size, locked: false, shown: true, pose: Pose::place(View::Front, bounds, camera) }
    }

    /// Place the plane again for `view` (nothing while locked).
    pub fn place(&mut self, view: View, bounds: &Aabb, camera: &Camera) {
        if self.locked {
            return;
        }
        self.view = view;
        self.pose = Pose::place(view, bounds, camera);
    }

    /// Height over width of the picture.
    pub fn aspect(&self) -> f32 {
        self.picture.height as f32 / self.picture.width.max(1) as f32
    }

    pub fn corners(&self) -> [Vec3; 4] {
        self.pose.corners(self.size, self.aspect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Aabb {
        Aabb { min: Vec3::new(-1.0, -2.0, -0.5), max: Vec3::new(1.0, 2.0, 0.5) }
    }

    #[test]
    fn front_side_and_top_planes_sit_just_behind_the_model_facing_the_viewer() {
        let cam = Camera::new();
        let b = bounds();
        let front = Pose::place(View::Front, &b, &cam);
        assert_eq!(front.normal(), Vec3::Z);
        assert!(front.center.z < b.min.z && front.center.z > b.min.z - 0.3, "{front:?}");
        assert_eq!(front.center.truncate(), glam::Vec2::ZERO);
        let side = Pose::place(View::Side, &b, &cam);
        assert_eq!(side.normal(), Vec3::X);
        assert!(side.center.x < b.min.x && side.up == Vec3::Y);
        let top = Pose::place(View::Top, &b, &cam);
        assert_eq!(top.normal(), Vec3::Y);
        assert!(top.center.y < b.min.y);
        // The picture's corners: top left to the viewer's left and up, the width `size`.
        let c = front.corners(4.0, 0.5);
        assert_eq!(c[0], Vec3::new(-2.0, 1.0, front.center.z));
        assert_eq!(c[2], Vec3::new(2.0, -1.0, front.center.z));
        assert!((c[1] - c[0]).length() - 4.0 < 1e-6 && (c[3] - c[0]).length() - 2.0 < 1e-6);
    }

    #[test]
    fn a_free_plane_faces_the_camera_from_behind_the_model() {
        let mut cam = Camera::new();
        cam.yaw = 2.1;
        cam.pitch = -0.4;
        let b = bounds();
        let free = Pose::place(View::Free, &b, &cam);
        let n = free.normal();
        assert!((n + cam.forward()).length() < 1e-5, "faces the camera: {n:?} vs {:?}", cam.forward());
        assert!(free.up.dot(cam.up()) > 0.999 && free.right.dot(cam.right()) > 0.999);
        // Behind every corner of the bounds.
        let c = (b.min + b.max) * 0.5;
        for corner in [b.min, b.max, Vec3::new(b.min.x, b.max.y, b.min.z), Vec3::new(b.max.x, b.min.y, b.max.z)] {
            assert!((corner - free.center).dot(n) > 0.0, "{corner:?} is behind the plane");
        }
        assert!((free.center - c).dot(cam.forward()) > 0.0, "behind the centre as seen");
        // Nothing to place from: the plane still has a sane pose.
        let empty = Pose::place(View::Front, &Aabb::empty(), &cam);
        assert!(empty.center.is_finite() && empty.center.z < 0.0);
    }

    #[test]
    fn a_reference_starts_in_front_view_and_lock_keeps_its_place() {
        let cam = Camera::new();
        let pic = Arc::new(Picture { width: 4, height: 2, rgba: vec![[0; 4]; 8], key: 1, encoded: Default::default() });
        let mut r = Reference::new(pic, "sheet.png".into(), &bounds(), &cam);
        assert_eq!((r.view, r.size, r.opacity), (View::Front, 6.0, 0.5));
        assert_eq!(r.aspect(), 0.5);
        let before = r.pose;
        r.place(View::Top, &bounds(), &cam);
        assert!(r.pose != before && r.view == View::Top);
        r.locked = true;
        r.place(View::Side, &bounds(), &cam);
        assert_eq!(r.view, View::Top);
        assert_eq!(View::from_code(View::Free.code()), View::Free);
        assert_eq!(View::from_code(200), View::Front);
    }
}
