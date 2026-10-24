//! Masking, ZBrush style: Ctrl + drag paints a mask on the model, Ctrl + Alt + drag erases
//! it, Ctrl + click on the model blurs it (Ctrl + Alt + click sharpens), Ctrl + click on the
//! background inverts it and Ctrl + drag on the background clears it. Masked vertices are
//! held still by every brush and by Transpose.
//!
//! The mask is one byte per vertex of the shown level ([`SculptMesh::mask`], empty when
//! nothing is masked); [`crate::multires::Multires`] carries it to another level when the
//! shown level changes.

use crate::mesh::SculptMesh;
use crate::sculpt::{Brush, BrushKind, StampSettings, focal_falloff};
use crate::brush_asset::{Pattern, tangent};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::HashSet;

/// Whole-mask operations (Masking palette and shortcuts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskOp {
    Invert,
    Clear,
    All,
    Blur,
    Sharpen,
    /// GrowMask / ShrinkMask: one ring of polygons more or less (never across hidden
    /// polygons).
    Grow,
    Shrink,
    /// Mask By Cavity: mask the recessed surface, more where it is deeper; the value is
    /// the sensitivity, 1..=100.
    Cavity(u8),
    /// Mask By Polygroups: mask the borders between polygroups, `width` rings wide (Invert
    /// masks the insides instead).
    GroupBorders(u8),
}

/// The mask `op` makes of the current one.
pub fn apply(mesh: &SculptMesh, op: MaskOp) -> Vec<u8> {
    match op {
        MaskOp::Invert => inverted(mesh),
        MaskOp::Clear => Vec::new(),
        MaskOp::All => vec![255; mesh.vertex_count()],
        MaskOp::Blur => blurred(mesh, 1.0),
        MaskOp::Sharpen => sharpened(mesh, 1.0),
        MaskOp::Grow => spread(mesh, true),
        MaskOp::Shrink => spread(mesh, false),
        MaskOp::Cavity(sensitivity) => cavity(mesh, sensitivity),
        MaskOp::GroupBorders(width) => group_borders(mesh, width),
    }
}

/// Whether `v` has a visible polygon (every vertex while nothing is hidden).
fn shown(mesh: &SculptMesh, v: u32) -> bool {
    mesh.shown.is_empty() || mesh.shown[v as usize] != 0
}

/// Grow (`grow`) or shrink the mask by one ring: each shown vertex takes the largest (or
/// smallest) value among itself and its shown neighbours, so soft edges move too.
pub fn spread(mesh: &SculptMesh, grow: bool) -> Vec<u8> {
    if mesh.mask.is_empty() {
        return Vec::new();
    }
    spread_values(mesh, &mesh.mask, grow)
}

/// [`spread`] of the values `m` (one per vertex of `mesh`).
fn spread_values(mesh: &SculptMesh, m: &[u8], grow: bool) -> Vec<u8> {
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let own = m[v as usize];
            if !shown(mesh, v) {
                return own;
            }
            let near = mesh.neighbors(v).iter().filter(|&&n| shown(mesh, n)).map(|&n| m[n as usize]);
            if grow { near.fold(own, u8::max) } else { near.fold(own, u8::min) }
        })
        .collect()
}

/// Mask By Cavity: how far each vertex sits below its neighbours, along its normal and
/// relative to their spacing, mapped through `sensitivity` (1..=100) and softened once.
/// Hidden polygons keep their mask.
pub fn cavity(mesh: &SculptMesh, sensitivity: u8) -> Vec<u8> {
    let gain = sensitivity.clamp(1, 100) as f32 / 100.0 * 16.0;
    let raw: Vec<f32> = (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let nb = mesh.neighbors(v);
            if nb.is_empty() {
                return 0.0;
            }
            let p = mesh.positions[v as usize];
            let mean = nb.iter().map(|&n| mesh.positions[n as usize]).sum::<Vec3>() / nb.len() as f32;
            let spacing = nb.iter().map(|&n| mesh.positions[n as usize].distance(p)).sum::<f32>() / nb.len() as f32;
            ((mean - p).dot(mesh.normal_at(v)) / spacing.max(1e-12) * gain).clamp(0.0, 1.0)
        })
        .collect();
    let old = &mesh.mask;
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            if !shown(mesh, v) {
                return old.get(v as usize).copied().unwrap_or(0);
            }
            let nb = mesh.neighbors(v);
            let soft = (raw[v as usize] + nb.iter().map(|&n| raw[n as usize]).sum::<f32>()) / (nb.len() + 1) as f32;
            (soft * 255.0).round() as u8
        })
        .collect()
}

/// Mask By Polygroups: the vertices where polygroups meet, grown `width - 1` rings, fully
/// masked, everything else free. Nothing is masked on a single-group mesh.
pub fn group_borders(mesh: &SculptMesh, width: u8) -> Vec<u8> {
    if mesh.groups.is_empty() {
        return Vec::new();
    }
    let border: Vec<u8> = (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let mut tris = mesh.incident(v).iter().map(|&t| mesh.groups[t as usize]);
            let first = tris.next();
            (shown(mesh, v) && first.is_some_and(|g| tris.any(|h| h != g))) as u8 * 255
        })
        .collect();
    let mut grown = border;
    for _ in 1..width.max(1) {
        grown = spread_values(mesh, &grown, true);
    }
    let old = &mesh.mask;
    grown
        .into_par_iter()
        .enumerate()
        .map(|(v, m)| if shown(mesh, v as u32) { m } else { old.get(v).copied().unwrap_or(0) })
        .collect()
}

/// Mask values of whole chunks (their owned ranges), saved for undo.
pub type MaskChunks = Vec<(u32, Vec<u8>)>;

/// What a mask undo step puts back.
pub enum MaskData {
    /// Some chunks, after a mask stroke.
    Chunks(MaskChunks),
    /// The whole mask (empty = nothing masked), after invert, clear, blur and the like.
    Whole(Vec<u8>),
}

impl MaskData {
    pub fn bytes(&self) -> usize {
        match self {
            Self::Chunks(c) => c.iter().map(|(_, v)| v.len()).sum(),
            Self::Whole(v) => v.len(),
        }
    }
}

/// How much a brush may move vertex `v`: 1 where it is free, 0 where fully masked.
#[inline]
pub fn freedom(mask: &[u8], v: u32) -> f32 {
    if mask.is_empty() {
        1.0
    } else {
        1.0 - mask[v as usize] as f32 * (1.0 / 255.0)
    }
}

/// Replace the whole mask and return the previous one. Every chunk is re-uploaded.
pub fn replace(mesh: &mut SculptMesh, mask: Vec<u8>) -> Vec<u8> {
    debug_assert!(mask.is_empty() || mask.len() == mesh.vertex_count());
    for ch in &mut mesh.chunks {
        ch.dirty_mask = true;
    }
    std::mem::replace(&mut mesh.mask, mask)
}

/// The mask with every value flipped (ZBrush: Ctrl + click on the background, Ctrl + I).
pub fn inverted(mesh: &SculptMesh) -> Vec<u8> {
    if mesh.mask.is_empty() {
        vec![255; mesh.vertex_count()]
    } else {
        mesh.mask.par_iter().map(|&m| 255 - m).collect()
    }
}

/// One pass of neighbour averaging, `amount` of the way (ZBrush: Ctrl + click on the model).
pub fn blurred(mesh: &SculptMesh, amount: f32) -> Vec<u8> {
    if mesh.mask.is_empty() {
        return Vec::new();
    }
    let m = &mesh.mask;
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            let own = m[v as usize] as f32;
            let avg = average(mesh, v).unwrap_or(own);
            (own + (avg - own) * amount).round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// The opposite of [`blurred`]: push soft values towards fully masked or fully free, which
/// narrows the soft edge of a mask and leaves hard values as they are (ZBrush: Ctrl + Alt +
/// click on the model).
pub fn sharpened(mesh: &SculptMesh, amount: f32) -> Vec<u8> {
    if mesh.mask.is_empty() {
        return Vec::new();
    }
    let k = 1.0 + amount;
    mesh.mask
        .par_iter()
        .map(|&m| ((m as f32 - 127.5) * k + 127.5).round().clamp(0.0, 255.0) as u8)
        .collect()
}

fn average(mesh: &SculptMesh, v: u32) -> Option<f32> {
    let nb = mesh.neighbors(v);
    if nb.is_empty() {
        return None;
    }
    let sum: u32 = nb.iter().map(|&n| mesh.mask[n as usize] as u32).sum();
    Some(sum as f32 / nb.len() as f32)
}

/// Mask values of the owned ranges of `chunks` (zeros when nothing is masked).
pub fn snapshot_chunks(mesh: &SculptMesh, chunks: &[u32]) -> MaskChunks {
    chunks
        .iter()
        .map(|&c| {
            let (s, e) = mesh.chunks[c as usize].range();
            let data = if mesh.mask.is_empty() { vec![0; e - s] } else { mesh.mask[s..e].to_vec() };
            (c, data)
        })
        .collect()
}

/// Put back chunk values saved by [`snapshot_chunks`] and return what they replaced.
pub fn restore_chunks(mesh: &mut SculptMesh, saved: MaskChunks) -> MaskChunks {
    let ids: Vec<u32> = saved.iter().map(|(c, _)| *c).collect();
    let inverse = snapshot_chunks(mesh, &ids);
    if mesh.mask.is_empty() {
        mesh.mask = vec![0; mesh.vertex_count()];
    }
    for (c, data) in saved {
        let ch = &mut mesh.chunks[c as usize];
        let (s, e) = ch.range();
        mesh.mask[s..e].copy_from_slice(&data);
        ch.dirty_mask = true;
    }
    inverse
}

/// A Ctrl + drag on the model: paints (or with Alt erases) the mask under the brush.
pub struct MaskStroke {
    erase: bool,
    /// Every dab is applied at each of these images of it (symmetry).
    images: crate::symmetry::Images,
    snapshots: MaskChunks,
    snapshotted: HashSet<u32>,
    last_dab: Option<Vec3>,
    pub dabs: u32,
    kind: BrushKind,
    assets: StampSettings,
}

impl MaskStroke {
    pub fn begin(erase: bool, symmetry: impl Into<crate::symmetry::Symmetry>) -> Self {
        Self {
            erase,
            images: symmetry.into().images(),
            snapshots: Vec::new(),
            snapshotted: HashSet::new(),
            last_dab: None,
            dabs: 0,
            kind: BrushKind::MaskPen,
            assets: StampSettings::default(),
        }
    }

    /// One dab at a surface hit, with the brush's radius and strength (ZBrush: MaskPen with
    /// Draw Size and Z Intensity).
    #[cfg(test)]
    pub fn dab(&mut self, mesh: &mut SculptMesh, brush: &Brush, hit: Vec3) {
        self.dab_normal(mesh,brush,hit,Vec3::Z);
    }

    pub fn with_brush(mut self,kind:BrushKind,assets:StampSettings)->Self {
        self.kind=kind;self.assets=assets;self
    }

    pub fn dab_normal(&mut self, mesh: &mut SculptMesh, brush: &Brush, hit: Vec3, normal: Vec3) {
        if let Some(last) = self.last_dab {
            if (hit - last).length() < brush.radius * 0.15 {
                return;
            }
        }
        self.last_dab = Some(hit);
        // Every symmetry copy masks a whole dab of its own, in turn (as in ZBrush), so
        // overlapping copies both apply.
        let mut any = false;
        for k in 0..self.images.len() {
            any |= self.dab_copy(mesh, brush, k, hit, normal);
        }
        if any {
            self.dabs += 1;
        }
    }

    /// One symmetry copy `k` of a dab at `hit` (the brush's own frame). Returns whether it
    /// reached any vertex.
    fn dab_copy(&mut self, mesh: &mut SculptMesh, brush: &Brush, k: usize, hit: Vec3, normal: Vec3) -> bool {
        let sel = mesh.select_sphere(self.images.map(k, hit), brush.radius);
        if sel.is_empty() {
            return false;
        }
        let chunks = sel.chunks();
        let fresh: Vec<u32> = chunks.iter().copied().filter(|c| self.snapshotted.insert(*c)).collect();
        self.snapshots.extend(snapshot_chunks(mesh, &fresh));
        if mesh.mask.is_empty() {
            if self.erase {
                return true;
            }
            mesh.mask = vec![0; mesh.vertex_count()];
        }
        let step = brush.strength * 255.0 * if self.erase { -1.0 } else { 1.0 }
            * if self.kind==BrushKind::MaskSoft {0.4}else{1.0};
        let r = brush.radius;
        let (u,tangent_v)=tangent(normal);
        let values = sel.par_map(|i, v| {
            // The alpha is placed in the brush's own frame, so every copy shows it alike.
            let p=self.images.unmap(k,mesh.positions[v as usize]);
            let delta=p-hit;
            let uv=glam::Vec2::new(delta.dot(u),-delta.dot(tangent_v))/(2.0*r)+glam::Vec2::splat(0.5);
            let alpha=if self.assets.alpha==Pattern::Off {1.0}else{self.assets.alpha.alpha(uv)};
            let f=focal_falloff(sel.dist[i],r,brush.focal_shift);
            let f=if self.kind==BrushKind::MaskSoft {f.sqrt()}else{f};
            let m = mesh.mask[v as usize] as f32 + step * f * alpha;
            m.round().clamp(0.0, 255.0) as u8
        });
        for (&v, m) in sel.verts.iter().zip(values) {
            mesh.mask[v as usize] = m;
        }
        for c in chunks {
            mesh.chunks[c as usize].dirty_mask = true;
        }
        true
    }

    /// The values the stroke replaced, if it changed anything.
    pub fn end(self) -> Option<MaskChunks> {
        if self.snapshots.is_empty() { None } else { Some(self.snapshots) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::cube_sphere;
    use crate::sculpt::BrushKind;

    fn sphere() -> SculptMesh {
        let (p, i) = cube_sphere(30, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn nearest(mesh: &SculptMesh, p: Vec3) -> usize {
        (0..mesh.vertex_count())
            .min_by(|&a, &b| (mesh.positions[a] - p).length_squared().total_cmp(&(mesh.positions[b] - p).length_squared()))
            .unwrap()
    }

    #[test]
    fn paint_erase_and_undo_a_mask_stroke() {
        let mut mesh = sphere();
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0, ..Brush::default() };
        let hit = Vec3::Z;
        let mut s = MaskStroke::begin(false, false);
        s.dab(&mut mesh, &brush, hit);
        let centre = nearest(&mesh, hit);
        let far = nearest(&mesh, -Vec3::Z);
        assert!(mesh.mask[centre] > 100 && mesh.mask[far] == 0);
        assert!(freedom(&mesh.mask, centre as u32) < 0.6);
        let saved = s.end().unwrap();
        // Undo: back to all zero; redo puts the stroke back.
        let redo = restore_chunks(&mut mesh, saved);
        assert!(mesh.mask.iter().all(|&m| m == 0));
        restore_chunks(&mut mesh, redo);
        assert!(mesh.mask[centre] > 100);
        // Ctrl + Alt erases.
        let mut e = MaskStroke::begin(true, false);
        for _ in 0..4 {
            e.last_dab = None;
            e.dab(&mut mesh, &brush, hit);
        }
        assert_eq!(mesh.mask[centre], 0);
    }

    #[test]
    fn symmetry_masks_the_mirror_image() {
        let mut mesh = sphere();
        let brush = Brush { kind: BrushKind::Draw, radius: 0.3, strength: 1.0, ..Brush::default() };
        let hit = Vec3::new(0.6, 0.2, 0.77).normalize();
        let mut s = MaskStroke::begin(false, true);
        s.dab(&mut mesh, &brush, hit);
        let a = nearest(&mesh, hit);
        let b = nearest(&mesh, Vec3::new(-hit.x, hit.y, hit.z));
        assert!(mesh.mask[a] > 100);
        assert!((mesh.mask[a] as i32 - mesh.mask[b] as i32).abs() < 30);
    }

    #[test]
    fn invert_blur_and_sharpen() {
        let mut mesh = sphere();
        let all = inverted(&mesh);
        assert!(all.iter().all(|&m| m == 255));
        // A hard edge: the +X half masked.
        let half: Vec<u8> = mesh.positions.iter().map(|p| if p.x > 0.0 { 255 } else { 0 }).collect();
        replace(&mut mesh, half.clone());
        assert!(mesh.chunks.iter().all(|c| c.dirty_mask));
        let soft = blurred(&mesh, 1.0);
        let edge = (0..mesh.vertex_count()).filter(|&v| soft[v] != half[v]).count();
        assert!(edge > 0, "blur softens the edge");
        assert!((0..mesh.vertex_count()).all(|v| mesh.positions[v].x.abs() < 0.2 || soft[v] == half[v]), "only near the edge");
        replace(&mut mesh, soft.clone());
        let hard = sharpened(&mesh, 1.0);
        let spread = |m: &[u8]| m.iter().filter(|&&x| x > 20 && x < 235).count();
        assert!(spread(&hard) < spread(&soft), "sharpen narrows the soft band");
        assert_eq!(inverted(&mesh).iter().zip(&soft).filter(|(a, b)| **a as u32 + **b as u32 != 255).count(), 0);
    }
}

#[cfg(test)]
mod brush_tool_tests {
    use super::*;
    #[test]
    fn soft_mask_has_gentler_strength_and_alpha_can_block_masking() {
        let make=||{let (p,i)=crate::mesh::cube_sphere(28,1.0);SculptMesh::from_raw(p,i)};
        let brush=Brush{radius:0.4,strength:1.0,..Brush::default()};
        let mut pen=make();let positions=pen.positions.clone();let mut soft=make();
        for (mesh,kind) in [(&mut pen,BrushKind::MaskPen),(&mut soft,BrushKind::MaskSoft)] {
            let mut stroke=MaskStroke::begin(false,false).with_brush(kind,StampSettings::default());
            stroke.dab_normal(mesh,&brush,Vec3::Z,Vec3::Z);
        }
        assert!(pen.mask.iter().max()>soft.mask.iter().max());
        assert!(soft.mask.iter().any(|&m|m>0));assert_eq!(soft.positions,positions);
        let image=crate::brush_asset::BrushImage{id:1234,name:"black".into(),source:"black.png".into(),width:2,height:2,rgba:vec![[0,0,0,255];4]};
        let mut blocked=make();let mut stroke=MaskStroke::begin(false,false).with_brush(BrushKind::MaskPen,
            StampSettings{alpha:Pattern::Image(std::sync::Arc::new(image)),..StampSettings::default()});
        stroke.dab_normal(&mut blocked,&brush,Vec3::Z,Vec3::Z);
        assert!(blocked.mask.iter().all(|&m|m==0));assert_eq!(blocked.positions,positions);
    }
}

#[cfg(test)]
mod mask_op_tests {
    use super::*;
    use crate::primitive::Primitive;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(16, 1.0);
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn grow_and_shrink_move_the_mask_one_ring_and_stop_at_hidden_polygons() {
        let mut m = sphere();
        let v = 0usize;
        let mut mask = vec![0u8; m.vertex_count()];
        mask[v] = 255;
        m.mask = mask;
        let grown = apply(&m, MaskOp::Grow);
        let ring: Vec<u32> = m.neighbors(v as u32).to_vec();
        assert!(ring.iter().all(|&n| grown[n as usize] == 255));
        assert_eq!(grown.iter().filter(|&&x| x == 255).count(), ring.len() + 1);
        m.mask = grown;
        let back = apply(&m, MaskOp::Shrink);
        assert_eq!(back.iter().filter(|&&x| x == 255).count(), 1);
        assert_eq!(back[v], 255);
        // Nothing masked: nothing to grow.
        m.mask = Vec::new();
        assert!(apply(&m, MaskOp::Grow).is_empty());
        // Hidden polygons stop the growth: hide everything around the masked vertex.
        let mut m = sphere();
        let mut mask = vec![0u8; m.vertex_count()];
        mask[v] = 255;
        m.mask = mask;
        let hidden: Vec<u8> = (0..m.triangle_count() as u32).map(|t| (!m.incident(v as u32).contains(&t)) as u8).collect();
        m.set_hidden(hidden);
        let mut grown = m.mask.clone();
        for _ in 0..4 {
            m.mask = grown;
            grown = apply(&m, MaskOp::Grow);
        }
        let reached = grown.iter().enumerate().filter(|(_, x)| **x == 255).map(|(i, _)| i as u32).collect::<Vec<_>>();
        assert!(reached.iter().all(|&r| r == v as u32 || ring.contains(&r)), "{} masked", reached.len());
    }

    #[test]
    fn mask_by_cavity_masks_grooves_not_ridges_or_flats() {
        let (p, i) = Primitive::Plane.build(80, 1.0);
        let p: Vec<Vec3> = p
            .into_iter()
            .map(|q| q + Vec3::Z * (0.03 * (-((q.y - 0.3) / 0.04).powi(2)).exp() - 0.03 * (-((q.y + 0.3) / 0.04).powi(2)).exp()))
            .collect();
        let m = SculptMesh::from_raw(p, i);
        // Chunking reorders vertices: look them up in the mesh.
        let p = &m.positions;
        let near = |y: f32| (0..p.len()).min_by(|&a, &b| (p[a].truncate() - glam::Vec2::new(0.0, y)).length().total_cmp(&(p[b].truncate() - glam::Vec2::new(0.0, y)).length())).unwrap();
        let mask = apply(&m, MaskOp::Cavity(50));
        let (groove, ridge, flat) = (mask[near(-0.3)], mask[near(0.3)], mask[near(0.0)]);
        assert!(groove > 128 && ridge == 0 && flat < 8, "groove {groove} ridge {ridge} flat {flat}");
        // More sensitivity reaches the shallower sides of the groove.
        let more = apply(&m, MaskOp::Cavity(100));
        let side = near(-0.34);
        assert!(more[side] >= mask[side]);
    }

    #[test]
    fn mask_by_polygroups_masks_the_borders_between_groups() {
        let (p, i, g) = crate::primitive::Settings { kind: Primitive::Cube, ..Default::default() }.build(1.0);
        let m = SculptMesh::from_raw_with_groups(p, i, g);
        let mask = apply(&m, MaskOp::GroupBorders(1));
        // Cube edges are group borders; face centres are not.
        for (v, q) in m.positions.iter().enumerate() {
            let on_edge = [q.x.abs(), q.y.abs(), q.z.abs()].iter().filter(|c| **c > 0.999 * m.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max)).count() >= 2;
            assert_eq!(mask[v] == 255, on_edge, "{q:?}");
        }
        let wider = apply(&m, MaskOp::GroupBorders(3));
        assert!(wider.iter().filter(|&&x| x == 255).count() > mask.iter().filter(|&&x| x == 255).count());
        // One group: no borders, no mask.
        assert!(apply(&sphere(), MaskOp::GroupBorders(2)).is_empty());
    }
}
