//! Repeating geometric relief profiles in brush-radius coordinates.
use crate::sculpt::BrushKind;

fn band(x: f32, half: f32, edge: f32) -> f32 {
    ((half - x.abs()) / edge + 0.5).clamp(0.0, 1.0)
}

pub fn profile(kind: BrushKind, across: f32, along: f32) -> f32 {
    let periodic =
        |pitch: f32, offset: f32| ((along / pitch + offset + 0.5).rem_euclid(1.0) - 0.5) * pitch;
    match kind {
        BrushKind::Zipper => {
            let phase = periodic(0.36, if across > 0.0 { 0.5 } else { 0.0 });
            let tooth = band(across.abs() - 0.25, 0.23, 0.04) * band(phase, 0.12, 0.04);
            let rails = 0.22 * (-((across.abs() - 0.55) / 0.07).powi(2)).exp();
            let seam = 0.24 * (-(across / 0.055).powi(2)).exp();
            (tooth + rails - seam).clamp(-1.0, 1.0)
        }
        BrushKind::Stitch => {
            let phase = periodic(0.45, 0.0);
            let thread =
                (-((phase + across * 0.28) / 0.045).powi(2)).exp() * band(across, 0.48, 0.06);
            thread - 0.16 * (-(across / 0.06).powi(2)).exp()
        }
        BrushKind::Rake => {
            let phase = ((across + 0.075) / 0.25).rem_euclid(1.0) - 0.5;
            -(-((phase) / 0.14).powi(2)).exp() * band(across, 0.8, 0.1)
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zipper_has_two_staggered_tooth_rows_and_a_recessed_seam() {
        assert!(profile(BrushKind::Zipper, -0.28, 0.0) > 0.9);
        assert!(profile(BrushKind::Zipper, 0.28, 0.0) < 0.1);
        assert!(profile(BrushKind::Zipper, 0.28, 0.18) > 0.9);
        assert!(profile(BrushKind::Zipper, 0.0, 0.0) < 0.0);
        for kind in [BrushKind::Zipper, BrushKind::Stitch, BrushKind::Rake] {
            for i in -100..100 {
                let h = profile(kind, i as f32 * 0.02, i as f32 * 0.13);
                assert!(h.is_finite() && h.abs() <= 1.01);
            }
        }
    }
}

#[cfg(test)]
mod stroke_tests {
    use super::*;
    use crate::{
        mesh::SculptMesh,
        primitive::Primitive,
        sculpt::{Brush, Stroke},
    };
    use glam::Vec3;
    fn plane() -> SculptMesh {
        let (p, i) = Primitive::Plane.build(120, 1.0);
        SculptMesh::from_raw(p, i)
    }
    fn path(m: &mut SculptMesh, kind: BrushKind, steps: usize, invert: bool) {
        let b = Brush {
            kind,
            radius: 0.25,
            strength: 1.0,
            ..Brush::default()
        };
        let mut s = Stroke::begin(invert, false, false);
        for i in 0..=steps {
            let h = Vec3::new(-0.6 + 1.2 * i as f32 / steps as f32, 0.0, 0.0);
            s.dab(m, &b, h, Vec3::Z, -Vec3::Z, (h + Vec3::Z * 4.0, -Vec3::Z));
        }
        s.end(m);
    }
    #[test]
    fn details_have_continuous_spacing_and_respect_masks() {
        for kind in [BrushKind::Zipper, BrushKind::Stitch, BrushKind::Rake] {
            let mut a = plane();
            let mut b = plane();
            let before = a.positions.clone();
            path(&mut a, kind, 10, false);
            path(&mut b, kind, 40, false);
            assert!(
                a.positions
                    .iter()
                    .zip(&before)
                    .any(|(a, b)| a.distance(*b) > 0.001),
                "{kind:?}"
            );
            let error = a
                .positions
                .iter()
                .zip(&b.positions)
                .map(|(a, b)| a.distance(*b))
                .fold(0.0f32, f32::max);
            assert!(error < 1e-5, "{kind:?} event spacing error {error}");
            assert!(a.positions.iter().all(|p| p.is_finite()));
            let mut masked = plane();
            masked.mask = vec![255; masked.vertex_count()];
            path(&mut masked, kind, 20, false);
            assert_eq!(masked.positions, before);
        }
    }
    #[test]
    fn rake_alt_reverses_grooves_and_forms_have_distinct_behavior() {
        let mut a = plane();
        let mut b = plane();
        path(&mut a, BrushKind::Rake, 20, false);
        path(&mut b, BrushKind::Rake, 20, true);
        assert!(a.positions.iter().any(|p| p.z < -0.001));
        assert!(a.positions.iter().all(|p| p.z <= 1e-6));
        assert!(b.positions.iter().any(|p| p.z > 0.001));
        let mut a = plane();
        let mut b = plane();
        for (m, kind) in [(&mut a, BrushKind::FormSoft), (&mut b, BrushKind::FormHard)] {
            let brush = Brush {
                kind,
                radius: 0.4,
                strength: 1.0,
                ..Brush::default()
            };
            let mut s = Stroke::begin(false, false, false);
            s.dab(
                m,
                &brush,
                Vec3::ZERO,
                Vec3::Z,
                -Vec3::Z,
                (Vec3::Z * 4.0, -Vec3::Z),
            );
            s.end(m);
        }
        let soft = a.positions.iter().map(|p| p.z).fold(0.0f32, f32::max);
        let hard = b.positions.iter().map(|p| p.z).fold(0.0f32, f32::max);
        assert!(soft > 0.001 && hard > soft * 1.5);
    }
}
