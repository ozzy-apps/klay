//! Dispatch mesh files from Document Import and LightBox into the shared sculpt mesh.
mod fbx;
mod gltf;
mod goz;

use crate::{
    obj::{self, Imported},
    stl,
};
use glam::{DVec3, Vec3};
use std::path::Path;

pub const EXTENSIONS: &[&str] = &["obj", "stl", "glb", "gltf", "fbx", "goz"];

pub struct Part {
    pub name: String,
    pub mesh: Imported,
}

pub fn accepts_extension(ext: &std::ffi::OsStr) -> bool {
    EXTENSIONS.iter().any(|kind| ext.eq_ignore_ascii_case(kind))
}

pub fn read(path: &Path, stl_z_up: bool) -> Result<Vec<Part>, String> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = path.file_stem().map_or_else(
        || "Imported mesh".into(),
        |s| s.to_string_lossy().into_owned(),
    );
    let parts = match ext.as_str() {
        "ztl" | "zpr" => return Err(
            "Open this file in ZBrush and export FBX, OBJ or GoZ, then import the export into Klay"
                .into(),
        ),
        "obj" => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            vec![Part {
                name,
                mesh: obj::parse(&text)?,
            }]
        }
        "stl" => vec![Part {
            name,
            mesh: stl::parse_oriented(&read_bytes(path)?, stl_z_up)?,
        }],
        "glb" | "gltf" => gltf::read(path, &name)?,
        "fbx" => fbx::parse(&read_bytes(path)?, &name)?,
        "goz" => vec![goz::parse(&read_bytes(path)?, name)?],
        _ => {
            return Err(format!(
                "Unsupported model format. Choose {}",
                EXTENSIONS.join(", ")
            ));
        }
    };
    if parts.is_empty() {
        return Err("The file has no polygon meshes".into());
    }
    Ok(parts)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))
}

/// Fit a whole assembly together so independently editable SubTools keep their placement.
pub fn fit(parts: &mut [Part]) -> f64 {
    let (lo, hi) = parts
        .iter()
        .flat_map(|part| &part.mesh.positions)
        .fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.as_dvec3()), hi.max(p.as_dvec3()))
        });
    let centre = (lo + hi) * 0.5;
    let scale = 2.0 / (hi - lo).max_element().max(1e-12);
    for p in parts.iter_mut().flat_map(|part| &mut part.mesh.positions) {
        *p = ((p.as_dvec3() - centre) * scale).as_vec3();
    }
    scale
}

fn checked_position(p: Vec3) -> Result<Vec3, String> {
    p.is_finite()
        .then_some(p)
        .ok_or_else(|| "Invalid vertex position".into())
}
