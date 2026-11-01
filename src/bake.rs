//! Normal and displacement maps (ZBrush: Tool › Normal Map and Displacement Map): the detail
//! of the highest subdivision level, put into textures over the base level's UVs.
//!
//! Every triangle of the highest level lies inside one base triangle, at a place known from
//! the subdivision itself ([`Place`]), so nothing is ray cast and nothing can miss. The
//! highest level's triangles are drawn into the texture at their UVs, and each sample reads:
//! - for a normal map, the highest level's smooth normal, either in the tangent space of
//!   the base level (its smooth normals and UV tangents, the way Blender and game engines
//!   read tangent-space maps) or in object space;
//! - for a displacement map, how far the highest level stands out from the base level
//!   divided smoothly with nothing sculpted above it, along that smooth surface's normal:
//!   what a renderer that subdivides the base and displaces it expects.
//!
//! Samples are averaged per texel, and the islands are padded outwards so that filtering
//! and mipmaps do not pull the background in.

use glam::{Mat3, Vec2, Vec3};
use rayon::prelude::*;

/// Where a triangle of the highest level lies in the base level: the base triangle, and
/// for each of its corners the barycentric weights of the base triangle's corners 1 and 2
/// (corner 0 takes the rest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub tri: u32,
    pub w: [Vec2; 3],
}

/// What a bake reads, copied out of a subtool so it can run on a thread of its own.
pub struct Source {
    pub base_positions: Vec<Vec3>,
    pub base_indices: Vec<u32>,
    /// One UV triple per base triangle.
    pub uvs: Vec<[Vec2; 3]>,
    /// The highest level.
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// One per triangle of the highest level.
    pub places: Vec<Place>,
    /// The base level divided smoothly, in the highest level's vertex order; only needed
    /// for a displacement map (empty otherwise).
    pub smooth: Vec<Vec3>,
}

impl Source {
    /// Everything a bake of `kind` needs from `doc`, which must have UVs. The levels are
    /// synced first, so the highest level is up to date.
    pub fn of(doc: &mut crate::multires::Multires, kind: Kind) -> Self {
        doc.sync_to(doc.top());
        let base = doc.base_mesh();
        let top = doc.level_mesh(doc.top());
        Self {
            base_positions: base.positions.clone(),
            base_indices: base.indices.clone(),
            uvs: doc.uvs().to_vec(),
            positions: top.positions.clone(),
            indices: top.indices.clone(),
            places: doc.top_places(),
            smooth: if kind == Kind::Displacement { doc.smooth_top() } else { Vec::new() },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Displacement,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Normal => "Normal map",
            Kind::Displacement => "Displacement map",
        }
    }
}

/// The space a normal map is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    /// Relative to the surface (Blender, game engines): blue where the detail is flat.
    Tangent,
    /// Object-space directions, for a model that does not deform.
    Object,
}

/// Tool › Texture Maps settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Width and height in texels.
    pub size: usize,
    /// Samples per texel along each side (1 to 4).
    pub samples: u32,
    /// Texels the islands are grown by.
    pub padding: usize,
    pub space: Space,
    /// Green points down (DirectX, Unreal) instead of up (OpenGL, Blender, Unity).
    pub flip_g: bool,
    /// Displacement as 32-bit float OpenEXR in model units; otherwise a 16-bit PNG with
    /// mid grey for no displacement.
    pub exr: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { size: 2048, samples: 2, padding: 8, space: Space::Tangent, flip_g: false, exr: true }
    }
}

/// A baked map: `channels` floats per texel (3 for a normal map, its components in
/// -1..1; 1 for a displacement map, in model units), row 0 at the top (V = 1).
pub struct Map {
    pub kind: Kind,
    pub size: usize,
    pub channels: usize,
    pub data: Vec<f32>,
    /// Share of texels the islands cover, before padding.
    pub coverage: f32,
    /// Displacement only: the largest distance in or out.
    pub max_abs: f32,
}

/// Area-weighted vertex normals (as [`SculptMesh::normal_at`](crate::mesh::SculptMesh::normal_at)
/// and Export OBJ compute them).
fn vertex_normals(p: &[Vec3], idx: &[u32]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; p.len()];
    for t in idx.chunks_exact(3) {
        let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
        let f = (b - a).cross(c - a);
        for &v in t {
            n[v as usize] += f;
        }
    }
    n.par_iter_mut().for_each(|x| *x = x.normalize_or(Vec3::Y));
    n
}

/// Per base corner: its UV tangent (made perpendicular to the vertex normal) and the sign
/// of the bitangent, shared by the corners of a vertex that have the same UV (one island),
/// each triangle weighted by its angle there, as MikkTSpace does.
fn corner_tangents(p: &[Vec3], idx: &[u32], uvs: &[[Vec2; 3]], normals: &[Vec3]) -> Vec<(Vec3, f32)> {
    use crate::fx::FxHashMap;
    let key = |t: usize, k: usize| (idx[3 * t + k], uvs[t][k].x.to_bits(), uvs[t][k].y.to_bits());
    let mut sums: FxHashMap<(u32, u32, u32), (Vec3, Vec3)> = FxHashMap::default();
    for t in 0..idx.len() / 3 {
        let v = [idx[3 * t], idx[3 * t + 1], idx[3 * t + 2]].map(|i| p[i as usize]);
        let q = uvs[t];
        let (e1, e2) = (v[1] - v[0], v[2] - v[0]);
        let (d1, d2) = (q[1] - q[0], q[2] - q[0]);
        let r = d1.x * d2.y - d2.x * d1.y;
        if r.abs() < 1e-20 {
            continue;
        }
        let tan = ((e1 * d2.y - e2 * d1.y) / r).normalize_or_zero();
        let bit = ((e2 * d1.x - e1 * d2.x) / r).normalize_or_zero();
        for k in 0..3 {
            let (a, b) = (v[(k + 1) % 3] - v[k], v[(k + 2) % 3] - v[k]);
            let angle = a.angle_between(b);
            let angle = if angle.is_finite() { angle } else { 0.0 };
            let s = sums.entry(key(t, k)).or_insert((Vec3::ZERO, Vec3::ZERO));
            s.0 += tan * angle;
            s.1 += bit * angle;
        }
    }
    (0..idx.len())
        .into_par_iter()
        .map(|c| {
            let (t, k) = (c / 3, c % 3);
            let n = normals[idx[c] as usize];
            let (tan, bit) = sums.get(&key(t, k)).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let mut tan = (tan - n * n.dot(tan)).normalize_or_zero();
            if tan == Vec3::ZERO {
                tan = n.any_orthonormal_vector();
            }
            let sign = if n.cross(tan).dot(bit) < 0.0 { -1.0 } else { 1.0 };
            (tan, sign)
        })
        .collect()
}

/// What a sample interpolates over one triangle of the highest level, per corner.
#[derive(Clone, Copy, Default)]
struct Corner {
    /// The highest level's normal (normal map) or the displacement in `nh.x`.
    nh: Vec3,
    /// The base level's normal and tangent there (tangent-space normal maps).
    n: Vec3,
    t: Vec3,
}

/// Bake a map of `kind` from `src`.
pub fn bake(src: &Source, kind: Kind, s: &Settings) -> Result<Map, String> {
    if src.uvs.is_empty() {
        return Err("no UVs · Unwrap in UV Map first".into());
    }
    if src.uvs.len() * 3 != src.base_indices.len() || src.places.len() * 3 != src.indices.len() {
        return Err("the UVs or levels do not match the mesh".into());
    }
    if kind == Kind::Displacement && src.smooth.len() != src.positions.len() {
        return Err("the smooth base does not match the highest level".into());
    }
    let size = s.size.clamp(16, 16384);
    let ss = s.samples.clamp(1, 4) as usize;
    let channels = if kind == Kind::Normal { 3 } else { 1 };
    let tangent = kind == Kind::Normal && s.space == Space::Tangent;

    // Per-vertex values of the highest level.
    let high: Vec<Vec3> = match kind {
        Kind::Normal => vertex_normals(&src.positions, &src.indices),
        Kind::Displacement => {
            let sn = vertex_normals(&src.smooth, &src.indices);
            src.positions
                .par_iter()
                .zip(&src.smooth)
                .zip(&sn)
                .map(|((p, q), n)| Vec3::new((*p - *q).dot(*n), 0.0, 0.0))
                .collect()
        }
    };
    let max_abs = if kind == Kind::Displacement { high.par_iter().map(|d| d.x.abs()).reduce(|| 0.0, f32::max) } else { 0.0 };
    // The base level's frames, per corner.
    let (base_n, frames) = if tangent {
        let n = vertex_normals(&src.base_positions, &src.base_indices);
        let f = corner_tangents(&src.base_positions, &src.base_indices, &src.uvs, &n);
        (n, f)
    } else {
        (Vec::new(), Vec::new())
    };

    // UVs of the highest level's triangles, in sample coordinates (Y down).
    let g = (size * ss) as f32;
    let uv_of = |pl: &Place, k: usize| {
        let q = src.uvs[pl.tri as usize];
        let w = pl.w[k];
        q[0] * (1.0 - w.x - w.y) + q[1] * w.x + q[2] * w.y
    };
    let screen = |t: usize| -> [Vec2; 3] {
        let pl = &src.places[t];
        std::array::from_fn(|k| {
            let uv = uv_of(pl, k);
            Vec2::new(uv.x * g, (1.0 - uv.y) * g)
        })
    };
    let corners = |t: usize| -> [Corner; 3] {
        let pl = &src.places[t];
        std::array::from_fn(|k| {
            let mut c = Corner { nh: high[src.indices[3 * t + k] as usize], ..Default::default() };
            if tangent {
                let w = pl.w[k];
                let b = [1.0 - w.x - w.y, w.x, w.y];
                let bt = pl.tri as usize;
                for j in 0..3 {
                    c.n += base_n[src.base_indices[3 * bt + j] as usize] * b[j];
                    c.t += frames[3 * bt + j].0 * b[j];
                }
            }
            c
        })
    };
    let sign_of = |t: usize| frames.get(3 * src.places[t].tri as usize).map_or(1.0, |f| f.1);

    // Triangles into bands of texel rows, so each band is filled by one thread.
    const BAND: usize = 16;
    let bands = size.div_ceil(BAND);
    let mut pairs: Vec<(u32, u32)> = (0..src.places.len())
        .into_par_iter()
        .flat_map_iter(|t| {
            let q = screen(t);
            let lo = q[0].y.min(q[1].y).min(q[2].y);
            let hi = q[0].y.max(q[1].y).max(q[2].y);
            let ok = lo.is_finite() && hi.is_finite() && hi >= 0.0 && lo < g;
            let (a, b) = if ok {
                ((lo.max(0.0) as usize / ss / BAND).min(bands - 1), (hi.min(g - 1.0) as usize / ss / BAND).min(bands - 1))
            } else {
                (1, 0)
            };
            (a..=b).map(move |band| (band as u32, t as u32))
        })
        .collect();
    pairs.par_sort_unstable();
    let mut starts = vec![0usize; bands + 1];
    for &(b, _) in &pairs {
        starts[b as usize + 1] += 1;
    }
    for b in 0..bands {
        starts[b + 1] += starts[b];
    }

    let mut data = vec![0f32; size * size * channels];
    let mut count = vec![0u16; size * size];
    data.par_chunks_mut(BAND * size * channels)
        .zip(count.par_chunks_mut(BAND * size))
        .enumerate()
        .for_each(|(band, (out, cnt))| {
            let rows = cnt.len() / size;
            let (y0, y1) = (band * BAND * ss, (band * BAND + rows) * ss);
            for &(_, t) in &pairs[starts[band]..starts[band + 1]] {
                let t = t as usize;
                let q = screen(t);
                let area = (q[1] - q[0]).perp_dot(q[2] - q[0]);
                if area.abs() < 1e-12 {
                    continue;
                }
                let c = corners(t);
                let sign = sign_of(t);
                let lo = q[0].min(q[1]).min(q[2]);
                let hi = q[0].max(q[1]).max(q[2]);
                let ya = ((lo.y - 0.5).ceil().max(y0 as f32)) as usize;
                let yb = ((hi.y - 0.5).floor().min(y1 as f32 - 1.0)) as isize;
                let xa = ((lo.x - 0.5).ceil().max(0.0)) as usize;
                let xb = ((hi.x - 0.5).floor().min(g - 1.0)) as isize;
                if yb < ya as isize || xb < xa as isize {
                    continue;
                }
                let eps = -1e-5 * area.abs();
                for sy in ya..=yb as usize {
                    for sx in xa..=xb as usize {
                        let p = Vec2::new(sx as f32 + 0.5, sy as f32 + 0.5);
                        let l0 = (q[2] - q[1]).perp_dot(p - q[1]) * area.signum();
                        let l1 = (q[0] - q[2]).perp_dot(p - q[2]) * area.signum();
                        let l2 = (q[1] - q[0]).perp_dot(p - q[0]) * area.signum();
                        if l0 < eps || l1 < eps || l2 < eps {
                            continue;
                        }
                        let inv = 1.0 / area.abs();
                        let (b0, b1, b2) = (l0 * inv, l1 * inv, l2 * inv);
                        let texel = (sy / ss - band * BAND) * size + sx / ss;
                        let o = &mut out[texel * channels..texel * channels + channels];
                        let nh = c[0].nh * b0 + c[1].nh * b1 + c[2].nh * b2;
                        match kind {
                            Kind::Displacement => o[0] += nh.x,
                            Kind::Normal => {
                                let nh = nh.normalize_or(Vec3::Z);
                                let v = if tangent {
                                    let n = (c[0].n * b0 + c[1].n * b1 + c[2].n * b2).normalize_or(Vec3::Z);
                                    let tg = (c[0].t * b0 + c[1].t * b1 + c[2].t * b2).normalize_or(Vec3::X);
                                    let bt = n.cross(tg) * sign;
                                    let m = Mat3::from_cols(tg, bt, n);
                                    let x = if m.determinant().abs() > 1e-6 { m.inverse() * nh } else { Vec3::new(tg.dot(nh), bt.dot(nh), n.dot(nh)) };
                                    let mut x = x.normalize_or(Vec3::Z);
                                    if s.flip_g {
                                        x.y = -x.y;
                                    }
                                    x
                                } else {
                                    nh
                                };
                                o[0] += v.x;
                                o[1] += v.y;
                                o[2] += v.z;
                            }
                        }
                        cnt[texel] = cnt[texel].saturating_add(1);
                    }
                }
            }
            // Means, and normals of unit length again.
            for (i, &n) in cnt.iter().enumerate() {
                if n > 0 {
                    let o = &mut out[i * channels..i * channels + channels];
                    if channels == 3 {
                        let v = Vec3::new(o[0], o[1], o[2]).normalize_or(Vec3::Z);
                        o.copy_from_slice(&v.to_array());
                    } else {
                        o[0] /= n as f32;
                    }
                }
            }
        });
    let covered = count.par_iter().filter(|&&n| n > 0).count();
    let mut known: Vec<bool> = count.par_iter().map(|&n| n > 0).collect();
    drop(count);
    pad(&mut data, &mut known, size, channels, s.padding);
    // The background: flat.
    let empty: &[f32] = if channels == 3 { if kind == Kind::Normal && s.space == Space::Object { &[0.0, 0.0, 0.0] } else { &[0.0, 0.0, 1.0] } } else { &[0.0] };
    data.par_chunks_mut(channels).zip(known.par_iter()).for_each(|(o, &k)| {
        if !k {
            o.copy_from_slice(empty);
        }
    });
    Ok(Map { kind, size, channels, data, coverage: covered as f32 / (size * size) as f32, max_abs })
}

/// Grow the known texels outwards `rounds` times: each unknown texel next to known ones
/// takes their mean.
fn pad(data: &mut [f32], known: &mut [bool], size: usize, channels: usize, rounds: usize) {
    for _ in 0..rounds {
        let grown: Vec<Option<[f32; 3]>> = (0..size * size)
            .into_par_iter()
            .map(|i| {
                if known[i] {
                    return None;
                }
                let (x, y) = ((i % size) as isize, (i / size) as isize);
                let mut sum = [0f32; 3];
                let mut n = 0;
                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        let (nx, ny) = (x + dx, y + dy);
                        if (dx, dy) == (0, 0) || nx < 0 || ny < 0 || nx >= size as isize || ny >= size as isize {
                            continue;
                        }
                        let j = ny as usize * size + nx as usize;
                        if known[j] {
                            for c in 0..channels {
                                sum[c] += data[j * channels + c];
                            }
                            n += 1;
                        }
                    }
                }
                (n > 0).then(|| sum.map(|v| v / n as f32))
            })
            .collect();
        let mut any = false;
        for (i, g) in grown.into_iter().enumerate() {
            if let Some(v) = g {
                let o = &mut data[i * channels..i * channels + channels];
                o.copy_from_slice(&v[..channels]);
                if channels == 3 {
                    let n = Vec3::from_slice(o).normalize_or(Vec3::Z);
                    o.copy_from_slice(&n.to_array());
                }
                known[i] = true;
                any = true;
            }
        }
        if !any {
            break;
        }
    }
}

/// Write `map` to `path`: a normal map as an 8-bit RGB PNG; a displacement map as a 32-bit
/// float OpenEXR (`.exr`) in model units, or a 16-bit grey PNG where mid grey is no
/// displacement and white is `max_abs` out. Returns the bytes written.
pub fn write(map: &Map, path: &std::path::Path) -> Result<u64, String> {
    let exr = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    let n = map.size as u32;
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    match (map.kind, exr) {
        (Kind::Normal, false) => {
            let px: Vec<u8> = map.data.par_iter().map(|&v| ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8).collect();
            image::RgbImage::from_raw(n, n, px).expect("size matches").save(path).map_err(|e| fail(&e))?;
        }
        (Kind::Displacement, false) => {
            let scale = map.max_abs.max(1e-12);
            let px: Vec<u16> = map.data.par_iter().map(|&d| ((0.5 + d / (2.0 * scale)).clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
            image::ImageBuffer::<image::Luma<u16>, _>::from_raw(n, n, px).expect("size matches").save(path).map_err(|e| fail(&e))?;
        }
        (_, true) => {
            let names: &[&str] = if map.channels == 3 { &["B", "G", "R"] } else { &["Y"] };
            // Channels go in name order: B, G, R from the stored R, G, B.
            let order: Vec<usize> = if map.channels == 3 { vec![2, 1, 0] } else { vec![0] };
            std::fs::write(path, exr_bytes(map.size, map.channels, &map.data, names, &order)).map_err(|e| fail(&e))?;
        }
    }
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| fail(&e))
}

/// An uncompressed scanline OpenEXR file of 32-bit float channels.
fn exr_bytes(size: usize, channels: usize, data: &[f32], names: &[&str], order: &[usize]) -> Vec<u8> {
    fn attr(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(kind.as_bytes());
        out.push(0);
        out.extend_from_slice(&(value.len() as i32).to_le_bytes());
        out.extend_from_slice(value);
    }
    let mut out = vec![0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0];
    let mut ch = Vec::new();
    for name in names {
        ch.extend_from_slice(name.as_bytes());
        ch.push(0);
        ch.extend_from_slice(&2i32.to_le_bytes()); // FLOAT
        ch.extend_from_slice(&[0, 0, 0, 0]); // pLinear, reserved
        ch.extend_from_slice(&1i32.to_le_bytes());
        ch.extend_from_slice(&1i32.to_le_bytes());
    }
    ch.push(0);
    attr(&mut out, "channels", "chlist", &ch);
    attr(&mut out, "compression", "compression", &[0]);
    let window: Vec<u8> = [0i32, 0, size as i32 - 1, size as i32 - 1].iter().flat_map(|v| v.to_le_bytes()).collect();
    attr(&mut out, "dataWindow", "box2i", &window);
    attr(&mut out, "displayWindow", "box2i", &window);
    attr(&mut out, "lineOrder", "lineOrder", &[0]);
    attr(&mut out, "pixelAspectRatio", "float", &1f32.to_le_bytes());
    attr(&mut out, "screenWindowCenter", "v2f", &[0u8; 8]);
    attr(&mut out, "screenWindowWidth", "float", &1f32.to_le_bytes());
    out.push(0);
    let line = 8 + size * channels * 4;
    let table = out.len() + size * 8;
    for y in 0..size {
        out.extend_from_slice(&((table + y * line) as u64).to_le_bytes());
    }
    out.reserve(size * line);
    for y in 0..size {
        out.extend_from_slice(&(y as i32).to_le_bytes());
        out.extend_from_slice(&((size * channels * 4) as i32).to_le_bytes());
        for &c in order {
            for x in 0..size {
                out.extend_from_slice(&data[(y * size + x) * channels + c].to_le_bytes());
            }
        }
    }
    out
}

/// A small picture of `map`, `side` pixels square, RGBA (the Texture Maps preview).
pub fn preview(map: &Map, side: usize) -> Vec<[u8; 4]> {
    let step = map.size as f32 / side as f32;
    let scale = map.max_abs.max(1e-12);
    (0..side * side)
        .map(|i| {
            let (x, y) = (((i % side) as f32 + 0.5) * step, ((i / side) as f32 + 0.5) * step);
            let j = (y as usize).min(map.size - 1) * map.size + (x as usize).min(map.size - 1);
            let d = &map.data[j * map.channels..j * map.channels + map.channels];
            let b = |v: f32| ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            if map.channels == 3 { [b(d[0]), b(d[1]), b(d[2]), 255] } else { let g = b(d[0] / scale); [g, g, g, 255] }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::SculptMesh;
    use crate::multires::Multires;

    /// A UV-mapped sphere divided twice, its highest level pushed out by `bump`.
    fn stack(bump: impl Fn(Vec3) -> f32 + Sync) -> Multires {
        stack_n(2, bump)
    }

    fn stack_n(divides: usize, bump: impl Fn(Vec3) -> f32 + Sync) -> Multires {
        let (p, i) = crate::mesh::cube_sphere(6, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let u = crate::uv::unwrap(&mesh.positions, &mesh.indices, &[], &crate::uv::Settings::default()).unwrap();
        let mut doc = Multires::new(mesh, usize::MAX);
        doc.set_uvs(u.uvs);
        for _ in 0..divides {
            doc.divide();
        }
        let top = doc.top();
        let all: Vec<u32> = (0..doc.mesh().chunks.len() as u32).collect();
        let snap = doc.mesh().snapshot_chunks(&all);
        let m = doc.mesh_mut();
        let moved: Vec<Vec3> = m.positions.iter().map(|&q| q + q.normalize() * bump(q)).collect();
        m.positions = moved;
        doc.finish_stroke(top, snap);
        doc
    }

    fn settings() -> Settings {
        Settings { size: 128, samples: 1, padding: 2, ..Settings::default() }
    }

    #[test]
    fn every_top_triangle_lies_where_its_uvs_put_it() {
        let doc = stack(|_| 0.0);
        let places = doc.top_places();
        let top = doc.level_mesh(doc.top());
        assert_eq!(places.len(), top.triangle_count());
        // The base triangle, blended at a top corner, gives the smooth surface's direction
        // there (both lie on the sphere's rays only roughly, so compare loosely).
        let base = doc.base_mesh();
        for (t, pl) in places.iter().enumerate().step_by(37) {
            for k in 0..3 {
                let b = &base.indices[3 * pl.tri as usize..3 * pl.tri as usize + 3];
                let w = pl.w[k];
                let flat = base.positions[b[0] as usize] * (1.0 - w.x - w.y) + base.positions[b[1] as usize] * w.x + base.positions[b[2] as usize] * w.y;
                let fine = top.positions[top.indices[3 * t + k] as usize];
                assert!(flat.normalize().dot(fine.normalize()) > 0.99, "{t} {k}");
            }
        }
        // Every base triangle holds sixteen top triangles.
        let mut per = vec![0; base.triangle_count()];
        for pl in &places {
            per[pl.tri as usize] += 1;
        }
        assert!(per.iter().all(|&n| n == 16));
    }

    #[test]
    fn an_unsculpted_stack_bakes_flat() {
        let mut doc = stack(|_| 0.0);
        let src = Source::of(&mut doc, Kind::Displacement);
        let d = bake(&src, Kind::Displacement, &settings()).unwrap();
        assert!(d.max_abs < 1e-5, "{}", d.max_abs);
        assert!(d.coverage > 0.3, "{}", d.coverage);
        let src = Source::of(&mut doc, Kind::Normal);
        let n = bake(&src, Kind::Normal, &settings()).unwrap();
        // The smooth top against the base's interpolated normals: close to straight up.
        let mean_z = n.data.chunks_exact(3).map(|c| c[2]).sum::<f32>() / (n.size * n.size) as f32;
        assert!(mean_z > 0.97, "{mean_z}");
    }

    #[test]
    fn displacement_reads_back_the_bump() {
        let h = 0.05;
        let mut doc = stack(|q| if q.y > 0.5 { h } else { 0.0 });
        let src = Source::of(&mut doc, Kind::Displacement);
        let d = bake(&src, Kind::Displacement, &settings()).unwrap();
        assert!((d.max_abs - h).abs() < 0.01, "{}", d.max_abs);
        // Texels that read the raised cap are close to h, the rest close to zero.
        let near_h = d.data.iter().filter(|&&v| (v - h).abs() < 0.01).count();
        let near_0 = d.data.iter().filter(|&&v| v.abs() < 0.005).count();
        assert!(near_h > 200 && near_0 > 2000, "{near_h} {near_0}");
    }

    #[test]
    fn bumps_tilt_the_normal_map_and_flip_g_mirrors_green() {
        let mut doc = stack(|q| 0.03 * (q.x * 20.0).sin());
        let src = Source::of(&mut doc, Kind::Normal);
        let a = bake(&src, Kind::Normal, &settings()).unwrap();
        let b = bake(&src, Kind::Normal, &Settings { flip_g: true, ..settings() }).unwrap();
        let tilted = a.data.chunks_exact(3).filter(|c| c[2] < 0.95).count();
        assert!(tilted > 1000, "{tilted}");
        for (x, y) in a.data.chunks_exact(3).zip(b.data.chunks_exact(3)).step_by(97) {
            assert!((x[0] - y[0]).abs() < 1e-4 && (x[1] + y[1]).abs() < 1e-4 && (x[2] - y[2]).abs() < 1e-4);
        }
        // Unit length everywhere, padding and background included.
        assert!(a.data.chunks_exact(3).all(|c| (Vec3::from_slice(c).length() - 1.0).abs() < 1e-3));
    }

    #[test]
    fn maps_write_as_png_and_exr() {
        let mut doc = stack(|q| 0.02 * q.x);
        let dir = std::env::temp_dir().join(format!("klay-bake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = Source::of(&mut doc, Kind::Displacement);
        let d = bake(&src, Kind::Displacement, &settings()).unwrap();
        let exr = dir.join("d.exr");
        let bytes = write(&d, &exr).unwrap();
        assert_eq!(bytes as usize, std::fs::read(&exr).unwrap().len());
        let raw = std::fs::read(&exr).unwrap();
        assert_eq!(&raw[..4], &[0x76, 0x2f, 0x31, 0x01]);
        // Last scanline: its y, then its floats.
        let line = 8 + 128 * 4;
        let last = &raw[raw.len() - line..];
        assert_eq!(i32::from_le_bytes(last[..4].try_into().unwrap()), 127);
        let png = dir.join("d.png");
        write(&d, &png).unwrap();
        let img = image::open(&png).unwrap();
        assert_eq!(img.color(), image::ColorType::L16);
        let src = Source::of(&mut doc, Kind::Normal);
        let n = bake(&src, Kind::Normal, &settings()).unwrap();
        let np = dir.join("n.png");
        write(&n, &np).unwrap();
        let img = image::open(&np).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (128, 128));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Files for checking maps in Blender: the base level with UVs, the highest level, and
    /// both maps, into `$KLAY_BAKE_DIR`.
    #[test]
    #[ignore]
    fn blender_check_files() {
        let dir = std::path::PathBuf::from(std::env::var("KLAY_BAKE_DIR").expect("KLAY_BAKE_DIR"));
        let mut doc = stack_n(4, |q| 0.04 * (q.x * 9.0).sin() * (q.z * 9.0).sin() + if q.y > 0.6 { 0.05 } else { 0.0 });
        let s = Settings { size: 1024, ..Settings::default() };
        for kind in [Kind::Normal, Kind::Displacement] {
            let src = Source::of(&mut doc, kind);
            let m = bake(&src, kind, &s).unwrap();
            write(&m, &dir.join(if kind == Kind::Normal { "normal.png" } else { "disp.exr" })).unwrap();
        }
        let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("low.obj")).unwrap());
        doc.base_mesh().write_obj_uv(&mut w, "low", 0, doc.uvs(), 0).unwrap();
        drop(w);
        doc.level_mesh(doc.top()).export_obj(dir.join("high.obj").to_str().unwrap()).unwrap();
    }
}
