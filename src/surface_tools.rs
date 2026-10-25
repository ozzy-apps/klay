//! Screen-space masking gestures and topology-preserving line clipping.
use crate::mesh::SculptMesh;
use crate::multires::ChunkData;
use crate::sculpt::{BrushKind, Shape};
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

    /// The point at screen pixel `s` and normalized depth `z` (as `ndc_z` gives it).
    pub fn unproject(self, s: Vec2, z: f32) -> Vec3 {
        let ndc = Vec4::new(s.x / self.size.x * 2.0 - 1.0, 1.0 - s.y / self.size.y * 2.0, z, 1.0);
        let q = self.matrix.inverse() * ndc;
        q.truncate() / q.w
    }

    /// Normalized device depth of `p`.
    pub fn ndc_z(self, p: Vec3) -> Option<f32> {
        let q = self.matrix * p.extend(1.0);
        (q.w > 1e-6 && q.is_finite()).then(|| q.z / q.w)
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
        if snap && self.kind.shape() == Some(Shape::Line) {
            let d = p - self.start;
            let angle = (d.y.atan2(d.x) / std::f32::consts::FRAC_PI_4).round()
                * std::f32::consts::FRAC_PI_4;
            p = self.start + Vec2::new(angle.cos(), angle.sin()) * d.length();
        }
        self.end = p;
        self.alternate = alternate;
        if self.kind.shape() == Some(Shape::Lasso)
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
        match self.kind.shape() {
            Some(Shape::Rect) => (p - lo).min_element() >= 0.0 && (hi - p).min_element() >= 0.0,
            Some(Shape::Circle) => {
                let radius = (hi - lo) * 0.5;
                radius.min_element() > 1.0
                    && ((p - (lo + hi) * 0.5) / radius).length_squared() <= 1.0
            }
            Some(Shape::Line) => {
                let d = self.end - self.start;
                d.length_squared() > 4.0 && d.perp_dot(p - self.start) < 0.0
            }
            Some(Shape::Lasso) => {
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
            None => false,
        }
    }

    /// The outline as a closed polygon in pixels (rectangle, 64-sided ellipse or lasso).
    pub fn outline(&self) -> Vec<Vec2> {
        let lo = self.start.min(self.end);
        let hi = self.start.max(self.end);
        match self.kind.shape() {
            Some(Shape::Rect) => vec![lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)],
            Some(Shape::Circle) => {
                let (c, r) = ((lo + hi) * 0.5, (hi - lo) * 0.5);
                (0..64)
                    .map(|i| {
                        let a = i as f32 / 64.0 * std::f32::consts::TAU;
                        c + Vec2::new(a.cos(), a.sin()) * r
                    })
                    .collect()
            }
            Some(Shape::Lasso) => self.points.clone(),
            _ => Vec::new(),
        }
    }

    /// Whether the gesture is too small to mean anything (a click).
    pub fn tiny(&self) -> bool {
        if self.kind.shape() == Some(Shape::Lasso) {
            self.points.len() < 3
        } else {
            self.start.distance(self.end) < 3.0
        }
    }
}

/// Nearest point of the closed outline `poly` to `p`.
fn nearest_on_outline(poly: &[Vec2], p: Vec2) -> Vec2 {
    let mut best = (f32::MAX, p);
    for k in 0..poly.len() {
        let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
        let d = b - a;
        let t = ((p - a).dot(d) / d.length_squared().max(1e-12)).clamp(0.0, 1.0);
        let q = a + d * t;
        let e = q.distance_squared(p);
        if e < best.0 {
            best = (e, q);
        }
    }
    best.1
}

/// Clip Rect / Clip Circle: every visible vertex whose screen position is outside the
/// outline (inside with Alt) moves, at its own depth, to the outline's nearest point, so the
/// model is pressed into the outline's shape all the way through. With symmetry the mirrored
/// outline presses the other side. Returns the positions it replaced, per chunk.
pub fn clip_shape(
    mesh: &mut SculptMesh,
    screen: Screen,
    gesture: &Gesture,
    symmetry: impl Into<crate::symmetry::Symmetry>,
) -> Option<ChunkData> {
    if gesture.tiny() {
        return None;
    }
    let poly = gesture.outline();
    if poly.len() < 3 {
        return None;
    }
    let images = symmetry.into().images();
    let ellipse = gesture.kind.shape() == Some(Shape::Circle);
    let (lo, hi) = (gesture.start.min(gesture.end), gesture.start.max(gesture.end));
    let (centre, radii) = ((lo + hi) * 0.5, ((hi - lo) * 0.5).max(Vec2::splat(1.0)));
    let target = |s: Vec2| -> Option<Vec2> {
        let inside = gesture.contains(s);
        if inside != gesture.alternate {
            return None;
        }
        // Onto the ellipse along its radius (a squashed circle stays a squashed circle).
        Some(if ellipse {
            let q = (s - centre) / radii;
            centre + q.normalize_or(Vec2::X) * radii
        } else {
            nearest_on_outline(&poly, s)
        })
    };
    let moved = |v: usize| -> Option<Vec3> {
        if !mesh.shown.is_empty() && mesh.shown[v] == 0 {
            return None;
        }
        let p = mesh.positions[v];
        for k in 0..images.len() {
            let own = images.unmap(k, p);
            let (Some(s), Some(z)) = (screen.project(own), screen.ndc_z(own)) else { continue };
            if let Some(t) = target(s) {
                let q = images.map(k, screen.unproject(t, z));
                let freedom = crate::mask::freedom(&mesh.mask, v as u32);
                return Some(p + (q - p) * freedom);
            }
        }
        None
    };
    let changed: Vec<_> = mesh
        .chunks
        .par_iter()
        .enumerate()
        .filter_map(|(id, c)| {
            let (a, b) = c.range();
            let mut any = false;
            let new: Vec<Vec3> = (a..b)
                .map(|v| match moved(v) {
                    Some(q) if q.is_finite() && q.distance_squared(mesh.positions[v]) > 1e-16 => {
                        any = true;
                        q
                    }
                    _ => mesh.positions[v],
                })
                .collect();
            any.then(|| (id as u32, mesh.positions[a..b].to_vec(), new))
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

/// Select Rect / Select Lasso: 1 for every triangle whose middle the outline covers.
pub fn tris_in_gesture(mesh: &SculptMesh, screen: Screen, gesture: &Gesture) -> Vec<u8> {
    (0..mesh.triangle_count())
        .into_par_iter()
        .map(|t| {
            let c = (0..3).map(|k| mesh.positions[mesh.indices[3 * t + k] as usize]).sum::<Vec3>() / 3.0;
            screen.project(c).is_some_and(|s| gesture.contains(s)) as u8
        })
        .collect()
}

/// Signed distance per vertex from a line gesture's cut, for `cut::line_cut`: positive on
/// the arrow's side (negative with Alt). The cut is the plane through the line along the
/// view. With X symmetry, `side` is the half the line was drawn on: that half is cut by the
/// line's plane and the other by its mirror image.
pub fn line_distance(mesh: &SculptMesh, screen: Screen, gesture: &Gesture, side: Option<f32>) -> Option<Vec<f32>> {
    let mut plane = screen.plane(gesture.start, gesture.end)?;
    // `plane` is positive on the side `clip_line` removes: the arrow's side.
    if gesture.alternate {
        plane = -plane;
    }
    let mirrored = Vec4::new(-plane.x, plane.y, plane.z, plane.w);
    let at = |pl: Vec4, p: Vec3| pl.truncate().dot(p) + pl.w;
    Some(
        mesh.positions
            .par_iter()
            .map(|&p| match side {
                Some(s) if plane.x.abs() > 1e-6 && p.x * s < 0.0 => at(mirrored, p),
                _ => at(plane, p),
            })
            .collect(),
    )
}

/// Shape masks select all visible vertices through the view depth; with symmetry a vertex
/// is inside when any image of it is.
pub fn mask_shape(
    mesh: &SculptMesh,
    screen: Screen,
    gesture: &Gesture,
    strength: f32,
    symmetry: impl Into<crate::symmetry::Symmetry>,
) -> Option<Vec<u8>> {
    let images = symmetry.into().images();
    if gesture.start.distance(gesture.end) < 3.0 && gesture.kind.shape() != Some(Shape::Lasso) {
        return None;
    }
    let step =
        (strength.clamp(0.0, 1.0) * 255.0).round() as i16 * if gesture.alternate { -1 } else { 1 };
    let bounds = if gesture.kind.shape() == Some(Shape::Lasso) {
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
            let inside = (0..images.len()).any(|k| screen.project(images.unmap(k, p)).is_some_and(contains));
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

/// Where a solid mesh crosses a plane: the outline segments in the plane's own 2D frame,
/// binned in a square grid so a point is tested inside or outside, and its nearest outline
/// point found, by looking at a few cells.
struct Section {
    origin: Vec3,
    u: Vec3,
    v: Vec3,
    segments: Vec<(Vec2, Vec2)>,
    /// Grid corner, cell size, cells per side, and the segments touching each cell.
    lo: Vec2,
    cell: f32,
    side: usize,
    cells: Vec<Vec<u32>>,
}

impl Section {
    fn new(mesh: &SculptMesh, plane: Vec4) -> Option<Self> {
        let n = plane.truncate();
        let origin = -n * plane.w;
        let u = n.any_orthonormal_vector();
        let v = n.cross(u);
        let at = |p: Vec3| Vec2::new((p - origin).dot(u), (p - origin).dot(v));
        let segments: Vec<(Vec2, Vec2)> = mesh
            .indices
            .par_chunks_exact(3)
            .filter_map(|t| {
                let d = [0, 1, 2].map(|k| n.dot(mesh.positions[t[k] as usize]) + plane.w);
                let mut ends = [Vec2::ZERO; 2];
                let mut found = 0;
                for k in 0..3 {
                    let (a, b) = (k, (k + 1) % 3);
                    if (d[a] > 0.0) != (d[b] > 0.0) && found < 2 {
                        // From the lower vertex id, so the two triangles of an edge get the
                        // very same point: a gap of rounding between their copies lets a ray
                        // slip through the outline and turns inside and outside around.
                        let (a, b) = if t[a] < t[b] { (a, b) } else { (b, a) };
                        let f = d[a] / (d[a] - d[b]);
                        ends[found] = at(mesh.positions[t[a] as usize].lerp(mesh.positions[t[b] as usize], f));
                        found += 1;
                    }
                }
                (found == 2).then_some((ends[0], ends[1]))
            })
            .collect();
        if segments.is_empty() {
            return None;
        }
        let (lo, hi) = segments.iter().fold((Vec2::MAX, Vec2::MIN), |(lo, hi), (a, b)| (lo.min(*a).min(*b), hi.max(*a).max(*b)));
        let side = ((segments.len() as f32).sqrt().ceil() as usize).clamp(1, 1024);
        let cell = ((hi - lo).max_element() / side as f32).max(1e-9);
        let mut cells = vec![Vec::new(); side * side];
        let index = |p: Vec2| (((p - lo) / cell).floor().as_ivec2()).clamp(glam::IVec2::ZERO, glam::IVec2::splat(side as i32 - 1));
        for (k, (a, b)) in segments.iter().enumerate() {
            let (c0, c1) = (index(a.min(*b)), index(a.max(*b)));
            for y in c0.y..=c1.y {
                for x in c0.x..=c1.x {
                    cells[y as usize * side + x as usize].push(k as u32);
                }
            }
        }
        Some(Self { origin, u, v, segments, lo, cell, side, cells })
    }

    fn cell_of(&self, p: Vec2) -> (i64, i64) {
        let c = ((p - self.lo) / self.cell).floor();
        (c.x as i64, c.y as i64)
    }

    /// Whether `q` lies inside the outline: crossings of a ray toward +u (even-odd), each
    /// counted in the cell where it happens so a segment in several cells counts once.
    fn inside(&self, q: Vec2) -> bool {
        let (cx, cy) = self.cell_of(q);
        let side = self.side as i64;
        if cy < 0 || cy >= side || cx >= side {
            return false;
        }
        let mut inside = false;
        for x in cx.max(0)..side {
            for &k in &self.cells[(cy * side + x) as usize] {
                let (a, b) = self.segments[k as usize];
                if (a.y > q.y) != (b.y > q.y) {
                    let at = a.x + (b.x - a.x) * (q.y - a.y) / (b.y - a.y);
                    // The segment's own cells: rounding can put `at` just past them, and
                    // then the crossing would be counted nowhere.
                    let column = |x: f32| (((x - self.lo.x) / self.cell).floor() as i64).clamp(0, side - 1);
                    let home = column(at).clamp(column(a.x.min(b.x)), column(a.x.max(b.x)));
                    if at > q.x && home == x {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    /// `q` (a point on the plane) itself when it is inside the cross-section, otherwise the
    /// nearest point of its outline.
    fn keep_inside(&self, q: Vec3) -> Vec3 {
        let flat = Vec2::new((q - self.origin).dot(self.u), (q - self.origin).dot(self.v));
        if self.inside(flat) {
            return q;
        }
        // Rings of cells outward from the point's own (clamped onto the grid), until a ring
        // is farther away than the best point found.
        let side = self.side as i64;
        let (cx, cy) = self.cell_of(flat);
        let (cx, cy) = (cx.clamp(0, side - 1), cy.clamp(0, side - 1));
        let gap = |x: i64, y: i64| {
            let a = self.lo + Vec2::new(x as f32, y as f32) * self.cell;
            let d = (a - flat).max(flat - (a + Vec2::splat(self.cell))).max(Vec2::ZERO);
            d.length_squared()
        };
        let mut best = (f32::MAX, flat);
        for ring in 0..side {
            let mut any = false;
            for y in (cy - ring).max(0)..=(cy + ring).min(side - 1) {
                for x in (cx - ring).max(0)..=(cx + ring).min(side - 1) {
                    if (x - cx).abs().max((y - cy).abs()) != ring || gap(x, y) > best.0 {
                        continue;
                    }
                    any = true;
                    for &k in &self.cells[(y * side + x) as usize] {
                        let (a, b) = self.segments[k as usize];
                        let ab = b - a;
                        let t = ((flat - a).dot(ab) / ab.length_squared().max(1e-20)).clamp(0.0, 1.0);
                        let c = a + ab * t;
                        let d = c.distance_squared(flat);
                        if d < best.0 {
                            best = (d, c);
                        }
                    }
                }
            }
            if !any && best.0 < f32::MAX {
                break;
            }
        }
        self.origin + self.u * best.1.x + self.v * best.1.y
    }
}

/// Collapse the chosen half-space onto the guide plane, preserving topology and paint.
///
/// With X symmetry, `symmetry` is the side (+1 or -1 along X) of the half the line was
/// drawn on: that half is clipped by the line's plane and the other half by its mirror
/// image, so the result is symmetric. Each half gets one plane: the line's plane only
/// matches the guide on its own half, and applying its mirror there too would cut deeper
/// than the guide wherever the view is turned or panned off the symmetry plane.
pub fn clip_line(
    mesh: &mut SculptMesh,
    screen: Screen,
    gesture: &Gesture,
    symmetry: Option<f32>,
) -> Option<ChunkData> {
    let mut plane = screen.plane(gesture.start, gesture.end)?;
    if gesture.alternate {
        plane = -plane;
    }
    let mirrored = Vec4::new(-plane.x, plane.y, plane.z, plane.w);
    // A vertex on the removed side moves straight onto the plane. On a solid it must land
    // inside the model's cross-section there: where the cut crosses an earlier cut face (or
    // takes more than half of a round part), moving straight would put it outside, sticking
    // out past the earlier cut once the model is turned. Such a vertex goes to the nearest
    // point of the cross-section's outline instead.
    let solid = crate::wedge::closed(mesh);
    let section = |plane: Vec4| solid.then(|| Section::new(mesh, plane)).flatten();
    let own = section(plane);
    let other = symmetry.filter(|_| plane.x.abs() > 1e-6).and_then(|_| section(mirrored));
    let project = |v: usize, plane: Vec4, section: Option<&Section>, freedom: f32| {
        let p = mesh.positions[v];
        let n = plane.truncate();
        let distance = n.dot(p) + plane.w;
        if distance <= 0.0 {
            return p;
        }
        let q = p - n * distance;
        let q = section.map_or(q, |s| s.keep_inside(q));
        p + (q - p) * freedom
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
                    match symmetry {
                        Some(side) if plane.x.abs() > 1e-6 => {
                            let (a, b) = (own.as_ref(), other.as_ref());
                            if p.x.abs() <= 1e-6 {
                                // On the symmetry plane both halves meet: the mean of the two
                                // keeps the vertex on it.
                                (project(v, plane, a, freedom) + project(v, mirrored, b, freedom)) * 0.5
                            } else if p.x * side > 0.0 {
                                project(v, plane, a, freedom)
                            } else {
                                project(v, mirrored, b, freedom)
                            }
                        }
                        _ => project(v, plane, own.as_ref(), freedom),
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
            let saved = clip_line(&mut m, screen(), &g, None).unwrap();
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
        clip_line(&mut m, s, &g, None).unwrap();
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
        assert!(clip_line(&mut m, screen(), &g, Some(1.0)).is_none());
        m.mask.fill(128);
        clip_line(&mut m, screen(), &g, Some(1.0)).unwrap();
        for (p, old) in m.positions.iter().zip(&before) {
            if old.y > 0.0 {
                assert!((p.y - old.y * (128.0 / 255.0)).abs() < 1e-6);
            }
        }
        let mut hidden = mesh();
        hidden.set_hidden(vec![1; hidden.triangle_count()]);
        assert!(clip_line(&mut hidden, screen(), &g, None).is_none());
    }

    #[test]
    fn a_symmetric_clip_cuts_each_half_once_and_stays_symmetric() {
        // A view turned and raised off the symmetry plane: the line's plane leans in X.
        let mut camera = crate::camera::Camera::new();
        camera.aspect = 1.0;
        camera.yaw += 0.6;
        camera.pitch += 0.25;
        let s = Screen { matrix: camera.view_proj(), size: Vec2::splat(400.0) };
        let mut g = Gesture::new(BrushKind::ClipLine, Vec2::new(0.0, 120.0), false);
        g.end = Vec2::new(400.0, 120.0);
        let plane = s.plane(g.start, g.end).unwrap();
        assert!(plane.x.abs() > 0.1);
        let mirrored = Vec4::new(-plane.x, plane.y, plane.z, plane.w);
        let side = -plane.x.signum();
        let mut m = mesh();
        let before = m.positions.clone();
        clip_line(&mut m, s, &g, Some(side)).unwrap();
        let at = |q: Vec4, p: Vec3| q.truncate().dot(p) + q.w;
        for (p, old) in m.positions.iter().zip(&before) {
            if old.x.abs() <= 1e-6 {
                // On the symmetry plane: the mean of both cuts keeps it there.
                assert!(p.x.abs() <= 1e-6);
                continue;
            }
            let own = if old.x * side > 0.0 { plane } else { mirrored };
            if at(own, *old) <= 0.0 {
                assert_eq!(p, old, "kept side of its own plane moved");
            } else {
                assert!(at(own, *p).abs() < 1e-5, "cut onto its own plane");
            }
        }
        // Mirror images of each other stay mirror images.
        let key = |p: Vec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
        let index: std::collections::HashMap<_, _> = before.iter().enumerate().map(|(i, p)| (key(*p), i)).collect();
        let mut pairs = 0;
        for (i, p) in before.iter().enumerate() {
            if let Some(&j) = index.get(&key(Vec3::new(-p.x, p.y, p.z))) {
                let (a, b) = (m.positions[i], m.positions[j]);
                assert!(a.distance(Vec3::new(-b.x, b.y, b.z)) < 1e-5, "{a} vs {b}");
                pairs += 1;
            }
        }
        assert!(pairs > before.len() / 2);
    }

    #[test]
    #[ignore]
    fn time_crossing_clips_on_a_million_vertices() {
        let (p, i) = crate::mesh::cube_sphere(400, 1.0);
        let mut m = SculptMesh::from_raw(p, i);
        let s = screen();
        for (a, b) in [(Vec2::new(260.0, 0.0), Vec2::new(260.0, 400.0)), (Vec2::new(60.0, 400.0), Vec2::new(300.0, 0.0)), (Vec2::new(400.0, 300.0), Vec2::new(0.0, 280.0))] {
            let mut g = Gesture::new(BrushKind::ClipLine, a, false);
            g.end = b;
            let t = std::time::Instant::now();
            clip_line(&mut m, s, &g, None).unwrap();
            eprintln!("TIME {} vertices: {:.0} ms", m.vertex_count(), t.elapsed().as_secs_f32() * 1000.0);
        }
    }

    #[test]
    fn crossing_clips_never_push_an_earlier_cut_back_out() {
        // A wedge from three cuts whose removed sides face away from each other, like
        // slicing a pie piece out of a sphere: right, upper left, then bottom.
        let s = screen();
        let cuts = [
            (Vec2::new(260.0, 0.0), Vec2::new(260.0, 400.0)),
            (Vec2::new(60.0, 400.0), Vec2::new(300.0, 0.0)),
            (Vec2::new(400.0, 300.0), Vec2::new(0.0, 280.0)),
        ];
        let mut m = mesh();
        let mut planes = Vec::new();
        for (a, b) in cuts {
            let mut g = Gesture::new(BrushKind::ClipLine, a, false);
            g.end = b;
            planes.push(s.plane(a, b).unwrap());
            clip_line(&mut m, s, &g, None).unwrap();
            // Every vertex stays on the kept side of every cut so far.
            for (k, plane) in planes.iter().enumerate() {
                let worst = m.positions.iter().map(|p| plane.truncate().dot(*p) + plane.w).fold(f32::MIN, f32::max);
                assert!(worst < 1e-4, "after {} cuts, cut {k} was pushed back out by {worst}", planes.len());
            }
        }
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
