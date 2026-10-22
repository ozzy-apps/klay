//! Screen-space masking gestures and topology-preserving line clipping.
use crate::mesh::SculptMesh;
use crate::multires::ChunkData;
use crate::sculpt::BrushKind;
use glam::{Mat4, Vec2, Vec3, Vec4};
use rayon::prelude::*;

#[derive(Clone, Copy)]
pub struct Screen {
    pub matrix: Mat4,
    pub size: Vec2,
}

impl Screen {
    pub fn project(self, p: Vec3) -> Option<Vec2> {
        let q = self.matrix * p.extend(1.0);
        if !q.is_finite() || q.w <= 1e-6 {
            return None;
        }
        let ndc = q.truncate() / q.w;
        Some(Vec2::new(ndc.x + 1.0, 1.0 - ndc.y) * self.size * 0.5)
    }

    /// Homogeneous screen line transformed back into a world-space clipping plane.
    pub fn plane(self, a: Vec2, b: Vec2) -> Option<Vec4> {
        if !a.is_finite() || !b.is_finite() || a.distance(b) < 2.0 {
            return None;
        }
        let ndc = |p: Vec2| {
            Vec3::new(
                p.x / self.size.x * 2.0 - 1.0,
                1.0 - p.y / self.size.y * 2.0,
                1.0,
            )
        };
        let line = ndc(a).cross(ndc(b));
        let plane = self.matrix.transpose() * Vec4::new(line.x, line.y, 0.0, line.z);
        let length = plane.truncate().length();
        (length > 1e-8 && plane.is_finite()).then(|| plane / length)
    }
}

#[derive(Clone)]
pub struct Gesture {
    pub kind: BrushKind,
    pub start: Vec2,
    pub end: Vec2,
    pub points: Vec<Vec2>,
    pub alternate: bool,
    pub ctrl: bool,
}

impl Gesture {
    pub fn new(kind: BrushKind, start: Vec2, alternate: bool) -> Self {
        Self {
            kind,
            start,
            end: start,
            points: vec![start],
            alternate,
            ctrl: false,
        }
    }

    pub fn update(&mut self, mut p: Vec2, snap: bool, alternate: bool) {
        if snap && matches!(self.kind, BrushKind::ClipLine | BrushKind::MaskLine) {
            let d = p - self.start;
            let angle = (d.y.atan2(d.x) / std::f32::consts::FRAC_PI_4).round()
                * std::f32::consts::FRAC_PI_4;
            p = self.start + Vec2::new(angle.cos(), angle.sin()) * d.length();
        }
        self.end = p;
        self.alternate = alternate;
        if self.kind == BrushKind::MaskLasso
            && self
                .points
                .last()
                .is_none_or(|last| last.distance(p) >= 2.0)
        {
            if self.points.len() < 2048 {
                self.points.push(p);
            } else {
                *self.points.last_mut().unwrap() = p;
            }
        }
    }

    pub fn contains(&self, p: Vec2) -> bool {
        let lo = self.start.min(self.end);
        let hi = self.start.max(self.end);
        match self.kind {
            BrushKind::MaskRect => (p - lo).min_element() >= 0.0 && (hi - p).min_element() >= 0.0,
            BrushKind::MaskCircle => {
                let radius = (hi - lo) * 0.5;
                radius.min_element() > 1.0
                    && ((p - (lo + hi) * 0.5) / radius).length_squared() <= 1.0
            }
            BrushKind::MaskLine => {
                let d = self.end - self.start;
                d.length_squared() > 4.0 && d.perp_dot(p - self.start) < 0.0
            }
            BrushKind::MaskLasso => {
                if self.points.len() < 3 {
                    return false;
                }
                let mut inside = false;
                let mut a = *self.points.last().unwrap();
                for &b in &self.points {
                    if (a.y > p.y) != (b.y > p.y)
                        && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x
                    {
                        inside = !inside;
                    }
                    a = b;
                }
                inside
            }
            _ => false,
        }
    }
}

/// Shape masks select all visible vertices through the view depth, with X mirroring.
pub fn mask_shape(
    mesh: &SculptMesh,
    screen: Screen,
    gesture: &Gesture,
    strength: f32,
    symmetry: bool,
) -> Option<Vec<u8>> {
    if gesture.start.distance(gesture.end) < 3.0 && gesture.kind != BrushKind::MaskLasso {
        return None;
    }
    let step =
        (strength.clamp(0.0, 1.0) * 255.0).round() as i16 * if gesture.alternate { -1 } else { 1 };
    let bounds = if gesture.kind == BrushKind::MaskLasso {
        Some(gesture.points.iter().fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(lo, hi), &p| (lo.min(p), hi.max(p)),
        ))
    } else {
        None
    };
    let contains = |p: Vec2| {
        bounds.is_none_or(|(lo, hi)| (p - lo).min_element() >= 0.0 && (hi - p).min_element() >= 0.0)
            && gesture.contains(p)
    };
    let mask: Vec<u8> = mesh
        .positions
        .par_iter()
        .enumerate()
        .map(|(v, &p)| {
            let old = mesh.mask.get(v).copied().unwrap_or(0);
            if !mesh.shown.is_empty() && mesh.shown[v] == 0 {
                return old;
            }
            let inside = screen.project(p).is_some_and(contains)
                || (symmetry
                    && screen
                        .project(Vec3::new(-p.x, p.y, p.z))
                        .is_some_and(contains));
            if inside {
                (old as i16 + step).clamp(0, 255) as u8
            } else {
                old
            }
        })
        .collect();
    mask.iter()
        .enumerate()
        .any(|(v, &m)| m != mesh.mask.get(v).copied().unwrap_or(0))
        .then_some(mask)
}

/// Collapse the chosen half-space onto the guide plane, preserving topology and paint.
pub fn clip_line(
    mesh: &mut SculptMesh,
    screen: Screen,
    gesture: &Gesture,
    symmetry: bool,
) -> Option<ChunkData> {
    let mut plane = screen.plane(gesture.start, gesture.end)?;
    if gesture.alternate {
        plane = -plane;
    }
    let mirrored = Vec4::new(-plane.x, plane.y, plane.z, plane.w);
    let project = |mut p: Vec3, plane: Vec4, freedom: f32| {
        let distance = plane.truncate().dot(p) + plane.w;
        if distance > 0.0 {
            p -= plane.truncate() * distance * freedom;
        }
        p
    };
    let changed: Vec<_> = mesh
        .chunks
        .par_iter()
        .enumerate()
        .filter_map(|(id, c)| {
            let (a, b) = c.range();
            let new: Vec<_> = (a..b)
                .map(|v| {
                    let p = mesh.positions[v];
                    if (!mesh.shown.is_empty() && mesh.shown[v] == 0) || screen.project(p).is_none()
                    {
                        return p;
                    }
                    let freedom = crate::mask::freedom(&mesh.mask, v as u32);
                    let q = project(p, plane, freedom);
                    if symmetry && plane.x.abs() > 1e-6 {
                        project(q, mirrored, freedom)
                    } else {
                        q
                    }
                })
                .collect();
            new.iter()
                .zip(&mesh.positions[a..b])
                .any(|(p, q)| p.distance_squared(*q) > 1e-16)
                .then(|| (id as u32, mesh.positions[a..b].to_vec(), new))
        })
        .collect();
    if changed.is_empty() {
        return None;
    }
    let mut before = Vec::with_capacity(changed.len());
    let mut after = Vec::with_capacity(changed.len());
    for (id, old, new) in changed {
        before.push((id, old));
        after.push((id, new));
    }
    let ids: Vec<_> = after.iter().map(|(id, _)| *id).collect();
    mesh.restore_chunks(&after);
    mesh.finish_edit(&ids);
    Some(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mesh() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(24, 1.0);
        SculptMesh::from_raw(p, i)
    }
    fn screen() -> Screen {
        Screen {
            matrix: Mat4::IDENTITY,
            size: Vec2::splat(400.0),
        }
    }

    #[test]
    fn clipping_projects_the_selected_side_and_preserves_topology_paint_and_undo() {
        for alternate in [false, true] {
            let mut m = mesh();
            let before = m.positions.clone();
            let indices = m.indices.clone();
            m.colors = vec![[80, 40, 20, 128]; m.vertex_count()];
            let colors = m.colors.clone();
            let mut g = Gesture::new(BrushKind::ClipLine, Vec2::new(200.0, 0.0), alternate);
            g.end = Vec2::new(200.0, 400.0);
            let saved = clip_line(&mut m, screen(), &g, false).unwrap();
            for (p, old) in m.positions.iter().zip(&before) {
                let selected = if alternate { old.x < 0.0 } else { old.x > 0.0 };
                if selected {
                    assert!(p.x.abs() < 1e-6);
                } else {
                    assert_eq!(p, old);
                }
            }
            assert_eq!(m.indices, indices);
            assert_eq!(m.colors, colors);
            m.restore_chunks(&saved);
            assert_eq!(m.positions, before);
        }
    }

    #[test]
    fn perspective_clip_points_land_on_the_screen_guide() {
        let mut camera = crate::camera::Camera::new();
        camera.aspect = 1.0;
        let s = Screen {
            matrix: camera.view_proj(),
            size: Vec2::splat(400.0),
        };
        let mut m = mesh();
        let before = m.positions.clone();
        let mut g = Gesture::new(BrushKind::ClipLine, Vec2::new(220.0, 0.0), false);
        g.end = Vec2::new(220.0, 400.0);
        clip_line(&mut m, s, &g, false).unwrap();
        let mut changed = 0;
        for (&p, &old) in m.positions.iter().zip(&before) {
            if p.distance_squared(old) > 1e-12 {
                changed += 1;
                assert!((s.project(p).unwrap().x - 220.0).abs() < 1e-3);
            }
        }
        assert!(changed > 100);
    }

    #[test]
    fn clip_obeys_masks_hidden_vertices_and_mirrored_plane_strength() {
        let mut m = mesh();
        let before = m.positions.clone();
        m.mask = vec![255; m.vertex_count()];
        let mut g = Gesture::new(BrushKind::ClipLine, Vec2::new(0.0, 200.0), false);
        g.end = Vec2::new(400.0, 200.0);
        assert!(clip_line(&mut m, screen(), &g, true).is_none());
        m.mask.fill(128);
        clip_line(&mut m, screen(), &g, true).unwrap();
        for (p, old) in m.positions.iter().zip(&before) {
            if old.y > 0.0 {
                assert!((p.y - old.y * (128.0 / 255.0)).abs() < 1e-6);
            }
        }
        let mut hidden = mesh();
        hidden.set_hidden(vec![1; hidden.triangle_count()]);
        assert!(clip_line(&mut hidden, screen(), &g, false).is_none());
    }

    #[test]
    fn shape_masks_cover_depth_erase_and_preserve_hidden_vertices() {
        for kind in [
            BrushKind::MaskRect,
            BrushKind::MaskCircle,
            BrushKind::MaskLasso,
            BrushKind::MaskLine,
        ] {
            let mut m = mesh();
            let positions = m.positions.clone();
            let mut g = Gesture::new(kind, Vec2::new(140.0, 140.0), false);
            g.end = Vec2::new(260.0, 260.0);
            g.points = vec![
                Vec2::new(140.0, 140.0),
                Vec2::new(260.0, 140.0),
                Vec2::new(260.0, 260.0),
                Vec2::new(140.0, 260.0),
            ];
            let mask = mask_shape(&m, screen(), &g, 1.0, false).unwrap();
            assert!(mask.contains(&255) && mask.contains(&0), "{kind:?}");
            m.mask = mask;
            g.alternate = true;
            let cleared = mask_shape(&m, screen(), &g, 1.0, false).unwrap();
            assert!(cleared.iter().all(|&m| m == 0));
            assert_eq!(m.positions, positions);
            m.set_hidden(vec![1; m.triangle_count()]);
            assert!(mask_shape(&m, screen(), &g, 1.0, false).is_none());
        }
    }

    #[test]
    fn lasso_is_closed_and_concave_and_mirroring_selects_both_sides() {
        let mut g = Gesture::new(BrushKind::MaskLasso, Vec2::ZERO, false);
        g.points = vec![
            Vec2::ZERO,
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 100.0),
            Vec2::new(50.0, 40.0),
            Vec2::new(0.0, 100.0),
        ];
        assert!(g.contains(Vec2::new(25.0, 25.0)));
        assert!(!g.contains(Vec2::new(50.0, 80.0)));
        let m = mesh();
        let mut g = Gesture::new(BrushKind::MaskRect, Vec2::new(250.0, 150.0), false);
        g.end = Vec2::new(350.0, 250.0);
        let a = mask_shape(&m, screen(), &g, 1.0, false).unwrap();
        let b = mask_shape(&m, screen(), &g, 1.0, true).unwrap();
        assert!(b.iter().filter(|&&m| m > 0).count() > a.iter().filter(|&&m| m > 0).count());
    }
}
