use super::{Part, checked_position};
use crate::obj::Imported;
use glam::{Vec2, Vec3};
use std::collections::{HashMap, HashSet};

pub(super) fn parse(bytes: &[u8], name: &str) -> Result<Vec<Part>, String> {
    let opts = ufbx::LoadOpts {
        file_format: ufbx::FileFormat::Fbx,
        index_error_handling: ufbx::IndexErrorHandling::AbortLoading,
        ignore_animation: true,
        ignore_embedded: true,
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        ..Default::default()
    };
    let scene = ufbx::load_memory(bytes, opts).map_err(|e| {
        let mut buf = [0; 1024];
        ufbx::format_error(&mut buf, &e);
        format!(
            "FBX: {}",
            String::from_utf8_lossy(&buf).trim_end_matches('\0').trim()
        )
    })?;
    let mut parts = Vec::new();
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.as_ref().filter(|mesh| mesh.num_triangles > 0) else {
            continue;
        };
        let name = if node.element.name.is_empty() {
            name.into()
        } else {
            node.element.name.to_string()
        };
        let transform = &node.geometry_to_world;
        let mirrored = ufbx::matrix_determinant(transform) < 0.0;
        let positions = mesh
            .vertices
            .iter()
            .map(|&p| {
                let p = ufbx::transform_position(transform, p);
                checked_position(Vec3::new(p.x as f32, p.y as f32, p.z as f32))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut imported = Imported {
            positions,
            ..Default::default()
        };
        if mesh.vertex_color.exists {
            imported.colors = mesh
                .vertex_first_index
                .iter()
                .map(|&i| {
                    let c = mesh
                        .vertex_color
                        .indices
                        .get(i as usize)
                        .and_then(|&j| mesh.vertex_color.values.get(j as usize));
                    c.map_or([0; 4], |c| {
                        let a = c.w.clamp(0.0, 1.0);
                        let channel = |v: f64| (v.clamp(0.0, 1.0) * a * 255.0).round() as u8;
                        [
                            channel(c.x),
                            channel(c.y),
                            channel(c.z),
                            (a * 255.0).round() as u8,
                        ]
                    })
                })
                .collect();
        }
        let mut groups = HashMap::new();
        let mut corners = Vec::new();
        let mut face_vertices = HashSet::new();
        let mut triangulated = vec![0; mesh.max_face_triangles * 3];
        let mut triangles = Vec::new();
        for (i, &face) in mesh.faces.iter().enumerate() {
            if face.num_indices < 3 || mesh.face_hole.get(i) == Some(&true) {
                imported.skipped += 1;
                continue;
            }
            let begin = face.index_begin as usize;
            let end = begin + face.num_indices as usize;
            corners.clear();
            for j in begin..end {
                let uv = mesh
                    .vertex_uv
                    .indices
                    .get(j)
                    .and_then(|&idx| mesh.vertex_uv.values.get(idx as usize))
                    .map(|uv| Vec2::new(uv.x as f32, uv.y as f32));
                if uv.is_some_and(|uv| !uv.is_finite()) {
                    return Err(format!("FBX mesh {name} has invalid UVs"));
                }
                corners.push((mesh.vertex_indices[j], uv));
            }
            face_vertices.clear();
            if !corners.iter().all(|&(v, _)| face_vertices.insert(v)) {
                imported.skipped += 1;
                continue;
            }
            let count = mesh.triangulate_face(&mut triangulated, face) as usize;
            if count != corners.len() - 2 {
                imported.skipped += 1;
                continue;
            }
            triangles.clear();
            for tri in triangulated[..count * 3].chunks_exact(3) {
                let local = |j| {
                    let corner = tri[j] as usize - begin;
                    if mirrored {
                        corners.len() - 1 - corner
                    } else {
                        corner
                    }
                };
                triangles.push(if mirrored {
                    [local(0), local(2), local(1)]
                } else {
                    [local(0), local(1), local(2)]
                });
            }
            if mirrored {
                corners.reverse();
            }
            let slot = mesh.face_material.get(i).copied().unwrap_or(0);
            let group = match groups.get(&slot) {
                Some(&group) => group,
                None => {
                    let group =
                        u16::try_from(groups.len()).map_err(|_| "Too many FBX materials")?;
                    let label = node
                        .materials
                        .get(slot as usize)
                        .map(|m| m.element.name.replace(['\r', '\n'], " "))
                        .unwrap_or_default();
                    imported.layout.groups.push(label);
                    groups.insert(slot, group);
                    group
                }
            };
            imported.push_polygon(&corners, &triangles, group);
        }
        if !imported.indices.is_empty() {
            imported.compact_attributes();
            parts.push(Part {
                name,
                mesh: imported,
            });
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_fbx_instances_in_metres_with_quad_topology() {
        let input = br#"; FBX 7.4.0 project file
FBXHeaderExtension: { FBXHeaderVersion: 1003 FBXVersion: 7400 }
GlobalSettings: {
    Properties70: {
        P: "UnitScaleFactor", "double", "Number", "", 1
    }
}
Objects: {
    Geometry: 10, "Geometry::Panel", "Mesh" {
        Vertices: *12 { a: 0,0,0,100,0,0,100,100,0,0,100,0 }
        PolygonVertexIndex: *4 { a: 0,1,2,-4 }
    }
    Model: 20, "Model::Panel", "Mesh" {
        Properties70: { P: "Lcl Translation", "Lcl Translation", "", "A", 200,0,0 }
    }
    Model: 30, "Model::Mirror", "Mesh" {
        Properties70: { P: "Lcl Scaling", "Lcl Scaling", "", "A", -1,1,1 }
    }
}
Connections: {
    C: "OO",10,20
    C: "OO",10,30
    C: "OO",20,0
    C: "OO",30,0
}
"#;
        let parts = parse(input, "file").unwrap();
        assert_eq!(parts.len(), 2);
        let panel = parts.iter().find(|p| p.name == "Panel").unwrap();
        assert_eq!(panel.mesh.positions[0], Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(panel.mesh.layout.ends, [2]);
        assert!(
            panel
                .mesh
                .quads
                .iter()
                .all(|&edge| edge < crate::mesh::LONE)
        );
        let mirrored = parts.iter().find(|p| p.name == "Mirror").unwrap();
        assert_eq!(mirrored.mesh.positions[1], -Vec3::X);
        for part in &parts {
            for tri in part.mesh.indices.chunks_exact(3) {
                let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| part.mesh.positions[i as usize]);
                assert!((b - a).cross(c - a).z > 0.0);
            }
        }
    }
}
