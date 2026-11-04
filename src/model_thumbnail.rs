//! Small, transparent model previews for LightBox. No window or GPU is needed.
use crate::{camera::Camera, model_import, primitive::Primitive};
use egui::{Color32, ColorImage};
use glam::{Mat3, Vec2, Vec3};
use std::path::Path;

pub(crate) struct Geometry {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    pub colors: Vec<[u8; 4]>,
}

pub fn is_model(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("klay") || model_import::accepts_extension(ext)
    })
}

pub fn from_file(path: &Path, stl_z_up: bool) -> Option<ColorImage> {
    let ext = path.extension()?;
    if ext.eq_ignore_ascii_case("klay") {
        let project = crate::project::open(path).ok()?;
        let mut meshes = Vec::new();
        for (i, tool) in project.tools.into_iter().enumerate() {
            if (i == project.active || tool.visible)
                && let Ok(mesh) = tool.packed.into_preview_geometry()
            {
                meshes.push(mesh);
            }
        }
        return render(&meshes);
    }
    let meshes: Vec<_> = std::panic::catch_unwind(|| model_import::read(path, stl_z_up))
        .ok()?.ok()?.into_iter()
        .map(|part| Geometry {
            positions: part.mesh.positions,
            indices: part.mesh.indices,
            colors: part.mesh.colors,
        }).collect();
    render(&meshes)
}

pub fn primitive(kind: Primitive) -> Option<ColorImage> {
    let (positions, indices) = kind.build(kind.default_segments(), 1.0);
    render(&[Geometry {
        positions,
        indices,
        colors: Vec::new(),
    }])
}

/// Fit the visible triangles into a square and rasterize their depth and vertex colors.
fn render(meshes: &[Geometry]) -> Option<ColorImage> {
    let mut camera = Camera::new();
    if let Some(image) = render_view(meshes, &camera) {
        return Some(image);
    }
    // A flat model can be edge-on from the default angle.
    camera.pitch = 0.0;
    camera.yaw = 0.0;
    render_view(meshes, &camera).or_else(|| {
        camera.yaw = std::f32::consts::FRAC_PI_2;
        render_view(meshes, &camera)
    })
}

fn render_view(meshes: &[Geometry], camera: &Camera) -> Option<ColorImage> {
    const SIZE: usize = 160;
    let view = Mat3::from_cols(camera.right(), -camera.up(), -camera.forward()).transpose();
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut projected = Vec::with_capacity(meshes.len());
    for mesh in meshes {
        let points: Vec<Vec3> = mesh.positions.iter().map(|&p| view * p).collect();
        for tri in mesh.indices.chunks_exact(3) {
            let Some(points) = triangle(&points, tri) else {
                continue;
            };
            for p in points {
                min = min.min(p.truncate());
                max = max.max(p.truncate());
            }
        }
        projected.push(points);
    }
    let span = (max - min).max_element();
    if !span.is_finite() || span <= 0.0 {
        return None;
    }
    let center = min.lerp(max, 0.5);
    let scale = SIZE as f32 * 0.88 / span;
    let middle = Vec2::splat(SIZE as f32 * 0.5);
    let mut image = ColorImage::filled([SIZE, SIZE], Color32::TRANSPARENT);
    let mut depth = vec![f32::NEG_INFINITY; SIZE * SIZE];
    let light = (view * crate::bpr::light_dir(camera)).normalize();
    let clay = Vec3::new(201.0, 171.0, 147.0);

    for (mesh, mut points) in meshes.iter().zip(projected) {
        // Area-weighted normals, without building a sculpting mesh or its adjacency.
        let mut normals = vec![Vec3::ZERO; points.len()];
        for tri in mesh.indices.chunks_exact(3) {
            let Some([a, b, c]) = triangle(&points, tri) else {
                continue;
            };
            let normal = (b - a).cross(c - a);
            for &i in tri {
                normals[i as usize] += normal;
            }
        }
        for (i, normal) in normals.iter_mut().enumerate() {
            *normal = {
                let normal = normal.normalize_or(Vec3::Z);
                let facing = if normal.z < 0.0 { -normal } else { normal };
                let [r, g, b, coverage] = mesh.colors.get(i).copied().unwrap_or([0; 4]);
                let base = Vec3::new(r as f32, g as f32, b as f32)
                    + clay * (1.0 - coverage as f32 / 255.0);
                base * (0.39 + 0.61 * facing.dot(light).max(0.0))
            };
        }
        let colors = normals;
        for point in &mut points {
            let pixel = (point.truncate() - center) * scale + middle;
            point.x = pixel.x;
            point.y = pixel.y;
        }
        for tri in mesh.indices.chunks_exact(3) {
            let Some([a, b, c]) = triangle(&points, tri) else {
                continue;
            };
            let ab = (b - a).truncate();
            let ac = (c - a).truncate();
            let area = ab.perp_dot(ac);
            if area.abs() < 1e-7 {
                continue;
            }

            // Pixels are sampled at their centers. Most triangles of a dense sculpt
            // cover none, so skip them before computing the barycentric weights.
            let low = (a.min(b).min(c).truncate() - Vec2::splat(0.5))
                .ceil()
                .max(Vec2::ZERO);
            let high = (a.max(b).max(c).truncate() - Vec2::splat(0.5))
                .floor()
                .min(Vec2::splat((SIZE - 1) as f32));
            if low.x > high.x || low.y > high.y {
                continue;
            }
            let [ca, cb, cc] = [
                colors[tri[0] as usize],
                colors[tri[1] as usize],
                colors[tri[2] as usize],
            ];
            let inv = area.recip();
            for y in low.y as usize..=high.y as usize {
                for x in low.x as usize..=high.x as usize {
                    let offset = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - a.truncate();
                    let u = offset.perp_dot(ac) * inv;
                    let v = ab.perp_dot(offset) * inv;
                    let w = 1.0 - u - v;
                    if u < -1e-6 || v < -1e-6 || w < -1e-6 {
                        continue;
                    }
                    let z = a.z * w + b.z * u + c.z * v;
                    let index = y * SIZE + x;
                    if z > depth[index] {
                        depth[index] = z;
                        let rgb = ca * w + cb * u + cc * v;
                        image.pixels[index] =
                            Color32::from_rgb(rgb.x as u8, rgb.y as u8, rgb.z as u8);
                    }
                }
            }
        }
    }
    image.pixels.iter().any(|p| p.a() != 0).then_some(image)
}

fn triangle(points: &[Vec3], tri: &[u32]) -> Option<[Vec3; 3]> {
    let p = [
        *points.get(tri[0] as usize)?,
        *points.get(tri[1] as usize)?,
        *points.get(tri[2] as usize)?,
    ];
    p.iter().all(|v| v.is_finite()).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_models_make_thumbnails_without_sidecars() {
        let folder = std::env::temp_dir().join(format!("klay-thumbnails-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let obj = folder.join("painted.OBJ");
        std::fs::write(
            &obj,
            "v 0 0 0 1 0 0\nv 1 0 0 1 0 0\nv 0 1 0 1 0 0\nf 1 2 3\n",
        )
        .unwrap();
        let stl = folder.join("solid.STL");
        let (positions, indices) = Primitive::Cube.build(2, 1.0);
        crate::stl::write_binary(
            std::fs::File::create(&stl).unwrap(),
            "Cube",
            &positions,
            &indices,
            |p| p,
        )
        .unwrap();

        let project = folder.join("legacy.klay");
        let packed = crate::multires::Multires::new(
            crate::mesh::SculptMesh::from_raw(positions, indices),
            50_000,
        )
        .pack();
        crate::project::write_version(
            std::fs::File::create(&project).unwrap(),
            &[("Cube", true, &packed)],
            0,
            1,
        )
        .unwrap();
        let painted = from_file(&obj, false).unwrap();
        assert!(
            painted
                .pixels
                .iter()
                .any(|p| p.r() > 80 && p.g() == 0 && p.a() == 255)
        );
        for path in [&obj, &stl, &project] {
            let preview = from_file(path, true).unwrap();
            assert!(preview.pixels.iter().filter(|p| p.a() == 255).count() > 1000);
            assert!(preview.pixels.iter().any(|p| p.a() == 0));
        }
        for kind in Primitive::ALL {
            assert!(primitive(kind).is_some(), "{kind:?}");
        }
        std::fs::remove_dir_all(folder).unwrap();
    }
}
