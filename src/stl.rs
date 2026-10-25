//! STL import and export (Document › Import / Export STL, 3D Print). STL is a soup of
//! triangles with no shared vertices; reading welds corners at the same position so the
//! mesh comes back connected. Writing is binary STL to any [`Write`], so a slicer or another
//! tool can reuse [`BinaryWriter`] / [`write_binary`] directly.

use crate::obj::Imported;
use glam::Vec3;
use std::collections::HashMap;
use std::io::{self, Seek, SeekFrom, Write};

/// Binary STL: an 80-byte header, a little-endian `u32` triangle count, then 50 bytes per
/// triangle (normal, three corners, a `u16` attribute).
const HEADER: usize = 80;
const RECORD: usize = 50;

/// Writes binary STL one triangle at a time. The count goes in the header up front: pass
/// it to [`new`](Self::new), or pass 0 and fix it with [`patch_count`] once the file is
/// written (for writers that can seek).
pub struct BinaryWriter<W: Write> {
    w: W,
    written: u64,
}

impl<W: Write> BinaryWriter<W> {
    pub fn new(mut w: W, header: &str, count: u32) -> io::Result<Self> {
        let mut head = [0u8; HEADER];
        // A header that starts with "solid" makes some readers take the file for ASCII.
        let text = header.trim_start_matches("solid");
        let n = text.len().min(HEADER);
        head[..n].copy_from_slice(&text.as_bytes()[..n]);
        w.write_all(&head)?;
        w.write_all(&count.to_le_bytes())?;
        Ok(Self { w, written: 0 })
    }

    /// One triangle, its normal from the winding (counter-clockwise seen from outside).
    pub fn triangle(&mut self, [a, b, c]: [Vec3; 3]) -> io::Result<()> {
        let n = (b - a).cross(c - a).normalize_or_zero();
        let mut rec = [0u8; RECORD];
        for (k, v) in [n, a, b, c].iter().enumerate() {
            for (j, x) in v.to_array().iter().enumerate() {
                let at = 12 * k + 4 * j;
                rec[at..at + 4].copy_from_slice(&x.to_le_bytes());
            }
        }
        self.w.write_all(&rec)?;
        self.written += 1;
        Ok(())
    }

    /// Every triangle of an indexed mesh, each position passed through `place` (scale,
    /// axis swap) on the way out.
    pub fn mesh(&mut self, positions: &[Vec3], indices: &[u32], place: impl Fn(Vec3) -> Vec3) -> io::Result<()> {
        for t in indices.chunks_exact(3) {
            self.triangle([t[0], t[1], t[2]].map(|v| place(positions[v as usize])))?;
        }
        Ok(())
    }

    /// The writer back and how many triangles went in.
    pub fn finish(mut self) -> io::Result<(W, u64)> {
        self.w.flush()?;
        Ok((self.w, self.written))
    }
}

/// Put `count` in the header of the binary STL that starts at the beginning of `w`.
pub fn patch_count<W: Write + Seek>(w: &mut W, count: u64) -> io::Result<()> {
    let count = u32::try_from(count).map_err(|_| io::Error::other("more than 4 billion triangles do not fit in an STL"))?;
    let end = w.stream_position()?;
    w.seek(SeekFrom::Start(HEADER as u64))?;
    w.write_all(&count.to_le_bytes())?;
    w.seek(SeekFrom::Start(end))?;
    Ok(())
}

/// An indexed mesh as binary STL, in one call (for other tools, such as a slicer).
#[allow(dead_code)]
pub fn write_binary<W: Write>(w: W, header: &str, positions: &[Vec3], indices: &[u32], place: impl Fn(Vec3) -> Vec3) -> io::Result<W> {
    let count = u32::try_from(indices.len() / 3).map_err(|_| io::Error::other("too many triangles for an STL"))?;
    let mut out = BinaryWriter::new(w, header, count)?;
    out.mesh(positions, indices, place)?;
    Ok(out.finish()?.0)
}

/// Read binary or ASCII STL and weld it into an indexed mesh. Fails on a file with no
/// triangles or one cut short.
pub fn parse(bytes: &[u8]) -> Result<Imported, String> {
    let corners = if is_binary(bytes) { binary_corners(bytes)? } else { ascii_corners(bytes)? };
    if corners.is_empty() {
        return Err("the file has no triangles".into());
    }
    let (positions, indices, skipped) = weld(&corners);
    if indices.is_empty() {
        return Err("every triangle in the file is flat (corners at one point)".into());
    }
    let groups = vec![0; indices.len() / 3];
    Ok(Imported { positions, indices, groups, colors: Vec::new(), skipped })
}

/// Binary when the size matches the count in the header. Many binary files also start with
/// "solid", so that word alone does not mean ASCII.
fn is_binary(bytes: &[u8]) -> bool {
    if bytes.len() < HEADER + 4 {
        return false;
    }
    let count = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as u64;
    let size = (HEADER + 4) as u64 + count * RECORD as u64;
    if size == bytes.len() as u64 {
        return true;
    }
    // Not "solid ..." text: binary, maybe with bytes after the triangles.
    let text = bytes.trim_ascii_start();
    !(text.len() >= 5 && text[..5].eq_ignore_ascii_case(b"solid")) && size <= bytes.len() as u64
}

fn binary_corners(bytes: &[u8]) -> Result<Vec<[f32; 3]>, String> {
    let count = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let body = &bytes[HEADER + 4..];
    if body.len() / RECORD < count {
        return Err(format!("the file says {count} triangles but holds {}", body.len() / RECORD));
    }
    let f = |b: &[u8], at: usize| f32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let mut corners = Vec::with_capacity(count * 3);
    for rec in body.chunks_exact(RECORD).take(count) {
        for k in 1..4 {
            corners.push([f(rec, 12 * k), f(rec, 12 * k + 4), f(rec, 12 * k + 8)]);
        }
    }
    Ok(corners)
}

/// `vertex x y z` lines between `outer loop` and `endloop`; a loop of more than three
/// corners is split into a fan.
fn ascii_corners(bytes: &[u8]) -> Result<Vec<[f32; 3]>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not an STL file (neither binary nor text)".to_string())?;
    if !text.trim_start().get(..5).is_some_and(|s| s.eq_ignore_ascii_case("solid")) {
        return Err("not an STL file".into());
    }
    let mut corners = Vec::new();
    let mut lp: Vec<[f32; 3]> = Vec::with_capacity(4);
    for (line_no, line) in text.lines().enumerate() {
        let mut parts = line.split_ascii_whitespace();
        match parts.next() {
            Some("vertex") => {
                let v: Vec<f32> = parts.take(3).map_while(|s| s.parse().ok()).collect();
                let [x, y, z] = v[..] else {
                    return Err(format!("line {}: a vertex needs three numbers", line_no + 1));
                };
                lp.push([x, y, z]);
            }
            Some("endloop") => {
                for k in 1..lp.len().saturating_sub(1) {
                    corners.extend_from_slice(&[lp[0], lp[k], lp[k + 1]]);
                }
                lp.clear();
            }
            _ => {}
        }
    }
    Ok(corners)
}

/// Join corners at exactly the same position (−0 counts as 0) into shared vertices. Flat
/// triangles (two corners at one point) and repeats of a triangle are left out and counted.
pub fn weld(corners: &[[f32; 3]]) -> (Vec<Vec3>, Vec<u32>, usize) {
    let key = |p: [f32; 3]| p.map(|x| if x == 0.0 { 0u32 } else { x.to_bits() });
    let mut index: HashMap<[u32; 3], u32> = HashMap::with_capacity(corners.len() / 5);
    let mut positions = Vec::with_capacity(corners.len() / 5);
    let mut indices = Vec::with_capacity(corners.len());
    let mut seen: std::collections::HashSet<[u32; 3]> = std::collections::HashSet::with_capacity(corners.len() / 3);
    let mut skipped = 0;
    for tri in corners.chunks_exact(3) {
        let t = [0, 1, 2].map(|k| {
            let p = tri[k];
            *index.entry(key(p)).or_insert_with(|| {
                positions.push(Vec3::from_array(p));
                positions.len() as u32 - 1
            })
        });
        let mut sorted = t;
        sorted.sort_unstable();
        if sorted[0] == sorted[1] || sorted[1] == sorted[2] || !seen.insert(sorted) {
            skipped += 1;
            continue;
        }
        indices.extend_from_slice(&t);
    }
    (positions, indices, skipped)
}

/// ASCII STL of an indexed mesh (tests, and anyone who wants text).
#[allow(dead_code)]
pub fn write_ascii<W: Write>(mut w: W, name: &str, positions: &[Vec3], indices: &[u32]) -> io::Result<W> {
    writeln!(w, "solid {name}")?;
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|v| positions[v as usize]);
        let n = (b - a).cross(c - a).normalize_or_zero();
        writeln!(w, "  facet normal {:e} {:e} {:e}\n    outer loop", n.x, n.y, n.z)?;
        for p in [a, b, c] {
            writeln!(w, "      vertex {:e} {:e} {:e}", p.x, p.y, p.z)?;
        }
        writeln!(w, "    endloop\n  endfacet")?;
    }
    writeln!(w, "endsolid {name}")?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<Vec3>, Vec<u32>) {
        let (p, i, _) = crate::primitive::Settings { kind: crate::primitive::Primitive::Cube, ..Default::default() }.build(1.0);
        (p, i)
    }

    /// Edges used by exactly one triangle, and by more than two.
    fn open_and_non_manifold(indices: &[u32]) -> (usize, usize) {
        let r = crate::print::check(indices);
        (r.open_edges, r.non_manifold)
    }

    #[test]
    fn binary_round_trip_welds_back_to_the_closed_mesh() {
        let (p, i) = cube();
        assert_eq!(open_and_non_manifold(&i), (0, 0), "the primitive cube is closed");
        let bytes = write_binary(Vec::new(), "Klay cube", &p, &i, |v| v).unwrap();
        assert_eq!(bytes.len(), 84 + 50 * i.len() / 3);
        assert!(is_binary(&bytes));
        let back = parse(&bytes).unwrap();
        assert_eq!(back.positions.len(), p.len(), "every shared corner welded into one vertex");
        assert_eq!(back.indices.len(), i.len());
        assert_eq!(back.skipped, 0);
        assert_eq!(open_and_non_manifold(&back.indices), (0, 0), "still closed after the round trip");
        // Same triangles, same positions and winding.
        for (t, u) in i.chunks(3).zip(back.indices.chunks(3)) {
            for k in 0..3 {
                assert_eq!(p[t[k] as usize], back.positions[u[k] as usize]);
            }
        }
    }

    #[test]
    fn ascii_round_trip_and_a_binary_header_that_says_solid() {
        let (p, i) = cube();
        let text = write_ascii(Vec::new(), "cube", &p, &i).unwrap();
        assert!(!is_binary(&text));
        let back = parse(&text).unwrap();
        assert_eq!((back.positions.len(), back.indices.len()), (p.len(), i.len()));
        assert_eq!(open_and_non_manifold(&back.indices), (0, 0));
        // A binary file whose header starts with "solid" still reads as binary.
        let mut bytes = write_binary(Vec::new(), "x", &p, &i, |v| v).unwrap();
        bytes[..11].copy_from_slice(b"solid cube ");
        assert_eq!(parse(&bytes).unwrap().indices.len(), i.len());
    }

    #[test]
    fn welding_drops_flat_and_repeated_triangles() {
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let c = [0.0, 1.0, 0.0];
        let d = [-0.0, 1.0, 0.0]; // −0 welds with c
        let corners = [a, b, c, b, a, b, c, b, a, a, b, d, b, [1.0, 1.0, 0.0], c];
        let (pos, idx, skipped) = weld(&corners);
        assert_eq!(pos.len(), 4);
        assert_eq!(idx, vec![0, 1, 2, 1, 3, 2]);
        assert_eq!(skipped, 3, "one flat, one repeat seen from the other side, one exact repeat");
    }

    #[test]
    fn short_or_empty_files_fail() {
        let (p, i) = cube();
        let bytes = write_binary(Vec::new(), "x", &p, &i, |v| v).unwrap();
        assert!(parse(&bytes[..bytes.len() - 10]).is_err());
        assert!(parse(b"solid empty\nendsolid empty\n").is_err());
        assert!(parse(b"hello").is_err());
        assert!(parse(&write_binary(Vec::new(), "x", &p, &[], |v| v).unwrap()).is_err());
    }

    #[test]
    fn the_count_can_be_patched_after_writing() {
        let (p, i) = cube();
        let mut w = BinaryWriter::new(io::Cursor::new(Vec::new()), "patched", 0).unwrap();
        w.mesh(&p, &i, |v| v * 10.0).unwrap();
        let (mut cur, n) = w.finish().unwrap();
        patch_count(&mut cur, n).unwrap();
        let back = parse(cur.get_ref()).unwrap();
        assert_eq!(back.indices.len(), i.len());
        let size = back.positions.iter().fold(Vec3::ZERO, |m, v| m.max(v.abs()));
        assert!((size.max_element() - 10.0).abs() < 1e-4, "placed ×10 on the way out");
    }
}
