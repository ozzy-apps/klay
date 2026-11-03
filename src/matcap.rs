//! MatCaps: sphere-lit images looked up by the view-space normal, as in ZBrush. Each one is
//! two images of a lit sphere, painted here (no borrowed artwork): a diffuse layer that the
//! surface colour multiplies (the material's own colour, polypaint or a polygroup colour)
//! and a specular layer added on top, so painting tints a MatCap the way it tints clay.
//!
//! The renderer uploads every layer into one texture array (`gpu_layers`) and `shader.wgsl`
//! samples it; brush and material thumbnails sample the same images on the CPU (`sample`).
//!
//! Material › Load MatCap adds the user's own pictures of a lit sphere ([`Look::User`]): the
//! picture is the diffuse layer, there is no specular layer, and the unpainted colour is
//! white, so polypaint tints the picture. They are listed after the built-in MatCaps, copied
//! into the config folder (`klay/matcaps/`, with `klay/matcaps.list` for their order) and
//! loaded again at the next start ([`restore`]).

use glam::{Vec2, Vec3};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

/// Width and height of each MatCap image.
pub const SIZE: usize = 256;

/// Materials before the MatCaps are Klay's original procedural clays (`shader.wgsl`).
pub const FIRST: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    RedWax,
    SkinShade,
    GrayClay,
    WhiteClay,
    FormRead,
    Chrome,
    ToyPlastic,
    StudioGray,
    CavityWhite,
    Slate,
    RimDark,
    Normal,
    /// A loaded MatCap (Material › Load MatCap), by its index in [`users`].
    User(u16),
}

impl Look {
    pub const ALL: [Look; 12] = [
        Look::RedWax,
        Look::SkinShade,
        Look::GrayClay,
        Look::WhiteClay,
        Look::FormRead,
        Look::Chrome,
        Look::ToyPlastic,
        Look::StudioGray,
        Look::CavityWhite,
        Look::Slate,
        Look::RimDark,
        Look::Normal,
    ];

    /// Its name in the Material palette (`workspace::MATERIALS`, which must match).
    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            Look::RedWax => "MatCap Red Wax",
            Look::SkinShade => "MatCap Skin",
            Look::GrayClay => "MatCap Gray",
            Look::WhiteClay => "MatCap White",
            Look::FormRead => "MatCap Form",
            Look::Chrome => "MatCap Chrome",
            Look::ToyPlastic => "MatCap Toy Plastic",
            Look::StudioGray => "MatCap Studio Gray",
            Look::CavityWhite => "MatCap Cavity White",
            Look::Slate => "MatCap Slate",
            Look::RimDark => "MatCap Rim Dark",
            Look::Normal => "MatCap Normal",
            Look::User(_) => "MatCap",
        }
    }

    /// The surface colour where nothing is painted (linear, like the clays in the shader).
    pub fn base(self) -> Vec3 {
        match self {
            Look::RedWax => Vec3::new(0.58, 0.09, 0.06),
            Look::SkinShade => Vec3::new(0.80, 0.50, 0.40),
            Look::GrayClay => Vec3::new(0.50, 0.50, 0.51),
            Look::WhiteClay => Vec3::new(0.88, 0.87, 0.85),
            Look::FormRead => Vec3::new(0.66, 0.64, 0.62),
            Look::Chrome => Vec3::new(0.92, 0.93, 0.95),
            Look::ToyPlastic => Vec3::new(0.06, 0.24, 0.78),
            Look::StudioGray => Vec3::new(0.56, 0.56, 0.57),
            Look::CavityWhite => Vec3::new(0.86, 0.85, 0.83),
            Look::Slate => Vec3::new(0.30, 0.36, 0.44),
            Look::RimDark => Vec3::new(0.13, 0.13, 0.14),
            Look::Normal | Look::User(_) => Vec3::ONE,
        }
    }

    /// Diffuse and specular light for a view-space normal `n` (x right, y up, z toward the
    /// viewer).
    fn light(self, n: Vec3) -> (Vec3, Vec3) {
        let key = Vec3::new(-0.52, 0.62, 0.59).normalize();
        let fill = Vec3::new(0.72, -0.18, 0.67).normalize();
        let lambert = |l: Vec3| n.dot(l).max(0.0);
        // Light wrapping past the terminator, as in translucent wax and skin.
        let wrap = |l: Vec3, w: f32| ((n.dot(l) + w) / (1.0 + w)).max(0.0);
        let blinn = |l: Vec3, power: f32| n.dot((l + Vec3::Z).normalize()).max(0.0).powf(power);
        let facing = n.z.clamp(0.0, 1.0);
        let fresnel = |k: i32| (1.0 - facing).powi(k);
        let sky = n.y * 0.5 + 0.5;
        match self {
            Look::RedWax => {
                let soft = wrap(key, 0.55);
                // Light scattered under the surface warms and lifts the shadow side.
                let scatter = (soft - lambert(key)).max(0.0);
                let diffuse = Vec3::splat(0.05 + 0.88 * soft + 0.08 * lambert(fill))
                    + Vec3::new(0.36, 0.06, 0.03) * scatter;
                let spec = Vec3::splat(0.10 * blinn(key, 10.0) + 0.42 * blinn(key, 80.0) + 0.07 * fresnel(3));
                (diffuse, spec)
            }
            Look::SkinShade => {
                let soft = wrap(key, 0.45);
                let scatter = (soft - lambert(key)).max(0.0);
                let diffuse = Vec3::splat(0.07 + 0.86 * soft + 0.10 * lambert(fill) + 0.04 * sky)
                    + Vec3::new(0.42, 0.05, 0.0) * scatter;
                let spec = Vec3::splat(0.07 * blinn(key, 24.0)) + Vec3::new(0.05, 0.055, 0.06) * fresnel(4);
                (diffuse, spec)
            }
            Look::GrayClay => {
                let diffuse = Vec3::splat(0.07 + 0.10 * sky + 0.82 * lambert(key) + 0.12 * lambert(fill));
                (diffuse, Vec3::splat(0.05 * blinn(key, 30.0)))
            }
            Look::WhiteClay => {
                let diffuse = Vec3::splat((0.16 + 0.78 * wrap(key, 0.4) + 0.06 * sky + 0.06 * lambert(fill)) * (1.0 - 0.30 * fresnel(2)));
                (diffuse, Vec3::splat(0.04 * blinn(key, 16.0)))
            }
            Look::FormRead => {
                // Hard key, near-black shadow side, cool rim: every plane change reads.
                let diffuse = Vec3::splat(0.03 + 1.02 * lambert(key).powf(1.4) + 0.06 * lambert(fill));
                let spec = Vec3::splat(0.22 * blinn(key, 60.0)) + Vec3::new(0.26, 0.32, 0.40) * fresnel(4);
                (diffuse, spec)
            }
            Look::Chrome => {
                // A studio reflected in the mirror direction: bright sky, dark horizon band,
                // warm grey floor; softboxes go in the specular layer.
                let r = n * (2.0 * n.z) - Vec3::Z;
                let y = r.y;
                let smooth = |a: f32, b: f32, x: f32| {
                    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                };
                let upper = Vec3::new(0.50, 0.55, 0.62).lerp(Vec3::new(0.95, 0.97, 1.0), smooth(0.05, 0.9, y));
                let lower = Vec3::new(0.30, 0.27, 0.24).lerp(Vec3::new(0.14, 0.13, 0.12), smooth(-0.9, -0.1, y));
                let horizon = Vec3::splat(0.05);
                let env = if y >= 0.0 {
                    horizon.lerp(upper, smooth(0.0, 0.12, y))
                } else {
                    horizon.lerp(lower, smooth(0.0, -0.10, y))
                };
                let diffuse = env * 0.92;
                let boxes = softbox(r, Vec3::new(-0.55, 0.55, 0.63), 0.22, 0.10)
                    + softbox(r, Vec3::new(0.75, 0.20, 0.63), 0.10, 0.25) * 0.6;
                (diffuse, Vec3::splat(boxes))
            }
            Look::StudioGray => {
                // ZBrush's BasicMaterial look: warm key, cool fill, firm contrast.
                let diffuse = Vec3::new(1.0, 0.97, 0.92) * (0.04 + 0.92 * lambert(key).powf(1.1))
                    + Vec3::new(0.80, 0.88, 1.0) * (0.20 * lambert(fill) + 0.05 * sky);
                (diffuse, Vec3::splat(0.12 * blinn(key, 40.0) + 0.04 * fresnel(3)))
            }
            Look::CavityWhite => {
                // Darkens hard wherever the surface turns away from you: creases, pores and
                // edges read at once.
                let diffuse = Vec3::splat((0.32 + 0.68 * wrap(key, 0.3)) * (1.0 - 0.7 * fresnel(1).powf(1.6)));
                (diffuse, Vec3::splat(0.05 * blinn(key, 24.0)))
            }
            Look::Slate => {
                let diffuse = Vec3::splat(0.05 + 0.82 * lambert(key) + 0.12 * lambert(fill) + 0.04 * sky);
                let spec = Vec3::splat(0.55 * blinn(key, 90.0)) + Vec3::new(0.18, 0.22, 0.28) * fresnel(4);
                (diffuse, spec)
            }
            Look::RimDark => {
                // Silhouettes: a dark body, a bright rim and a small highlight.
                let diffuse = Vec3::splat(0.05 + 0.25 * lambert(key));
                let spec = Vec3::new(0.80, 0.86, 0.95) * fresnel(2).powf(1.2) + Vec3::splat(0.3 * blinn(key, 60.0));
                (diffuse, spec)
            }
            Look::Normal => {
                // The normal as colour (x red, y green, z blue), to check surface flow.
                let c = (n * 0.5 + Vec3::splat(0.5)).clamp(Vec3::ZERO, Vec3::ONE);
                (Vec3::new(c.x.powf(2.2), c.y.powf(2.2), c.z.powf(2.2)), Vec3::ZERO)
            }
            Look::ToyPlastic => {
                let diffuse = Vec3::splat(0.10 + 0.84 * lambert(key) + 0.16 * lambert(fill) + 0.06 * sky);
                let r = n * (2.0 * n.z) - Vec3::Z;
                let spec = 0.95 * blinn(key, 160.0)
                    + 0.18 * fresnel(3)
                    + 0.30 * softbox(r, Vec3::new(-0.55, 0.55, 0.63), 0.22, 0.10);
                (diffuse, Vec3::splat(spec))
            }
            // Loaded MatCaps are pictures, not painted (`sample` reads them).
            Look::User(_) => (Vec3::ONE, Vec3::ZERO),
        }
    }
}

/// A soft-edged rectangular light seen in reflection direction `r`: `half_w` / `half_h`
/// are its half extents across and along the vertical.
fn softbox(r: Vec3, centre: Vec3, half_w: f32, half_h: f32) -> f32 {
    let c = centre.normalize();
    if r.dot(c) <= 0.0 {
        return 0.0;
    }
    let right = Vec3::Y.cross(c).normalize();
    let up = c.cross(right);
    let d = r - c;
    let (x, y) = (d.dot(right).abs() / half_w, d.dot(up).abs() / half_h);
    let edge = |t: f32| (1.0 - ((t - 0.8) / 0.4).clamp(0.0, 1.0)).powi(2);
    edge(x) * edge(y)
}

/// One MatCap's images, linear RGB, row 0 at the top (view-space up).
pub struct Images {
    pub diffuse: Vec<Vec3>,
    pub specular: Vec<Vec3>,
}

/// The view-space normal shown at image coordinates `uv` (0..1, v down). Outside the
/// sphere it is the silhouette normal, so filtering at the rim stays on the sphere.
pub fn normal_at(uv: Vec2) -> Vec3 {
    let p = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let len2 = p.length_squared();
    if len2 >= 1.0 {
        let e = p / len2.sqrt();
        Vec3::new(e.x, e.y, 0.0)
    } else {
        Vec3::new(p.x, p.y, (1.0 - len2).sqrt())
    }
}

/// Where a view-space normal looks its colour up (the inverse of [`normal_at`]).
pub fn uv_of(n: Vec3) -> Vec2 {
    let mut p = Vec2::new(n.x, n.y);
    let len = p.length();
    if len > 1.0 {
        p /= len;
    }
    Vec2::new(0.5 + 0.5 * p.x, 0.5 - 0.5 * p.y)
}

fn paint(look: Look) -> Images {
    let (diffuse, specular) = (0..SIZE * SIZE)
        .into_par_iter()
        .map(|i| {
            let uv = Vec2::new(((i % SIZE) as f32 + 0.5) / SIZE as f32, ((i / SIZE) as f32 + 0.5) / SIZE as f32);
            look.light(normal_at(uv))
        })
        .unzip();
    Images { diffuse, specular }
}

/// Every MatCap, painted once.
pub fn library() -> &'static [Images] {
    static LIBRARY: OnceLock<Vec<Images>> = OnceLock::new();
    LIBRARY.get_or_init(|| Look::ALL.iter().map(|&l| paint(l)).collect())
}

/// The MatCap of material `material`, if it is one: a built-in one, or a loaded one after
/// them.
pub fn of_material(material: usize) -> Option<Look> {
    let i = material.checked_sub(FIRST)?;
    if let Some(look) = Look::ALL.get(i) {
        return Some(*look);
    }
    let k = i - Look::ALL.len();
    (k < user_count()).then_some(Look::User(k as u16))
}

/// Number of materials: the clays, the built-in MatCaps and the loaded ones.
pub fn material_count() -> usize {
    FIRST + Look::ALL.len() + user_count()
}

// --- Loaded MatCaps (Material › Load MatCap) ----------------------------------------------

/// A MatCap loaded from a picture: the picture resampled to [`SIZE`] as the diffuse layer.
pub struct UserMatcap {
    /// The picture's file name without its extension, as listed in the Material palette.
    pub name: String,
    /// The file it was loaded from (the copy in the config folder once stored), and the
    /// file the user picked, so picking it again selects it instead of loading it twice.
    pub path: PathBuf,
    pub source: PathBuf,
    pub diffuse: Vec<Vec3>,
}

fn users() -> &'static RwLock<Vec<UserMatcap>> {
    static USERS: OnceLock<RwLock<Vec<UserMatcap>>> = OnceLock::new();
    USERS.get_or_init(Default::default)
}

/// Counts up whenever the loaded MatCaps change, so the renderer knows to upload the
/// texture array again.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn user_count() -> usize {
    users().read().map_or(0, |u| u.len())
}

/// The name shown for material `material`: `MATERIALS` for the clays and built-in MatCaps,
/// the file name for a loaded one.
pub fn material_name(material: usize) -> String {
    let table = &crate::workspace::MATERIALS;
    if let Some((_, name)) = table.get(material) {
        return (*name).to_string();
    }
    users().read().ok().and_then(|u| u.get(material - table.len()).map(|m| m.name.clone())).unwrap_or_default()
}

/// The material called `name` (case does not matter), or its number.
pub fn material_named(name: &str) -> Option<usize> {
    if let Some(i) = name.parse::<usize>().ok().filter(|&i| i < material_count()) {
        return Some(i);
    }
    (0..material_count()).find(|&i| material_name(i).eq_ignore_ascii_case(name))
}

/// The picture at `path` as a MatCap: decoded, taken as sRGB and resampled to [`SIZE`]
/// (bilinear), its alpha ignored.
fn picture_matcap(path: &Path) -> Result<UserMatcap, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("{}: an empty picture", path.display()));
    }
    let px = img.as_raw();
    let linear = |c: u8| (c as f32 / 255.0).powf(2.2);
    let at = |x: i64, y: i64| {
        let x = x.clamp(0, w as i64 - 1) as usize;
        let y = y.clamp(0, h as i64 - 1) as usize;
        let i = (y * w as usize + x) * 4;
        Vec3::new(linear(px[i]), linear(px[i + 1]), linear(px[i + 2]))
    };
    let diffuse = (0..SIZE * SIZE)
        .into_par_iter()
        .map(|i| {
            let u = ((i % SIZE) as f32 + 0.5) / SIZE as f32;
            let v = ((i / SIZE) as f32 + 0.5) / SIZE as f32;
            let (x, y) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let top = at(x0, y0).lerp(at(x0 + 1, y0), fx);
            let bottom = at(x0, y0 + 1).lerp(at(x0 + 1, y0 + 1), fx);
            top.lerp(bottom, fy)
        })
        .collect();
    let name = path.file_stem().map_or_else(|| "MatCap".to_string(), |s| s.to_string_lossy().into_owned());
    Ok(UserMatcap { name, path: path.to_path_buf(), source: path.to_path_buf(), diffuse })
}

/// Load the picture at `path` as a new MatCap after the others and return its material
/// index. With `store` (the config root, `$XDG_CONFIG_HOME`), the picture is copied into
/// `klay/matcaps/` and listed, so it is loaded again at the next start. A picture already
/// loaded (same path) is selected, not loaded twice.
pub fn load_user(path: &Path, store: Option<&Path>) -> Result<usize, String> {
    if let Some(k) = users().read().ok().and_then(|u| u.iter().position(|m| m.path == path || m.source == path)) {
        return Ok(FIRST + Look::ALL.len() + k);
    }
    let mut m = picture_matcap(path)?;
    if let Some(root) = store {
        m.path = store_copy(root, path)?;
    }
    let mut u = users().write().map_err(|_| "the MatCap list is poisoned")?;
    u.push(m);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let k = u.len() - 1;
    drop(u);
    if let Some(root) = store {
        write_list(root);
    }
    Ok(FIRST + Look::ALL.len() + k)
}

/// Take loaded MatCap `k` away (its copy in the config folder too, when `store` is given).
pub fn remove_user(k: usize, store: Option<&Path>) {
    let removed = users().write().ok().filter(|u| k < u.len()).map(|mut u| u.remove(k));
    let Some(removed) = removed else { return };
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if let Some(root) = store {
        if removed.path.starts_with(folder(root)) {
            let _ = std::fs::remove_file(&removed.path);
        }
        write_list(root);
    }
}

/// `klay/matcaps/` under the config root.
fn folder(root: &Path) -> PathBuf {
    root.join("klay/matcaps")
}

/// Copy `path` into the config folder under its own name (a number added when another
/// picture already has it) and return the copy's path.
fn store_copy(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let dir = folder(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let stem = path.file_stem().map_or_else(|| "matcap".to_string(), |s| s.to_string_lossy().into_owned());
    let ext = path.extension().map_or_else(String::new, |e| format!(".{}", e.to_string_lossy()));
    let candidates = std::iter::once(format!("{stem}{ext}")).chain((2..).map(|n| format!("{stem} {n}{ext}")));
    let target = candidates.map(|n| dir.join(n)).find(|p| !p.exists()).unwrap();
    std::fs::copy(path, &target).map_err(|e| format!("cannot copy {} into the config folder: {e}", path.display()))?;
    Ok(target)
}

/// Write `klay/matcaps.list`: the stored copies' file names, in material order.
fn write_list(root: &Path) {
    let Ok(u) = users().read() else { return };
    let mut text = String::from("# Klay matcaps v1\n");
    for m in u.iter() {
        if let Some(name) = m.path.file_name().filter(|_| m.path.starts_with(folder(root))) {
            text.push_str(&name.to_string_lossy());
            text.push('\n');
        }
    }
    let path = root.join("klay/matcaps.list");
    let _ = std::fs::create_dir_all(root.join("klay"));
    let _ = std::fs::write(path, text);
}

/// Load the MatCaps stored under the config root `root` again, in their listed order (any
/// picture in the folder that the list misses comes after, by name). Returns what could
/// not be loaded.
pub fn restore(root: &Path) -> Vec<String> {
    let dir = folder(root);
    let listed: Vec<String> = std::fs::read_to_string(root.join("klay/matcaps.list"))
        .ok()
        .filter(|t| t.starts_with("# Klay matcaps v1\n"))
        .map(|t| t.lines().skip(1).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let mut names = listed.clone();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut extra: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !listed.contains(n))
            .collect();
        extra.sort();
        names.extend(extra);
    }
    let mut failed = Vec::new();
    for n in names {
        let path = dir.join(&n);
        if !path.is_file() {
            continue;
        }
        if let Err(e) = load_user(&path, None) {
            failed.push(e);
        }
    }
    failed
}

/// Tests that load or count MatCaps take this, since the loaded MatCaps are shared.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Loaded MatCap `k`'s picture, bilinear at a view-space normal.
fn user_sample(k: usize, n: Vec3) -> Vec3 {
    users().read().ok().and_then(|u| u.get(k).map(|m| bilinear(&m.diffuse, n))).unwrap_or(Vec3::ONE)
}

fn encode(c: f32) -> u8 {
    (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8
}

/// The images as sRGB RGBA8 texture array layers: diffuse then specular per MatCap, in
/// [`Look::ALL`] order, then the loaded MatCaps (their picture, then a black specular
/// layer). The renderer samples them as `Rgba8UnormSrgb` (linear in the shader).
pub fn gpu_layers() -> Vec<u8> {
    let mut out = Vec::with_capacity(layer_count() as usize * SIZE * SIZE * 4);
    let push = |out: &mut Vec<u8>, layer: &[Vec3]| out.extend(layer.iter().flat_map(|c| [encode(c.x), encode(c.y), encode(c.z), 255]));
    for m in library() {
        push(&mut out, &m.diffuse);
        push(&mut out, &m.specular);
    }
    if let Ok(u) = users().read() {
        for m in u.iter() {
            push(&mut out, &m.diffuse);
            out.extend(std::iter::repeat_n([0, 0, 0, 255], SIZE * SIZE).flatten());
        }
    }
    out
}

/// Number of texture array layers in [`gpu_layers`].
pub fn layer_count() -> u32 {
    ((Look::ALL.len() + user_count()) * 2) as u32
}

/// `img` (a [`SIZE`]-square image) bilinear at the view-space normal `n`.
fn bilinear(img: &[Vec3], n: Vec3) -> Vec3 {
    let uv = uv_of(n) * SIZE as f32 - Vec2::splat(0.5);
    let (x0, y0) = (uv.x.floor(), uv.y.floor());
    let (fx, fy) = (uv.x - x0, uv.y - y0);
    let at = |x: f32, y: f32| {
        let x = (x as i32).clamp(0, SIZE as i32 - 1) as usize;
        let y = (y as i32).clamp(0, SIZE as i32 - 1) as usize;
        img[y * SIZE + x]
    };
    let top = at(x0, y0).lerp(at(x0 + 1.0, y0), fx);
    let bottom = at(x0, y0 + 1.0).lerp(at(x0 + 1.0, y0 + 1.0), fx);
    top.lerp(bottom, fy)
}

/// Bilinear diffuse and specular light of `look` for a view-space normal (thumbnails).
pub fn sample(look: Look, n: Vec3) -> (Vec3, Vec3) {
    if let Look::User(k) = look {
        return (user_sample(k as usize, n), Vec3::ZERO);
    }
    let m = &library()[Look::ALL.iter().position(|&l| l == look).unwrap()];
    (bilinear(&m.diffuse, n), bilinear(&m.specular, n))
}

/// The shaded colour of `base` under `look` for a view-space normal, linear.
pub fn shade(look: Look, base: Vec3, n: Vec3) -> Vec3 {
    let (d, s) = sample(look, n);
    base * d + s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uv_and_normal_are_inverse_and_up_is_the_top_of_the_image() {
        for uv in [Vec2::new(0.5, 0.5), Vec2::new(0.3, 0.2), Vec2::new(0.8, 0.6)] {
            assert!(uv_of(normal_at(uv)).distance(uv) < 1e-5);
        }
        // Facing the viewer is the centre; up is the top row; right is the right column.
        assert!(uv_of(Vec3::Z).distance(Vec2::splat(0.5)) < 1e-6);
        assert!(uv_of(Vec3::Y).y < 0.01 && uv_of(Vec3::X).x > 0.99);
        // Normals turned away (interpolated past the silhouette) stay on the rim.
        assert!(uv_of(Vec3::new(3.0, 0.0, -1.0)).x <= 1.0);
    }

    #[test]
    fn every_matcap_is_lit_from_the_upper_left_and_distinct() {
        let lib = library();
        assert_eq!(lib.len(), Look::ALL.len());
        let mut looks = Vec::new();
        for (m, &look) in lib.iter().zip(&Look::ALL) {
            assert_eq!(m.diffuse.len(), SIZE * SIZE);
            assert!(m.diffuse.iter().chain(&m.specular).all(|c| c.is_finite() && c.min_element() >= 0.0));
            let luma = |n: Vec3| shade(look, look.base(), n.normalize()).dot(Vec3::splat(1.0 / 3.0));
            // The key light is upper left (Chrome reflects a bright sky above instead); Normal
            // shows directions, not light.
            if look != Look::Normal {
                assert!(luma(Vec3::new(-0.5, 0.5, 0.7)) > luma(Vec3::new(0.5, -0.5, 0.7)), "{:?}", look);
            }
            let probe: Vec<Vec3> = [Vec3::Z, Vec3::new(0.6, 0.3, 0.74), Vec3::new(-0.2, -0.8, 0.56)]
                .iter()
                .map(|&n| shade(look, Vec3::ONE, n.normalize()))
                .collect();
            assert!(looks.iter().all(|p: &Vec<Vec3>| p != &probe), "{:?} duplicates another MatCap", look);
            looks.push(probe);
        }
    }

    #[test]
    fn the_surface_colour_tints_the_diffuse_layer_only() {
        // Polypaint: a different base changes the colour, the highlight stays white.
        // The highlight sits halfway between the key light and the viewer.
        let n = (Vec3::new(-0.52, 0.62, 0.59).normalize() + Vec3::Z).normalize();
        let (d, s) = sample(Look::ToyPlastic, n);
        assert!(s.min_element() > 0.3, "a white highlight: {s:?}");
        let red = shade(Look::ToyPlastic, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.3, -0.3, 0.9).normalize());
        assert!(red.x > red.y * 3.0 && red.x > red.z * 3.0, "{red:?}");
        assert!((shade(Look::ToyPlastic, Vec3::ZERO, n) - s).length() < 1e-6);
        assert!(d.length() > 0.5);
    }

    #[test]
    fn gpu_layers_hold_every_image_in_srgb() {
        let _lock = test_lock();
        let bytes = gpu_layers();
        assert_eq!(bytes.len(), layer_count() as usize * SIZE * SIZE * 4);
        // The centre texel of Gray's diffuse layer encodes its linear value.
        let i = (SIZE / 2 * SIZE + SIZE / 2) * 4 + 2 * 2 * SIZE * SIZE * 4;
        let c = library()[2].diffuse[SIZE / 2 * SIZE + SIZE / 2].x;
        assert_eq!(bytes[i], encode(c));
        assert_eq!(of_material(0), None);
        assert_eq!(of_material(FIRST), Some(Look::RedWax));
        assert_eq!(of_material(FIRST + Look::ALL.len() + user_count()), None);
    }

    #[test]
    fn a_loaded_matcap_is_listed_after_the_built_in_ones_and_comes_back_from_the_config_folder() {
        let _lock = test_lock();
        let root = std::env::temp_dir().join(format!("klay-matcap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pictures")).unwrap();
        // Left half red, right half blue: a MatCap that colours by which way the surface faces.
        let img = image::RgbaImage::from_fn(8, 8, |x, _| if x < 4 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) });
        let path = root.join("pictures/sunset.png");
        img.save(&path).unwrap();
        let before = material_count();
        let i = load_user(&path, Some(&root)).expect("loads");
        assert_eq!((i, material_count()), (before, before + 1));
        assert_eq!(material_name(i), "sunset");
        let k = user_count() - 1;
        assert_eq!(of_material(i), Some(Look::User(k as u16)));
        assert_eq!(material_named("SUNSET"), Some(i));
        assert_eq!(material_named(&i.to_string()), Some(i));
        assert_eq!(material_named("MatCap Red Wax"), Some(FIRST));
        assert_eq!(material_named("no such"), None);
        // Facing right reads the blue half, left the red half; polypaint tints the picture.
        let look = Look::User(k as u16);
        let right = shade(look, look.base(), Vec3::new(0.8, 0.0, 0.6).normalize());
        let left = shade(look, look.base(), Vec3::new(-0.8, 0.0, 0.6).normalize());
        assert!(right.z > 0.9 && right.x < 0.05 && left.x > 0.9 && left.z < 0.05, "{right:?} {left:?}");
        assert!(shade(look, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.8, 0.0, 0.6)).length() < 0.05, "red paint on blue");
        // The texture array carries it: the last diffuse layer's right side is blue, its
        // specular layer black.
        let bytes = gpu_layers();
        assert_eq!(bytes.len(), layer_count() as usize * SIZE * SIZE * 4);
        let layer = (FIRST + Look::ALL.len() + k - FIRST) * 2;
        let at = |l: usize, x: usize| {
            let o = (l * SIZE * SIZE + SIZE / 2 * SIZE + x) * 4;
            [bytes[o], bytes[o + 1], bytes[o + 2]]
        };
        assert_eq!(at(layer, SIZE - 10), [0, 0, 255]);
        assert_eq!(at(layer, 10), [255, 0, 0]);
        assert_eq!(at(layer + 1, 10), [0, 0, 0]);
        // Loading the same picture again selects it instead of adding it.
        assert_eq!(load_user(&path, Some(&root)).unwrap(), i);
        assert_eq!(material_count(), before + 1);
        // The picture was copied into the config folder and listed.
        let copy = root.join("klay/matcaps/sunset.png");
        assert!(copy.is_file());
        assert_eq!(std::fs::read_to_string(root.join("klay/matcaps.list")).unwrap(), "# Klay matcaps v1\nsunset.png\n");
        // The next start loads it again from there, in the same place in the list.
        let g = generation();
        remove_user(k, None);
        assert_eq!((material_count(), generation()), (before, g + 1));
        assert!(restore(&root).is_empty());
        assert_eq!(material_count(), before + 1);
        assert_eq!(material_name(before), "sunset");
        assert!(users().read().unwrap()[k].path == copy);
        // Removed for good: the copy and its line go, and nothing comes back.
        remove_user(k, Some(&root));
        assert!(!copy.exists() && material_count() == before);
        assert!(restore(&root).is_empty() && material_count() == before);
        // Nothing to load gives a message, not a MatCap.
        assert!(load_user(&root.join("none.png"), None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod material_table_tests {
    use super::*;

    #[test]
    fn the_material_palette_lists_every_matcap_in_order() {
        let table = &crate::workspace::MATERIALS;
        assert_eq!(table.len(), FIRST + Look::ALL.len());
        for (i, look) in Look::ALL.iter().enumerate() {
            let (swatch, name) = table[FIRST + i];
            assert_eq!(name, look.name());
            // The swatch is the unpainted colour in sRGB.
            let b = look.base();
            for (c, l) in [swatch.r(), swatch.g(), swatch.b()].into_iter().zip([b.x, b.y, b.z]) {
                assert!((c as i32 - encode(l) as i32).abs() <= 1, "{name}");
            }
        }
    }

    #[test]
    fn the_normal_matcap_shows_directions_as_colour() {
        let right = shade(Look::Normal, Look::Normal.base(), Vec3::X);
        let up = shade(Look::Normal, Look::Normal.base(), Vec3::Y);
        assert!(right.x > 0.9 && right.y < 0.3 && up.y > 0.9 && up.x < 0.3, "{right:?} {up:?}");
    }
}
