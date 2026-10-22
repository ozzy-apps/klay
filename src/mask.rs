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
}

/// The mask `op` makes of the current one.
pub fn apply(mesh: &SculptMesh, op: MaskOp) -> Vec<u8> {
    match op {
        MaskOp::Invert => inverted(mesh),
        MaskOp::Clear => Vec::new(),
        MaskOp::All => vec![255; mesh.vertex_count()],
        MaskOp::Blur => blurred(mesh, 1.0),
        MaskOp::Sharpen => sharpened(mesh, 1.0),
    }
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
    symmetry: bool,
    snapshots: MaskChunks,
    snapshotted: HashSet<u32>,
    last_dab: Option<Vec3>,
    pub dabs: u32,
    kind: BrushKind,
    assets: StampSettings,
}

impl MaskStroke {
    pub fn begin(erase: bool, symmetry: bool) -> Self {
        Self {
            erase,
            symmetry,
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
        let sel = if self.symmetry {
            mesh.select_sphere_mirror_x(hit, brush.radius)
        } else {
            mesh.select_sphere(hit, brush.radius)
        };
        if sel.is_empty() {
            return;
        }
        self.dabs += 1;
        let chunks = sel.chunks();
        let fresh: Vec<u32> = chunks.iter().copied().filter(|c| self.snapshotted.insert(*c)).collect();
        self.snapshots.extend(snapshot_chunks(mesh, &fresh));
        if mesh.mask.is_empty() {
            if self.erase {
                return;
            }
            mesh.mask = vec![0; mesh.vertex_count()];
        }
        let step = brush.strength * 255.0 * if self.erase { -1.0 } else { 1.0 }
            * if self.kind==BrushKind::MaskSoft {0.4}else{1.0};
        let r = brush.radius;
        let (u,tangent_v)=tangent(normal);
        let values = sel.par_map(|i, v| {
            let p=mesh.positions[v as usize];let p=if sel.side(i)<0.0 {Vec3::new(-p.x,p.y,p.z)}else{p};
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
