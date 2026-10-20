//! Project files (`.klay`): every subtool of the model, in the packed form parked subtools
//! already use (see [`crate::multires::Packed`]).
//!
//! Layout, little-endian, written as a few large blocks:
//!
//! ```text
//! magic "KLAYPRJ\0" · version u32 · subtool count u32 · active subtool u32
//! per subtool: name (u32 length + UTF-8) · visible u8 · packed stack
//! end marker "KLAYEND\0"
//! ```
//!
//! Every length is checked against the bytes left in the file before anything is
//! allocated, so a truncated or foreign file is refused with a message, never a crash. The
//! topology itself is checked when a stack is rebuilt ([`Packed::try_unpack`]).

use crate::multires::Packed;
use bytemuck::{Pod, Zeroable};
use std::io::{self, Read, Write};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"KLAYPRJ\0";
pub const END: &[u8; 8] = b"KLAYEND\0";
pub const VERSION: u32 = 1;

/// One subtool as stored in a project.
pub struct SavedTool {
    pub name: String,
    pub visible: bool,
    pub packed: Packed,
}

pub struct Project {
    pub tools: Vec<SavedTool>,
    pub active: usize,
}

/// Writer for the file layout: plain values and whole slices of plain data.
pub struct Out<W: Write> {
    w: W,
}

impl<W: Write> Out<W> {
    pub fn new(w: W) -> Self {
        Self { w }
    }

    pub fn u8(&mut self, v: u8) -> io::Result<()> {
        self.w.write_all(&[v])
    }

    pub fn u32(&mut self, v: u32) -> io::Result<()> {
        self.w.write_all(&v.to_le_bytes())
    }

    pub fn u64(&mut self, v: u64) -> io::Result<()> {
        self.w.write_all(&v.to_le_bytes())
    }

    /// Element count, then the elements as raw bytes.
    pub fn slice<T: Pod>(&mut self, s: &[T]) -> io::Result<()> {
        self.u64(s.len() as u64)?;
        self.w.write_all(bytemuck::cast_slice(s))
    }

    pub fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)
    }

    pub fn into_inner(self) -> W {
        self.w
    }
}

/// Reader for the file layout. `left` is the number of bytes the source still holds; every
/// read and every allocation is checked against it.
pub struct In<R: Read> {
    r: R,
    left: u64,
}

pub type Fail = String;

impl<R: Read> In<R> {
    pub fn new(r: R, len: u64) -> Self {
        Self { r, left: len }
    }

    fn take(&mut self, n: u64) -> Result<(), Fail> {
        if n > self.left {
            return Err("the file ends early (truncated or not a Klay project)".into());
        }
        self.left -= n;
        Ok(())
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<(), Fail> {
        self.take(buf.len() as u64)?;
        self.r.read_exact(buf).map_err(|e| format!("read failed: {e}"))
    }

    /// Fill `buf` with the next bytes, checked against what is left.
    pub fn fill_raw(&mut self, buf: &mut [u8]) -> Result<(), Fail> {
        self.fill(buf)
    }

    pub fn u8(&mut self) -> Result<u8, Fail> {
        let mut b = [0u8; 1];
        self.fill(&mut b)?;
        Ok(b[0])
    }

    pub fn u32(&mut self) -> Result<u32, Fail> {
        let mut b = [0u8; 4];
        self.fill(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn u64(&mut self) -> Result<u64, Fail> {
        let mut b = [0u8; 8];
        self.fill(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// A slice written by [`Out::slice`].
    pub fn vec<T: Pod + Zeroable>(&mut self) -> Result<Vec<T>, Fail> {
        let n = self.u64()?;
        let size = std::mem::size_of::<T>() as u64;
        let bytes = n
            .checked_mul(size)
            .filter(|&b| b <= self.left)
            .ok_or("a block is longer than the file (truncated or corrupt)")?;
        let mut v = vec![T::zeroed(); n as usize];
        let buf: &mut [u8] = bytemuck::cast_slice_mut(&mut v);
        debug_assert_eq!(buf.len() as u64, bytes);
        self.fill(buf)?;
        Ok(v)
    }

    pub fn exact(&mut self, expect: &[u8], what: &str) -> Result<(), Fail> {
        let mut b = vec![0u8; expect.len()];
        self.fill(&mut b)?;
        if b != expect {
            return Err(format!("{what} not found (not a Klay project, or corrupt)"));
        }
        Ok(())
    }

    pub fn left(&self) -> u64 {
        self.left
    }
}

/// Serialise a project into `w`.
pub fn write_to<W: Write>(w: W, tools: &[(&str, bool, &Packed)], active: usize) -> io::Result<W> {
    let mut out = Out::new(w);
    out.bytes(MAGIC)?;
    out.u32(VERSION)?;
    out.u32(tools.len() as u32)?;
    out.u32(active as u32)?;
    for (name, visible, packed) in tools {
        out.slice(name.as_bytes())?;
        out.u8(*visible as u8)?;
        packed.write_to(&mut out)?;
    }
    out.bytes(END)?;
    Ok(out.into_inner())
}

/// Write a project file next to `path` and move it into place, so a failed save never
/// leaves a half-written file where the previous one was. Returns the size in bytes.
pub fn save(path: &Path, tools: &[(&str, bool, &Packed)], active: usize) -> io::Result<u64> {
    let tmp = path.with_extension("klay-saving");
    let file = std::fs::File::create(&tmp)?;
    let w = write_to(io::BufWriter::with_capacity(8 << 20, file), tools, active)?;
    let file = w.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(std::fs::metadata(path)?.len())
}

/// Parse a project from `r` holding `len` bytes. Lengths and markers are checked here;
/// the topology is checked when each stack is rebuilt.
pub fn read_from<R: Read>(r: R, len: u64) -> Result<Project, Fail> {
    let mut input = In::new(r, len);
    input.exact(MAGIC, "Klay project header")?;
    let version = input.u32()?;
    if version != VERSION {
        return Err(format!("project version {version} is not supported (this build reads {VERSION})"));
    }
    let count = input.u32()? as usize;
    let active = input.u32()? as usize;
    if count == 0 || active >= count {
        return Err("the project lists no usable subtools".into());
    }
    let mut tools = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let name: Vec<u8> = input.vec()?;
        let name = String::from_utf8(name).map_err(|_| "a subtool name is not valid text")?;
        let visible = input.u8()? != 0;
        let packed = Packed::read_from(&mut input)?;
        tools.push(SavedTool {
            name,
            visible,
            packed,
        });
    }
    input.exact(END, "end of project marker")?;
    if input.left() != 0 {
        return Err("unexpected data after the end of the project".into());
    }
    Ok(Project { tools, active })
}

pub fn open(path: &Path) -> Result<Project, Fail> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    read_from(io::BufReader::with_capacity(8 << 20, file), len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{SculptMesh, cube_sphere};
    use crate::multires::Multires;
    use glam::Vec3;

    fn sphere(segments: u32, r: f32) -> SculptMesh {
        let (p, i) = cube_sphere(segments, r);
        SculptMesh::from_raw(p, i)
    }

    /// Move the vertex nearest `p` by `d` at the active level, as a stroke would.
    fn nudge(mr: &mut Multires, p: Vec3, d: Vec3) {
        let level = mr.active();
        let mesh = mr.mesh_mut();
        let v = (0..mesh.vertex_count())
            .min_by(|&a, &b| (mesh.positions[a] - p).length_squared().total_cmp(&(mesh.positions[b] - p).length_squared()))
            .unwrap();
        let c = mesh.vert_chunk[v];
        let snapshot = mesh.snapshot_chunks(&[c]);
        mesh.positions[v] += d;
        mesh.finish_edit(&[c]);
        mr.finish_stroke(level, snapshot);
    }

    fn sample_stacks() -> (Multires, Multires) {
        let mut a = Multires::new(sphere(10, 1.0), usize::MAX);
        nudge(&mut a, Vec3::X, Vec3::X * 0.2);
        assert!(a.divide());
        nudge(&mut a, Vec3::Y, Vec3::Y * 0.1);
        assert!(a.divide());
        nudge(&mut a, Vec3::Z, Vec3::Z * 0.05);
        a.set_active(1);
        let b = Multires::new(sphere(6, 0.5), 1234);
        (a, b)
    }

    fn bytes_of(tools: &[(&str, bool, &Packed)], active: usize) -> Vec<u8> {
        write_to(Vec::new(), tools, active).unwrap()
    }

    #[test]
    fn projects_round_trip_byte_for_byte_and_rebuild_the_same_levels() {
        let (a, b) = sample_stacks();
        let (pa, pb) = (a.pack_copy(), b.pack_copy());
        let bytes = bytes_of(&[("Body", true, &pa), ("Eye", false, &pb)], 0);
        let project = read_from(&bytes[..], bytes.len() as u64).expect("reads back");
        assert_eq!(project.active, 0);
        assert_eq!(project.tools.len(), 2);
        assert_eq!((project.tools[0].name.as_str(), project.tools[0].visible), ("Body", true));
        assert_eq!((project.tools[1].name.as_str(), project.tools[1].visible), ("Eye", false));
        let again: Vec<(&str, bool, &Packed)> =
            project.tools.iter().map(|t| (t.name.as_str(), t.visible, &t.packed)).collect();
        assert_eq!(bytes_of(&again, 0), bytes, "the format is deterministic and lossless");

        let back = project.tools.into_iter().next().unwrap().packed.try_unpack().expect("valid");
        let reference = pa.unpack();
        assert_eq!(back.count(), 3);
        assert_eq!(back.active(), 1);
        for l in 0..3 {
            assert_eq!(back.level_mesh(l).positions, reference.level_mesh(l).positions, "level {l}");
            assert_eq!(back.level_mesh(l).indices, reference.level_mesh(l).indices);
        }
    }

    #[test]
    fn truncated_files_are_refused() {
        let (a, b) = sample_stacks();
        let (pa, pb) = (a.pack_copy(), b.pack_copy());
        let bytes = bytes_of(&[("Body", true, &pa), ("Eye", true, &pb)], 1);
        for cut in [0, 4, 8, 12, 20, 100, bytes.len() / 3, bytes.len() / 2, bytes.len() - 9, bytes.len() - 1] {
            let r = read_from(&bytes[..cut], cut as u64);
            assert!(r.is_err(), "a file cut at {cut} of {} bytes was accepted", bytes.len());
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(read_from(&longer[..], longer.len() as u64).is_err());
        assert!(read_from(&b"PK\x03\x04 not a project"[..], 21).is_err());
    }

    #[test]
    fn corrupt_files_never_crash() {
        let (a, b) = sample_stacks();
        let (pa, pb) = (a.pack_copy(), b.pack_copy());
        let bytes = bytes_of(&[("Body", true, &pa), ("Eye", true, &pb)], 0);
        // Silence the panic messages that the rebuild guard turns into errors.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut refused = 0;
        for _ in 0..300 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut bad = bytes.clone();
            let at = (state % bad.len() as u64) as usize;
            bad[at] ^= (state >> 32) as u8 | 1;
            match read_from(&bad[..], bad.len() as u64) {
                Err(_) => refused += 1,
                Ok(p) => {
                    for t in p.tools {
                        if t.packed.try_unpack().is_err() {
                            refused += 1;
                        }
                    }
                }
            }
        }
        std::panic::set_hook(hook);
        // Most flips land in positions or offsets, which are any float; the rest must be
        // caught, and nothing may panic out of here.
        assert!(refused > 0);
    }

    #[test]
    fn targeted_corruptions_are_reported() {
        let mr = Multires::new(sphere(6, 1.0), 99);
        let p = mr.pack_copy();
        let bytes = bytes_of(&[("A", true, &p)], 0);
        // Header (8 + 4 + 4 + 4), name (8 + 1), visible (1), active (4), budget (8), then
        // the positions block: count (8) + data, then the index block.
        let n_pos = mr.mesh().vertex_count();
        let idx_data = 8 + 4 + 4 + 4 + 8 + 1 + 1 + 4 + 8 + 8 + n_pos * 12 + 8;
        let mut bad = bytes.clone();
        bad[idx_data..idx_data + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let project = read_from(&bad[..], bad.len() as u64).expect("sizes are still fine");
        let err = project.tools.into_iter().next().unwrap().packed.try_unpack().err();
        assert!(err.is_some_and(|e| e.contains("missing vertices")));
        let mut wrong_version = bytes.clone();
        wrong_version[8] = 9;
        assert!(read_from(&wrong_version[..], bytes.len() as u64).is_err_and(|e| e.contains("version")));
    }
}
