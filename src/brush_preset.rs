//! Portable Klay brush presets. Imported stamps are stored as the exact pixels the brush uses.
use crate::{
    brush_asset::{BrushImage, Pattern},
    pressure::PressureCurve,
    primitive::Primitive,
    project::{In, Out},
    sculpt::{AlphaTurn, AutoMask, Brush, BrushKind, StampSettings},
    workspace::{TabletPressure, Workspace},
    zmodeler::{EdgeAction, PointAction, PolyAction, PolyTarget},
};
use glam::Vec2;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const EXTENSION: &str = "klaybrush";
const MAGIC: &[u8; 8] = b"KLAYBRSH";
/// 2 added Wrap after Auto Masking; version 1 presets load with the default Wrap.
const VERSION: u32 = 2;
const MAX_FILE_BYTES: u64 = 10 << 20;

pub struct Preset {
    pub brush: Brush,
    pub alternate: [u8; 3],
    pub subtract: bool,
    pub dynamic: bool,
    pub assets: StampSettings,
    pub pressure: TabletPressure,
    pub stroke: &'static str,
    pub spacing: f32,
    pub lazy_mouse: bool,
    pub lazy_radius: f32,
    pub curve_smooth: f32,
    pub curve_step: f32,
    pub tube_taper: f32,
    pub tube_sides: u32,
    pub sculptris: bool,
    pub sculptris_detail: f32,
    pub insert_shape: Primitive,
    pub poly_action: PolyAction,
    pub poly_target: PolyTarget,
    pub edge_action: EdgeAction,
    pub edge_loop: bool,
    pub point_action: PointAction,
}

pub fn path_for(folder: &Path, name: &str) -> io::Result<PathBuf> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 180
        || name.starts_with('.')
        || name.ends_with('.')
        || name
            .chars()
            .any(|c| c.is_control() || "/\\<>:\"|?*".contains(c))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter a valid brush name",
        ));
    }
    Ok(folder.join(format!("{name}.{EXTENSION}")))
}

/// Write a complete preset before publishing it, and preserve existing presets unless replacing.
pub fn save(path: &Path, preset: &Preset, replace: bool) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let folder = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(folder)?;
    let temp = folder.join(format!(
        ".brush-{}-{}.saving",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        let mut file = BufWriter::new(file);
        write(&mut file, preset)?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        if replace {
            fs::rename(&temp, path)
        } else {
            match fs::hard_link(&temp, path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
                // Some user asset drives do not support hard links.
                Err(_) => {
                    let mut source = File::open(&temp)?;
                    let mut target = OpenOptions::new().write(true).create_new(true).open(path)?;
                    let result = io::copy(&mut source, &mut target).and_then(|_| target.sync_all());
                    drop(target);
                    if result.is_err() {
                        let _ = fs::remove_file(path);
                    }
                    result
                }
            }
        }
    })();
    let _ = fs::remove_file(temp);
    result
}

fn write<W: Write>(w: W, p: &Preset) -> io::Result<()> {
    let mut out = Out::new(w);
    out.bytes(MAGIC)?;
    out.u32(VERSION)?;
    out.bytes(p.brush.kind.code().as_bytes())?;
    out.u8(p.subtract as u8)?;
    for value in [
        p.brush.radius,
        p.brush.strength,
        p.brush.focal_shift,
        p.brush.rgb_intensity,
    ] {
        out.u32(value.to_bits())?;
    }
    out.bytes(&p.brush.color)?;
    out.bytes(&p.alternate)?;
    out.bytes(&[p.brush.paint as u8, p.brush.sculpt as u8, p.dynamic as u8])?;
    out.slice(p.stroke.as_bytes())?;
    for value in [
        p.spacing,
        p.lazy_radius,
        p.curve_smooth,
        p.curve_step,
        p.tube_taper,
        p.sculptris_detail,
    ] {
        out.u32(value.to_bits())?;
    }
    out.u32(p.tube_sides)?;
    out.bytes(&[
        p.lazy_mouse as u8,
        p.sculptris as u8,
        p.pressure.size as u8,
        p.pressure.intensity as u8,
        p.edge_loop as u8,
    ])?;
    out.slice(&p.pressure.size_curve.points)?;
    out.slice(&p.pressure.intensity_curve.points)?;
    for value in [
        p.insert_shape.short(),
        p.poly_action.name(),
        p.poly_target.name(),
        p.edge_action.name(),
        p.point_action.name(),
        p.assets.turn.name(),
    ] {
        out.slice(value.as_bytes())?;
    }
    for value in [
        p.assets.angle,
        p.assets.texture_strength,
        p.assets.texture_scale,
        p.assets.texture_angle,
    ] {
        out.u32(value.to_bits())?;
    }
    out.bytes(&[
        p.assets.auto.backface as u8,
        p.assets.auto.topological as u8,
    ])?;
    out.u32(p.assets.wrap.to_bits())?;
    write_pattern(&mut out, &p.assets.alpha)?;
    write_pattern(&mut out, &p.assets.texture)
}

fn write_pattern<W: Write>(out: &mut Out<W>, pattern: &Pattern) -> io::Result<()> {
    if let Pattern::Image(image) = pattern {
        out.u8(1)?;
        out.slice(image.name.as_bytes())?;
        out.u32(image.width as u32)?;
        out.u32(image.height as u32)?;
        out.slice(&image.rgba)
    } else {
        out.u8(0)?;
        out.u64(pattern.key())
    }
}

fn input(path: &Path) -> Result<In<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err("Brush preset is too large".into());
    }
    Ok(In::new(BufReader::new(file), size))
}

fn header<R: Read>(input: &mut In<R>) -> Result<(BrushKind, bool, u32), String> {
    let mut magic = [0; MAGIC.len()];
    input.fill_raw(&mut magic)?;
    if &magic != MAGIC {
        return Err("Invalid Klay brush preset".into());
    }
    let version = input.u32()?;
    if !(1..=VERSION).contains(&version) {
        return Err("Unsupported Klay brush preset version".into());
    }
    let a = input.u8()? as char;
    let b = input.u8()? as char;
    let kind = BrushKind::from_code(a, b).ok_or("Unknown base brush")?;
    Ok((kind, boolean(input)?, version))
}

pub fn preview(path: &Path) -> Option<(BrushKind, bool)> {
    header(&mut input(path).ok()?).ok().map(|(kind, subtract, _)| (kind, subtract))
}

pub fn load(path: &Path) -> Result<Preset, String> {
    let mut input = input(path)?;
    let (kind, subtract, version) = header(&mut input)?;
    let mut brush = Brush {
        kind,
        radius: float(&mut input, 0.01, 2.0)?,
        strength: float(&mut input, 0.0, 1.0)?,
        focal_shift: float(&mut input, -100.0, 100.0)?,
        rgb_intensity: float(&mut input, 0.0, 1.0)?,
        ..Brush::default()
    };
    input.fill_raw(&mut brush.color)?;
    let mut alternate = [0; 3];
    input.fill_raw(&mut alternate)?;
    brush.paint = boolean(&mut input)?;
    brush.sculpt = boolean(&mut input)?;
    let dynamic = boolean(&mut input)?;
    let stroke = choice(&mut input, &Workspace::stroke_modes(kind), |mode| mode)?;
    let spacing = float(&mut input, 0.03, 1.0)?;
    let lazy_radius = float(&mut input, 5.0, 300.0)?;
    let curve_smooth = float(&mut input, 0.0, 2.0)?;
    let curve_step = float(&mut input, 0.16, 1.0)?;
    let tube_taper = float(&mut input, 0.0, 1.0)?;
    let sculptris_detail = float(&mut input, 0.0, 100.0)?;
    let tube_sides = input.u32()?;
    if !(crate::curve_tube::Tube::MIN_SIDES..=crate::curve_tube::Tube::MAX_SIDES)
        .contains(&tube_sides)
    {
        return Err("Invalid tube sides".into());
    }
    let lazy_mouse = boolean(&mut input)?;
    let sculptris = boolean(&mut input)?;
    let size = boolean(&mut input)?;
    let intensity = boolean(&mut input)?;
    let edge_loop = boolean(&mut input)?;
    let pressure = TabletPressure {
        size,
        intensity,
        size_curve: curve(&mut input)?,
        intensity_curve: curve(&mut input)?,
    };
    let insert_shape = choice(&mut input, &Primitive::ALL, Primitive::short)?;
    let poly_action = choice(&mut input, &PolyAction::ALL, PolyAction::name)?;
    let poly_target = choice(&mut input, &PolyTarget::ALL, PolyTarget::name)?;
    let edge_action = choice(&mut input, &EdgeAction::ALL, EdgeAction::name)?;
    let point_action = choice(&mut input, &PointAction::ALL, PointAction::name)?;
    let turn = choice(&mut input, &AlphaTurn::ALL, AlphaTurn::name)?;
    let angle = float(&mut input, -180.0, 180.0)?;
    let texture_strength = float(&mut input, 0.0, 1.0)?;
    let texture_scale = float(&mut input, 0.25, 16.0)?;
    let texture_angle = float(&mut input, -180.0, 180.0)?;
    let auto = AutoMask {
        backface: boolean(&mut input)?,
        topological: boolean(&mut input)?,
    };
    let wrap = if version >= 2 { float(&mut input, 0.0, 1.0)? } else { StampSettings::default().wrap };
    let assets = StampSettings {
        turn,
        angle,
        texture_strength,
        texture_scale,
        texture_angle,
        wrap,
        auto,
        alpha: read_pattern(&mut input, Pattern::alphas())?,
        texture: read_pattern(&mut input, Pattern::textures())?,
        spotlight: None,
    };
    if input.left() != 0 {
        return Err("Unexpected data after the brush preset".into());
    }
    Ok(Preset {
        brush,
        alternate,
        subtract,
        dynamic,
        assets,
        pressure,
        stroke,
        spacing,
        lazy_mouse,
        lazy_radius,
        curve_smooth,
        curve_step,
        tube_taper,
        tube_sides,
        sculptris,
        sculptris_detail,
        insert_shape,
        poly_action,
        poly_target,
        edge_action,
        edge_loop,
        point_action,
    })
}

fn read_pattern<R: Read>(input: &mut In<R>, builtins: Vec<Pattern>) -> Result<Pattern, String> {
    if !boolean(input)? {
        let key = input.u64()?;
        return builtins
            .into_iter()
            .find(|p| p.key() == key)
            .ok_or_else(|| "Unknown built-in stamp".into());
    }
    let name = text(input)?;
    let width = input.u32()? as usize;
    let height = input.u32()? as usize;
    if !(1..=1024).contains(&width) || !(1..=1024).contains(&height) {
        return Err("Invalid stamp dimensions".into());
    }
    let rgba = input.vec()?;
    if rgba.len() != width * height {
        return Err("Stamp pixels do not match its dimensions".into());
    }
    Ok(Pattern::Image(BrushImage::from_pixels(
        name, None, width, height, rgba,
    )))
}

fn text<R: Read>(input: &mut In<R>) -> Result<String, String> {
    let bytes: Vec<u8> = input.vec()?;
    if bytes.len() > 4096 {
        return Err("Brush preset label is too long".into());
    }
    String::from_utf8(bytes).map_err(|_| "Invalid text in brush preset".into())
}

fn choice<R: Read, T: Copy>(
    input: &mut In<R>,
    items: &[T],
    name: impl Fn(T) -> &'static str,
) -> Result<T, String> {
    let text = text(input)?;
    items
        .iter()
        .copied()
        .find(|&item| name(item) == text)
        .ok_or_else(|| format!("Unknown brush option: {text}"))
}

fn boolean<R: Read>(input: &mut In<R>) -> Result<bool, String> {
    match input.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("Invalid toggle in brush preset".into()),
    }
}

fn float<R: Read>(input: &mut In<R>, min: f32, max: f32) -> Result<f32, String> {
    let value = f32::from_bits(input.u32()?);
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err("A brush setting is out of range".into())
    }
}

fn curve<R: Read>(input: &mut In<R>) -> Result<PressureCurve, String> {
    let points: Vec<Vec2> = input.vec()?;
    if !(2..=64).contains(&points.len())
        || points.first().unwrap().x != 0.0
        || points.last().unwrap().x != 1.0
        || points.windows(2).any(|w| w[0].x >= w[1].x)
        || points
            .iter()
            .any(|p| !p.is_finite() || !(0.0..=1.0).contains(&p.y))
    {
        return Err("Invalid tablet pressure curve".into());
    }
    Ok(PressureCurve { points })
}
