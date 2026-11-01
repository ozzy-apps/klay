//! ZSpheres (ZBrush: the ZSphere tool): a tree of spheres sketched on the canvas as an
//! armature, turned into a mesh to sculpt by **Make Adaptive Skin**.
//!
//! A ZSphere subtool's mesh is the tree as drawn ([`Tree::display`]): every sphere, one
//! polygroup each so a click knows which sphere it hit, and a thin link between each sphere
//! and its parent. Editing replaces that mesh; the tree itself rides on the level stack
//! ([`crate::multires::Multires::zspheres`]), so undo, parking and projects keep it.
//!
//! The skin ([`Tree::skin`]) is the union of the spheres and of further spheres placed
//! along each link with the radius tapering between its ends, remeshed by Wedge into one
//! closed, even surface and polished so the joints blend.
//!
//! A Klay addition: any node can be a cube instead of a sphere, or anything between
//! (**roundness** 1 a sphere, 0 a cube, half-size the radius), turned to follow its link.
//! The shapes along a link blend the roundness of its ends, so a chain from a sphere to a
//! cube goes smoothly from round to square.

use crate::mesh::SculptMesh;
use glam::Vec3;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZSphere {
    pub center: Vec3,
    pub radius: f32,
    /// The sphere it grows from; [`NONE`] for the root.
    pub parent: u32,
    /// 1 a sphere, 0 a cube, between them a rounded cube.
    pub roundness: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    pub spheres: Vec<ZSphere>,
}

impl Default for Tree {
    /// One root sphere at the origin, as a new ZSphere starts.
    fn default() -> Self {
        Self { spheres: vec![ZSphere { center: Vec3::ZERO, radius: 0.5, parent: NONE, roundness: 1.0 }] }
    }
}

/// Points of a unit sphere and its triangles, for drawing and skinning.
fn unit_sphere(segments: u32) -> (Vec<Vec3>, Vec<u32>) {
    let (p, i) = crate::mesh::cube_sphere(segments, 1.0);
    (p.into_iter().map(|q| q.normalize()).collect(), i)
}

/// A point `q` of the unit sphere moved onto the shape of `roundness`: the sphere itself at
/// 1, the cube of half-size 1 at 0, the straight blend between.
fn shaped(q: Vec3, roundness: f32) -> Vec3 {
    let cube = q / q.abs().max_element().max(1e-6);
    cube.lerp(q, roundness.clamp(0.0, 1.0))
}

/// Axes turned so the third points along `d`, the first kept as near world Y as it can be
/// (so a mirrored link gets the mirrored turn).
fn frame(d: Vec3) -> glam::Mat3 {
    let d = d.normalize_or(Vec3::Y);
    let r = if d.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
    let u = (r - d * d.dot(r)).normalize();
    glam::Mat3::from_cols(u, d.cross(u), d)
}

impl Tree {
    /// Add a sphere at `center` with `radius` under `parent`; returns its index.
    /// A new sphere takes its parent's roundness.
    pub fn add(&mut self, parent: u32, center: Vec3, radius: f32) -> u32 {
        let roundness = self.spheres.get(parent as usize).map_or(1.0, |p| p.roundness);
        self.spheres.push(ZSphere { center, radius: radius.max(1e-4), parent, roundness });
        self.spheres.len() as u32 - 1
    }

    /// Delete sphere `i` (not the root): its children grow from its parent instead.
    /// Returns whether it went.
    pub fn delete(&mut self, i: u32) -> bool {
        let Some(&z) = self.spheres.get(i as usize) else { return false };
        if z.parent == NONE {
            return false;
        }
        self.spheres.remove(i as usize);
        for s in &mut self.spheres {
            if s.parent == i {
                s.parent = z.parent;
            }
            if s.parent != NONE && s.parent > i {
                s.parent -= 1;
            }
        }
        true
    }

    /// The sphere mirroring `i` across X: `i` itself when it sits on the plane, `None` when
    /// it has no counterpart.
    pub fn mirror_of(&self, i: u32) -> Option<u32> {
        let z = self.spheres[i as usize];
        let tol = 1e-3 * z.radius.max(1e-3);
        if z.center.x.abs() <= tol {
            return Some(i);
        }
        let image = Vec3::new(-z.center.x, z.center.y, z.center.z);
        (0..self.spheres.len() as u32)
            .filter(|&j| j != i)
            .min_by(|&a, &b| self.spheres[a as usize].center.distance(image).total_cmp(&self.spheres[b as usize].center.distance(image)))
            .filter(|&j| self.spheres[j as usize].center.distance(image) <= tol * 10.0)
    }

    /// Which way node `i` is turned: along the link from its parent, or for the root
    /// toward its first child.
    pub fn axes(&self, i: u32) -> glam::Mat3 {
        let z = self.spheres[i as usize];
        let d = if z.parent != NONE {
            z.center - self.spheres[z.parent as usize].center
        } else {
            self.spheres.iter().find(|c| c.parent == i).map_or(Vec3::Y, |c| c.center - z.center)
        };
        frame(d)
    }

    /// Points of node `i`'s shape, from unit sphere points `up`.
    fn node_points<'a>(&self, i: u32, up: &'a [Vec3]) -> impl Iterator<Item = Vec3> + 'a {
        let z = self.spheres[i as usize];
        let m = self.axes(i);
        up.iter().map(move |&q| z.center + m * shaped(q, z.roundness) * z.radius)
    }

    /// The sphere whose polygroup is `group` in [`display`](Self::display), or for a link
    /// (group 0) the sphere nearest `point`.
    pub fn sphere_of(&self, group: u16, point: Vec3) -> u32 {
        if group > 0 && (group as usize) <= self.spheres.len() {
            return group as u32 - 1;
        }
        (0..self.spheres.len() as u32)
            .min_by(|&a, &b| {
                let d = |k: u32| self.spheres[k as usize].center.distance(point) - self.spheres[k as usize].radius;
                d(a).total_cmp(&d(b))
            })
            .unwrap_or(0)
    }

    /// The tree as drawn: each sphere in polygroup `index + 1`, and between each sphere and
    /// its parent a thin tapered tube in polygroup 0.
    pub fn display(&self) -> SculptMesh {
        let (up, ui) = unit_sphere(6);
        let (mut p, mut idx, mut groups) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..self.spheres.len() {
            let base = p.len() as u32;
            p.extend(self.node_points(k as u32, &up));
            idx.extend(ui.iter().map(|&i| base + i));
            groups.extend(std::iter::repeat_n(k as u16 + 1, ui.len() / 3));
        }
        const SIDES: u32 = 8;
        for z in &self.spheres {
            if z.parent == NONE {
                continue;
            }
            let q = self.spheres[z.parent as usize];
            let axis = z.center - q.center;
            let len = axis.length();
            if len <= (z.radius + q.radius) * 0.5 {
                continue;
            }
            let d = axis / len;
            let u = d.any_orthonormal_vector();
            let v = d.cross(u);
            let (ra, rb) = (q.radius * 0.25, z.radius * 0.25);
            let base = p.len() as u32;
            for (c, r) in [(q.center + d * q.radius * 0.9, ra), (z.center - d * z.radius * 0.9, rb)] {
                for k in 0..SIDES {
                    let a = k as f32 / SIDES as f32 * std::f32::consts::TAU;
                    p.push(c + (u * a.cos() + v * a.sin()) * r);
                }
            }
            for k in 0..SIDES {
                let (a0, a1) = (base + k, base + (k + 1) % SIDES);
                let (b0, b1) = (a0 + SIDES, a1 + SIDES);
                idx.extend_from_slice(&[a0, a1, b1, a0, b1, b0]);
                groups.extend_from_slice(&[0, 0]);
            }
            // Caps close the tube.
            let (ca, cb) = (p.len() as u32, p.len() as u32 + 1);
            p.push(q.center + d * q.radius * 0.9);
            p.push(z.center - d * z.radius * 0.9);
            for k in 0..SIDES {
                let (a0, a1) = (base + k, base + (k + 1) % SIDES);
                idx.extend_from_slice(&[ca, a1, a0, cb, a0 + SIDES, a1 + SIDES]);
                groups.extend_from_slice(&[0, 0]);
            }
        }
        SculptMesh::from_raw_with_groups(p, idx, groups)
    }

    /// The smallest sphere radius.
    fn min_radius(&self) -> f32 {
        self.spheres.iter().map(|z| z.radius).fold(f32::INFINITY, f32::min).max(1e-4)
    }

    /// Make Adaptive Skin: one closed surface around the tree, voxels `detail` times finer
    /// than the smallest sphere's radius (more gives more polygons).
    pub fn skin(&self, detail: f32) -> Result<SculptMesh, String> {
        let (up, ui) = unit_sphere(10);
        let (mut p, mut idx) = (Vec::new(), Vec::new());
        let mut ball = |c: Vec3, r: f32, m: glam::Mat3, round: f32| {
            let base = p.len() as u32;
            p.extend(up.iter().map(|&q| c + m * shaped(q, round) * r));
            idx.extend(ui.iter().map(|&i| base + i));
        };
        for (i, z) in self.spheres.iter().enumerate() {
            ball(z.center, z.radius, self.axes(i as u32), z.roundness);
            if z.parent == NONE {
                continue;
            }
            // Along the link: radius and roundness blend, the shapes turned with the link.
            let q = self.spheres[z.parent as usize];
            let m = frame(z.center - q.center);
            let len = z.center.distance(q.center);
            let step = 0.12 * z.radius.min(q.radius);
            let n = ((len / step).ceil() as usize).clamp(1, 256);
            for k in 1..n {
                let t = k as f32 / n as f32;
                ball(q.center.lerp(z.center, t), q.radius + (z.radius - q.radius) * t, m, q.roundness + (z.roundness - q.roundness) * t);
            }
        }
        let union = SculptMesh::from_raw(p, idx);
        let voxel = self.min_radius() / detail.max(1.0);
        let settings = crate::wedge::Settings { resolution: ((2.0 / voxel) as u32).clamp(32, 1024), ..Default::default() };
        let out = crate::wedge::remesh(&union, &settings)?;
        let mesh = SculptMesh::from_raw(out.positions, out.indices);
        // Blend the joints and the scallops between link spheres.
        Ok(match crate::deform::shaped(&mesh, crate::subtool::Deform::Polish, 30.0) {
            Some(p) => SculptMesh::from_raw(p, mesh.indices.clone()),
            None => mesh,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm() -> Tree {
        let mut t = Tree::default();
        let a = t.add(0, Vec3::new(0.5, 0.0, 0.0), 0.3);
        t.add(a, Vec3::new(1.2, 0.0, 0.0), 0.2);
        t
    }

    #[test]
    fn display_tags_each_sphere_and_delete_reattaches_children() {
        let mut t = arm();
        let m = t.display();
        for (k, z) in t.spheres.iter().enumerate() {
            let tri = (0..m.triangle_count() as u32).find(|&tri| m.group(tri) == k as u16 + 1).unwrap();
            let c = m.positions[m.indices[3 * tri as usize] as usize];
            assert!((c.distance(z.center) - z.radius).abs() < 1e-4);
            assert_eq!(t.sphere_of(k as u16 + 1, c), k as u32);
        }
        assert!(!t.delete(0), "the root stays");
        assert!(t.delete(1));
        assert_eq!(t.spheres.len(), 2);
        assert_eq!(t.spheres[1].parent, 0);
    }

    #[test]
    fn mirror_finds_the_counterpart_across_x() {
        let mut t = Tree::default();
        let a = t.add(0, Vec3::new(0.4, 0.1, 0.0), 0.2);
        let b = t.add(0, Vec3::new(-0.4, 0.1, 0.0), 0.2);
        assert_eq!(t.mirror_of(a), Some(b));
        assert_eq!(t.mirror_of(0), Some(0));
        let c = t.add(a, Vec3::new(0.9, 0.5, 0.0), 0.1);
        assert_eq!(t.mirror_of(c), None);
    }

    #[test]
    fn the_skin_is_one_closed_surface_around_every_sphere() {
        let t = arm();
        let skin = t.skin(4.0).unwrap();
        assert!(crate::wedge::closed(&skin));
        // Every sphere's top lies near the skin, and the middle of a link is covered.
        let near = |q: Vec3| skin.positions.iter().map(|p| p.distance(q)).fold(f32::INFINITY, f32::min);
        for z in &t.spheres {
            assert!(near(z.center + Vec3::Y * z.radius) < z.radius * 0.25, "{z:?}");
        }
        let mid = Vec3::new(0.85, 0.25, 0.0);
        assert!(near(mid) < 0.08, "{}", near(mid));
    }

    #[test]
    fn a_cube_node_is_square_and_its_link_goes_from_round_to_square() {
        let mut t = Tree::default();
        let a = t.add(0, Vec3::new(1.5, 0.0, 0.0), 0.4);
        t.spheres[a as usize].roundness = 0.0;
        // The cube node: its shape reaches the corners of a cube turned along the link.
        let (up, _) = unit_sphere(6);
        let far = t.node_points(a, &up).map(|p| p.distance(t.spheres[a as usize].center)).fold(0.0, f32::max);
        assert!((far - 0.4 * 3f32.sqrt()).abs() < 1e-3, "{far}");
        // In the skin, the cross-section near the cube is square (reaches out along the
        // diagonal), near the sphere round.
        let skin = t.skin(4.0).unwrap();
        let reach = |x: f32, dir: Vec3| {
            skin.positions.iter().filter(|p| (p.x - x).abs() < 0.04).map(|p| Vec3::new(0.0, p.y, p.z).dot(dir)).fold(0.0, f32::max)
        };
        let diag = Vec3::new(0.0, 1.0, 1.0).normalize();
        let square = reach(1.5, diag) / reach(1.5, Vec3::Y);
        let round = reach(0.0, diag) / reach(0.0, Vec3::Y);
        assert!(square > 1.2 && (round - 1.0).abs() < 0.1, "{square} {round}");
    }
}
