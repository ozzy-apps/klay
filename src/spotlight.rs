//! Spotlight (ZBrush: Texture › Spotlight): a picture laid over the canvas, to work from as a
//! reference and to paint and sculpt through.
//!
//! The picture is placed on the canvas by its centre (a share of the canvas's width and
//! height), its width (a share of the canvas's width), a turn and an opacity. With **Paint
//! through** on, a stroke reads it where each vertex lands on screen ([`Projector`]): polypaint
//! takes the picture's colour there, and sculpting brushes its brightness as their strength,
//! so a photo or a pattern goes onto the model as seen. Off the picture, brushes do nothing.

use glam::{Mat4, Vec2, Vec3};
use std::sync::Arc;

/// A loaded picture, straight-alpha sRGB.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[u8; 4]>,
    /// Changes with every picture loaded, so the interface knows to upload it again.
    pub key: u64,
    /// The picture's file as read (PNG, JPEG or BMP), kept so a project can store it;
    /// empty for a picture made in memory ([`Picture::encoded`] makes a PNG then).
    pub encoded: Arc<Vec<u8>>,
}

impl Picture {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::decode(bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A picture from its file's bytes (as a project stores them).
    pub fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.to_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self { width, height, rgba: bytemuck::cast_vec(img.into_raw()), key: Self::next_key(), encoded: Arc::new(bytes) })
    }

    fn next_key() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The picture as a file's bytes: as loaded, or a PNG made from the pixels.
    pub fn encoded(&self) -> Arc<Vec<u8>> {
        if !self.encoded.is_empty() {
            return self.encoded.clone();
        }
        let mut out = std::io::Cursor::new(Vec::new());
        let img = image::RgbaImage::from_raw(self.width, self.height, bytemuck::cast_slice(&self.rgba).to_vec()).expect("pixels match the size");
        let _ = img.write_to(&mut out, image::ImageFormat::Png);
        Arc::new(out.into_inner())
    }

    /// Bilinear colour at `uv` (0..1, V down), each channel 0..1.
    fn sample(&self, uv: Vec2) -> [f32; 4] {
        let (w, h) = (self.width as f32, self.height as f32);
        let p = Vec2::new(uv.x * w - 0.5, uv.y * h - 0.5);
        let (x0, y0) = (p.x.floor(), p.y.floor());
        let (fx, fy) = (p.x - x0, p.y - y0);
        let at = |x: f32, y: f32| {
            let x = (x as i64).clamp(0, self.width as i64 - 1) as usize;
            let y = (y as i64).clamp(0, self.height as i64 - 1) as usize;
            self.rgba[y * self.width as usize + x].map(|c| c as f32 / 255.0)
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
    }
}

/// Where the picture sits on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Centre, as shares of the canvas's width and height.
    pub center: Vec2,
    /// Width as a share of the canvas's width.
    pub scale: f32,
    /// Turn, radians.
    pub angle: f32,
    pub opacity: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Self { center: Vec2::splat(0.5), scale: 0.5, angle: 0.0, opacity: 0.5 }
    }
}

impl Placement {
    /// The picture's corners on a canvas of `size` at `origin` (top left, top right,
    /// bottom right, bottom left), for a picture of `aspect` (height / width).
    pub fn corners(&self, origin: Vec2, size: Vec2, aspect: f32) -> [Vec2; 4] {
        let half = Vec2::new(1.0, aspect) * (self.scale * size.x * 0.5);
        let c = origin + self.center * size;
        let (s, k) = self.angle.sin_cos();
        let turn = |v: Vec2| Vec2::new(v.x * k - v.y * s, v.x * s + v.y * k);
        [Vec2::new(-half.x, -half.y), Vec2::new(half.x, -half.y), half, Vec2::new(-half.x, half.y)].map(|v| c + turn(v))
    }

    /// The picture's UV (0..1, V down) at canvas point `p` (canvas of `size` from the
    /// origin), or `None` off the picture.
    pub fn uv(&self, p: Vec2, size: Vec2, aspect: f32) -> Option<Vec2> {
        let d = p - self.center * size;
        let (s, k) = (-self.angle).sin_cos();
        let d = Vec2::new(d.x * k - d.y * s, d.x * s + d.y * k);
        let uv = d / (Vec2::new(1.0, aspect) * self.scale * size.x) + Vec2::splat(0.5);
        (0.0..=1.0).contains(&uv.x).then_some(())?;
        (0.0..=1.0).contains(&uv.y).then_some(uv)
    }
}

/// The Spotlight as the interface holds it.
pub struct Spotlight {
    pub picture: Arc<Picture>,
    pub name: String,
    pub place: Placement,
    /// Shown on the canvas (Shift + Z).
    pub shown: bool,
    /// Z: a drag on the canvas moves the picture instead of sculpting.
    pub edit: bool,
    /// Strokes read the picture.
    pub paint_through: bool,
}

/// The picture as a stroke reads it: where model points land on it.
pub struct Projector {
    picture: Arc<Picture>,
    view_proj: Mat4,
    canvas: Vec2,
    place: Placement,
}

impl std::fmt::Debug for Projector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Projector({}×{})", self.picture.width, self.picture.height)
    }
}

impl Projector {
    /// For `spot` seen through `view_proj` on a canvas `canvas` pixels in size.
    pub fn new(spot: &Spotlight, view_proj: Mat4, canvas: Vec2) -> Self {
        Self { picture: spot.picture.clone(), view_proj, canvas, place: spot.place }
    }

    /// The picture's colour (0..1 sRGB with alpha) where world point `p` lands, or `None`
    /// off the picture or behind the eye.
    pub fn sample(&self, p: Vec3) -> Option<[f32; 4]> {
        let c = self.view_proj * p.extend(1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let n = c.truncate() / c.w;
        let px = Vec2::new((n.x + 1.0) * 0.5 * self.canvas.x, (1.0 - n.y) * 0.5 * self.canvas.y);
        let aspect = self.picture.height as f32 / self.picture.width.max(1) as f32;
        self.place.uv(px, self.canvas, aspect).map(|uv| self.picture.sample(uv))
    }

    /// Brightness times coverage where `p` lands (0 off the picture), for sculpting.
    pub fn strength(&self, p: Vec3) -> f32 {
        self.sample(p).map_or(0.0, |c| (c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722) * c[3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Arc<Picture> {
        // Left half red, right half blue, 4 × 2.
        let rgba = (0..8).map(|i| if i % 4 < 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
        Arc::new(Picture { width: 4, height: 2, rgba, key: 1, encoded: Default::default() })
    }

    #[test]
    fn corners_and_uv_agree_and_turning_turns_both() {
        for angle in [0.0, 0.7] {
            let place = Placement { center: Vec2::new(0.4, 0.6), scale: 0.3, angle, opacity: 1.0 };
            let size = Vec2::new(800.0, 500.0);
            let c = place.corners(Vec2::ZERO, size, 0.5);
            let mid = (c[0] + c[2]) * 0.5;
            assert!(place.uv(mid, size, 0.5).unwrap().distance(Vec2::splat(0.5)) < 1e-4);
            let near_tl = c[0] + (mid - c[0]) * 0.02;
            assert!(place.uv(near_tl, size, 0.5).unwrap().length() < 0.05);
            assert!(place.uv(c[0] - (mid - c[0]) * 0.1, size, 0.5).is_none(), "outside");
        }
    }

    #[test]
    fn a_projected_point_reads_the_colour_it_lands_on() {
        let cam = crate::camera::Camera { aspect: 2.0, ..crate::camera::Camera::new() };
        let spot = Spotlight { picture: picture(), name: "t".into(), place: Placement { scale: 0.5, ..Default::default() }, shown: true, edit: false, paint_through: true };
        let pr = Projector::new(&spot, cam.view_proj(), Vec2::new(800.0, 400.0));
        // The picture covers the middle half of the canvas: its left half red, its right blue.
        let (o, d) = cam.ray(250.0, 200.0, 800.0, 400.0);
        let left = pr.sample(o + d * 3.0).unwrap();
        assert!(left[0] > 0.9 && left[2] < 0.1, "{left:?}");
        let (o, d) = cam.ray(550.0, 200.0, 800.0, 400.0);
        assert!(pr.sample(o + d * 3.0).unwrap()[2] > 0.9);
        assert!(pr.strength(o + d * 3.0) > 0.05);
        let (o, d) = cam.ray(400.0, 10.0, 800.0, 400.0);
        assert!(pr.sample(o + d * 3.0).is_none(), "above the picture");
    }
}
