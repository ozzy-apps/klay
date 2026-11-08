//! The rectangle inset: the one move a box modeller makes for every round feature (a breast,
//! an eye socket, a mouth, a hole). A rectangle of quads is picked out of the surrounding grid
//! by walking quad strips, cut out, and filled with rings in from its border and a small grid
//! at its middle. The grid outside is untouched; the only poles are four 5-poles at the
//! rectangle's corners and four 3-poles at the corners of the centre grid.

use super::body::disc_border;
use super::*;

/// Quad adjacency: per vertex its quads and its count of neighbours, per undirected edge the
/// quads on it.
pub(super) struct Adj {
    vq: Vec<Vec<u32>>,
    eq: HashMap<(u32, u32), Vec<u32>>,
    val: Vec<u32>,
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn degenerate(q: &[u32; 4]) -> bool {
    (0..4).any(|k| (k + 1..4).any(|l| q[k] == q[l]))
}

impl Adj {
    pub(super) fn new(quads: &[[u32; 4]], nv: usize) -> Self {
        let mut vq: Vec<Vec<u32>> = vec![Vec::new(); nv];
        let mut eq: HashMap<(u32, u32), Vec<u32>> = HashMap::default();
        for (f, q) in quads.iter().enumerate() {
            for k in 0..4 {
                let (a, b) = (q[k], q[(k + 1) % 4]);
                if (a as usize) < nv && !vq[a as usize].contains(&(f as u32)) {
                    vq[a as usize].push(f as u32);
                }
                if a != b {
                    let e = eq.entry(edge_key(a, b)).or_default();
                    if !e.contains(&(f as u32)) {
                        e.push(f as u32);
                    }
                }
            }
        }
        let mut val = vec![0u32; nv];
        for &(a, b) in eq.keys() {
            if (a as usize) < nv {
                val[a as usize] += 1;
            }
            if (b as usize) < nv {
                val[b as usize] += 1;
            }
        }
        Adj { vq, eq, val }
    }

    /// An interior vertex of a regular grid: four quads, four neighbours.
    pub(super) fn regular(&self, v: u32) -> bool {
        (v as usize) < self.vq.len() && self.vq[v as usize].len() == 4 && self.val[v as usize] == 4
    }

    /// The quads at vertex `v`.
    pub(super) fn quads_at(&self, v: u32) -> &[u32] {
        self.vq.get(v as usize).map_or(&[], |q| q.as_slice())
    }

    /// The quads on edge (`a`, `b`).
    pub(super) fn quads_on(&self, a: u32, b: u32) -> &[u32] {
        self.eq.get(&edge_key(a, b)).map_or(&[], |q| q.as_slice())
    }
}

/// The edge of quad `f` opposite `e` = (p, q), as (p's partner, q's partner): the partner of a
/// vertex is the one across the quad's side from it. None if `e` is not an edge of `f` or `f`
/// is degenerate.
pub(super) fn opposite(quads: &[[u32; 4]], f: usize, e: (u32, u32)) -> Option<(u32, u32)> {
    let q = quads[f];
    if degenerate(&q) {
        return None;
    }
    let k = (0..4).find(|&k| edge_key(q[k], q[(k + 1) % 4]) == edge_key(e.0, e.1))?;
    Some(if q[k] == e.0 { (q[(k + 3) % 4], q[(k + 2) % 4]) } else { (q[(k + 2) % 4], q[(k + 3) % 4]) })
}

/// From quad `f` entered over edge `e_in`, the quad past its opposite edge and that edge
/// (labelled like `e_in`: each vertex the partner of the one it stands in for). None at a
/// border or a degenerate quad.
pub(super) fn across(adj: &Adj, quads: &[[u32; 4]], f: usize, e_in: (u32, u32)) -> Option<(usize, (u32, u32))> {
    let e_out = opposite(quads, f, e_in)?;
    let on = adj.eq.get(&edge_key(e_out.0, e_out.1))?;
    if on.len() != 2 {
        return None;
    }
    let g = if on[0] as usize == f { on[1] } else if on[1] as usize == f { on[0] } else { return None } as usize;
    if degenerate(&quads[g]) {
        return None;
    }
    Some((g, e_out))
}

/// An `a` × `b` block of quads of a regular grid round `seed`: `u_edge` is an edge of `seed`;
/// the strip through it runs `ia` quads past that edge (−u) and `a` − 1 − `ia` past the
/// opposite one (+u); from each strip quad the cross strip runs `ib` quads on the side of
/// `u_edge`'s first vertex (−v) and `b` − 1 − `ib` on the other (+v). None if a step leaves
/// the grid or meets a quad twice. The quads in (row, column) order, rows along v, columns
/// along u. Poles inside the block do not stop it (they are cut away with it); whether its
/// border is a rectangle is for `rect_border` to say.
#[allow(clippy::too_many_arguments)]
pub(super) fn grid_rect(adj: &Adj, quads: &[[u32; 4]], seed: usize, u_edge: (u32, u32), a: usize, b: usize, ia: usize, ib: usize) -> Option<Vec<usize>> {
    if a == 0 || b == 0 || ia >= a || ib >= b {
        return None;
    }
    // The strip along u: each quad with its −u edge (−v vertex, +v vertex).
    let mut strip: Vec<(usize, (u32, u32))> = Vec::with_capacity(a);
    let (mut f, mut e) = (seed, opposite(quads, seed, u_edge)?);
    for _ in 0..ia {
        let (g, e2) = across(adj, quads, f, e)?;
        strip.push((g, opposite(quads, g, e2)?));
        f = g;
        e = e2;
    }
    strip.reverse();
    strip.push((seed, u_edge));
    let (mut f, mut e) = (seed, u_edge);
    for _ in 0..a - 1 - ia {
        let (g, e2) = across(adj, quads, f, e)?;
        strip.push((g, e2));
        f = g;
        e = e2;
    }
    // Across it: from each strip quad, ib quads toward −v and the rest toward +v.
    let mut rows = vec![vec![usize::MAX; a]; b];
    for (i, &(f, (p, q))) in strip.iter().enumerate() {
        rows[ib][i] = f;
        let (p2, q2) = opposite(quads, f, (p, q))?;
        let (mut g, mut e) = (f, (q, q2));
        for j in 1..=ib {
            let (h, e2) = across(adj, quads, g, e)?;
            rows[ib - j][i] = h;
            g = h;
            e = e2;
        }
        let (mut g, mut e) = (f, (p, p2));
        for j in 1..b - ib {
            let (h, e2) = across(adj, quads, g, e)?;
            rows[ib + j][i] = h;
            g = h;
            e = e2;
        }
    }
    let out: Vec<usize> = rows.into_iter().flatten().collect();
    let mut seen: HashSet<usize> = HashSet::default();
    for &f in &out {
        if !seen.insert(f) {
            return None;
        }
    }
    Some(out)
}

/// The size of the cells at `seed`: the length of its edge across `u_edge` at `u_edge`'s
/// first vertex (along u), and the length of `u_edge` itself (along v).
pub(super) fn cell_sizes(quads: &[[u32; 4]], p: &[Vec3], seed: usize, u_edge: (u32, u32)) -> (f32, f32) {
    let q = quads[seed];
    let cv = p[u_edge.1 as usize].distance(p[u_edge.0 as usize]);
    let cu = (0..4).find(|&k| q[k] == u_edge.0).map_or(cv, |k| {
        let other = if q[(k + 1) % 4] == u_edge.1 { q[(k + 3) % 4] } else { q[(k + 1) % 4] };
        p[other as usize].distance(p[u_edge.0 as usize])
    });
    (cu, cv)
}

/// The places of the seed in an `a` × `b` block, the centre first, then farther and farther
/// off it (the block slides to keep clear of poles): (ia, ib) pairs. With `fix_a` the seed
/// stays at the middle column.
pub(super) fn shifts(a: usize, b: usize, fix_a: bool) -> Vec<(usize, usize)> {
    let (ca, cb) = (a / 2, b / 2);
    let mut out: Vec<(usize, usize)> = (0..a).flat_map(|ia| (0..b).map(move |ib| (ia, ib))).filter(|&(ia, _)| !fix_a || ia == ca).collect();
    out.sort_by_key(|&(ia, ib)| (ia.abs_diff(ca) + ib.abs_diff(cb), ib.abs_diff(cb), ia, ib));
    out
}

/// The border of the rectangle of quads `qs` as a loop starting at a corner (a border vertex
/// in exactly one of the quads; side vertices lie in two), with the indices of its four
/// corners. None unless the quads are a disc with exactly four corners and opposite sides of
/// equal length.
pub(super) fn rect_border(quads: &[[u32; 4]], qs: &[usize]) -> Option<(Vec<u32>, [usize; 4])> {
    let lp = disc_border(quads, qs)?;
    let mut count: HashMap<u32, u32> = HashMap::default();
    for &f in qs {
        let mut q = quads[f].to_vec();
        q.sort_unstable();
        q.dedup();
        for v in q {
            *count.entry(v).or_default() += 1;
        }
    }
    let n = lp.len();
    let corners: Vec<usize> = (0..n).filter(|&m| count.get(&lp[m]) == Some(&1)).collect();
    if corners.len() != 4 || (0..n).any(|m| !corners.contains(&m) && count.get(&lp[m]) != Some(&2)) {
        return None;
    }
    let c0 = corners[0];
    let border: Vec<u32> = (0..n).map(|m| lp[(c0 + m) % n]).collect();
    let c: [usize; 4] = [0, corners[1] - c0, corners[2] - c0, corners[3] - c0];
    let sides = [c[1], c[2] - c[1], c[3] - c[2], n - c[3]];
    if sides[0] != sides[2] || sides[1] != sides[3] || sides[0] == 0 || sides[1] == 0 {
        return None;
    }
    Some((border, c))
}

/// Rings in from `border` (a rectangle's border loop, `corners` its corner indices) toward
/// its centroid, `k` of them, each with the border's count, joined to the one before with a
/// band of quads (no merges, no poles). Ring j (t = j / (k + 1)) is the border rounded toward
/// a circle by `round` · j / k, shrunk toward the centroid by 0.8 t, lifted toward `tip` by
/// 1 − (1 − t)² of the way when there is one (so it climbs a breast or sinks into a socket),
/// and put on the surface by `surface`. The rings' points, the last one innermost.
#[allow(clippy::too_many_arguments)]
pub(super) fn inset_fill(border: &[u32], _corners: [usize; 4], k: usize, round: f32, tip: Option<Vec3>, surface: &dyn Fn(Vec3) -> Vec3, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Vec<Vec<u32>> {
    let n = border.len();
    let pts: Vec<Vec3> = border.iter().map(|&v| out_p[v as usize]).collect();
    let c = pts.iter().copied().sum::<Vec3>() / n.max(1) as f32;
    let rr = pts.iter().map(|p| p.distance(c)).sum::<f32>() / n.max(1) as f32;
    let mut rings: Vec<Vec<u32>> = Vec::with_capacity(k);
    let mut prev: Vec<u32> = border.to_vec();
    for j in 1..=k {
        let t = j as f32 / (k + 1) as f32;
        let rounding = round * j as f32 / k as f32;
        let lift = tip.map(|tp| (tp - c) * (1.0 - (1.0 - t) * (1.0 - t)));
        let ring: Vec<u32> = (0..n).map(|m| {
            let p = pts[m];
            let circle = c + (p - c).normalize_or_zero() * rr;
            let mut q = p.lerp(circle, rounding).lerp(c, 0.8 * t);
            if let Some(l) = lift {
                q += l;
            }
            out_p.push(surface(q));
            (out_p.len() - 1) as u32
        }).collect();
        for m in 0..n {
            quads.push([prev[m], prev[(m + 1) % n], ring[(m + 1) % n], ring[m]]);
        }
        rings.push(ring.clone());
        prev = ring;
    }
    rings
}

/// A grid over a row (an even count): `a` × b quads, b = n / 2 − `a`, its border the row,
/// its inner points a Coons patch between the sides. On a pinned row (`sym`) with `a` even the
/// row's point 0 is the middle of a side, so a grid line runs along the plane. The grid of
/// point ids, rows along b.
pub(super) fn cap_dims(row: &[u32], a: usize, sym: bool, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Vec<Vec<u32>> {
    let n = row.len();
    let b = n / 2 - a;
    let shift = if sym && a % 2 == 0 { n - a / 2 } else { 0 };
    let at = |k: usize| row[(k + shift) % n];
    let mut ids = vec![vec![0u32; a + 1]; b + 1];
    for i in 0..=a {
        ids[0][i] = at(i);
        ids[b][i] = at(2 * a + b - i);
    }
    for j in 0..=b {
        ids[j][a] = at(a + j);
        ids[j][0] = at((2 * a + 2 * b - j) % n);
    }
    let (c0, c1, c2, c3) = (out_p[ids[0][0] as usize], out_p[ids[0][a] as usize], out_p[ids[b][a] as usize], out_p[ids[b][0] as usize]);
    for j in 1..b {
        for i in 1..a {
            let (u, v) = (i as f32 / a as f32, j as f32 / b as f32);
            let (bot, top) = (out_p[ids[0][i] as usize], out_p[ids[b][i] as usize]);
            let (lft, rgt) = (out_p[ids[j][0] as usize], out_p[ids[j][a] as usize]);
            let q = bot * (1.0 - v) + top * v + lft * (1.0 - u) + rgt * u - (c0 * (1.0 - u) * (1.0 - v) + c1 * u * (1.0 - v) + c2 * u * v + c3 * (1.0 - u) * v);
            out_p.push(q);
            ids[j][i] = (out_p.len() - 1) as u32;
        }
    }
    for j in 0..b {
        for i in 0..a {
            quads.push([ids[j][i], ids[j][i + 1], ids[j + 1][i + 1], ids[j + 1][i]]);
        }
    }
    ids
}

/// The inner points of a cap (all points from `from` on) lifted toward `tip` like a dome over
/// `row`: all the way at the row's middle, none at the row; then put on the surface.
pub(super) fn dome_cap(row: &[u32], from: usize, tip: Option<Vec3>, surface: &dyn Fn(Vec3) -> Vec3, out_p: &mut [Vec3]) {
    let pts: Vec<Vec3> = row.iter().map(|&v| out_p[v as usize]).collect();
    let c = pts.iter().copied().sum::<Vec3>() / pts.len().max(1) as f32;
    let rr = pts.iter().map(|q| q.distance(c)).sum::<f32>() / pts.len().max(1) as f32;
    for v in from..out_p.len() {
        if let Some(tp) = tip {
            let d = out_p[v].distance(c) / rr.max(1e-9);
            out_p[v] += (tp - c) * (1.0 - d * d).max(0.0);
        }
        out_p[v] = surface(out_p[v]);
    }
}

/// The quad with the tip `v` as a corner and the edge from `v` to walk as `u_edge`: the edge
/// at `v` least along `along` (so the strip through it runs along `along`), and of the two
/// quads on it the one whose centre has the greater `side` (the + side of a plane). None
/// unless `v` is a regular grid vertex.
pub(super) fn seed_at(adj: &Adj, quads: &[[u32; 4]], p: &[Vec3], v: u32, along: Vec3, side: &dyn Fn(Vec3) -> f32) -> Option<(usize, (u32, u32))> {
    if !adj.regular(v) {
        return None;
    }
    let mut nbs: Vec<u32> = Vec::new();
    for &f in adj.quads_at(v) {
        let q = quads[f as usize];
        for k in 0..4 {
            if q[k] == v {
                for u in [q[(k + 1) % 4], q[(k + 3) % 4]] {
                    if !nbs.contains(&u) {
                        nbs.push(u);
                    }
                }
            }
        }
    }
    let u = nbs.into_iter().min_by(|&a, &b| {
        let da = (p[a as usize] - p[v as usize]).normalize_or_zero().dot(along).abs();
        let db = (p[b as usize] - p[v as usize]).normalize_or_zero().dot(along).abs();
        da.total_cmp(&db).then(a.cmp(&b))
    })?;
    let on = adj.eq.get(&edge_key(v, u))?;
    let centre = |f: u32| quads[f as usize].iter().map(|&w| p[w as usize]).sum::<Vec3>() * 0.25;
    let f = on.iter().copied().max_by(|&a, &b| side(centre(a)).total_cmp(&side(centre(b))).then(b.cmp(&a)))?;
    Some((f as usize, (v, u)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_grid(n: usize) -> (Vec<Vec3>, Vec<[u32; 4]>) {
        let mut p = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                p.push(Vec3::new(i as f32, j as f32, 0.0));
            }
        }
        let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let mut q = Vec::new();
        for j in 0..n {
            for i in 0..n {
                q.push([at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        (p, q)
    }

    #[test]
    fn rect_inset_on_a_flat_grid() {
        let (mut p, mut quads) = flat_grid(10);
        let adj = Adj::new(&quads, p.len());
        // The quad right above and right of the centre vertex, walking out through its left
        // edge (bottom-left vertex first: −v is down): a 4 × 4 block round the centre.
        let seed = 5 * 10 + 5;
        let u_edge = (quads[seed][0], quads[seed][3]);
        let block = grid_rect(&adj, &quads, seed, u_edge, 4, 4, 2, 2).expect("a 4x4 block");
        assert_eq!(block.len(), 16);
        let mut rows: Vec<usize> = block.iter().map(|&f| f / 10).collect();
        let mut cols: Vec<usize> = block.iter().map(|&f| f % 10).collect();
        rows.sort_unstable();
        rows.dedup();
        cols.sort_unstable();
        cols.dedup();
        assert_eq!(rows, vec![3, 4, 5, 6]);
        assert_eq!(cols, vec![3, 4, 5, 6]);
        assert_eq!(block[0], 3 * 10 + 3);
        assert_eq!(block[15], 6 * 10 + 6);
        // Too wide for the grid.
        assert!(grid_rect(&adj, &quads, seed, u_edge, 12, 4, 6, 2).is_none());
        let (border, corners) = rect_border(&quads, &block).expect("a rectangle");
        assert_eq!(border.len(), 16);
        assert_eq!(corners, [0, 4, 8, 12]);
        let mut gone = block.clone();
        gone.sort_unstable();
        for &f in gone.iter().rev() {
            quads.swap_remove(f);
        }
        let rings = inset_fill(&border, corners, 2, 1.0, None, &|q| q, &mut p, &mut quads);
        assert_eq!(rings.len(), 2);
        let from = p.len();
        cap_dims(&rings[1], 4, false, &mut p, &mut quads);
        dome_cap(&rings[1], from, Some(Vec3::new(5.0, 5.0, 1.0)), &|q| q, &mut p);
        assert_eq!(quads.len(), 100 - 16 + 32 + 16);
        // A closed disc: every edge on one quad (the outer border) or two, V − E + F = 1.
        let mut on: HashMap<(u32, u32), u32> = HashMap::default();
        for q in &quads {
            assert!(!degenerate(q));
            for k in 0..4 {
                *on.entry(edge_key(q[k], q[(k + 1) % 4])).or_default() += 1;
            }
        }
        assert!(on.values().all(|&c| c == 1 || c == 2));
        assert_eq!(on.values().filter(|&&c| c == 1).count(), 40);
        // (The block's nine inner points are left behind in `p`, in no quad.)
        let used: HashSet<u32> = on.keys().flat_map(|&(a, b)| [a, b]).collect();
        assert_eq!(used.len(), p.len() - 9);
        assert_eq!(used.len() as i64 - on.len() as i64 + quads.len() as i64, 1);
        // Exactly four 5-poles and four 3-poles among the interior vertices.
        let mut val = vec![0u32; p.len()];
        let mut outer = vec![false; p.len()];
        for (&(a, b), &c) in &on {
            val[a as usize] += 1;
            val[b as usize] += 1;
            if c == 1 {
                outer[a as usize] = true;
                outer[b as usize] = true;
            }
        }
        let poles: Vec<u32> = (0..p.len()).filter(|&v| !outer[v] && val[v] != 0 && val[v] != 4).map(|v| val[v]).collect();
        assert_eq!(poles.iter().filter(|&&v| v == 5).count(), 4);
        assert_eq!(poles.iter().filter(|&&v| v == 3).count(), 4);
        assert_eq!(poles.len(), 8);
        // The 5-poles are the block's corners, the 3-poles the centre grid's.
        for &c in &corners {
            assert_eq!(val[border[c] as usize], 5);
            assert_eq!(val[rings[1][c] as usize], 3);
        }
    }
}
