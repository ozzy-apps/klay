//! Shared immutable CPU transfer contract; no graphics, windowing or sculpt dependencies.
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};
mod storage;
pub mod transport;
pub const MAX_VERTICES: usize = 500_000;
pub const MAX_TRIANGLES: usize = 500_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
impl Mesh {
    pub fn validate(&self) -> Result<(), String> {
        if self.positions.is_empty() || self.triangles.is_empty() {
            return Err("Mesh contains no triangles".into());
        }
        if self.positions.len() > MAX_VERTICES || self.triangles.len() > MAX_TRIANGLES {
            return Err("Mesh exceeds bridge limit (500,000 vertices / triangles); choose a lower shown subdivision level".into());
        }
        if self
            .positions
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err("Invalid mesh coordinates".into());
        }
        for t in &self.triangles {
            if t.iter().any(|i| *i as usize >= self.positions.len()) {
                return Err("Triangle index outside vertex array".into());
            }
            let [a, b, c] = t.map(|i| self.positions[i as usize]);
            let u = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
            let v = std::array::from_fn::<_, 3, _>(|i| c[i] - a[i]);
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            if n.iter().map(|v| v * v).sum::<f32>() < 1e-16 {
                return Err("Mesh contains degenerate triangles".into());
            }
        }
        Ok(())
    }
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"STRATA-MESH-v1\0");
        h.update((self.positions.len() as u64).to_le_bytes());
        h.update((self.triangles.len() as u64).to_le_bytes());
        for v in self.positions.iter().flatten() {
            h.update(v.to_le_bytes());
        }
        for i in self.triangles.iter().flatten() {
            h.update(i.to_le_bytes());
        }
        format!("{:x}", h.finalize())
    }
}
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub transfer_id: String,
    pub producer: String,
    pub document_id: String,
    pub identity_scope: String,
    pub objects: Vec<BridgeObject>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeObject {
    pub object_id: String,
    pub name: String,
    pub revision: u64,
    pub mesh_hash: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub source_to_asset_mm: [[f32; 4]; 4],
    pub subdivision_level: u32,
    pub face_policy: String,
    pub boolean_policy: String,
}
#[derive(Debug)]
pub struct Package {
    pub manifest: Manifest,
    pub meshes: Vec<Mesh>,
}
pub fn write_package(path: &Path, manifest: &Manifest, meshes: &[Mesh]) -> Result<(), String> {
    if meshes.len() != manifest.objects.len() {
        return Err("Bridge object count mismatch".into());
    }
    for (o, m) in manifest.objects.iter().zip(meshes) {
        m.validate()?;
        if m.hash() != o.mesh_hash
            || m.positions.len() != o.vertex_count
            || m.triangles.len() != o.triangle_count
        {
            return Err("Bridge geometry metadata mismatch".into());
        }
    }
    storage::atomic_write(path, |f| {
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("manifest.json", opts)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut z, manifest).map_err(|e| e.to_string())?;
        let mut written = std::collections::HashSet::new();
        for (o, m) in manifest.objects.iter().zip(meshes) {
            if !written.insert(&o.mesh_hash) {
                continue;
            }
            z.start_file(format!("meshes/{}.positions.bin", o.mesh_hash), opts)
                .map_err(|e| e.to_string())?;
            for v in m.positions.iter().flatten() {
                z.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
            }
            z.start_file(format!("meshes/{}.triangles.bin", o.mesh_hash), opts)
                .map_err(|e| e.to_string())?;
            for i in m.triangles.iter().flatten() {
                z.write_all(&i.to_le_bytes()).map_err(|e| e.to_string())?;
            }
        }
        z.finish().map_err(|e| e.to_string())?;
        Ok(())
    })
}
pub fn read_package(path: &Path) -> Result<Package, String> {
    let mut z = storage::open_archive(path)?;
    let bytes = storage::read_entry(&mut z, "manifest.json", 1024 * 1024)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if manifest.protocol_major != 1 {
        return Err("Unsupported bridge protocol major version".into());
    }
    if !["session", "persistent"].contains(&manifest.identity_scope.as_str())
        || manifest.document_id.is_empty()
        || manifest.transfer_id.is_empty()
    {
        return Err("Invalid bridge identity".into());
    }
    if manifest.objects.is_empty() || manifest.objects.len() > 128 {
        return Err("Bridge requires 1–128 objects".into());
    }
    let mut ids = std::collections::HashSet::new();
    let mut meshes = vec![];
    for o in &manifest.objects {
        if o.object_id.is_empty() || !ids.insert(o.object_id.clone()) {
            return Err("Duplicate or missing source object identity".into());
        }
        if o.face_policy != "all_faces" || o.boolean_policy != "raw_operands" {
            return Err("Prototype accepts all_faces / raw_operands only".into());
        }
        if o.mesh_hash.len() != 64
            || !o.mesh_hash.bytes().all(|c| c.is_ascii_hexdigit())
            || o.vertex_count > MAX_VERTICES
            || o.triangle_count > MAX_TRIANGLES
        {
            return Err("Invalid bridge mesh metadata".into());
        }
        let a = &o.source_to_asset_mm;
        if a.iter().flatten().any(|v| !v.is_finite()) || a[3] != [0., 0., 0., 1.] {
            return Err("Invalid source transform".into());
        }
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if det.abs() < 1e-12 {
            return Err("Singular source transform".into());
        }
        let p = storage::read_entry(
            &mut z,
            &format!("meshes/{}.positions.bin", o.mesh_hash),
            (o.vertex_count * 12) as u64,
        )?;
        let t = storage::read_entry(
            &mut z,
            &format!("meshes/{}.triangles.bin", o.mesh_hash),
            (o.triangle_count * 12) as u64,
        )?;
        if p.len() != o.vertex_count * 12 || t.len() != o.triangle_count * 12 {
            return Err("Bridge mesh byte length mismatch".into());
        }
        let mesh = Mesh {
            positions: p
                .chunks_exact(12)
                .map(|v| {
                    std::array::from_fn(|i| {
                        f32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap())
                    })
                })
                .collect(),
            triangles: t
                .chunks_exact(12)
                .map(|v| {
                    std::array::from_fn(|i| {
                        u32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap())
                    })
                })
                .collect(),
        };
        mesh.validate()?;
        if mesh.hash() != o.mesh_hash {
            return Err("Bridge mesh hash mismatch".into());
        }
        meshes.push(mesh);
    }
    Ok(Package { manifest, meshes })
}
