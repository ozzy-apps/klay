//! Starting meshes, ZBrush style: Sphere3D, Cube3D, Cylinder3D, Ring3D (a torus) and
//! Plane3D, each with a resolution so a model can start low and gain detail through Divide.
//!
//! Every primitive is a triangulated quad grid with one diagonal per quad, which keeps the
//! interior vertices at valence 6 (Loop subdivision's regular case). Sphere, Cube and
//! Cylinder are the surface of a lattice box mapped onto their shape (the sphere is the
//! cube sphere Klay always used), so they have no poles. All of them are closed manifolds
//! except Plane, whose border is subdivided with Loop's boundary rules (see
//! `crate::multires`). Each fits the box `[-size, size]^3`.

use glam::Vec3;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Primitive {
    Sphere,
    Cube,
    Cylinder,
    /// A torus (ZBrush: Ring3D).
    Ring,
    Plane,
}

impl Primitive {
    pub const ALL: [Self; 5] = [Self::Sphere, Self::Cube, Self::Cylinder, Self::Ring, Self::Plane];

    /// ZBrush name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sphere => "Sphere3D",
            Self::Cube => "Cube3D",
            Self::Cylinder => "Cylinder3D",
            Self::Ring => "Ring3D",
            Self::Plane => "Plane3D",
        }
    }

    /// Plain name, used for new subtools ("Cube 2").
    pub fn short(self) -> &'static str {
        match self {
            Self::Sphere => "Sphere",
            Self::Cube => "Cube",
            Self::Cylinder => "Cylinder",
            Self::Ring => "Ring",
            Self::Plane => "Plane",
        }
    }

    /// `--primitive NAME`: the plain or ZBrush name in any case; `torus` also means Ring.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        let s = s.strip_suffix("3d").unwrap_or(&s);
        match s {
            "torus" => Some(Self::Ring),
            _ => Self::ALL.into_iter().find(|p| p.short().eq_ignore_ascii_case(s)),
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Whether the surface is closed (every edge shared by two triangles).
    pub fn closed(self) -> bool {
        self != Self::Plane
    }

    /// About 4 000 vertices, like ZBrush's Sphere3D at HDivide / VDivide 64.
    pub fn default_segments(self) -> u32 {
        match self {
            Self::Sphere | Self::Cube => 26,
            Self::Cylinder => 24,
            Self::Ring => 37,
            Self::Plane => 63,
        }
    }

    /// About a million vertices; for Sphere exactly the 960 002-vertex sphere Klay used to
    /// start with (`--segments 400`).
    pub fn dense_segments(self) -> u32 {
        match self {
            Self::Sphere | Self::Cube => 400,
            Self::Cylinder => 380,
            Self::Ring => 577,
            Self::Plane => 999,
        }
    }

    /// Slider range: from the coarsest sensible mesh to about 13M vertices.
    pub fn segment_range(self) -> (u32, u32) {
        match self {
            Self::Sphere => (2, 1500),
            Self::Cube => (1, 1500),
            Self::Cylinder => (2, 1300),
            Self::Ring => (3, 2100),
            Self::Plane => (2, 3600),
        }
    }

    /// Height rows of a cylinder with `n` segments per quarter turn, so its wall cells
    /// come out about square.
    fn cylinder_rows(n: u32) -> u32 {
        ((n as f32 * 4.0 / std::f32::consts::PI).round() as u32).max(1)
    }

    /// Vertices the mesh will have.
    pub fn vertex_count(self, segments: u32) -> usize {
        let (lo, hi) = self.segment_range();
        let segments = segments.clamp(lo, hi);
        let n = segments as usize;
        match self {
            Self::Sphere | Self::Cube => box_vertex_count([n, n, n]),
            Self::Cylinder => box_vertex_count([n, Self::cylinder_rows(segments) as usize, n]),
            Self::Ring => 3 * n * n,
            Self::Plane => (n + 1) * (n + 1),
        }
    }

    /// Positions and triangles (counter-clockwise seen from outside), fitting
    /// `[-size, size]^3` and centred on the origin.
    pub fn build(self, segments: u32, size: f32) -> (Vec<Vec3>, Vec<u32>) {
        let (lo, hi) = self.segment_range();
        let n = segments.clamp(lo, hi);
        match self {
            Self::Sphere => cube_sphere(n, size),
            Self::Cube => box_surface([n, n, n], |p| p * size),
            Self::Cylinder => box_surface([n, Self::cylinder_rows(n), n], |p| {
                // Square to disk (elliptical grid mapping): the box's four sides land on
                // the wall, its top and bottom on the caps.
                let (x2, z2) = (p.x * p.x, p.z * p.z);
                Vec3::new(p.x * (1.0 - z2 / 2.0).sqrt(), p.y, p.z * (1.0 - x2 / 2.0).sqrt()) * size
            }),
            Self::Ring => ring(n, size),
            Self::Plane => plane(n, size),
        }
    }
}

/// The primitive used by New, Append and Rebuild, with one resolution per primitive
/// (ZBrush: Tool > Initialize).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub kind: Primitive,
    pub segments: [u32; 5],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kind: Primitive::Sphere,
            segments: Primitive::ALL.map(Primitive::default_segments),
        }
    }
}

impl Settings {
    /// Segments of the selected primitive.
    pub fn current(&self) -> u32 {
        self.segments[self.kind.index()]
    }

    pub fn set_current(&mut self, segments: u32) {
        let (lo, hi) = self.kind.segment_range();
        self.segments[self.kind.index()] = segments.clamp(lo, hi);
    }

    pub fn vertex_count(&self) -> usize {
        self.kind.vertex_count(self.current())
    }

    pub fn build(&self, size: f32) -> (Vec<Vec3>, Vec<u32>) {
        self.kind.build(self.current(), size)
    }
}

/// Lattice points on the surface of an `a × b × c` box.
fn box_vertex_count([a, b, c]: [usize; 3]) -> usize {
    (a + 1) * (b + 1) * (c + 1) - (a - 1) * (b - 1) * (c - 1)
}

/// The surface of a box of `dims` quads along x, y and z, each lattice point mapped by
/// `map` from its place in `[-1, 1]^3`. Quads are split along the same diagonal and wound
/// outwards; vertices are numbered in the order the faces first reach them.
fn box_surface(dims: [u32; 3], map: impl Fn(Vec3) -> Vec3) -> (Vec<Vec3>, Vec<u32>) {
    let [a, b, c] = dims.map(|d| d as usize);
    let count = box_vertex_count([a, b, c]);
    let mut ids: HashMap<u64, u32> = HashMap::with_capacity(count);
    let mut positions: Vec<Vec3> = Vec::with_capacity(count);
    let key = |l: [u32; 3]| (l[0] as u64) | ((l[1] as u64) << 21) | ((l[2] as u64) << 42);
    let mut vertex = |lattice: [u32; 3], positions: &mut Vec<Vec3>| -> u32 {
        *ids.entry(key(lattice)).or_insert_with(|| {
            let p = Vec3::new(
                lattice[0] as f32 / dims[0] as f32,
                lattice[1] as f32 / dims[1] as f32,
                lattice[2] as f32 / dims[2] as f32,
            ) * 2.0
                - 1.0;
            positions.push(map(p));
            (positions.len() - 1) as u32
        })
    };
    let quads = 2 * (a * b + b * c + c * a);
    let mut indices: Vec<u32> = Vec::with_capacity(quads * 6);
    // Each face: a fixed axis at 0 or its full length; the other two axes vary.
    for axis in 0..3usize {
        for side in [0, dims[axis]] {
            let (u_axis, v_axis) = ((axis + 1) % 3, (axis + 2) % 3);
            for i in 0..dims[u_axis] {
                for j in 0..dims[v_axis] {
                    let mut l = |s: u32, t: u32| {
                        let mut lat = [0u32; 3];
                        lat[axis] = side;
                        lat[u_axis] = s;
                        lat[v_axis] = t;
                        vertex(lat, &mut positions)
                    };
                    let v00 = l(i, j);
                    let v10 = l(i + 1, j);
                    let v11 = l(i + 1, j + 1);
                    let v01 = l(i, j + 1);
                    // Outward winding: flip when the face sits on the negative side.
                    if side != 0 {
                        indices.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
                    } else {
                        indices.extend_from_slice(&[v00, v11, v10, v00, v01, v11]);
                    }
                }
            }
        }
    }
    debug_assert_eq!(positions.len(), count);
    (positions, indices)
}

/// A sphere built from a subdivided cube (`segments` quads per cube edge), giving
/// near-uniform triangle density with no pole pinching. Vertex count is 6 segments² + 2.
pub fn cube_sphere(segments: u32, radius: f32) -> (Vec<Vec3>, Vec<u32>) {
    box_surface([segments; 3], |p| {
        // Cube-to-sphere mapping with better area uniformity than plain normalisation.
        let (x2, y2, z2) = (p.x * p.x, p.y * p.y, p.z * p.z);
        let s = Vec3::new(
            p.x * (1.0 - y2 / 2.0 - z2 / 2.0 + y2 * z2 / 3.0).sqrt(),
            p.y * (1.0 - z2 / 2.0 - x2 / 2.0 + z2 * x2 / 3.0).sqrt(),
            p.z * (1.0 - x2 / 2.0 - y2 / 2.0 + x2 * y2 / 3.0).sqrt(),
        );
        s.normalize() * radius
    })
}

/// Torus about the Y axis, outer radius `size`, tube radius a quarter of it: `3n` segments
/// around the ring and `n` around the tube, so the cells are about square.
fn ring(n: u32, size: f32) -> (Vec<Vec3>, Vec<u32>) {
    let (major, minor) = (3 * n as usize, n as usize);
    let (big, small) = (0.75 * size, 0.25 * size);
    let mut positions = Vec::with_capacity(major * minor);
    for i in 0..major {
        let theta = std::f32::consts::TAU * i as f32 / major as f32;
        for j in 0..minor {
            let phi = std::f32::consts::TAU * j as f32 / minor as f32;
            let r = big + small * phi.cos();
            positions.push(Vec3::new(r * theta.cos(), small * phi.sin(), r * theta.sin()));
        }
    }
    let id = |i: usize, j: usize| ((i % major) * minor + j % minor) as u32;
    let mut indices = Vec::with_capacity(major * minor * 6);
    for i in 0..major {
        for j in 0..minor {
            let (v00, v10, v11, v01) = (id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1));
            indices.extend_from_slice(&[v00, v11, v10, v00, v01, v11]);
        }
    }
    (positions, indices)
}

/// Square in the XY plane facing +Z (the front view), `n` quads per side. The two corner
/// quads that would put a corner on a diagonal are split the other way, so all four
/// corners touch one triangle and stay put when the plane is divided.
fn plane(n: u32, size: f32) -> (Vec<Vec3>, Vec<u32>) {
    let n = n as usize;
    let side = n + 1;
    let mut positions = Vec::with_capacity(side * side);
    for j in 0..side {
        for i in 0..side {
            let (x, y) = (i as f32 / n as f32 * 2.0 - 1.0, j as f32 / n as f32 * 2.0 - 1.0);
            positions.push(Vec3::new(x, y, 0.0) * size);
        }
    }
    let id = |i: usize, j: usize| (j * side + i) as u32;
    let mut indices = Vec::with_capacity(n * n * 6);
    for j in 0..n {
        for i in 0..n {
            let (v00, v10, v11, v01) = (id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1));
            if (i, j) == (0, 0) || (i, j) == (n - 1, n - 1) {
                indices.extend_from_slice(&[v00, v10, v01, v10, v11, v01]);
            } else {
                indices.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
            }
        }
    }
    (positions, indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::SculptMesh;
    use crate::multires::Multires;

    /// The cube sphere exactly as Klay built it before primitives existed, kept to check
    /// that `--segments` still gives the same mesh bit for bit.
    fn reference_cube_sphere(segments: u32, radius: f32) -> (Vec<Vec3>, Vec<u32>) {
        let n = segments;
        let mut ids: HashMap<u64, u32> = HashMap::new();
        let mut positions: Vec<Vec3> = Vec::new();
        let key = |x: u32, y: u32, z: u32| (x as u64) | ((y as u64) << 21) | ((z as u64) << 42);
        let mut vertex = |lattice: [u32; 3], positions: &mut Vec<Vec3>| -> u32 {
            let k = key(lattice[0], lattice[1], lattice[2]);
            *ids.entry(k).or_insert_with(|| {
                let p = Vec3::new(lattice[0] as f32, lattice[1] as f32, lattice[2] as f32) / n as f32 * 2.0 - 1.0;
                let (x2, y2, z2) = (p.x * p.x, p.y * p.y, p.z * p.z);
                let s = Vec3::new(
                    p.x * (1.0 - y2 / 2.0 - z2 / 2.0 + y2 * z2 / 3.0).sqrt(),
                    p.y * (1.0 - z2 / 2.0 - x2 / 2.0 + z2 * x2 / 3.0).sqrt(),
                    p.z * (1.0 - x2 / 2.0 - y2 / 2.0 + x2 * y2 / 3.0).sqrt(),
                );
                positions.push(s.normalize() * radius);
                (positions.len() - 1) as u32
            })
        };
        let mut indices: Vec<u32> = Vec::new();
        for axis in 0..3usize {
            for &side in &[0u32, n] {
                let (u_axis, v_axis) = ((axis + 1) % 3, (axis + 2) % 3);
                for i in 0..n {
                    for j in 0..n {
                        let mut l = |a: u32, b: u32| {
                            let mut lat = [0u32; 3];
                            lat[axis] = side;
                            lat[u_axis] = a;
                            lat[v_axis] = b;
                            vertex(lat, &mut positions)
                        };
                        let v00 = l(i, j);
                        let v10 = l(i + 1, j);
                        let v11 = l(i + 1, j + 1);
                        let v01 = l(i, j + 1);
                        if side == n {
                            indices.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
                        } else {
                            indices.extend_from_slice(&[v00, v11, v10, v00, v01, v11]);
                        }
                    }
                }
            }
        }
        (positions, indices)
    }

    #[test]
    fn the_sphere_is_the_old_cube_sphere_bit_for_bit() {
        for n in [1, 2, 7, 26, 64] {
            let (p, i) = cube_sphere(n, 1.0);
            let (rp, ri) = reference_cube_sphere(n, 1.0);
            assert_eq!(i, ri, "indices at {n}");
            assert!(p.iter().zip(&rp).all(|(a, b)| a.to_array().map(f32::to_bits) == b.to_array().map(f32::to_bits)));
            assert_eq!(p.len(), rp.len());
        }
        let (p, _) = Primitive::Sphere.build(400, 1.0);
        assert_eq!(p.len(), 960_002, "--segments 400 is still the 960k sphere");
    }

    #[test]
    fn names_parse_both_ways() {
        for p in Primitive::ALL {
            assert_eq!(Primitive::parse(p.name()), Some(p));
            assert_eq!(Primitive::parse(p.short()), Some(p));
            assert_eq!(Primitive::parse(&p.short().to_uppercase()), Some(p));
        }
        assert_eq!(Primitive::parse("torus"), Some(Primitive::Ring));
        assert_eq!(Primitive::parse("cone"), None);
    }

    /// Every edge with the number of triangles that use it, and whether each use runs
    /// the other way from its neighbour (consistent winding).
    fn edge_uses(indices: &[u32]) -> HashMap<(u32, u32), (u32, i32)> {
        let mut edges: HashMap<(u32, u32), (u32, i32)> = HashMap::new();
        for t in indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let e = edges.entry((a.min(b), a.max(b))).or_default();
                e.0 += 1;
                e.1 += if a < b { 1 } else { -1 };
            }
        }
        edges
    }

    #[test]
    fn every_primitive_is_a_clean_manifold_with_outward_winding() {
        for p in Primitive::ALL {
            let n = p.default_segments();
            let (pos, idx) = p.build(n, 1.0);
            assert_eq!(pos.len(), p.vertex_count(n), "{p:?} vertex count");
            let around = (3500..4200).contains(&pos.len());
            assert!(around, "{p:?} starts at {} vertices, not about 4k", pos.len());
            // Fits the unit box.
            for q in &pos {
                assert!(q.abs().max_element() <= 1.0 + 1e-5, "{p:?} {q:?}");
            }
            // No degenerate triangles, no repeated corners, every vertex used.
            let mut used = vec![false; pos.len()];
            for t in idx.chunks_exact(3) {
                assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
                let area = (pos[t[1] as usize] - pos[t[0] as usize]).cross(pos[t[2] as usize] - pos[t[0] as usize]).length();
                assert!(area > 1e-7, "{p:?} has a degenerate triangle");
                t.iter().for_each(|&v| used[v as usize] = true);
            }
            assert!(used.iter().all(|&u| u), "{p:?} has a loose vertex");
            // Two triangles per edge (one on a border), wound in opposite directions.
            let edges = edge_uses(&idx);
            let border = edges.values().filter(|e| e.0 == 1).count();
            assert!(edges.values().all(|e| e.0 <= 2), "{p:?} has an edge on more than two triangles");
            assert!(edges.values().filter(|e| e.0 == 2).all(|e| e.1 == 0), "{p:?} winding is inconsistent");
            assert_eq!(border == 0, p.closed(), "{p:?} border edges: {border}");
            let euler = pos.len() as i64 - edges.len() as i64 + (idx.len() / 3) as i64;
            let expect = match p {
                Primitive::Ring => 0,
                Primitive::Plane => 1,
                _ => 2,
            };
            assert_eq!(euler, expect, "{p:?} Euler characteristic");
            // Outward: the signed volume is positive for closed shapes, and the plane faces +Z.
            if p.closed() {
                let volume: f32 = idx
                    .chunks_exact(3)
                    .map(|t| pos[t[0] as usize].dot(pos[t[1] as usize].cross(pos[t[2] as usize])) / 6.0)
                    .sum();
                assert!(volume > 0.1, "{p:?} volume {volume}");
            } else {
                for t in idx.chunks_exact(3) {
                    let n = (pos[t[1] as usize] - pos[t[0] as usize]).cross(pos[t[2] as usize] - pos[t[0] as usize]);
                    assert!(n.z > 0.0);
                }
            }
            // And the chunked mesh accepts it.
            let mesh = SculptMesh::from_raw(pos, idx);
            assert!(!mesh.edges_over_two_triangles(), "{p:?}");
        }
    }

    #[test]
    fn every_primitive_divides_twice_cleanly() {
        for p in Primitive::ALL {
            let n = p.default_segments();
            let (pos, idx) = p.build(n, 1.0);
            let base_bounds = {
                let mut b = crate::mesh::Aabb::empty();
                pos.iter().for_each(|&q| b.grow(q));
                b
            };
            let mut mr = Multires::new(SculptMesh::from_raw(pos, idx), usize::MAX);
            assert!(mr.divide() && mr.divide(), "{p:?}");
            let counts = mr.vertex_counts();
            for l in 1..3 {
                let (parent, child) = (mr.level_mesh(l - 1), mr.level_mesh(l));
                assert_eq!(counts[l], parent.vertex_count() + parent.edge_count(), "{p:?} level {l}");
                assert_eq!(child.triangle_count(), 4 * parent.triangle_count());
                assert!(!child.edges_over_two_triangles());
                let edges = edge_uses(&child.indices);
                assert!(edges.values().filter(|e| e.0 == 2).all(|e| e.1 == 0), "{p:?} winding at level {l}");
                let border = edges.values().filter(|e| e.0 == 1).count();
                assert_eq!(border == 0, p.closed());
                assert!(child.positions.iter().all(|q| q.is_finite()));
            }
            // Smoothing stays inside the base shape's box and keeps its overall size.
            let top = mr.level_mesh(2).bounds();
            assert!(top.min.cmpge(base_bounds.min - 1e-4).all() && top.max.cmple(base_bounds.max + 1e-4).all(), "{p:?}");
            let (e0, e2) = (base_bounds.max - base_bounds.min, top.max - top.min);
            assert!((e2 / e0.max(Vec3::splat(1e-6))).max_element() > 0.9, "{p:?} shrank to {e2:?}");
        }
    }

    #[test]
    fn the_plane_keeps_its_corners_and_straight_border_when_divided() {
        let (pos, idx) = Primitive::Plane.build(4, 1.0);
        let mut mr = Multires::new(SculptMesh::from_raw(pos, idx), usize::MAX);
        assert!(mr.divide() && mr.divide());
        let top = mr.level_mesh(2);
        for c in [Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 0.0)] {
            assert!(top.positions.iter().any(|&q| (q - c).length() < 1e-6), "corner {c:?} moved");
        }
        // Border vertices stay on the square's edges, and it stays flat.
        let edges = edge_uses(&top.indices);
        for (&(a, b), e) in &edges {
            if e.0 == 1 {
                for v in [a, b] {
                    let q = top.positions[v as usize];
                    assert!((q.x.abs() - 1.0).abs() < 1e-5 || (q.y.abs() - 1.0).abs() < 1e-5, "{q:?} left the border");
                }
            }
        }
        assert!(top.positions.iter().all(|q| q.z.abs() < 1e-6));
    }

    #[test]
    fn every_primitive_survives_a_project_round_trip_with_its_levels() {
        for p in Primitive::ALL {
            let (pos, idx) = p.build(6, 1.0);
            let mut mr = Multires::new(SculptMesh::from_raw(pos, idx), usize::MAX);
            assert!(mr.divide() && mr.divide());
            // A sculpted base after dividing: the replay must still rebuild the same stencils.
            mr.set_active(0);
            let m = mr.mesh_mut();
            let c = m.vert_chunk[0];
            let snap = m.snapshot_chunks(&[c]);
            m.positions[0] += Vec3::splat(0.05);
            m.finish_edit(&[c]);
            mr.finish_stroke(0, snap);
            mr.set_active(2);
            let bytes = crate::project::write_to(Vec::new(), &[("A", true, &mr.pack_copy())], 0).unwrap();
            let project = crate::project::read_from(&bytes[..], bytes.len() as u64).unwrap();
            let back = project.tools.into_iter().next().unwrap().packed.try_unpack().expect("rebuilds");
            for l in 0..3 {
                assert_eq!(back.level_mesh(l).indices, mr.level_mesh(l).indices, "{p:?} level {l}");
                let worst = back
                    .level_mesh(l)
                    .positions
                    .iter()
                    .zip(&mr.level_mesh(l).positions)
                    .map(|(a, b)| (*a - *b).length())
                    .fold(0.0f32, f32::max);
                assert!(worst < 1e-5, "{p:?} level {l} differs by {worst}");
            }
        }
    }
}
