//! Dynamics (ZBrush: Tool › Dynamics): the shown level simulated as cloth, so it drapes
//! under gravity over the other visible subtools, inflates, or settles.
//!
//! Position-based dynamics: each step moves every vertex by its velocity and the forces,
//! then pulls each edge back toward its rest length (**Stiffness**) and each pair of
//! vertices across an edge toward theirs (**Bend**, which keeps folds from crumpling),
//! then pushes anything that went into a collider or the floor back out to **Thickness**.
//! The corrections are gathered per vertex (Jacobi), so a step runs in parallel. Masked
//! vertices are pinned (partly masked ones partly), as ZBrush pins them.

use glam::Vec3;
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Simulation steps.
    pub steps: u32,
    /// Pull down, in units of the model's size per second squared (0 for none).
    pub gravity: f32,
    /// Push along the normals each step, out (positive) or in (negative).
    pub inflate: f32,
    /// How firmly edges keep their length, 0 to 1.
    pub stiffness: f32,
    /// How firmly the surface resists folding, 0 to 1.
    pub bend: f32,
    /// Distance kept from colliders and the floor, as a share of the model's size.
    pub thickness: f32,
    /// Collide with a floor under the lowest point of everything visible.
    pub floor: bool,
    /// How much sliding a contact stops, 0 to 1.
    pub friction: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { steps: 150, gravity: 1.0, inflate: 0.0, stiffness: 0.9, bend: 0.1, thickness: 0.005, floor: true, friction: 0.6 }
    }
}

/// What a simulation runs on, copied out of the subtool so it can run on a thread.
pub struct Cloth {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Per vertex: how free it is (1 free, 0 pinned).
    pub free: Vec<f32>,
    /// Edges and bend pairs, per vertex: (other vertex, rest length).
    stretch: Vec<Vec<(u32, f32)>>,
    bend: Vec<Vec<(u32, f32)>>,
    /// The other visible subtools' triangles.
    colliders: Option<crate::project_all::Sources>,
    floor_y: f32,
    size: f32,
}

impl Cloth {
    /// `mesh` as cloth, pinned where masked, colliding with `colliders` (triangles of the
    /// other visible subtools).
    pub fn new(mesh: &crate::mesh::SculptMesh, colliders: Option<crate::project_all::Sources>, floor_y: f32) -> Self {
        let n = mesh.vertex_count();
        let p = &mesh.positions;
        let stretch: Vec<Vec<(u32, f32)>> = (0..n as u32).into_par_iter().map(|v| mesh.neighbors(v).iter().map(|&w| (w, p[v as usize].distance(p[w as usize]))).collect()).collect();
        let mut bend = vec![Vec::new(); n];
        for (_, _, t, u) in crate::crease::edges(mesh) {
            if u == u32::MAX {
                continue;
            }
            let tri = |x: u32| &mesh.indices[3 * x as usize..3 * x as usize + 3];
            let (ta, tb) = (tri(t), tri(u));
            let (Some(&c), Some(&d)) = (ta.iter().find(|v| !tb.contains(v)), tb.iter().find(|v| !ta.contains(v))) else { continue };
            let r = p[c as usize].distance(p[d as usize]);
            bend[c as usize].push((d, r));
            bend[d as usize].push((c, r));
        }
        let (lo, hi) = p.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &q| (a.min(q), b.max(q)));
        let free = (0..n).map(|v| crate::mask::freedom(&mesh.mask, v as u32) * if mesh.shown.is_empty() || mesh.shown[v] != 0 { 1.0 } else { 0.0 }).collect();
        Self { positions: p.clone(), indices: mesh.indices.clone(), free, stretch, bend, colliders, floor_y, size: (hi - lo).max_element().max(1e-6) }
    }

    fn normals(&self, x: &[Vec3]) -> Vec<Vec3> {
        let mut n = vec![Vec3::ZERO; x.len()];
        for t in self.indices.chunks_exact(3) {
            let f = (x[t[1] as usize] - x[t[0] as usize]).cross(x[t[2] as usize] - x[t[0] as usize]);
            for &v in t {
                n[v as usize] += f;
            }
        }
        n.into_iter().map(|v| v.normalize_or_zero()).collect()
    }

    /// Run `s.steps` steps (or until `stop` is set), calling `progress` with the step
    /// number and the positions every few steps. Returns the final positions.
    pub fn run(&self, s: &Settings, stop: &std::sync::atomic::AtomicBool, mut progress: impl FnMut(u32, &[Vec3])) -> Vec<Vec3> {
        let dt = 1.0 / 60.0;
        let g = Vec3::NEG_Y * (s.gravity * self.size * 4.0);
        let thick = s.thickness * self.size;
        let mut x = self.positions.clone();
        let mut prev = x.clone();
        const SOLVE: usize = 20;
        for step in 0..s.steps {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let normals = if s.inflate != 0.0 { self.normals(&x) } else { Vec::new() };
            // Move by velocity (with a little damping) and the forces.
            let moved: Vec<Vec3> = (0..x.len())
                .into_par_iter()
                .map(|v| {
                    let f = self.free[v];
                    if f <= 0.0 {
                        return x[v];
                    }
                    let vel = (x[v] - prev[v]) * 0.98;
                    let push = normals.get(v).map_or(Vec3::ZERO, |n| *n * (s.inflate * self.size * 0.002));
                    x[v] + (vel + g * dt * dt + push) * f
                })
                .collect();
            prev = std::mem::replace(&mut x, moved);
            for _ in 0..SOLVE {
                x = (0..x.len())
                    .into_par_iter()
                    .map(|v| {
                        let wv = self.free[v];
                        if wv <= 0.0 {
                            return x[v];
                        }
                        let mut corr = Vec3::ZERO;
                        let mut count = 0.0f32;
                        for (pairs, k) in [(&self.stretch[v], s.stiffness), (&self.bend[v], s.bend)] {
                            if k <= 0.0 {
                                continue;
                            }
                            for &(w, rest) in pairs {
                                let ww = self.free[w as usize];
                                let d = x[w as usize] - x[v];
                                let len = d.length();
                                if len < 1e-9 {
                                    continue;
                                }
                                corr += d / len * ((len - rest) * k * wv / (wv + ww));
                                count += 1.0;
                            }
                        }
                        // Over-relaxed average of the corrections.
                        if count > 0.0 { x[v] + corr * (1.5 / count.max(1.0)).min(1.0) } else { x[v] }
                    })
                    .collect();
            }
            // Collisions: out of the colliders and above the floor, with friction holding
            // back the sliding along the contact.
            let mu = s.friction.clamp(0.0, 1.0);
            x.par_iter_mut().zip(&self.free).zip(&prev).for_each(|((q, &f), &before)| {
                if f <= 0.0 {
                    return;
                }
                let mut contact = None;
                if let Some(c) = &self.colliders {
                    if let Some((at, n)) = c.closest(*q, Vec3::ZERO) {
                        let depth = (*q - at).dot(n);
                        if depth < thick && (*q - at).length() < thick * 4.0 + self.size * 0.05 {
                            *q = at + (*q - at).reject_from(n) + n * thick;
                            contact = Some(n);
                        }
                    }
                }
                if s.floor && q.y < self.floor_y + thick {
                    q.y = self.floor_y + thick;
                    contact = Some(Vec3::Y);
                }
                if let Some(n) = contact {
                    let slide = (*q - before).reject_from(n);
                    *q -= slide * mu;
                }
            });
            if step % 4 == 3 {
                progress(step + 1, &x);
            }
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::SculptMesh;
    use std::sync::atomic::AtomicBool;

    /// A flat cloth facing up at height `y`.
    fn cloth(y: f32) -> SculptMesh {
        let (p, i) = crate::primitive::Primitive::Plane.build(16, 1.0);
        // The plane faces along an axis; turn it to lie flat (in X and Z) at `y`.
        let flat = (0..3).find(|&k| p.iter().all(|q| q[k].abs() < 1e-6)).unwrap();
        let p = p.into_iter().map(|q| {
            let mut r = q;
            r[flat] = 0.0;
            let (a, b) = match flat { 0 => (q.y, q.z), 1 => (q.x, q.z), _ => (q.x, q.y) };
            Vec3::new(a, y + r[flat], b)
        }).collect();
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn a_cloth_pinned_along_one_edge_hangs_and_keeps_its_edges() {
        let mut m = cloth(0.0);
        let xmin = m.positions.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        m.mask = m.positions.iter().map(|p| if p.x < xmin + 1e-4 { 255 } else { 0 }).collect();
        let c = Cloth::new(&m, None, -10.0);
        let out = c.run(&Settings { steps: 200, floor: false, ..Default::default() }, &AtomicBool::new(false), |_, _| {});
        for (a, b) in m.positions.iter().zip(&out) {
            if a.x < xmin + 1e-4 {
                assert_eq!(a, b, "pinned");
            }
        }
        let low = out.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(low < -1.0, "it hangs: {low}");
        // Edges stretch only a little.
        let stretch = c.stretch.iter().enumerate().flat_map(|(v, l)| l.iter().map(move |&(w, r)| (v, w, r))).map(|(v, w, r)| out[v].distance(out[w as usize]) / r).fold(0.0, f32::max);
        assert!(stretch < 1.25, "{stretch}");
    }

    #[test]
    fn a_cloth_drapes_over_a_sphere_and_rests_on_the_floor() {
        let m = cloth(1.3);
        let (p, i) = crate::mesh::cube_sphere(16, 0.6);
        let sphere = SculptMesh::from_raw(p, i);
        let col = crate::project_all::Sources::new(&[&sphere]);
        let c = Cloth::new(&m, Some(col), -0.6);
        let out = c.run(&Settings { steps: 300, ..Default::default() }, &AtomicBool::new(false), |_, _| {});
        // Nothing went into the sphere; the middle rests on top of it; the corners fell
        // below its top.
        assert!(out.iter().all(|p| p.length() > 0.6 - 0.02), "{}", out.iter().map(|p| p.length()).fold(f32::INFINITY, f32::min));
        let top = out.iter().filter(|p| p.x.abs() < 0.1 && p.z.abs() < 0.1).map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        assert!((top - 0.6).abs() < 0.1, "{top}");
        assert!(out.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) < 0.2);
        assert!(out.iter().all(|p| p.y >= -0.6 - 1e-4), "above the floor");
    }

    /// A picture of a cloth draped over a sphere, into `$KLAY_BAKE_DIR/drape.png`.
    #[test]
    #[ignore]
    fn drape_picture() {
        let dir = std::path::PathBuf::from(std::env::var("KLAY_BAKE_DIR").expect("KLAY_BAKE_DIR"));
        let mut m = cloth(1.3);
        let (p, i) = crate::mesh::cube_sphere(24, 0.6);
        let sphere = SculptMesh::from_raw(p, i);
        let c = Cloth::new(&m, Some(crate::project_all::Sources::new(&[&sphere])), -0.6);
        m.positions = c.run(&Settings { steps: 400, ..Default::default() }, &AtomicBool::new(false), |_, _| {});
        let mut cam = crate::camera::Camera::new();
        cam.aspect = 1.0;
        cam.distance = 4.0;
        let mut sc = crate::bpr::Scene::new(cam, 600, 600, crate::matcap::Look::WhiteClay, glam::Vec3::new(0.78, 0.62, 0.5), [40, 42, 50]);
        sc.add(&SculptMesh::from_raw(m.positions.clone(), m.indices.clone()));
        sc.add(&sphere);
        let img = crate::bpr::render(&sc, &crate::bpr::Settings::default());
        image::save_buffer(dir.join("drape.png"), &img.rgba, img.width, img.height, image::ColorType::Rgba8).unwrap();
    }
}
