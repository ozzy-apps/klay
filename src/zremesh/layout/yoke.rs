//! The yoke: how a box modeller closes the trunk at the shoulders and at the crotch. A wide,
//! shallow grid is laid over the trunk's ring (a cap), rectangular holes are cut out of it
//! where the neck, the arms or the legs leave, and the rows round those are bridged into the
//! holes one to one. The trunk keeps its one count, the count changes happen through the grid,
//! and the only poles are the four at each hole's corners.

use super::*;

/// The cells of a yoke: the grid `a` × `b` (a across the plane, b front to back), the neck
/// hole `neck` (width, depth) in its middle and an arm hole `arm` at each side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Yoke {
    pub a: usize,
    pub b: usize,
    pub neck: (usize, usize),
    pub arm: (usize, usize),
}

/// The widths of the grid tried for a trunk of `half` cells per side, nearest the aspect's
/// `a0` first: even, at least 4 from either end.
fn widths(half: usize, a0: usize) -> Vec<usize> {
    let mut out: Vec<usize> = (2..half).filter(|a| a % 2 == 0 && half - a >= 2).collect();
    out.sort_by_key(|&a| (a.abs_diff(a0), std::cmp::Reverse(a)));
    out
}

/// The grid for a trunk row of `n_trunk` points (in fours), a neck row of `n_neck` (in fours)
/// and arm rows of `n_arm` (even): a + b = n_trunk / 2 with a / b near the ring's width over
/// its depth (`w` over `d`). The neck hole is `a_n` × `b_n` with 2(a_n + b_n) = n_neck, a_n
/// even so it straddles the plane; each arm hole `a_a` × `b_a` with 2(a_a + b_a) = n_arm, its
/// outer column on the grid's side. Holes keep a row of grid in front and behind
/// (depth ≤ b − 2) and a column between them (2 a_a + a_n + 2 ≤ a); the neck hole is at most
/// half the grid wide, an arm hole at least 4. Of what fits, the neck hole is the shallowest
/// (the rows in front of it and behind climb the chest and the back) and the holes the
/// squarest. None if nothing fits.
pub(super) fn yoke_dims(n_trunk: usize, n_neck: usize, n_arm: usize, w: f32, d: f32) -> Option<Yoke> {
    if n_trunk % 4 != 0 || n_neck % 4 != 0 || n_arm % 2 != 0 || n_trunk < 16 || n_neck < 8 || n_arm < 8 {
        return None;
    }
    let half = n_trunk / 2;
    let a0 = ((half as f32 * w / (w + d).max(1e-9) / 2.0).round() as usize * 2).clamp(2, half - 2);
    let (hn, ha) = (n_neck / 2, n_arm / 2);
    for a in widths(half, a0) {
        let b = half - a;
        let mut best: Option<((usize, std::cmp::Reverse<usize>, std::cmp::Reverse<usize>), Yoke)> = None;
        for a_n in (2..hn).filter(|x| x % 2 == 0) {
            let b_n = hn - a_n;
            if b_n == 0 || b_n + 2 > b || 2 * a_n > a {
                continue;
            }
            for a_a in 4..ha {
                let b_a = ha - a_a;
                if b_a == 0 || b_a + 2 > b || 2 * a_a + a_n + 2 > a {
                    continue;
                }
                // Shallow, square; at a tie the wider neck hole, then the wider arm hole.
                let key = (b_n + a_n.abs_diff(b_n) + a_a.abs_diff(b_a), std::cmp::Reverse(a_n), std::cmp::Reverse(a_a));
                let y = Yoke { a, b, neck: (a_n, b_n), arm: (a_a, b_a) };
                if best.is_none_or(|(k, _)| key < k) {
                    best = Some((key, y));
                }
            }
        }
        if let Some((_, y)) = best {
            return Some(y);
        }
    }
    None
}

/// The cells of a crotch cap: the grid `a` × `b` over the trunk's last row and the leg holes
/// (width, depth), one at columns [1, 1 + width) and its mirror image, with the perineum strip
/// (two columns or more) between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Crotch {
    pub a: usize,
    pub b: usize,
    pub leg: (usize, usize),
}

/// The grid for the trunk's last row of `n_in` points (in fours) over legs of `n_leg` (even):
/// a + b = n_in / 2 near the ring's aspect; each hole `a_l` × `b_l` with 2(a_l + b_l) = n_leg,
/// a column of grid outside it, two or more between the two (2 a_l + 4 ≤ a), a row of grid in
/// front and behind, at least 4 wide and as square as it can be. None if nothing fits.
pub(super) fn crotch_dims(n_in: usize, n_leg: usize, w: f32, d: f32) -> Option<Crotch> {
    if n_in % 4 != 0 || n_leg % 2 != 0 || n_in < 16 || n_leg < 8 {
        return None;
    }
    let half = n_in / 2;
    let a0 = ((half as f32 * w / (w + d).max(1e-9) / 2.0).round() as usize * 2).clamp(2, half - 2);
    let hl = n_leg / 2;
    for a in widths(half, a0) {
        let b = half - a;
        let best = (4..hl).filter_map(|a_l| {
            let b_l = hl - a_l;
            (b_l > 0 && b_l + 2 <= b && 2 * a_l + 4 <= a).then_some((a_l.abs_diff(b_l), std::cmp::Reverse(a_l), Crotch { a, b, leg: (a_l, b_l) }))
        }).min_by_key(|x| (x.0, x.1));
        if let Some((_, _, c)) = best {
            return Some(c);
        }
    }
    None
}

/// A part of the working surface: the triangles whose corners all lie in a set of pieces,
/// with their normals and a grid to find the nearest point on them.
pub(super) struct Patch {
    tris: Vec<[u32; 3]>,
    nrm: Vec<Vec3>,
    grid: crate::zremesh::TriGrid,
}

impl Patch {
    /// The triangles of `t` (points `p`, normals `fnrm`) with every corner `inside`, in a grid
    /// of cells four `quad`s across (so the nearest point is found from well inside the
    /// body). None if there are none.
    pub(super) fn new(p: &[Vec3], t: &[[u32; 3]], fnrm: &[Vec3], inside: &dyn Fn(u32) -> bool, quad: f32) -> Option<Self> {
        let keep: Vec<usize> = (0..t.len()).filter(|&f| t[f].iter().all(|&v| inside(v))).collect();
        if keep.is_empty() {
            return None;
        }
        let tris: Vec<[u32; 3]> = keep.iter().map(|&f| t[f]).collect();
        let nrm: Vec<Vec3> = keep.iter().map(|&f| fnrm[f]).collect();
        let grid = crate::zremesh::TriGrid::from_tris(p, &tris, quad * 4.0);
        Some(Patch { tris, nrm, grid })
    }

    /// The nearest point of the patch to `q` and the normal there, if one is within reach.
    pub(super) fn nearest(&self, p: &[Vec3], q: Vec3) -> Option<(Vec3, Vec3)> {
        self.grid.closest_tris(p, &self.tris, q).map(|(c, f)| (c, self.nrm[f as usize]))
    }

    /// Where the line from `q` along `dir`, up to `reach` away, comes nearest the patch: the
    /// patch's point there. Sampled every quarter of `step`. None if the line stays out of
    /// reach of the patch.
    pub(super) fn settle(&self, p: &[Vec3], q: Vec3, dir: Vec3, reach: f32, step: f32) -> Option<Vec3> {
        let n = ((reach / (step * 0.25).max(1e-9)).ceil() as usize).clamp(1, 400);
        let mut best: Option<(f32, Vec3)> = None;
        for k in 0..=n {
            let x = q + dir * (reach * k as f32 / n as f32);
            if let Some((c, _)) = self.nearest(p, x) {
                let d = c.distance(x);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, c));
                }
            }
        }
        best.map(|b| b.1)
    }
}

/// A grid of `a` × b cells over the pinned row `row` (b = n / 2 − a; a even, so a grid line
/// runs along the plane), its inner points settled onto `patch`: a point on the + side of
/// `plane` or on the plane goes to where the line from it along `dir` (up to `reach` away)
/// comes nearest the patch, so the cap climbs onto the shoulders above the trunk's ring or
/// sinks into the crotch below it; the − side's points are the images of the + side's. The
/// grid's point ids (rows front to back) and its cells, taken off `quads` again for the
/// caller to put back the ones it keeps; and whether the high columns are on the + side.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn settle_cap(row: &[u32], a: usize, plane: MirrorPlane, patch: &Patch, p: &[Vec3], dir: Vec3, reach: f32, quad: f32, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Result<(Vec<Vec<u32>>, Vec<[u32; 4]>, bool), String> {
    let b = row.len() / 2 - a;
    if a % 2 != 0 || a < 2 || b < 2 {
        return Err(format!("a cap of {a} x {b} has no inside"));
    }
    let q0 = quads.len();
    let ids = rect::cap_dims(row, a, true, out_p, quads);
    let cells: Vec<[u32; 4]> = quads.split_off(q0);
    let plus_high = plane.distance(out_p[ids[0][a] as usize]) > 0.0;
    let on_plus = |i: usize| if plus_high { i > a / 2 } else { i < a / 2 };
    for j in 1..b {
        for i in (1..a).filter(|&i| on_plus(i) || i == a / 2) {
            let v = ids[j][i] as usize;
            let mut q = patch.settle(p, out_p[v], dir, reach, quad).ok_or("the cap finds no surface")?;
            if i == a / 2 {
                q = plane.project(q);
            }
            out_p[v] = q;
        }
    }
    for j in 1..b {
        for i in (1..a).filter(|&i| !on_plus(i) && i != a / 2) {
            out_p[ids[j][i] as usize] = plane.reflect(out_p[ids[j][a - i] as usize]);
        }
    }
    Ok((ids, cells, plus_high))
}

/// How many of the `cells` not in `skip` face the wrong way: away from the patch under their
/// middles, or with no area. The grid may be wound either way, so the side the fewer face
/// is the wrong one. (The wrong-facing count, the count checked.)
pub(super) fn inward(cells: &[[u32; 4]], skip: &HashSet<usize>, patch: &Patch, p: &[Vec3], out_p: &[Vec3], quad: f32) -> (usize, usize) {
    let (mut with, mut against, mut flat) = (0usize, 0usize, 0usize);
    for (k, c) in cells.iter().enumerate() {
        if skip.contains(&k) {
            continue;
        }
        let [p0, p1, p2, p3] = c.map(|v| out_p[v as usize]);
        let n = (p2 - p0).cross(p3 - p1);
        let Some((_, sn)) = patch.nearest(p, (p0 + p1 + p2 + p3) * 0.25) else { continue };
        if n.length() < quad * quad * 0.01 {
            flat += 1;
        } else if n.dot(sn) > 0.0 {
            with += 1;
        } else {
            against += 1;
        }
    }
    (with.min(against) + flat, with + against + flat)
}

/// The border of the block of cells [`i0`, `i0` + `w`) × [`j0`, `j0` + `h`) of the grid of
/// point ids `ids` (rows along j): the loop from its (j0, i0) corner along +i, then +j, then
/// −i, then −j; 2(w + h) points.
pub(super) fn hole_border(ids: &[Vec<u32>], i0: usize, j0: usize, w: usize, h: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(2 * (w + h));
    out.extend((0..w).map(|k| ids[j0][i0 + k]));
    out.extend((0..h).map(|k| ids[j0 + k][i0 + w]));
    out.extend((0..w).map(|k| ids[j0 + h][i0 + w - k]));
    out.extend((0..h).map(|k| ids[j0 + h - k][i0]));
    out
}

/// The cells of the block [`i0`, `i0` + `w`) × [`j0`, `j0` + `h`) as indices into a grid of
/// `a` cells per row.
pub(super) fn hole_cells(a: usize, i0: usize, j0: usize, w: usize, h: usize) -> Vec<usize> {
    (j0..j0 + h).flat_map(|j| (i0..i0 + w).map(move |i| j * a + i)).collect()
}

/// `lp` turned to start at index `k`, and run the other way round when `reverse`.
pub(super) fn turned(lp: &[u32], k: usize, reverse: bool) -> Vec<u32> {
    let n = lp.len();
    if reverse {
        (0..n).map(|m| lp[(k + n - m) % n]).collect()
    } else {
        (0..n).map(|m| lp[(k + m) % n]).collect()
    }
}

/// How far apart two loops of one count are when the second is turned to lie best against the
/// first: the least sum of squared distances over its turns.
pub(super) fn loop_cost(a: &[Vec3], b: &[Vec3]) -> f32 {
    let n = a.len();
    if n == 0 || b.len() != n {
        return f32::INFINITY;
    }
    (0..n).map(|sh| (0..n).map(|i| a[i].distance_squared(b[(i + sh) % n])).sum::<f32>()).fold(f32::INFINITY, f32::min)
}

/// A band between a hole's border and a limb's row of the same count on the + side of `plane`,
/// and its mirror image between their images: `border_m[k]` is the image of `border[k]`; the
/// row's image is made here, the row reflected and run the other way, so it keeps its ring's
/// sense. The border is run the way round that lies against the row (the other way it would
/// twist), and the row turned to it by `join_band`. Returns the row's image.
pub(super) fn bridge_mirrored(border: &[u32], border_m: &[u32], row: &[u32], plane: MirrorPlane, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Vec<u32> {
    let n = row.len();
    let row_m: Vec<u32> = (0..n).map(|i| {
        out_p.push(plane.reflect(out_p[row[(n - i) % n] as usize]));
        (out_p.len() - 1) as u32
    }).collect();
    let pos = |ids: &[u32], out_p: &[Vec3]| -> Vec<Vec3> { ids.iter().map(|&v| out_p[v as usize]).collect() };
    let row_pos = pos(row, out_p);
    let fwd = pos(border, out_p);
    let back: Vec<Vec3> = fwd.iter().rev().copied().collect();
    let reverse = loop_cost(&back, &row_pos) < loop_cost(&fwd, &row_pos);
    let b: Vec<u32> = if reverse { border.iter().rev().copied().collect() } else { border.to_vec() };
    let mut mir: HashMap<u32, u32> = HashMap::default();
    for k in 0..border.len() {
        mir.insert(border[k], border_m[k]);
    }
    for i in 0..n {
        mir.insert(row[i], row_m[(n - i) % n]);
    }
    let mut half: Vec<[u32; 4]> = Vec::new();
    join_band(&b, row, out_p, &mut half);
    for q in half {
        quads.push(q);
        let m = q.map(|v| mir[&v]);
        quads.push([m[1], m[0], m[3], m[2]]);
    }
    row_m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symmetry::Axis;

    #[test]
    fn yoke_dims_for_the_t_pose_at_10k() {
        // Trunk 56, neck 24, arms 20, a ring twice as wide as deep: 18 x 10, holes 6 x 6 and
        // 5 x 5 (5 + 1 + 6 + 1 + 5 = 18).
        let y = yoke_dims(56, 24, 20, 0.44, 0.24).expect("fits");
        assert_eq!(y, Yoke { a: 18, b: 10, neck: (6, 6), arm: (5, 5) });
        // Neck 28: the shallower 8 x 6 hole (7 rows in front of it on a 14-deep grid), the
        // arms 4 x 6 beside it.
        let y = yoke_dims(64, 28, 20, 0.40, 0.29).expect("fits");
        assert_eq!(y, Yoke { a: 18, b: 14, neck: (8, 6), arm: (4, 6) });
        // Neck 32: an 8 x 8 hole, the arms 4 x 6. Neck 36 or 40 over a trunk of 56 would need
        // a hole over half the grid wide or arm holes under 4.
        let y = yoke_dims(56, 32, 20, 0.44, 0.24).expect("fits");
        assert_eq!(y, Yoke { a: 18, b: 10, neck: (8, 8), arm: (4, 6) });
        assert!(yoke_dims(56, 36, 20, 0.44, 0.24).is_none());
        assert!(yoke_dims(56, 40, 20, 0.44, 0.24).is_none());
        // Counts not in fours (a pinned row) are refused.
        assert!(yoke_dims(54, 24, 20, 0.44, 0.24).is_none());
        assert!(yoke_dims(56, 26, 20, 0.44, 0.24).is_none());
    }

    #[test]
    fn crotch_dims_for_the_t_pose_at_10k() {
        let c = crotch_dims(56, 30, 0.36, 0.2).expect("fits");
        assert_eq!(c, Crotch { a: 18, b: 10, leg: (7, 8) });
        // Narrower hips: a 16-wide grid takes 6 x 9 holes.
        let c = crotch_dims(56, 30, 0.3, 0.26).expect("fits");
        assert_eq!(c.a + c.b, 28);
        assert_eq!(2 * (c.leg.0 + c.leg.1), 30);
        assert!(2 * c.leg.0 + 4 <= c.a && c.leg.1 + 2 <= c.b);
        assert!(crotch_dims(56, 31, 0.36, 0.2).is_none());
    }

    #[test]
    fn hole_border_runs_round_the_block() {
        let ids: Vec<Vec<u32>> = (0..5).map(|j| (0..7).map(|i| (j * 7 + i) as u32).collect()).collect();
        let lp = hole_border(&ids, 2, 1, 3, 2);
        assert_eq!(lp, vec![9, 10, 11, 12, 19, 26, 25, 24, 23, 16]);
        assert_eq!(hole_cells(6, 2, 1, 3, 2), vec![8, 9, 10, 14, 15, 16]);
        assert_eq!(turned(&[1, 2, 3, 4], 1, false), vec![2, 3, 4, 1]);
        assert_eq!(turned(&[1, 2, 3, 4], 1, true), vec![2, 1, 4, 3]);
    }

    #[test]
    fn mirrored_bridge_lies_flat_either_way_round() {
        let plane = MirrorPlane { axis: Axis::X, at: 0.0 };
        for reverse in [false, true] {
            let mut p: Vec<Vec3> = Vec::new();
            // A square hole border on the + side at y = 0, and its image.
            let sq = [Vec3::new(1.0, 0.0, 1.0), Vec3::new(2.0, 0.0, 1.0), Vec3::new(3.0, 0.0, 1.0), Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 0.0, -1.0), Vec3::new(2.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0)];
            let border: Vec<u32> = sq.iter().map(|&q| { p.push(q); (p.len() - 1) as u32 }).collect();
            let border_m: Vec<u32> = sq.iter().map(|&q| { p.push(plane.reflect(q)); (p.len() - 1) as u32 }).collect();
            // The limb's row a quad above, round the same way or the other.
            let mut row: Vec<u32> = sq.iter().map(|&q| { p.push(q + Vec3::Y); (p.len() - 1) as u32 }).collect();
            if reverse {
                row.reverse();
            }
            let mut quads = Vec::new();
            let row_m = bridge_mirrored(&border, &border_m, &row, plane, &mut p, &mut quads);
            assert_eq!(quads.len(), 16);
            assert_eq!(row_m.len(), 8);
            // Every quad is flat and square: its corners one apart, no diagonal across.
            for q in &quads {
                for k in 0..4 {
                    let d = p[q[k] as usize].distance(p[q[(k + 1) % 4] as usize]);
                    assert!((d - 1.0).abs() < 1e-5, "reverse {reverse}: quad {q:?} has an edge of {d}");
                }
            }
            // And the - side is the exact image of the + side.
            for q in quads.iter().step_by(2).zip(quads.iter().skip(1).step_by(2)) {
                for k in 0..4 {
                    let want = plane.reflect(p[q.0[k] as usize]);
                    assert!(q.1.iter().any(|&v| p[v as usize].distance(want) < 1e-6));
                }
            }
        }
    }
}
