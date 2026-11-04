//! Catmull–Clark position rules on a cage made of paired triangles. The triangle
//! subdivision already has the required child vertices: each quad diagonal's
//! midpoint slot is its face point.
use crate::mesh::SculptMesh;
use glam::Vec3;

const NONE: u32 = u32::MAX;

pub struct Rules {
    face_of_triangle: Vec<usize>,
    faces: Vec<[u32; 4]>,
}

impl Rules {
    pub fn new(mesh: &SculptMesh) -> Option<Self> {
        if mesh.quads.is_empty() || mesh.quads.contains(&crate::mesh::LONE) {
            return None;
        }
        let mates = mesh.quad_mates();
        let mut face_of_triangle = vec![0; mates.len()];
        let mut faces = Vec::with_capacity(mates.len() / 2);
        for (t, mate) in mates.into_iter().enumerate() {
            let q = mate?;
            if t < q {
                let (a, b) = (mesh.quads[t] as usize, mesh.quads[q] as usize);
                face_of_triangle[t] = faces.len();
                face_of_triangle[q] = faces.len();
                faces.push([
                    mesh.indices[3 * t + a],
                    mesh.indices[3 * t + (a + 2) % 3],
                    mesh.indices[3 * t + (a + 1) % 3],
                    mesh.indices[3 * q + (b + 2) % 3],
                ]);
            }
        }
        Some(Self {
            face_of_triangle,
            faces,
        })
    }

    /// Visit each polygon containing this vertex (and optionally its edge) once.
    fn around(&self, mesh: &SculptMesh, a: u32, b: u32, mut visit: impl FnMut(usize)) {
        let incident = mesh.incident(a);
        for (i, &t) in incident.iter().enumerate() {
            if b != NONE && !mesh.indices[3 * t as usize..3 * t as usize + 3].contains(&b) {
                continue;
            }
            let f = self.face_of_triangle[t as usize];
            if incident[..i].iter().any(|&q| {
                self.face_of_triangle[q as usize] == f
                    && (b == NONE || mesh.indices[3 * q as usize..3 * q as usize + 3].contains(&b))
            }) {
                continue;
            }
            visit(f);
        }
    }

    pub fn dependencies(
        &self,
        mesh: &SculptMesh,
        [a, b, _, _]: [u32; 4],
        mut visit: impl FnMut(u32),
    ) {
        self.around(mesh, a, b, |f| {
            self.faces[f].into_iter().for_each(&mut visit)
        });
    }

    pub fn smooth(&self, mesh: &SculptMesh, p: &[Vec3], [a, b, c, d]: [u32; 4]) -> Vec3 {
        let at = |v: u32| p[v as usize];
        let centre = |f: usize| self.faces[f].into_iter().map(at).sum::<Vec3>() * 0.25;
        let mut sum = Vec3::ZERO;
        let mut count = 0;
        if b == NONE {
            let mut neighbours = Vec3::ZERO;
            self.around(mesh, a, b, |f| {
                let face = self.faces[f];
                let i = face.iter().position(|&v| v == a).unwrap();
                sum += centre(f);
                neighbours += at(face[(i + 1) % 4]) + at(face[(i + 3) % 4]);
                count += 1;
            });
            if count <= 1 {
                at(a)
            } else if c != NONE {
                at(a) * 0.75 + (at(c) + at(d)) * 0.125
            } else {
                let n = count as f32;
                // F is the mean of face points; 2R is P plus the mean of neighbours.
                ((n - 2.0) * at(a) + (sum + neighbours * 0.5) / n) / n
            }
        } else {
            self.around(mesh, a, b, |f| {
                sum += centre(f);
                count += 1;
            });
            if d == NONE || a > b {
                (at(a) + at(b)) * 0.5
            } else if count == 1 {
                sum
            } else {
                (at(a) + at(b) + sum) * 0.25
            }
        }
    }
}
