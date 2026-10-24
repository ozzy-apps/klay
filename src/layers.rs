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
    pub rec: Option<(usize, Vec<Vec3>)>,
}

impl Layers {
    /// Fold what was sculpted into the recording layer since the last time, from the
    /// highest level's positions `top`. A layer recording at intensity 0 takes it at 1.
    pub fn freeze(&mut self, top: &[Vec3]) {
        let Some((i, base)) = self.rec.as_mut() else { return };
        let layer = &mut self.list[*i];
        if layer.delta.len() != top.len() || base.len() != top.len() {
            return;
        }
        let k = if layer.applied().abs() > 1e-4 { 1.0 / layer.applied() } else { 1.0 };
        layer.delta.par_iter_mut().zip(top.par_iter().zip(base.par_iter())).for_each(|(d, (p, b))| *d += (*p - *b) * k);
        base.copy_from_slice(top);
    }

    /// The sum of every layer's applied offsets at vertex `v`.
    fn contribution(&self, v: usize) -> Vec3 {
        self.list.iter().map(|l| l.delta[v] * l.applied()).sum()
    }

    /// Change `mesh` (the highest level) and the layers to `target`, a frozen state: the
    /// sculpt without layers stays, the layers' part becomes `target`'s. Returns the state
    /// it replaced (frozen), which undoes it.
    pub fn swap(&mut self, mesh: &mut SculptMesh, target: Layers) -> Layers {
        self.freeze(&mesh.positions);
        let n = mesh.vertex_count();
        let fits = |l: &Layers| l.list.iter().all(|x| x.delta.len() == n);
        if fits(self) && fits(&target) {
            let (old, new) = (&*self, &target);
            mesh.positions.par_iter_mut().enumerate().for_each(|(v, p)| *p += new.contribution(v) - old.contribution(v));
        }
        let mut target = target;
        if let Some((_, base)) = target.rec.as_mut() {
            *base = mesh.positions.clone();
        }
        let all: Vec<u32> = (0..mesh.chunks.len() as u32).collect();
        mesh.finish_edit(&all);
        std::mem::replace(self, target)
    }

    /// Memory held, bytes.
    pub fn bytes(&self) -> usize {
        self.list.iter().map(|l| l.delta.len() * 12).sum::<usize>() + self.rec.as_ref().map_or(0, |(_, b)| b.len() * 12)
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
        next.list.push(SculptLayer { name: "Layer 1".into(), delta: vec![Vec3::ZERO; n], intensity: 1.0, visible: true });
        next.rec = Some((0, Vec::new()));
        layers.swap(&mut m, next);
        // Sculpt: push everything out by 0.1.
        for p in &mut m.positions {
            *p += Vec3::X * 0.1;
        }
        layers.freeze(&m.positions);
        assert!(layers.list[0].delta.iter().all(|d| (*d - Vec3::X * 0.1).length() < 1e-6));
        // Half intensity takes half the offsets back out; hiding takes the rest.
        let mut half = layers.clone();
        half.rec = None;
        half.list[0].intensity = 0.5;
        let undo = layers.swap(&mut m, half);
        assert!(m.positions.iter().zip(&start).all(|(p, q)| (*p - *q - Vec3::X * 0.05).length() < 1e-6));
        let mut hidden = layers.clone();
        hidden.list[0].visible = false;
        layers.swap(&mut m, hidden);
        assert!(m.positions.iter().zip(&start).all(|(p, q)| p.distance(*q) < 1e-6));
        // Undo states put the shape back.
        layers.swap(&mut m, undo);
        assert!(m.positions.iter().zip(&start).all(|(p, q)| (*p - *q - Vec3::X * 0.1).length() < 1e-6));
    }
}
