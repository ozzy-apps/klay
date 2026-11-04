//! OBJ import. The sculpting mesh uses triangles, with the original polygons and corner
//! UVs kept for export and quad pairs marked for modeling.

use glam::{Vec2, Vec3};
use std::collections::HashMap;

#[derive(Default)]
pub struct Imported {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Polygroup per triangle.
    pub groups: Vec<u16>,
    /// Premultiplied sRGB polypaint per vertex, empty when the file has no vertex colours.
    pub colors: Vec<[u8; 4]>,
    pub mask: Vec<u8>,
    pub uvs: Vec<[Vec2; 3]>,
    pub quads: Vec<u8>,
    pub layout: Layout,
    /// Faces with fewer than three corners or repeated corners, left out.
    pub skipped: usize,
}

#[derive(Clone, Default)]
pub struct Layout {
    /// End of each face's triangle range in `triangles`.
    pub ends: Vec<u32>,
    pub triangles: Vec<u32>,
    /// Original polygon corners, each referring to one corner in the triangulation.
    pub boundary: Vec<u32>,
    pub groups: Vec<String>,
    /// Which original corners had UVs, when the file had a mixture of mapped and unmapped faces.
    pub mapped: Vec<u8>,
    pub seams: Vec<[u32; 2]>,
}

impl Layout {
    pub fn write<W: std::io::Write>(
        &self,
        out: &mut crate::project::Out<W>,
    ) -> std::io::Result<()> {
        out.slice(&self.ends)?;
        out.slice(&self.triangles)?;
        out.slice(&self.boundary)?;
        out.slice(&self.mapped)?;
        out.slice(&self.seams)?;
        out.u32(self.groups.len() as u32)?;
        for name in &self.groups {
            out.slice(name.as_bytes())?;
        }
        Ok(())
    }

    pub fn read<R: std::io::Read>(
        input: &mut crate::project::In<R>,
        indices: &[u32],
    ) -> Result<Self, String> {
        let ends: Vec<u32> = input.vec()?;
        let triangles: Vec<u32> = input.vec()?;
        let boundary: Vec<u32> = input.vec()?;
        let mapped: Vec<u8> = input.vec()?;
        let seams: Vec<[u32; 2]> = input.vec()?;
        let count = input.u32()?;
        if count > u16::MAX as u32 + 1 {
            return Err("Too many OBJ groups".into());
        }
        let groups = (0..count)
            .map(|_| String::from_utf8(input.vec()?).map_err(|_| "Invalid OBJ group name".into()))
            .collect::<Result<Vec<String>, String>>()?;
        let n = indices.len() / 3;
        if ends.is_empty() != triangles.is_empty()
            || boundary.len() != triangles.len() + 2 * ends.len()
            || (!ends.is_empty()
                && (ends[0] == 0
                    || ends.last().copied() != Some(n as u32)
                    || ends.windows(2).any(|e| e[0] >= e[1])
                    || triangles.len() != n))
            || (!mapped.is_empty() && (mapped.len() != n || mapped.iter().any(|&b| b > 7)))
            || groups.iter().any(|name| name.contains(['\r', '\n']))
        {
            return Err("OBJ face data does not match the mesh".into());
        }
        let mut seen = vec![false; triangles.len()];
        for &t in &triangles {
            if t as usize >= n || std::mem::replace(&mut seen[t as usize], true) {
                return Err("Invalid OBJ face triangle".into());
            }
        }
        let mut start = 0;
        let mut owner = vec![0; triangles.len()];
        for (f, &end) in ends.iter().enumerate() {
            for &t in &triangles[start..end as usize] {
                owner[t as usize] = f;
            }
            start = end as usize;
        }
        start = 0;
        for (f, &end) in ends.iter().enumerate() {
            let corners = &boundary[start + 2 * f..end as usize + 2 * (f + 1)];
            if corners
                .iter()
                .any(|&c| owner.get(c as usize / 3) != Some(&f))
            {
                return Err("Invalid OBJ polygon boundary".into());
            }
            start = end as usize;
        }
        if !seams.is_empty() {
            let edges: crate::fx::FxHashSet<_> = indices
                .chunks_exact(3)
                .flat_map(|t| {
                    (0..3).map(move |i| {
                        let (a, b) = (t[i], t[(i + 1) % 3]);
                        [a.min(b), a.max(b)]
                    })
                })
                .collect();
            if seams.windows(2).any(|e| e[0] >= e[1]) || seams.iter().any(|e| !edges.contains(e)) {
                return Err("UV seams do not match the base mesh".into());
            }
        }
        Ok(Self {
            ends,
            triangles,
            boundary,
            groups,
            mapped,
            seams,
        })
    }
}

impl Imported {
    /// Keep polygon boundaries and corner attributes alongside the sculpting triangles.
    pub fn push_polygon(
        &mut self,
        corners: &[(u32, Option<Vec2>)],
        triangles: &[[usize; 3]],
        group: u16,
    ) {
        let first = self.layout.boundary.len();
        self.layout.boundary.resize(first + corners.len(), 0);
        for (k, &local) in triangles.iter().enumerate() {
            let start = self.indices.len() as u32;
            for (i, corner) in local.into_iter().enumerate() {
                self.layout.boundary[first + corner] = start + i as u32;
            }
            let tri = local.map(|c| corners[c]);
            self.indices.extend(tri.map(|c| c.0));
            self.push_triangle_uvs(tri.map(|c| c.1));
            self.quads.push(if corners.len() == 4 {
                let other = triangles[1 - k].map(|c| corners[c].0);
                crate::mesh::shared_edge(&tri.map(|c| c.0), &other).unwrap_or(crate::mesh::LONE)
            } else {
                crate::mesh::LONE
            });
            self.groups.push(group);
        }
        self.layout.ends.push(self.groups.len() as u32);
    }

    pub fn push_triangle_uvs(&mut self, uv: [Option<Vec2>; 3]) {
        let bits = uv.iter().enumerate().fold(0, |bits, (i, uv)| bits | (u8::from(uv.is_some()) << i));
        self.layout.mapped.push(bits);
        self.uvs.push(uv.map(Option::unwrap_or_default));
    }

    pub fn compact_attributes(&mut self) {
        if self.layout.mapped.iter().all(|&bits| bits == 0) {
            self.uvs.clear();
            self.layout.mapped.clear();
        } else if self.layout.mapped.iter().all(|&bits| bits == 7) {
            self.layout.mapped.clear();
        }
        if self.quads.iter().all(|&q| q == crate::mesh::LONE) {
            self.quads.clear();
        }
    }

    pub fn into_doc(mut self, budget: usize) -> crate::multires::Multires {
        let (mut mesh, vertex_slots, triangle_slots) = crate::mesh::SculptMesh::from_raw_tracked(
            self.positions,
            self.indices,
            self.groups,
            self.colors,
            self.quads,
        );
        if self.mask.iter().any(|&m| m != 0) {
            let mut mask = vec![0; vertex_slots.len()];
            for (&slot, value) in vertex_slots.iter().zip(self.mask) {
                mask[slot as usize] = value;
            }
            crate::mask::replace(&mut mesh, mask);
        }
        let mut doc = crate::multires::Multires::new(mesh, budget);
        if !self.uvs.is_empty() {
            let mut uvs = vec![[Vec2::ZERO; 3]; triangle_slots.len()];
            for (&slot, uv) in triangle_slots.iter().zip(self.uvs) {
                uvs[slot as usize] = uv;
            }
            doc.swap_uvs(uvs);
        }
        if !self.layout.mapped.is_empty() {
            let mut mapped = vec![0; triangle_slots.len()];
            for (&slot, &bits) in triangle_slots.iter().zip(&self.layout.mapped) {
                mapped[slot as usize] = bits;
            }
            self.layout.mapped = mapped;
        }
        if !self.layout.ends.is_empty() {
            self.layout
                .boundary
                .iter_mut()
                .for_each(|c| *c = 3 * triangle_slots[*c as usize / 3] + *c % 3);
            self.layout.triangles = triangle_slots;
        }
        doc.obj_layout = self.layout;
        doc
    }

    /// Match the cage by vertex number and oriented triangles, independent of face order.
    pub fn align_to(&self, old: &crate::mesh::SculptMesh) -> Option<crate::multires::Multires> {
        if self.positions.len() != old.vertex_count() || self.indices.len() != old.indices.len() {
            return None;
        }
        let key = |t: &[u32]| {
            let k = (0..3).min_by_key(|&k| t[k]).unwrap();
            [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
        };
        let mut faces = crate::fx::FxHashMap::default();
        for (t, tri) in self.indices.chunks_exact(3).enumerate() {
            if faces.insert(key(tri), t).is_some() {
                return None;
            }
        }
        let mut groups = Vec::with_capacity(self.groups.len());
        let mut uvs = Vec::with_capacity(self.uvs.len());
        let mut mapped = Vec::with_capacity(self.layout.mapped.len());
        for tri in old.indices.chunks_exact(3) {
            let t = faces.remove(&key(tri))?;
            groups.push(self.groups[t]);
            let src = &self.indices[3 * t..3 * t + 3];
            let k = src.iter().position(|&v| v == tri[0])?;
            if let Some(uv) = self.uvs.get(t) {
                uvs.push([uv[k], uv[(k + 1) % 3], uv[(k + 2) % 3]]);
            }
            if let Some(&bits) = self.layout.mapped.get(t) {
                mapped.push((0..3).fold(0, |b, i| b | (((bits >> ((k + i) % 3)) & 1) << i)));
            }
        }
        let mut mesh = old.clone();
        mesh.positions.clone_from(&self.positions);
        if !self.colors.is_empty() {
            mesh.colors.clone_from(&self.colors);
        }
        if self.layout.groups.iter().any(|g| !g.is_empty()) {
            mesh.set_groups(groups);
        }
        let mut doc = crate::multires::Multires::new(mesh, 0);
        doc.swap_uvs(uvs);
        doc.obj_layout.groups.clone_from(&self.layout.groups);
        doc.obj_layout.mapped = mapped;
        Some(doc)
    }
}

/// Read OBJ text. Fails on a face that names a vertex the file does not have, or on a file
/// with no faces.
pub fn parse(text: &str) -> Result<Imported, String> {
    let mut mesh = Imported::default();
    let mut any_color = false;
    let mut names: HashMap<String, u16> = HashMap::new();
    let mut group = 0u16;
    mesh.layout.groups.push(String::new());
    let mut texcoords = Vec::new();
    let mut corners: Vec<(u32, Option<Vec2>)> = Vec::with_capacity(8);
    for (line_no, line) in text.lines().enumerate() {
        let mut parts = line
            .split('#')
            .next()
            .unwrap_or("")
            .split_ascii_whitespace();
        let index = |s: &str, count: usize| {
            let i = s
                .parse::<i64>()
                .map_err(|_| format!("line {}: bad index {s:?}", line_no + 1))?;
            let at = if i > 0 { i - 1 } else { count as i64 + i };
            (at >= 0 && at < count as i64)
                .then_some(at as usize)
                .ok_or_else(|| format!("line {}: index {s} is out of bounds", line_no + 1))
        };
        match parts.next() {
            Some("v") => {
                let nums: Vec<f32> = parts.take(6).map_while(|s| s.parse().ok()).collect();
                if nums.len() < 3 || nums.iter().any(|x| !x.is_finite()) {
                    return Err(format!(
                        "line {}: a vertex needs three numbers",
                        line_no + 1
                    ));
                }
                mesh.positions.push(Vec3::new(nums[0], nums[1], nums[2]));
                if nums.len() == 6 {
                    any_color = true;
                    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                    mesh.colors.push([c(nums[3]), c(nums[4]), c(nums[5]), 255]);
                } else {
                    mesh.colors.push([0; 4]);
                }
            }
            Some("vt") => {
                let u = parts.next().and_then(|s| s.parse::<f32>().ok());
                let v = parts.next().map_or(Some(0.0), |s| s.parse::<f32>().ok());
                let uv = u
                    .zip(v)
                    .map(|(u, v)| Vec2::new(u, v))
                    .filter(|uv| uv.is_finite())
                    .ok_or_else(|| format!("line {}: invalid texture coordinate", line_no + 1))?;
                texcoords.push(uv);
            }
            Some("o") | Some("g") => {
                let name = parts.collect::<Vec<_>>().join(" ");
                if names.is_empty() && mesh.groups.is_empty() {
                    mesh.layout.groups.clear();
                }
                group = match names.get(&name) {
                    Some(&id) => id,
                    None => {
                        let id = u16::try_from(mesh.layout.groups.len())
                            .map_err(|_| "Too many OBJ groups")?;
                        mesh.layout.groups.push(name.clone());
                        names.insert(name, id);
                        id
                    }
                };
            }
            Some("f") => {
                corners.clear();
                for item in parts {
                    let mut fields = item.split('/');
                    let vertex = index(fields.next().unwrap_or(""), mesh.positions.len())?;
                    let uv = fields
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|s| index(s, texcoords.len()).map(|i| texcoords[i]))
                        .transpose()?;
                    corners.push((vertex as u32, uv));
                }
                let distinct = corners
                    .iter()
                    .enumerate()
                    .all(|(k, a)| !corners[..k].iter().any(|b| a.0 == b.0));
                if corners.len() < 3 || !distinct {
                    mesh.skipped += 1;
                    continue;
                }
                let count = corners.len();
                let long_face;
                let two_triangles = [[0, 1, 2], [0, 2, 3]];
                let tris: &[[usize; 3]] = if count <= 4 {
                    &two_triangles[..count - 2]
                } else {
                    let points: Vec<_> = corners
                        .iter()
                        .map(|&(v, _)| mesh.positions[v as usize])
                        .collect();
                    long_face = triangulate(&points);
                    if long_face.len() != count - 2 {
                        mesh.skipped += 1;
                        continue;
                    }
                    &long_face
                };
                mesh.push_polygon(&corners, tris, group);
            }
            _ => {}
        }
    }
    if mesh.indices.is_empty() {
        return Err("the file has no faces".into());
    }
    if !any_color {
        mesh.colors.clear();
    }
    mesh.compact_attributes();
    Ok(mesh)
}

fn triangulate(points: &[Vec3]) -> Vec<[usize; 3]> {
    let normal: Vec3 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(&a, &b)| a.cross(b))
        .sum();
    let (u, v) = crate::brush_asset::tangent(normal.normalize_or(Vec3::Z));
    let mut uv: Vec<_> = points
        .iter()
        .map(|&p| Vec2::new(p.dot(u), p.dot(v)))
        .collect();
    let area: f32 = uv
        .iter()
        .zip(uv.iter().cycle().skip(1))
        .map(|(&a, &b)| a.perp_dot(b))
        .sum();
    if area < 0.0 {
        uv.iter_mut().for_each(|q| q.x = -q.x);
    }
    crate::cut::ear_clip(&uv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quads_triangles_groups_colours_and_negative_indices() {
        let text = "# a quad and a triangle\nv 0 0 0 1 0 0\nv 1 0 0 0 1 0\nv 1 1 0 0 0 1\nv 0 1 0 1 1 1\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\no first\nf 1/1/1 2/2/2 3/3/3 4/4/4\ng second\nv 0 0 1 0 0 0\nf -1 1 2\nf 1 1 2\n";
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
    fn parity_obj_polygon_corner_uv_and_group_survive_the_project_format() {
        let input = "v 0 0 0
v 2 0 0
v 2 2 0
v 1 1 0
v 0 2 0
v 0 0 1
vt 0 0
vt 1 0
vt 1 1
vt .5 .5
vt 0 1
vt .25 .25
g Cutout
f 1/1 2/2 3/3 4/4 5/5
g Rim
f 2/-1 1 6/1";
        let doc = parse(input).unwrap().into_doc(200_000);
        let packed = doc.pack_copy();
        let data = crate::project::write_to(Vec::new(), &[("Cutout", true, &packed)], 0).unwrap();
        let project = crate::project::read_from(data.as_slice(), data.len() as u64).unwrap();
        let doc = project
            .tools
            .into_iter()
            .next()
            .unwrap()
            .packed
            .try_unpack()
            .unwrap();
        let mut text = Vec::new();
        doc.write_obj(&mut text, "Cutout", 0, 0).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("g Cutout\n"));
        assert!(text.contains("g Rim\n"));
        let faces: Vec<_> = text.lines().filter(|s| s.starts_with("f ")).collect();
        assert_eq!(faces[0].split_whitespace().count(), 6);
        assert!(faces[1].contains("//"));
        let back = parse(&text).unwrap();
        assert_eq!(back.layout.ends, vec![3, 4]);
        assert_eq!(back.layout.mapped.last().copied(), Some(5));
        let mut coordinates: Vec<_> = back.uvs[..3].iter().flatten().copied().collect();
        coordinates.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
        coordinates.dedup();
        assert_eq!(coordinates.len(), 5);
    }

    #[test]
    fn klay_exports_import_with_their_polygroups() {
        let (p, i, g) = crate::primitive::Settings {
            kind: crate::primitive::Primitive::Cube,
            ..Default::default()
        }
        .build(1.0);
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
    }
}
