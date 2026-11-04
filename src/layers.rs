//! Sculpt layers (ZBrush: Tool › Layers). A layer holds an offset per vertex of the highest
//! subdivision level; the shape you see there is the sculpt without layers plus every
//! visible layer's offsets times its intensity. While a layer records (REC), what you sculpt
//! goes into it: its offsets are what changed since it started recording, divided by its
//! intensity. Layers live with the stack (`Multires::layers`) so undo can put them back.
//!
//! The recording layer's offsets are kept "live" (stored offsets plus the change since
//! `base`) and folded in ([`Layers::freeze`]) before anything reads or changes the layers.

use crate::mesh::SculptMesh;
use glam::Vec3;
use rayon::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct SculptLayer {
    pub name: String,
    /// Offset per vertex of the highest level, at intensity 1.
    pub delta: Vec<Vec3>,
    /// Premultiplied paint and mask changes in byte units. Empty until a layer edits them.
    pub paint: Vec<[f32; 4]>,
    pub mask: Vec<f32>,
    /// -1..=2; 1 is the layer as recorded.
    pub intensity: f32,
    pub visible: bool,
}

impl SculptLayer {
    /// How much of the layer is in the shape now.
    pub fn applied(&self) -> f32 {
        if self.visible { self.intensity } else { 0.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layers {
    pub list: Vec<SculptLayer>,
    /// The recording layer and the highest level's positions when its offsets were last
    /// folded in.
    pub rec: Option<(usize, Recording)>,
    /// Quantization and clipping remainder. Keeps toggles reversible at any intensity.
    pub paint_remainder: Vec<[f32; 4]>,
    pub mask_remainder: Vec<f32>,
    pub top_mask: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    pub(crate) positions: Vec<Vec3>,
    paint: Vec<[u8; 4]>,
    mask: Vec<u8>,
}

impl Recording {
    pub fn of(mesh: &SculptMesh) -> Self {
        Self {
            positions: mesh.positions.clone(),
            paint: mesh.colors.clone(),
            mask: mesh.mask.clone(),
        }
    }
}

impl Layers {
    /// Fold what was sculpted into the recording layer since the last time, from the
    /// highest level's positions `top`. A layer recording at intensity 0 takes it at 1.
    pub fn freeze(&mut self, mesh: &SculptMesh) {
        let top = &mesh.positions;
        let Some((i, base)) = self.rec.as_mut() else {
            return;
        };
        let layer = &mut self.list[*i];
        if layer.delta.len() != top.len() || base.positions.len() != top.len() {
            return;
        }
        let k = if layer.applied().abs() > 1e-4 {
            1.0 / layer.applied()
        } else {
            1.0
        };
        layer
            .delta
            .par_iter_mut()
            .zip(top.par_iter().zip(base.positions.par_iter()))
            .for_each(|(d, (p, b))| *d += (*p - *b) * k);
        base.positions.copy_from_slice(top);
        if base.paint != mesh.colors {
            layer.paint.resize(top.len(), [0.0; 4]);
            layer.paint.par_iter_mut().enumerate().for_each(|(v, d)| {
                let a = base.paint.get(v).copied().unwrap_or_default();
                let b = mesh.colors.get(v).copied().unwrap_or_default();
                for i in 0..4 {
                    d[i] += (b[i] as f32 - a[i] as f32) * k;
                }
            });
            base.paint.clone_from(&mesh.colors);
        }
        if base.mask != mesh.mask {
            layer.mask.resize(top.len(), 0.0);
            layer.mask.par_iter_mut().enumerate().for_each(|(v, d)| {
                let a = base.mask.get(v).copied().unwrap_or_default();
                let b = mesh.mask.get(v).copied().unwrap_or_default();
                *d += (b as f32 - a as f32) * k;
            });
            base.mask.clone_from(&mesh.mask);
        }
    }

    /// The sum of every layer's applied offsets at vertex `v`.
    fn contribution(&self, v: usize) -> Vec3 {
        self.list.iter().map(|l| l.delta[v] * l.applied()).sum()
    }

    fn paint_at(&self, v: usize) -> glam::Vec4 {
        self.list
            .iter()
            .filter_map(|l| {
                l.paint
                    .get(v)
                    .map(|c| glam::Vec4::from_array(*c) * l.applied())
            })
            .sum()
    }

    fn mask_at(&self, v: usize) -> f32 {
        self.list
            .iter()
            .filter_map(|l| l.mask.get(v).map(|m| m * l.applied()))
            .sum()
    }

    pub fn has_mask(&self) -> bool {
        self.list.iter().any(|l| !l.mask.is_empty())
    }

    /// Change `mesh` (the highest level) and the layers to `target`, a frozen state: the
    /// sculpt without layers stays, the layers' part becomes `target`'s. Returns the state
    /// it replaced (frozen), which undoes it.
    pub fn swap(&mut self, mesh: &mut SculptMesh, target: Layers) -> Layers {
        self.freeze(mesh);
        let n = mesh.vertex_count();
        let mut target = target;
        let with_mask = self.has_mask() || target.has_mask();
        let fits = |l: &Layers| l.list.iter().all(|x| x.delta.len() == n);
        if fits(self) && fits(&target) {
            let (old, new) = (&*self, &target);
            mesh.positions
                .par_iter_mut()
                .enumerate()
                .for_each(|(v, p)| *p += new.contribution(v) - old.contribution(v));
            let paint_remainder = if old
                .list
                .iter()
                .chain(&new.list)
                .any(|l| !l.paint.is_empty())
            {
                crate::paint::ensure(mesh);
                let remainder: Vec<_> = mesh
                    .colors
                    .par_iter_mut()
                    .enumerate()
                    .map(|(v, c)| {
                        let p = glam::Vec4::from_array(c.map(|x| x as f32))
                            + glam::Vec4::from_array(
                                old.paint_remainder.get(v).copied().unwrap_or_default(),
                            )
                            + new.paint_at(v)
                            - old.paint_at(v);
                        let values = p.to_array();
                        let a = values[3].round().clamp(0.0, 255.0);
                        *c = std::array::from_fn(|i| values[i].round().clamp(0.0, a) as u8);
                        std::array::from_fn(|i| values[i] - c[i] as f32)
                    })
                    .collect();
                mesh.chunks.iter_mut().for_each(|c| c.dirty_color = true);
                Some(remainder)
            } else {
                None
            };
            if with_mask {
                let (values, remainder): (Vec<_>, Vec<_>) = (0..n)
                    .into_par_iter()
                    .map(|v| {
                        let raw = mesh.mask.get(v).copied().unwrap_or_default() as f32
                            + old.mask_remainder.get(v).copied().unwrap_or_default()
                            + new.mask_at(v)
                            - old.mask_at(v);
                        let value = raw.round().clamp(0.0, 255.0) as u8;
                        (value, raw - value as f32)
                    })
                    .unzip();
                crate::mask::replace(mesh, values);
                target.mask_remainder = remainder;
            }
            if let Some(remainder) = paint_remainder {
                target.paint_remainder = remainder;
            }
        }
        if let Some((_, base)) = target.rec.as_mut() {
            *base = Recording::of(mesh);
        }
        let all: Vec<u32> = (0..mesh.chunks.len() as u32).collect();
        mesh.finish_edit(&all);
        std::mem::replace(self, target)
    }

    /// Memory held, bytes.
    pub fn bytes(&self) -> usize {
        self.list
            .iter()
            .map(|l| l.delta.len() * 12 + l.paint.len() * 16 + l.mask.len() * 4)
            .sum::<usize>()
            + self.paint_remainder.len() * 16
            + self.mask_remainder.len() * 4
            + self.top_mask.len()
            + self.rec.as_ref().map_or(0, |(_, b)| {
                b.positions.len() * 12 + b.paint.len() * 4 + b.mask.len()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(6, 1.0);
        SculptMesh::from_raw(p, i)
    }

    #[test]
    fn recording_freezes_and_intensity_scales_the_layer() {
        let mut m = sphere();
        let start = m.positions.clone();
        let n = m.vertex_count();
        let mut layers = Layers::default();
        // New layer, recording.
        let mut next = layers.clone();
        next.list.push(SculptLayer {
            name: "Layer 1".into(),
            delta: vec![Vec3::ZERO; n],
            intensity: 1.0,
            visible: true,
            paint: Vec::new(),
            mask: Vec::new(),
        });
        next.rec = Some((0, Recording::default()));
        layers.swap(&mut m, next);
        // Sculpt: push everything out by 0.1.
        for p in &mut m.positions {
            *p += Vec3::X * 0.1;
        }
        layers.freeze(&m);
        assert!(
            layers.list[0]
                .delta
                .iter()
                .all(|d| (*d - Vec3::X * 0.1).length() < 1e-6)
        );
        // Half intensity takes half the offsets back out; hiding takes the rest.
        let mut half = layers.clone();
        half.rec = None;
        half.list[0].intensity = 0.5;
        let undo = layers.swap(&mut m, half);
        assert!(
            m.positions
                .iter()
                .zip(&start)
                .all(|(p, q)| (*p - *q - Vec3::X * 0.05).length() < 1e-6)
        );
        let mut hidden = layers.clone();
        hidden.list[0].visible = false;
        layers.swap(&mut m, hidden);
        assert!(
            m.positions
                .iter()
                .zip(&start)
                .all(|(p, q)| p.distance(*q) < 1e-6)
        );
        // Undo states put the shape back.
        layers.swap(&mut m, undo);
        assert!(
            m.positions
                .iter()
                .zip(&start)
                .all(|(p, q)| (*p - *q - Vec3::X * 0.1).length() < 1e-6)
        );
    }
}
