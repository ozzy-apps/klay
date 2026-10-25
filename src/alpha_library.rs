//! Klay's own detail alphas and textures: tileable 2048 × 2048 16-bit height images painted
//! from periodic noise and cell patterns the first time each is used (no borrowed artwork).
//! White is high. As alphas they fade out softly toward the stamp's edge; as textures they
//! repeat seamlessly.
//!
//! Every pattern is a smooth function: edges are ramps several texels wide, never cliffs,
//! and the image is read back with a cubic B-spline, so a large Draw Size over a dense mesh
//! shows smooth slopes instead of texel steps. Asset thumbnails are painted straight from
//! the functions (`preview`), so browsing the grid does not paint every full image.

use glam::Vec2;
use std::sync::OnceLock;

pub const SIZE: usize = 2048;
const MASK: i32 = SIZE as i32 - 1;
const COUNT: usize = 16;

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
    Skin,
    Creases,
    CrackedEarth,
    Panels,
    WoodGrain,
    Cobbles,
    Hammered,
}

impl Baked {
    /// In grid order. New patterns go at the end: saved choices refer to them by position.
    pub const ALL: [Baked; COUNT] = [
        Baked::Pores,
        Baked::Wrinkles,
        Baked::Scales,
        Baked::Cells,
        Baked::Weave,
        Baked::Rock,
        Baked::Bark,
        Baked::Leather,
        Baked::Fibers,
        Baked::Skin,
        Baked::Creases,
        Baked::CrackedEarth,
        Baked::Panels,
        Baked::WoodGrain,
        Baked::Cobbles,
        Baked::Hammered,
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
            Baked::Skin => "Skin",
            Baked::Creases => "Creases",
            Baked::CrackedEarth => "Cracked Earth",
            Baked::Panels => "Panels",
            Baked::WoodGrain => "Wood Grain",
            Baked::Cobbles => "Cobbles",
            Baked::Hammered => "Hammered",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&b| b == self).unwrap()
    }

    fn height(self, uv: Vec2) -> f32 {
        use std::f32::consts::PI;
        match self {
            Baked::Pores => {
                // Soft round bumps of varying size, not in every cell.
                let mut h = 0.0;
                points(uv, 26, 5, |d, id| {
                    let r = 0.16 + 0.14 * hash1(id, 7);
                    if hash1(id, 11) < 0.75 {
                        h += bump(d.length() / r);
                    }
                });
                h + fbm(uv, 32, 3) * 0.08
            }
            Baked::Wrinkles => {
                // Fine, slightly wandering lines along x that fade in and out.
                let y = uv.y * 14.0 + (fbm(uv, 4, 3) - 0.5) * 2.4;
                let line = (1.0 - soft_abs((y * PI).sin(), 0.08)).powi(8);
                line * (0.35 + 0.65 * fbm(uv + Vec2::splat(0.37), 3, 2))
            }
            Baked::Scales => {
                // Rounded domes with a soft shoulder (no cliff at the rim), in shallow trenches.
                let (f1, f2, _) = worley(uv, 9, 5);
                let t = f1 / 0.62;
                let dome = (1.0 - t * t).max(0.0).sqrt() * smooth(1.0, 0.86, t);
                dome * smooth(0.0, 0.1, f2 - f1)
            }
            Baked::Cells => {
                let (f1, f2, id) = worley(uv, 7, 5);
                let plate = smooth(0.0, 0.06, f2 - f1);
                plate * (0.75 + 0.25 * hash1(id, 3)) * (1.0 - 0.15 * f1) + fbm(uv, 24, 3) * 0.05
            }
            Baked::Weave => {
                // Plain weave: each thread rises over one crossing thread and dips under the
                // next, smoothly along its length.
                let p = uv * 12.0;
                let (i, j) = (p.x.floor(), p.y.floor());
                let f = p - p.floor();
                let thread = |t: f32| {
                    let s = (t * PI).sin();
                    s * (2.0 - s)
                };
                let along_x = 0.5 + 0.5 * (PI * (p.x - 0.5) + PI * j).cos();
                let along_y = 0.5 - 0.5 * (PI * (p.y - 0.5) + PI * i).cos();
                let h = thread(f.y) * (0.5 + 0.5 * along_x);
                let v = thread(f.x) * (0.5 + 0.5 * along_y);
                smooth_max(h, v, 0.12) * (0.9 + 0.1 * fbm(uv, 64, 2))
            }
            Baked::Rock => ridged(uv, 4, 5),
            Baked::Bark => {
                let grooves = ridged_aniso(uv, [14, 3], 4);
                grooves * 0.8 + fbm_aniso(uv + Vec2::splat(0.5), [8, 2], 3) * 0.2
            }
            Baked::Leather => {
                let (f1, f2, _) = worley(uv, 34, 5);
                smooth(0.0, 0.3, f2 - f1) * (0.7 + 0.3 * (1.0 - f1)) * 0.75 + fbm(uv, 64, 3) * 0.25
            }
            Baked::Fibers => {
                // Many thin strands running along y, each wandering a little.
                let x = uv.x * 46.0 + (fbm_aniso(uv, [1, 3], 3) - 0.5) * 6.0;
                let strand = (1.0 - soft_abs((x * PI).sin(), 0.12)).powi(4);
                strand * (0.45 + 0.55 * fbm(uv, 6, 2)) + fbm(uv, 16, 2) * 0.1
            }
            Baked::Skin => {
                // Dense small pits crossed by two sets of faint creases, as on the back of a hand.
                let mut pits = 0.0;
                points(uv, 44, 21, |d, id| {
                    if hash1(id, 4) < 0.85 {
                        pits += bump(d.length() / (0.16 + 0.12 * hash1(id, 9)));
                    }
                });
                // Integer directions, so the lines repeat across the tile.
                let crease = |a: f32, b: f32, seed: f32| {
                    let warp = Vec2::new(seed, seed * 0.7);
                    let t = a * uv.x + b * uv.y + (fbm(uv + warp, 6, 2) - 0.5) * 0.8;
                    let line = (1.0 - soft_abs((t * PI).sin(), 0.05)).powi(10);
                    line * smooth(0.35, 0.7, fbm(uv + warp * 2.0, 4, 2))
                };
                let creases = smooth_max(crease(9.0, 5.0, 0.13), crease(9.0, -5.0, 0.61), 0.1);
                0.75 - 0.35 * pits.min(1.0) - 0.3 * creases + fbm(uv, 12, 4) * 0.3
            }
            Baked::Creases => {
                // Deep, softly rolled folds that fade in and out, like a forehead's.
                let t = uv.y * 7.0 + (fbm(uv, 3, 3) - 0.5) * 3.0;
                let fold = (1.0 - soft_abs((t * PI).sin(), 0.05)).powi(6);
                let depth = smooth(0.25, 0.65, fbm(uv + Vec2::splat(0.23), 4, 2));
                let roll = soft_abs((t * PI).sin(), 0.05).sqrt();
                0.55 + 0.35 * roll * (0.5 + 0.5 * depth) - fold * depth * 0.55 + fbm(uv, 32, 2) * 0.04
            }
            Baked::CrackedEarth => {
                // Large warped plates with deep cracks, split again by finer shallow ones.
                let warp = |s: f32| Vec2::new(fbm(uv + Vec2::splat(s), 6, 3), fbm(uv + Vec2::splat(s + 0.31), 6, 3)) - Vec2::splat(0.5);
                let (a1, a2, id) = worley(uv + warp(0.11) * 0.06, 5, 13);
                let (b1, b2, _) = worley(uv + warp(0.47) * 0.03, 13, 17);
                let big = smooth(0.0, 0.1, a2 - a1);
                let small = smooth(0.0, 0.07, b2 - b1);
                big * (0.55 + 0.45 * small) * (0.82 + 0.18 * hash1(id, 5)) * (1.0 - 0.1 * a1) + fbm(uv, 40, 3) * 0.06
            }
            Baked::Panels => {
                // Hard-surface plating: four rows of panels with staggered seams, rivets along
                // the row seams.
                let rows = 4.0;
                let y = uv.y * rows;
                let row = y.floor() as i32;
                let dy = (y - y.round()).abs() / rows;
                let (mut dx, mut panel) = (f32::MAX, 0);
                for k in 0..3 {
                    let s = (k as f32 + 0.2 + 0.6 * hash1((row.rem_euclid(4) * 3 + k) as u32, 29)) / 3.0;
                    let d = (uv.x - s).rem_euclid(1.0);
                    dx = dx.min(d.min(1.0 - d));
                    panel += ((uv.x - uv.x.floor()) >= s) as i32;
                }
                let seam = |d: f32| smooth(0.003, 0.011, d);
                // Each panel sits at its own height; the step hides in the seam.
                let level = 0.84 + 0.08 * hash1((row.rem_euclid(4) * 3 + panel % 3) as u32, 31);
                let plate = seam(dy) * seam(dx) * level;
                // Rivets: domes with a soft foot, a little inside each row seam.
                let rx = (uv.x * 36.0).fract() - 0.5;
                let ry = dy * 36.0 - 1.1;
                let rivet = bump(Vec2::new(rx, ry).length() / 0.28).powf(0.75) * seam(dx);
                plate + rivet * 0.18
            }
            Baked::WoodGrain => {
                // Long grain: late-wood bands that wander and pinch, over fine streaks.
                let w = fbm_aniso(uv, [5, 1], 3);
                let x = uv.x * 22.0 + (w - 0.5) * 6.0 + 0.4 * (uv.y * 2.0 * PI).sin();
                let ring = 0.5 + 0.5 * (x * 2.0 * PI).sin();
                let band = smooth(0.6, 0.97, ring);
                let streaks = fbm_aniso(uv, [96, 6], 3);
                0.25 + 0.45 * band + 0.3 * streaks
            }
            Baked::Cobbles => {
                // Rounded stones packed in mortar.
                let warp = Vec2::new(fbm(uv, 8, 2), fbm(uv + Vec2::splat(0.4), 8, 2)) - Vec2::splat(0.5);
                let (f1, f2, id) = worley(uv + warp * 0.05, 6, 23);
                let stone = smooth(0.03, 0.55, f2 - f1).powf(0.7);
                stone * (0.8 + 0.2 * hash1(id, 8)) + fbm(uv, 32, 3) * 0.08 * stone
            }
            Baked::Hammered => {
                // Beaten metal: the shallow spherical dents of many strikes, each flattening
                // what was there, so the surface is their soft lower envelope.
                let mut h = 0.0f32;
                points(uv, 11, 37, |d, id| {
                    let r = 0.75 + 0.25 * hash1(id, 2);
                    let depth = 0.6 + 0.4 * hash1(id, 6);
                    let dent = depth * ((d.length() / r).powi(2) - 1.0);
                    h = smooth_min(h, dent, 0.08);
                });
                h + fbm(uv, 64, 2) * 0.03
            }
        }
    }
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// |x| with its point rounded off over about `e`.
fn soft_abs(x: f32, e: f32) -> f32 {
    (x * x + e * e).sqrt() - e
}

/// A round bump of height 1 at `t` = 0 that lands flat (zero slope) at `t` = 1.
fn bump(t: f32) -> f32 {
    (1.0 - t * t).max(0.0).powi(2)
}

/// Polynomial smooth minimum: the lower of `a` and `b`, the corner rounded over about `k`.
fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

fn smooth_max(a: f32, b: f32, k: f32) -> f32 {
    -smooth_min(-a, -b, k)
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
    fbm_aniso(uv, [period, period], octaves)
}

fn fbm_aniso(uv: Vec2, period: [i32; 2], octaves: u32) -> f32 {
    let (mut sum, mut amp, mut total) = (0.0, 1.0, 0.0);
    for o in 0..octaves {
        sum += noise_aniso(uv, [period[0] << o, period[1] << o]) * amp;
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

/// Calls `f(offset, id)` for each of the jittered points (one per cell of an `n` × `n` grid
/// repeating over uv 0..1) in the 3 × 3 cells around `uv`, with `offset` from the point to
/// `uv` in cell units. Every point within 1.1 cells of `uv` is among them.
fn points(uv: Vec2, n: i32, seed: u32, mut f: impl FnMut(Vec2, u32)) {
    let p = uv * n as f32;
    let (cx, cy) = (p.x.floor() as i32, p.y.floor() as i32);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (gx, gy) = (cx + dx, cy + dy);
            let (wx, wy) = (gx.rem_euclid(n), gy.rem_euclid(n));
            let h = hash(wx, wy, seed);
            let jitter = Vec2::new((h & 0xffff) as f32 / 65535.0, (h >> 16) as f32 / 65535.0);
            let point = Vec2::new(gx as f32, gy as f32) + Vec2::splat(0.1) + jitter * 0.8;
            f(p - point, (wy * n + wx) as u32);
        }
    }
}

/// Distances to the nearest and second nearest of `n` × `n` jittered points (in cell units)
/// that repeat over uv 0..1, and an id for the nearest one's cell. Searches 5 × 5 cells, so
/// both distances are exact and the cell borders have no breaks.
fn worley(uv: Vec2, n: i32, seed: u32) -> (f32, f32, u32) {
    let p = uv * n as f32;
    let (cx, cy) = (p.x.floor() as i32, p.y.floor() as i32);
    let (mut f1, mut f2, mut id) = (f32::MAX, f32::MAX, 0);
    for dy in -2..=2 {
        for dx in -2..=2 {
            let (gx, gy) = (cx + dx, cy + dy);
            let (wx, wy) = (gx.rem_euclid(n), gy.rem_euclid(n));
            let h = hash(wx, wy, seed);
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

static IMAGES: [OnceLock<Vec<u16>>; COUNT] = [const { OnceLock::new() }; COUNT];

/// Heights of `b` over an `n` × `n` grid of texel centres, stretched to 0..1.
fn paint(b: Baked, n: usize) -> Vec<f32> {
    // Painted on plain threads, not rayon's: the first use is often inside a brush's
    // parallel loop, and a rayon worker waiting here could pick up another dab that
    // waits for this same image, and never finish (a deadlock).
    let mut raw = vec![0.0f32; n * n];
    let threads = std::thread::available_parallelism().map_or(4, |t| t.get()).min(16);
    let rows = n.div_ceil(threads);
    std::thread::scope(|scope| {
        for (k, part) in raw.chunks_mut(rows * n).enumerate() {
            scope.spawn(move || {
                for (j, out) in part.iter_mut().enumerate() {
                    let i = k * rows * n + j;
                    *out = b.height(Vec2::new(((i % n) as f32 + 0.5) / n as f32, ((i / n) as f32 + 0.5) / n as f32));
                }
            });
        }
    });
    let (lo, hi) = raw.iter().fold((f32::MAX, f32::MIN), |(a, z), &v| (a.min(v), z.max(v)));
    let span = (hi - lo).max(1e-6);
    raw.iter_mut().for_each(|v| *v = (*v - lo) / span);
    raw
}

/// The image of `b`, 0..65535 heights, row 0 at uv.y = 0, painted on first use (8 MB, a
/// few tens of milliseconds).
pub fn image(b: Baked) -> &'static [u16] {
    IMAGES[b.index()].get_or_init(|| paint(b, SIZE).into_iter().map(|v| (v * 65535.0).round() as u16).collect())
}

/// Weights of the four texels around a point `t` (0..1) past the second, for a uniform
/// cubic B-spline: smooth in value, slope and curvature, so no texel edges show in shading.
pub fn bspline(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    let s = 1.0 - t;
    [s * s * s / 6.0, (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0, (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0, t3 / 6.0]
}

/// Height of `b` at `uv`, repeating, 0..1: a cubic B-spline over the 4 × 4 nearest texels.
pub fn sample(b: Baked, uv: Vec2) -> f32 {
    let img = image(b);
    let p = (uv - uv.floor()) * SIZE as f32 - Vec2::splat(0.5);
    let i = p.floor();
    let (wx, wy) = (bspline(p.x - i.x), bspline(p.y - i.y));
    let (ix, iy) = (i.x as i32 - 1, i.y as i32 - 1);
    let mut sum = 0.0;
    for (dy, wy) in wy.iter().enumerate() {
        let row = &img[((iy + dy as i32) & MASK) as usize * SIZE..][..SIZE];
        let mut r = 0.0;
        for (dx, wx) in wx.iter().enumerate() {
            r += row[((ix + dx as i32) & MASK) as usize] as f32 * wx;
        }
        sum += r * wy;
    }
    sum / 65535.0
}

/// An `n` × `n` view of `b` over uv 0..1 for a thumbnail, 0..1: painted straight from the
/// pattern at four times the size and averaged down, so fine seams still show.
pub fn preview(b: Baked, n: usize) -> Vec<f32> {
    let big = paint(b, n * 4);
    (0..n * n)
        .map(|i| {
            let (x, y) = (i % n * 4, i / n * 4);
            (0..16).map(|k| big[(y + k / 4) * n * 4 + x + k % 4]).sum::<f32>() / 16.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_fills_the_range_tiles_and_differs() {
        let mut seen: Vec<Vec<u16>> = Vec::new();
        for b in Baked::ALL {
            let img = image(b);
            let (lo, hi) = img.iter().fold((u16::MAX, 0), |(a, z), &v| (a.min(v), z.max(v)));
            assert!(lo == 0 && hi == u16::MAX, "{b:?}");
            let at = |x: usize, y: usize| img[y * SIZE + x] as f32 / 65535.0;
            // Seamless: across the wrap, neighbours differ no more than neighbours inside do.
            let step_x = |x0: usize, x1: usize| (0..SIZE).map(|y| (at(x0, y) - at(x1, y)).abs()).sum::<f32>() / SIZE as f32;
            let step_y = |y0: usize, y1: usize| (0..SIZE).map(|x| (at(x, y0) - at(x, y1)).abs()).sum::<f32>() / SIZE as f32;
            let inside = (1..SIZE / 2).step_by(3).map(|k| step_x(k, k + 1).max(step_y(k, k + 1))).fold(0.0, f32::max);
            let (seam, seam_y) = (step_x(SIZE - 1, 0), step_y(SIZE - 1, 0));
            assert!(seam <= inside * 1.1 && seam_y <= inside * 1.1, "{b:?} seams {seam} {seam_y} vs {inside}");
            // Neither flat nor noise only: a fair share of mid and high values.
            let mean = img.iter().map(|&v| v as f32 / 65535.0).sum::<f32>() / img.len() as f32;
            assert!(mean > 0.05 && mean < 0.9, "{b:?} mean {mean}");
            let sample: Vec<u16> = img.iter().step_by(9973).copied().collect();
            assert!(seen.iter().all(|s| s != &sample), "{b:?} duplicates another");
            seen.push(sample);
        }
        assert!((sample(Baked::Rock, Vec2::new(0.25, 0.5)) - sample(Baked::Rock, Vec2::new(1.25, -0.5))).abs() < 1e-6);
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = Baked::ALL.iter().map(|b| b.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), COUNT);
    }

    /// Steps along lines of `count` samples `step` apart in uv, from a few starts and
    /// directions. Returns the largest change in height between neighbours, and the largest
    /// change in that change (where the slope breaks: texel steps show up here).
    fn roughness(f: impl Fn(Vec2) -> f32, step: f32, count: usize) -> (f32, f32) {
        let (mut jump, mut kink) = (0.0f32, 0.0f32);
        for k in 0..48u32 {
            let a = k as f32 * 2.39996;
            let dir = Vec2::new(a.cos(), a.sin());
            let start = Vec2::new(hash1(k, 1), hash1(k, 2));
            let h: Vec<f32> = (0..count).map(|i| f(start + dir * (i as f32 * step))).collect();
            let d: Vec<f32> = h.windows(2).map(|w| w[1] - w[0]).collect();
            jump = jump.max(d.iter().map(|x| x.abs()).fold(0.0, f32::max));
            kink = kink.max(d.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max));
        }
        (jump, kink)
    }

    #[test]
    fn large_stamps_sample_smoothly_across_edges() {
        // Scales as a texture at Draw Size 1.27 and Tiling 2 over a 16.6M-vertex sphere of
        // radius 1 is about 0.0007 uv between vertices; sample three times finer than that.
        let step = 0.00025;
        for b in Baked::ALL {
            let (jump, kink) = roughness(|uv| sample(b, uv), step, 3000);
            // No cliffs: neighbouring samples never differ by more than a few percent of
            // the full height.
            assert!(jump < 0.08, "{b:?} jumps {jump}");
            // And the slope bends gradually instead of breaking at texel edges: from one
            // pair of neighbours to the next it changes by much less than the steepest slope.
            assert!(kink < jump * 0.65, "{b:?} slope breaks {kink} vs {jump}");
        }
    }

    #[test]
    fn bilinear_sampling_would_show_texel_steps() {
        // The same measure on a bilinear read of the image, as Klay did before: here the
        // slope breaks at every texel edge, which is what the shading shows as steps.
        let b = Baked::Scales;
        let img = image(b);
        let bilinear = |uv: Vec2| {
            let p = (uv - uv.floor()) * SIZE as f32 - Vec2::splat(0.5);
            let i = p.floor();
            let t = p - i;
            let at = |dx: i32, dy: i32| img[((i.y as i32 + dy) & MASK) as usize * SIZE + ((i.x as i32 + dx) & MASK) as usize] as f32 / 65535.0;
            let top = at(0, 0) + (at(1, 0) - at(0, 0)) * t.x;
            let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * t.x;
            top + (bottom - top) * t.y
        };
        let step = 0.05 / SIZE as f32;
        let (jump, kink) = roughness(bilinear, step, 3000);
        let (smooth_jump, smooth_kink) = roughness(|uv| sample(b, uv), step, 3000);
        assert!(kink > jump * 0.15, "bilinear {kink} vs {jump}");
        assert!(smooth_kink < smooth_jump * 0.05, "B-spline {smooth_kink} vs {smooth_jump}");
        assert!(kink > smooth_kink * 5.0, "bilinear {kink} vs B-spline {smooth_kink}");
    }

    #[test]
    fn bspline_weights_sum_to_one_and_join_smoothly() {
        for k in 0..=20 {
            let w = bspline(k as f32 / 20.0);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
            assert!(w.iter().all(|&x| x >= 0.0));
        }
        // At t = 1 the weights equal those at t = 0 shifted one texel along.
        let (a, b) = (bspline(1.0), bspline(0.0));
        assert!((a[1] - b[0]).abs() < 1e-6 && (a[2] - b[1]).abs() < 1e-6 && (a[3] - b[2]).abs() < 1e-6);
    }

    #[test]
    fn previews_match_the_painted_image() {
        let b = Baked::Panels;
        let direct = preview(b, 64);
        let read: Vec<f32> = (0..64 * 64)
            .map(|i| {
                let c = Vec2::new((i % 64) as f32, (i / 64) as f32) / 64.0;
                (0..16).map(|k| sample(b, c + Vec2::new((k % 4) as f32 + 0.5, (k / 4) as f32 + 0.5) / 256.0)).sum::<f32>() / 16.0
            })
            .collect();
        let diff = direct.iter().zip(&read).map(|(a, b)| (a - b).abs()).sum::<f32>() / direct.len() as f32;
        assert!(diff < 0.05, "preview differs from image by {diff}");
    }

    #[test]
    #[ignore]
    fn export_sheet() {
        // KLAY_EXPORT_ALPHAS=dir cargo test --release export_sheet -- --ignored
        let Ok(dir) = std::env::var("KLAY_EXPORT_ALPHAS") else { return };
        let n = 256;
        let cols = 8;
        let rows = COUNT.div_ceil(cols);
        let mut out = vec![0u8; n * cols * n * rows];
        for (k, b) in Baked::ALL.iter().enumerate() {
            let (ox, oy) = ((k % cols) * n, (k / cols) * n);
            for y in 0..n {
                for x in 0..n {
                    let uv = Vec2::new((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
                    let a = crate::brush_asset::Pattern::Library(*b).alpha(uv);
                    out[(oy + y) * n * cols + ox + x] = (a * 255.0) as u8;
                }
            }
        }
        image::save_buffer(std::path::Path::new(&dir).join("alphas.png"), &out, (n * cols) as u32, (n * rows) as u32, image::ColorType::L8).unwrap();
    }
}
