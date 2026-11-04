//! GoZ mesh exchange, using the blocks described in Maxon's GoZ SDK.
use super::{Part, checked_position};
use crate::obj::Imported;
use glam::{Vec2, Vec3};

const MESH_NAME: u32 = 1;
const POINTS: u32 = 10_001;
const DOUBLE_POINTS: u32 = 10_002;
const FACES4: u32 = 20_001;
const FACES4_REPEAT: u32 = 20_002;
const FACES3: u32 = 20_003;
const UV4: u32 = 25_001;
const UV3: u32 = 25_002;
const MASK8: u32 = 30_001;
const MASK16: u32 = 30_002;
const MRGB: u32 = 35_001;
const GROUPS: u32 = 40_001;

pub(super) fn parse(bytes: &[u8], mut name: String) -> Result<Part, String> {
    let mut tail = bytes
        .get(32..)
        .filter(|_| bytes.starts_with(b"GoZb"))
        .ok_or("Invalid GoZ header")?;
    let mut mesh = Imported::default();
    let mut faces = None;
    let mut uvs = None;
    let mut groups = None;
    loop {
        // Older writers use just a zero tag for the end marker.
        let tag = tail.get(..4).map(u32_at).ok_or("Missing GoZ end marker")?;
        if tag == 0 {
            break;
        }
        let header = tail.get(..16).ok_or("Truncated GoZ block header")?;
        let size = u32_at(&header[4..]) as usize;
        let count = u32_at(&header[8..]) as usize;
        let payload = tail.get(16..size).ok_or("Invalid GoZ block size")?;
        tail = &tail[size..];
        let records = |stride| {
            if count.checked_mul(stride) != Some(payload.len()) {
                return Err(format!("Invalid GoZ block {tag} length"));
            }
            Ok(payload.chunks_exact(stride))
        };
        match tag {
            MESH_NAME => {
                let raw = payload.split(|&b| b == 0).next().unwrap_or_default();
                let text = String::from_utf8_lossy(raw);
                let label = text.strip_prefix("GoZMesh_").unwrap_or(&text).trim();
                if !label.is_empty() {
                    name = label.to_owned();
                }
            }
            POINTS => {
                mesh.positions = records(12)?
                    .map(|p| {
                        checked_position(Vec3::new(f32_at(p), f32_at(&p[4..]), f32_at(&p[8..])))
                    })
                    .collect::<Result<_, _>>()?;
            }
            DOUBLE_POINTS => {
                let coord = |p: &[u8]| f64::from_le_bytes(p[..8].try_into().unwrap()) as f32;
                mesh.positions = records(24)?
                    .map(|p| checked_position(Vec3::new(coord(p), coord(&p[8..]), coord(&p[16..]))))
                    .collect::<Result<_, _>>()?;
            }
            FACES4 | FACES4_REPEAT | FACES3 => {
                faces = Some((tag, records(if tag == FACES3 { 12 } else { 16 })?));
            }
            UV4 | UV3 => uvs = Some(records(if tag == UV4 { 32 } else { 24 })?),
            MASK8 => mesh.mask = records(1)?.map(|m| 255 - m[0]).collect(),
            MASK16 => {
                mesh.mask = records(2)?
                    .map(|m| {
                        let value = u16::from_le_bytes([m[0], m[1]]);
                        255 - ((u32::from(value) + 128) / 257) as u8
                    })
                    .collect()
            }
            MRGB => mesh.colors = records(4)?.map(|c| [c[2], c[1], c[0], 255]).collect(),
            GROUPS => groups = Some(records(2)?),
            _ => {}
        }
    }
    let (face_kind, faces) = faces.ok_or("GoZ file has no polygons")?;
    if (!mesh.colors.is_empty() && mesh.colors.len() != mesh.positions.len())
        || (!mesh.mask.is_empty() && mesh.mask.len() != mesh.positions.len())
        || uvs.as_ref().is_some_and(|uvs| uvs.len() != faces.len())
        || groups
            .as_ref()
            .is_some_and(|groups| groups.len() != faces.len())
    {
        return Err("GoZ attribute count does not match the mesh".into());
    }
    for face in faces {
        let uv = uvs.as_mut().and_then(Iterator::next);
        let group = groups
            .as_mut()
            .and_then(Iterator::next)
            .map_or(0, |g| u16::from_le_bytes([g[0], g[1]]));
        let mut corners = [(0, None); 4];
        let mut len = 0;
        for (j, v) in face.chunks_exact(4).map(u32_at).enumerate() {
            if j == 3
                && ((face_kind == FACES4 && v == u32::MAX)
                    || (face_kind == FACES4_REPEAT && v == corners[2].0))
            {
                break;
            }
            if v as usize >= mesh.positions.len() {
                return Err("GoZ face index is out of range".into());
            }
            let tex = uv
                .map(|uv| {
                    uv.get(j * 8..j * 8 + 8)
                        .ok_or("GoZ face is missing a UV corner")
                })
                .transpose()?
                .map(|uv| Vec2::new(f32_at(uv), 1.0 - f32_at(&uv[4..])));
            if tex.is_some_and(|uv| !uv.is_finite()) {
                return Err("Invalid GoZ UV".into());
            }
            corners[j] = (v, tex);
            len += 1;
        }
        let corners = &corners[..len];
        if corners
            .iter()
            .enumerate()
            .any(|(i, c)| corners[..i].iter().any(|other| other.0 == c.0))
        {
            mesh.skipped += 1;
            continue;
        }
        mesh.push_polygon(corners, &[[0, 1, 2], [0, 2, 3]][..len - 2], group);
    }
    if mesh.indices.is_empty() {
        return Err("GoZ file has no usable polygons".into());
    }
    mesh.compact_attributes();
    Ok(Part { name, mesh })
}

fn u32_at(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn f32_at(bytes: &[u8]) -> f32 {
    f32::from_bits(u32_at(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_goz_polygons_and_vertex_channels() {
        fn block(bytes: &mut Vec<u8>, tag: u32, count: u32, payload: &[u8]) {
            for word in [tag, 16 + payload.len() as u32, count, 0] {
                bytes.extend(word.to_le_bytes());
            }
            bytes.extend(payload);
        }
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(b"GoZb");
        block(&mut bytes, MESH_NAME, 1, b"GoZMesh_Study\0\0\0");
        let points: Vec<_> = [Vec3::ZERO, Vec3::X, Vec3::X + Vec3::Y, Vec3::Y, Vec3::Z]
            .into_iter()
            .flat_map(|p| p.to_array())
            .flat_map(f32::to_le_bytes)
            .collect();
        block(&mut bytes, POINTS, 5, &points);
        let faces: Vec<_> = [0, 1, 2, 3, 1, 0, 4, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        block(&mut bytes, FACES4, 2, &faces);
        let uvs: Vec<_> = [0.0, 0.25]
            .repeat(8)
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        block(&mut bytes, UV4, 2, &uvs);
        block(
            &mut bytes,
            MASK16,
            5,
            &[0, 0, 255, 255, 255, 255, 255, 255, 255, 255],
        );
        block(&mut bytes, MRGB, 5, &[20, 40, 60, 0].repeat(5));
        block(&mut bytes, GROUPS, 2, &[8, 0, 12, 0]);
        block(&mut bytes, 0, 0, &[]);
        let part = parse(&bytes, "file".into()).unwrap();
        assert_eq!(part.name, "Study");
        assert_eq!(part.mesh.layout.ends, [2, 3]);
        assert_eq!(part.mesh.groups, [8, 8, 12]);
        assert_eq!(part.mesh.uvs[0][0], Vec2::new(0.0, 0.75));
        let doc = part.mesh.into_doc(10_000);
        let mesh = doc.mesh();
        assert_eq!(mesh.colors, [[60, 40, 20, 255]; 5]);
        assert_eq!(mesh.mask.iter().filter(|&&value| value == 255).count(), 1);
        assert_eq!(
            mesh.quads
                .iter()
                .filter(|&&edge| edge < crate::mesh::LONE)
                .count(),
            2
        );
        assert!(parse(&bytes[..60], "file".into()).is_err());
    }
}
