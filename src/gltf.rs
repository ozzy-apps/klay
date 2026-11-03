//! glTF 2.0 binary export (Document › Export glTF, SubTool › Export glTF): one `.glb` file
//! holding every subtool given as a node, with positions, normals, UVs (when the lowest
//! level is shown and has them), the polypaint as `COLOR_0`, a primitive per polygroup or
//! one per subtool, and a PBR material that embeds the normal and colour maps baked this
//! session as PNG textures, so Blender, Godot, Unity and Unreal show the sculpt on the
//! low-poly mesh directly. Hand-written JSON and a BIN chunk: no glTF crate.
//!
//! Klay is Y up, as glTF is, so no axis change; positions are scaled by the 3D Print scale
//! into metres. UV seams split vertices (glTF has one UV per vertex, Klay one per corner).
//! A displacement map has no place in glTF; the app's status line names its file instead.

use crate::mesh::{SculptMesh, unpack_normal};
use glam::{Vec2, Vec3};
use std::collections::HashMap;
use std::io::{self, Write};

const MAGIC: u32 = 0x4654_6C67;
const JSON_CHUNK: u32 = 0x4E4F_534A;
const BIN_CHUNK: u32 = 0x004E_4942;
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const TRIANGLES: u32 = 4;

/// One subtool to write: its shown level and what goes with it.
pub struct Node<'a> {
    pub name: &'a str,
    pub positions: &'a [Vec3],
    /// Unit normals, one per vertex (see [`Node::from_mesh`]).
    pub normals: Vec<Vec3>,
    pub indices: &'a [u32],
    /// Polygroup per triangle; empty for one group.
    pub groups: &'a [u16],
    /// UVs per triangle corner, one triple per triangle; empty for none.
    pub uvs: &'a [[Vec2; 3]],
    /// Premultiplied sRGB polypaint per vertex; empty when unpainted.
    pub colors: &'a [[u8; 4]],
    /// A tangent-space (OpenGL +Y) normal map of this subtool as a PNG, embedded as the
    /// material's `normalTexture`.
    pub normal_map: Option<Vec<u8>>,
    /// The polypaint baked into a PNG, embedded as `baseColorTexture`; the vertex colours
    /// are then left out, since the two would multiply.
    pub color_map: Option<Vec<u8>>,
}

impl<'a> Node<'a> {
    /// `mesh` as shown, with its area-weighted vertex normals, no textures.
    pub fn from_mesh(name: &'a str, mesh: &'a SculptMesh, uvs: &'a [[Vec2; 3]]) -> Self {
        let normals = mesh.compute_all_normals().into_iter().map(|n| unpack_normal(n).normalize_or(Vec3::Y)).collect();
        Node { name, positions: &mesh.positions, normals, indices: &mesh.indices, groups: &mesh.groups, uvs, colors: &mesh.colors, normal_map: None, color_map: None }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Metres per Klay unit (the 3D Print scale is millimetres per unit).
    pub scale: f32,
    /// A mesh primitive per polygroup (the groups come through as sub-meshes / material
    /// slots), else one per subtool.
    pub per_group: bool,
}

/// What was written, for the status line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub nodes: usize,
    /// After splitting at UV seams.
    pub vertices: usize,
    pub triangles: usize,
    pub primitives: usize,
    pub textures: usize,
    pub bytes: u64,
}

/// The BIN chunk and the JSON of the buffer views and accessors into it.
#[derive(Default)]
struct Bin {
    data: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Bin {
    /// A buffer view of `bytes`, 4-byte aligned; returns its index.
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
        let offset = self.data.len();
        self.data.extend_from_slice(bytes);
        let target = target.map_or(String::new(), |t| format!(",\"target\":{t}"));
        self.views.push(format!("{{\"buffer\":0,\"byteOffset\":{offset},\"byteLength\":{}{target}}}", bytes.len()));
        self.views.len() - 1
    }

    fn accessor(&mut self, view: usize, component: u32, count: usize, kind: &str, extra: &str) -> usize {
        self.accessors.push(format!("{{\"bufferView\":{view},\"componentType\":{component},\"count\":{count},\"type\":\"{kind}\"{extra}}}"));
        self.accessors.len() - 1
    }

    fn floats(&mut self, values: &[f32], count: usize, kind: &str, extra: &str) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(ARRAY_BUFFER));
        self.accessor(view, FLOAT, count, kind, extra)
    }

    /// Triangle indices, 16-bit when every index fits.
    fn indices(&mut self, indices: &[u32], vertices: usize) -> usize {
        let (bytes, component): (Vec<u8>, u32) = if vertices <= u16::MAX as usize {
            (indices.iter().flat_map(|&i| (i as u16).to_le_bytes()).collect(), UNSIGNED_SHORT)
        } else {
            (indices.iter().flat_map(|i| i.to_le_bytes()).collect(), UNSIGNED_INT)
        };
        let view = self.view(&bytes, Some(ELEMENT_ARRAY_BUFFER));
        self.accessor(view, component, indices.len(), "SCALAR", "")
    }
}

/// A JSON string literal of `s`.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON number: finite, never `NaN` or `inf` (which JSON has no spelling for).
fn num(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    format!("{v}")
}

fn list<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    let mut out = String::from("[");
    for (k, item) in items.into_iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str(&item.to_string());
    }
    out.push(']');
    out
}

/// sRGB (0..1) to linear, as the glTF spec wants vertex colours.
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Premultiplied sRGB polypaint composited over white, as linear RGBA.
fn vertex_color(c: [u8; 4]) -> [f32; 4] {
    let gap = 255 - c[3];
    let over = |x: u8| linear((x.saturating_add(gap)) as f32 / 255.0);
    [over(c[0]), over(c[1]), over(c[2]), 1.0]
}

/// The vertex arrays of one node, split at UV seams when it has UVs.
struct Vertices {
    positions: Vec<f32>,
    normals: Vec<f32>,
    /// Empty for none.
    uvs: Vec<f32>,
    colors: Vec<f32>,
    indices: Vec<u32>,
    count: usize,
    min: Vec3,
    max: Vec3,
}

fn vertices(node: &Node, scale: f32) -> Vertices {
    let with_uvs = !node.uvs.is_empty() && node.uvs.len() == node.indices.len() / 3;
    let with_colors = node.colors.len() == node.positions.len() && node.color_map.is_none();
    let (mut positions, mut normals, mut uvs, mut colors) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut min, mut max) = (Vec3::INFINITY, Vec3::NEG_INFINITY);
    let mut push = |v: u32, uv: Option<Vec2>| {
        let p = node.positions[v as usize] * scale;
        min = min.min(p);
        max = max.max(p);
        positions.extend_from_slice(&p.to_array());
        normals.extend_from_slice(&node.normals.get(v as usize).copied().unwrap_or(Vec3::Y).to_array());
        if let Some(uv) = uv {
            // glTF's V runs down the image; Klay's (and OBJ's) up.
            uvs.extend_from_slice(&[uv.x, 1.0 - uv.y]);
        }
        if with_colors {
            colors.extend_from_slice(&vertex_color(node.colors[v as usize]));
        }
    };
    let (indices, count) = if with_uvs {
        let mut map: HashMap<(u32, u32, u32), u32> = HashMap::with_capacity(node.positions.len() * 5 / 4);
        let mut indices = Vec::with_capacity(node.indices.len());
        for (t, tri) in node.indices.chunks_exact(3).enumerate() {
            for (k, &v) in tri.iter().enumerate() {
                let uv = node.uvs[t][k];
                let next = map.len() as u32;
                let i = *map.entry((v, uv.x.to_bits(), uv.y.to_bits())).or_insert_with(|| {
                    push(v, Some(uv));
                    next
                });
                indices.push(i);
            }
        }
        (indices, map.len())
    } else {
        for v in 0..node.positions.len() as u32 {
            push(v, None);
        }
        (node.indices.to_vec(), node.positions.len())
    };
    if count == 0 {
        (min, max) = (Vec3::ZERO, Vec3::ZERO);
    }
    Vertices { positions, normals, uvs, colors, indices, count, min, max }
}

/// Triangles of `indices` grouped by polygroup, in order of first appearance.
fn by_group(indices: &[u32], groups: &[u16]) -> Vec<(u16, Vec<u32>)> {
    let mut order: Vec<(u16, Vec<u32>)> = Vec::new();
    let mut slot: HashMap<u16, usize> = HashMap::new();
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let g = groups.get(t).copied().unwrap_or(0);
        let k = *slot.entry(g).or_insert_with(|| {
            order.push((g, Vec::new()));
            order.len() - 1
        });
        order[k].1.extend_from_slice(tri);
    }
    order
}

/// Write `nodes` as one GLB to `w`. Fails on no nodes or one with no triangles.
pub fn write<W: Write>(mut w: W, nodes: &[Node], opts: &Options) -> io::Result<Summary> {
    if nodes.is_empty() {
        return Err(io::Error::other("nothing visible to export"));
    }
    let mut bin = Bin::default();
    let mut summary = Summary { nodes: nodes.len(), ..Default::default() };
    let (mut meshes, mut node_json, mut materials, mut images, mut textures) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (n, node) in nodes.iter().enumerate() {
        if node.indices.len() < 3 {
            return Err(io::Error::other(format!("{}: no triangles", node.name)));
        }
        let v = vertices(node, opts.scale);
        summary.vertices += v.count;
        summary.triangles += v.indices.len() / 3;
        let bounds = format!(",\"min\":{},\"max\":{}", list(v.min.to_array().map(num)), list(v.max.to_array().map(num)));
        let position = bin.floats(&v.positions, v.count, "VEC3", &bounds);
        let normal = bin.floats(&v.normals, v.count, "VEC3", "");
        let mut attributes = format!("\"POSITION\":{position},\"NORMAL\":{normal}");
        if !v.uvs.is_empty() {
            attributes += &format!(",\"TEXCOORD_0\":{}", bin.floats(&v.uvs, v.count, "VEC2", ""));
        }
        if !v.colors.is_empty() {
            attributes += &format!(",\"COLOR_0\":{}", bin.floats(&v.colors, v.count, "VEC4", ""));
        }
        // The material: white, rough, not metallic, with the baked maps as its textures.
        let mut texture = |png: &Option<Vec<u8>>, what: &str| -> Option<usize> {
            let png = png.as_ref()?;
            let view = bin.view(png, None);
            images.push(format!("{{\"bufferView\":{view},\"mimeType\":\"image/png\",\"name\":{}}}", quoted(&format!("{} {what}", node.name))));
            textures.push(format!("{{\"sampler\":0,\"source\":{}}}", images.len() - 1));
            Some(textures.len() - 1)
        };
        let normal_tex = texture(&node.normal_map, "normal");
        let color_tex = texture(&node.color_map, "color");
        summary.textures += normal_tex.is_some() as usize + color_tex.is_some() as usize;
        let mut pbr = String::from("\"baseColorFactor\":[1,1,1,1],\"metallicFactor\":0,\"roughnessFactor\":1");
        if let Some(t) = color_tex {
            pbr += &format!(",\"baseColorTexture\":{{\"index\":{t}}}");
        }
        let normal_ref = normal_tex.map_or(String::new(), |t| format!(",\"normalTexture\":{{\"index\":{t}}}"));
        materials.push(format!("{{\"name\":{},\"pbrMetallicRoughness\":{{{pbr}}}{normal_ref}}}", quoted(node.name)));
        let material = materials.len() - 1;
        // One primitive per polygroup, or one for the subtool.
        let parts: Vec<(Option<u16>, Vec<u32>)> = if opts.per_group && !node.groups.is_empty() {
            by_group(&v.indices, node.groups).into_iter().map(|(g, i)| (Some(g), i)).collect()
        } else {
            vec![(None, v.indices)]
        };
        let primitives: Vec<String> = parts
            .iter()
            .map(|(group, indices)| {
                let accessor = bin.indices(indices, v.count);
                let extras = group.map_or(String::new(), |g| format!(",\"extras\":{{\"polygroup\":{g}}}"));
                format!("{{\"attributes\":{{{attributes}}},\"indices\":{accessor},\"material\":{material},\"mode\":{TRIANGLES}{extras}}}")
            })
            .collect();
        summary.primitives += primitives.len();
        meshes.push(format!("{{\"name\":{},\"primitives\":{}}}", quoted(node.name), list(primitives)));
        node_json.push(format!("{{\"name\":{},\"mesh\":{n}}}", quoted(node.name)));
    }
    let mut json = format!(
        "{{\"asset\":{{\"version\":\"2.0\",\"generator\":\"Klay {}\"}},\"scene\":0,\"scenes\":[{{\"nodes\":{}}}],\"nodes\":{},\"meshes\":{},\"materials\":{},",
        env!("CARGO_PKG_VERSION"),
        list(0..nodes.len()),
        list(node_json),
        list(meshes),
        list(materials),
    );
    if !images.is_empty() {
        json += &format!(
            "\"images\":{},\"textures\":{},\"samplers\":[{{\"magFilter\":9729,\"minFilter\":9987,\"wrapS\":10497,\"wrapT\":10497}}],",
            list(images),
            list(textures)
        );
    }
    json += &format!("\"accessors\":{},\"bufferViews\":{},\"buffers\":[{{\"byteLength\":{}}}]}}", list(bin.accessors), list(bin.views), bin.data.len());
    // The chunks: JSON padded with spaces, BIN with zeros, both to four bytes.
    let mut json = json.into_bytes();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let mut data = bin.data;
    while data.len() % 4 != 0 {
        data.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + data.len();
    let total = u32::try_from(total).map_err(|_| io::Error::other("the file would be larger than 4 GB, which a GLB cannot be"))?;
    for word in [MAGIC, 2, total, json.len() as u32, JSON_CHUNK] {
        w.write_all(&word.to_le_bytes())?;
    }
    w.write_all(&json)?;
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&BIN_CHUNK.to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()?;
    summary.bytes = total as u64;
    Ok(summary)
}

/// The JSON and BIN chunks of a GLB (tests, and the app's start-up check).
#[cfg(test)]
pub fn chunks(bytes: &[u8]) -> Result<(&str, &[u8]), String> {
    let word = |at: usize| -> Result<u32, String> {
        bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("cut short at byte {at}"))
    };
    if word(0)? != MAGIC || word(4)? != 2 {
        return Err("not a glTF 2 binary".into());
    }
    if word(8)? as usize != bytes.len() {
        return Err(format!("the header says {} bytes, the file has {}", word(8)?, bytes.len()));
    }
    let (json_len, kind) = (word(12)? as usize, word(16)?);
    if kind != JSON_CHUNK {
        return Err("the first chunk is not JSON".into());
    }
    let json = std::str::from_utf8(&bytes[20..20 + json_len]).map_err(|e| e.to_string())?;
    let at = 20 + json_len;
    let (bin_len, kind) = (word(at)? as usize, word(at + 4)?);
    if kind != BIN_CHUNK {
        return Err("the second chunk is not BIN".into());
    }
    Ok((json, &bytes[at + 8..at + 8 + bin_len]))
}

/// A small JSON reader for the tests, enough to look into the files Klay writes.
#[cfg(test)]
pub mod json {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> &Value {
            match self {
                Value::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map_or(&Value::Null, |(_, v)| v),
                _ => &Value::Null,
            }
        }

        pub fn at(&self, i: usize) -> &Value {
            match self {
                Value::Arr(items) => items.get(i).unwrap_or(&Value::Null),
                _ => &Value::Null,
            }
        }

        pub fn arr(&self) -> &[Value] {
            match self {
                Value::Arr(items) => items,
                _ => &[],
            }
        }

        pub fn num(&self) -> f64 {
            match self {
                Value::Num(n) => *n,
                _ => f64::NAN,
            }
        }

        pub fn int(&self) -> usize {
            self.num() as usize
        }

        pub fn str(&self) -> &str {
            match self {
                Value::Str(s) => s,
                _ => "",
            }
        }

        pub fn is_null(&self) -> bool {
            matches!(self, Value::Null)
        }
    }

    pub fn parse(text: &str) -> Result<Value, String> {
        let mut p = Parser { s: text.as_bytes(), at: 0 };
        let v = p.value()?;
        p.skip_ws();
        if p.at != p.s.len() {
            return Err(format!("trailing text at byte {}", p.at));
        }
        Ok(v)
    }

    struct Parser<'a> {
        s: &'a [u8],
        at: usize,
    }

    impl Parser<'_> {
        fn skip_ws(&mut self) {
            while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\n' | b'\r' | b'\t') {
                self.at += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Result<(), String> {
            self.skip_ws();
            if self.s.get(self.at) == Some(&c) {
                self.at += 1;
                Ok(())
            } else {
                Err(format!("expected {:?} at byte {}", c as char, self.at))
            }
        }

        fn value(&mut self) -> Result<Value, String> {
            self.skip_ws();
            match self.s.get(self.at).copied() {
                Some(b'{') => {
                    self.at += 1;
                    let mut pairs = Vec::new();
                    self.skip_ws();
                    if self.s.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        return Ok(Value::Obj(pairs));
                    }
                    loop {
                        self.skip_ws();
                        let key = self.string()?;
                        self.eat(b':')?;
                        pairs.push((key, self.value()?));
                        self.skip_ws();
                        match self.s.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b'}') => {
                                self.at += 1;
                                return Ok(Value::Obj(pairs));
                            }
                            _ => return Err(format!("expected , or }} at byte {}", self.at)),
                        }
                    }
                }
                Some(b'[') => {
                    self.at += 1;
                    let mut items = Vec::new();
                    self.skip_ws();
                    if self.s.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(Value::Arr(items));
                    }
                    loop {
                        items.push(self.value()?);
                        self.skip_ws();
                        match self.s.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b']') => {
                                self.at += 1;
                                return Ok(Value::Arr(items));
                            }
                            _ => return Err(format!("expected , or ] at byte {}", self.at)),
                        }
                    }
                }
                Some(b'"') => Ok(Value::Str(self.string()?)),
                Some(b't') if self.s[self.at..].starts_with(b"true") => {
                    self.at += 4;
                    Ok(Value::Bool(true))
                }
                Some(b'f') if self.s[self.at..].starts_with(b"false") => {
                    self.at += 5;
                    Ok(Value::Bool(false))
                }
                Some(b'n') if self.s[self.at..].starts_with(b"null") => {
                    self.at += 4;
                    Ok(Value::Null)
                }
                Some(c) if c == b'-' || c.is_ascii_digit() => {
                    let start = self.at;
                    while self.at < self.s.len() && matches!(self.s[self.at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                        self.at += 1;
                    }
                    let text = std::str::from_utf8(&self.s[start..self.at]).unwrap();
                    text.parse().map(Value::Num).map_err(|e| format!("{text:?}: {e}"))
                }
                _ => Err(format!("unexpected byte {}", self.at)),
            }
        }

        fn string(&mut self) -> Result<String, String> {
            self.eat(b'"')?;
            let mut out = Vec::new();
            loop {
                let c = *self.s.get(self.at).ok_or("unterminated string")?;
                self.at += 1;
                match c {
                    b'"' => break,
                    b'\\' => {
                        let e = *self.s.get(self.at).ok_or("unterminated escape")?;
                        self.at += 1;
                        match e {
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'u' => {
                                let hex = std::str::from_utf8(self.s.get(self.at..self.at + 4).ok_or("short \\u escape")?).unwrap();
                                let code = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                self.at += 4;
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(char::from_u32(code).unwrap_or('?').encode_utf8(&mut buf).as_bytes());
                            }
                            other => out.push(other),
                        }
                    }
                    other => out.push(other),
                }
            }
            String::from_utf8(out).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::SculptMesh;
    use crate::primitive::{Primitive, Settings};

    fn cube() -> SculptMesh {
        let (p, i, g) = Settings { kind: Primitive::Cube, ..Default::default() }.build(1.0);
        SculptMesh::from_raw_with_groups(p, i, g)
    }

    /// A tiny PNG (1×1), enough to be an image.
    fn png() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([128, 128, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// Write `nodes` and read the file back: the parsed JSON, the BIN chunk, the summary.
    fn round_trip(nodes: &[Node], opts: &Options) -> (json::Value, Vec<u8>, Summary) {
        let mut bytes = Vec::new();
        let summary = write(&mut bytes, nodes, opts).unwrap();
        assert_eq!(summary.bytes as usize, bytes.len());
        let (text, bin) = chunks(&bytes).unwrap();
        (json::parse(text).unwrap(), bin.to_vec(), summary)
    }

    /// The accessor's byte length is what its count and type say.
    fn accessor_bytes(doc: &json::Value, i: usize) -> (usize, usize) {
        let a = doc.get("accessors").at(i);
        let per = match a.get("componentType").int() {
            5123 => 2,
            5125 | 5126 => 4,
            other => panic!("component type {other}"),
        };
        let n = match a.get("type").str() {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            other => panic!("type {other}"),
        };
        let view = doc.get("bufferViews").at(a.get("bufferView").int());
        (a.get("count").int() * per * n, view.get("byteLength").int())
    }

    #[test]
    fn a_cube_writes_positions_normals_groups_and_counts() {
        let mesh = cube();
        let node = Node::from_mesh("Cube", &mesh, &[]);
        let (doc, bin, summary) = round_trip(&[node], &Options { scale: 0.025, per_group: true });
        assert_eq!(doc.get("asset").get("version").str(), "2.0");
        assert_eq!(doc.get("nodes").at(0).get("name").str(), "Cube");
        assert_eq!(doc.get("nodes").at(0).get("mesh").int(), 0);
        assert_eq!(doc.get("scenes").at(0).get("nodes").arr().len(), 1);
        let prims = doc.get("meshes").at(0).get("primitives").arr();
        assert_eq!(prims.len(), mesh.group_count(), "a primitive per polygroup");
        assert_eq!(summary, Summary { nodes: 1, vertices: mesh.vertex_count(), triangles: mesh.triangle_count(), primitives: prims.len(), textures: 0, bytes: summary.bytes });
        // Positions in metres, their min and max set.
        let pos = doc.get("accessors").at(prims[0].get("attributes").get("POSITION").int());
        assert_eq!(pos.get("count").int(), mesh.vertex_count());
        assert!((pos.get("min").at(0).num() + 0.025).abs() < 1e-6 && (pos.get("max").at(1).num() - 0.025).abs() < 1e-6, "{pos:?}");
        // No UVs, no colours; every accessor's bytes match its view; all index accessors
        // together cover every triangle; the buffer is the BIN chunk.
        assert!(prims[0].get("attributes").get("TEXCOORD_0").is_null() && prims[0].get("attributes").get("COLOR_0").is_null());
        let mut tris = 0;
        for p in prims {
            let (expected, got) = accessor_bytes(&doc, p.get("indices").int());
            assert_eq!(expected, got);
            tris += doc.get("accessors").at(p.get("indices").int()).get("count").int() / 3;
            assert_eq!(p.get("material").int(), 0);
        }
        assert_eq!(tris, mesh.triangle_count());
        for i in 0..doc.get("accessors").arr().len() {
            let (expected, got) = accessor_bytes(&doc, i);
            assert_eq!(expected, got, "accessor {i}");
        }
        assert_eq!(doc.get("buffers").at(0).get("byteLength").int(), bin.len());
        for v in doc.get("bufferViews").arr() {
            assert_eq!(v.get("byteOffset").int() % 4, 0);
            assert!(v.get("byteOffset").int() + v.get("byteLength").int() <= bin.len());
        }
        // Normals are unit.
        let n = doc.get("accessors").at(prims[0].get("attributes").get("NORMAL").int());
        let view = doc.get("bufferViews").at(n.get("bufferView").int());
        let at = view.get("byteOffset").int();
        let f = |k: usize| f32::from_le_bytes(bin[at + 4 * k..at + 4 * k + 4].try_into().unwrap());
        for v in 0..n.get("count").int() {
            let len = (f(3 * v).powi(2) + f(3 * v + 1).powi(2) + f(3 * v + 2).powi(2)).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "vertex {v}: {len}");
        }
        // The material: white, rough, not metallic, no textures.
        let m = doc.get("materials").at(0);
        assert_eq!(m.get("pbrMetallicRoughness").get("metallicFactor").num(), 0.0);
        assert_eq!(m.get("pbrMetallicRoughness").get("roughnessFactor").num(), 1.0);
        assert!(m.get("normalTexture").is_null() && doc.get("images").is_null());
        // One primitive for the whole subtool when asked.
        let node = Node::from_mesh("Cube", &mesh, &[]);
        let (doc, _, s) = round_trip(&[node], &Options { scale: 1.0, per_group: false });
        assert_eq!((doc.get("meshes").at(0).get("primitives").arr().len(), s.primitives), (1, 1));
    }

    #[test]
    fn uvs_split_seams_colours_go_linear_and_textures_make_the_material() {
        let mesh = cube();
        // Each triangle's corners at their own UVs: every corner becomes its own vertex.
        let uvs: Vec<[Vec2; 3]> = (0..mesh.triangle_count()).map(|t| [Vec2::new(t as f32 * 0.01, 0.0), Vec2::new(t as f32 * 0.01 + 0.005, 0.0), Vec2::new(t as f32 * 0.01, 0.005)]).collect();
        let mut painted = cube();
        painted.colors = vec![[128, 0, 0, 128]; painted.vertex_count()];
        let mut node = Node::from_mesh("Painted", &painted, &uvs);
        node.normal_map = Some(png());
        let (doc, bin, summary) = round_trip(&[node], &Options { scale: 1.0, per_group: false });
        assert_eq!(summary.vertices, mesh.indices.len(), "split at every seam");
        let attributes = doc.get("meshes").at(0).get("primitives").at(0).get("attributes");
        let uv = doc.get("accessors").at(attributes.get("TEXCOORD_0").int());
        assert_eq!((uv.get("type").str(), uv.get("count").int()), ("VEC2", summary.vertices));
        // The first corner's V (0) is written flipped (1).
        let view = doc.get("bufferViews").at(uv.get("bufferView").int());
        let at = view.get("byteOffset").int();
        let f = |k: usize| f32::from_le_bytes(bin[at + 4 * k..at + 4 * k + 4].try_into().unwrap());
        assert_eq!((f(0), f(1)), (0.0, 1.0));
        // Colours: half-covered red over white is (1, 127/255, 127/255) sRGB, linear
        // (1, 0.2122, 0.2122).
        let c = doc.get("accessors").at(attributes.get("COLOR_0").int());
        assert_eq!((c.get("type").str(), c.get("count").int()), ("VEC4", summary.vertices));
        let at = doc.get("bufferViews").at(c.get("bufferView").int()).get("byteOffset").int();
        let g = |k: usize| f32::from_le_bytes(bin[at + 4 * k..at + 4 * k + 4].try_into().unwrap());
        assert!((g(0) - 1.0).abs() < 1e-6 && (g(1) - 0.2122).abs() < 1e-3 && (g(2) - 0.2122).abs() < 1e-3 && g(3) == 1.0, "{} {} {} {}", g(0), g(1), g(2), g(3));
        // The normal map is an embedded PNG the material's normalTexture points at.
        let m = doc.get("materials").at(0);
        let tex = doc.get("textures").at(m.get("normalTexture").get("index").int());
        let img = doc.get("images").at(tex.get("source").int());
        assert_eq!(img.get("mimeType").str(), "image/png");
        let view = doc.get("bufferViews").at(img.get("bufferView").int());
        let (at, len) = (view.get("byteOffset").int(), view.get("byteLength").int());
        assert_eq!(&bin[at..at + len], &png()[..]);
        assert!(m.get("pbrMetallicRoughness").get("baseColorTexture").is_null());
        assert_eq!(summary.textures, 1);
        // With a colour map too, the vertex colours stay out (they would multiply).
        let mut node = Node::from_mesh("Painted", &painted, &uvs);
        node.color_map = Some(png());
        node.normal_map = Some(png());
        let (doc, _, summary) = round_trip(&[node], &Options { scale: 1.0, per_group: false });
        let attributes = doc.get("meshes").at(0).get("primitives").at(0).get("attributes");
        assert!(attributes.get("COLOR_0").is_null() && !attributes.get("TEXCOORD_0").is_null());
        let m = doc.get("materials").at(0);
        assert_eq!(doc.get("textures").at(m.get("pbrMetallicRoughness").get("baseColorTexture").get("index").int()).get("source").int(), 1);
        assert_eq!((summary.textures, doc.get("images").arr().len(), doc.get("samplers").arr().len()), (2, 2, 1));
        // UVs of the wrong length are left out.
        let node = Node::from_mesh("Cube", &mesh, &uvs[..3]);
        let (doc, _, s) = round_trip(&[node], &Options { scale: 1.0, per_group: false });
        assert!(doc.get("meshes").at(0).get("primitives").at(0).get("attributes").get("TEXCOORD_0").is_null());
        assert_eq!(s.vertices, mesh.vertex_count());
    }

    #[test]
    fn two_nodes_wide_indices_and_odd_names() {
        let small = cube();
        let (p, i) = crate::mesh::cube_sphere(110, 1.0);
        let big = SculptMesh::from_raw(p, i);
        assert!(big.vertex_count() > u16::MAX as usize);
        let nodes = [Node::from_mesh("a \"quoted\"\\name\n", &small, &[]), Node::from_mesh("Big", &big, &[])];
        let (doc, _, summary) = round_trip(&nodes, &Options { scale: 1.0, per_group: true });
        assert_eq!(summary.nodes, 2);
        assert_eq!(doc.get("nodes").at(0).get("name").str(), "a \"quoted\"\\name\n");
        assert_eq!(doc.get("nodes").at(1).get("mesh").int(), 1);
        assert_eq!(doc.get("materials").arr().len(), 2);
        let first = doc.get("meshes").at(0).get("primitives").at(0);
        let second = doc.get("meshes").at(1).get("primitives").at(0);
        assert_eq!(doc.get("accessors").at(first.get("indices").int()).get("componentType").int(), UNSIGNED_SHORT as usize);
        assert_eq!(doc.get("accessors").at(second.get("indices").int()).get("componentType").int(), UNSIGNED_INT as usize);
        assert_eq!(second.get("material").int(), 1);
        assert_eq!(summary.vertices, small.vertex_count() + big.vertex_count());
        // Nothing to write, or a node without triangles, is an error.
        assert!(write(Vec::new(), &[], &Options { scale: 1.0, per_group: true }).is_err());
        let empty = Node { name: "x", positions: &[Vec3::ZERO], normals: vec![Vec3::Y], indices: &[], groups: &[], uvs: &[], colors: &[], normal_map: None, color_map: None };
        assert!(write(Vec::new(), &[empty], &Options { scale: 1.0, per_group: true }).is_err());
        assert!(chunks(b"hello world, not a glb at all").is_err());
    }

    #[test]
    fn the_json_reader_reads_what_the_writer_writes() {
        let v = json::parse(r#"{"a":[1,2.5,-3e2,true,false,null],"b":{"c":"x\"y\\z\n\u00e9"},"d":[]}"#).unwrap();
        assert_eq!(v.get("a").at(1).num(), 2.5);
        assert_eq!(v.get("a").at(2).num(), -300.0);
        assert_eq!(v.get("a").at(3), &json::Value::Bool(true));
        assert!(v.get("a").at(5).is_null());
        assert_eq!(v.get("b").get("c").str(), "x\"y\\z\né");
        assert!(v.get("d").arr().is_empty() && v.get("missing").is_null());
        assert!(json::parse("{\"a\":}").is_err() && json::parse("[1,2] x").is_err());
        assert_eq!(quoted("tab\there"), "\"tab\\there\"");
        assert_eq!(num(f32::NAN), "0");
    }
}
