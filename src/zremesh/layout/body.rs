//! The body built the way a box modeller blocks it out. The part function (distance from the
//! top of the head) is cut into bands a quad apart; each band's connected pieces are linked to
//! the pieces next to them by the level ring between. Down from the head, ring follows ring:
//! each is a row of points and each band between two rows a strip of quads. Where a ring
//! splits (armpits, crotch), short strips across the split join its branches, and the ring
//! above is bridged into all of them as one loop. A grid caps the top of the head and the end
//! of each limb.

use super::*;
use crate::symmetry::Axis;

/// A level ring between a piece and one of the pieces past it.
struct Ring {
    /// Points round it, larger values of the part function on the left (seen from outside).
    pts: Vec<Vec3>,
    /// For each point, the vertex on its edge in the nearer piece (smaller values).
    near: Vec<u32>,
    /// And the vertex on its edge in the farther piece.
    far: Vec<u32>,
    /// The nearer and the farther piece.
    from: u32,
    to: u32,
    /// Length in quads.
    girth: f32,
}

/// How a piece where the ring splits is bridged: the rings past it it keeps, the strips
/// between them (ring, crossing index, ring, crossing index, points along, edges), and the
/// count of the loop the ring above is joined to.
struct Split {
    rings: Vec<usize>,
    counts: Vec<usize>,
    strips: Vec<(usize, usize, usize, usize, Vec<Vec3>, usize)>,
    total: usize,
}

fn even(g: f32) -> usize {
    ((g * 0.5).round() as usize * 2).max(4)
}

/// A count in fours: a ring the mirror plane cuts in two then has an even count each side.
fn even4(g: f32) -> usize {
    ((g * 0.25).round() as usize * 4).max(4)
}

/// Two rows joined as an open strip (not round): one quad per step where the counts match,
/// three-to-one merges spread along it (away from its ends) where they differ.
fn join_open(b: &[u32], t: &[u32], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) {
    let (b, t) = if t.len() > b.len() { (t, b) } else { (b, t) };
    let (nb, nt) = (b.len() - 1, t.len() - 1);
    let diff = nb - nt;
    let k = (diff / 2).min(nt);
    let tris = diff - 2 * k;
    let units = nt - k + k + tris;
    let specials = k + tris;
    let mut kinds: Vec<u8> = vec![1; units];
    for j in 0..specials {
        let at = ((j as f32 + 0.5) * units as f32 / specials as f32) as usize;
        kinds[at.min(units - 1)] = if j < k { 3 } else { 0 };
    }
    let (mut i, mut o) = (0usize, 0usize);
    for &st in &kinds {
        match st {
            1 => {
                quads.push([b[i + 1], b[i], t[o], t[o + 1]]);
                i += 1;
                o += 1;
            }
            3 => {
                let mid = (out_p[t[o] as usize] + out_p[t[o + 1] as usize]) * 0.5;
                out_p.push(out_p[b[i + 1] as usize].lerp(mid, 0.4));
                let m1 = (out_p.len() - 1) as u32;
                out_p.push(out_p[b[i + 2] as usize].lerp(mid, 0.4));
                let m2 = (out_p.len() - 1) as u32;
                quads.push([b[i + 1], b[i], t[o], m1]);
                quads.push([b[i + 2], b[i + 1], m1, m2]);
                quads.push([b[i + 3], b[i + 2], m2, t[o + 1]]);
                quads.push([m2, m1, t[o], t[o + 1]]);
                i += 3;
                o += 1;
            }
            _ => {
                quads.push([b[i + 1], b[i], t[o], t[o]]);
                i += 1;
            }
        }
    }
}

/// A band between two rows that are their own mirror images (points 0 and n/2 on the plane,
/// i and n − i mirrored): one half joined as an open strip, the other half its mirror image.
/// None if the halves cannot be joined without a triangle.
fn join_sym(lo: &[u32], hi: &[u32], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, plane: MirrorPlane) -> bool {
    let (n0, n1) = (lo.len(), hi.len());
    if n0 % 2 != 0 || n1 % 2 != 0 || (n0 / 2).abs_diff(n1 / 2) % 2 != 0 {
        return false;
    }
    let mut mir: HashMap<u32, u32> = HashMap::default();
    for i in 0..n0 {
        mir.insert(lo[i], lo[(n0 - i) % n0]);
    }
    for j in 0..n1 {
        mir.insert(hi[j], hi[(n1 - j) % n1]);
    }
    let first_new = out_p.len();
    let mut half: Vec<[u32; 4]> = Vec::new();
    join_open(&lo[..=n0 / 2], &hi[..=n1 / 2], out_p, &mut half);
    for v in first_new..out_p.len() {
        let q = plane.reflect(out_p[v]);
        out_p.push(q);
        mir.insert(v as u32, (out_p.len() - 1) as u32);
    }
    for q in half {
        quads.push(q);
        let m = q.map(|v| mir[&v]);
        quads.push([m[1], m[0], m[3], m[2]]);
    }
    true
}

/// Where a band's reductions are gathered, as a box modeller hides them: on the back, on the
/// inside of a limb, or on the sole.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pref {
    Back,
    Inside,
    Sole,
}

/// A strip of quads between `b` (the longer row) and `t` with a diamond at each index in
/// `kites`: the kite quad [b(i+1), b(i), b(i-1), t(j)] takes two edges of `b` into none of
/// `t`, so b(i) is left with valence 3 and t(j) with valence 5, the way a box modeller drops
/// two edge loops at once. Every other step is a plain quad. Closed rows are joined round,
/// `t` turned so its points sit nearest the matching points of `b`; open rows are joined end
/// to end. b(i) is moved away from t(j) by `ZR_KITELIFT` (0.4) of the band's height so the
/// kite has a convex corner there (toward it, the kite folds in; the rows' relax evens the
/// band below out). Returns the kite points moved.
fn kite_strip(b: &[u32], t: &[u32], kites: &[usize], closed: bool, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Vec<u32> {
    let (n0, n1) = (b.len(), t.len());
    let lift: f32 = std::env::var("ZR_KITELIFT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.4);
    let (start, steps) = kite_steps(n0, kites, closed);
    let bb = |i: usize| b[i % n0];
    let sh = if closed {
        let b_pos: Vec<Vec3> = b.iter().map(|&v| out_p[v as usize]).collect();
        let t_pos: Vec<Vec3> = t.iter().map(|&v| out_p[v as usize]).collect();
        (0..n1).min_by(|&x, &y| band_cost(&b_pos, &t_pos, start, &steps, x).total_cmp(&band_cost(&b_pos, &t_pos, start, &steps, y))).unwrap_or(0)
    } else {
        0
    };
    let tt = |o: usize| t[(o + sh) % n1];
    let (mut i, mut o) = (start, 0usize);
    let mut moved = Vec::new();
    for &st in &steps {
        if st == 1 {
            quads.push([bb(i + 1), bb(i), tt(o), tt(o + 1)]);
            i += 1;
            o += 1;
        } else {
            quads.push([bb(i + 2), bb(i + 1), bb(i), tt(o)]);
            let (v, u) = (bb(i + 1), tt(o));
            out_p[v as usize] = out_p[v as usize].lerp(out_p[u as usize], -lift);
            moved.push(v);
            i += 2;
        }
    }
    moved
}

/// The steps along a row of `n0` points with kites at `kites`: where to start (just before
/// the first kite round a closed row, the end of an open one) and each step's edges, 2 at a
/// kite (two edges of this row, none of the other), else 1.
fn kite_steps(n0: usize, kites: &[usize], closed: bool) -> (usize, Vec<u8>) {
    let is_kite = |i: usize| kites.contains(&(i % n0));
    let (edges, start) = if closed { (n0, kites.first().map_or(0, |&k| (k + n0 - 1) % n0)) } else { (n0 - 1, 0) };
    let mut steps: Vec<u8> = Vec::new();
    let mut i = start;
    while i < start + edges {
        if is_kite(i + 1) && i + 2 <= start + edges {
            steps.push(2);
            i += 2;
        } else {
            steps.push(1);
            i += 1;
        }
    }
    (start, steps)
}

/// How far a band's steps reach across: the squared distances from each step's point on the
/// longer row to its point on the shorter, the shorter turned by `sh`.
fn band_cost(b_pos: &[Vec3], t_pos: &[Vec3], start: usize, steps: &[u8], sh: usize) -> f32 {
    let (n0, n1) = (b_pos.len(), t_pos.len());
    let (mut i, mut o, mut cost) = (start, 0usize, 0.0f32);
    for &st in steps {
        cost += b_pos[i % n0].distance_squared(t_pos[(o + sh) % n1]);
        i += st as usize;
        if st == 1 {
            o += 1;
        }
    }
    cost
}

/// The least cost of a band with kites at `cols` of the longer row: over every turn of the
/// shorter row when closed; the first halves end to end when pinned.
fn kite_cost(long_pos: &[Vec3], short_pos: &[Vec3], cols: &[usize], pinned: bool) -> f32 {
    if pinned {
        let (b, t) = (&long_pos[..=long_pos.len() / 2], &short_pos[..=short_pos.len() / 2]);
        let (start, steps) = kite_steps(b.len(), cols, false);
        band_cost(b, t, start, &steps, 0)
    } else {
        let (start, steps) = kite_steps(long_pos.len(), cols, true);
        (0..short_pos.len()).map(|sh| band_cost(long_pos, short_pos, start, &steps, sh)).fold(f32::INFINITY, f32::min)
    }
}

/// A band between two closed rows of different counts joined with diamonds instead of
/// merges: `cols` are the indices on the longer row of the kites' valence-3 points, each at
/// least 3 apart and as many as half the difference. False (nothing built) if they do not fit.
fn reduce_band(lo: &[u32], hi: &[u32], cols: &[usize], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> bool {
    let flip = hi.len() > lo.len();
    let (b, t): (Vec<u32>, Vec<u32>) = if flip { (hi.iter().rev().copied().collect(), lo.iter().rev().copied().collect()) } else { (lo.to_vec(), hi.to_vec()) };
    let (n0, n1) = (b.len(), t.len());
    if n0 < n1 + 2 || n0 - n1 != 2 * cols.len() || n1 < 3 {
        return false;
    }
    let mut kites: Vec<usize> = cols.iter().map(|&c| if flip { n0 - 1 - c } else { c }).collect();
    kites.sort_unstable();
    for k in 0..kites.len() {
        let gap = (kites[(k + 1) % kites.len()] + n0 - kites[k]) % n0;
        if kites.len() > 1 && gap < 3 {
            return false;
        }
    }
    kite_strip(&b, &t, &kites, true, out_p, quads);
    true
}

/// A band between two pinned rows (points 0 and n/2 on the plane, i and n − i mirrored) of
/// different counts: the half on one side joined with diamonds at `cols_plus` (indices on
/// the longer row's half, 2 from its ends and 3 apart), the other half its mirror image.
/// False if the counts do not allow it or the kites do not fit.
fn reduce_sym(lo: &[u32], hi: &[u32], cols_plus: &[usize], plane: MirrorPlane, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> bool {
    let (n0, n1) = (lo.len(), hi.len());
    if n0 % 2 != 0 || n1 % 2 != 0 || n0 == n1 || (n0 / 2).abs_diff(n1 / 2) % 2 != 0 {
        return false;
    }
    let flip = n1 > n0;
    let (long, short) = if flip { (hi, lo) } else { (lo, hi) };
    let (nb, nt) = (long.len() / 2, short.len() / 2);
    if (nb - nt) / 2 != cols_plus.len() {
        return false;
    }
    let mut kites: Vec<usize> = cols_plus.iter().map(|&c| if flip { nb - c } else { c }).collect();
    kites.sort_unstable();
    if kites.iter().any(|&k| k < 2 || k + 2 > nb) || kites.windows(2).any(|w| w[1] - w[0] < 3) {
        return false;
    }
    let (b, t): (Vec<u32>, Vec<u32>) = if flip {
        (long[..=nb].iter().rev().copied().collect(), short[..=nt].iter().rev().copied().collect())
    } else {
        (long[..=nb].to_vec(), short[..=nt].to_vec())
    };
    let mut mir: HashMap<u32, u32> = HashMap::default();
    for i in 0..n0 {
        mir.insert(lo[i], lo[(n0 - i) % n0]);
    }
    for j in 0..n1 {
        mir.insert(hi[j], hi[(n1 - j) % n1]);
    }
    let mut half: Vec<[u32; 4]> = Vec::new();
    for v in kite_strip(&b, &t, &kites, false, out_p, &mut half) {
        let m = mir[&v];
        out_p[m as usize] = plane.reflect(out_p[v as usize]);
    }
    for q in half {
        quads.push(q);
        let m = q.map(|v| mir[&v]);
        quads.push([m[1], m[0], m[3], m[2]]);
    }
    true
}

/// The columns of `d` diamonds on a row of points `row_pos`, gathered 3 apart round the
/// preferred point: the one furthest back (smallest component `fwd`), nearest the plane, or
/// lowest. On a pinned row the indices are on its first half (point 0 in front, n/2 at the
/// back), 2 from its ends, and their mirror images make the pairs. None if they do not fit.
fn reduce_cols(row_pos: &[Vec3], d: usize, prefer: Pref, plane: Option<MirrorPlane>, fwd: usize, pinned: bool) -> Option<Vec<usize>> {
    let n = row_pos.len();
    let score = |i: usize| -> f32 {
        match (prefer, plane) {
            (Pref::Back, _) => row_pos[i][fwd],
            (Pref::Inside, Some(pl)) => pl.distance(row_pos[i]).abs(),
            (Pref::Inside, None) => row_pos[i][fwd],
            (Pref::Sole, _) => row_pos[i].y,
        }
    };
    if d == 0 {
        return Some(Vec::new());
    }
    if pinned {
        let nb = n / 2;
        if nb < 4 || 3 * (d - 1) + 4 > nb {
            return None;
        }
        let c = match prefer {
            Pref::Back => nb,
            _ => (2..=nb - 2).min_by(|&a, &b| score(a).total_cmp(&score(b))).unwrap_or(nb),
        }.clamp(2, nb - 2);
        // Out from the preferred point, alternating sides, as far as the half allows.
        let mut cols = Vec::new();
        let mut k = 0usize;
        while cols.len() < d && k <= nb {
            for s in [c as isize - 3 * k as isize, c as isize + 3 * k as isize] {
                if cols.len() < d && s >= 2 && s as usize + 2 <= nb && !cols.contains(&(s as usize)) {
                    cols.push(s as usize);
                }
            }
            k += 1;
        }
        if cols.len() < d {
            return None;
        }
        cols.sort_unstable();
        Some(cols)
    } else {
        if 3 * d + (d % 2 == 0) as usize > n {
            return None;
        }
        let c = (0..n).min_by(|&a, &b| score(a).total_cmp(&score(b))).unwrap_or(0) as isize;
        // Spread symmetric about the preferred point, so a limb and its mirror image agree:
        // c, c ± 3, c ± 6 … for an odd count; c ± 2, c ± 5 … for an even one.
        let mut cols: Vec<usize> = (0..d).map(|k| {
            let off = if d % 2 == 1 {
                let m = (k as isize + 1) / 2;
                if k % 2 == 1 { -3 * m } else { 3 * m }
            } else {
                let m = k as isize / 2;
                if k % 2 == 0 { -(2 + 3 * m) } else { 2 + 3 * m }
            };
            (c + off).rem_euclid(n as isize) as usize
        }).collect();
        cols.sort_unstable();
        Some(cols)
    }
}

/// The columns of `d` diamonds on the longer row spread where the two rows diverge, for a
/// step too big to gather in one place (the neck into the shoulders: gathered, the quads
/// between would shear). Each point of the longer row goes to its nearest point of the
/// shorter; where c of them share one, (c − 1) / 2 of their edges want a kite. The kites are
/// dealt along the row by that weight, 3 apart, the rest at the best places left. On a
/// pinned row: on its first half, 2 from its ends. None if they do not fit.
fn fan_cols(long_pos: &[Vec3], short_pos: &[Vec3], d: usize, pinned: bool) -> Option<Vec<usize>> {
    let (n, m) = (long_pos.len(), short_pos.len());
    if d == 0 {
        return Some(Vec::new());
    }
    if n < 4 || m == 0 {
        return None;
    }
    let nearest: Vec<usize> = long_pos.iter().map(|&q| (0..m).min_by(|&a, &b| short_pos[a].distance_squared(q).total_cmp(&short_pos[b].distance_squared(q))).unwrap_or(0)).collect();
    let mut share = vec![0usize; m];
    for &j in &nearest {
        share[j] += 1;
    }
    let (lo, hi) = if pinned { (2usize, n / 2 - 2) } else { (0, n - 1) };
    if hi < lo {
        return None;
    }
    let mut weight: Vec<f32> = (lo..=hi).map(|i| { let c = share[nearest[i]] as f32; (c - 1.0).max(0.0) / (2.0 * c) }).collect();
    let total: f32 = weight.iter().sum();
    if total <= 0.0 {
        weight = vec![1.0; hi + 1 - lo];
    }
    let scale = d as f32 / weight.iter().sum::<f32>();
    let far = |cols: &[usize], i: usize| cols.iter().all(|&c| { let g = c.abs_diff(i); g >= 3 && (pinned || n - g >= 3) });
    let mut cols: Vec<usize> = Vec::new();
    let mut acc = 0.5f32;
    for i in lo..=hi {
        acc += weight[i - lo] * scale;
        if acc >= 1.0 && cols.len() < d && far(&cols, i) {
            cols.push(i);
            acc -= 1.0;
        }
    }
    while cols.len() < d {
        let best = (lo..=hi).filter(|&i| far(&cols, i)).max_by(|&a, &b| weight[a - lo].total_cmp(&weight[b - lo]).then(b.cmp(&a)))?;
        cols.push(best);
    }
    cols.sort_unstable();
    Some(cols)
}

/// Points paired with their mirror images moved to agree exactly (a point that is its own
/// image onto the plane).
fn symmetrize(p: &mut [Vec3], plane: MirrorPlane, quad: f32) -> usize {
    let cell = quad.max(1e-6);
    let key = |q: Vec3| { let c = (q / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    for (v, &q) in p.iter().enumerate() {
        grid.entry(key(q)).or_default().push(v as u32);
    }
    let nearest = |q: Vec3, p: &[Vec3]| -> Option<u32> {
        let c = key(q);
        let mut best: Option<(f32, u32)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &u in grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)).into_iter().flatten() {
                        let d = p[u as usize].distance(q);
                        if best.is_none_or(|b| d < b.0) {
                            best = Some((d, u));
                        }
                    }
                }
            }
        }
        best.filter(|b| b.0 < quad * 0.35).map(|b| b.1)
    };
    let partner: Vec<Option<u32>> = (0..p.len()).map(|v| nearest(plane.reflect(p[v]), p)).collect();
    let mut lonely = 0;
    for v in 0..p.len() {
        match partner[v] {
            Some(u) if u as usize == v => p[v] = plane.project(p[v]),
            Some(u) if partner[u as usize] == Some(v as u32) => {
                if v < u as usize {
                    let a = (p[v] + plane.reflect(p[u as usize])) * 0.5;
                    p[v] = a;
                    p[u as usize] = plane.reflect(a);
                }
            }
            _ => lonely += 1,
        }
    }
    lonely
}

/// With `plane`, `w` and `phi` are their own mirror images in it and the result is too: rings
/// it cuts get counts in fours, pinned to it front and back (the centre line); bands between
/// them are built on one half and mirrored.
pub(super) fn sweep_body(w: &Work, phi: &[f32], quad: f32, src: u32, plane: Option<MirrorPlane>) -> Result<(Vec<Vec3>, Vec<[u32; 4]>), String> {
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    let nv = w.p.len();
    let step = quad * std::env::var("ZR_BODYSTEP").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
    let lo = phi[src as usize];
    let band = |v: u32| -> i64 {
        let x = phi[v as usize];
        if x.is_finite() { ((x - lo) / step).floor() as i64 } else { -1 }
    };
    // Pieces: connected parts of each band.
    fn root(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    let mut par: Vec<u32> = (0..nv as u32).collect();
    for v in 0..nv as u32 {
        let b = band(v);
        if b < 0 {
            continue;
        }
        for &u in w.neighbours(v) {
            if u > v && band(u) == b {
                let (a, c) = (root(&mut par, v), root(&mut par, u));
                if a != c {
                    par[a as usize] = c;
                }
            }
        }
    }
    let comp: Vec<u32> = (0..nv as u32).map(|v| root(&mut par, v)).collect();
    // Crossings of each level, per link (nearer piece, farther piece): segments between
    // crossing points, each point keyed by its edge and level.
    type Key = (u32, u32, i64);
    let mut segs: HashMap<(u32, u32), Vec<(Key, Key, usize)>> = HashMap::default();
    let mut at: HashMap<Key, (Vec3, u32, u32)> = HashMap::default();
    for (f, t) in w.t.iter().enumerate() {
        let bs = t.map(band);
        if bs.iter().any(|&b| b < 0) {
            continue;
        }
        let (bmin, bmax) = (*bs.iter().min().unwrap(), *bs.iter().max().unwrap());
        for k in (bmin + 1)..=bmax {
            let lv = lo + k as f32 * step;
            let mut keys: Vec<Key> = Vec::new();
            let mut near = None;
            let mut far = None;
            for e in 0..3 {
                let (a, b) = (t[e], t[(e + 1) % 3]);
                let (ba, bb) = (bs[e], bs[(e + 1) % 3]);
                if (ba < k) != (bb < k) {
                    let key = (a.min(b), a.max(b), k);
                    at.entry(key).or_insert_with(|| {
                        let (fa, fb) = (phi[a as usize], phi[b as usize]);
                        let s = ((lv - fa) / (fb - fa)).clamp(0.0, 1.0);
                        (w.p[a as usize].lerp(w.p[b as usize], s), if ba < k { a } else { b }, if ba < k { b } else { a })
                    });
                    keys.push(key);
                }
            }
            for e in 0..3 {
                if bs[e] == k - 1 {
                    near = Some(comp[t[e] as usize]);
                }
                if bs[e] == k {
                    far = Some(comp[t[e] as usize]);
                }
            }
            if let (2, Some(x), Some(y)) = (keys.len(), near, far) {
                segs.entry((x, y)).or_default().push((keys[0], keys[1], f));
            }
        }
    }
    // Each link's ring: its segments chained into one closed loop.
    let face_grad = |f: usize| -> Vec3 {
        let t = w.t[f];
        let (p0, p1, p2) = (w.p[t[0] as usize], w.p[t[1] as usize], w.p[t[2] as usize]);
        let fv = t.map(|v| phi[v as usize]);
        let n = (p1 - p0).cross(p2 - p0);
        (n.cross(p2 - p1) * fv[0] + n.cross(p0 - p2) * fv[1] + n.cross(p1 - p0) * fv[2]) / n.length_squared().max(1e-24)
    };
    let mut links: Vec<(u32, u32)> = segs.keys().copied().collect();
    links.sort_unstable();
    let mut rings: Vec<Ring> = Vec::new();
    for &(x, y) in &links {
        let sg = &segs[&(x, y)];
        let mut adj: HashMap<Key, Vec<usize>> = HashMap::default();
        for (i, s) in sg.iter().enumerate() {
            adj.entry(s.0).or_default().push(i);
            adj.entry(s.1).or_default().push(i);
        }
        if adj.values().any(|v| v.len() != 2) {
            return Err(format!("a level ring near {:.3} is not one loop", at[&sg[0].0].0));
        }
        let mut order: Vec<Key> = vec![sg[0].0];
        let mut used = vec![false; sg.len()];
        let mut cur_seg = 0usize;
        let mut sense = 0.0f32;
        loop {
            used[cur_seg] = true;
            let s = sg[cur_seg];
            let last = *order.last().unwrap();
            let next = if s.0 == last { s.1 } else { s.0 };
            let d = at[&next].0 - at[&last].0;
            sense += w.fnrm[s.2].cross(d).dot(face_grad(s.2));
            if next == order[0] {
                break;
            }
            order.push(next);
            let Some(&ns) = adj[&next].iter().find(|&&i| !used[i]) else { return Err("a level ring does not close".into()) };
            cur_seg = ns;
        }
        if used.iter().any(|u| !u) {
            return Err(format!("a level ring near {:.3} is more than one loop", at[&sg[0].0].0));
        }
        if sense < 0.0 {
            order.reverse();
        }
        let pts: Vec<Vec3> = order.iter().map(|k| at[k].0).collect();
        let near: Vec<u32> = order.iter().map(|k| at[k].1).collect();
        let far: Vec<u32> = order.iter().map(|k| at[k].2).collect();
        let girth = (0..pts.len()).map(|i| pts[i].distance(pts[(i + 1) % pts.len()])).sum::<f32>() / quad;
        rings.push(Ring { pts, near, far, from: x, to: y, girth });
    }
    // Arc length along each ring, and with a plane where it crosses it: (front, back) arc
    // positions, front the crossing further forward (+Z, or +X for a Z plane); None if it
    // does not cross exactly twice.
    let ring_cum: Vec<Vec<f32>> = rings.iter().map(|r| {
        let m = r.pts.len();
        let mut c = vec![0.0f32];
        for q in 0..m {
            c.push(c[q] + r.pts[q].distance(r.pts[(q + 1) % m]));
        }
        c
    }).collect();
    let fwd = plane.map_or(2, |pl| if pl.axis == Axis::Z { 0 } else { 2 });
    let ring_cross: Vec<Option<(f32, f32)>> = rings.iter().enumerate().map(|(ri, r)| {
        let pl = plane?;
        let m = r.pts.len();
        let mut xs: Vec<(f32, Vec3)> = Vec::new();
        for q in 0..m {
            let (a, b) = (pl.distance(r.pts[q]), pl.distance(r.pts[(q + 1) % m]));
            if (a >= 0.0) != (b >= 0.0) {
                let t = a / (a - b);
                xs.push((ring_cum[ri][q] + t * (ring_cum[ri][q + 1] - ring_cum[ri][q]), r.pts[q].lerp(r.pts[(q + 1) % m], t)));
            }
        }
        if xs.len() != 2 {
            return None;
        }
        Some(if xs[0].1[fwd] >= xs[1].1[fwd] { (xs[0].0, xs[1].0) } else { (xs[1].0, xs[0].0) })
    }).collect();
    let point_at = |r: usize, sd: f32| -> Vec3 {
        let c = &ring_cum[r];
        let m = rings[r].pts.len();
        let total = c[m];
        let sd = sd.rem_euclid(total.max(1e-12));
        let i = c.partition_point(|&x| x <= sd).saturating_sub(1).min(m - 1);
        let u = ((sd - c[i]) / (c[i + 1] - c[i]).max(1e-12)).clamp(0.0, 1.0);
        rings[r].pts[i].lerp(rings[r].pts[(i + 1) % m], u)
    };
    let mut outs: HashMap<u32, Vec<usize>> = HashMap::default();
    let mut ins: HashMap<u32, Vec<usize>> = HashMap::default();
    for (i, r) in rings.iter().enumerate() {
        outs.entry(r.from).or_default().push(i);
        ins.entry(r.to).or_default().push(i);
    }
    let min_girth: f32 = std::env::var("ZR_BODYMIN").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
    // The rings past a piece that are kept (thin ones, fingers and ears, are left off).
    let kept = |p: u32| -> Vec<usize> {
        let mut v: Vec<usize> = outs.get(&p).map(|v| v.iter().copied().filter(|&r| rings[r].girth >= min_girth).collect()).unwrap_or_default();
        v.sort_by(|&a, &b| rings[b].girth.total_cmp(&rings[a].girth));
        v
    };
    // The rings of the run that starts with ring `r`: on while one ring is kept.
    let chain_from = |r: usize| -> Vec<usize> {
        let mut out = vec![r];
        let mut p = rings[r].to;
        loop {
            let k = kept(p);
            if k.len() != 1 || out.len() > 1_000_000 {
                break;
            }
            out.push(k[0]);
            p = rings[k[0]].to;
        }
        out
    };
    // Counts along a run: one count (the geometric mean of its girths) while its widest ring
    // is at most `ZR_BODYRATIO` (3) times its narrowest, as a box modeller keeps one count
    // along a limb or the trunk; else stepping where the girth drifts past 1.5 times.
    let ratio: f32 = std::env::var("ZR_BODYRATIO").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
    let drift: f32 = std::env::var("ZR_BODYDRIFT").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
    let plan_one = |rs: &[usize]| -> Vec<usize> {
        let gs: Vec<f32> = rs.iter().map(|&r| rings[r].girth.max(1e-3)).collect();
        let q = |g: f32| if ring_cross[rs[0]].is_some() { even4(g) } else { even(g) };
        let (mn, mx) = gs.iter().fold((f32::INFINITY, 0.0f32), |m, &g| (m.0.min(g), m.1.max(g)));
        if mx <= mn * ratio {
            let gm = (gs.iter().map(|g| g.ln()).sum::<f32>() / gs.len() as f32).exp();
            return vec![q(gm); gs.len()];
        }
        let mut n = q(gs[0]);
        gs.iter().map(|&g| {
            if g > n as f32 * drift || g < n as f32 / drift {
                n = q(g);
            }
            n
        }).collect()
    };
    // A run that narrows and then flares again near its end (the ankle into the foot, the
    // wrist into the hand) is two: the limb with one count to its narrowest ring, then the
    // foot or hand with its own (`ZR_NOANKLE` off). The foot's or hand's rings are noted:
    // their reductions go on the sole.
    let after_ankle: std::cell::RefCell<HashSet<usize>> = std::cell::RefCell::new(HashSet::default());
    let plan = |rs: &[usize]| -> Vec<usize> {
        let n = rs.len();
        if n >= 8 && std::env::var_os("ZR_NOANKLE").is_none() {
            let gs: Vec<f32> = rs.iter().map(|&r| rings[r].girth).collect();
            let from = n * 2 / 5;
            if let Some(e) = (from..n.saturating_sub(2)).min_by(|&a, &b| gs[a].total_cmp(&gs[b])) {
                let tail_max = gs[e + 1..].iter().copied().fold(0.0f32, f32::max);
                if tail_max > gs[e] * 1.3 {
                    after_ankle.borrow_mut().extend(rs[e + 1..].iter().copied());
                    let mut out = plan_one(&rs[..=e]);
                    out.extend(plan_one(&rs[e + 1..]));
                    return out;
                }
            }
        }
        plan_one(rs)
    };
    // A split: shortest strips between its rings through the piece (a tree), each ring's
    // count its run's.
    let split_plan = |p: u32, rs: &[usize], totals: &HashMap<u32, usize>| -> Result<Split, String> {
        let n = rs.len();
        let inside = |v: u32| comp[v as usize] == p;
        // From each ring: distance through the piece, and where it came from.
        let mut best: Vec<Vec<Option<(f32, usize, usize, Vec<Vec3>)>>> = vec![vec![None; n]; n];
        for i in 0..n {
            let ri = &rings[rs[i]];
            let mut d: HashMap<u32, f32> = HashMap::default();
            let mut from: HashMap<u32, u32> = HashMap::default();
            let mut start: HashMap<u32, usize> = HashMap::default();
            let mut heap = std::collections::BinaryHeap::new();
            for (ci, &v) in ri.near.iter().enumerate() {
                let dd = ri.pts[ci].distance(w.p[v as usize]);
                if d.get(&v).is_none_or(|&x| dd < x) {
                    d.insert(v, dd);
                    start.insert(v, ci);
                    heap.push((std::cmp::Reverse(dd.to_bits()), v));
                }
            }
            while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
                let dv = f32::from_bits(db);
                if dv > d[&v] {
                    continue;
                }
                for &u in w.neighbours(v) {
                    if !inside(u) {
                        continue;
                    }
                    let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                    if d.get(&u).is_none_or(|&x| nd < x) {
                        d.insert(u, nd);
                        from.insert(u, v);
                        let s0 = start[&v];
                        start.insert(u, s0);
                        heap.push((std::cmp::Reverse(nd.to_bits()), u));
                    }
                }
            }
            for j in 0..n {
                if j == i {
                    continue;
                }
                let rj = &rings[rs[j]];
                let mut bestj: Option<(f32, usize, u32)> = None;
                for (cj, &v) in rj.near.iter().enumerate() {
                    if let Some(&dv) = d.get(&v) {
                        let tot = dv + rj.pts[cj].distance(w.p[v as usize]);
                        if bestj.is_none_or(|b| tot < b.0) {
                            bestj = Some((tot, cj, v));
                        }
                    }
                }
                if let Some((len, cj, v)) = bestj {
                    let mut path = vec![rj.pts[cj]];
                    let mut x = v;
                    path.push(w.p[x as usize]);
                    while let Some(&y) = from.get(&x) {
                        x = y;
                        path.push(w.p[x as usize]);
                    }
                    let ci = start[&v];
                    path.push(ri.pts[ci]);
                    path.reverse();
                    best[i][j] = Some((len, ci, cj, path));
                }
            }
        }
        // Tree: shortest strips first (Kruskal).
        let mut cand: Vec<(f32, usize, usize)> = Vec::new();
        for i in 0..n {
            for j in (i + 1)..n {
                if let Some(b) = &best[i][j] {
                    cand.push((b.0, i, j));
                }
            }
        }
        cand.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut grp: Vec<usize> = (0..n).collect();
        fn g(grp: &mut [usize], mut x: usize) -> usize {
            while grp[x] != x {
                x = grp[x];
            }
            x
        }
        let mut strips = Vec::new();
        for (_, i, j) in cand {
            let (a, b) = (g(&mut grp, i), g(&mut grp, j));
            if a != b {
                grp[a] = b;
                let (_, mut ci, mut cj, mut path) = best[i][j].clone().unwrap();
                let len = |path: &[Vec3]| path.windows(2).map(|e| e[0].distance(e[1])).sum::<f32>();
                let mut m = ((len(&path) / quad).round() as usize).max(1);
                // Between two rings either side of the plane (the crotch): the half from the
                // first ring to the plane, and its mirror image; an even count, its middle on
                // the plane.
                if let Some(pl) = plane {
                    let (ri, rj) = (&rings[rs[i]], &rings[rs[j]]);
                    let side = |r: &Ring| { let d: f32 = r.pts.iter().map(|&q| pl.distance(q)).sum(); d };
                    if ring_cross[rs[i]].is_none() && ring_cross[rs[j]].is_none() && side(ri) * side(rj) < 0.0 {
                        let mut d: HashMap<u32, f32> = HashMap::default();
                        let mut from: HashMap<u32, u32> = HashMap::default();
                        let mut start: HashMap<u32, usize> = HashMap::default();
                        let mut heap = std::collections::BinaryHeap::new();
                        for (c, &v) in ri.near.iter().enumerate() {
                            let dd = ri.pts[c].distance(w.p[v as usize]);
                            if d.get(&v).is_none_or(|&x| dd < x) {
                                d.insert(v, dd);
                                start.insert(v, c);
                                heap.push((std::cmp::Reverse(dd.to_bits()), v));
                            }
                        }
                        let mut hit: Option<u32> = None;
                        while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
                            let dv = f32::from_bits(db);
                            if dv > d[&v] {
                                continue;
                            }
                            if pl.distance(w.p[v as usize]) == 0.0 {
                                hit = Some(v);
                                break;
                            }
                            for &u in w.neighbours(v) {
                                if !inside(u) {
                                    continue;
                                }
                                let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                                if d.get(&u).is_none_or(|&x| nd < x) {
                                    d.insert(u, nd);
                                    from.insert(u, v);
                                    let s0 = start[&v];
                                    start.insert(u, s0);
                                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                                }
                            }
                        }
                        if let Some(v) = hit {
                            let mut half = vec![w.p[v as usize]];
                            let mut x = v;
                            while let Some(&y) = from.get(&x) {
                                x = y;
                                half.push(w.p[x as usize]);
                            }
                            ci = start[&v];
                            half.push(ri.pts[ci]);
                            half.reverse();
                            let target = pl.reflect(ri.pts[ci]);
                            cj = (0..rj.pts.len()).min_by(|&a, &b| rj.pts[a].distance(target).total_cmp(&rj.pts[b].distance(target))).unwrap_or(0);
                            let mut full = half.clone();
                            full.extend(half.iter().rev().skip(1).map(|&q| pl.reflect(q)));
                            let n = full.len();
                            full[n - 1] = rj.pts[cj];
                            path = full;
                            m = (((len(&path) / quad) * 0.5).round() as usize).max(1) * 2;
                        }
                    }
                }
                strips.push((i, ci, j, cj, path, m));
            }
        }
        if strips.len() + 1 != n {
            return Err("a split's branches are not joined".into());
        }
        // A branch that splits again at once takes that split's count (no band between to
        // change it in).
        let counts: Vec<usize> = rs.iter().map(|&r| {
            let run = chain_from(r);
            match totals.get(&rings[r].to) {
                Some(&t) if run.len() == 1 => t,
                _ => plan(&run)[0],
            }
        }).collect();
        let total = counts.iter().sum::<usize>() + strips.iter().map(|s| 2 * s.5).sum::<usize>();
        Ok(Split { rings: rs.to_vec(), counts, strips, total })
    };
    let mut out_p: Vec<Vec3> = Vec::new();
    let mut quads: Vec<[u32; 4]> = Vec::new();
    // A row: `n` points evenly round ring `r`. With a plane: a ring it cuts has points on it
    // front (0) and back (n/2) and each half spaced evenly, the halves mirror images; any
    // other starts at its frontmost point (so a ring and its mirror image agree). Without:
    // turned to lie against `prev` (if any).
    let make_row = |r: usize, n: usize, prev: Option<&[Vec3]>, out_p: &mut Vec<Vec3>| -> Vec<u32> {
        let pts = &rings[r].pts;
        let mut row = resample_loop(pts, n);
        if let (Some(pl), Some((f, b))) = (plane, ring_cross[r]) {
            if n % 2 == 0 {
                let total = ring_cum[r][pts.len()];
                let half = n / 2;
                let l1 = (b - f).rem_euclid(total);
                let l2 = total - l1;
                row = (0..half).map(|t| point_at(r, f + l1 * t as f32 / half as f32)).chain((0..half).map(|t| point_at(r, b + l2 * t as f32 / half as f32))).collect();
                row[0] = pl.project(row[0]);
                row[half] = pl.project(row[half]);
                for i in 1..half {
                    row[n - i] = pl.reflect(row[i]);
                }
            }
        } else if plane.is_some() {
            let k = (0..pts.len()).max_by(|&a, &b| pts[a][fwd].total_cmp(&pts[b][fwd])).unwrap_or(0);
            let rot: Vec<Vec3> = (0..pts.len()).map(|i| pts[(i + k) % pts.len()]).collect();
            row = resample_loop(&rot, n);
        } else if let Some(prev) = prev {
            let against = if prev.len() == n { prev.to_vec() } else { resample_loop(prev, n) };
            let sh = (0..n).min_by(|&a, &b| {
                let ca: f32 = (0..n).map(|i| against[i].distance_squared(row[(i + a) % n])).sum();
                let cb: f32 = (0..n).map(|i| against[i].distance_squared(row[(i + b) % n])).sum();
                ca.total_cmp(&cb)
            }).unwrap_or(0);
            row = (0..n).map(|i| row[(i + sh) % n]).collect();
        }
        row.into_iter().map(|q| { out_p.push(q); (out_p.len() - 1) as u32 }).collect()
    };
    // Whether rows of ring `r` are pinned to the plane (and n their count).
    let pinned = |r: usize, n: usize| plane.is_some() && ring_cross[r].is_some() && n % 2 == 0;
    // A band between two rows: plain quads at equal counts; at different counts, diamonds
    // (`ZR_OLDMERGE` off): a step up to a quarter of the smaller count gathers them where
    // `prefer` says, a bigger one spreads them where the rows diverge; else (an odd
    // difference, or more than fit) the merges of join_sym and join_band.
    let old_merge = std::env::var_os("ZR_OLDMERGE").is_some();
    let join_rows = |row: &[u32], row_sym: bool, next: &[u32], next_sym: bool, prefer: Pref, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>| {
        let (n0, n1) = (row.len(), next.len());
        let sym = plane.is_some() && row_sym && next_sym;
        if n0 != n1 && !old_merge && n0.abs_diff(n1) % 2 == 0 {
            let (long, short) = if n0 > n1 { (row, next) } else { (next, row) };
            let mut long_pos: Vec<Vec3> = long.iter().map(|&v| out_p[v as usize]).collect();
            let mut short_pos: Vec<Vec3> = short.iter().map(|&v| out_p[v as usize]).collect();
            let d = n0.abs_diff(n1) / 2;
            let pinned = plane.is_some() && sym;
            // A limb on the far side of the plane is decided as its mirror image (its rows
            // reflected and run the other way), so the two limbs come out alike.
            let far_side = match plane {
                Some(pl) if !pinned => pl.distance(long_pos.iter().copied().sum::<Vec3>() / long_pos.len() as f32) < 0.0,
                _ => false,
            };
            if far_side {
                for pos in [&mut long_pos, &mut short_pos] {
                    let pl = plane.unwrap();
                    let n = pos.len();
                    *pos = (0..n).map(|i| pl.reflect(pos[(n - i) % n])).collect();
                }
            }
            // Gathered when the step is small and the rows taper evenly enough that the
            // gathered band reaches across little further than one spread where the rows
            // diverge (`ZR_KITEGATHER`, 2, how much further it may; 0 spreads every band).
            let gather_most: f32 = std::env::var("ZR_KITEGATHER").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
            let dk = if pinned { d / 2 } else { d };
            let (cols_g, cols_s) = if pinned && d % 2 != 0 {
                (None, None)
            } else {
                (if d <= n0.min(n1) / 4 { reduce_cols(&long_pos, dk, prefer, plane, fwd, pinned) } else { None }, fan_cols(&long_pos, &short_pos, dk, pinned))
            };
            let costs = (cols_g.as_ref().map(|c| kite_cost(&long_pos, &short_pos, c, pinned)), cols_s.as_ref().map(|c| kite_cost(&long_pos, &short_pos, c, pinned)));
            let gather = match costs {
                (Some(g), Some(s)) => g <= s * gather_most,
                (Some(_), None) => true,
                _ => false,
            };
            let cols = (if gather { cols_g } else { cols_s }).map(|c| {
                if far_side {
                    let n = long.len();
                    let mut back: Vec<usize> = c.iter().map(|&i| (n - i) % n).collect();
                    back.sort_unstable();
                    back
                } else {
                    c
                }
            });
            let built = match plane {
                Some(pl) if sym => {
                    cols.filter(|c| reduce_sym(row, next, c, pl, out_p, quads)).map(|c| {
                        let n = long.len();
                        let mut all: Vec<usize> = c.iter().map(|&i| n - i).chain(c.iter().copied()).collect();
                        all.sort_unstable();
                        all
                    })
                }
                _ => cols.filter(|c| reduce_band(row, next, c, out_p, quads)),
            };
            if let Some(cols) = built {
                if debug {
                    let how = if gather { format!("{prefer:?}") } else { "spread".to_string() };
                    let (g, sp) = (costs.0.unwrap_or(f32::NAN), costs.1.unwrap_or(f32::NAN));
                    eprintln!("    reduce: {n0} -> {n1}, {d} diamonds at cols {cols:?} ({how}; reach gathered {g:.3}, spread {sp:.3})");
                }
                return;
            }
        }
        let done = match plane {
            Some(pl) if sym => join_sym(row, next, out_p, quads, pl),
            _ => false,
        };
        if !done {
            join_band(row, next, out_p, quads);
        }
    };
    // A grid over a row (an even count): a × b quads, its border the row. On a pinned row
    // (`sym`): a even and the row's point 0 the middle of a side, so a grid line runs along
    // the plane.
    let cap = |row: &[u32], sym: bool, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>| {
        let n = row.len();
        let mut a = (n / 4).max(1);
        if sym && a % 2 == 1 && n >= 8 {
            a += 1;
        }
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
    };
    // A cap's inner points lifted toward the end beyond its row (a dome: all the way at its
    // middle, none at the row), so it reaches the tip of a finger, a toe or the head instead
    // of lying flat across the last ring.
    let dome = |row: &[u32], from: usize, tip: Vec3, out_p: &mut Vec<Vec3>| {
        let pts: Vec<Vec3> = row.iter().map(|&v| out_p[v as usize]).collect();
        let c = pts.iter().copied().sum::<Vec3>() / pts.len().max(1) as f32;
        let rr = pts.iter().map(|q| q.distance(c)).sum::<f32>() / pts.len().max(1) as f32;
        let lift = tip - c;
        for v in from..out_p.len() {
            let d = out_p[v].distance(c) / rr.max(1e-9);
            out_p[v] += lift * (1.0 - d * d).max(0.0);
        }
    };
    // The farthest point past a piece: the end of everything beyond it, thin rings too.
    let mut far: HashMap<u32, u32> = HashMap::default();
    for v in 0..nv as u32 {
        if phi[v as usize].is_finite() {
            let e = far.entry(comp[v as usize]).or_insert(v);
            if phi[v as usize] > phi[*e as usize] {
                *e = v;
            }
        }
    }
    let tip_past = |p: u32| -> Vec3 {
        let mut best = far.get(&p).copied().unwrap_or(p);
        let mut stack = vec![p];
        let mut seen: HashSet<u32> = [p].into_iter().collect();
        while let Some(q) = stack.pop() {
            if let Some(&f) = far.get(&q) {
                if phi[f as usize] > phi[best as usize] {
                    best = f;
                }
            }
            for &r in outs.get(&q).into_iter().flatten() {
                if seen.insert(rings[r].to) {
                    stack.push(rings[r].to);
                }
            }
        }
        w.p[best as usize]
    };
    // Each split's bridged count, the deepest first (a split right above another takes its
    // count).
    let mut totals: HashMap<u32, usize> = HashMap::default();
    {
        let mut sp_pieces: Vec<u32> = outs.keys().copied().filter(|&p| kept(p).len() >= 2).collect();
        sp_pieces.sort_by_key(|&p| (std::cmp::Reverse(band(p)), p));
        for p in sp_pieces {
            if let Ok(sp) = split_plan(p, &kept(p), &totals) {
                totals.insert(p, sp.total);
            }
        }
    }
    // Down from the head: its run of rings to the first split. The head is its widest ring
    // before it narrows to the neck; the cap is the grid inside the first ring past
    // `ZR_CAPFRAC` (0.6) of that (the top of a quad sphere); one count from there to the neck,
    // then the neck widening into the shoulders.
    let head_run: Vec<usize> = {
        let mut out = Vec::new();
        let mut p = comp[src as usize];
        loop {
            let k: Vec<usize> = outs.get(&p).cloned().unwrap_or_default();
            if k.len() != 1 || out.len() > 1_000_000 {
                break;
            }
            out.push(k[0]);
            p = rings[k[0]].to;
        }
        out
    };
    let hg: Vec<f32> = head_run.iter().map(|&r| rings[r].girth).collect();
    if hg.len() < 4 {
        return Err("the head is too short".into());
    }
    let mut widest = 0usize;
    for i in 0..hg.len() {
        if hg[i] > hg[widest] {
            widest = i;
        }
        if hg[i] < hg[widest] * 0.85 && hg[widest] >= min_girth {
            break;
        }
    }
    let mut neck = widest;
    for i in widest..hg.len() {
        if hg[i] < hg[neck] {
            neck = i;
        }
        if hg[i] > hg[neck] * 1.15 {
            break;
        }
    }
    let cap_frac: f32 = std::env::var("ZR_CAPFRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.6);
    let first = (0..=widest).find(|&i| hg[i] >= hg[widest] * cap_frac && hg[i] >= 4.0).ok_or("no ring to cap the head")?;
    if debug {
        eprintln!("  body: head widest {:.1} quads, neck {:.1}, capped at {:.1} ({} rings down)", hg[widest], hg[neck], hg[first], first);
    }
    let mut counts: HashMap<usize, usize> = HashMap::default();
    let head_counts = plan(&head_run[first..=neck.max(first)]);
    for (i, &c) in head_counts.iter().enumerate() {
        counts.insert(head_run[first + i], c);
    }
    if neck + 1 < head_run.len() {
        for (i, c) in plan(&head_run[neck + 1..]).into_iter().enumerate() {
            counts.insert(head_run[neck + 1 + i], c);
        }
    }
    let first = head_run[first];
    let row0 = make_row(first, counts[&first], None, &mut out_p);
    let cap_from = out_p.len();
    cap(&row0, pinned(first, row0.len()), &mut out_p, &mut quads);
    dome(&row0, cap_from, w.p[src as usize], &mut out_p);
    // Points of the head (its cap and rows down to the neck): where the eyes are looked for.
    let mut head: HashSet<u32> = row0.iter().copied().chain(cap_from as u32..out_p.len() as u32).collect();
    let mut stack: Vec<(u32, Vec<u32>, bool, usize)> = vec![(rings[first].to, row0.clone(), pinned(first, row0.len()), first)];
    // Rows waiting where rings join (a loop in the model), with the ring each came through.
    let mut pending: HashMap<u32, Vec<(Vec<u32>, usize)>> = HashMap::default();
    let mut joins = 0;
    let mut splits = 0;
    let mut caps = 1;
    // Points of the torso's rows (rings the plane cuts, below the neck): where bumps (breasts)
    // are looked for.
    let mut torso: HashSet<u32> = HashSet::default();
    let head_rings: HashSet<usize> = head_run[..=neck.min(head_run.len() - 1)].iter().copied().collect();
    // The upper face: the head's rows down to a third of the way from its widest ring to the
    // neck (the jaw and chin below have their own dips).
    let upper_face: HashSet<usize> = head_run[..=(widest + (neck.saturating_sub(widest)) / 3).min(head_run.len() - 1)].iter().copied().collect();
    // The lower face: from just above the head's widest ring to the neck (the mouth).
    let lower_face_rings: HashSet<usize> = head_run[widest.saturating_sub(2)..neck.min(head_run.len() - 1)].iter().copied().collect();
    let mut lower_face: HashSet<u32> = HashSet::default();
    while let Some((p, row, row_sym, in_ring)) = stack.pop() {
        // Where rings join (a handle: a strap, a collar, an arm touching the body): the rows
        // wait until all have come; then short strips through the piece bridge them into one
        // loop (as at a split, upside down), which is joined to the ring past it, or, if the
        // piece also splits or ends, goes on as the ring into it. If the strips cannot be
        // found, the body is not built (planned instead).
        let in_k = ins.get(&p).map_or(0, |v| v.iter().filter(|&&r| rings[r].girth >= min_girth).count());
        let (row, row_sym) = if in_k > 1 {
            let waiting = pending.entry(p).or_default();
            waiting.push((row, in_ring));
            if waiting.len() < in_k {
                continue;
            }
            let arrived = pending.remove(&p).unwrap();
            let out = kept(p);
            let joined = (|| -> Result<Option<Vec<u32>>, String> {
                let n_in = arrived.len();
                let inside = |v: u32| comp[v as usize] == p;
                // Shortest paths through the piece between each pair of rings, from their
                // crossings' far vertices.
                let mut best: Vec<Vec<Option<(f32, Vec<Vec3>)>>> = vec![vec![None; n_in]; n_in];
                for i in 0..n_in {
                    let ri = &rings[arrived[i].1];
                    let mut d: HashMap<u32, f32> = HashMap::default();
                    let mut from: HashMap<u32, u32> = HashMap::default();
                    let mut heap = std::collections::BinaryHeap::new();
                    for &v in &ri.far {
                        if inside(v) && d.insert(v, 0.0).is_none() {
                            heap.push((std::cmp::Reverse(0u32), v));
                        }
                    }
                    while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
                        let dv = f32::from_bits(db);
                        if dv > d[&v] {
                            continue;
                        }
                        for &u in w.neighbours(v) {
                            if !inside(u) {
                                continue;
                            }
                            let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                            if d.get(&u).is_none_or(|&x| nd < x) {
                                d.insert(u, nd);
                                from.insert(u, v);
                                heap.push((std::cmp::Reverse(nd.to_bits()), u));
                            }
                        }
                    }
                    for j in 0..n_in {
                        if j == i {
                            continue;
                        }
                        let target = rings[arrived[j].1].far.iter().copied().filter(|v| d.contains_key(v)).min_by(|a, b| d[a].total_cmp(&d[b]));
                        if let Some(v) = target {
                            let mut path = vec![w.p[v as usize]];
                            let mut x = v;
                            while let Some(&y) = from.get(&x) {
                                x = y;
                                path.push(w.p[x as usize]);
                            }
                            path.reverse();
                            best[i][j] = Some((d[&v], path));
                        }
                    }
                }
                let mut cand: Vec<(f32, usize, usize)> = Vec::new();
                for i in 0..n_in {
                    for j in i + 1..n_in {
                        if let Some(b) = &best[i][j] {
                            cand.push((b.0, i, j));
                        }
                    }
                }
                cand.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut grp: Vec<usize> = (0..n_in).collect();
                let mut strips: Vec<(usize, usize, usize, usize, Vec<Vec3>, usize)> = Vec::new();
                let rows_in: Vec<Vec<u32>> = arrived.iter().map(|a| a.0.clone()).collect();
                let near_pt = |row: &[u32], q: Vec3, out_p: &[Vec3]| (0..row.len()).min_by(|&a, &b| out_p[row[a] as usize].distance(q).total_cmp(&out_p[row[b] as usize].distance(q))).unwrap_or(0);
                for (_, i, j) in cand {
                    let (mut a, mut b) = (i, j);
                    while grp[a] != a { a = grp[a]; }
                    while grp[b] != b { b = grp[b]; }
                    if a == b {
                        continue;
                    }
                    grp[a] = b;
                    let path = best[i][j].clone().unwrap().1;
                    let (ai, aj) = (near_pt(&rows_in[i], path[0], &out_p), near_pt(&rows_in[j], *path.last().unwrap(), &out_p));
                    let mut full = vec![out_p[rows_in[i][ai] as usize]];
                    full.extend(path.iter().copied());
                    full.push(out_p[rows_in[j][aj] as usize]);
                    let len: f32 = full.windows(2).map(|e| e[0].distance(e[1])).sum();
                    let m = ((len / quad).round() as usize).max(1);
                    strips.push((i, ai, j, aj, full, m));
                }
                if strips.len() + 1 != n_in {
                    return Err("the joining rings are not connected through the piece".into());
                }
                let att_row: Vec<HashMap<usize, usize>> = (0..n_in).map(|i| strips.iter().flat_map(|s| [(s.0, s.1), (s.2, s.3)]).filter(|x| x.0 == i).map(|x| (x.1, x.1)).collect()).collect();
                let mut strip_pts: Vec<Vec<u32>> = Vec::new();
                for s in &strips {
                    let (path, m) = (&s.4, s.5);
                    let pts: Vec<Vec3> = {
                        let mut cum = vec![0.0f32];
                        for e in path.windows(2) {
                            cum.push(cum.last().unwrap() + e[0].distance(e[1]));
                        }
                        let len = *cum.last().unwrap();
                        (1..m).map(|t| {
                            let goal = len * t as f32 / m as f32;
                            let idx = cum.partition_point(|&x| x <= goal).saturating_sub(1).min(path.len() - 2);
                            let u = ((goal - cum[idx]) / (cum[idx + 1] - cum[idx]).max(1e-12)).clamp(0.0, 1.0);
                            path[idx].lerp(path[idx + 1], u)
                        }).collect()
                    };
                    strip_pts.push(pts.into_iter().map(|q| { out_p.push(q); (out_p.len() - 1) as u32 }).collect());
                }
                let mut seq: Vec<u32> = Vec::new();
                tour(0, 0, None, &rows_in, &att_row, &strips, &strip_pts, &mut seq, true);
                // Splitting too, or ending: the loop goes on as the ring into the piece.
                if out.len() != 1 {
                    if debug {
                        eprintln!("    body join near {:.3}: rows {:?} bridged into {} round, then {} ways on", w.p[p as usize], rows_in.iter().map(|r| r.len()).collect::<Vec<_>>(), seq.len(), out.len());
                    }
                    return Ok(Some(seq));
                }
                let seq_pos: Vec<Vec3> = seq.iter().map(|&i| out_p[i as usize]).collect();
                let r = out[0];
                let next = make_row(r, seq.len(), Some(&seq_pos), &mut out_p);
                join_band(&seq, &next, &mut out_p, &mut quads);
                if debug {
                    eprintln!("    body join near {:.3}: rows {:?} bridged into {} round", w.p[p as usize], rows_in.iter().map(|r| r.len()).collect::<Vec<_>>(), seq.len());
                }
                let sym = pinned(r, next.len());
                stack.push((rings[r].to, next, sym, r));
                Ok(None)
            })();
            joins += 1;
            match joined {
                Ok(None) => continue,
                Ok(Some(seq)) => (seq, false),
                // A part of the body left unbuilt would be worse than the planned layout.
                Err(e) => return Err(format!("rings join near {:.3}: {e}", w.p[p as usize])),
            }
        } else {
            (row, row_sym)
        };
        if quads.len() > 4_000_000 {
            return Err("too many quads".into());
        }
        let k = kept(p);
        let row_pos: Vec<Vec3> = row.iter().map(|&i| out_p[i as usize]).collect();
        match k.len() {
            0 => {
                // Rows on to the tip (a finger's end is thinner than the rings kept), shrinking
                // toward it like the end of a sphere, then the cap.
                let tip = tip_past(p);
                let c = row_pos.iter().copied().sum::<Vec3>() / row_pos.len() as f32;
                let extra = (tip.distance(c) / quad).round() as usize;
                let mut last = row.clone();
                for k in 1..=extra.min(8) {
                    let t = k as f32 / (extra as f32 + 1.0);
                    let s = (1.0 - t * t).max(0.0).sqrt();
                    let cc = c.lerp(tip, t);
                    let next: Vec<u32> = row_pos.iter().map(|&q| { out_p.push(cc + (q - c) * s); (out_p.len() - 1) as u32 }).collect();
                    join_band(&last, &next, &mut out_p, &mut quads);
                    last = next;
                }
                let from = out_p.len();
                cap(&last, row_sym, &mut out_p, &mut quads);
                dome(&last, from, tip, &mut out_p);
                caps += 1;
            }
            1 => {
                let r = k[0];
                let g = rings[r].girth;
                let mut n = match counts.get(&r) {
                    Some(&c) => c,
                    None => {
                        let n0 = row.len();
                        if g > n0 as f32 * drift || g < n0 as f32 / drift { even(g) } else { n0 }
                    }
                };
                // Before a split, the count it bridges into (the merges go in this band, not
                // in the split's).
                let nk = kept(rings[r].to);
                if nk.len() >= 2 {
                    if let Ok(sp) = split_plan(rings[r].to, &nk, &totals) {
                        if (sp.total as f32) < g * 1.8 && (sp.total as f32) > g / 1.8 {
                            n = sp.total;
                        }
                    }
                }
                let next = make_row(r, n, Some(&row_pos), &mut out_p);
                let next_sym = pinned(r, n);
                // Where this band's reductions go: the back of the head, neck and trunk, the
                // inside of a limb, the sole of a foot or hand.
                let prefer = if plane.is_none() || ring_cross[r].is_some() || head_run.contains(&r) {
                    Pref::Back
                } else if after_ankle.borrow().contains(&r) {
                    Pref::Sole
                } else {
                    Pref::Inside
                };
                join_rows(&row, row_sym, &next, next_sym, prefer, &mut out_p, &mut quads);
                if next_sym && !head_rings.contains(&r) {
                    torso.extend(next.iter().copied());
                }
                if upper_face.contains(&r) {
                    head.extend(next.iter().copied());
                }
                if lower_face_rings.contains(&r) {
                    lower_face.extend(next.iter().copied());
                }
                stack.push((rings[r].to, next, next_sym, r));
            }
            _ => {
                let sp = split_plan(p, &k, &totals)?;
                splits += 1;
                let nr = sp.rings.len();
                // Attachment crossings per ring.
                let mut attach: Vec<Vec<usize>> = vec![Vec::new(); nr];
                for s in &sp.strips {
                    attach[s.0].push(s.1);
                    attach[s.2].push(s.3);
                }
                // Each ring's row: its count spread over the arcs between its attachments,
                // each arc starting at its attachment.
                let mut rows: Vec<Vec<u32>> = Vec::new();
                let mut att_row: Vec<HashMap<usize, usize>> = Vec::new();
                let mut rows_sym: Vec<bool> = Vec::new();
                for i in 0..nr {
                    let ring = &rings[sp.rings[i]];
                    let m = ring.pts.len();
                    let mut a = attach[i].clone();
                    a.sort_unstable();
                    a.dedup();
                    // A ring the plane cuts: its pinned row, each strip end moved onto the
                    // row point nearest it.
                    if pinned(sp.rings[i], sp.counts[i]) {
                        let ids = make_row(sp.rings[i], sp.counts[i], None, &mut out_p);
                        let mut amap: HashMap<usize, usize> = HashMap::default();
                        for &c in &a {
                            let q = ring.pts[c];
                            let k = (0..ids.len()).filter(|&k| k != 0 && k != ids.len() / 2).min_by(|&x, &y| out_p[ids[x] as usize].distance(q).total_cmp(&out_p[ids[y] as usize].distance(q))).unwrap_or(1);
                            out_p[ids[k] as usize] = q;
                            amap.insert(c, k);
                        }
                        rows.push(ids);
                        att_row.push(amap);
                        rows_sym.push(true);
                        continue;
                    }
                    rows_sym.push(false);
                    let cum: Vec<f32> = {
                        let mut c = vec![0.0f32];
                        for q in 0..m {
                            c.push(c[q] + ring.pts[q].distance(ring.pts[(q + 1) % m]));
                        }
                        c
                    };
                    let total = cum[m];
                    let arc_len = |s: usize, e: usize| -> f32 { if e > s { cum[e] - cum[s] } else { total - cum[s] + cum[e] } };
                    let na = a.len();
                    let lens: Vec<f32> = (0..na).map(|q| if na == 1 { total } else { arc_len(a[q], a[(q + 1) % na]) }).collect();
                    let cnt_total = sp.counts[i];
                    let mut cnts: Vec<usize> = lens.iter().map(|l| ((l / total.max(1e-9)) * cnt_total as f32).round().max(1.0) as usize).collect();
                    while cnts.iter().sum::<usize>() > cnt_total {
                        let q = (0..na).max_by_key(|&q| cnts[q]).unwrap();
                        cnts[q] -= 1;
                    }
                    while cnts.iter().sum::<usize>() < cnt_total {
                        let q = (0..na).max_by(|&x, &y| (lens[x] / cnts[x] as f32).total_cmp(&(lens[y] / cnts[y] as f32))).unwrap();
                        cnts[q] += 1;
                    }
                    let mut ids: Vec<u32> = Vec::new();
                    let mut amap: HashMap<usize, usize> = HashMap::default();
                    for q in 0..na {
                        amap.insert(a[q], ids.len());
                        let (s0, l) = (cum[a[q]], lens[q]);
                        for t in 0..cnts[q] {
                            let goal = (s0 + l * t as f32 / cnts[q] as f32) % total;
                            let idx = cum.partition_point(|&x| x <= goal).saturating_sub(1).min(m - 1);
                            let u = ((goal - cum[idx]) / (cum[idx + 1] - cum[idx]).max(1e-12)).clamp(0.0, 1.0);
                            out_p.push(ring.pts[idx].lerp(ring.pts[(idx + 1) % m], u));
                            ids.push((out_p.len() - 1) as u32);
                        }
                    }
                    rows.push(ids);
                    att_row.push(amap);
                }
                // Strip points between their ends (both ends are row points).
                let mut strip_pts: Vec<Vec<u32>> = Vec::new();
                for s in &sp.strips {
                    let path = &s.4;
                    let len: f32 = path.windows(2).map(|e| e[0].distance(e[1])).sum();
                    let m = s.5;
                    let mut cum = vec![0.0f32];
                    for e in path.windows(2) {
                        cum.push(cum.last().unwrap() + e[0].distance(e[1]));
                    }
                    let mut ids = Vec::new();
                    for t in 1..m {
                        let goal = len * t as f32 / m as f32;
                        let idx = cum.partition_point(|&x| x <= goal).saturating_sub(1).min(path.len() - 2);
                        let u = ((goal - cum[idx]) / (cum[idx + 1] - cum[idx]).max(1e-12)).clamp(0.0, 1.0);
                        out_p.push(path[idx].lerp(path[idx + 1], u));
                        ids.push((out_p.len() - 1) as u32);
                    }
                    strip_pts.push(ids);
                }
                // The loop the ring above is bridged into: round each ring, down each strip
                // and back (an Euler tour of the tree from ring 0).
                let mut seq: Vec<u32> = Vec::new();
                tour(0, 0, None, &rows, &att_row, &sp.strips, &strip_pts, &mut seq, true);
                if debug {
                    eprintln!("    body split near {:.3}: ring {} round into {:?} (strips {:?}), bridged loop {}", w.p[p as usize], row.len(), sp.counts, strip_pts.iter().map(|s| s.len() + 1).collect::<Vec<_>>(), seq.len());
                }
                // The ring above against the bridged loop, turned to lie best against it.
                let seq_pos: Vec<Vec3> = seq.iter().map(|&i| out_p[i as usize]).collect();
                let against = resample_loop(&seq_pos, row.len());
                let n = row.len();
                let sh = (0..n).min_by(|&a, &b| {
                    let ca: f32 = (0..n).map(|i| against[i].distance_squared(row_pos[(i + a) % n])).sum();
                    let cb: f32 = (0..n).map(|i| against[i].distance_squared(row_pos[(i + b) % n])).sum();
                    ca.total_cmp(&cb)
                }).unwrap_or(0);
                // With a plane and a pinned ring above: the loop started at its plane point in
                // front (the one whose next point is on the same side as the ring's), and if
                // it is its own mirror image, half joined and mirrored.
                let mut done = false;
                if let (Some(pl), true) = (plane, row_sym) {
                    let c = seq.len();
                    let pos = |i: usize| out_p[seq[i % c] as usize];
                    let near_plane = |q: Vec3| pl.distance(q).abs() < quad * 0.05;
                    let side1 = pl.distance(row_pos[1]) >= 0.0;
                    let start = (0..c).filter(|&k| near_plane(pos(k)) && (pl.distance(pos(k + 1)) >= 0.0) == side1).min_by(|&a, &b| pos(a).distance(row_pos[0]).total_cmp(&pos(b).distance(row_pos[0])));
                    if let Some(k) = start {
                        let rot: Vec<u32> = (0..c).map(|i| seq[(i + k) % c]).collect();
                        let rpos = |i: usize| out_p[rot[i % c] as usize];
                        let mirrored = c % 2 == 0 && near_plane(rpos(c / 2)) && (1..c).all(|i| pl.reflect(rpos(i)).distance(rpos(c - i)) < quad * 0.5);
                        if mirrored {
                            for &v in &rot {
                                if near_plane(out_p[v as usize]) {
                                    out_p[v as usize] = pl.project(out_p[v as usize]);
                                }
                            }
                            done = join_sym(&row, &rot, &mut out_p, &mut quads, pl);
                        }
                        if debug && !done {
                            eprintln!("    body split: bridged loop not its own mirror image ({c} points)");
                        }
                    }
                }
                if !done {
                    let turned: Vec<u32> = (0..n).map(|i| row[(i + sh) % n]).collect();
                    join_band(&turned, &seq, &mut out_p, &mut quads);
                }
                for i in 0..nr {
                    if rows_sym[i] {
                        torso.extend(rows[i].iter().copied());
                    }
                    let run = chain_from(sp.rings[i]);
                    for (k, c) in plan(&run).into_iter().enumerate() {
                        counts.insert(run[k], c);
                    }
                    stack.push((rings[sp.rings[i]].to, rows[i].clone(), rows_sym[i], sp.rings[i]));
                }
            }
        }
    }
    // Rows still waiting where rings join (the other never came): part of the body unbuilt.
    if let Some(p) = pending.keys().next() {
        return Err(format!("rings join near {:.3} but not all reach it", w.p[*p as usize]));
    }
    // And the whole surface covered (thin parts left off aside): else the planned layout.
    {
        let area_q: f32 = quads.iter().map(|q| {
            let (a, b, c, d) = (out_p[q[0] as usize], out_p[q[1] as usize], out_p[q[2] as usize], out_p[q[3] as usize]);
            ((b - a).cross(c - a).length() + (c - a).cross(d - a).length()) * 0.5
        }).sum();
        let area_w: f32 = w.t.iter().map(|t| (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5).sum();
        if area_q < area_w * 0.85 {
            return Err(format!("the rings cover {:.0}% of the surface", area_q / area_w.max(1e-12) * 100.0));
        }
    }
    if debug {
        eprintln!("  body: {} quads, {} splits, {} joins, {} caps, {} rings", quads.len(), splits, joins, caps, rings.len());
    }
    // Breasts (`ZR_NOBUMPS` off): on each run of mirrored rows of one count (the trunk), a bump
    // standing out from the grid smoothed is cut out as a rectangle of quads and capped like a
    // sphere: rings in from the rectangle's border, a grid at its middle; the other side the
    // mirror image.
    orient(&mut quads, &out_p);
    if let (Some(pl), None) = (plane, std::env::var_os("ZR_NOBUMPS")) {
        let made = bumps(&torso, &mut out_p, &mut quads, pl, quad, debug, &cap, 1.0, ("ZR_BUMP", 1.5, 0.4, 8.0, 0.6, 1.0));
        if debug {
            eprintln!("  body: {made} bumps capped");
        }
        if made > 0 {
            orient(&mut quads, &out_p);
        }
        // Eyes (`ZR_NOEYES` off): the sockets, the deepest dips on the front of the head,
        // ringed the same way.
        if std::env::var_os("ZR_NOEYES").is_none() {
            let eyes = bumps(&head, &mut out_p, &mut quads, pl, quad, debug, &cap, -1.0, ("ZR_EYE", 0.4, 0.5, 4.0, 0.5, 1.0));
            if debug {
                eprintln!("  body: {eyes} eye sockets ringed");
            }
            if eyes > 0 {
                orient(&mut quads, &out_p);
            }
        }
        // The mouth (`ZR_NOMOUTH` off): one ring across the centre line.
        if std::env::var_os("ZR_NOMOUTH").is_none() && mouth(&lower_face, &mut out_p, &mut quads, pl, quad, debug, &cap) {
            orient(&mut quads, &out_p);
        }
    }
    if let Some(pl) = plane {
        let lonely = symmetrize(&mut out_p, pl, quad);
        if debug {
            eprintln!("  body: mirrored, {lonely} of {} points without a mirror image", out_p.len());
        }
    }
    Ok((out_p, quads))
}

/// Faces turned to agree with their neighbours across shared edges, then all outward (the
/// enclosed volume positive).
fn orient(quads: &mut [[u32; 4]], p: &[Vec3]) {
    let edges = |q: &[u32; 4]| -> Vec<(u32, u32)> {
        let mut e = Vec::new();
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            if a != b {
                e.push((a, b));
            }
        }
        e
    };
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::default();
    for (f, q) in quads.iter().enumerate() {
        for (a, b) in edges(q) {
            by_edge.entry((a.min(b), a.max(b))).or_default().push(f);
        }
    }
    let flip = |q: &mut [u32; 4]| *q = [q[1], q[0], q[3], q[2]];
    let mut seen = vec![false; quads.len()];
    for s in 0..quads.len() {
        if seen[s] {
            continue;
        }
        seen[s] = true;
        let mut queue = VecDeque::from([s]);
        while let Some(f) = queue.pop_front() {
            for (a, b) in edges(&quads[f]) {
                for &g in &by_edge[&(a.min(b), a.max(b))] {
                    if g == f || seen[g] {
                        continue;
                    }
                    // Agreeing faces run a shared edge in opposite directions.
                    if edges(&quads[g]).contains(&(a, b)) {
                        let mut q = quads[g];
                        flip(&mut q);
                        quads[g] = q;
                    }
                    seen[g] = true;
                    queue.push_back(g);
                }
            }
        }
    }
    let vol: f32 = quads.iter().map(|q| {
        let (a, b, c, d) = (p[q[0] as usize], p[q[1] as usize], p[q[2] as usize], p[q[3] as usize]);
        a.dot(b.cross(c)) + a.dot(c.cross(d))
    }).sum();
    if vol < 0.0 {
        for q in quads.iter_mut() {
            flip(q);
        }
    }
}

/// Bumps on the front of the torso (breasts), capped like spheres bridged into it. Each
/// point's height over the result smoothed many times; the highest torso point on the front
/// of the + side standing over `ZR_BUMP` (1.5) quads out is a bump's tip, the torso points
/// round it over `ZR_BUMPFRAC` (0.4) of that its region. The quads within its reach (a disc)
/// are cut out and the hole filled with rings in toward the tip and a grid; the other side
/// the mirror image. Returns how many were capped.
/// `sign` −1 finds dips instead (eye sockets); `set` is the env prefix and defaults: (prefix,
/// least height in quads, region fraction, most reach in quads, least forward normal, pairs).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn bumps(torso: &HashSet<u32>, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, plane: MirrorPlane, quad: f32, debug: bool, cap: &dyn Fn(&[u32], bool, &mut Vec<Vec3>, &mut Vec<[u32; 4]>), sign: f32, set: (&str, f32, f32, f32, f32, f32)) -> usize {
    let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
    let pre = set.0;
    let (min_h, frac, max_r) = (env(pre, set.1), env(&format!("{pre}FRAC"), set.2), env(&format!("{pre}MAXR"), set.3));
    let np = out_p.len();
    let pos0: Vec<Vec3> = out_p.clone();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); np];
    let mut nrm = vec![Vec3::ZERO; np];
    for q in quads.iter() {
        let n = (out_p[q[2] as usize] - out_p[q[0] as usize]).cross(out_p[q[3] as usize] - out_p[q[1] as usize]);
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            if a != b {
                if !nb[a as usize].contains(&b) {
                    nb[a as usize].push(b);
                }
                if !nb[b as usize].contains(&a) {
                    nb[b as usize].push(a);
                }
            }
            nrm[q[k] as usize] += n;
        }
    }
    let mut sm = out_p.clone();
    for _ in 0..80 {
        let prev = sm.clone();
        for v in 0..np {
            if !nb[v].is_empty() {
                let avg = nb[v].iter().map(|&u| prev[u as usize]).sum::<Vec3>() / nb[v].len() as f32;
                sm[v] = prev[v] * 0.5 + avg * 0.5;
            }
        }
    }
    let h0: Vec<f32> = (0..np).map(|v| sign * (out_p[v] - sm[v]).dot(nrm[v].normalize_or_zero()) / quad).collect();
    // Dips against the height round them (smoothing pulls a whole convex head inward, so every
    // point on it stands out; what matters is sinking below the neighbourhood). Bumps stand
    // clear of the smoothed surface itself.
    let mut avg = h0.clone();
    for _ in 0..30 {
        let prev = avg.clone();
        for v in 0..np {
            if !nb[v].is_empty() {
                avg[v] = prev[v] * 0.5 + nb[v].iter().map(|&u| prev[u as usize]).sum::<f32>() / nb[v].len() as f32 * 0.5;
            }
        }
    }
    let h: Vec<f32> = (0..np).map(|v| if sign < 0.0 { h0[v] - avg[v] } else { h0[v] }).collect();
    let fwd = if plane.axis == Axis::Z { 0 } else { 2 };
    let front = |v: usize| nrm[v].normalize_or_zero()[fwd] > env(&format!("{pre}FRONT"), set.4);
    let plus = |v: usize| v < np && plane.distance(pos0[v]) > quad * 0.5;
    let mut made = 0;
    let mut used: HashSet<u32> = HashSet::default();
    if debug {
        let best = (0..np).filter(|&v| torso.contains(&(v as u32)) && plus(v)).max_by(|&a, &b| h[a].total_cmp(&h[b]));
        let best_front = (0..np).filter(|&v| torso.contains(&(v as u32)) && plus(v) && front(v)).max_by(|&a, &b| h[a].total_cmp(&h[b]));
        eprintln!("    {pre}: {} points looked at; most {:?}, most facing front {:?}", torso.len(), best.map(|v| (out_p[v], h[v])), best_front.map(|v| (out_p[v], h[v])));
    }
    for _ in 0..env(&format!("{pre}S"), set.5) as usize {
        let Some(tip) = (0..np).filter(|&v| torso.contains(&(v as u32)) && plus(v) && front(v) && h[v] > min_h && !used.contains(&(v as u32))).max_by(|&a, &b| h[a].total_cmp(&h[b])) else { break };
        let hp = h[tip];
        let mut region: HashSet<u32> = [tip as u32].into_iter().collect();
        let mut stack = vec![tip as u32];
        while let Some(v) = stack.pop() {
            for &u in &nb[v as usize] {
                if torso.contains(&u) && plus(u as usize) && h[u as usize] > hp * frac && region.insert(u) {
                    stack.push(u);
                }
            }
        }
        used.extend(region.iter().copied());
        let c = pos0[tip];
        let mut r = (region.iter().map(|&v| pos0[v as usize].distance(c)).fold(0.0f32, f32::max) + quad * 0.5).min(quad * max_r);
        // The quads within reach, shrunk until they are one disc (a single border loop).
        let mut found: Option<(Vec<usize>, Vec<u32>)> = None;
        while r >= quad * 2.0 {
            let inside = |q: &[u32; 4], cc: Vec3| q.iter().all(|&v| (v as usize) < np && pos0[v as usize].distance(cc) <= r);
            let qs: Vec<usize> = (0..quads.len()).filter(|&f| inside(&quads[f], c) && quads[f].iter().all(|&v| plus(v as usize))).collect();
            if let Some(border) = disc_border(quads, &qs) {
                found = Some((qs, border));
                break;
            }
            r -= quad * 0.5;
        }
        let Some((qs, border)) = found else {
            if debug {
                eprintln!("    bump at {c:.3} ({hp:.1} quads out): no disc of quads round it");
            }
            continue;
        };
        // The mirror image's quads and border, point for point.
        let mc = plane.reflect(c);
        let qs2: Vec<usize> = (0..quads.len()).filter(|&f| quads[f].iter().all(|&v| (v as usize) < np && pos0[v as usize].distance(mc) <= r && plane.distance(pos0[v as usize]) < -quad * 0.5)).collect();
        let Some(border2) = disc_border(quads, &qs2) else { continue };
        if border2.len() != border.len() || qs2.len() != qs.len() {
            if debug {
                eprintln!("    bump at {c:.3}: its mirror image does not match ({} / {} quads)", qs.len(), qs2.len());
            }
            continue;
        }
        let nbd = border.len();
        let img = |v: u32| plane.reflect(pos0[v as usize]);
        let start = (0..nbd).min_by(|&a, &b| pos0[border2[a] as usize].distance(img(border[0])).total_cmp(&pos0[border2[b] as usize].distance(img(border[0])))).unwrap();
        let fwd_ok = pos0[border2[(start + 1) % nbd] as usize].distance(img(border[1])) < pos0[border2[(start + nbd - 1) % nbd] as usize].distance(img(border[1]));
        let b2: Vec<u32> = (0..nbd).map(|m| if fwd_ok { border2[(start + m) % nbd] } else { border2[(start + nbd - m) % nbd] }).collect();
        if (0..nbd).any(|m| pos0[b2[m] as usize].distance(img(border[m])) > quad * 0.5) {
            if debug {
                eprintln!("    bump at {c:.3}: its mirror image's border does not line up");
            }
            continue;
        }
        let mut gone: Vec<usize> = qs.iter().chain(qs2.iter()).copied().collect();
        gone.sort_unstable();
        for &f in gone.iter().rev() {
            quads.swap_remove(f);
        }
        // Rings in toward the tip with the border's count (radial lines run in to the tip),
        // then one ring near it with a third as many (the merges round the tip), then a grid:
        // the right side built, the left its mirror image quad for quad.
        let rings_n = ((r / quad) * 0.5).round().clamp(1.0, 4.0) as usize;
        if debug {
            eprintln!("    bump at {c:.3}, {hp:.1} quads out: {} quads cut ({:.1} quads round), border {nbd}, {rings_n} rings and a grid", qs.len(), r / quad);
        }
        let (p_start, q_start) = (out_p.len(), quads.len());
        let mut prev = border.clone();
        for j in 1..=rings_n + 1 {
            let t = if j <= rings_n { 0.75 * j as f32 / rings_n as f32 } else { 0.88 };
            let pts: Vec<Vec3> = (0..nbd).map(|m| pos0[border[m] as usize].lerp(c, t)).collect();
            let cnt = if j <= rings_n { nbd } else { (nbd.div_ceil(3).div_ceil(4) * 4).max(8).min(nbd) };
            let ring: Vec<u32> = resample_loop(&pts, cnt).into_iter().map(|q| { out_p.push(q); (out_p.len() - 1) as u32 }).collect();
            join_band(&prev, &ring, out_p, quads);
            prev = ring;
        }
        cap(&prev, false, out_p, quads);
        let mut mir: HashMap<u32, u32> = HashMap::default();
        for m in 0..nbd {
            mir.insert(border[m], b2[m]);
        }
        for v in p_start..out_p.len() {
            let q = plane.reflect(out_p[v]);
            out_p.push(q);
            mir.insert(v as u32, (out_p.len() - 1) as u32);
        }
        for f in q_start..quads.len() {
            let m = quads[f].map(|v| mir[&v]);
            quads.push([m[1], m[0], m[3], m[2]]);
        }
        made += 2;
    }
    made
}

/// The border of the quads `qs` as one loop of points, if they make a disc (every border
/// point on two border edges, one loop through all of them, V − E + F = 1).
fn disc_border(quads: &[[u32; 4]], qs: &[usize]) -> Option<Vec<u32>> {
    if qs.is_empty() {
        return None;
    }
    let mut count: HashMap<(u32, u32), u32> = HashMap::default();
    let mut dir: HashMap<(u32, u32), (u32, u32)> = HashMap::default();
    let mut verts: HashSet<u32> = HashSet::default();
    for &f in qs {
        let q = quads[f];
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            if a == b {
                return None;
            }
            verts.insert(a);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
            dir.insert((a.min(b), a.max(b)), (a, b));
        }
    }
    let border: Vec<(u32, u32)> = count.iter().filter(|e| *e.1 == 1).map(|e| dir[e.0]).collect();
    if border.is_empty() {
        return None;
    }
    let mut next: HashMap<u32, u32> = HashMap::default();
    for &(a, b) in &border {
        if next.insert(a, b).is_some() {
            return None;
        }
    }
    let start = border[0].0;
    let mut lp = vec![start];
    let mut v = next[&start];
    while v != start {
        if lp.len() > border.len() {
            return None;
        }
        lp.push(v);
        v = *next.get(&v)?;
    }
    if lp.len() != border.len() {
        return None;
    }
    let e = count.len() as i64;
    if verts.len() as i64 - e + qs.len() as i64 != 1 {
        return None;
    }
    Some(lp)
}

/// The mouth: on the centre line of the lower face, facing front, the first point below the
/// nose tip that stands out less than its neighbours above and below. The quads within
/// `ZR_MOUTHR` (2.5) quads of it (a disc across the plane, its own mirror image) are cut out
/// and the hole filled with rings toward it pinned to the plane, then a grid with a line
/// along the plane. Whether one was made.
#[allow(clippy::type_complexity)]
fn mouth(face: &HashSet<u32>, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, plane: MirrorPlane, quad: f32, debug: bool, cap: &dyn Fn(&[u32], bool, &mut Vec<Vec3>, &mut Vec<[u32; 4]>)) -> bool {
    let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
    let np = out_p.len();
    let pos0 = out_p.clone();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); np];
    let mut nrm = vec![Vec3::ZERO; np];
    for q in quads.iter() {
        let n = (pos0[q[2] as usize] - pos0[q[0] as usize]).cross(pos0[q[3] as usize] - pos0[q[1] as usize]);
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            if a != b {
                if !nb[a as usize].contains(&b) {
                    nb[a as usize].push(b);
                }
                if !nb[b as usize].contains(&a) {
                    nb[b as usize].push(a);
                }
            }
            nrm[q[k] as usize] += n;
        }
    }
    let smooth = |x: &[Vec3], n: usize| -> Vec<Vec3> {
        let mut s = x.to_vec();
        for _ in 0..n {
            let prev = s.clone();
            for v in 0..np {
                if !nb[v].is_empty() {
                    s[v] = prev[v] * 0.5 + nb[v].iter().map(|&u| prev[u as usize]).sum::<Vec3>() / nb[v].len() as f32 * 0.5;
                }
            }
        }
        s
    };
    let sm = smooth(&pos0, 80);
    let h0: Vec<f32> = (0..np).map(|v| -(pos0[v] - sm[v]).dot(nrm[v].normalize_or_zero()) / quad).collect();
    let mut avg = h0.clone();
    for _ in 0..30 {
        let prev = avg.clone();
        for v in 0..np {
            if !nb[v].is_empty() {
                avg[v] = prev[v] * 0.5 + nb[v].iter().map(|&u| prev[u as usize]).sum::<f32>() / nb[v].len() as f32 * 0.5;
            }
        }
    }
    let h: Vec<f32> = (0..np).map(|v| h0[v] - avg[v]).collect();
    let fwd = if plane.axis == Axis::Z { 0 } else { 2 };
    let on_plane = |v: usize| plane.distance(pos0[v]).abs() < quad * 0.05;
    let front = |v: usize| nrm[v].normalize_or_zero()[fwd] > 0.5;
    // Down the centre line of the face: the nose tip is the point standing out most; the mouth
    // the first point below it standing out less than its neighbours above and below (at this
    // density closed lips are no dip, only a pause between nose and chin).
    let mut line: Vec<usize> = (0..np).filter(|&v| face.contains(&(v as u32)) && on_plane(v) && front(v)).collect();
    line.sort_by(|&a, &b| pos0[b].y.total_cmp(&pos0[a].y));
    if debug {
        eprintln!("    mouth: {} face points, the centre line down the front (y, dip): {:?}", face.len(), line.iter().map(|&v| ((pos0[v].y * 1000.0).round() / 1000.0, (h[v] * 100.0).round() / 100.0)).collect::<Vec<_>>());
    }
    let Some(nose) = (0..line.len()).min_by(|&a, &b| h[line[a]].total_cmp(&h[line[b]])) else { return false };
    // (The first such pause below the nose: the next one down is the crease above the chin.)
    let pause = (nose + 1..line.len().saturating_sub(1)).find(|&i| h[line[i]] > h[line[i - 1]] && h[line[i]] >= h[line[i + 1]]);
    let Some(pause) = pause else { return false };
    {
        let tip = line[pause];
        let hp = h[tip];
        let c = pos0[tip];
        let mut r = quad * env("ZR_MOUTHR", 2.5);
        while r >= quad * 1.5 {
            let qs: Vec<usize> = (0..quads.len()).filter(|&f| quads[f].iter().all(|&v| (v as usize) < np && pos0[v as usize].distance(c) <= r)).collect();
            if let Some(border) = disc_border(quads, &qs) {
                let n = border.len();
                // Turned to start at its upper point on the plane; its own mirror image?
                let ups: Vec<usize> = (0..n).filter(|&m| on_plane(border[m] as usize)).collect();
                if ups.len() == 2 {
                    let st = if pos0[border[ups[0]] as usize].y >= pos0[border[ups[1]] as usize].y { ups[0] } else { ups[1] };
                    let bd: Vec<u32> = (0..n).map(|m| border[(st + m) % n]).collect();
                    let mirrored = n % 2 == 0 && on_plane(bd[n / 2] as usize) && (1..n).all(|m| plane.reflect(pos0[bd[m] as usize]).distance(pos0[bd[n - m] as usize]) < quad * 0.3);
                    if mirrored {
                        let mut gone = qs.clone();
                        gone.sort_unstable();
                        for &f in gone.iter().rev() {
                            quads.swap_remove(f);
                        }
                        let rings_n = ((r / quad) * 0.5).round().clamp(1.0, 3.0) as usize;
                        let mut prev = bd.clone();
                        for j in 1..=rings_n {
                            let t = 0.8 * j as f32 / (rings_n as f32 + 0.5);
                            let mut pts: Vec<Vec3> = (0..n).map(|m| pos0[bd[m] as usize].lerp(c, t)).collect();
                            pts[0] = plane.project(pts[0]);
                            pts[n / 2] = plane.project(pts[n / 2]);
                            for m in 1..n / 2 {
                                pts[n - m] = plane.reflect(pts[m]);
                            }
                            let ring: Vec<u32> = pts.into_iter().map(|q| { out_p.push(q); (out_p.len() - 1) as u32 }).collect();
                            for m in 0..n {
                                quads.push([prev[m], prev[(m + 1) % n], ring[(m + 1) % n], ring[m]]);
                            }
                            prev = ring;
                        }
                        cap(&prev, true, out_p, quads);
                        if debug {
                            eprintln!("    mouth at {c:.3} ({hp:.2}): {} quads cut ({:.1} quads round), border {n}, {rings_n} rings and a grid", qs.len(), r / quad);
                        }
                        return true;
                    }
                }
            }
            r -= quad * 0.5;
        }
    }
    false
}

/// The loop round ring `i` from row point `start`, and down every strip off it to the ring at
/// its other end and back (an Euler tour of the tree of rings and strips): the border a band
/// is joined to where a ring splits or rings join.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn tour(i: usize, start: usize, parent: Option<usize>, rows: &[Vec<u32>], att_row: &[HashMap<usize, usize>], strips: &[(usize, usize, usize, usize, Vec<Vec3>, usize)], strip_pts: &[Vec<u32>], seq: &mut Vec<u32>, root: bool) {
    let n = rows[i].len();
    for r in 0..n {
        let idx = (start + r) % n;
        seq.push(rows[i][idx]);
        for (e, s) in strips.iter().enumerate() {
            if Some(e) == parent {
                continue;
            }
            let (here, there, there_c) = if s.0 == i && att_row[i].get(&s.1) == Some(&idx) {
                (true, s.2, s.3)
            } else if s.2 == i && att_row[i].get(&s.3) == Some(&idx) {
                (false, s.0, s.1)
            } else {
                continue;
            };
            let pts: Vec<u32> = if here { strip_pts[e].clone() } else { strip_pts[e].iter().rev().copied().collect() };
            seq.extend(&pts);
            let st = att_row[there][&there_c];
            tour(there, st, Some(e), rows, att_row, strips, strip_pts, seq, false);
            seq.extend(pts.iter().rev());
            seq.push(rows[i][idx]);
        }
    }
    if !root {
        seq.push(rows[i][start]);
    }
}

#[cfg(test)]
mod reduce_tests {
    use super::*;

    /// Each vertex's valence in the band with the edge it has into the row before or after
    /// (one per row point) counted in, and a check that the band is a manifold strip of
    /// quads: row edges used once, inner edges twice, four distinct corners, no corner
    /// angle near 0 or 180 degrees.
    fn valences(p: &[Vec3], quads: &[[u32; 4]], rows: &[&[u32]]) -> HashMap<u32, usize> {
        let mut used: HashMap<(u32, u32), usize> = HashMap::default();
        for q in quads {
            let mut c = q.to_vec();
            c.sort_unstable();
            c.dedup();
            assert_eq!(c.len(), 4, "quad {q:?} repeats a corner");
            for k in 0..4 {
                let (a, b, o) = (p[q[k] as usize], p[q[(k + 1) % 4] as usize], p[q[(k + 3) % 4] as usize]);
                let ang = (b - a).angle_between(o - a).to_degrees();
                assert!(ang > 10.0 && ang < 170.0, "quad {q:?} has a corner of {ang:.1} degrees");
                *used.entry((q[k].min(q[(k + 1) % 4]), q[k].max(q[(k + 1) % 4]))).or_default() += 1;
            }
        }
        let mut row_edges: HashSet<(u32, u32)> = HashSet::default();
        for r in rows {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                row_edges.insert((a.min(b), a.max(b)));
            }
        }
        let mut val: HashMap<u32, usize> = HashMap::default();
        for (&(a, b), &n) in &used {
            assert_eq!(n, if row_edges.contains(&(a, b)) { 1 } else { 2 }, "edge {a}-{b} used {n} times");
            *val.entry(a).or_default() += 1;
            *val.entry(b).or_default() += 1;
        }
        for r in rows {
            for &v in r.iter() {
                assert!(used.keys().any(|&(a, b)| a == v || b == v), "row point {v} is not in the band");
                *val.entry(v).or_default() += 1;
            }
        }
        val
    }

    fn circle(n: usize, y: f32, p: &mut Vec<Vec3>) -> Vec<u32> {
        let r = n as f32 / std::f32::consts::TAU;
        (0..n).map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            p.push(Vec3::new(-r * a.sin(), y, r * a.cos()));
            (p.len() - 1) as u32
        }).collect()
    }

    #[test]
    fn diamonds_take_a_closed_band_from_24_to_20() {
        for flip in [false, true] {
            let mut p = Vec::new();
            let (lo, hi) = if flip { (circle(20, 0.0, &mut p), circle(24, 1.0, &mut p)) } else { (circle(24, 0.0, &mut p), circle(20, 1.0, &mut p)) };
            let long = if flip { &hi } else { &lo };
            let long_pos: Vec<Vec3> = long.iter().map(|&v| p[v as usize]).collect();
            let cols = reduce_cols(&long_pos, 2, Pref::Back, None, 2, false).unwrap();
            // The back of the circle is index 12; an even count straddles it.
            assert_eq!(cols, vec![10, 14]);
            let mut quads = Vec::new();
            assert!(reduce_band(&lo, &hi, &cols, &mut p, &mut quads));
            assert_eq!(quads.len(), 22);
            let val = valences(&p, &quads, &[&lo, &hi]);
            let threes: Vec<u32> = val.iter().filter(|x| *x.1 == 3).map(|x| *x.0).collect();
            let fives: Vec<u32> = val.iter().filter(|x| *x.1 == 5).map(|x| *x.0).collect();
            assert_eq!(threes.len(), 2, "flip {flip}: valence 3 at {threes:?}");
            assert_eq!(fives.len(), 2, "flip {flip}: valence 5 at {fives:?}");
            assert!(val.values().all(|&v| (3..=5).contains(&v)));
            for &c in &cols {
                assert!(threes.contains(&long[c]), "flip {flip}: column {c} is not a valence-3 point");
            }
            let short = if flip { &lo } else { &hi };
            assert!(fives.iter().all(|v| short.contains(v)), "the valence-5 points are on the shorter row");
            // The kite points moved away from the other row, so the kites are convex.
            for &c in &cols {
                assert!((p[long[c] as usize].y - if flip { 1.0 } else { 0.0 }).abs() > 0.3);
            }
        }
    }

    #[test]
    fn mirrored_diamonds_take_a_pinned_band_from_28_to_24() {
        let plane = MirrorPlane { axis: Axis::X, at: 0.0 };
        let mut p = Vec::new();
        let lo = circle(28, 0.0, &mut p);
        let hi = circle(24, 1.0, &mut p);
        let lo_pos: Vec<Vec3> = lo.iter().map(|&v| p[v as usize]).collect();
        assert!(plane.distance(lo_pos[0]).abs() < 1e-5 && plane.distance(lo_pos[14]).abs() < 1e-5);
        assert_eq!(reduce_cols(&lo_pos, 1, Pref::Back, Some(plane), 2, true), Some(vec![12]));
        assert_eq!(reduce_cols(&lo_pos, 3, Pref::Back, Some(plane), 2, true), Some(vec![6, 9, 12]));
        assert_eq!(reduce_cols(&lo_pos, 1, Pref::Sole, Some(plane), 2, true), Some(vec![2]));
        let cols = reduce_cols(&lo_pos, 1, Pref::Back, Some(plane), 2, true).unwrap();
        let mut quads = Vec::new();
        assert!(reduce_sym(&lo, &hi, &cols, plane, &mut p, &mut quads));
        assert_eq!(quads.len(), 26);
        let val = valences(&p, &quads, &[&lo, &hi]);
        let mut threes: Vec<u32> = val.iter().filter(|x| *x.1 == 3).map(|x| *x.0).collect();
        threes.sort_unstable();
        assert_eq!(threes, vec![lo[12], lo[16]]);
        assert_eq!(val.values().filter(|&&v| v == 5).count(), 2);
        // Its own mirror image: every quad's reflection is a quad, and no point is lonely.
        for q in &quads {
            let want: Vec<Vec3> = q.iter().map(|&v| plane.reflect(p[v as usize])).collect();
            assert!(quads.iter().any(|r| (0..4).all(|k| r.iter().any(|&v| p[v as usize].distance(want[k]) < 1e-5))), "quad {q:?} has no mirror image");
        }
        assert_eq!(symmetrize(&mut p, plane, 1.0), 0);
        // A difference not in fours is not for diamonds.
        let mut p2 = Vec::new();
        let (a, b) = (circle(28, 0.0, &mut p2), circle(26, 1.0, &mut p2));
        assert!(!reduce_sym(&a, &b, &[], plane, &mut p2, &mut Vec::new()));
    }
}
