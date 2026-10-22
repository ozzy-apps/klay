//! Original clay studies for Klay's brush library. No borrowed ZBrush artwork.
use crate::sculpt::BrushKind;
use egui::ColorImage;
use glam::{Vec2, Vec3};
use std::f32::consts::PI;

pub fn image(kind: Option<BrushKind>, material: usize, invert: bool) -> ColorImage {
    let n = 160usize;
    let radius = if kind == Some(BrushKind::SnakeHook) {
        0.70f32
    } else {
        0.84f32
    };
    let c = crate::workspace::MATERIALS[material % 4].0;
    let base = Vec3::new(c.r() as f32, c.g() as f32, c.b() as f32) / 255.0;
    let sign = if invert { -1.0 } else { 1.0 };
    let spiral: Vec<Vec2> = (0..90)
        .map(|i| {
            let t = i as f32 / 89.0;
            let a = t * PI * 2.65 - 0.9;
            let r = 0.035 + t * 0.46;
            Vec2::new(r * a.cos() - 0.035, r * a.sin() * 0.9)
        })
        .collect();
    let curve: Vec<Vec2> = (0..64)
        .map(|i| {
            let t = i as f32 / 63.0;
            Vec2::new(-0.48 + t * 0.96, 0.22 * (t * PI * 1.8).sin() - 0.12)
        })
        .collect();
    let distance = |p: Vec2, path: &[Vec2]| {
        path.windows(2)
            .map(|e| {
                let d = e[1] - e[0];
                let t = ((p - e[0]).dot(d) / d.length_squared().max(1e-8)).clamp(0.0, 1.0);
                p.distance(e[0] + d * t)
            })
            .fold(f32::INFINITY, f32::min)
    };
    let noise = |p: Vec2| {
        (p.x * 29.0 + p.y * 13.0).sin() * (p.y * 31.0 - p.x * 7.0).sin() * 0.55
            + (p.x * 53.0 + p.y * 41.0).sin() * 0.25
            + (p.x * 17.0 - p.y * 23.0).cos() * 0.2
    };
    let warp = |p: Vec2| {
        if !matches!(
            kind,
            Some(BrushKind::Grab | BrushKind::Elastic | BrushKind::SnakeHook)
        ) {
            return p;
        }
        let angle = p.y.atan2(p.x) + 0.65;
        let (amount, width) = match kind {
            Some(BrushKind::SnakeHook) => (0.75, 0.13),
            Some(BrushKind::Elastic) => (0.28, 0.7),
            _ => (0.33, 0.22),
        };
        let pull = amount
            * (-(angle / width).powi(2)).exp()
            * (p.length() / radius).clamp(0.0, 1.0).powi(2);
        p / (1.0 + pull)
    };
    let shape = |p: Vec2| {
        let sphere = radius - warp(p).length();
        if kind == Some(BrushKind::ClipLine) {
            sphere.min(0.30 - p.x)
        } else {
            sphere
        }
    };
    let height = |p: Vec2| -> f32 {
        let q = warp(p);
        let z = (radius * radius - q.length_squared()).max(0.001).sqrt();
        let s = distance(p, &spiral);
        let d = distance(p, &curve);
        let soft = |d: f32, w: f32| (-(d / w).powi(2) * 2.0).exp();
        let h = match kind {
            None
            | Some(
                BrushKind::Grab | BrushKind::Elastic | BrushKind::SnakeHook | BrushKind::Paint,
            ) => 0.0,
            Some(BrushKind::Draw) => soft(s, 0.10) * 0.085 * sign,
            Some(BrushKind::DamStandard) => {
                -soft(d, 0.042) * 0.10 * sign + soft(d - 0.065, 0.03) * 0.012 * sign
            }
            Some(BrushKind::Pinch) => {
                soft(d, 0.026) * 0.055 * sign - soft(d - 0.055, 0.025) * 0.012 * sign
            }
            Some(BrushKind::Crease) => {
                -soft(s, 0.055) * 0.085 * sign + soft(s - 0.07, 0.028) * 0.025 * sign
            }
            Some(BrushKind::Inflate) => soft(d, 0.22) * 0.15 * sign,
            Some(BrushKind::Smooth) => noise(p) * 0.025 * (1.0 - soft(s, 0.16) * 0.98),
            Some(BrushKind::Clay) => {
                soft(d, 0.24) * (0.74 + p.x * 0.12 - z).max(0.03) * 0.65 * sign
            }
            Some(BrushKind::ClayBuildup) => {
                let band = ((0.43 - p.x.abs()) * 14.0).clamp(0.0, 1.0)
                    * ((0.32 - (p.y + p.x * 0.25).abs()) * 15.0).clamp(0.0, 1.0);
                band * (0.07 + ((p.y + p.x * 0.25) * 95.0).sin() * 0.015 + noise(p) * 0.009) * sign
            }
            Some(BrushKind::Flatten) => {
                (z.min(0.69 + p.x * 0.13 - p.y * 0.1) - z)
                    * ((0.68 - p.length()) * 12.0).clamp(0.0, 1.0)
            }
            Some(BrushKind::TrimDynamic) => z.min(0.65 + p.x * 0.22).min(0.73 - p.y * 0.18) - z,
            Some(BrushKind::Polish) => {
                let plane = 0.70 + p.x * 0.08 - p.y * 0.09;
                let delta = (z - plane).max(0.0);
                -delta * 0.92
            }
            Some(BrushKind::Blob) => soft(p.length(), 0.38) * 0.17 * sign,
            Some(BrushKind::Layer) => ((0.13 - d) * 70.0).clamp(0.0, 1.0) * 0.08 * sign,
            Some(BrushKind::Fill) => -soft(s, 0.10) * 0.085 * (0.35 + p.x).clamp(0.0, 1.0),
            Some(BrushKind::Scrape) => z.min(0.62 - p.x * 0.20 + p.y * 0.10) - z + noise(p) * 0.002,
            Some(BrushKind::Nudge) => {
                let shifted = Vec2::new(p.x - soft(p.length(), 0.45) * 0.18 * sign, p.y);
                soft(distance(shifted, &curve), 0.065) * 0.08
            }
            Some(BrushKind::Sharpen) => {
                soft(s, 0.033) * 0.12 * sign - soft(s - 0.06, 0.025) * 0.022 * sign
            }
            Some(BrushKind::FormSoft) => soft(d, 0.34) * 0.075 * sign,
            Some(BrushKind::FormHard) => ((0.25 - d) * 25.0).clamp(0.0, 1.0) * 0.13 * sign,
            Some(k @ (BrushKind::Zipper | BrushKind::Stitch | BrushKind::Rake)) => {
                crate::detail_brush::profile(k, p.x / 0.68, p.y / 0.7)
                    * 0.075
                    * sign
                    * ((0.57 - p.y.abs()) * 25.0).clamp(0.0, 1.0)
            }
            Some(
                BrushKind::ClipLine
                | BrushKind::MaskPen
                | BrushKind::MaskSoft
                | BrushKind::MaskRect
                | BrushKind::MaskLasso
                | BrushKind::MaskCircle
                | BrushKind::MaskLine,
            ) => 0.0,
        };
        z + h * (z / radius).clamp(0.0, 1.0)
    };
    let light = Vec3::new(-0.45, -0.6, 0.75).normalize();
    let half = (light + Vec3::Z).normalize();
    let mut pixels = vec![0u8; n * n * 4];
    let step = 2.0 / n as f32;
    for y in 0..n {
        for x in 0..n {
            let p = Vec2::new((x as f32 + 0.5) * step - 1.0, (y as f32 + 0.5) * step - 1.0);
            let edge = (shape(p) / step + 0.5).clamp(0.0, 1.0);
            if edge == 0.0 {
                continue;
            }
            let z = height(p);
            let gx = (height(p + Vec2::X * step) - height(p - Vec2::X * step)) / (step * 2.0);
            let gy = (height(p + Vec2::Y * step) - height(p - Vec2::Y * step)) / (step * 2.0);
            let normal = Vec3::new(-gx, -gy, 1.0).normalize();
            let diff = normal.dot(light).max(0.0);
            let fill = normal.dot(-light).max(0.0) * 0.10;
            let spec = normal.dot(half).max(0.0).powf(52.0)
                * if material == 1 || material == 3 {
                    0.32
                } else {
                    0.17
                };
            let mut color = base;
            if kind == Some(BrushKind::Paint) {
                let f = (-(distance(p, &spiral) / 0.09).powi(2) * 2.0).exp();
                color = color.lerp(Vec3::new(0.90, 0.36, 0.23), f * 0.85);
            }
            if let Some(k) = kind.filter(|k| k.is_mask()) {
                let coverage = match k {
                    BrushKind::MaskPen => (-(distance(p, &spiral) / 0.1).powi(2)).exp(),
                    BrushKind::MaskSoft => {
                        (-(p.distance(Vec2::new(-0.1, -0.1)) / 0.42).powi(2)).exp() * 0.85
                    }
                    BrushKind::MaskRect => {
                        ((0.4 - p.x.abs().max(p.y.abs())) * 80.0).clamp(0.0, 1.0)
                    }
                    BrushKind::MaskCircle => {
                        ((0.42 - (p / Vec2::new(1.15, 0.8)).length()) * 80.0).clamp(0.0, 1.0)
                    }
                    BrushKind::MaskLasso => {
                        ((0.4 - (p.x.abs() * 0.7 + p.y.abs() + p.x * 0.2)) * 80.0).clamp(0.0, 1.0)
                    }
                    BrushKind::MaskLine => ((p.x + p.y * 0.6) * 80.0).clamp(0.0, 1.0),
                    _ => 0.0,
                };
                color *= 1.0 - coverage * 0.82;
            }
            let ao = (0.84 + z * 0.18).min(1.0);
            let lit = (color * (0.23 + diff * 0.77 + fill) * ao + Vec3::splat(spec))
                .clamp(Vec3::ZERO, Vec3::ONE);
            let i = (y * n + x) * 4;
            pixels[i] = (lit.x * 255.0) as u8;
            pixels[i + 1] = (lit.y * 255.0) as u8;
            pixels[i + 2] = (lit.z * 255.0) as u8;
            pixels[i + 3] = (edge * 255.0).round() as u8;
        }
    }
    ColorImage::from_rgba_unmultiplied([n, n], &pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_catalog_has_distinct_clay_studies_and_transparent_edges() {
        let mut images = Vec::new();
        for k in BrushKind::ALL {
            let i = image(Some(k), 0, false);
            assert_eq!(i[(0, 0)].a(), 0);
            assert_eq!(i[(80, 80)].a(), 255);
            assert!(
                images.iter().all(|p: &ColorImage| p.pixels != i.pixels),
                "duplicate preview: {k:?}"
            );
            if let Ok(root) = std::env::var("KLAY_EXPORT_PREVIEWS") {
                let p = std::path::Path::new(&root);
                std::fs::create_dir_all(p).unwrap();
                let bytes: Vec<u8> = i
                    .pixels
                    .iter()
                    .flat_map(|c| c.to_srgba_unmultiplied())
                    .collect();
                image::save_buffer(
                    p.join(format!("{k:?}.png")),
                    &bytes,
                    160,
                    160,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            images.push(i);
        }
    }
}
