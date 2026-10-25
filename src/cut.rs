//! Cutting a mesh along a screen gesture (ZBrush's Trim, Slice and Knife brushes).
//!
//! Line cuts (Trim Curve, Slice Curve, Knife Curve) are exact: every visible triangle the
//! cut crosses is split along it, so the new edges lie on the cut, and the opening a Trim or
//! Knife leaves is closed with a flat cap (ear clipping in the cut's plane). Trim Rect,
//! Circle and Lasso keep what is inside an outline drawn through the whole model: the model
//! is intersected with (or, with Alt, cut by) a prism built from the outline, through the
//! boolean remesher, as ZBrush trims a DynaMesh.
use crate::mesh::SculptMesh;
use glam::{Vec2, Vec3};
use std::collections::HashMap;

/// What a line cut does with the side the cut's arrow points to (where `d > 0`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineCut {
    /// Remove that side and close the opening flat.
    Trim,
    /// Give that side's polygons a new polygroup.
    Slice,
    /// Separate the two sides along the cut and close both: two pieces, two polygroups.
    Knife,
}

/// Raw arrays after a split, with each triangle's side of the cut (+1, or −1 for the rest).
struct Raw {
    pos: Vec<Vec3>,
    col: Vec<[u8; 4]>,
    idx: Vec<u32>,
    grp: Vec<u16>,
    side: Vec<i8>,
    /// The vertex lies on the cut.
    on: Vec<bool>,
}

/// Split every visible triangle that the signed distance `d` (one value per vertex) crosses,
/// so that no triangle straddles zero. Hidden triangles are left whole on the kept side.
fn split(mesh: &SculptMesh, d: &[f32]) -> Raw {
    let scale = {
        let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        (hi - lo).length().max(1e-6)
    };
    let eps = scale * 1e-6;
    let mut d: Vec<f32> = d.iter().map(|&x| if x.abs() < eps { 0.0 } else { x }).collect();
    let mut pos = mesh.positions.clone();
    let mut col = mesh.colors.clone();
    let painted = !col.is_empty();
    let mut idx = Vec::with_capacity(mesh.indices.len() + 64);
    let mut grp = Vec::with_capacity(mesh.triangle_count() + 32);
    let mut side = Vec::with_capacity(mesh.triangle_count() + 32);
    let mut crossing: HashMap<(u32, u32), u32> = HashMap::new();
    let mut cross = |a: u32, b: u32, pos: &mut Vec<Vec3>, col: &mut Vec<[u8; 4]>, d: &mut Vec<f32>| -> u32 {
        let key = (a.min(b), a.max(b));
        if let Some(&v) = crossing.get(&key) {
            return v;
        }
        let (da, db) = (d[a as usize], d[b as usize]);
        let t = (da / (da - db)).clamp(0.0, 1.0);
        let v = pos.len() as u32;
        pos.push(pos[a as usize].lerp(pos[b as usize], t));
        if painted {
            let (ca, cb) = (col[a as usize], col[b as usize]);
            col.push(std::array::from_fn(|k| (ca[k] as f32 + (cb[k] as f32 - ca[k] as f32) * t).round() as u8));
        }
        d.push(0.0);
        crossing.insert(key, v);
        v
    };
    let sign = |x: f32| if x > 0.0 { 1i8 } else if x < 0.0 { -1 } else { 0 };
    for t in 0..mesh.triangle_count() {
        let tri = [mesh.indices[3 * t], mesh.indices[3 * t + 1], mesh.indices[3 * t + 2]];
        let g = mesh.group(t as u32);
        if !mesh.visible(t as u32) {
            idx.extend_from_slice(&tri);
            grp.push(g);
            side.push(-1);
            continue;
        }
        let s = tri.map(|v| sign(d[v as usize]));
        let (pos_n, neg_n) = (s.iter().filter(|&&x| x > 0).count(), s.iter().filter(|&&x| x < 0).count());
        if pos_n == 0 || neg_n == 0 {
            idx.extend_from_slice(&tri);
            grp.push(g);
            side.push(if pos_n > 0 { 1 } else { -1 });
            continue;
        }
        let mut push = |a: u32, b: u32, c: u32, sd: i8| {
            idx.extend_from_slice(&[a, b, c]);
            grp.push(g);
            side.push(sd);
        };
        if let Some(z) = (0..3).find(|&k| s[k] == 0) {
            // One corner on the cut, the other two on either side.
            let (a, b) = (tri[(z + 1) % 3], tri[(z + 2) % 3]);
            let m = cross(a, b, &mut pos, &mut col, &mut d);
            push(tri[z], a, m, s[(z + 1) % 3]);
            push(tri[z], m, b, s[(z + 2) % 3]);
        } else {
            // One corner alone on its side.
            let l = (0..3).find(|&k| s[k] != s[(k + 1) % 3] && s[k] != s[(k + 2) % 3]).unwrap();
            let (lv, a, b) = (tri[l], tri[(l + 1) % 3], tri[(l + 2) % 3]);
            let m1 = cross(lv, a, &mut pos, &mut col, &mut d);
            let m2 = cross(lv, b, &mut pos, &mut col, &mut d);
            push(lv, m1, m2, s[l]);
            push(m1, a, b, s[(l + 1) % 3]);
            push(m1, b, m2, s[(l + 1) % 3]);
        }
    }
    let on = d.iter().map(|&x| x == 0.0).collect();
    Raw { pos, col, idx, grp, side, on }
}

/// Closed border loops made of edges whose ends both lie on the cut, each in the direction
/// its triangle runs it, with that triangle's side.
fn cut_loops(idx: &[u32], side: &[i8], on: &[bool]) -> Vec<(Vec<u32>, i8)> {
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in idx.chunks(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut next: HashMap<u32, Vec<(u32, i8)>> = HashMap::new();
    for (ti, t) in idx.chunks(3).enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if count[&(a.min(b), a.max(b))] == 1 && on[a as usize] && on[b as usize] {
                next.entry(a).or_default().push((b, side[ti]));
            }
        }
    }
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut loops = Vec::new();
    for start in starts {
        while let Some((first, sd)) = next.get_mut(&start).and_then(|v| v.pop()) {
            let mut lp = vec![start];
            let mut at = first;
            let mut closed = true;
            while at != start {
                lp.push(at);
                match next.get_mut(&at).and_then(|v| v.pop()) {
                    Some((n, _)) => at = n,
                    None => {
                        closed = false;
                        break;
                    }
                }
                if lp.len() > idx.len() {
                    closed = false;
                    break;
                }
            }
            if closed && lp.len() >= 3 {
                loops.push((lp, sd));
            }
        }
    }
    loops
}

/// Triangles covering the polygon `pts` (in order, counter-clockwise), by ear clipping.
pub fn ear_clip(pts: &[Vec2]) -> Vec<[usize; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let mut ring: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let cross = |a: Vec2, b: Vec2, c: Vec2| (b - a).perp_dot(c - a);
    let inside = |p: Vec2, a: Vec2, b: Vec2, c: Vec2| cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0;
    let mut guard = 0;
    let mut i = 0;
    while ring.len() > 3 && guard < n * n + 16 {
        guard += 1;
        let m = ring.len();
        let (ia, ib, ic) = (ring[(i + m - 1) % m], ring[i % m], ring[(i + 1) % m]);
        let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
        let convex = cross(a, b, c) > 0.0;
        let ear = convex
            && !ring.iter().any(|&j| {
                j != ia && j != ib && j != ic && {
                    let p = pts[j];
                    // Only reflex corners can sit inside an ear; testing all is still correct.
                    inside(p, a, b, c) && p != a && p != b && p != c
                }
            });
        if ear {
            out.push([ia, ib, ic]);
            ring.remove(i % m);
            i = if i == 0 { 0 } else { i - 1 };
        } else {
            i = (i + 1) % m;
            // No ear found in a whole round: the outline folds over itself; clip anyway.
            if guard % m == 0 && guard > 0 && !ring.is_empty() && i == 0 {
                let m = ring.len();
                let k = (0..m)
                    .max_by(|&x, &y| {
                        let f = |k: usize| cross(pts[ring[(k + m - 1) % m]], pts[ring[k]], pts[ring[(k + 1) % m]]);
                        f(x).total_cmp(&f(y))
                    })
                    .unwrap();
                out.push([ring[(k + m - 1) % m], ring[k], ring[(k + 1) % m]]);
                ring.remove(k);
            }
        }
    }
    if ring.len() == 3 {
        out.push([ring[0], ring[1], ring[2]]);
    }
    out
}

/// Triangles closing the loop `lp` (vertex numbers into `pos`, given in the direction its
/// border triangles run it), facing away from them.
fn cap(pos: &[Vec3], lp: &[u32]) -> Vec<[u32; 3]> {
    // The cap runs the loop the other way round.
    let rev: Vec<u32> = lp.iter().rev().copied().collect();
    let pts3: Vec<Vec3> = rev.iter().map(|&v| pos[v as usize]).collect();
    // Newell's normal: the polygon is counter-clockwise round it.
    let mut n = Vec3::ZERO;
    for k in 0..pts3.len() {
        let (a, b) = (pts3[k], pts3[(k + 1) % pts3.len()]);
        n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    let n = n.normalize_or(Vec3::Z);
    let (u, v) = crate::brush_asset::tangent(n);
    let pts: Vec<Vec2> = pts3.iter().map(|p| Vec2::new(p.dot(u), p.dot(v))).collect();
    // `tangent` gives a right-handed (u, v, n) or not; make the outline counter-clockwise.
    let area: f32 = (0..pts.len()).map(|k| pts[k].perp_dot(pts[(k + 1) % pts.len()])).sum();
    let tris = if area >= 0.0 {
        ear_clip(&pts)
    } else {
        let flipped: Vec<Vec2> = pts.iter().map(|p| Vec2::new(p.x, -p.y)).collect();
        ear_clip(&flipped)
    };
    tris.into_iter().map(|t| t.map(|k| rev[k])).collect()
}

/// Cut `mesh` where `d` (signed distance per vertex) is zero and do `what` with the side
/// where it is positive. `None` when the cut misses the visible surface.
pub fn line_cut(mesh: &SculptMesh, d: &[f32], what: LineCut) -> Option<SculptMesh> {
    if d.len() != mesh.vertex_count() || mesh.triangle_count() == 0 {
        return None;
    }
    let mut raw = split(mesh, d);
    let ahead = raw.side.iter().filter(|&&s| s > 0).count();
    if ahead == 0 || (what != LineCut::Trim && ahead == raw.side.len()) {
        return None;
    }
    let mut next_group = raw.grp.iter().copied().max().unwrap_or(0).wrapping_add(1);
    match what {
        LineCut::Slice => {
            for (g, &s) in raw.grp.iter_mut().zip(&raw.side) {
                if s > 0 {
                    *g = next_group;
                }
            }
        }
        LineCut::Trim => {
            let keep: Vec<usize> = (0..raw.side.len()).filter(|&t| raw.side[t] <= 0).collect();
            if keep.is_empty() {
                return None;
            }
            let idx: Vec<u32> = keep.iter().flat_map(|&t| raw.idx[3 * t..3 * t + 3].to_vec()).collect();
            let grp: Vec<u16> = keep.iter().map(|&t| raw.grp[t]).collect();
            let side = vec![-1i8; grp.len()];
            raw.idx = idx;
            raw.grp = grp;
            raw.side = side;
            for (lp, _) in cut_loops(&raw.idx, &raw.side, &raw.on) {
                for t in cap(&raw.pos, &lp) {
                    raw.idx.extend_from_slice(&t);
                    raw.grp.push(next_group);
                    raw.side.push(-1);
                }
            }
        }
        LineCut::Knife => {
            // The far side gets its own copies of the vertices on the cut, and a polygroup.
            let mut copy: HashMap<u32, u32> = HashMap::new();
            for t in 0..raw.side.len() {
                if raw.side[t] <= 0 {
                    continue;
                }
                raw.grp[t] = next_group;
                for k in 0..3 {
                    let v = raw.idx[3 * t + k];
                    if raw.on[v as usize] {
                        let c = *copy.entry(v).or_insert_with(|| {
                            raw.pos.push(raw.pos[v as usize]);
                            if !raw.col.is_empty() {
                                raw.col.push(raw.col[v as usize]);
                            }
                            raw.on.push(true);
                            (raw.pos.len() - 1) as u32
                        });
                        raw.idx[3 * t + k] = c;
                    }
                }
            }
            let near_cap = next_group.wrapping_add(1);
            for (lp, sd) in cut_loops(&raw.idx, &raw.side, &raw.on) {
                for t in cap(&raw.pos, &lp) {
                    raw.idx.extend_from_slice(&t);
                    raw.grp.push(if sd > 0 { next_group } else { near_cap });
                    raw.side.push(sd);
                }
            }
            next_group = next_group.wrapping_add(2);
        }
    }
    let _ = next_group;
    // Drop the vertices nothing uses any more.
    let mut map = vec![u32::MAX; raw.pos.len()];
    let (mut pos, mut col) = (Vec::new(), Vec::new());
    for v in raw.idx.iter_mut() {
        if map[*v as usize] == u32::MAX {
            map[*v as usize] = pos.len() as u32;
            pos.push(raw.pos[*v as usize]);
            if !raw.col.is_empty() {
                col.push(raw.col[*v as usize]);
            }
        }
        *v = map[*v as usize];
    }
    Some(SculptMesh::from_raw_painted(pos, raw.idx, raw.grp, col))
}

/// A closed prism through `[near, far]` along the view: the outline `outline` (screen
/// pixels, in order) placed at both depths by `at(point, depth)`, sides and both ends
/// closed, facing out.
pub fn prism(outline: &[Vec2], at: impl Fn(Vec2, f32) -> Vec3, near: f32, far: f32) -> Option<SculptMesh> {
    let mut pts: Vec<Vec2> = Vec::with_capacity(outline.len());
    for &p in outline {
        if pts.last().is_none_or(|q: &Vec2| q.distance(p) > 0.5) {
            pts.push(p);
        }
    }
    while pts.len() > 3 && pts[0].distance(*pts.last().unwrap()) <= 0.5 {
        pts.pop();
    }
    let n = pts.len();
    if n < 3 {
        return None;
    }
    // Counter-clockwise in screen coordinates (y down), for the ear clipper.
    let area: f32 = (0..n).map(|k| pts[k].perp_dot(pts[(k + 1) % n])).sum();
    if area.abs() < 4.0 {
        return None;
    }
    if area < 0.0 {
        pts.reverse();
    }
    let mut positions: Vec<Vec3> = pts.iter().map(|&p| at(p, near)).collect();
    positions.extend(pts.iter().map(|&p| at(p, far)));
    if positions.iter().any(|p| !p.is_finite()) {
        return None;
    }
    let mut indices = Vec::new();
    for k in 0..n as u32 {
        let (a, b) = (k, (k + 1) % n as u32);
        let (c, d) = (b + n as u32, a + n as u32);
        indices.extend_from_slice(&[a, b, c, a, c, d]);
    }
    for t in ear_clip(&pts) {
        let t = t.map(|k| k as u32);
        indices.extend_from_slice(&[t[0], t[2], t[1]]);
        indices.extend_from_slice(&[t[0] + n as u32, t[1] + n as u32, t[2] + n as u32]);
    }
    // Turn it inside out if it faces in (signed volume below zero).
    let volume: f32 = indices
        .chunks(3)
        .map(|t| positions[t[0] as usize].dot(positions[t[1] as usize].cross(positions[t[2] as usize])))
        .sum();
    if volume < 0.0 {
        for t in indices.chunks_mut(3) {
            t.swap(1, 2);
        }
    }
    let groups = vec![0u16; indices.len() / 3];
    Some(SculptMesh::from_raw_painted(positions, indices, groups, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(16, 1.0);
        SculptMesh::from_raw(p, i)
    }
    fn plane_d(m: &SculptMesh, x: f32) -> Vec<f32> {
        m.positions.iter().map(|p| p.x - x).collect()
    }
    /// Every edge has two triangles, each way once.
    fn closed(m: &SculptMesh) -> bool {
        let mut count: HashMap<(u32, u32), i32> = HashMap::new();
        for t in m.indices.chunks(3) {
            for k in 0..3 {
                *count.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
            }
        }
        count.iter().all(|(&(a, b), &c)| c == 1 && count.get(&(b, a)) == Some(&1))
    }
    fn volume(m: &SculptMesh) -> f32 {
        m.indices
            .chunks(3)
            .map(|t| m.positions[t[0] as usize].dot(m.positions[t[1] as usize].cross(m.positions[t[2] as usize])) / 6.0)
            .sum()
    }

    #[test]
    fn trim_cuts_exactly_on_the_plane_and_closes_the_cut() {
        let m = sphere();
        let out = line_cut(&m, &plane_d(&m, 0.3), LineCut::Trim).unwrap();
        assert!(out.positions.iter().all(|p| p.x <= 0.3 + 1e-4), "nothing left past the cut");
        assert!(out.positions.iter().filter(|p| (p.x - 0.3).abs() < 1e-4).count() > 20, "new edges on the cut");
        assert!(closed(&out), "the cap closes the opening");
        // A sphere cap of height 0.7 is cut away: volume 4/3π − π h²(3 − h)/3.
        let h = 0.7f32;
        let expect = 4.0 / 3.0 * std::f32::consts::PI - std::f32::consts::PI * h * h * (3.0 - h) / 3.0;
        assert!((volume(&out) - expect).abs() < expect * 0.03, "{} vs {expect}", volume(&out));
        // The cap is its own polygroup.
        assert!(out.group_count() >= 2);
    }

    #[test]
    fn slice_regroups_one_side_without_moving_anything_and_knife_makes_two_closed_pieces() {
        let m = sphere();
        let d = plane_d(&m, -0.2);
        let sliced = line_cut(&m, &d, LineCut::Slice).unwrap();
        assert!(closed(&sliced));
        assert!((volume(&sliced) - volume(&m)).abs() < 1e-3);
        assert_eq!(sliced.group_count(), 2);
        // Every triangle is wholly on one side, and its side decides its polygroup.
        let centre_x = |t: u32| {
            let tri = &sliced.indices[3 * t as usize..3 * t as usize + 3];
            tri.iter().map(|&v| sliced.positions[v as usize].x).sum::<f32>() / 3.0
        };
        let far = (0..sliced.triangle_count() as u32).find(|&t| centre_x(t) > 0.5).map(|t| sliced.group(t)).unwrap();
        for t in 0..sliced.triangle_count() as u32 {
            let tri = &sliced.indices[3 * t as usize..3 * t as usize + 3];
            let xs: Vec<f32> = tri.iter().map(|&v| sliced.positions[v as usize].x + 0.2).collect();
            assert!(xs.iter().all(|&x| x >= -1e-4) || xs.iter().all(|&x| x <= 1e-4), "{xs:?}");
            assert_eq!(sliced.group(t) == far, centre_x(t) > -0.2);
        }
        let knifed = line_cut(&m, &d, LineCut::Knife).unwrap();
        assert!(closed(&knifed), "both pieces are closed");
        assert!((volume(&knifed) - volume(&m)).abs() < volume(&m) * 0.02, "the pieces fill the sphere");
        assert!(knifed.group_count() >= 3);
        // The pieces share no vertex: the far side has its own copies on the cut.
        assert!(knifed.vertex_count() > sliced.vertex_count());
    }

    #[test]
    fn a_cut_that_misses_the_model_does_nothing() {
        let m = sphere();
        assert!(line_cut(&m, &plane_d(&m, 2.0), LineCut::Trim).is_none());
        assert!(line_cut(&m, &plane_d(&m, -2.0), LineCut::Slice).is_none());
    }

    #[test]
    fn ear_clipping_covers_a_concave_outline_once() {
        // An L shape.
        let pts = [Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0), Vec2::new(2.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 2.0), Vec2::new(0.0, 2.0)];
        let tris = ear_clip(&pts);
        assert_eq!(tris.len(), 4);
        let area: f32 = tris.iter().map(|t| (pts[t[1]] - pts[t[0]]).perp_dot(pts[t[2]] - pts[t[0]]) * 0.5).sum();
        assert!((area - 3.0).abs() < 1e-5);
        assert!(tris.iter().all(|t| (pts[t[1]] - pts[t[0]]).perp_dot(pts[t[2]] - pts[t[0]]) > 0.0));
    }

    #[test]
    fn prisms_are_closed_and_face_out() {
        let lasso = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(5.0, 4.0), Vec2::new(0.0, 10.0)];
        let p = prism(&lasso, |q, z| Vec3::new(q.x, q.y, z), -1.0, 1.0).unwrap();
        assert!(closed(&p));
        assert!(volume(&p) > 0.0);
    }
}
