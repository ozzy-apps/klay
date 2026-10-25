//! CPU brush stamps, shared by sculpting, painting and asset-picker previews.
use glam::{Vec2, Vec3, Vec4};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Debug)]
pub struct BrushImage {
    pub id: u64,
    pub name: String,
    pub source: std::path::PathBuf,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<[u8; 4]>,
}

impl PartialEq for BrushImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl BrushImage {
    pub fn load(path: &Path) -> Result<Arc<Self>, String> {
        if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 64 << 20 {
            return Err("Image exceeds 64 MB".into());
        }
        let mut reader = image::ImageReader::open(path)
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(256 << 20);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .map_err(|e| format!("Cannot decode image: {e}"))?;
        let image = if decoded.width().max(decoded.height()) > 1024 {
            decoded.resize(1024, 1024, image::imageops::FilterType::Triangle)
        } else {
            decoded
        }
        .into_rgba8();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            source: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            width: image.width() as usize,
            height: image.height() as usize,
            rgba: image.pixels().map(|p| p.0).collect(),
        }))
    }

    /// Colour at `uv` (0..1, clamped at the edges): a cubic B-spline over the 4 × 4 nearest
    /// pixels, so 8-bit steps and pixel edges do not show as ridges at a large Draw Size.
    fn sample(&self, uv: Vec2) -> Vec4 {
        let p = uv.clamp(Vec2::ZERO, Vec2::ONE)
            * Vec2::new((self.width - 1) as f32, (self.height - 1) as f32);
        let i = p.floor();
        let (wx, wy) = (
            crate::alpha_library::bspline(p.x - i.x),
            crate::alpha_library::bspline(p.y - i.y),
        );
        let (ix, iy) = (i.x as i64 - 1, i.y as i64 - 1);
        let mut sum = Vec4::ZERO;
        for (dy, wy) in wy.iter().enumerate() {
            let y = (iy + dy as i64).clamp(0, self.height as i64 - 1) as usize;
            for (dx, wx) in wx.iter().enumerate() {
                let x = (ix + dx as i64).clamp(0, self.width as i64 - 1) as usize;
                let c = Vec4::from_array(self.rgba[y * self.width + x].map(|v| v as f32));
                sum += c * (wx * wy);
            }
        }
        sum / 255.0
    }
}

/// How a library pattern used as an alpha fades out toward the stamp's edge.
fn alpha_fade(uv: Vec2) -> f32 {
    let r = (uv * 2.0 - Vec2::ONE).length();
    let t = ((1.0 - r) / 0.3).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Pattern {
    #[default]
    Off,
    Soft,
    Hard,
    Square,
    Dots,
    Stripes,
    Grain,
    Cracks,
    Marble,
    /// One of Klay's painted detail images (`alpha_library`).
    Library(crate::alpha_library::Baked),
    Image(Arc<BrushImage>),
}

impl Pattern {
    pub fn locator(&self) -> String {
        match self {
            Self::Image(i) => format!("file:{}", i.source.to_string_lossy()),
            _ => format!("builtin:{}", self.key()),
        }
    }
    pub fn alphas() -> Vec<Self> {
        vec![
            Self::Off,
            Self::Soft,
            Self::Hard,
            Self::Square,
            Self::Dots,
            Self::Stripes,
            Self::Grain,
            Self::Cracks,
        ]
        .into_iter()
        .chain(crate::alpha_library::Baked::ALL.map(Self::Library))
        .collect()
    }
    pub fn textures() -> Vec<Self> {
        vec![
            Self::Off,
            Self::Grain,
            Self::Cracks,
            Self::Marble,
            Self::Stripes,
            Self::Dots,
        ]
        .into_iter()
        .chain(crate::alpha_library::Baked::ALL.map(Self::Library))
        .collect()
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Off => "Off",
            Self::Soft => "Soft",
            Self::Hard => "Round",
            Self::Square => "Square",
            Self::Dots => "Dots",
            Self::Stripes => "Stripes",
            Self::Grain => "Grain",
            Self::Cracks => "Cracks",
            Self::Marble => "Marble",
            Self::Library(b) => b.name(),
            Self::Image(i) => &i.name,
        }
    }
    pub fn key(&self) -> u64 {
        match self {
            Self::Image(i) => 100 + i.id,
            Self::Off => 0,
            Self::Soft => 1,
            Self::Hard => 2,
            Self::Square => 3,
            Self::Dots => 4,
            Self::Stripes => 5,
            Self::Grain => 6,
            Self::Cracks => 7,
            Self::Marble => 8,
            Self::Library(b) => 20 + b.index() as u64,
        }
    }
    /// UV in [0,1] for alpha stamps; textures repeat using their tiling setting.
    pub fn sample(&self, uv: Vec2, repeat: bool) -> Vec4 {
        let uv = if repeat { uv - uv.floor() } else { uv };
        if !repeat && (uv.min_element() < 0.0 || uv.max_element() > 1.0) {
            return Vec4::ZERO;
        }
        if let Self::Image(i) = self {
            return i.sample(uv);
        }
        if let Self::Library(b) = self {
            // As an alpha it fades out toward the stamp's edge; as a texture it repeats.
            let fade = if repeat { 1.0 } else { alpha_fade(uv) };
            return Vec3::splat(crate::alpha_library::sample(*b, uv) * fade).extend(1.0);
        }
        let p = uv * 2.0 - Vec2::ONE;
        let r = p.length();
        let noise = |p: Vec2| {
            let q = p.floor();
            let f = p - q;
            let f = f * f * (Vec2::splat(3.0) - f * 2.0);
            let hash = |v: Vec2| {
                ((v.dot(Vec2::new(127.1, 311.7))).sin() * 43758.5453)
                    .fract()
                    .abs()
            };
            let a = hash(q);
            let b = hash(q + Vec2::X);
            let c = hash(q + Vec2::Y);
            let d = hash(q + Vec2::ONE);
            (a + (b - a) * f.x) * (1.0 - f.y) + (c + (d - c) * f.x) * f.y
        };
        let h = match self {
            Self::Off => 1.0,
            Self::Soft => (1.0 - r * r).max(0.0).powi(2),
            Self::Hard => ((1.0 - r) * 32.0).clamp(0.0, 1.0),
            Self::Square => ((1.0 - p.abs().max_element()) * 24.0).clamp(0.0, 1.0),
            Self::Dots => {
                let q = (uv * 5.0).fract() - Vec2::splat(0.5);
                ((0.3 - q.length()) * 20.0).clamp(0.0, 1.0)
            }
            Self::Stripes => (0.5 + 0.5 * (uv.x * std::f32::consts::TAU * 6.0).cos()).powi(4),
            Self::Grain => (noise(uv * 38.0) * 0.65 + noise(uv * 91.0) * 0.35).clamp(0.0, 1.0),
            Self::Cracks => {
                let n = noise(uv * 7.0) + noise(uv * 19.0) * 0.3;
                ((n - 0.55).abs() * 9.0).clamp(0.0, 1.0)
            }
            Self::Marble => 0.5 + 0.5 * ((uv.x + uv.y) * 18.0 + noise(uv * 5.0) * 9.0).sin(),
            Self::Library(_) | Self::Image(_) => unreachable!(),
        };
        let color = if matches!(self, Self::Marble) {
            Vec3::new(0.25, 0.30, 0.34).lerp(Vec3::new(0.92, 0.88, 0.80), h)
        } else {
            Vec3::splat(h)
        };
        color.extend(1.0)
    }
    /// An `n` × `n` picture of the stamp (as `sample(uv, false)`) for the asset grid.
    /// Library patterns are painted at this size straight from the pattern, so browsing
    /// does not paint each full-size image.
    pub fn thumbnail(&self, n: usize) -> Vec<Vec4> {
        let uv = |i: usize| Vec2::new((i % n) as f32 + 0.5, (i / n) as f32 + 0.5) / n as f32;
        if let Self::Library(b) = self {
            let h = crate::alpha_library::preview(*b, n);
            return (0..n * n).map(|i| Vec3::splat(h[i] * alpha_fade(uv(i))).extend(1.0)).collect();
        }
        (0..n * n).map(|i| self.sample(uv(i), false)).collect()
    }
    pub fn alpha(&self, uv: Vec2) -> f32 {
        let c = self.sample(uv, false);
        c.truncate().dot(Vec3::new(0.2126, 0.7152, 0.0722)) * c.w
    }
}

/// Stable right-handed tangent frame. Mirrored strokes sample the original side's image.
pub fn tangent(normal: Vec3) -> (Vec3, Vec3) {
    let n = normal.normalize_or(Vec3::Z);
    let up = if n.y.abs() < 0.95 { Vec3::Y } else { Vec3::Z };
    let u = up.cross(n).normalize_or(Vec3::X);
    (u, n.cross(u))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stamps_have_stable_edges_and_repeat_without_nan() {
        for p in Pattern::alphas().into_iter().chain(Pattern::textures()) {
            for uv in [
                Vec2::ZERO,
                Vec2::splat(0.5),
                Vec2::ONE,
                Vec2::new(-3.0, 4.0),
            ] {
                let c = p.sample(uv, true);
                assert!(c.is_finite());
                assert!(c.min_element() >= 0.0 && c.max_element() <= 1.0);
            }
            assert_eq!(p.alpha(Vec2::new(-1.0, 2.0)), 0.0);
        }
        assert_eq!(Pattern::Soft.alpha(Vec2::splat(0.5)), 1.0);
        assert_eq!(Pattern::Soft.alpha(Vec2::ZERO), 0.0);
    }
    #[test]
    fn library_patterns_fill_both_grids_with_stable_keys() {
        let (alphas, textures) = (Pattern::alphas(), Pattern::textures());
        for name in ["Skin", "Creases", "Cracked Earth", "Panels", "Wood Grain", "Cobbles", "Hammered", "Scales", "Pores"] {
            assert!(alphas.iter().any(|p| p.name() == name), "{name} missing from the alpha grid");
            assert!(textures.iter().any(|p| p.name() == name), "{name} missing from the texture grid");
        }
        // Saved brush assets refer to built-ins by key: the older ones keep theirs.
        use crate::alpha_library::Baked;
        assert_eq!(Pattern::Library(Baked::Pores).locator(), "builtin:20");
        assert_eq!(Pattern::Library(Baked::Scales).locator(), "builtin:22");
        assert_eq!(Pattern::Library(Baked::Fibers).locator(), "builtin:28");
        let mut keys: Vec<u64> = alphas.iter().map(Pattern::key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), alphas.len());
        // Every thumbnail is a full picture with something on it.
        for p in alphas.iter().chain(&textures).filter(|p| **p != Pattern::Off) {
            let t = p.thumbnail(24);
            assert_eq!(t.len(), 24 * 24);
            assert!(t.iter().all(|c| c.is_finite() && c.min_element() >= 0.0 && c.max_element() <= 1.0), "{}", p.name());
            assert!(t.iter().any(|c| c.x > 0.2), "{} thumbnail is blank", p.name());
        }
    }
    #[test]
    fn imported_images_sample_smoothly() {
        // A hard black / white edge in an 8-bit image: neighbouring samples across it
        // change gradually, with no step where pixels meet.
        let i = BrushImage {
            id: 0,
            name: "edge".into(),
            source: Default::default(),
            width: 8,
            height: 1,
            rgba: (0..8).map(|x| if x < 4 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect(),
        };
        let h: Vec<f32> = (0..=400).map(|k| i.sample(Vec2::new(k as f32 / 400.0, 0.5)).x).collect();
        let d: Vec<f32> = h.windows(2).map(|w| w[1] - w[0]).collect();
        let jump = d.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let kink = d.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(d.iter().all(|&x| x >= -1e-6), "not monotonic");
        assert!(kink < jump * 0.1, "slope breaks {kink} vs {jump}");
    }
    #[test]
    fn imports_rgba_and_rejects_corrupt_images() {
        let p = std::env::temp_dir().join(format!("klay-stamp-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 128, 0, 0]))
            .save(&p)
            .unwrap();
        let i = BrushImage::load(&p).unwrap();
        assert_eq!(i.sample(Vec2::splat(0.5)).w, 0.0);
        std::fs::write(&p, b"not an image").unwrap();
        assert!(BrushImage::load(&p).is_err());
        let _ = std::fs::remove_file(p);
    }
}
