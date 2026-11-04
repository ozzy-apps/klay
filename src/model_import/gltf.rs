use super::{Part, checked_position, read_bytes};
use crate::obj::Imported;
use base64::{Engine, engine::general_purpose::STANDARD};
use glam::{Mat4, Vec2, Vec3};
use gltf::{
    Semantic,
    accessor::{DataType, Dimensions},
    mesh::Mode,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path},
};

pub(super) fn read(path: &Path, name: &str) -> Result<Vec<Part>, String> {
    let bytes = read_bytes(path)?;
    let gltf::Gltf { document, mut blob } =
        gltf::Gltf::from_slice(&bytes).map_err(|e| format!("glTF: {e}"))?;
    if document.as_json().asset.version != "2.0" {
        return Err("Klay imports glTF 2.0 files".into());
    }
    let buffers = document
        .buffers()
        .map(|buffer| {
            let mut data = match buffer.source() {
                gltf::buffer::Source::Bin => blob.take().ok_or("Missing glTF binary chunk")?,
                gltf::buffer::Source::Uri(uri) => buffer_uri(path, uri)?,
            };
            if data.len() < buffer.length() {
                return Err(format!("glTF buffer {} is truncated", buffer.index()));
            }
            data.truncate(buffer.length());
            Ok(data)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for view in document.views() {
        let len = buffers[view.buffer().index()].len();
        if view.offset() > len || view.length() > len - view.offset() {
            return Err("A glTF buffer view is out of range".into());
        }
    }
    let mut parts = Vec::new();
    if let Some(scene) = document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        let mut stack: Vec<_> = scene.nodes().map(|node| (node, Mat4::IDENTITY)).collect();
        stack.reverse();
        let mut visited = HashSet::new();
        while let Some((node, parent)) = stack.pop() {
            if !visited.insert(node.index()) {
                return Err("The glTF scene repeats a node in its hierarchy".into());
            }
            let matrix = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
            if let Some(mesh) = node.mesh() {
                let label = node.name().or(mesh.name()).unwrap_or(name);
                if let Some(part) = mesh_part(mesh, matrix, &buffers, label)? {
                    parts.push(part);
                }
            }
            let children: Vec<_> = node.children().collect();
            stack.extend(children.into_iter().rev().map(|child| (child, matrix)));
        }
    } else {
        for mesh in document.meshes() {
            let label = mesh.name().unwrap_or(name).to_owned();
            if let Some(part) = mesh_part(mesh, Mat4::IDENTITY, &buffers, &label)? {
                parts.push(part);
            }
        }
    }
    Ok(parts)
}

fn buffer_uri(model: &Path, uri: &str) -> Result<Vec<u8>, String> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (media_type, content) = data.split_once(',').ok_or("Invalid glTF data URI")?;
        let content = percent_encoding::percent_decode_str(content).collect::<Vec<_>>();
        return if media_type.ends_with(";base64") {
            STANDARD
                .decode(content)
                .map_err(|e| format!("Invalid glTF base64 buffer: {e}"))
        } else {
            Ok(content)
        };
    }
    let decoded = percent_encoding::percent_decode_str(uri)
        .decode_utf8()
        .map_err(|_| "Invalid glTF buffer path")?;
    let relative = Path::new(decoded.as_ref());
    if decoded.contains([':', '\\'])
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("Keep glTF buffers in the model folder or a subfolder".into());
    }
    let dir = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("Cannot open glTF model folder: {e}"))?;
    let path = dir
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("Cannot find glTF buffer {uri}: {e}"))?;
    if !path.starts_with(&dir) || !path.is_file() {
        return Err("Keep glTF buffers in the model folder or a subfolder".into());
    }
    read_bytes(&path)
}

fn mesh_part(
    mesh: gltf::Mesh<'_>,
    matrix: Mat4,
    buffers: &[Vec<u8>],
    name: &str,
) -> Result<Option<Part>, String> {
    let mut imported = Imported::default();
    let mut blocks = HashMap::new();
    let with_uvs = mesh
        .primitives()
        .any(|p| p.get(&Semantic::TexCoords(0)).is_some());
    let mirrored = matrix.determinant() < 0.0;
    for primitive in mesh.primitives() {
        let mode = primitive.mode();
        if !matches!(
            mode,
            Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan
        ) {
            continue;
        }
        let positions = primitive
            .get(&Semantic::Positions)
            .ok_or("Missing glTF vertex positions")?;
        if positions.dimensions() != Dimensions::Vec3 || positions.data_type() != DataType::F32 {
            return Err("glTF POSITION must contain floating point triples".into());
        }
        let color = primitive.get(&Semantic::Colors(0));
        let uv = primitive.get(&Semantic::TexCoords(0));
        let index = primitive.indices();
        if index.as_ref().is_some_and(|a| {
            a.dimensions() != Dimensions::Scalar
                || !matches!(a.data_type(), DataType::U8 | DataType::U16 | DataType::U32)
        }) || uv.as_ref().is_some_and(|a| {
            a.dimensions() != Dimensions::Vec2
                || !matches!(a.data_type(), DataType::U8 | DataType::U16 | DataType::F32)
        }) || color.as_ref().is_some_and(|a| {
            !matches!(a.dimensions(), Dimensions::Vec3 | Dimensions::Vec4)
                || !matches!(a.data_type(), DataType::U8 | DataType::U16 | DataType::F32)
        }) {
            return Err("Invalid glTF index, UV or color accessor type".into());
        }
        let count = positions.count();
        let key = (positions.index(), color.as_ref().map(|a| a.index()));
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
        // A sparse accessor can otherwise claim billions of implicit zero vertices.
        for accessor in [Some(positions), color.clone(), uv.clone(), index.clone()]
            .into_iter()
            .flatten()
        {
            if accessor.count() == 0
                || accessor.count() > 64_000_000
                || accessor
                    .sparse()
                    .is_some_and(|s| s.count() == 0 || s.count() > accessor.count())
            {
                return Err("Invalid or oversized glTF mesh accessor".into());
            }
        }
        let start = match blocks.get(&key) {
            Some(&start) => start,
            None => {
                let start = u32::try_from(imported.positions.len())
                    .map_err(|_| "Too many glTF vertices")?;
                let positions = reader.read_positions().ok_or("Unreadable glTF positions")?;
                if positions.len() != count || count > (u32::MAX - start) as usize {
                    return Err("Invalid glTF position count".into());
                }
                for p in positions {
                    imported.positions.push(checked_position(
                        matrix.transform_point3(Vec3::from_array(p)),
                    )?);
                }
                if color.is_some() {
                    let colors = reader
                        .read_colors(0)
                        .ok_or("Unreadable glTF vertex colors")?
                        .into_rgba_f32();
                    if colors.len() != count {
                        return Err("glTF color count does not match positions".into());
                    }
                    imported.colors.resize(start as usize, [0; 4]);
                    imported.colors.extend(colors.map(polypaint));
                }
                blocks.insert(key, start);
                start
            }
        };
        let uvs = if uv.is_some() {
            let uvs = reader
                .read_tex_coords(0)
                .ok_or("Unreadable glTF UVs")?
                .into_f32();
            if uvs.len() != count {
                return Err("glTF UV count does not match positions".into());
            }
            let uvs: Vec<_> = uvs.map(|[u, v]| Vec2::new(u, 1.0 - v)).collect();
            if uvs.iter().any(|uv| !uv.is_finite()) {
                return Err("Invalid glTF UV".into());
            }
            Some(uvs)
        } else {
            None
        };
        let indices: Vec<_> = if index.is_some() {
            reader
                .read_indices()
                .ok_or("Unreadable glTF triangle indices")?
                .into_u32()
                .collect()
        } else {
            (0..count as u32).collect()
        };
        if indices.len() < 3
            || (mode == Mode::Triangles && indices.len() % 3 != 0)
            || indices.iter().any(|&i| i as usize >= count)
        {
            return Err("Invalid glTF triangle indices".into());
        }
        let group = u16::try_from(imported.layout.groups.len())
            .map_err(|_| "Too many glTF primitives in one mesh")?;
        imported
            .layout
            .groups
            .push(primitive.material().name().map_or_else(
                || format!("Primitive {}", primitive.index() + 1),
                |name| name.replace(['\r', '\n'], " "),
            ));
        let mut push = |mut tri: [u32; 3]| {
            if tri[0] == tri[1] || tri[1] == tri[2] || tri[2] == tri[0] {
                imported.skipped += 1;
                return;
            }
            if mirrored {
                tri.swap(1, 2);
            }
            imported.indices.extend(tri.map(|i| start + i));
            imported.groups.push(group);
            if with_uvs {
                imported.push_triangle_uvs(tri.map(|i| uvs.as_ref().map(|uv| uv[i as usize])));
            }
        };
        match mode {
            Mode::Triangles => indices
                .chunks_exact(3)
                .for_each(|t| push([t[0], t[1], t[2]])),
            Mode::TriangleStrip => indices.windows(3).enumerate().for_each(|(i, t)| {
                push(if i % 2 == 0 {
                    [t[0], t[1], t[2]]
                } else {
                    [t[1], t[0], t[2]]
                });
            }),
            Mode::TriangleFan => indices[1..]
                .windows(2)
                .for_each(|t| push([indices[0], t[0], t[1]])),
            _ => unreachable!(),
        }
    }
    if !imported.colors.is_empty() {
        imported.colors.resize(imported.positions.len(), [0; 4]);
    }
    imported.compact_attributes();
    Ok((!imported.indices.is_empty()).then(|| Part {
        name: name.to_owned(),
        mesh: imported,
    }))
}

fn polypaint([r, g, b, a]: [f32; 4]) -> [u8; 4] {
    let a = a.clamp(0.0, 1.0);
    let channel = |value: f32| {
        let c = value.clamp(0.0, 1.0);
        let srgb = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (srgb * a * 255.0).round() as u8
    };
    [
        channel(r),
        channel(g),
        channel(b),
        (a * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_gltf_buffer_and_applies_nested_reflection_to_a_triangle_strip() {
        let dir = std::env::temp_dir().join(format!("klay-gltf-buffer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("strip.gltf");
        let positions = [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::X + Vec3::Y];
        let bytes: Vec<_> = positions
            .into_iter()
            .flat_map(|p| p.to_array())
            .flat_map(f32::to_le_bytes)
            .collect();
        let json = r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":48,"uri":"BUFFER"}],
            "bufferViews":[{"buffer":0,"byteLength":48}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "meshes":[{"primitives":[{"mode":5,"attributes":{"POSITION":0}}]}],
            "scenes":[{"nodes":[0]}],
            "nodes":[{"translation":[3,0,0],"children":[1]},{"name":"Reflect","mesh":0,"scale":[-1,1,1]}]
        }"#;
        let bin = dir.join("local data.bin");
        std::fs::write(&bin, &bytes).unwrap();
        std::fs::write(&path, json.replace("BUFFER", "local%20data.bin")).unwrap();
        let parts = read(&path, "strip").unwrap();
        assert_eq!(parts[0].name, "Reflect");
        assert_eq!(parts[0].mesh.positions[1], Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(parts[0].mesh.indices, [0, 2, 1, 2, 3, 1]);
        std::fs::remove_file(bin).unwrap();
        assert!(read(&path, "strip").is_err());
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            STANDARD.encode(bytes)
        );
        std::fs::write(&path, json.replace("BUFFER", &uri)).unwrap();
        assert_eq!(
            read(&path, "strip").unwrap()[0].mesh.indices,
            parts[0].mesh.indices
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
