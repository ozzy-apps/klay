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

    fn sample(&self, uv: Vec2) -> Vec4 {
        let p = uv.clamp(Vec2::ZERO, Vec2::ONE)
            * Vec2::new((self.width - 1) as f32, (self.height - 1) as f32);
        let (x, y) = (p.x.floor() as usize, p.y.floor() as usize);
        let at = |x: usize, y: usize| {
            Vec4::from_array(self.rgba[y * self.width + x].map(|v| v as f32 / 255.0))
        };
        at(x, y)
            .lerp(at((x + 1).min(self.width - 1), y), p.x.fract())
            .lerp(
                at(x, (y + 1).min(self.height - 1)).lerp(
                    at((x + 1).min(self.width - 1), (y + 1).min(self.height - 1)),
                    p.x.fract(),
                ),
                p.y.fract(),
            )
    }
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
            Self::Image(_) => unreachable!(),
        };
        let color = if matches!(self, Self::Marble) {
            Vec3::new(0.25, 0.30, 0.34).lerp(Vec3::new(0.92, 0.88, 0.80), h)
        } else {
            Vec3::splat(h)
        };
        color.extend(1.0)
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
