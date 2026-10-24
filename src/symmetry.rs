//! Symmetry, ZBrush style (Transform › Activate Symmetry): mirror planes across X, Y and Z
//! and Radial symmetry about one axis with a count. Together they make a set of images, each
//! an orthogonal map through the origin; a brush applied at a point also applies at every
//! image of it, mirrored or turned like its image.
//!
//! Brushes select once for all images ([`crate::mesh::SculptMesh::select_sphere_images`]):
//! every vertex goes to the nearest image of the brush, and the brush works in that image's
//! frame ([`Images::unmap`] into the brush's own frame, [`Images::map`] back out).

use glam::{Mat3, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    pub fn name(self) -> &'static str {
        match self {
            Axis::X => "X",
            Axis::Y => "Y",
            Axis::Z => "Z",
        }
    }

    pub fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }
}

/// Most images a symmetry may make (eight mirror combinations times a radial count of 8,
/// or a radial count of 64 alone).
pub const MAX_IMAGES: usize = 64;

/// Which symmetry is on.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Symmetry {
    /// Mirror across the plane through the origin perpendicular to X (Y, Z).
    pub x: bool,
    pub y: bool,
    pub z: bool,
    /// Radial: copies turned about this axis, this many in a full turn (2 or more).
    pub radial: Option<(Axis, u8)>,
}

/// `true` is ZBrush's default X symmetry.
impl From<bool> for Symmetry {
    fn from(x: bool) -> Self {
        Self { x, ..Self::default() }
    }
}

impl Symmetry {
    pub fn is_on(&self) -> bool {
        self.images().len() > 1
    }

    /// Every image, the identity first. Duplicates (a radial half turn equals two mirrors)
    /// are dropped; more than [`MAX_IMAGES`] are cut off.
    pub fn images(&self) -> Images {
        let mut maps = vec![Mat3::IDENTITY];
        for (on, axis) in [(self.x, Vec3::X), (self.y, Vec3::Y), (self.z, Vec3::Z)] {
            if on {
                let mirror = Mat3::from_diagonal(Vec3::ONE - 2.0 * axis);
                let more: Vec<Mat3> = maps.iter().map(|m| mirror * *m).collect();
                maps.extend(more);
            }
        }
        if let Some((axis, count)) = self.radial.filter(|(_, c)| *c >= 2) {
            let turns: Vec<Mat3> = (0..count as u32)
                .map(|k| Mat3::from_axis_angle(axis.unit(), std::f32::consts::TAU * k as f32 / count as f32))
                .collect();
            maps = turns.iter().flat_map(|t| maps.iter().map(move |m| *t * *m)).collect();
        }
        let mut unique: Vec<Mat3> = Vec::with_capacity(maps.len());
        for m in maps {
            if !unique.iter().any(|u| u.abs_diff_eq(m, 1e-5)) {
                unique.push(m);
            }
        }
        unique.truncate(MAX_IMAGES);
        Images(unique)
    }

    /// Short description for the interface, e.g. "X · Radial Z 8".
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = [(self.x, "X"), (self.y, "Y"), (self.z, "Z")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, n)| n.to_string())
            .collect();
        if let Some((axis, count)) = self.radial {
            parts.push(format!("Radial {} {count}", axis.name()));
        }
        if parts.is_empty() { "Off".into() } else { parts.join(" · ") }
    }
}

/// The images of a symmetry: orthogonal maps, the identity first.
#[derive(Clone, Debug, PartialEq)]
pub struct Images(pub Vec<Mat3>);

impl Images {
    /// No symmetry: the identity alone.
    pub fn identity() -> Self {
        Images(vec![Mat3::IDENTITY])
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `v` (a point, direction or normal) in image `k`.
    #[inline]
    pub fn map(&self, k: usize, v: Vec3) -> Vec3 {
        if k == 0 { v } else { self.0[k] * v }
    }

    /// `v` in image `k` taken back to the brush's own frame (the maps are orthogonal).
    #[inline]
    pub fn unmap(&self, k: usize, v: Vec3) -> Vec3 {
        if k == 0 { v } else { self.0[k].transpose() * v }
    }

    /// Whether image `k` mirrors (turns handedness), so patterns across a stroke flip.
    pub fn mirrors(&self, k: usize) -> bool {
        self.0[k].determinant() < 0.0
    }

    /// Every image of point `c`.
    pub fn of(&self, c: Vec3) -> Vec<Vec3> {
        self.0.iter().map(|m| *m * c).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_combine_mirrors_and_radial_without_duplicates() {
        assert_eq!(Symmetry::default().images().len(), 1);
        assert!(!Symmetry::default().is_on());
        let x = Symmetry::from(true).images();
        assert_eq!(x.len(), 2);
        assert_eq!(x.map(1, Vec3::new(1.0, 2.0, 3.0)), Vec3::new(-1.0, 2.0, 3.0));
        assert!(x.mirrors(1) && !x.mirrors(0));
        let xyz = Symmetry { x: true, y: true, z: true, radial: None }.images();
        assert_eq!(xyz.len(), 8);
        let radial = Symmetry { radial: Some((Axis::Z, 6)), ..Default::default() }.images();
        assert_eq!(radial.len(), 6);
        let p = Vec3::new(1.0, 0.0, 0.5);
        for q in radial.of(p) {
            assert!((q.truncate().length() - 1.0).abs() < 1e-5 && (q.z - 0.5).abs() < 1e-6);
        }
        // A radial half turn about Z is X and Y mirrors together: no duplicates.
        let both = Symmetry { x: true, y: true, z: false, radial: Some((Axis::Z, 2)) }.images();
        assert_eq!(both.len(), 4);
        // Mirror with radial: twice the radial count.
        assert_eq!(Symmetry { x: true, radial: Some((Axis::Y, 8)), ..Default::default() }.images().len(), 16);
        // Capped.
        assert_eq!(Symmetry { x: true, y: true, z: true, radial: Some((Axis::Z, 64)) }.images().len(), MAX_IMAGES);
        // unmap undoes map.
        let v = Vec3::new(0.3, -0.7, 0.2);
        for k in 0..xyz.len() {
            assert!(xyz.unmap(k, xyz.map(k, v)).distance(v) < 1e-6);
        }
    }

    #[test]
    fn labels_read_like_the_palette() {
        assert_eq!(Symmetry::default().label(), "Off");
        assert_eq!(Symmetry { x: true, radial: Some((Axis::Z, 8)), ..Default::default() }.label(), "X · Radial Z 8");
    }
}
