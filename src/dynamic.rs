//! Dynamic Subdivision (ZBrush: Geometry › Dynamic Subdiv): a smooth preview of a low
//! polygon model, drawn in its place, while brushes and ZModeler keep working on the model
//! itself. Nothing is added to the model until **Apply**.
//!
//! The preview divides the model in three kinds of steps, in this order:
//! - **Flat Subdiv**: new vertices on the old triangles, the shape unchanged;
//! - **QGrid**: smooth steps with polygroup borders, creases and edges sharper than 40°
//!   held sharp, so the edges stay crisp while the faces between them settle;
//! - **Smooth Subdiv**: plain smooth steps, which round the held edges off into bevels as
//!   wide as the last QGrid step's triangles (more QGrid, narrower bevels).
//!
//! **Thickness** first turns an open surface into a closed shell that thick.

use crate::mesh::SculptMesh;
use crate::multires::{StandIn, divided};
use glam::Vec3;
use rayon::prelude::*;

/// The preview is kept under this many triangles; Smooth Subdiv steps are dropped to fit.
pub const MAX_TRIANGLES: usize = 4_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Smooth steps (0–4).
    pub smooth: u8,
    /// Flat steps before them (0–3).
    pub flat: u8,
    /// Steps with sharp edges held (0–3).
    pub qgrid: u8,
    /// Shell thickness for open surfaces, as a share of the model's size (0 for none).
    pub thickness: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { smooth: 2, flat: 0, qgrid: 0, thickness: 0.0 }
    }
}

impl Settings {
    pub fn steps(&self) -> u32 {
        (self.smooth + self.flat + self.qgrid) as u32
    }

    /// Settings that keep the preview of `mesh` under `max` triangles: Smooth Subdiv steps
    /// go first, then QGrid, then Flat.
    pub fn fitted(&self, mesh: &SculptMesh, max: usize) -> Self {
        let mut s = *self;
        let shell = if s.thickness > 0.0 && has_border(mesh) { 2.5 } else { 1.0 };
        let tris = |s: &Settings| mesh.triangle_count() as f64 * shell * 4f64.powi(s.steps() as i32);
        while tris(&s) > max as f64 && s.steps() > 0 {
            if s.smooth > 0 {
                s.smooth -= 1;
            } else if s.qgrid > 0 {
                s.qgrid -= 1;
            } else {
                s.flat -= 1;
            }
        }
        s
    }
}

fn has_border(mesh: &SculptMesh) -> bool {
    crate::crease::edges(mesh).iter().any(|e| e.3 == u32::MAX)
}

/// The model `mesh` divided as `s` says (fitted to [`MAX_TRIANGLES`] by the caller).
pub fn build(mesh: &SculptMesh, s: &Settings) -> SculptMesh {
    // Loop subdivision needs a two-manifold mesh; anything else is shown as it is.
    if mesh.subdivision_blocker().is_some() {
        return copy(mesh);
    }
    let size = {
        let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &p| (a.min(p), b.max(p)));
        (hi - lo).max_element().max(1e-6)
    };
    let mut m = if s.thickness > 0.0 && has_border(mesh) {
        let mut all = SculptMesh::from_raw_painted(mesh.positions.clone(), mesh.indices.clone(), mesh.groups.clone(), Vec::new());
        all.mask = vec![255; all.vertex_count()];
        crate::subtool_ops::extracted(&all, s.thickness * size).unwrap_or_else(|| copy(mesh))
    } else {
        copy(mesh)
    };
    for _ in 0..s.flat {
        m = divided(&m, true);
    }
    if s.qgrid > 0 {
        let mut held = m.crease_edges();
        held.extend(crate::crease::polygroup_borders(&m));
        held.extend(crate::crease::sharp_edges(&m, 40.0));
        held.sort_unstable();
        held.dedup();
        let bits = m.crease_bits(&held);
        m.set_creases(bits);
        for _ in 0..s.qgrid {
            m = divided(&m, false);
        }
    }
    // Plain smooth steps keep the model's own creases only.
    if s.qgrid > 0 {
        m.set_creases(Vec::new());
    }
    for _ in 0..s.smooth {
        m = divided(&m, false);
    }
    m
}

/// A plain copy of `mesh` (positions, triangles, polygroups, creases).
fn copy(mesh: &SculptMesh) -> SculptMesh {
    let mut m = SculptMesh::from_raw_full(mesh.positions.clone(), mesh.indices.clone(), mesh.groups.clone(), Vec::new(), mesh.quads.clone());
    if !mesh.creases.is_empty() {
        m.set_creases(mesh.creases.clone());
    }
    m
}

/// The preview as a draw-only mesh.
pub fn preview(mesh: &SculptMesh, s: &Settings) -> StandIn {
    let m = build(mesh, &s.fitted(mesh, MAX_TRIANGLES));
    let normals = (0..m.vertex_count() as u32).into_par_iter().map(|v| crate::mesh::pack_normal(m.normal_at(v))).collect();
    let mut bounds = crate::mesh::Aabb::empty();
    for &p in &m.positions {
        bounds.grow(p);
    }
    StandIn { normals, colors: Vec::new(), indices: m.visible_indices().unwrap_or_else(|| m.indices.clone()), level: 0, bounds, positions: m.positions }
}

/// **Apply**: the preview as the model's new topology, rebuilt as a fresh mesh.
pub fn applied(mesh: &SculptMesh, s: &Settings) -> SculptMesh {
    let m = build(mesh, &s.fitted(mesh, MAX_TRIANGLES));
    let mut out = SculptMesh::from_raw_full(m.positions, m.indices, m.groups, Vec::new(), m.quads);
    if !m.creases.is_empty() && m.creases.len() == out.triangle_count() {
        out.set_creases(m.creases);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> SculptMesh {
        let (p, i) = crate::primitive::box_surface([1; 3], |p| p);
        SculptMesh::from_raw(p, i)
    }

    fn max_radius(m: &SculptMesh) -> f32 {
        m.positions.iter().map(|p| p.length()).fold(0.0, f32::max)
    }

    #[test]
    fn smooth_rounds_a_cube_flat_keeps_it_and_qgrid_keeps_its_edges() {
        let c = cube();
        let corner = max_radius(&c);
        let smooth = build(&c, &Settings { smooth: 3, ..Default::default() });
        assert_eq!(smooth.triangle_count(), c.triangle_count() * 64);
        assert!(max_radius(&smooth) < corner * 0.8, "smooth rounds the corners off");
        let flat = build(&c, &Settings { smooth: 0, flat: 2, ..Default::default() });
        let half = c.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max);
        assert!(flat.positions.iter().all(|p| p.abs().max_element() > half - 1e-5), "flat stays on the cube's faces");
        // QGrid then smooth: corners rounded much less than with smooth alone.
        let q = build(&c, &Settings { smooth: 2, qgrid: 2, ..Default::default() });
        let plain = build(&c, &Settings { smooth: 2, ..Default::default() });
        assert!(max_radius(&q) - max_radius(&plain) > 0.2 * (corner - max_radius(&plain)), "{} {} {}", max_radius(&q), max_radius(&plain), corner);
        let more = build(&c, &Settings { smooth: 2, qgrid: 3, ..Default::default() });
        assert!(max_radius(&more) > max_radius(&q), "more QGrid gives narrower bevels");
    }

    #[test]
    fn thickness_closes_an_open_plane_and_budget_drops_smooth_steps() {
        let (p, i) = crate::primitive::Primitive::Plane.build(4, 1.0);
        let plane = SculptMesh::from_raw(p, i);
        let shell = build(&plane, &Settings { smooth: 1, thickness: 0.1, ..Default::default() });
        assert!(!has_border(&shell));
        let z = shell.positions.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max) - shell.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(z > 0.05, "{z}");
        let s = Settings { smooth: 4, ..Default::default() }.fitted(&plane, plane.triangle_count() * 20);
        assert_eq!(s.smooth, 2);
        let pv = preview(&cube(), &Settings::default());
        assert_eq!(pv.indices.len() / 3, cube().triangle_count() * 16);
    }
}
