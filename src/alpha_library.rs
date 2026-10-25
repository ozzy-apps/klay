//! Klay's own detail alphas and textures: tileable 512 × 512 height images painted from
//! periodic noise and cell patterns the first time each is used (no borrowed artwork).
//! White is high. As alphas they fade out softly toward the stamp's edge; as textures they
//! repeat seamlessly.

use glam::Vec2;
use std::sync::OnceLock;

pub const SIZE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Baked {
    Pores,
    Wrinkles,
    Scales,
    Cells,
    Weave,
    Rock,
    Bark,
    Leather,
    Fibers,
}

impl Baked {
    pub const ALL: [Baked; 9] = [
        Baked::Pores,
        Baked::Wrinkles,
        Baked::Scales,
        Baked::Cells,
        Baked::Weave,
        Baked::Rock,
        Baked::Bark,
        Baked::Leather,
        Baked::Fibers,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Baked::Pores => "Pores",
            Baked::Wrinkles => "Wrinkles",
            Baked::Scales => "Scales",
            Baked::Cells => "Dry Cracks",
            Baked::Weave => "Weave",
            Baked::Rock => "Rock",
            Baked::Bark => "Bark",
            Baked::Leather => "Leather",
            Baked::Fibers => "Fibers",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&b| b == self).unwrap()
    }

    fn height(self, uv: Vec2) -> f32 {
        match self {
            Baked::Pores => {
                // Soft round pits of varying size, not in every cell.
                let (f1, _, id) = worley(uv, 26);
                let r = 0.16 + 0.14 * hash1(id, 7);
                let present = hash1(id, 11) < 0.75;
                let pore = if present { (1.0 - (f1 / r).powi(2)).max(0.0).powi(2) } else { 0.0 };
                pore + fbm(uv, 32, 3) * 0.08
            }
            Baked::Wrinkles => {
                // Fine, slightly wandering lines along x that fade in and out.
                let y = uv.y * 14.0 + (fbm(uv, 4, 3) - 0.5) * 2.4;
                let line = (1.0 - (y * std::f32::consts::PI).sin().abs()).powi(8);
                line * (0.35 + 0.65 * fbm(uv + Vec2::splat(0.37), 3, 2))
            }
            Baked::Scales => {
                let (f1, f2, _) = worley(uv, 9);
                let dome = (1.0 - (f1 / 0.62).powi(2)).max(0.0).sqrt();
                dome * smooth(0.0, 0.08, f2 - f1)
            }
            Baked::Cells => {
                let (f1, f2, id) = worley(uv, 7);
                let plate = smooth(0.0, 0.06, f2 - f1);
                plate * (0.75 + 0.25 * hash1(id, 3)) * (1.0 - 0.15 * f1) + fbm(uv, 24, 3) * 0.05
            }
            Baked::Weave => {
                let n = 12.0;
                let p = uv * n;
                let (i, j) = (p.x.floor() as i32, p.y.floor() as i32);
                let f = p - p.floor();
                let thread = |t: f32| ((t * std::f32::consts::PI).sin().max(0.0)).powf(0.6);
                let (h, v) = (thread(f.y), thread(f.x));
                // Over and under: horizontal threads on top in alternate cells.
                let over = (i + j).rem_euclid(2) == 0;
                let (top, under) = if over { (h, v) } else { (v, h) };
                (top * 0.85 + 0.15).max(under * 0.6) * (0.9 + 0.1 * fbm(uv, 64, 2))
            }
            Baked::Rock => ridged(uv, 4, 5),
            Baked::Bark => {
                let stretched = Vec2::new(uv.x, uv.y * 0.25);
                let grooves = ridged_aniso(uv, [14, 3], 4);
                grooves * 0.8 + fbm(stretched + Vec2::splat(0.5), 8, 3) * 0.2
            }
            Baked::Leather => {
                let (f1, f2, _) = worley(uv, 34);
                smooth(0.0, 0.14, f2 - f1) * (0.7 + 0.3 * (1.0 - f1)) * 0.75 + fbm(uv, 64, 3) * 0.25
            }
            Baked::Fibers => {
                // Many thin strands running along y, each wandering a little.
                let x = uv.x * 46.0 + (fbm(Vec2::new(uv.x * 0.25, uv.y), 3, 3) - 0.5) * 6.0;
                let strand = (1.0 - (x * std::f32::consts::PI).sin().abs()).powi(4);
                strand * (0.45 + 0.55 * fbm(uv, 6, 2)) + fbm(uv, 16, 2) * 0.1
            }
        }
    }
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^ (h >> 15)
}

fn hash1(id: u32, seed: u32) -> f32 {
    (hash(id as i32, 0, seed) & 0xffff) as f32 / 65535.0
}

/// Value noise that repeats every `period` × `period` lattice cells over uv 0..1.
fn noise_aniso(uv: Vec2, period: [i32; 2]) -> f32 {
    let p = uv * Vec2::new(period[0] as f32, period[1] as f32);
    let i = p.floor();
    let f = p - i;
    let s = f * f * (Vec2::splat(3.0) - 2.0 * f);
    let (ix, iy) = (i.x as i32, i.y as i32);
    let at = |dx: i32, dy: i32| (hash((ix + dx).rem_euclid(period[0]), (iy + dy).rem_euclid(period[1]), 1) & 0xffff) as f32 / 65535.0;
    let a = at(0, 0) + (at(1, 0) - at(0, 0)) * s.x;
    let b = at(0, 1) + (at(1, 1) - at(0, 1)) * s.x;
    a + (b - a) * s.y
}

/// Fractal value noise, tileable over uv 0..1 (octaves double the period).
fn fbm(uv: Vec2, period: i32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut total) = (0.0, 1.0, 0.0);
    for o in 0..octaves {
        let p = period << o;
        sum += noise_aniso(uv, [p, p]) * amp;
        total += amp;
        amp *= 0.5;
    }
    sum / total
}

/// Ridged fractal noise: sharp crests where the noise crosses its middle.
fn ridged(uv: Vec2, period: i32, octaves: u32) -> f32 {
    ridged_aniso(uv, [period, period], octaves)
}

fn ridged_aniso(uv: Vec2, period: [i32; 2], octaves: u32) -> f32 {
    let (mut sum, mut amp, mut total) = (0.0, 1.0, 0.0);
    for o in 0..octaves {
        let n = noise_aniso(uv, [period[0] << o, period[1] << o]);
        sum += (1.0 - (n * 2.0 - 1.0).abs()).powi(2) * amp;
        total += amp;
        amp *= 0.55;
    }
    sum / total
}

/// Distances to the nearest and second nearest of `n` × `n` jittered points (in cell units)
/// that repeat over uv 0..1, and an id for the nearest one's cell.
fn worley(uv: Vec2, n: i32) -> (f32, f32, u32) {
    let p = uv * n as f32;
    let (cx, cy) = (p.x.floor() as i32, p.y.floor() as i32);
    let (mut f1, mut f2, mut id) = (f32::MAX, f32::MAX, 0);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (gx, gy) = (cx + dx, cy + dy);
            let (wx, wy) = (gx.rem_euclid(n), gy.rem_euclid(n));
            let h = hash(wx, wy, 5);
            let jitter = Vec2::new((h & 0xffff) as f32 / 65535.0, (h >> 16) as f32 / 65535.0);
            let point = Vec2::new(gx as f32, gy as f32) + Vec2::splat(0.1) + jitter * 0.8;
            let d = p.distance(point);
            if d < f1 {
                f2 = f1;
                f1 = d;
                id = (wy * n + wx) as u32;
            } else if d < f2 {
                f2 = d;
            }
        }
    }
    (f1, f2, id)
}

/// The image of `b`, 0..1 heights, row 0 at uv.y = 0, painted on first use.
pub fn image(b: Baked) -> &'static [f32] {
    static IMAGES: [OnceLock<Vec<f32>>; 9] = [const { OnceLock::new() }; 9];
    IMAGES[b.index()].get_or_init(|| {
        // Painted on plain threads, not rayon's: the first use is often inside a brush's
        // parallel loop, and a rayon worker waiting here could pick up another dab that
        // waits for this same image, and never finish (a deadlock).
        let mut raw = vec![0.0f32; SIZE * SIZE];
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
        let rows = SIZE.div_ceil(threads);
        std::thread::scope(|scope| {
            for (k, part) in raw.chunks_mut(rows * SIZE).enumerate() {
                scope.spawn(move || {
                    for (j, out) in part.iter_mut().enumerate() {
                        let i = k * rows * SIZE + j;
                        *out = b.height(Vec2::new(((i % SIZE) as f32 + 0.5) / SIZE as f32, ((i / SIZE) as f32 + 0.5) / SIZE as f32));
                    }
                });
            }
        });
        // Stretch to the full 0..1 range.
        let (lo, hi) = raw.iter().fold((f32::MAX, f32::MIN), |(a, z), &v| (a.min(v), z.max(v)));
        let span = (hi - lo).max(1e-6);
        raw.into_iter().map(|v| (v - lo) / span).collect()
    })
}

/// Bilinear height of `b` at `uv`, repeating.
pub fn sample(b: Baked, uv: Vec2) -> f32 {
    let img = image(b);
    let p = (uv - uv.floor()) * SIZE as f32 - Vec2::splat(0.5);
    let (x0, y0) = (p.x.floor(), p.y.floor());
    let (fx, fy) = (p.x - x0, p.y - y0);
    let at = |x: f32, y: f32| img[(y as i32).rem_euclid(SIZE as i32) as usize * SIZE + (x as i32).rem_euclid(SIZE as i32) as usize];
    let top = at(x0, y0) + (at(x0 + 1.0, y0) - at(x0, y0)) * fx;
    let bottom = at(x0, y0 + 1.0) + (at(x0 + 1.0, y0 + 1.0) - at(x0, y0 + 1.0)) * fx;
    top + (bottom - top) * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_fills_the_range_tiles_and_differs() {
        let mut seen: Vec<Vec<f32>> = Vec::new();
        for b in Baked::ALL {
            let img = image(b);
            let (lo, hi) = img.iter().fold((f32::MAX, f32::MIN), |(a, z), &v| (a.min(v), z.max(v)));
            assert!(lo == 0.0 && (hi - 1.0).abs() < 1e-6, "{b:?}");
            // Seamless: across the wrap, neighbours differ no more than neighbours inside do.
            let step_x = |x0: usize, x1: usize| (0..SIZE).map(|y| (img[y * SIZE + x0] - img[y * SIZE + x1]).abs()).sum::<f32>() / SIZE as f32;
            let step_y = |y0: usize, y1: usize| (0..SIZE).map(|x| (img[y0 * SIZE + x] - img[y1 * SIZE + x]).abs()).sum::<f32>() / SIZE as f32;
            let inside = (1..SIZE / 2).map(|k| step_x(k, k + 1).max(step_y(k, k + 1))).fold(0.0, f32::max);
            let (seam, seam_y) = (step_x(SIZE - 1, 0), step_y(SIZE - 1, 0));
            assert!(seam <= inside * 1.1 && seam_y <= inside * 1.1, "{b:?} seams {seam} {seam_y} vs {inside}");
            // Neither flat nor noise only: a fair share of mid and high values.
            let mean = img.iter().sum::<f32>() / img.len() as f32;
            assert!(mean > 0.05 && mean < 0.9, "{b:?} mean {mean}");
            let sample: Vec<f32> = img.iter().step_by(997).copied().collect();
            assert!(seen.iter().all(|s| s != &sample), "{b:?} duplicates another");
            seen.push(sample);
        }
        assert!((sample(Baked::Rock, Vec2::new(0.25, 0.5)) - sample(Baked::Rock, Vec2::new(1.25, -0.5))).abs() < 1e-6);
    }

    #[test]
    #[ignore]
    fn export_sheet() {
        // KLAY_EXPORT_ALPHAS=dir cargo test --release export_sheet -- --ignored
        let Ok(dir) = std::env::var("KLAY_EXPORT_ALPHAS") else { return };
        let n = 256;
        let mut out = vec![0u8; n * 9 * n];
        for (k, b) in Baked::ALL.iter().enumerate() {
            for y in 0..n {
                for x in 0..n {
                    let uv = Vec2::new((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
                    let a = crate::brush_asset::Pattern::Library(*b).alpha(uv);
                    out[y * n * 9 + k * n + x] = (a * 255.0) as u8;
                }
            }
        }
        image::save_buffer(std::path::Path::new(&dir).join("alphas.png"), &out, (n * 9) as u32, n as u32, image::ColorType::L8).unwrap();
    }
}
