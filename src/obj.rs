//! OBJ import (Document › Import; ZBrush: Tool › Import). Positions and faces, with `o` / `g`
//! names as polygroups (Klay's own export writes one per polygroup and subtool) and vertex
//! colours (`v x y z r g b`) as polypaint. Polygons are split into triangles as fans;
//! texture coordinates, normals and materials are ignored.

use glam::Vec3;
use std::collections::HashMap;

pub struct Imported {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Polygroup per triangle.
    pub groups: Vec<u16>,
    /// Premultiplied sRGB polypaint per vertex, empty when the file has no vertex colours.
    pub colors: Vec<[u8; 4]>,
    /// Faces with fewer than three corners or repeated corners, left out.
    pub skipped: usize,
}

/// Read OBJ text. Fails on a face that names a vertex the file does not have, or on a file
/// with no faces.
pub fn parse(text: &str) -> Result<Imported, String> {
    let mut positions = Vec::new();
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut any_color = false;
    let mut indices = Vec::new();
    let mut groups = Vec::new();
    let mut names: HashMap<String, u16> = HashMap::new();
    let mut group = 0u16;
    let mut skipped = 0;
    let mut corners: Vec<u32> = Vec::with_capacity(8);
    for (line_no, line) in text.lines().enumerate() {
        let mut parts = line.split_ascii_whitespace();
        match parts.next() {
            Some("v") => {
                let nums: Vec<f32> = parts.take(6).map_while(|s| s.parse().ok()).collect();
                if nums.len() < 3 {
                    return Err(format!("line {}: a vertex needs three numbers", line_no + 1));
                }
                positions.push(Vec3::new(nums[0], nums[1], nums[2]));
                if nums.len() == 6 {
                    any_color = true;
                    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                    colors.push([c(nums[3]), c(nums[4]), c(nums[5]), 255]);
                } else {
                    colors.push([0; 4]);
                }
            }
            Some("o") | Some("g") => {
                let name = parts.collect::<Vec<_>>().join(" ");
                let next = names.len() as u16;
                group = *names.entry(name).or_insert(next);
            }
            Some("f") => {
                corners.clear();
                for item in parts {
                    let first = item.split('/').next().unwrap_or("");
                    let i: i64 = first.parse().map_err(|_| format!("line {}: bad face corner {item:?}", line_no + 1))?;
                    // 1-based, or negative counting back from the last vertex so far.
                    let index = if i > 0 { i - 1 } else { positions.len() as i64 + i };
                    if index < 0 || index >= positions.len() as i64 {
                        return Err(format!("line {}: face corner {i} is not a vertex", line_no + 1));
                    }
                    corners.push(index as u32);
                }
                let distinct = corners.iter().enumerate().all(|(k, a)| !corners[..k].contains(a));
                if corners.len() < 3 || !distinct {
                    skipped += 1;
                    continue;
                }
                for k in 1..corners.len() - 1 {
                    indices.extend_from_slice(&[corners[0], corners[k], corners[k + 1]]);
                    groups.push(group);
                }
            }
            _ => {}
        }
    }
    if indices.is_empty() {
        return Err("the file has no faces".into());
    }
    Ok(Imported { positions, indices, groups, colors: if any_color { colors } else { Vec::new() }, skipped })
}

/// Centre `positions` on the origin and scale them so their largest extent is `size`;
/// returns the scale applied.
pub fn fit(positions: &mut [Vec3], size: f32) -> f32 {
    let (lo, hi) = positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)));
    let centre = (lo + hi) * 0.5;
    let scale = size / (hi - lo).max_element().max(1e-12);
    for p in positions.iter_mut() {
        *p = (*p - centre) * scale;
    }
    scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quads_triangles_groups_colours_and_negative_indices() {
        let text = "# a quad and a triangle\nv 0 0 0 1 0 0\nv 1 0 0 0 1 0\nv 1 1 0 0 0 1\nv 0 1 0 1 1 1\no first\nf 1/1/1 2/2/2 3/3/3 4/4/4\ng second\nv 0 0 1 0 0 0\nf -1 1 2\nf 1 1 2\n";
        let m = parse(text).unwrap();
        assert_eq!(m.positions.len(), 5);
        assert_eq!(m.indices, vec![0, 1, 2, 0, 2, 3, 4, 0, 1]);
        assert_eq!(m.groups, vec![0, 0, 1]);
        assert_eq!(m.colors[0], [255, 0, 0, 255]);
        assert_eq!(m.skipped, 1, "the face with a repeated corner");
        assert!(parse("v 0 0 0\nf 1 2 3\n").is_err());
        assert!(parse("v 0 0 0\n").is_err());
    }

    #[test]
    fn klay_exports_import_with_their_polygroups() {
        let (p, i, g) = crate::primitive::Settings { kind: crate::primitive::Primitive::Cube, ..Default::default() }.build(1.0);
        let mesh = crate::mesh::SculptMesh::from_raw_with_groups(p, i, g);
        let mut out = Vec::new();
        mesh.write_obj(&mut out, "Cube", 0).unwrap();
        let back = parse(std::str::from_utf8(&out).unwrap()).unwrap();
        assert_eq!(back.positions.len(), mesh.vertex_count());
        assert_eq!(back.indices.len(), mesh.indices.len());
        let count = |g: &[u16]| {
            let mut v = g.to_vec();
            v.sort();
            v.dedup();
            v.len()
        };
        assert_eq!(count(&back.groups), mesh.group_count());
        let mut pts = back.positions.clone();
        let s = fit(&mut pts, 2.0);
        assert!(s > 0.0 && pts.iter().all(|q| q.abs().max_element() <= 1.0 + 1e-5));
    }
}
