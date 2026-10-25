//! Curve strokes (ZBrush: Stroke › Curve).
//!
//! A freehand stroke follows every wobble of the hand: each dab or relief segment takes
//! its direction from two cursor samples a few pixels apart, so a Zipper's teeth tilt and
//! its spacing stretches wherever the hand shakes. In Curve mode the stroke is first drawn
//! as a path on the screen; on release the path is smoothed, resampled at even steps and
//! laid onto the surface as a [`Curve`]. The brush is then applied along that curve:
//! dab brushes at even steps, detail reliefs (Zipper, Stitch, Rake) as one pattern
//! measured in the curve's own frame (see [`band`]), so every vertex is set once from its
//! nearest point on a smooth line. The curve stays on screen and can be bent.

use crate::mesh::{Group, Selection, SculptMesh, split_ranges};
use glam::{Vec2, Vec3};
use rayon::prelude::*;

/// Whether `kind` can stroke along a curve: every brush that works in dabs or reliefs, not
/// the drag brushes (they pull what they picked up) nor shape tools and masks.
/// Insert brushes (CurveTube) are left out: they always draw a curve (see [`always`]).
pub fn capable(kind: crate::sculpt::BrushKind) -> bool {
    !kind.is_drag() && !kind.is_shape() && !kind.is_mask() && !kind.is_insert() && !kind.is_imm() && !kind.is_zmodeler()
}

/// Brushes that only work along a curve (ZBrush's curve insert brushes, such as CurveTube).
pub fn always(kind: crate::sculpt::BrushKind) -> bool {
    kind.is_insert()
}

/// Brushes that start in Curve mode, as ZBrush's zipper and stitch brushes do.
pub const CURVE_BY_DEFAULT: [crate::sculpt::BrushKind; 3] =
    [crate::sculpt::BrushKind::Zipper, crate::sculpt::BrushKind::Stitch, crate::sculpt::BrushKind::Rake];

/// `path` resampled at even arc-length steps of `step` (both ends kept).
pub fn resample(path: &[Vec2], step: f32) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::new();
    let Some(&first) = path.first() else {
        return out;
    };
    out.push(first);
    let step = step.max(1e-3);
    let mut carried = 0.0; // arc length since the last output sample
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        if len <= 0.0 {
            continue;
        }
        let mut t = step - carried;
        while t <= len {
            out.push(a + (b - a) * (t / len));
            t += step;
        }
        carried = len - (t - step);
    }
    let last = *path.last().unwrap();
    if out.last().is_some_and(|p| p.distance(last) > step * 0.25) || out.len() == 1 {
        out.push(last);
    } else if let Some(p) = out.last_mut() {
        *p = last;
    }
    out.dedup_by(|a, b| a.distance(*b) < 1e-6);
    out
}

/// The screen path of a stroke made smooth: resampled finely, Gaussian-smoothed along its
/// length with a spread of `smooth` pixels, then resampled every `step` pixels.
///
/// A light first pass takes out fast jitter before the main one: the smoothing window is
/// measured along the path, and a shaky path is much longer than the line it follows, so a
/// single pass would smooth a shaky stroke less (and round its bends less) than a steady
/// one. Each end is placed by [`gauss`]'s end fit.
pub fn smooth_path(path: &[Vec2], smooth: f32, step: f32) -> Vec<Vec2> {
    // Fine enough not to alias hand jitter a few pixels long.
    let fine = (smooth / 6.0).clamp(0.25, 1.0);
    let u = resample(path, fine);
    if u.len() < 3 || smooth <= 0.0 {
        return resample(&u, step);
    }
    let pre = gauss(&u, (smooth / 4.0).clamp(1.0, 6.0), fine, false);
    let u = resample(&pre, fine);
    if u.len() < 3 {
        return resample(&u, step);
    }
    resample(&gauss(&u, smooth, fine, true), step)
}

/// Gaussian smoothing of a path sampled every `fine` pixels, with a spread of `smooth`
/// pixels. Past each end the path is mirrored through the end point, so a straight run
/// stays straight right up to it. With `fit_ends`, each end is first moved onto the line
/// through its first stretch's weighted mean: it keeps the drawn end's place along the
/// stroke but takes the averaged line's place across it, so the wobble of a pen landing or
/// lifting does not steer the first teeth.
fn gauss(u: &[Vec2], smooth: f32, fine: f32, fit_ends: bool) -> Vec<Vec2> {
    let sigma = smooth / fine;
    let reach = (sigma * 2.5).ceil() as usize;
    let weights: Vec<f32> = (0..=reach).map(|k| (-(k as f32 / sigma).powi(2) * 0.5).exp()).collect();
    let n = u.len();
    let end_point = |pts: &[Vec2]| -> Vec2 {
        let m = pts.len().min(reach + 1);
        if m < 4 {
            return pts[0];
        }
        let (mut total, mut sum) = (0.0f32, Vec2::ZERO);
        for k in 0..m {
            total += weights[k];
            sum += pts[k] * weights[k];
        }
        let mean = sum / total;
        let half = m / 2;
        let near: Vec2 = pts[..half].iter().sum::<Vec2>() / half as f32;
        let far: Vec2 = pts[half..m].iter().sum::<Vec2>() / (m - half) as f32;
        let dir = (far - near).normalize_or_zero();
        mean + dir * (pts[0] - mean).dot(dir)
    };
    let mut u = u.to_vec();
    if fit_ends {
        let reversed: Vec<Vec2> = u.iter().rev().take(reach + 1).copied().collect();
        let (first, last) = (end_point(&u), end_point(&reversed));
        u[0] = first;
        u[n - 1] = last;
    }
    let u = u;
    // Point `j` of the path extended by odd reflection through both ends.
    let at = |j: isize| -> Vec2 {
        let last = n as isize - 1;
        if j < 0 {
            let k = (-j).min(last) as usize;
            u[0] * 2.0 - u[k]
        } else if j > last {
            let k = (last - (j - last)).max(0) as usize;
            u[n - 1] * 2.0 - u[k]
        } else {
            u[j as usize]
        }
    };
    (0..n)
        .into_par_iter()
        .map(|i| {
            let i = i as isize;
            let (mut sum, mut total) = (u[i as usize] * weights[0], weights[0]);
            for k in 1..=reach as isize {
                sum += (at(i - k) + at(i + k)) * weights[k as usize];
                total += 2.0 * weights[k as usize];
            }
            sum / total
        })
        .collect()
}

/// A stroke laid onto the surface: points about `spacing` apart with the surface normal,
/// the unit tangent (in the surface) and the arc length at each. `pieces` are the runs
/// that stayed on the surface; a stroke that leaves the model is split there.
#[derive(Clone, Debug, Default)]
pub struct Curve {
    pub points: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub tangents: Vec<Vec3>,
    pub along: Vec<f32>,
    pub pieces: Vec<std::ops::Range<usize>>,
}

impl Curve {
    /// From surface hits (point, normal) of a smoothed screen path, `None` where the path
    /// left the surface. Each run is resampled at `spacing` in the world.
    pub fn from_hits(hits: &[Option<(Vec3, Vec3)>], spacing: f32) -> Self {
        let mut c = Curve::default();
        let mut run: Vec<(Vec3, Vec3)> = Vec::new();
        for h in hits.iter().chain(std::iter::once(&None)) {
            match h {
                Some(h) => run.push(*h),
                None => {
                    if !run.is_empty() {
                        c.add_piece(&run, spacing);
                    }
                    run.clear();
                }
            }
        }
        c
    }

    fn add_piece(&mut self, run: &[(Vec3, Vec3)], spacing: f32) {
        let start = self.points.len();
        // Resample by world arc length, interpolating normals.
        let mut pts = vec![run[0]];
        let mut carried = 0.0;
        for w in run.windows(2) {
            let ((a, na), (b, nb)) = (w[0], w[1]);
            let len = a.distance(b);
            if len <= 0.0 {
                continue;
            }
            let mut t = spacing - carried;
            while t <= len {
                let f = t / len;
                pts.push((a.lerp(b, f), na.lerp(nb, f)));
                t += spacing;
            }
            carried = len - (t - spacing);
        }
        let last = *run.last().unwrap();
        if pts.last().unwrap().0.distance(last.0) > spacing * 0.25 {
            pts.push(last);
        } else if pts.len() > 1 {
            *pts.last_mut().unwrap() = last;
        }
        let n = pts.len();
        // Normals averaged over a few neighbours; tangents from central differences,
        // kept in the surface's tangent plane.
        let normals: Vec<Vec3> = (0..n)
            .map(|i| {
                let lo = i.saturating_sub(2);
                let hi = (i + 2).min(n - 1);
                pts[lo..=hi].iter().map(|p| p.1).sum::<Vec3>().normalize_or(pts[i].1)
            })
            .collect();
        let raw: Vec<Vec3> = (0..n)
            .map(|i| {
                let (a, b) = (pts[i.saturating_sub(1)].0, pts[(i + 1).min(n - 1)].0);
                let t = b - a;
                (t - normals[i] * normals[i].dot(t)).normalize_or_zero()
            })
            .collect();
        let tangents: Vec<Vec3> = (0..n)
            .map(|i| {
                let lo = i.saturating_sub(1);
                let hi = (i + 1).min(n - 1);
                let t: Vec3 = raw[lo..=hi].iter().sum();
                let t = t - normals[i] * normals[i].dot(t);
                t.normalize_or(raw[i])
            })
            .collect();
        let mut s = 0.0;
        for i in 0..n {
            if i > 0 {
                s += pts[i].0.distance(pts[i - 1].0);
            }
            self.points.push(pts[i].0);
            self.along.push(s);
        }
        self.normals.extend(normals);
        self.tangents.extend(tangents);
        self.pieces.push(start..start + n);
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Total length of every piece.
    #[cfg(test)]
    pub fn length(&self) -> f32 {
        self.pieces.iter().map(|p| self.along[p.end - 1]).sum()
    }

    /// Segments `(i, i + 1)` within pieces.
    fn segments(&self) -> impl Iterator<Item = usize> + '_ {
        self.pieces.iter().flat_map(|p| p.start..p.end.saturating_sub(1))
    }

    /// The curve under an orthogonal map through the origin (a symmetry image).
    pub fn transformed(&self, map: &glam::Mat3) -> Curve {
        let m = |v: &Vec3| *map * *v;
        Curve {
            points: self.points.iter().map(m).collect(),
            normals: self.normals.iter().map(m).collect(),
            tangents: self.tangents.iter().map(m).collect(),
            along: self.along.clone(),
            pieces: self.pieces.clone(),
        }
    }
}

/// Where a vertex sits relative to the curve: arc length of its nearest point, signed
/// distance across (positive to the left of the direction of travel, seen from outside),
/// and the curve's normal there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Foot {
    pub along: f32,
    pub across: f32,
    pub point: Vec3,
    pub normal: Vec3,
}

/// Nearest point of `curve` to `p` among `segs` (segment start indices), or `None` when
/// it lies beyond an end by more than `overhang`, more than `reach` across, or more than
/// `depth` off the surface (the far side of a thin part).
fn foot(curve: &Curve, segs: &[usize], p: Vec3, reach: f32, depth: f32, overhang: f32) -> Option<Foot> {
    let mut best: Option<(f32, usize, f32)> = None;
    for &i in segs {
        let (a, b) = (curve.points[i], curve.points[i + 1]);
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
        let d2 = (a + ab * t).distance_squared(p);
        if best.is_none_or(|b| d2 < b.0) {
            best = Some((d2, i, t));
        }
    }
    let (_, i, t) = best?;
    let point = curve.points[i].lerp(curve.points[i + 1], t);
    let normal = curve.normals[i].lerp(curve.normals[i + 1], t).normalize_or(curve.normals[i]);
    let tangent = curve.tangents[i].lerp(curve.tangents[i + 1], t).normalize_or(curve.tangents[i]);
    let offset = p - point;
    // Past an end the nearest point is the end itself: allow only a short overhang.
    let first = curve.pieces.iter().any(|r| r.start == i) && t == 0.0;
    let last = curve.pieces.iter().any(|r| r.end == i + 2) && t == 1.0;
    let beyond = offset.dot(tangent);
    if (first && beyond < -overhang) || (last && beyond > overhang) {
        return None;
    }
    let across = offset.dot(normal.cross(tangent));
    if across.abs() > reach || offset.dot(normal).abs() > depth {
        return None;
    }
    let along = curve.along[i] + (curve.along[i + 1] - curve.along[i]) * t + if first || last { beyond } else { 0.0 };
    Some(Foot { along, across, point, normal })
}

/// The vertices within `reach` across `curve` (and across each of its symmetry `images`),
/// each with its [`Foot`] measured on the image it belongs to (the selection's image).
/// Chunks far from the curve are skipped by their bounds; each vertex looks only at the
/// segments near its chunk.
pub fn band(mesh: &SculptMesh, curve: &Curve, reach: f32, images: &crate::symmetry::Images) -> (Selection, Vec<Foot>) {
    let curves: Vec<Curve> = std::iter::once(curve.clone())
        .chain((1..images.len()).map(|k| curve.transformed(&images.0[k])))
        .collect();
    let (depth, overhang) = (reach * 0.65, reach * 0.04);
    let shown = &mesh.shown;
    let near = |c: &Curve, ch: &crate::mesh::Chunk| -> Vec<usize> {
        c.segments()
            .filter(|&i| {
                let (a, b) = (c.points[i], c.points[i + 1]);
                ch.aabb.intersects_sphere((a + b) * 0.5, reach + a.distance(b) * 0.5)
            })
            .collect()
    };
    let per_chunk: Vec<(u32, Vec<(u32, u8, Foot)>)> = mesh
        .chunks
        .par_iter()
        .enumerate()
        .filter_map(|(ci, ch)| {
            let lists: Vec<Vec<usize>> = curves.iter().map(|c| near(c, ch)).collect();
            if lists.iter().all(|l| l.is_empty()) {
                return None;
            }
            let hits: Vec<(u32, u8, Foot)> = (ch.vert_start..ch.vert_end)
                .filter_map(|v| {
                    if !shown.is_empty() && shown[v as usize] == 0 {
                        return None;
                    }
                    let p = mesh.positions[v as usize];
                    curves
                        .iter()
                        .zip(&lists)
                        .enumerate()
                        .filter(|(_, (_, l))| !l.is_empty())
                        .filter_map(|(k, (c, l))| {
                            // A mirror flips handedness: negate across so the pattern is the
                            // mirror image of the original's (point and normal stay on the
                            // image, where the vertex moves).
                            foot(c, l, p, reach, depth, overhang)
                                .map(|f| (k, if images.mirrors(k) { Foot { across: -f.across, ..f } } else { f }))
                        })
                        .min_by(|a, b| a.1.point.distance(p).total_cmp(&b.1.point.distance(p)))
                        .map(|(k, f)| (v, k as u8, f))
                })
                .collect();
            (!hits.is_empty()).then_some((ci as u32, hits))
        })
        .collect();
    let mut groups = Vec::with_capacity(per_chunk.len());
    let mut total = 0u32;
    for (chunk, hits) in &per_chunk {
        groups.push(Group { chunk: *chunk, start: total, end: total + hits.len() as u32 });
        total += hits.len() as u32;
    }
    let n = total as usize;
    let (mut verts, mut dist, mut image) = (vec![0u32; n], vec![0f32; n], vec![0u8; n]);
    let mut feet = vec![Foot { along: 0.0, across: 0.0, point: Vec3::ZERO, normal: Vec3::Z }; n];
    let ranges = || groups.iter().map(|g| (g.start as usize, g.end as usize));
    split_ranges(&mut verts, ranges())
        .into_par_iter()
        .zip(split_ranges(&mut dist, ranges()).into_par_iter())
        .zip(split_ranges(&mut image, ranges()).into_par_iter())
        .zip(split_ranges(&mut feet, ranges()).into_par_iter())
        .zip(per_chunk.par_iter())
        .for_each(|((((vs, ds), ss), fs), (_, hits))| {
            for (i, (v, k, f)) in hits.iter().enumerate() {
                vs[i] = *v;
                ds[i] = f.across.abs();
                ss[i] = *k;
                fs[i] = *f;
            }
        });
    let image = if images.len() > 1 { image } else { Vec::new() };
    (Selection { verts, dist, groups, image }, feet)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight line with high-frequency hand jitter of `amp` pixels.
    fn shaky(len: f32, amp: f32) -> Vec<Vec2> {
        (0..=(len as usize))
            .map(|i| {
                let x = i as f32;
                let jitter = (x * 1.7).sin() * amp + (x * 0.93 + 1.0).sin() * amp * 0.6;
                Vec2::new(x, 100.0 + jitter)
            })
            .collect()
    }

    #[test]
    fn resampling_is_even_and_keeps_both_ends() {
        let path = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 7.0)];
        let r = resample(&path, 2.0);
        assert_eq!(r[0], Vec2::ZERO);
        assert_eq!(*r.last().unwrap(), Vec2::new(10.0, 7.0));
        for w in r.windows(2).take(r.len() - 2) {
            // Corner-cut chords are a little shorter than the step; never longer.
            assert!(w[0].distance(w[1]) <= 2.0 + 1e-4);
        }
        assert_eq!(r.len(), 10);
        assert_eq!(resample(&[Vec2::ONE], 2.0), vec![Vec2::ONE]);
    }

    #[test]
    fn smoothing_takes_out_hand_jitter_and_keeps_the_ends() {
        let path = shaky(300.0, 3.0);
        let s = smooth_path(&path, 12.0, 3.0);
        // The ends stay at the start and end of the line, without the hand's wobble.
        assert!((s[0] - Vec2::new(0.0, 100.0)).length() < 1.0, "{:?}", s[0]);
        assert!((*s.last().unwrap() - Vec2::new(300.0, 100.0)).length() < 1.0, "{:?}", s.last());
        // Away from the ends the line is straight again.
        let inner: Vec<f32> = s.iter().filter(|p| p.x > 40.0 && p.x < 260.0).map(|p| (p.y - 100.0).abs()).collect();
        let worst = inner.iter().copied().fold(0.0f32, f32::max);
        assert!(worst < 0.3, "jitter left: {worst} px");
        // A deliberate bend (a quarter circle of radius 80 px) survives.
        let arc: Vec<Vec2> = (0..=90).map(|d| {
            let a = (d as f32).to_radians();
            Vec2::new(80.0 * a.cos(), 80.0 * a.sin())
        }).collect();
        let s = smooth_path(&arc, 12.0, 3.0);
        let off = s.iter().map(|p| (p.length() - 80.0).abs()).fold(0.0f32, f32::max);
        assert!(off < 2.0, "bend flattened by {off} px");
    }

    /// A curve along +X on the plane z = 0 (normal +Z).
    fn line(from: f32, to: f32) -> Curve {
        let hits: Vec<_> = (0..=20).map(|i| Some((Vec3::new(from + (to - from) * i as f32 / 20.0, 0.0, 0.0), Vec3::Z))).collect();
        Curve::from_hits(&hits, 0.01)
    }

    #[test]
    fn a_curve_is_even_and_measured_by_arc_length() {
        let c = line(-0.5, 0.5);
        assert_eq!(c.pieces.len(), 1);
        assert!((c.length() - 1.0).abs() < 1e-4);
        for w in c.points.windows(2) {
            assert!((w[0].distance(w[1]) - 0.01).abs() < 1e-3);
        }
        assert!(c.tangents.iter().all(|t| (*t - Vec3::X).length() < 1e-4));
        // Leaving the surface splits it.
        let mut hits: Vec<_> = (0..10).map(|i| Some((Vec3::new(i as f32 * 0.1, 0.0, 0.0), Vec3::Z))).collect();
        hits[5] = None;
        assert_eq!(Curve::from_hits(&hits, 0.01).pieces.len(), 2);
    }

    #[test]
    fn feet_measure_along_and_across_and_stop_at_the_ends() {
        let c = line(-0.5, 0.5);
        let segs: Vec<usize> = c.segments().collect();
        let f = foot(&c, &segs, Vec3::new(0.1, 0.2, 0.0), 0.5, 0.3, 0.02).unwrap();
        assert!((f.along - 0.6).abs() < 1e-4 && (f.across - 0.2).abs() < 1e-4, "{f:?}");
        // Normal x tangent = Z x X = Y: positive across is +Y.
        let f = foot(&c, &segs, Vec3::new(0.1, -0.2, 0.0), 0.5, 0.3, 0.02).unwrap();
        assert!((f.across + 0.2).abs() < 1e-4);
        assert!(foot(&c, &segs, Vec3::new(0.1, 0.6, 0.0), 0.5, 0.3, 0.02).is_none(), "too far across");
        assert!(foot(&c, &segs, Vec3::new(0.6, 0.0, 0.0), 0.5, 0.3, 0.02).is_none(), "past the end");
        assert!(foot(&c, &segs, Vec3::new(0.51, 0.0, 0.0), 0.5, 0.3, 0.02).is_some(), "a short overhang");
        assert!(foot(&c, &segs, Vec3::new(0.0, 0.0, 0.4), 0.5, 0.3, 0.02).is_none(), "the far side");
    }

    #[test]
    fn the_band_covers_each_vertex_once_with_its_nearest_foot() {
        let (p, i) = crate::primitive::Primitive::Plane.build(80, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let c = line(-0.5, 0.5);
        let (sel, feet) = band(&mesh, &c, 0.1, &crate::symmetry::Images::identity());
        assert!(!sel.is_empty());
        let mut seen = std::collections::HashSet::new();
        for (k, &v) in sel.verts.iter().enumerate() {
            assert!(seen.insert(v));
            let p = mesh.positions[v as usize];
            assert!(p.y.abs() <= 0.1 + 1e-5 && p.x >= -0.5 - 0.005 && p.x <= 0.5 + 0.005);
            assert!((feet[k].across - p.y).abs() < 1e-4);
            assert!((feet[k].along - (p.x + 0.5).clamp(0.0, 1.0)).abs() < 5e-3);
        }
        // Every vertex of the strip is in it.
        let expect = mesh.positions.iter().filter(|p| p.y.abs() <= 0.0999 && p.x.abs() <= 0.4999).count();
        assert!(sel.len() >= expect);
        // With symmetry, a curve on +X also selects its mirror image on -X, side -1.
        let c = line(0.3, 0.7);
        let (sel, feet) = band(&mesh, &c, 0.05, &crate::symmetry::Symmetry::from(true).images());
        let mirrored: Vec<usize> = (0..sel.len()).filter(|&k| sel.side(k) < 0.0).collect();
        assert!(!mirrored.is_empty() && mirrored.len() < sel.len());
        for k in mirrored {
            assert!(mesh.positions[sel.verts[k] as usize].x < 0.0);
            assert!(feet[k].point.x < 0.0);
        }
    }
}
