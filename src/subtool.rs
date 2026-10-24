//! SubTools: one model made of several meshes, ZBrush style.
//!
//! Exactly one subtool is active. Its full level stack lives in `App::doc`, with adjacency,
//! stencils, every level's positions and the GPU level cache, and it is the only one that
//! can be sculpted. Every other subtool is parked, which keeps it light:
//!
//! - RAM: only the lossless packed form of its stack ([`Packed`]): the base mesh, one byte
//!   per vertex per upper level to replay the subdivision, and the offsets of the chunks
//!   that were sculpted. Its undo list is kept; its redo list is dropped.
//! - GPU: a draw-only stand-in of at most [`STAND_IN_VERTICES`] vertices, sampled from its
//!   top level, with baked normals (40 bytes per vertex).
//!
//! Activating a parked subtool rebuilds its stack exactly (same vertex order and chunks),
//! so its undo history stays valid.

use crate::mesh::Aabb;
use crate::multires::Packed;
use crate::sculpt::History;
use crate::transform::Affine;

/// Stand-ins of parked subtools use the highest level with at most this many vertices.
pub const STAND_IN_VERTICES: usize = 2_000_000;

pub struct SubTool {
    /// Stable id, also the key of its stand-in on the GPU.
    pub id: u64,
    pub name: String,
    pub visible: bool,
    /// Bounds of what is drawn for it.
    pub bounds: Aabb,
    /// `None` for the active subtool, whose stack is `App::doc`.
    pub parked: Option<Box<Parked>>,
    /// Wedge is on (ZBrush: the DynaMesh button): Ctrl + drag on the background re-wedges.
    pub wedge: bool,
}

pub struct Parked {
    pub packed: Packed,
    pub history: History,
    /// Vertices of its stand-in; 0 when the stand-in did not fit on the GPU.
    pub stand_in_vertices: usize,
}

/// Deformation applied to the active subtool (ZBrush: Tool > Deformation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deform {
    OffsetX,
    OffsetY,
    OffsetZ,
    Size,
    Smooth,
    Polish,
    Inflate,
    Spherize,
    Noise,
    Twist,
    Taper,
}

impl Deform {
    pub const ALL: [Self; 11] = [
        Self::OffsetX, Self::OffsetY, Self::OffsetZ, Self::Size, Self::Smooth, Self::Polish,
        Self::Inflate, Self::Spherize, Self::Noise, Self::Twist, Self::Taper,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OffsetX => "Offset X",
            Self::OffsetY => "Offset Y",
            Self::OffsetZ => "Offset Z",
            Self::Size => "Size",
            Self::Smooth => "Smooth",
            Self::Polish => "Polish",
            Self::Inflate => "Inflate",
            Self::Spherize => "Spherize",
            Self::Noise => "Noise",
            Self::Twist => "Twist",
            Self::Taper => "Taper",
        }
    }

    /// Offset and Size map every level exactly (`deform_map`); the others reshape the shown
    /// level (`deform::shaped`).
    pub fn is_map(self) -> bool {
        matches!(self, Self::OffsetX | Self::OffsetY | Self::OffsetZ | Self::Size)
    }
}

/// The map for a deformation slider released at `value` (-100..100) on a subtool with
/// these bounds. Offsets move by up to the subtool's largest extent; Size scales by
/// 2^(value / 100) about the bounds centre.
pub fn deform_map(kind: Deform, value: f32, bounds: Aabb) -> Affine {
    use glam::Vec3;
    let center = (bounds.min + bounds.max) * 0.5;
    let extent = (bounds.max - bounds.min).max_element().max(1e-6);
    let step = value / 100.0 * extent;
    match kind {
        Deform::OffsetX => Affine::translation(Vec3::X * step),
        Deform::OffsetY => Affine::translation(Vec3::Y * step),
        Deform::OffsetZ => Affine::translation(Vec3::Z * step),
        Deform::Size => Affine::scaling(center, (value / 100.0).exp2()),
        _ => Affine::IDENTITY,
    }
}

/// Union of two boxes.
pub fn union(a: Aabb, b: Aabb) -> Aabb {
    let mut out = a;
    out.grow(b.min);
    out.grow(b.max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn deform_map_moves_by_the_extent_and_sizes_by_powers_of_two() {
        let b = Aabb {
            min: Vec3::new(-1.0, -0.5, 0.0),
            max: Vec3::new(1.0, 0.5, 1.0),
        };
        let m = deform_map(Deform::OffsetY, 50.0, b);
        assert!(m.is_translation());
        assert_eq!(m.translate, Vec3::Y * 1.0);
        let m = deform_map(Deform::Size, -100.0, b);
        assert_eq!(m.center, Vec3::new(0.0, 0.0, 0.5));
        assert_eq!(m.scale, 0.5);
        assert_eq!(m.translate, Vec3::ZERO);
        assert_eq!(deform_map(Deform::Size, 100.0, b).scale, 2.0);
    }
}
