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
    /// Mask By Cavity: mask the recessed surface, more where it is deeper (or the raised
    /// surface with a negative intensity).
    Cavity(Cavity),
    /// Mask By Polygroups: mask the borders between polygroups, `width` rings wide (Invert
    /// masks the insides instead).
    GroupBorders(u8),
    /// Mask By Smoothness: mask the smooth surface and leave the curvature changes (creases,
    /// edges, the rim of a form) free, as the Transpose Smart mask does.
    Smoothness(Smoothness),
    /// Mask Ambient Occlusion: mask the surface other surface shuts in, more where more of
    /// the sky above it is covered.
    Occlusion(Occlusion),
}

/// Mask By Smoothness settings, as ZBrush's Masking › Mask By Smoothness sliders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Smoothness {
    /// 1..=100: how many rings of polygons round a curvature change stay free.
    pub range: u8,
    /// 0..=100: how much of that range is a soft edge: 0 a hard edge `range` rings out,
    /// 100 a ramp from the curvature change to full mask at `range`.
    pub falloff: u8,
}

impl Default for Smoothness {
    fn default() -> Self {
        Self { range: 10, falloff: 50 }
    }
}

/// Mask Ambient Occlusion settings, as ZBrush's Masking › Mask Ambient Occlusion sliders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Occlusion {
    /// Occlusion Intensity, 0..=100: the mask a vertex gets when every ray is blocked.
    pub intensity: u8,
    /// AO ScanDist, 1..=100: how far a ray looks for a blocker, as a percentage of the
    /// subtool's longest side.
    pub scan_dist: u8,
    /// AO Aperture, 1..=180: the angle of the cone of rays about the normal (180 the whole
    /// hemisphere above the surface).
    pub aperture: u8,
}

impl Default for Occlusion {
    fn default() -> Self {
        Self { intensity: 100, scan_dist: 50, aperture: 180 }
    }
}

/// Rays per vertex of [`occlusion`].
pub const AO_RAYS: usize = 16;

/// Mask By Cavity settings, as ZBrush's Masking › Mask By Cavity sliders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cavity {
    /// -100..=100: how strongly a recess masks (100 masks even shallow ones, 10 only deep
    /// creases); negative values mask the raised surface, ridges and corners, instead.
    pub intensity: i8,
    /// 1..=100: how wide a recess may be and still count. 1 finds only the creases between
    /// neighbouring polygons (fine wrinkles, pores on a dense mesh); more smooths the
    /// surface further out before comparing, so wide grooves and the folds of a form are
    /// found too, as if seen at a lower subdivision level.
    pub depth: u8,
    /// 0..=100: softens the mask's edges, up to ten rings of polygons.
    pub blur: u8,
}

impl Default for Cavity {
    fn default() -> Self {
        Self { intensity: 50, depth: 1, blur: 0 }
    }
}

#[cfg(test)]
impl Cavity {
    /// The default settings at `intensity`.
    pub fn with_intensity(intensity: i8) -> Self {
        Self { intensity, ..Self::default() }
    }
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
        MaskOp::Cavity(settings) => cavity(mesh, settings),
        MaskOp::GroupBorders(width) => group_borders(mesh, width),
        MaskOp::Smoothness(settings) => smoothness(mesh, settings),
        MaskOp::Occlusion(settings) => occlusion(mesh, settings),
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

/// Mask By Cavity: how far each vertex sits below the surface smoothed `depth` rings out
/// (its neighbours' mean at depth 1), along its normal and relative to the polygon spacing
/// at that scale, mapped through the intensity (its sign choosing recesses or ridges),
/// softened once and then `blur` more. Hidden polygons keep their mask.
pub fn cavity(mesh: &SculptMesh, settings: Cavity) -> Vec<u8> {
    let Cavity { intensity, depth, blur } = settings;
    let intensity = intensity.clamp(-100, 100) as f32 / 100.0;
    let gain = intensity.abs() * 16.0;
    let sign = if intensity < 0.0 { -1.0 } else { 1.0 };
    let depth = depth.clamp(1, 100) as usize;
    let n = mesh.vertex_count() as u32;
    // The surface smoothed `depth` times: each pass replaces a vertex by the mean of its
    // neighbours, so the smoothing reaches about sqrt(depth) rings out.
    let mut smooth = mesh.positions.clone();
    for _ in 0..depth {
        smooth = (0..n)
            .into_par_iter()
            .map(|v| {
                let nb = mesh.neighbors(v);
                if nb.is_empty() {
                    return smooth[v as usize];
                }
                nb.iter().map(|&m| smooth[m as usize]).sum::<Vec3>() / nb.len() as f32
            })
            .collect();
    }
    let scale = (depth as f32).sqrt();
    let raw: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|v| {
            let nb = mesh.neighbors(v);
            if nb.is_empty() {
                return 0.0;
            }
            let p = mesh.positions[v as usize];
            let spacing = nb.iter().map(|&m| mesh.positions[m as usize].distance(p)).sum::<f32>() / nb.len() as f32;
            (sign * (smooth[v as usize] - p).dot(mesh.normal_at(v)) / (spacing * scale).max(1e-12) * gain).clamp(0.0, 1.0)
        })
        .collect();
    let soften = |raw: &[f32]| -> Vec<f32> {
        (0..n)
            .into_par_iter()
            .map(|v| {
                let nb = mesh.neighbors(v);
                (raw[v as usize] + nb.iter().map(|&m| raw[m as usize]).sum::<f32>()) / (nb.len() + 1) as f32
            })
            .collect()
    };
    let mut soft = soften(&raw);
    for _ in 0..(blur.min(100) as f32 / 10.0).round() as usize {
        soft = soften(&soft);
    }
    let old = &mesh.mask;
    (0..n)
        .into_par_iter()
        .map(|v| {
            if !shown(mesh, v) {
                return old.get(v as usize).copied().unwrap_or(0);
            }
            (soft[v as usize] * 255.0).round() as u8
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

/// Below this angle between a vertex's normal and a neighbour's the surface counts as
/// smooth for [`smoothness`]; at [`EDGE_DEG`] and beyond it is a full curvature change.
const SMOOTH_DEG: f32 = 8.0;
const EDGE_DEG: f32 = 20.0;

/// Mask By Smoothness: every shown vertex is free in proportion to the widest angle between
/// its normal and a shown neighbour's (smooth below [`SMOOTH_DEG`], a full edge at
/// [`EDGE_DEG`]), that freedom is spread `range` rings out, falling to nothing over the
/// outer `falloff` share of them, and the mask is what is left. Hidden polygons neither
/// spread nor take the mask; their vertices keep theirs.
pub fn smoothness(mesh: &SculptMesh, settings: Smoothness) -> Vec<u8> {
    let Smoothness { range, falloff } = settings;
    let range = range.clamp(1, 100) as usize;
    let falloff = falloff.min(100) as f32 / 100.0;
    let n = mesh.vertex_count() as u32;
    let normals: Vec<Vec3> = (0..n).into_par_iter().map(|v| mesh.normal_at(v)).collect();
    let mut free: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|v| {
            if !shown(mesh, v) {
                return 0.0;
            }
            let nv = normals[v as usize];
            let widest = mesh
                .neighbors(v)
                .iter()
                .filter(|&&m| shown(mesh, m))
                .map(|&m| nv.dot(normals[m as usize]).clamp(-1.0, 1.0).acos().to_degrees())
                .fold(0.0, f32::max);
            ((widest - SMOOTH_DEG) / (EDGE_DEG - SMOOTH_DEG)).clamp(0.0, 1.0)
        })
        .collect();
    // Each ring out costs nothing over the plateau and a share of the ramp after it; taking
    // the best neighbour less this ring's cost every pass gives each vertex the freedom of
    // the nearest curvature change less the cost of the rings between.
    let plateau = range as f32 * (1.0 - falloff);
    let ramp = range as f32 * falloff;
    let cost = |d: f32| if ramp > 0.0 { ((d - plateau) / ramp).clamp(0.0, 1.0) } else { 0.0 };
    for k in 1..=range {
        let step = cost(k as f32) - cost(k as f32 - 1.0);
        if step >= 1.0 {
            break;
        }
        free = (0..n)
            .into_par_iter()
            .map(|v| {
                let own = free[v as usize];
                if !shown(mesh, v) {
                    return own;
                }
                mesh.neighbors(v).iter().filter(|&&m| shown(mesh, m)).map(|&m| free[m as usize] - step).fold(own, f32::max)
            })
            .collect();
    }
    let old = &mesh.mask;
    (0..n)
        .into_par_iter()
        .map(|v| {
            if !shown(mesh, v) {
                return old.get(v as usize).copied().unwrap_or(0);
            }
            ((1.0 - free[v as usize]) * 255.0).round() as u8
        })
        .collect()
}

/// Mask Ambient Occlusion: from each shown vertex, [`AO_RAYS`] rays leave just above the
/// surface in a cone of `aperture` degrees about the normal (a Fibonacci spiral over the
/// cone, turned by a different angle at every vertex), each looking `scan_dist` percent
/// of the subtool's longest side for a shown triangle; the share of them that find one,
/// weighted by how close to the normal they went, times the intensity, is the mask. Hidden
/// polygons neither block rays nor take the mask; their vertices keep theirs.
pub fn occlusion(mesh: &SculptMesh, settings: Occlusion) -> Vec<u8> {
    let Occlusion { intensity, scan_dist, aperture } = settings;
    let gain = intensity.min(100) as f32 / 100.0;
    let bounds = mesh.bounds();
    let size = (bounds.max - bounds.min).max_element();
    let reach = size * scan_dist.clamp(1, 100) as f32 / 100.0;
    let cos_min = (aperture.clamp(1, 180) as f32 / 2.0).to_radians().cos();
    let scene = crate::project_all::Sources::new(&[mesh]);
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    let old = &mesh.mask;
    (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| {
            if !shown(mesh, v) {
                return old.get(v as usize).copied().unwrap_or(0);
            }
            let p = mesh.positions[v as usize];
            let nb = mesh.neighbors(v);
            let normal = mesh.normal_at(v);
            if nb.is_empty() || normal == Vec3::ZERO || reach <= 0.0 {
                return 0;
            }
            let spacing = nb.iter().map(|&m| mesh.positions[m as usize].distance(p)).sum::<f32>() / nb.len() as f32;
            let origin = p + normal * (spacing * 0.05);
            let (tu, tv) = tangent(normal);
            let phase = (v as f32 * 0.618_034).fract() * std::f32::consts::TAU;
            let (mut blocked, mut total) = (0.0, 0.0);
            for i in 0..AO_RAYS {
                let cos_t = 1.0 - (i as f32 + 0.5) / AO_RAYS as f32 * (1.0 - cos_min);
                let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
                let phi = phase + i as f32 * golden;
                let d = normal * cos_t + tu * (sin_t * phi.cos()) + tv * (sin_t * phi.sin());
                total += cos_t;
                if scene.hit(origin, d, reach).is_some() {
                    blocked += cos_t;
                }
            }
            let occluded = if total > 0.0 { blocked / total } else { 0.0 };
            (occluded * gain * 255.0).round().clamp(0.0, 255.0) as u8
        })
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
    /// Direction from the dab before to this one (Alpha rotation: Follow).
    heading: Option<Vec3>,
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
            heading: None,
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
        self.heading = self.last_dab.map(|last| (hit - last).normalize_or_zero()).filter(|d| *d != Vec3::ZERO).or(self.heading);
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
        let sel = mesh.select_sphere(self.images.map_point(k, hit), brush.radius);
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
        // Stroke › Alpha rotation (Follow uses the last dab's direction).
        let base = tangent(normal);
        let fixed = self.assets.angle.to_radians();
        let angle = match self.assets.turn {
            crate::sculpt::AlphaTurn::Fixed => fixed,
            crate::sculpt::AlphaTurn::Follow => self.heading.map_or(fixed, |d| fixed + d.dot(base.0).atan2(d.dot(base.1))),
            crate::sculpt::AlphaTurn::Random => fixed + crate::sculpt::dab_turn(self.dabs, hit),
        };
        let (u,tangent_v)=crate::sculpt::turned(base, angle);
        let values = sel.par_map(|i, v| {
            // The alpha is placed in the brush's own frame, so every copy shows it alike.
            let p=self.images.unmap_point(k,mesh.positions[v as usize]);
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
        let image=crate::brush_asset::BrushImage{id:1234,name:"black".into(),source:Some("black.png".into()),width:2,height:2,rgba:vec![[0,0,0,255];4]};
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
        let mask = apply(&m, MaskOp::Cavity(Cavity::with_intensity(50)));
        let (groove, ridge, flat) = (mask[near(-0.3)], mask[near(0.3)], mask[near(0.0)]);
        assert!(groove > 128 && ridge == 0 && flat < 8, "groove {groove} ridge {ridge} flat {flat}");
        // More sensitivity reaches the shallower sides of the groove.
        let more = apply(&m, MaskOp::Cavity(Cavity::with_intensity(100)));
        let side = near(-0.34);
        assert!(more[side] >= mask[side]);
        // A negative intensity masks the ridge instead of the groove.
        let ridges = apply(&m, MaskOp::Cavity(Cavity::with_intensity(-50)));
        let (groove, ridge) = (ridges[near(-0.3)], ridges[near(0.3)]);
        assert!(ridge > 128 && groove == 0, "groove {groove} ridge {ridge}");
        // Blur spreads the mask onto the flat beside the groove without losing its middle.
        let blurred = apply(&m, MaskOp::Cavity(Cavity { intensity: 50, depth: 1, blur: 100 }));
        let beside = near(-0.2);
        assert!(blurred[beside] > mask[beside] && blurred[near(-0.3)] > 64, "beside {} was {} · middle {}", blurred[beside], mask[beside], blurred[near(-0.3)]);
    }

    #[test]
    fn cavity_depth_finds_wide_grooves_that_one_ring_misses() {
        // A shallow groove fifty polygons wide (a fold, a grime channel on a dense mesh)
        // and a ridge like it.
        let (p, i) = Primitive::Plane.build(160, 1.0);
        let p: Vec<Vec3> = p
            .into_iter()
            .map(|q| q + Vec3::Z * (0.03 * (-((q.y - 0.3) / 0.15).powi(2)).exp() - 0.03 * (-((q.y + 0.3) / 0.15).powi(2)).exp()))
            .collect();
        let m = SculptMesh::from_raw(p, i);
        let p = &m.positions;
        let near = |y: f32| (0..p.len()).min_by(|&a, &b| (p[a].truncate() - glam::Vec2::new(0.0, y)).length().total_cmp(&(p[b].truncate() - glam::Vec2::new(0.0, y)).length())).unwrap();
        let shallow = apply(&m, MaskOp::Cavity(Cavity { intensity: 100, depth: 1, blur: 0 }));
        let deep = apply(&m, MaskOp::Cavity(Cavity { intensity: 100, depth: 100, blur: 0 }));
        let (s, d) = (shallow[near(-0.3)], deep[near(-0.3)]);
        assert!(s < 40 && d > 60 && d > 3 * s, "one ring {s} · depth 100 {d}");
        assert_eq!(deep[near(0.3)], 0, "the ridge stays free");
        assert!(deep[near(0.0)] < 16, "the flat between stays free: {}", deep[near(0.0)]);
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

    /// A plane with a steep bump in its middle, and the vertex nearest (x, y).
    fn bumped_plane() -> (SculptMesh, impl Fn(&SculptMesh, f32, f32) -> usize) {
        // Sixty polygons across one unit, so a ring of polygons is 1/60.
        let (p, i) = Primitive::Plane.build(60, 0.5);
        let p: Vec<Vec3> = p.into_iter().map(|q| q + Vec3::Z * (0.1 * (-(q.truncate().length() / 0.05).powi(2)).exp())).collect();
        let near = |m: &SculptMesh, x: f32, y: f32| {
            let p = &m.positions;
            (0..p.len()).min_by(|&a, &b| (p[a].truncate() - glam::Vec2::new(x, y)).length().total_cmp(&(p[b].truncate() - glam::Vec2::new(x, y)).length())).unwrap()
        };
        (SculptMesh::from_raw(p, i), near)
    }

    #[test]
    fn mask_by_smoothness_frees_the_bump_and_masks_the_flat() {
        let (m, near) = bumped_plane();
        let hard = apply(&m, MaskOp::Smoothness(Smoothness { range: 2, falloff: 0 }));
        let (rim, flat, far) = (hard[near(&m, 0.04, 0.0)], hard[near(&m, 0.3, 0.0)], hard[near(&m, 0.0, 0.4)]);
        assert!(rim < 128 && flat == 255 && far == 255, "rim {rim} flat {flat} far {far}");
        // Range spreads the free band out from the bump, Falloff softens its edge.
        let wide = apply(&m, MaskOp::Smoothness(Smoothness { range: 12, falloff: 0 }));
        let beside = near(&m, 0.2, 0.0);
        assert!(wide[beside] < 128 && hard[beside] == 255 && wide[near(&m, 0.4, 0.0)] == 255, "beside {}", wide[beside]);
        let soft = apply(&m, MaskOp::Smoothness(Smoothness { range: 12, falloff: 100 }));
        assert!(soft[beside] > wide[beside] && soft[beside] < 255, "soft {}", soft[beside]);
        assert!(soft[near(&m, 0.12, 0.0)] < soft[beside], "the ramp rises outwards");
        // Hidden polygons keep their mask and stop the spread.
        let mut m = m;
        m.mask = vec![7; m.vertex_count()];
        let hidden: Vec<u8> = (0..m.triangle_count()).map(|t| (m.positions[m.indices[3 * t] as usize].x > 0.1) as u8).collect();
        m.set_hidden(hidden);
        let masked = apply(&m, MaskOp::Smoothness(Smoothness { range: 2, falloff: 0 }));
        assert_eq!(masked[near(&m, 0.4, 0.0)], 7);
        assert!(masked[near(&m, 0.04, 0.0)] < 128 && masked[near(&m, -0.3, 0.0)] == 255);
        // A smooth sphere (under 8° between neighbouring normals) is masked all over.
        let (p, i) = crate::mesh::cube_sphere(32, 1.0);
        let round = SculptMesh::from_raw(p, i);
        assert!(apply(&round, MaskOp::Smoothness(Smoothness::default())).iter().all(|&x| x == 255));
    }

    #[test]
    fn mask_ambient_occlusion_masks_the_covered_plane_not_the_open_one() {
        // Two planes a little apart: the lower one looks up at the upper.
        let (p, i) = Primitive::Plane.build(20, 1.0);
        let n = p.len() as u32;
        let mut all = p.clone();
        all.extend(p.iter().map(|q| *q + Vec3::Z * 0.05));
        let mut idx = i.clone();
        idx.extend(i.iter().map(|k| k + n));
        let mut m = SculptMesh::from_raw(all, idx);
        let centre = |m: &SculptMesh, z: f32| {
            let p = &m.positions;
            (0..p.len()).min_by(|&a, &b| (p[a] - Vec3::Z * z).length().total_cmp(&(p[b] - Vec3::Z * z).length())).unwrap()
        };
        let (lower, upper) = (centre(&m, 0.0), centre(&m, 0.05));
        let mask = apply(&m, MaskOp::Occlusion(Occlusion::default()));
        assert!(mask[lower] > 200 && mask[upper] == 0, "lower {} upper {}", mask[lower], mask[upper]);
        // Half the intensity, half the mask; a scan too short to reach the cover finds nothing.
        let half = apply(&m, MaskOp::Occlusion(Occlusion { intensity: 50, ..Occlusion::default() }));
        assert!((half[lower] as i32 - mask[lower] as i32 / 2).abs() <= 1);
        let short = apply(&m, MaskOp::Occlusion(Occlusion { scan_dist: 1, ..Occlusion::default() }));
        assert_eq!(short[lower], 0);
        // Hide the upper plane: it no longer blocks, and its vertices keep their mask.
        m.mask = mask.clone();
        let hidden: Vec<u8> = (0..m.triangle_count()).map(|t| (m.positions[m.indices[3 * t] as usize].z > 0.01) as u8).collect();
        m.set_hidden(hidden);
        let open = apply(&m, MaskOp::Occlusion(Occlusion::default()));
        assert_eq!(open[lower], 0);
        assert_eq!(open[upper], mask[upper]);
        // An open sphere is nowhere occluded.
        assert!(apply(&sphere(), MaskOp::Occlusion(Occlusion::default())).iter().all(|&x| x == 0));
    }
}
