//! Exact CPU snapshots to the separate Strata process. Sculpt state stays in Klay.
use ::strata_bridge as wire;
use std::{collections::HashMap, path::PathBuf, sync::mpsc};
use wire::{
    BridgeObject, Manifest, Mesh,
    transport::{self, SendError},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Conversion {
    pub mm: f32,
    pub z_up: bool,
}
impl Conversion {
    fn matrix(self) -> [[f32; 4]; 4] {
        let m = self.mm;
        if self.z_up {
            [
                [m, 0., 0., 0.],
                [0., 0., -m, 0.],
                [0., m, 0., 0.],
                [0., 0., 0., 1.],
            ]
        } else {
            [
                [m, 0., 0., 0.],
                [0., m, 0., 0.],
                [0., 0., m, 0.],
                [0., 0., 0., 1.],
            ]
        }
    }
}
pub struct Request {
    pub ids: Vec<u64>,
    pub update_only: bool,
    pub conversion: Conversion,
}
pub enum Geometry {
    Ready(Mesh),
    Parked(crate::multires::Packed),
}
pub struct Source {
    pub id: u64,
    pub name: String,
    pub level: usize,
    pub revision: u64,
    pub conversion: Conversion,
    pub geometry: Geometry,
}
struct Completed {
    result: Result<usize, String>,
    links: Vec<(u64, Conversion)>,
}
pub struct State {
    pub document_id: String,
    pub pending: Option<Request>,
    accepted: HashMap<u64, Conversion>,
    revisions: HashMap<u64, u64>,
    running: Option<mpsc::Receiver<Completed>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            document_id: wire::new_id(),
            pending: None,
            accepted: HashMap::new(),
            revisions: HashMap::new(),
            running: None,
        }
    }
}
impl State {
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.running.is_some()
    }
    pub fn can_update(&self, ids: &[u64]) -> bool {
        !ids.is_empty() && ids.iter().all(|id| self.accepted.contains_key(id))
    }
    fn poll(&mut self) -> Option<Result<usize, String>> {
        let answer = self.running.as_ref()?.try_recv();
        match answer {
            Ok(done) => {
                self.running = None;
                if done.result.is_ok() {
                    self.accepted.extend(done.links);
                }
                Some(done.result)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.running = None;
                Some(Err("Strata transfer worker disconnected".into()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
fn mesh_snapshot(mesh: &crate::mesh::SculptMesh) -> Mesh {
    Mesh {
        positions: mesh.positions.iter().map(|p| p.to_array()).collect(),
        triangles: mesh
            .indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect(),
    }
}
pub fn package(document_id: String, sources: Vec<Source>) -> Result<(Manifest, Vec<Mesh>), String> {
    let mut objects = Vec::new();
    let mut meshes = Vec::new();
    for source in sources {
        let mesh = match source.geometry {
            Geometry::Ready(m) => m,
            Geometry::Parked(p) => {
                let doc = p.try_unpack()?;
                mesh_snapshot(doc.mesh())
            }
        };
        mesh.validate()?;
        objects.push(BridgeObject {
            object_id: format!("tool-{}", source.id),
            name: source.name,
            revision: source.revision,
            mesh_hash: mesh.hash(),
            vertex_count: mesh.positions.len(),
            triangle_count: mesh.triangles.len(),
            source_to_asset_mm: source.conversion.matrix(),
            subdivision_level: source.level as u32,
            face_policy: "all_faces".into(),
            boolean_policy: "raw_operands".into(),
        });
        meshes.push(mesh);
    }
    if objects.is_empty() {
        return Err("No subtools selected for Strata".into());
    }
    Ok((
        Manifest {
            protocol_major: 1,
            protocol_minor: 0,
            transfer_id: wire::new_id(),
            producer: format!("Klay {}", env!("CARGO_PKG_VERSION")),
            document_id,
            identity_scope: "session".into(),
            objects,
        },
        meshes,
    ))
}
fn studio_executable() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("STRATA_STUDIO_BIN") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err("STRATA_STUDIO_BIN does not name a Strata executable".into())
        };
    }
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("Strata");
    for p in [
        sibling.join("target/debug/strata-studio"),
        sibling.join("target/release/strata-studio"),
    ] {
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let p = directory.join("strata-studio");
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err("Strata executable not found; build Strata or set STRATA_STUDIO_BIN".into())
}
fn deliver(path: &std::path::Path, update_only: bool) -> Result<usize, String> {
    let directory = transport::directory()?;
    match transport::send_at(&directory, Some(path), update_only) {
        Ok(n) => return Ok(n),
        Err(SendError::Unavailable) => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut child = std::process::Command::new(studio_executable()?)
        .arg("--bridge-start")
        .spawn()
        .map_err(|e| format!("Could not open Strata: {e}"))?;
    // Reap the launched application when it eventually closes.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let start = std::time::Instant::now();
    loop {
        match transport::send_at(&directory, Some(path), update_only) {
            Ok(n) => return Ok(n),
            Err(SendError::Unavailable) if start.elapsed() < std::time::Duration::from_secs(30) => {
                std::thread::sleep(std::time::Duration::from_millis(150))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
impl crate::App {
    fn strata_ids(&self) -> Vec<u64> {
        self.subtools
            .iter()
            .enumerate()
            .filter(|(i, t)| *i == self.active_tool || (self.workspace.strata_visible && t.visible))
            .map(|(_, t)| t.id)
            .collect()
    }
    pub(crate) fn request_strata(&mut self, update_only: bool) {
        if self.strata.busy() {
            return;
        }
        let ids = self.strata_ids();
        if update_only && !self.strata.can_update(&ids) {
            self.status = "Send these subtools to Strata first".into();
            self.workspace.notify(self.status.clone());
            return;
        }
        self.strata.pending = Some(Request {
            ids,
            update_only,
            conversion: Conversion {
                mm: self.workspace.layout.print.scale,
                z_up: self.workspace.layout.print.z_up,
            },
        });
        self.status = "Send to Strata queued".into();
    }
    pub(crate) fn prepare_strata_sources(
        &mut self,
        request: &Request,
    ) -> Result<Vec<Source>, String> {
        if request.ids.len() > 128 {
            return Err("Strata bridge supports at most 128 subtools".into());
        }
        if self.workspace.live_boolean
            && self
                .subtools
                .iter()
                .filter(|t| request.ids.contains(&t.id))
                .any(|t| t.boolean != crate::boolean::BoolOp::Add)
        {
            return Err("Bake Live Boolean with Make Boolean Mesh before sending".into());
        }
        if !request.conversion.mm.is_finite()
            || request.conversion.mm <= 0.
            || request.conversion.mm > 10000.
        {
            return Err(
                "Strata requires a positive scale of at most 10000 mm per Klay unit".into(),
            );
        }
        let mut sources = vec![];
        for id in &request.ids {
            let (i, tool) = self
                .subtools
                .iter()
                .enumerate()
                .find(|(_, t)| t.id == *id)
                .ok_or("Source subtool changed before snapshot")?;
            let conversion = self
                .strata
                .accepted
                .get(id)
                .copied()
                .unwrap_or(request.conversion);
            let (level, geometry) = if i == self.active_tool {
                let m = self.doc.mesh();
                if m.positions.len() > wire::MAX_VERTICES
                    || m.indices.len() / 3 > wire::MAX_TRIANGLES
                {
                    return Err("Choose a lower shown subdivision level (bridge limit: 500,000 vertices / triangles)".into());
                }
                (self.doc.active(), Geometry::Ready(mesh_snapshot(m)))
            } else {
                let p = tool.parked.as_ref().ok_or("Missing parked subtool")?;
                let level = p.packed.active();
                if p.packed.counts()[level] > wire::MAX_VERTICES {
                    return Err(format!(
                        "{}: choose a lower shown subdivision level",
                        tool.name
                    ));
                }
                (level, Geometry::Parked(p.packed.clone()))
            };
            let revision = self.strata.revisions.entry(*id).or_default();
            *revision += 1;
            sources.push(Source {
                id: *id,
                name: tool.name.clone(),
                level,
                revision: *revision,
                conversion,
                geometry,
            });
        }
        Ok(sources)
    }
    pub(crate) fn run_strata(&mut self) {
        if let Some(result) = self.strata.poll() {
            self.status = match result {
                Ok(n) => format!("Sent {n} subtool(s) to Strata"),
                Err(e) => format!("Send to Strata failed: {e}"),
            };
            self.workspace.notify(self.status.clone());
        }
        if self.busy() || self.strata.running.is_some() {
            return;
        }
        let Some(request) = self.strata.pending.take() else {
            return;
        };
        let sources = match self.prepare_strata_sources(&request) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Send to Strata failed: {e}");
                self.workspace.notify(self.status.clone());
                return;
            }
        };
        let links = sources.iter().map(|s| (s.id, s.conversion)).collect();
        let document_id = self.strata.document_id.clone();
        let (tx, rx) = mpsc::channel();
        self.strata.running = Some(rx);
        self.status = "Sending to Strata".into();
        std::thread::spawn(move || {
            let result = (|| {
                let (manifest, meshes) = package(document_id, sources)?;
                let directory = transport::directory()?;
                let path = directory.join(format!("{}.stratabridge", manifest.transfer_id));
                wire::write_package(&path, &manifest, &meshes)?;
                let result = deliver(&path, request.update_only);
                let _ = std::fs::remove_file(path);
                result
            })();
            let _ = tx.send(Completed { result, links });
        });
    }
    pub(crate) fn strata_ui_state(&mut self) {
        self.workspace.strata_pending = self.strata.busy();
        self.workspace.strata_can_update = self.strata.can_update(&self.strata_ids());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_preserve_active_parked_levels_and_sculpt_state() {
        let mut app = crate::App::new(4, false, 64 << 20);
        app.automated = true;
        app.append_primitive();
        let ids = app.subtools.iter().map(|t| t.id).collect::<Vec<_>>();
        let active = app.active_tool;
        let before = app.doc.mesh().positions.clone();
        let history = app.history.bytes();
        let request = Request {
            ids,
            update_only: false,
            conversion: Conversion {
                mm: 25.,
                z_up: true,
            },
        };
        let sources = app.prepare_strata_sources(&request).unwrap();
        let (manifest, meshes) = package(app.strata.document_id.clone(), sources).unwrap();
        assert_eq!(manifest.objects.len(), 2);
        assert_eq!(
            meshes[active].positions,
            before.iter().map(|p| p.to_array()).collect::<Vec<_>>()
        );
        assert_eq!(app.doc.mesh().positions, before);
        assert_eq!(app.active_tool, active);
        assert_eq!(app.history.bytes(), history);
        assert!(
            app.subtools
                .iter()
                .enumerate()
                .all(|(i, t)| i == active || t.parked.is_some())
        );
    }
    #[test]
    fn link_scale_is_locked_and_new_projects_do_not_reuse_identity() {
        let mut app = crate::App::new(3, false, 32 << 20);
        app.automated = true;
        let id = app.subtools[0].id;
        let conversion = Conversion {
            mm: 10.,
            z_up: true,
        };
        app.strata.accepted.insert(id, conversion);
        let request = Request {
            ids: vec![id],
            update_only: true,
            conversion: Conversion {
                mm: 100.,
                z_up: false,
            },
        };
        let sources = app.prepare_strata_sources(&request).unwrap();
        assert_eq!(sources[0].conversion, conversion);
        assert_eq!(sources[0].revision, 1);
        let old = app.strata.document_id.clone();
        app.new_project();
        assert_ne!(app.strata.document_id, old);
        assert!(!app.strata.can_update(&[id]));
    }
}

/// Headless end-to-end client; the native Studio receiver is exercised separately.
pub fn self_test() {
    let mut settings = crate::primitive::Settings::default();
    settings.set_current(4);
    let mut app = crate::App::with_primitive(settings, false, 64 << 20);
    app.automated = true;
    app.workspace.layout.print.scale = 25.;
    app.workspace.layout.print.z_up = true;
    let id = app.subtools[0].id;
    for update_only in [false, true] {
        if update_only {
            app.doc.mesh_mut().positions[0].x += 0.1;
            app.workspace.layout.print.scale = 999.;
        }
        app.request_strata(update_only);
        let start = std::time::Instant::now();
        while app.strata.busy() {
            app.run_strata();
            assert!(start.elapsed().as_secs() < 50, "{}", app.status);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.strata.can_update(&[id]), "{}", app.status);
        assert!(!app.status.contains("failed"), "{}", app.status);
    }
    println!("Klay Send/Update client passed: two acknowledged CPU snapshots");
}
