//! Render (ZBrush: BPR, Best Preview Render): the view as it is, drawn again by ray tracing
//! on the CPU with what the viewport leaves out: shadows cast by the model on itself and
//! ambient occlusion in its creases, antialiased, at up to twice the canvas size.
//!
//! Every visible subtool is gathered into one triangle tree ([`crate::project_all::Sources`]).
//! Each pixel sends a few jittered rays; where one hits, the surface is shaded with the
//! material's MatCap by its smooth normal turned into view space, as the viewport does,
//! tinted by polypaint, then darkened by the share of short rays over its hemisphere that
//! hit something (ambient occlusion) and by whether the light reaches it (shadow).

use crate::matcap::Look;
use crate::project_all::Sources;
use glam::{Vec2, Vec3};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Pixels per canvas pixel (1 or 2).
    pub scale: u32,
    /// Rays per pixel along each side (1 to 4).
    pub samples: u32,
    /// Ambient occlusion rays per sample (0 for none).
    pub ao_rays: u32,
    /// How dark full occlusion is (0 to 1).
    pub ao_strength: f32,
    /// How dark shadow is (0 to 1; 0 for none).
    pub shadow: f32,
    /// A transparent background instead of the document's colour.
    pub transparent: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { scale: 1, samples: 2, ao_rays: 10, ao_strength: 0.75, shadow: 0.5, transparent: false }
    }
}

/// What is rendered: triangles with smooth normals and premultiplied sRGB paint per
/// vertex (empty where unpainted), and how it is seen.
pub struct Scene {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub colors: Vec<[u8; 4]>,
    pub triangles: Vec<[u32; 3]>,
    pub camera: crate::camera::Camera,
    /// Canvas size in pixels.
    pub width: u32,
    pub height: u32,
    pub look: Look,
    /// The surface colour where unpainted, linear.
    pub base: Vec3,
    /// The background, sRGB.
    pub background: [u8; 3],
}

impl Scene {
    pub fn new(camera: crate::camera::Camera, width: u32, height: u32, look: Look, base: Vec3, background: [u8; 3]) -> Self {
        Self { positions: Vec::new(), normals: Vec::new(), colors: Vec::new(), triangles: Vec::new(), camera, width, height, look, base, background }
    }

    /// Add the visible triangles of `mesh`.
    pub fn add(&mut self, mesh: &crate::mesh::SculptMesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&mesh.positions);
        self.normals.extend((0..mesh.vertex_count() as u32).into_par_iter().map(|v| mesh.normal_at(v)).collect::<Vec<_>>());
        if mesh.colors.len() == mesh.vertex_count() {
            if self.colors.len() < base as usize {
                self.colors.resize(base as usize, [0; 4]);
            }
            self.colors.extend_from_slice(&mesh.colors);
        } else if !self.colors.is_empty() {
            self.colors.extend(std::iter::repeat_n([0u8; 4], mesh.vertex_count()));
        }
        for (t, c) in mesh.indices.chunks_exact(3).enumerate() {
            if mesh.visible(t as u32) {
                self.triangles.push([c[0] + base, c[1] + base, c[2] + base]);
            }
        }
    }
}

/// A rendered image, RGBA8 straight alpha.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round() as u8
}

fn to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A small fixed hash to jitter rays, so renders are repeatable.
fn hash(mut x: u32) -> f32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x & 0xff_ffff) as f32 / 16_777_216.0
}

/// Toward the key light: from the upper left, in front, like the MatCaps' highlights.
pub fn light_dir(cam: &crate::camera::Camera) -> Vec3 {
    let (forward, up) = (cam.forward(), cam.up());
    let right = forward.cross(up).normalize();
    (up * 0.8 - right * 0.5 - forward * 0.6).normalize()
}

/// Render `scene`.
pub fn render(scene: &Scene, s: &Settings) -> Image {
    // The MatCap images are made on first use with rayon; made inside the parallel loop
    // below, the workers waiting for them would starve the one making them.
    let _ = crate::matcap::library();
    let scale = s.scale.clamp(1, 4);
    let (w, h) = (scene.width * scale, scene.height * scale);
    let tree = Sources::from_triangles(scene.positions.clone(), scene.triangles.clone());
    let (lo, hi) = scene.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)));
    let size = (hi - lo).max_element().max(1e-6);
    let ao_reach = size * 0.15;
    let eps = size * 1e-3;
    let cam = &scene.camera;
    let forward = cam.forward();
    let up = cam.up();
    let right = forward.cross(up).normalize();
    let light = light_dir(cam);
    // View space: x right, y up, z toward the viewer.
    let to_view = |n: Vec3| Vec3::new(n.dot(right), n.dot(up), -n.dot(forward));
    let n_side = s.samples.clamp(1, 4);
    let bg = scene.background.map(to_linear);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    rgba.par_chunks_mut((w * 4) as usize).enumerate().for_each(|(y, row)| {
        for x in 0..w as usize {
            let mut sum = Vec3::ZERO;
            let mut covered = 0.0;
            for sy in 0..n_side {
                for sx in 0..n_side {
                    let seed = ((y * w as usize + x) as u32).wrapping_mul(16) + sy * 4 + sx;
                    let jitter = Vec2::new((sx as f32 + hash(seed)) / n_side as f32, (sy as f32 + hash(seed ^ 0x9e37)) / n_side as f32);
                    let px = (x as f32 + jitter.x) / scale as f32;
                    let py = (y as f32 + jitter.y) / scale as f32;
                    let (o, d) = cam.ray(px, py, scene.width as f32, scene.height as f32);
                    let Some(hit) = tree.hit(o, d, f32::INFINITY) else { continue };
                    covered += 1.0;
                    let [a, b, c] = tree.triangle(hit.tri);
                    let wts = [1.0 - hit.u - hit.v, hit.u, hit.v];
                    let corner = [a, b, c];
                    let mut n = Vec3::ZERO;
                    for k in 0..3 {
                        n += scene.normals[corner[k] as usize] * wts[k];
                    }
                    let mut n = n.normalize_or(hit.normal);
                    if n.dot(d) > 0.0 {
                        n = -n;
                    }
                    let geo = if hit.normal.dot(d) > 0.0 { -hit.normal } else { hit.normal };
                    let p = o + d * hit.t + geo * eps;
                    // Base colour: polypaint where painted.
                    let mut base = scene.base;
                    if !scene.colors.is_empty() {
                        let mut paint = Vec3::ZERO;
                        let mut alpha = 0.0;
                        for k in 0..3 {
                            let col = scene.colors[corner[k] as usize];
                            paint += Vec3::new(to_linear(col[0]), to_linear(col[1]), to_linear(col[2])) * wts[k];
                            alpha += col[3] as f32 / 255.0 * wts[k];
                        }
                        if alpha > 1e-3 {
                            // Premultiplied: back to the colour, then over the base.
                            base = base.lerp(paint / alpha, alpha.min(1.0));
                        }
                    }
                    let mut colour = crate::matcap::shade(scene.look, base, to_view(n));
                    if s.ao_rays > 0 && s.ao_strength > 0.0 {
                        let (t1, t2) = {
                            let t = n.any_orthonormal_vector();
                            (t, n.cross(t))
                        };
                        let mut blocked = 0;
                        for k in 0..s.ao_rays {
                            // Cosine-weighted directions over the hemisphere.
                            let (r1, r2) = (hash(seed.wrapping_mul(31).wrapping_add(k * 2)), hash(seed.wrapping_mul(17).wrapping_add(k * 2 + 1)));
                            let phi = std::f32::consts::TAU * r1;
                            let rr = r2.sqrt();
                            let dir = (t1 * (phi.cos() * rr) + t2 * (phi.sin() * rr) + n * (1.0 - r2).max(0.0).sqrt()).normalize();
                            if tree.hit(p, dir, ao_reach).is_some() {
                                blocked += 1;
                            }
                        }
                        colour *= 1.0 - s.ao_strength * blocked as f32 / s.ao_rays as f32;
                    }
                    // Shadow only where the triangle itself faces the light: past its edge the
                    // smooth normal may still face it, and the ray would hit the next triangle
                    // (shadow acne). The light is jittered a little for soft edges.
                    if s.shadow > 0.0 && geo.dot(light) > 0.02 {
                        let (t1, t2) = (light.any_orthonormal_vector(), light.cross(light.any_orthonormal_vector()));
                        let j = Vec2::new(hash(seed ^ 0x51ed), hash(seed ^ 0x2c1b)) * 2.0 - Vec2::ONE;
                        let dir = (light + (t1 * j.x + t2 * j.y) * 0.06).normalize();
                        if tree.hit(p, dir, f32::INFINITY).is_some() {
                            colour *= 1.0 - s.shadow;
                        }
                    }
                    sum += colour;
                }
            }
            let total = (n_side * n_side) as f32;
            let cover = covered / total;
            let px = &mut row[x * 4..x * 4 + 4];
            if s.transparent {
                let c = if covered > 0.0 { sum / covered } else { Vec3::ZERO };
                px.copy_from_slice(&[to_srgb(c.x), to_srgb(c.y), to_srgb(c.z), (cover * 255.0).round() as u8]);
            } else {
                let c = (sum + Vec3::from_array(bg) * (total - covered)) / total;
                px.copy_from_slice(&[to_srgb(c.x), to_srgb(c.y), to_srgb(c.z), 255]);
            }
        }
    });
    Image { width: w, height: h, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::SculptMesh;

    fn scene(meshes: &[SculptMesh]) -> Scene {
        let mut cam = crate::camera::Camera::new();
        cam.aspect = 1.0;
        let mut s = Scene::new(cam, 64, 64, Look::WhiteClay, Look::WhiteClay.base(), [20, 20, 30]);
        for m in meshes {
            s.add(m);
        }
        s
    }

    fn brightness(img: &Image, x: u32, y: u32) -> f32 {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i..i + 3].iter().map(|&c| c as f32).sum::<f32>() / 3.0
    }

    #[test]
    fn a_sphere_renders_lit_on_the_background_and_transparent_outside() {
        let (p, i) = crate::mesh::cube_sphere(12, 0.6);
        let img = render(&scene(&[SculptMesh::from_raw(p.clone(), i.clone())]), &Settings::default());
        assert_eq!((img.width, img.height), (64, 64));
        assert!(brightness(&img, 32, 32) > 120.0, "the model is lit");
        assert_eq!(&img.rgba[..3], &[20, 20, 30], "the corner is background");
        let t = render(&scene(&[SculptMesh::from_raw(p, i)]), &Settings { transparent: true, scale: 2, ..Default::default() });
        assert_eq!((t.width, t.rgba[3], t.rgba[((64 * 128 + 64) * 4 + 3) as usize]), (128, 0, 255));
    }

    #[test]
    fn a_crease_darkens_with_occlusion_and_a_blocker_casts_a_shadow() {
        // Two spheres touching: the gap between them is darker with AO than without.
        let (p, i) = crate::mesh::cube_sphere(12, 0.4);
        let a = SculptMesh::from_raw(p.iter().map(|q| *q + Vec3::new(-0.38, 0.0, 0.0)).collect(), i.clone());
        let b = SculptMesh::from_raw(p.iter().map(|q| *q + Vec3::new(0.38, 0.0, 0.0)).collect(), i.clone());
        let sc = scene(&[a, b]);
        let plain = render(&sc, &Settings { ao_rays: 0, shadow: 0.0, ..Default::default() });
        let ao = render(&sc, &Settings { ao_rays: 16, shadow: 0.0, ..Default::default() });
        let total = |img: &Image| (0..img.width * img.height).map(|k| brightness(img, k % img.width, k / img.width)).sum::<f32>();
        assert!(total(&ao) < total(&plain) - 500.0, "{} {}", total(&ao), total(&plain));
        // A small sphere between a big one and the light shadows it.
        let cam = crate::camera::Camera::new();
        let (p, i) = crate::mesh::cube_sphere(12, 0.5);
        let big = SculptMesh::from_raw(p, i);
        let (q, j) = crate::mesh::cube_sphere(8, 0.15);
        let blocker = SculptMesh::from_raw(q.iter().map(|x| *x + light_dir(&cam) * 0.85).collect(), j);
        let sc = scene(&[big, blocker]);
        let lit = render(&sc, &Settings { ao_rays: 0, shadow: 0.0, ..Default::default() });
        let shadowed = render(&sc, &Settings { ao_rays: 0, shadow: 0.6, ..Default::default() });
        assert!(total(&shadowed) < total(&lit) - 500.0, "{} {}", total(&shadowed), total(&lit));
    }
}
