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
    /// The level it was cut at (the farther piece's band).
    level: i64,
    /// On a hard loop of the working mesh (a crease): its row lies on the crease.
    hard: bool,
}

/// The rings' spacing along the part function: a quad (`ZR_BODYSTEP` times it).
pub(super) fn body_step(quad: f32) -> f32 {
    quad * std::env::var("ZR_BODYSTEP").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0)
}

/// Segments of lines by the cells their boxes cover, for the nearest point on a line.
struct SegGrid {
    cell: f32,
    segs: Vec<(Vec3, Vec3, u32)>,
    grid: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl SegGrid {
    fn new(cell: f32) -> Self {
        Self { cell: cell.max(1e-6), segs: Vec::new(), grid: HashMap::default() }
    }
    fn key(&self, q: Vec3) -> (i32, i32, i32) {
        let c = (q / self.cell).floor();
        (c.x as i32, c.y as i32, c.z as i32)
    }
    /// A segment of line `id`.
    fn add(&mut self, a: Vec3, b: Vec3, id: u32) {
        let s = self.segs.len() as u32;
        self.segs.push((a, b, id));
        let (ka, kb) = (self.key(a), self.key(b));
        for x in ka.0.min(kb.0)..=ka.0.max(kb.0) {
            for y in ka.1.min(kb.1)..=ka.1.max(kb.1) {
                for z in ka.2.min(kb.2)..=ka.2.max(kb.2) {
                    self.grid.entry((x, y, z)).or_default().push(s);
                }
            }
        }
    }
    /// A closed line's segments.
    fn add_loop(&mut self, pts: &[Vec3], id: u32) {
        for i in 0..pts.len() {
            self.add(pts[i], pts[(i + 1) % pts.len()], id);
        }
    }
    fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
    /// The nearest point on a line within `reach` of `q`: its distance, the point, the line.
    fn nearest(&self, q: Vec3, reach: f32) -> Option<(f32, Vec3, u32)> {
        let r = (reach / self.cell).ceil() as i32;
        let k = self.key(q);
        let mut best: Option<(f32, Vec3, u32)> = None;
        for dx in -r..=r {
            for dy in -r..=r {
                for dz in -r..=r {
                    for &s in self.grid.get(&(k.0 + dx, k.1 + dy, k.2 + dz)).into_iter().flatten() {
                        let (a, b, id) = self.segs[s as usize];
                        let ab = b - a;
                        let t = ((q - a).dot(ab) / ab.length_squared().max(1e-24)).clamp(0.0, 1.0);
                        let p = a + ab * t;
                        let d = p.distance(q);
                        if d <= reach && best.is_none_or(|x| d < x.0) {
                            best = Some((d, p, id));
                        }
                    }
                }
            }
        }
        best
    }
}

/// What a closed hard line of the working mesh is to the body.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum LoopKind {
    /// Round a part, the way the level rings run (a collar, a cuff): it becomes a ring.
    Transverse,
    /// The outline of a plate, or a loop that cuts nothing off.
    Plate,
}

/// A closed hard line of the working mesh.
pub(super) struct HardLoop {
    /// Its vertices round once.
    pub(super) verts: Vec<u32>,
    pub(super) kind: LoopKind,
    /// A plate's faces: the smaller side of the surface the loop cuts off (empty when it cuts
    /// nothing off).
    pub(super) inside: Vec<usize>,
}

/// The hard edges of the working mesh sorted for the body builder: closed loops, open lines
/// (as vertex chains), and the corners of the edge lines. Empty: the body built by the part
/// function alone.
#[derive(Default)]
pub(super) struct BodyFeatures {
    pub(super) loops: Vec<HardLoop>,
    pub(super) lines: Vec<Vec<u32>>,
    pub(super) corners: Vec<Vec3>,
    /// Detect Edges is on: creases are also looked for in the surface itself.
    pub(super) detect: bool,
}

impl BodyFeatures {
    /// From the hard edges of `w` (its open border is not one), `phi` the part function and
    /// `step` the rings' spacing. A closed chain whose span of `phi` is under 1.5 steps and
    /// that cuts the surface in two, the lower-phi side from the higher, is transverse; any
    /// other closed chain is a plate.
    pub(super) fn new(w: &Work, phi: &[f32], step: f32, corners: Vec<Vec3>, detect: bool) -> Self {
        let mut edges: Vec<(u32, u32)> = (0..w.hard.len() as u32)
            .filter(|&h| w.hard[h as usize] && w.twin[h as usize] != NONE)
            .map(|h| {
                let (a, b) = (w.from(h), w.to(h));
                (a.min(b), a.max(b))
            })
            .collect();
        edges.sort_unstable();
        edges.dedup();
        let (chains, _) = crate::crease::chains(&edges);
        let mut out = Self { loops: Vec::new(), lines: Vec::new(), corners, detect };
        let nf = w.t.len();
        for c in chains {
            if !(c.len() > 3 && c.first() == c.last()) {
                if c.len() >= 2 {
                    out.lines.push(c);
                }
                continue;
            }
            let verts: Vec<u32> = c[..c.len() - 1].to_vec();
            let n = verts.len();
            let (lo, hi) = verts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |m, &v| (m.0.min(phi[v as usize]), m.1.max(phi[v as usize])));
            // The faces either side: connected parts of the surface not crossing the loop.
            let on: HashSet<(u32, u32)> = (0..n).map(|i| { let (a, b) = (verts[i], verts[(i + 1) % n]); (a.min(b), a.max(b)) }).collect();
            let crosses = |f: usize, k: usize| { let (a, b) = (w.t[f][k], w.t[f][(k + 1) % 3]); on.contains(&(a.min(b), a.max(b))) };
            let mut comp = vec![u32::MAX; nf];
            let mut sizes: Vec<f32> = Vec::new();
            let mut sides: Vec<u32> = Vec::new();
            // Per side: loop edges whose third vertex has a lower phi, and a higher.
            let mut lower: Vec<usize> = Vec::new();
            let mut higher: Vec<usize> = Vec::new();
            for f0 in 0..nf {
                if comp[f0] != u32::MAX || !(0..3).any(|k| crosses(f0, k)) {
                    continue;
                }
                let id = sizes.len() as u32;
                let mut q = vec![f0];
                comp[f0] = id;
                let mut area = 0.0;
                while let Some(f) = q.pop() {
                    let t = w.t[f];
                    area += (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length();
                    for k in 0..3 {
                        let o = w.twin[f * 3 + k];
                        if o == NONE || crosses(f, k) {
                            continue;
                        }
                        let g = o as usize / 3;
                        if comp[g] == u32::MAX {
                            comp[g] = id;
                            q.push(g);
                        }
                    }
                }
                sizes.push(area);
                sides.push(id);
                lower.push(0);
                higher.push(0);
            }
            for f in 0..nf {
                let id = comp[f];
                if id == u32::MAX {
                    continue;
                }
                for k in 0..3 {
                    if crosses(f, k) {
                        let t = w.t[f];
                        let mid = (phi[t[k] as usize] + phi[t[(k + 1) % 3] as usize]) * 0.5;
                        let third = phi[t[(k + 2) % 3] as usize];
                        if third < mid {
                            lower[id as usize] += 1;
                        } else {
                            higher[id as usize] += 1;
                        }
                    }
                }
            }
            let separates = sides.len() >= 2;
            // Transverse: one side (nearly) all lower, the other (nearly) all higher.
            let mostly = |a: usize, b: usize| a * 5 >= (a + b) * 4;
            let across = separates && (mostly(lower[0], higher[0]) && mostly(higher[1], lower[1]) || mostly(higher[0], lower[0]) && mostly(lower[1], higher[1]));
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                let c = verts.iter().map(|&v| w.p[v as usize]).sum::<Vec3>() / n as f32;
                eprintln!("    hard loop near {c:.3}: {n} vertices, {:.2} steps of the part function, {} sides, lower/higher {:?}", (hi - lo) / step, sides.len(), lower.iter().zip(&higher).collect::<Vec<_>>());
            }
            if across && lo.is_finite() && hi - lo < 1.5 * step {
                out.loops.push(HardLoop { verts, kind: LoopKind::Transverse, inside: Vec::new() });
            } else {
                let inside = if separates {
                    let inner = *sides.iter().min_by(|&&a, &&b| sizes[a as usize].total_cmp(&sizes[b as usize])).unwrap();
                    (0..nf).filter(|&f| comp[f] == inner).collect()
                } else {
                    Vec::new()
                };
                out.loops.push(HardLoop { verts, kind: LoopKind::Plate, inside });
            }
        }
        out
    }
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

/// The shoulders as a yoke: the ring whose row the neck hole takes (`m` in the head run), the
/// trunk ring and the arm rings (+ side first) past the split, their counts, and the grid.
#[derive(Clone, Copy)]
struct YokePlan {
    m: usize,
    trunk: usize,
    plus: usize,
    minus: usize,
    n_trunk: usize,
    n_arm: usize,
    dims: yoke::Yoke,
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
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("    join_sym refused ({n0}, {n1})");
        }
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

/// A row's spread in its own plane (the plane of the row as a polygon): its centroid, the
/// long and short axes (unit vectors), and the semi-axes of the ellipse with the same second
/// moments. Sums in f64, so a row and its mirror image agree.
fn row_axes(pts: &[Vec3]) -> (Vec3, Vec3, Vec3, f32, f32) {
    use glam::DVec3;
    let n = pts.len().max(1) as f64;
    let d: Vec<DVec3> = pts.iter().map(|p| p.as_dvec3()).collect();
    let c = d.iter().fold(DVec3::ZERO, |s, &q| s + q) / n;
    let mut nrm = DVec3::ZERO;
    for i in 0..d.len() {
        nrm += (d[i] - c).cross(d[(i + 1) % d.len()] - c);
    }
    let nrm = nrm.normalize_or(DVec3::Z);
    let seed = d.first().map_or(DVec3::X, |&q| q - c);
    let e1 = (seed - nrm * seed.dot(nrm)).normalize_or(nrm.any_orthonormal_vector());
    let e2 = nrm.cross(e1);
    let (mut a, mut b, mut cc) = (0.0f64, 0.0f64, 0.0f64);
    for &q in &d {
        let (x, y) = ((q - c).dot(e1), (q - c).dot(e2));
        a += x * x;
        b += x * y;
        cc += y * y;
    }
    let mid = (a + cc) * 0.5;
    let half = (((a - cc) * 0.5).powi(2) + b * b).sqrt();
    let (l1, l2) = (mid + half, (mid - half).max(0.0));
    let (vx, vy) = if b.abs() > 1e-18 { (l1 - cc, b) } else if a >= cc { (1.0, 0.0) } else { (0.0, 1.0) };
    let u = (e1 * vx + e2 * vy).normalize_or(e1);
    let v = nrm.cross(u);
    (c.as_vec3(), u.as_vec3(), v.as_vec3(), (2.0 * l1 / n).sqrt() as f32, (2.0 * l2 / n).sqrt() as f32)
}

/// Where a row with axes `u`, `v` (semi-axes `big`, `small`) should start so that a grid's
/// first side runs along its long axis: the point nearest the corner at 135° behind the
/// long axis, going the row's way round. The points' angles are taken on the ellipse
/// squashed to a circle, so the four corners are the quarter points of the row.
fn corner_start(pts: &[Vec3], c: Vec3, u: Vec3, v: Vec3, big: f32, small: f32) -> usize {
    let n = pts.len();
    let xy: Vec<(f32, f32)> = pts.iter().map(|&p| ((p - c).dot(u) / big.max(1e-9), (p - c).dot(v) / small.max(1e-9))).collect();
    let area: f32 = (0..n).map(|i| { let (p, q) = (xy[i], xy[(i + 1) % n]); p.0 * q.1 - q.0 * p.1 }).sum();
    let goal = if area >= 0.0 { -0.75 * std::f32::consts::PI } else { -0.25 * std::f32::consts::PI };
    let off = |i: usize| {
        let t = xy[i].1.atan2(xy[i].0) - goal;
        (t - (t / std::f32::consts::TAU).round() * std::f32::consts::TAU).abs()
    };
    (0..n).min_by(|&i, &j| off(i).total_cmp(&off(j))).unwrap_or(0)
}

/// Each point's mirror image among `p`: the point nearest its reflection within 0.35 quad (a
/// point on the plane is its own), or None. The pairs are mutual, and a point is the image
/// of one point only, so two points at one place get two images (the nearest free point, if
/// it is as near as the nearest).
pub(in crate::zremesh) fn mirror_pairs(p: &[Vec3], plane: MirrorPlane, quad: f32) -> Vec<Option<u32>> {
    let cell = quad.max(1e-6);
    let key = |q: Vec3| { let c = (q / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    for (v, &q) in p.iter().enumerate() {
        grid.entry(key(q)).or_default().push(v as u32);
    }
    // The points within reach of `q`, nearest first.
    let near = |q: Vec3| -> Vec<(f32, u32)> {
        let c = key(q);
        let mut found: Vec<(f32, u32)> = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &u in grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)).into_iter().flatten() {
                        let d = p[u as usize].distance(q);
                        if d < quad * 0.35 {
                            found.push((d, u));
                        }
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        found
    };
    let mut partner: Vec<Option<u32>> = vec![None; p.len()];
    for v in 0..p.len() {
        if partner[v].is_some() {
            continue;
        }
        let cands = near(plane.reflect(p[v]));
        if let Some(&(d0, _)) = cands.first() {
            if let Some(&(_, u)) = cands.iter().find(|&&(d, u)| partner[u as usize].is_none() && d <= d0 + quad * 0.02) {
                partner[v] = Some(u);
                partner[u as usize] = Some(v as u32);
            }
        }
    }
    partner
}

/// Points paired with their mirror images moved to agree exactly (a point that is its own
/// image onto the plane).
fn symmetrize(p: &mut [Vec3], plane: MirrorPlane, quad: f32) -> usize {
    let partner = mirror_pairs(p, plane, quad);
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
/// them are built on one half and mirrored. `feats`: the hard edges of `w` (see
/// [`BodyFeatures`]): a hard loop round a part becomes one of its rings, rows snap to hard
/// lines; empty, the part function alone decides.
pub(super) fn sweep_body(w: &Work, phi: &[f32], quad: f32, src: u32, plane: Option<MirrorPlane>, feats: &BodyFeatures) -> Result<(Vec<Vec3>, Vec<[u32; 4]>), String> {
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    let nv = w.p.len();
    let step = body_step(quad);
    if debug && (!feats.loops.is_empty() || !feats.lines.is_empty()) {
        let transverse = feats.loops.iter().filter(|l| l.kind == LoopKind::Transverse).count();
        let plates: Vec<usize> = feats.loops.iter().filter(|l| l.kind == LoopKind::Plate).map(|l| l.inside.len()).collect();
        eprintln!("  body: {} hard loops ({transverse} transverse, {} plates of {:?} faces), {} lines, {} corners", feats.loops.len(), plates.len(), plates, feats.lines.len(), feats.corners.len());
    }
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
        let level = (sg[0].0).2;
        rings.push(Ring { pts, near, far, from: x, to: y, girth, level, hard: false });
    }
    let min_girth: f32 = std::env::var("ZR_BODYMIN").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
    // Creases round a part become its rings (HARD-1: every crease a mesh edge loop): a
    // crease replaces the level ring of its link nearest it, so the ring's row lies on the
    // crease; and (`ZR_SUPPORT`, off with 0) a ring 0.4 step either side of it, cut from the
    // part function within the link, holds the crease's shape (HARD-2). Pieces between the
    // new rings are virtual (ids past the vertices): one ring in, one out. Two sources:
    // the transverse hard loops of `feats` (closed chains of hard edges: polygroup borders,
    // creases, clean edge lines), and (`ZR_FOLDLOOPS`, off with 0, only with Detect Edges)
    // creases found from the surface itself, as the edge lines of a voxel model are too
    // broken to chain: along each link the level sets a tenth of a step apart are cut, and
    // where the surface's fold across them (the angle between its normals 0.3 step before
    // and after) peaks over `ZR_FOLDMIN` (50°) a crease runs round the part.
    let n_level = rings.len();
    let mut hard_rings = 0usize;
    let mut support_rings = 0usize;
    let fold_loops = feats.detect && std::env::var("ZR_FOLDLOOPS").map(|v| v != "0").unwrap_or(true);
    if fold_loops || feats.loops.iter().any(|l| l.kind == LoopKind::Transverse) {
        let support = std::env::var("ZR_SUPPORT").map(|v| v != "0").unwrap_or(true);
        // Every face by the pieces its vertices are in.
        let mut piece_faces: HashMap<u32, Vec<u32>> = HashMap::default();
        for (f, t) in w.t.iter().enumerate() {
            let mut cs: Vec<u32> = t.iter().map(|&v| comp[v as usize]).collect();
            cs.sort_unstable();
            cs.dedup();
            for c in cs {
                piece_faces.entry(c).or_default().push(f as u32);
            }
        }
        let link_faces = |link: (u32, u32)| -> Vec<u32> {
            let mut fs: Vec<u32> = piece_faces.get(&link.0).cloned().unwrap_or_default();
            fs.extend(piece_faces.get(&link.1).into_iter().flatten());
            fs.sort_unstable();
            fs.dedup();
            fs
        };
        // Whether every point of `a` lies within `reach` of the closed line `b` and the
        // other way round.
        let runs_along = |a: &[Vec3], b: &[Vec3], reach: f32| -> bool {
            let mut gb = SegGrid::new(quad);
            gb.add_loop(b, 0);
            let mut ga = SegGrid::new(quad);
            ga.add_loop(a, 0);
            a.iter().all(|&q| gb.nearest(q, reach).is_some()) && b.iter().all(|&q| ga.nearest(q, reach).is_some())
        };
        // The level sets of the part function at `lv` over `faces`: each closed loop as its
        // points in ring order and each point's lower and higher vertex. None if a level
        // set there is not a set of simple loops.
        let level_loops = |lv: f32, faces: &[u32]| -> Option<Vec<(Vec<Vec3>, Vec<u32>, Vec<u32>)>> {
            let mut at: HashMap<(u32, u32), (Vec3, u32, u32)> = HashMap::default();
            let mut sg: Vec<((u32, u32), (u32, u32), usize)> = Vec::new();
            for &f in faces {
                let t = w.t[f as usize];
                let fv = t.map(|v| phi[v as usize]);
                if fv.iter().any(|x| !x.is_finite()) {
                    continue;
                }
                let mut keys: Vec<(u32, u32)> = Vec::new();
                for e in 0..3 {
                    let (a, b) = (t[e], t[(e + 1) % 3]);
                    let (fa, fb) = (fv[e], fv[(e + 1) % 3]);
                    if (fa < lv) != (fb < lv) {
                        let key = (a.min(b), a.max(b));
                        at.entry(key).or_insert_with(|| {
                            let s = ((lv - fa) / (fb - fa)).clamp(0.0, 1.0);
                            (w.p[a as usize].lerp(w.p[b as usize], s), if fa < lv { a } else { b }, if fa < lv { b } else { a })
                        });
                        keys.push(key);
                    }
                }
                if keys.len() == 2 {
                    sg.push((keys[0], keys[1], f as usize));
                }
            }
            let mut adj: HashMap<(u32, u32), Vec<usize>> = HashMap::default();
            for (i, s) in sg.iter().enumerate() {
                adj.entry(s.0).or_default().push(i);
                adj.entry(s.1).or_default().push(i);
            }
            if adj.values().any(|v| v.len() > 2) {
                return None;
            }
            let mut used = vec![false; sg.len()];
            let mut out = Vec::new();
            for s0 in 0..sg.len() {
                if used[s0] {
                    continue;
                }
                let mut order: Vec<(u32, u32)> = vec![sg[s0].0];
                let mut cur = s0;
                let mut sense = 0.0f32;
                let mut closed = false;
                loop {
                    used[cur] = true;
                    let s = sg[cur];
                    let last = *order.last().unwrap();
                    let next = if s.0 == last { s.1 } else { s.0 };
                    sense += w.fnrm[s.2].cross(at[&next].0 - at[&last].0).dot(face_grad(s.2));
                    if next == order[0] {
                        closed = true;
                        break;
                    }
                    order.push(next);
                    let Some(&ns) = adj[&next].iter().find(|&&i| !used[i]) else { break };
                    cur = ns;
                }
                if !closed {
                    continue;
                }
                if sense < 0.0 {
                    order.reverse();
                }
                out.push((order.iter().map(|k| at[k].0).collect(), order.iter().map(|k| at[k].1).collect(), order.iter().map(|k| at[k].2).collect()));
            }
            Some(out)
        };
        // The one level loop at `lv` within the link running along `along`.
        let level_loop = |lv: f32, faces: &[u32], along: &[Vec3]| -> Option<(Vec<Vec3>, Vec<u32>, Vec<u32>)> {
            let mut found: Option<(Vec<Vec3>, Vec<u32>, Vec<u32>)> = None;
            for l in level_loops(lv, faces)? {
                if runs_along(&l.0, along, quad) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(l);
                }
            }
            found
        };
        // A vertex of a crease point in `piece`: itself when it is there, else a neighbour
        // within three steps, else the vertex of the ring's `old` slice nearest it.
        let pick = |v: u32, piece: u32, ring: &Ring, old: &[u32]| -> u32 {
            if comp[v as usize] == piece {
                return v;
            }
            let mut seen = vec![v];
            let mut frontier = vec![v];
            for _ in 0..3 {
                let mut next = Vec::new();
                for &x in &frontier {
                    for &u in w.neighbours(x) {
                        if comp[u as usize] == piece {
                            return u;
                        }
                        if !seen.contains(&u) {
                            seen.push(u);
                            next.push(u);
                        }
                    }
                }
                frontier = next;
            }
            let q = w.p[v as usize];
            (0..old.len()).min_by(|&a, &b| ring.pts[a].distance(q).total_cmp(&ring.pts[b].distance(q))).map_or(v, |i| old[i])
        };
        // Each crease point's vertex in the ring's nearer piece and in its farther piece,
        // from the vertices `vs` the crease was cut on (a level set between two rings has
        // both its vertices in one band: the ring's split or join needs them in its pieces).
        let pick_sides = |vs: &[u32], ring: &Ring| -> (Vec<u32>, Vec<u32>) {
            (vs.iter().map(|&v| pick(v, ring.from, ring, &ring.near)).collect(), vs.iter().map(|&v| pick(v, ring.to, ring, &ring.far)).collect())
        };
        // The creases to place: (ring, level in steps, points, vertices in the nearer
        // piece, vertices in the farther piece, where from).
        let mut creases: Vec<(usize, f32, Vec<Vec3>, Vec<u32>, Vec<u32>, &str)> = Vec::new();
        for lp in feats.loops.iter().filter(|l| l.kind == LoopKind::Transverse) {
            let verts = &lp.verts;
            let n = verts.len();
            let m = verts.iter().map(|&v| (phi[v as usize] - lo) / step).sum::<f32>() / n as f32;
            let k = m.round() as i64;
            let lpos: Vec<Vec3> = verts.iter().map(|&v| w.p[v as usize]).collect();
            let centroid = lpos.iter().copied().sum::<Vec3>() / n as f32;
            let mut lgrid = SegGrid::new(quad);
            lgrid.add_loop(&lpos, 0);
            // The level ring at k whose points lie along the loop.
            let near_loop = |r: usize| rings[r].pts.iter().filter(|&&q| lgrid.nearest(q, quad).is_some()).count() as f32 / rings[r].pts.len() as f32;
            let ring_centroid = |r: usize| rings[r].pts.iter().copied().sum::<Vec3>() / rings[r].pts.len() as f32;
            let cand = (0..n_level)
                .filter(|&r| rings[r].level == k && ring_centroid(r).distance(centroid) < quad * 2.0)
                .map(|r| (r, near_loop(r)))
                .filter(|x| x.1 >= 0.9)
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
            let Some((r, _)) = cand else {
                if debug {
                    eprintln!("    hard loop near {centroid:.3} ({:.1} quads, level {m:.1}): no level ring runs along it; left", lgrid.segs.iter().map(|s| s.0.distance(s.1)).sum::<f32>() / quad);
                }
                continue;
            };
            // Every point of the loop near the ring: else it runs into another link too.
            let rp = &rings[r].pts;
            if lpos.iter().any(|&q| rp.iter().all(|&x| x.distance(q) > quad * 1.5)) {
                if debug {
                    eprintln!("    hard loop near {centroid:.3} (level {m:.1}): spans more than one link; left");
                }
                continue;
            }
            // The loop's way round: the same sense as a level ring (the part function
            // increasing on the left, seen from outside).
            let mut sense = 0.0f32;
            for i in 0..n {
                let (a, b) = (verts[i], verts[(i + 1) % n]);
                if let Some(h) = w.fan(a).into_iter().find(|&h| w.to(h) == b) {
                    let f = h as usize / 3;
                    sense += w.fnrm[f].cross(w.p[b as usize] - w.p[a as usize]).dot(face_grad(f));
                }
            }
            let mut order = verts.clone();
            if sense < 0.0 {
                order.reverse();
            }
            let (near, far) = pick_sides(&order, &rings[r]);
            let pts: Vec<Vec3> = order.iter().map(|&v| w.p[v as usize]).collect();
            creases.push((r, m, pts, near, far, "hard loop"));
        }
        if fold_loops {
            // The fold across the part function at each vertex: the angle between the
            // surface's normals 0.3 step down and up the function from it (along the
            // neighbours lowest and highest in it).
            let reach = step * 0.3;
            let walk = |v: u32, up: bool| -> u32 {
                let mut x = v;
                for _ in 0..12 {
                    if (phi[x as usize] - phi[v as usize]).abs() >= reach {
                        break;
                    }
                    let next = w.neighbours(x).iter().copied().filter(|&u| phi[u as usize].is_finite()).min_by(|&a, &b| {
                        let (fa, fb) = (phi[a as usize], phi[b as usize]);
                        if up { fb.total_cmp(&fa) } else { fa.total_cmp(&fb) }
                    });
                    match next {
                        Some(u) if (up && phi[u as usize] > phi[x as usize]) || (!up && phi[u as usize] < phi[x as usize]) => x = u,
                        _ => break,
                    }
                }
                x
            };
            let fold: Vec<f32> = (0..nv as u32).map(|v| if phi[v as usize].is_finite() { w.vnrm[walk(v, false) as usize].angle_between(w.vnrm[walk(v, true) as usize]).to_degrees() } else { 0.0 }).collect();
            let fold_min: f32 = std::env::var("ZR_FOLDMIN").ok().and_then(|v| v.parse().ok()).unwrap_or(50.0);
            // Only rings round a part of some size (`ZR_FOLDGIRTH`, 8 quads): the level sets
            // round a plate standing out from the body are not its creases.
            let fold_girth: f32 = std::env::var("ZR_FOLDGIRTH").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
            let bend = |near: &[u32], far: &[u32]| -> f32 { (near.iter().chain(far).map(|&v| fold[v as usize]).sum::<f32>()) / (near.len() + far.len()).max(1) as f32 };
            // The run each ring is on: rings in a row through pieces with one ring in and one
            // out (a limb, the trunk). A short run is a plate or a boss standing out from the
            // body, whose edge is not a crease round a part (`ZR_FOLDRUN`, 8 rings).
            let fold_run: usize = std::env::var("ZR_FOLDRUN").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
            let mut outs: HashMap<u32, Vec<usize>> = HashMap::default();
            let mut ins: HashMap<u32, Vec<usize>> = HashMap::default();
            for (i, r) in rings.iter().enumerate().filter(|x| x.1.girth >= min_girth) {
                outs.entry(r.from).or_default().push(i);
                ins.entry(r.to).or_default().push(i);
            }
            let run_len = |r: usize| -> usize {
                let mut n = 1;
                for forward in [true, false] {
                    let mut p = if forward { rings[r].to } else { rings[r].from };
                    for _ in 0..n_level {
                        let (one_in, one_out) = (ins.get(&p).map_or(0, |v| v.len()), outs.get(&p).map_or(0, |v| v.len()));
                        if one_in != 1 || one_out != 1 {
                            break;
                        }
                        let next = if forward { outs[&p][0] } else { ins[&p][0] };
                        n += 1;
                        p = if forward { rings[next].to } else { rings[next].from };
                    }
                }
                n
            };
            let mut found: Vec<(usize, f32, f32)> = Vec::new();
            for r in 0..n_level {
                if creases.iter().any(|c| c.0 == r) || rings[r].girth < min_girth.max(fold_girth) || run_len(r) < fold_run {
                    continue;
                }
                let k = rings[r].level as f32;
                let faces = link_faces((rings[r].from, rings[r].to));
                // The fold along the level sets from half a step below the ring to just
                // under half a step above (the next ring claims the rest).
                let samples: Vec<(f32, Option<(Vec<Vec3>, Vec<u32>, Vec<u32>)>)> = (-5..5).map(|j| {
                    let t = k + j as f32 * 0.1;
                    (t, if j == 0 { Some((rings[r].pts.clone(), rings[r].near.clone(), rings[r].far.clone())) } else { level_loop(lo + t * step, &faces, &rings[r].pts) })
                }).collect();
                let bends: Vec<f32> = samples.iter().map(|s| s.1.as_ref().map_or(0.0, |l| bend(&l.1, &l.2))).collect();
                let Some(best) = (0..bends.len()).filter(|&i| samples[i].1.is_some()).max_by(|&a, &b| bends[a].total_cmp(&bends[b]).then(b.cmp(&a))) else { continue };
                // A crease: the fold peaks over the bar and is well above the link's quietest
                // level set (a tube bent all along is not a crease). Levels with no single
                // loop have no fold measured and do not count.
                let quiet = (0..bends.len()).filter(|&i| samples[i].1.is_some()).map(|i| bends[i]).fold(f32::INFINITY, f32::min);
                if bends[best] < fold_min || bends[best] < quiet + 15.0 {
                    continue;
                }
                let (t, lp) = (samples[best].0, samples[best].1.clone().unwrap());
                found.push((r, t, bends[best]));
                // Its lower vertices into the ring's nearer piece, its higher into the farther.
                let near: Vec<u32> = lp.1.iter().map(|&v| pick(v, rings[r].from, &rings[r], &rings[r].near)).collect();
                let far: Vec<u32> = lp.2.iter().map(|&v| pick(v, rings[r].to, &rings[r], &rings[r].far)).collect();
                creases.push((r, t, lp.0, near, far, "fold"));
            }
            if debug {
                let mut at: Vec<(f32, f32, Vec3)> = found.iter().map(|f| ((f.1 * 10.0).round() / 10.0, f.2.round(), rings[f.0].pts.iter().copied().sum::<Vec3>() / rings[f.0].pts.len() as f32)).collect();
                at.sort_by(|a, b| a.0.total_cmp(&b.0));
                eprintln!("  body: {} creases found by fold (level, degrees, where):", found.len());
                for (t, d, c) in at {
                    eprintln!("    crease at level {t:.1}: {d:.0}°, round {c:.2}");
                }
            }
        }
        // Each crease into its ring.
        creases.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut placed: Vec<(usize, f32, (u32, u32), Vec<Vec3>)> = Vec::new();
        for (r, m, pts, near, far, how) in creases {
            if rings[r].hard {
                if debug {
                    eprintln!("    crease ({how}) at level {m:.1}: its ring is taken by another; left");
                }
                continue;
            }
            let n = pts.len();
            let girth = (0..n).map(|i| pts[i].distance(pts[(i + 1) % n])).sum::<f32>() / quad;
            if girth < min_girth {
                continue;
            }
            let link = (rings[r].from, rings[r].to);
            let ring = &mut rings[r];
            ring.pts = pts.clone();
            ring.near = near;
            ring.far = far;
            ring.girth = girth;
            ring.hard = true;
            hard_rings += 1;
            placed.push((r, m, link, pts));
        }
        if support {
            let mut next_piece = nv as u32;
            // Where rings now lie along each link, in steps: the hard rings; supports keep
            // 0.3 step from them and from the level rings either side.
            let mut taken: Vec<((u32, u32), f32)> = placed.iter().map(|p| (p.2, p.1)).collect();
            for &(r, m, link, ref pts) in &placed {
                let k = rings[r].level as f32;
                let faces = link_faces(link);
                for (side, t) in [(-1, m - 0.4), (1, m + 0.4)] {
                    let clear = t > k - 1.0 + 0.3 && t < k + 1.0 - 0.3 && taken.iter().all(|&(l, x)| l != link || (x - t).abs() >= 0.3);
                    if !clear {
                        continue;
                    }
                    let Some((spts, near, far)) = level_loop(lo + t * step, &faces, pts) else {
                        if debug {
                            eprintln!("    hard ring at level {m:.1}: no support ring at {t:.1} (the level set there is not one loop along it)");
                        }
                        continue;
                    };
                    let girth = (0..spts.len()).map(|i| spts[i].distance(spts[(i + 1) % spts.len()])).sum::<f32>() / quad;
                    if girth < min_girth {
                        continue;
                    }
                    let vp = next_piece;
                    next_piece += 1;
                    let level = rings[r].level;
                    if side < 0 {
                        rings.push(Ring { pts: spts, near, far, from: rings[r].from, to: vp, girth, level, hard: false });
                        rings[r].from = vp;
                    } else {
                        rings.push(Ring { pts: spts, near, far, from: vp, to: rings[r].to, girth, level, hard: false });
                        rings[r].to = vp;
                    }
                    taken.push((link, t));
                    support_rings += 1;
                }
            }
        }
        if debug {
            eprintln!("  body: {hard_rings} hard loops became rings, {support_rings} support rings beside them");
        }
        // KLAY_ZR_HARDRINGS=file.obj: the hard rings and their support rings as OBJ polylines
        // (`l` records), for levels.py.
        if let Some(path) = std::env::var_os("KLAY_ZR_HARDRINGS") {
            use std::fmt::Write;
            let mut s = String::new();
            let mut n = 0usize;
            for r in rings.iter().filter(|r| r.hard).chain(rings[n_level..].iter()) {
                for q in &r.pts {
                    writeln!(s, "v {} {} {}", q.x, q.y, q.z).unwrap();
                }
                s.push('l');
                for k in 0..=r.pts.len() {
                    write!(s, " {}", n + k % r.pts.len() + 1).unwrap();
                }
                s.push('\n');
                n += r.pts.len();
            }
            std::fs::write(path, s).ok();
        }
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
    // A thin tube (a finger: two rings or more, all under 8 quads round) gets at least
    // `ZR_MINTUBE` (8) points round it, so its tip cap is a 2 × 2 grid (0 for the girth's own
    // count).
    let min_tube: usize = std::env::var("ZR_MINTUBE").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let tubular = |rs: &[usize]| rs.len() >= 2 && rs.iter().all(|&r| rings[r].girth < 8.0);
    // A ring the plane cuts takes its count in fours (its row is pinned front and back);
    // a run with one count takes fours when any of its rings is cut (harmless on the rest).
    let plan_one = |rs: &[usize]| -> Vec<usize> {
        let gs: Vec<f32> = rs.iter().map(|&r| rings[r].girth.max(1e-3)).collect();
        let floor = if tubular(rs) { min_tube } else { 0 };
        let q = |g: f32, r: usize| (if ring_cross[r].is_some() { even4(g) } else { even(g) }).max(floor);
        let (mn, mx) = gs.iter().fold((f32::INFINITY, 0.0f32), |m, &g| (m.0.min(g), m.1.max(g)));
        if mx <= mn * ratio {
            let gm = (gs.iter().map(|g| g.ln()).sum::<f32>() / gs.len() as f32).exp();
            let n = (if rs.iter().any(|&r| ring_cross[r].is_some()) { even4(gm) } else { even(gm) }).max(floor);
            return vec![n; gs.len()];
        }
        let mut n = q(gs[0], rs[0]);
        rs.iter().zip(&gs).map(|(&r, &g)| {
            if g > n as f32 * drift || g < n as f32 / drift {
                n = q(g, r);
            } else if ring_cross[r].is_some() && n % 4 != 0 {
                // The count carried from an uncut ring onto a cut one: to the nearest fours.
                n = even4(n as f32);
            }
            n
        }).collect()
    };
    // A run that narrows and then flares again near its end (the ankle into the foot, the
    // wrist into the hand) is two: the limb with one count to its narrowest ring, then the
    // foot or hand with its own (`ZR_NOANKLE` off). The foot's or hand's rings are noted:
    // their reductions go on the sole. Where the limb's count is the larger, it carries on
    // through the ankle to the ring before the foot's widest, as a box modeller's shin loops
    // wrap the ankle and the instep; from the widest ring on the foot's own counts hold. The
    // ankle band is then an equal join, and the step down to the foot's count lies between
    // two rings round the whole foot, gathered on the sole, not under the heel where a ring
    // round the whole foot meets one round the forefoot (`ZR_NOCARRY` or `ZR_OLDMERGE` gives
    // the foot its own count from its first ring, as before). When the limb's count at the
    // ankle and the foot's first differ by less than `ZR_ANKLEDIFF` (8) the run is not parted
    // at all: planned as one, the heel keeps the leg's columns and no merge ring forms (0
    // parts it as before). A neck widening into the shoulders also parts here, and keeps its
    // own counts.
    let after_ankle: std::cell::RefCell<HashSet<usize>> = std::cell::RefCell::new(HashSet::default());
    let carry = std::env::var_os("ZR_NOCARRY").is_none() && std::env::var_os("ZR_OLDMERGE").is_none();
    let ankle_diff: usize = std::env::var("ZR_ANKLEDIFF").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
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
                    let mut foot = plan_one(&rs[e + 1..]);
                    if out[e].abs_diff(foot[0]) < ankle_diff {
                        if debug {
                            eprintln!("    plan: ankle at ring {e} of {n} (girth {:.1}) kept in its run: {} above, {} below", gs[e], out[e], foot[0]);
                        }
                        return plan_one(rs);
                    }
                    if carry && out[e] > foot[0] {
                        let widest = (e + 1..n).max_by(|&a, &b| gs[a].total_cmp(&gs[b]).then(b.cmp(&a))).unwrap_or(e + 1);
                        for c in &mut foot[..widest - e - 1] {
                            *c = out[e];
                        }
                        if debug {
                            eprintln!("    plan: {} carried through the ankle (ring {e} of {n}, girth {:.1}) to the widest ring past it: girths {:?} -> counts {:?}", out[e], gs[e], gs[e + 1..].iter().map(|g| (g * 10.0).round() / 10.0).collect::<Vec<_>>(), foot);
                        }
                    }
                    out.extend(foot);
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
                            if pl.distance(w.p[v as usize]).abs() < quad * 1e-3 {
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
    // The open hard lines, for rows to snap to (`ZR_NOHARDROWS`: off), and the corners of
    // the hard lines, for the rows of hard rings to hold.
    let hard_rows = std::env::var_os("ZR_NOHARDROWS").is_none();
    let mut line_grid = SegGrid::new(quad);
    if hard_rows {
        for (i, line) in feats.lines.iter().enumerate() {
            for e in line.windows(2) {
                line_grid.add(w.p[e[0] as usize], w.p[e[1] as usize], i as u32);
            }
        }
    }
    // Per ring, the columns of its row snapped to a line: (column, line, rings running).
    let snapped_cols: std::cell::RefCell<HashMap<usize, Vec<(usize, u32, u32)>>> = std::cell::RefCell::new(HashMap::default());
    let snapped_rows = std::cell::Cell::new(0usize);
    // A row: `n` points evenly round ring `r`. With a plane: a ring it cuts has points on it
    // front (0) and back (n/2) and each half spaced evenly, the halves mirror images; any
    // other starts at its frontmost point (so a ring and its mirror image agree). Without:
    // turned to lie against `prev` (if any), the row of ring `prev_ring`. A point within 0.3
    // quad of a hard line moves onto it (one point per place on a line), and a line snapped on
    // three rings running keeps its column: the turn against `prev` holds those columns and
    // fits the rest. On a hard ring the point nearest each corner of the loop takes it.
    let make_row = |r: usize, n: usize, prev: Option<&[Vec3]>, prev_ring: Option<usize>, out_p: &mut Vec<Vec3>| -> Vec<u32> {
        let pts = &rings[r].pts;
        let mut row = resample_loop(pts, n);
        let is_pinned = plane.is_some() && ring_cross[r].is_some() && n % 2 == 0;
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
        }
        // The points that may move: not the two on the plane of a pinned row; its first half
        // moves and the other half mirrors it.
        let free: Vec<usize> = if is_pinned { (1..n / 2).collect() } else { (0..n).collect() };
        if hard_rows && rings[r].hard && !feats.corners.is_empty() {
            for &c in &feats.corners {
                if let Some(i) = free.iter().copied().filter(|&i| row[i].distance(c) < quad * 0.5).min_by(|&a, &b| row[a].distance(c).total_cmp(&row[b].distance(c))) {
                    row[i] = c;
                    if is_pinned {
                        row[n - i] = plane.unwrap().reflect(c);
                    }
                }
            }
        }
        // (index, line) of the points snapped.
        let mut snapped: Vec<(usize, u32)> = Vec::new();
        if !line_grid.is_empty() {
            let mut cands: Vec<(usize, u32, Vec3, f32)> = free.iter().filter_map(|&i| line_grid.nearest(row[i], quad * 0.3).map(|(d, q, l)| (i, l, q, d))).collect();
            cands.sort_by(|a, b| a.3.total_cmp(&b.3).then(a.0.cmp(&b.0)));
            let mut taken: Vec<(usize, u32, Vec3)> = Vec::new();
            for c in cands {
                if taken.iter().any(|t| t.1 == c.1 && t.2.distance(c.2) < quad * 0.5) {
                    continue;
                }
                taken.push((c.0, c.1, c.2));
            }
            for (i, l, q) in taken {
                row[i] = q;
                if is_pinned {
                    row[n - i] = plane.unwrap().reflect(q);
                }
                snapped.push((i, l));
            }
        }
        if let (false, None, Some(prev)) = (is_pinned, plane, prev) {
            let against = if prev.len() == n { prev.to_vec() } else { resample_loop(prev, n) };
            let cost = |a: usize| -> f32 { (0..n).map(|i| against[i].distance_squared(row[(i + a) % n])).sum() };
            // The turns that put a snapped point in the column its line held on the ring
            // before (when it has run three rings), by how many columns each holds.
            let held: Vec<(usize, u32)> = prev_ring.and_then(|pr| snapped_cols.borrow().get(&pr).map(|v| v.iter().filter(|x| x.2 >= 2).map(|x| (x.0, x.1)).collect())).unwrap_or_default();
            let mut votes: Vec<(usize, usize)> = Vec::new();
            for &(i, l) in &snapped {
                for &(c, hl) in &held {
                    if hl == l {
                        let sh = (i + n - c) % n;
                        match votes.iter_mut().find(|v| v.0 == sh) {
                            Some(v) => v.1 += 1,
                            None => votes.push((sh, 1)),
                        }
                    }
                }
            }
            let most = votes.iter().map(|v| v.1).max().unwrap_or(0);
            let sh = if most > 0 {
                votes.iter().filter(|v| v.1 == most).map(|v| v.0).min_by(|&a, &b| cost(a).total_cmp(&cost(b)).then(a.cmp(&b))).unwrap_or(0)
            } else {
                (0..n).min_by(|&a, &b| cost(a).total_cmp(&cost(b))).unwrap_or(0)
            };
            row = (0..n).map(|i| row[(i + sh) % n]).collect();
            for s in &mut snapped {
                s.0 = (s.0 + n - sh) % n;
            }
        }
        if !snapped.is_empty() {
            snapped_rows.set(snapped_rows.get() + 1);
            let runs: Vec<(u32, u32)> = prev_ring.and_then(|pr| snapped_cols.borrow().get(&pr).map(|v| v.iter().map(|x| (x.1, x.2)).collect())).unwrap_or_default();
            let rec: Vec<(usize, u32, u32)> = snapped.iter().map(|&(i, l)| (i, l, 1 + runs.iter().find(|x| x.0 == l).map_or(0, |x| x.1))).collect();
            snapped_cols.borrow_mut().insert(r, rec);
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
        rect::cap_dims(row, a, sym, out_p, quads);
    };
    // A cap's inner points lifted toward the end beyond its row (a dome: all the way at its
    // middle, none at the row, but no further than `most`), so it reaches the tip of a
    // finger, a toe or the head instead of lying flat across the last ring.
    let dome = |row: &[u32], from: usize, tip: Vec3, most: f32, out_p: &mut Vec<Vec3>| {
        let pts: Vec<Vec3> = row.iter().map(|&v| out_p[v as usize]).collect();
        let c = pts.iter().copied().sum::<Vec3>() / pts.len().max(1) as f32;
        let rr = pts.iter().map(|q| q.distance(c)).sum::<f32>() / pts.len().max(1) as f32;
        let mut lift = tip - c;
        if lift.length() > most {
            lift *= most / lift.length();
        }
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
    // (Thin rings past a piece beside the one kept, an ear or a wisp of hair, do not end the
    // run; the thin rings at the crown, before the first kept one, are the run's start.)
    let head_run: Vec<usize> = {
        let mut out = Vec::new();
        let mut p = comp[src as usize];
        loop {
            let all: Vec<usize> = outs.get(&p).cloned().unwrap_or_default();
            let k = if all.len() == 1 { all } else { kept(p) };
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
    // Which side of the plane a ring lies on (the sum of its points' distances), and its
    // extents across the plane and front to back.
    let side_of = |r: usize| -> f32 { plane.map_or(0.0, |pl| rings[r].pts.iter().map(|&q| pl.distance(q)).sum::<f32>()) };
    let extents = |r: usize| -> (f32, f32) {
        let ax = plane.map_or(0, |pl| pl.axis as usize);
        let span = |k: usize| {
            let (lo, hi) = rings[r].pts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), q| (lo.min(q[k]), hi.max(q[k])));
            hi - lo
        };
        (span(ax), span(fwd))
    };
    // A piece where the ring parts into two legs: two kept rings past it, neither cut by the
    // plane, one each side.
    let two_legs = |p: u32| -> Option<(usize, usize)> {
        let ks = kept(p);
        if ks.len() != 2 || ring_cross[ks[0]].is_some() || ring_cross[ks[1]].is_some() || side_of(ks[0]) * side_of(ks[1]) >= 0.0 {
            return None;
        }
        Some(if side_of(ks[0]) > 0.0 { (ks[0], ks[1]) } else { (ks[1], ks[0]) })
    };
    // The crotch as a cap with leg holes (`ZR_CROTCHHOLES=1`; off unless asked for: the
    // piece between the hips and the legs is one band deep, and a grid over the hips with
    // holes for the legs has far more cells than fit in it).
    let crotch_on = plane.is_some() && std::env::var("ZR_CROTCHHOLES").is_ok_and(|v| v != "0");
    // How many of a cap's grid quads may face inward (`ZR_YOKEINWARD`, percent, 10) before
    // the cap is given up.
    let inward_most: usize = std::env::var("ZR_YOKEINWARD").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    // Under a yoke the trunk keeps one count down to the crotch, as a box modeller's does
    // (its ratio rule, not the ankle rule, which would give the hips their own count);
    // `ZR_TRUNKSTEP` plans it as before.
    let yoke_on = plane.is_some() && std::env::var_os("ZR_YOKE").is_some();
    let trunk_one = yoke_on && std::env::var_os("ZR_TRUNKSTEP").is_none();
    let trunk_plan = |rs: &[usize]| -> Vec<usize> {
        if trunk_one && rs.last().is_some_and(|&r| two_legs(rings[r].to).is_some()) {
            plan_one(rs)
        } else {
            plan(rs)
        }
    };
    // The count a run past a split takes: the next split's bridged count when it splits
    // again at once, else its run's first.
    let run_count = |r: usize| -> usize {
        let run = chain_from(r);
        match totals.get(&rings[r].to) {
            Some(&t) if run.len() == 1 => t,
            _ => trunk_plan(&run)[0],
        }
    };
    // The ring a hole is bridged to: the limb's first, or with `ZR_YOKESECOND` the second of
    // its run when there is one with the same count. The first ring (an arm's at the armpit,
    // a leg's at the perineum) touches the trunk's ring at the saddle between them, and the
    // band to it has no height there (two slivers at the armpits, which the relax does not
    // open); a band to the second ring is a quad tall all round but comes out more stretched
    // under the arm (on the T-pose 46 badly shaped quads at the shoulders against 36).
    let yoke_second = std::env::var_os("ZR_YOKESECOND").is_some();
    let past_saddle = |r: usize, n: usize| -> usize {
        let run = chain_from(r);
        if yoke_second && run.len() >= 2 && plan(&run)[1] == n { run[1] } else { r }
    };
    // The crotch cap's legs and grid for the split piece `p` entered over ring `in_ring` with
    // a row of `n_in`: None if the piece is not a crotch or the grid does not fit.
    let crotch_candidate = |p: u32, in_ring: usize, n_in: usize| -> Option<(usize, usize, usize, yoke::Crotch)> {
        if !crotch_on || !pinned(in_ring, n_in) {
            return None;
        }
        let (r_plus, r_minus) = two_legs(p)?;
        let n_leg = run_count(r_plus);
        if run_count(r_minus) != n_leg {
            return None;
        }
        let (wd, dp) = extents(in_ring);
        yoke::crotch_dims(n_in, n_leg, wd, dp).map(|c| (r_plus, r_minus, n_leg, c))
    };
    // The shoulders as a yoke (opt-in, `ZR_YOKE=1`: on the T-pose it removes the ring of poles
    // at the neck base but the breasts are then not found and the shoulders come out coarser): with a plane, when the head run ends at a
    // piece with three rings past it (the trunk, cut by the plane, and an arm each side), a
    // grid over the trunk's ring with holes for the neck and the arms takes the place of the
    // rings between the neck and that piece. The neck hole takes the row of the ring after
    // the neck (the neck's own if there is none), its count the largest in fours, down from
    // its planned one, that fits the grid and is under half the trunk's count by 4 or more
    // (a box modeller's neck takes under half the trunk's loops; the rest of the grid's
    // depth then climbs the chest and the back); the band from the neck to it is the one
    // reduction.
    let yoke_plan: Option<YokePlan> = (|| {
        if !yoke_on {
            return None;
        }
        let p_s = rings[*head_run.last()?].to;
        let ks = kept(p_s);
        if ks.len() != 3 || ring_cross[ks[0]].is_none() || ring_cross[ks[1]].is_some() || ring_cross[ks[2]].is_some() || side_of(ks[1]) * side_of(ks[2]) >= 0.0 {
            return None;
        }
        let (trunk, plus, minus) = if side_of(ks[1]) > 0.0 { (ks[0], ks[1], ks[2]) } else { (ks[0], ks[2], ks[1]) };
        let (n_trunk, n_arm) = (run_count(trunk), run_count(plus));
        if run_count(minus) != n_arm {
            return None;
        }
        let (wd, dp) = extents(trunk);
        let m = (neck + 1).min(head_run.len() - 1);
        let r = head_run[m];
        let planned = counts[&r];
        // The grid's cells should cover the surface it replaces (the pieces from under the
        // neck row to the split) at about a quad each: the trunk's ring under the grid takes
        // the smallest count in fours, from the trunk's own up to twice it, whose grid has
        // `ZR_YOKEFILL` (0.3) of those cells; the trunk steps back to its own count on the
        // next band, with diamonds (`ZR_YOKEPLAIN`: the trunk's own count, a coarser grid).
        let covered: HashSet<u32> = head_run[m..].iter().map(|&r| rings[r].to).collect();
        let area: f64 = w.t.iter().filter(|t| t.iter().all(|&v| covered.contains(&comp[v as usize]))).map(|t| {
            let (a, b, c) = (w.p[t[0] as usize], w.p[t[1] as usize], w.p[t[2] as usize]);
            (b - a).cross(c - a).length() as f64 * 0.5
        }).sum();
        let fill: f64 = std::env::var("ZR_YOKEFILL").ok().and_then(|v| v.parse().ok()).unwrap_or(0.3);
        let target = area / (quad as f64 * quad as f64) * fill;
        let plain = std::env::var_os("ZR_YOKEPLAIN").is_some();
        let top = if plain { n_trunk } else { n_trunk * 2 };
        let mut best: Option<(usize, YokePlan)> = None;
        let mut n_y = n_trunk.div_ceil(4) * 4;
        while n_y <= top {
            // The largest neck hole that fits this grid: the band from the neck to it is the
            // one reduction under the jaw.
            let mut n = planned.min((n_y / 2).saturating_sub(4)) / 4 * 4;
            while n >= 12 && n * 2 >= planned {
                if pinned(r, n) && pinned(trunk, n_y) {
                    if let Some(dims) = yoke::yoke_dims(n_y, n, n_arm, wd, dp) {
                        let cells = dims.a * dims.b - dims.neck.0 * dims.neck.1 - 2 * dims.arm.0 * dims.arm.1;
                        let y = YokePlan { m, trunk, plus, minus, n_trunk: n_y, n_arm, dims };
                        if cells as f64 >= target || plain {
                            if debug {
                                eprintln!("  body: yoke ring {n_y} (trunk {n_trunk}): {cells} cells for {:.0} quads of surface", area / (quad as f64 * quad as f64));
                            }
                            return Some(y);
                        }
                        if best.as_ref().is_none_or(|(c, _)| cells > *c) {
                            best = Some((cells, y));
                        }
                        break;
                    }
                }
                n -= 4;
            }
            n_y += 4;
        }
        if best.is_none() && debug {
            eprintln!("  body: yoke does not fit (trunk {n_trunk}, neck {planned}, arms {n_arm}, ring {:.2} by {:.2})", wd, dp);
        }
        best.map(|b| b.1)
    })();
    if let Some(y) = yoke_plan {
        let r = head_run[y.m];
        let n = 2 * (y.dims.neck.0 + y.dims.neck.1);
        if debug && counts[&r] != n {
            eprintln!("  body: neck row {} instead of {} for the yoke", n, counts[&r]);
        }
        counts.insert(r, n);
    }
    let first = head_run[first];
    let row0 = make_row(first, counts[&first], None, None, &mut out_p);
    let cap_from = out_p.len();
    cap(&row0, pinned(first, row0.len()), &mut out_p, &mut quads);
    dome(&row0, cap_from, w.p[src as usize], f32::INFINITY, &mut out_p);
    // The working surface, for cap points to be put on.
    let tg = crate::zremesh::TriGrid::from_tris(&w.p, &w.t, quad);
    let surface = |q: Vec3| tg.closest_tris(&w.p, &w.t, q).map_or(q, |c| c.0);
    // How a limb's end is capped (`ZR_OLDTIPS`: rows a quad apart shrinking like a sphere,
    // then a grid domed all the way to the tip, as before): a row whose long radius is
    // `ZR_FLATASPECT` (1.3) times its short one is a flat end (a foot's last rows, slanting
    // over the toes, are 1.35 to 1.45; a thumb's 1.27).
    let old_tips = std::env::var_os("ZR_OLDTIPS").is_some();
    let flat_aspect: f32 = std::env::var("ZR_FLATASPECT").ok().and_then(|v| v.parse().ok()).unwrap_or(1.3);
    // How far past its ring's girth a split's bridged count may be for the band above to
    // step up to it: 1.8 times, as before, or `ZR_SPLITRATIO` (2.2) when one of the split's
    // runs is a thin tube lifted to the `ZR_MINTUBE` floor (fingers of 8 bridge to about
    // twice the palm's girth).
    let split_ratio: f32 = std::env::var("ZR_SPLITRATIO").ok().and_then(|v| v.parse().ok()).unwrap_or(2.2);
    let floored = |rs: &[usize]| tubular(rs) && rs.iter().any(|&r| even(rings[r].girth) < min_tube);
    // Whether rings join at piece `p` (more than one kept ring comes in).
    let joins_at = |p: u32| ins.get(&p).map_or(0, |v| v.iter().filter(|&&r| rings[r].girth >= min_girth).count()) > 1;
    // Whether a row lies on the far side of the plane, where it runs the other way round
    // from its twin (`ZR_NOCAPMIRROR`: no row does, the caps then differ).
    let far_side = |row_sym: bool, row_pos: &[Vec3]| {
        !row_sym
            && std::env::var_os("ZR_NOCAPMIRROR").is_none()
            && plane.is_some_and(|pl| pl.distance(row_pos.iter().copied().sum::<Vec3>() / row_pos.len().max(1) as f32) < 0.0)
    };
    // The cap at the end of a limb, on `row`, the tip past it at `tip`. A flat end (the toe
    // end of a foot, a mitten) goes on in rows a quad apart keeping the row's shape (shrunk
    // only near the end), the last half a quad or more short of the tip, then a grid laid
    // straight across, more quads along its long side, its inside lifted toward the tip by
    // at most half a quad and put on the surface. A round end (a finger) goes on in rows
    // shrinking toward the tip like a sphere, half a quad apart while they are wider than
    // 0.6 quad, then a grid domed toward the tip by at most half a quad.
    let end_cap = |row: &[u32], row_sym: bool, tip: Vec3, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>| {
        let row_pos: Vec<Vec3> = row.iter().map(|&i| out_p[i as usize]).collect();
        let n = row.len();
        let (c, _, _, big, small) = row_axes(&row_pos);
        let dist = tip.distance(c);
        if !old_tips && n >= 6 && big >= small * flat_aspect {
            let extra = (dist / quad).floor() as usize;
            let mut last = row.to_vec();
            let mut rows = 0;
            for k in 1..=extra.min(8) {
                let t = k as f32 / (extra as f32 + 1.0);
                let s = (1.0 - t.powi(4)).max(0.0).powf(0.25);
                let cc = c.lerp(tip, t);
                let next: Vec<u32> = row_pos.iter().map(|&q| { out_p.push(cc + (q - c) * s); (out_p.len() - 1) as u32 }).collect();
                join_band(&last, &next, out_p, quads);
                last = next;
                rows += 1;
            }
            let last_pos: Vec<Vec3> = last.iter().map(|&i| out_p[i as usize]).collect();
            let (c, ax_u, ax_v, big, small) = row_axes(&last_pos);
            let half = n / 2;
            let mut a = ((half as f32 * big / (big + small)).round() as usize).clamp(1, half - 1);
            if row_sym && a % 2 == 1 {
                a = if a + 1 < half { a + 1 } else { a - 1 };
            }
            let a = a.max(1);
            // A row on the far side of the plane runs the other way round from its twin's:
            // reversed, then started at the corner where its long side begins, so the cap is
            // the twin's mirror image (a pinned row keeps its start: the grid line along the
            // plane). cap_dims winds its quads the row's way, so the reversed row's cap comes
            // out inside out until orient() at the end of the build turns it.
            if far_side(row_sym, &row_pos) {
                last = (0..n).map(|i| last[(n - i) % n]).collect();
            }
            if !row_sym {
                let last_pos: Vec<Vec3> = last.iter().map(|&i| out_p[i as usize]).collect();
                let k = corner_start(&last_pos, c, ax_u, ax_v, big, small);
                last = (0..n).map(|i| last[(i + k) % n]).collect();
            }
            let from = out_p.len();
            rect::cap_dims(&last, a, row_sym, out_p, quads);
            let dir = (tip - c).normalize_or_zero();
            let lift = tip.distance(c).min(quad * 0.5);
            let (eu, ev) = last_pos.iter().fold((0.0f32, 0.0f32), |m, &q| (m.0.max((q - c).dot(ax_u).abs()), m.1.max((q - c).dot(ax_v).abs())));
            for v in from..out_p.len() {
                let q = out_p[v] - c;
                let (x, y) = (q.dot(ax_u) / eu.max(1e-9), q.dot(ax_v) / ev.max(1e-9));
                out_p[v] += dir * (lift * (1.0 - x * x - y * y).max(0.0));
                out_p[v] = surface(out_p[v]);
            }
            if debug {
                eprintln!("    cap flat {a} x {} after {rows} rows near {:.3}: row of {n}, radii {:.2} x {:.2} quads, tip {:.2} quads past", half - a, c, big / quad, small / quad, dist / quad);
            }
        } else {
            let rr = row_pos.iter().map(|q| q.distance(c)).sum::<f32>() / n as f32;
            let extra = if old_tips { (dist / quad).round() as usize } else { (dist / (quad * 0.5)).floor() as usize };
            let mut last = row.to_vec();
            let mut rows = 0;
            for k in 1..=extra.min(if old_tips { 8 } else { 16 }) {
                let t = k as f32 / (extra as f32 + 1.0);
                let s = (1.0 - t * t).max(0.0).sqrt();
                if !old_tips && rr * s < quad * 0.6 {
                    break;
                }
                let cc = c.lerp(tip, t);
                let next: Vec<u32> = row_pos.iter().map(|&q| { out_p.push(cc + (q - c) * s); (out_p.len() - 1) as u32 }).collect();
                join_band(&last, &next, out_p, quads);
                last = next;
                rows += 1;
            }
            // Reversed on the far side of the plane, so the cap is the twin's mirror image
            // (the dome only reads the row's centre and radius).
            let nl = last.len();
            let capped: Vec<u32> = if far_side(row_sym, &row_pos) { (0..nl).map(|i| last[(nl - i) % nl]).collect() } else { last.clone() };
            let from = out_p.len();
            cap(&capped, row_sym, out_p, quads);
            dome(&last, from, tip, if old_tips { f32::INFINITY } else { quad * 0.5 }, out_p);
            if debug {
                let a = (n / 4).max(1) + usize::from(row_sym && (n / 4).max(1) % 2 == 1 && n >= 8);
                eprintln!("    cap round {rows} rows, {a} x {} near {:.3}: row of {n}, radii {:.2} x {:.2} quads, tip {:.2} quads past", n / 2 - a, c, big / quad, small / quad, dist / quad);
            }
        }
    };
    // Points of the head (its cap and rows down to the neck): where the eyes are looked for.
    let mut head: HashSet<u32> = row0.iter().copied().chain(cap_from as u32..out_p.len() as u32).collect();
    let mut stack: Vec<(u32, Vec<u32>, bool, usize)> = vec![(rings[first].to, row0.clone(), pinned(first, row0.len()), first)];
    // Rows waiting where rings join (a loop in the model), with the ring each came through.
    let mut pending: HashMap<u32, Vec<(Vec<u32>, usize)>> = HashMap::default();
    let mut joins = 0;
    let mut splits = 0;
    let mut caps = 1;
    let mut yokes = 0;
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
        let (row, row_sym) = if joins_at(p) {
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
                let next = make_row(r, seq.len(), Some(&seq_pos), None, &mut out_p);
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
        // The yoke, from the neck row: the trunk's row and a grid over it settled onto the
        // shoulders, the neck hole in its middle bridged to this row, an arm hole at each
        // side bridged to the arm's row; the trunk and the arms go on from there. If the grid
        // does not lie on the surface, the rings below are built as before.
        if let Some(y) = yoke_plan.filter(|y| head_run[y.m] == in_ring && yokes == 0) {
            let pl = plane.unwrap();
            let (np0, nq0) = (out_p.len(), quads.len());
            let built = (|| -> Result<(), String> {
                let yoke::Yoke { a, b, neck: (a_n, b_n), arm: (a_a, b_a) } = y.dims;
                if row.len() != 2 * (a_n + b_n) {
                    return Err(format!("the neck row has {} points, the hole {}", row.len(), 2 * (a_n + b_n)));
                }
                let row_t = make_row(y.trunk, y.n_trunk, None, None, &mut out_p);
                let centroid = |ids: &[u32], out_p: &[Vec3]| ids.iter().map(|&v| out_p[v as usize]).sum::<Vec3>() / ids.len().max(1) as f32;
                let (c_neck, c_trunk) = (centroid(&row, &out_p), centroid(&row_t, &out_p));
                // The shoulders' surface: the pieces from under the neck row to the split.
                let covered: HashSet<u32> = head_run[y.m..].iter().map(|&r| rings[r].to).collect();
                let patch = yoke::Patch::new(&w.p, &w.t, &w.fnrm, &|v| covered.contains(&comp[v as usize]), quad).ok_or("no surface under the yoke")?;
                let up = c_neck - c_trunk;
                let (ids, cells, plus_high) = yoke::settle_cap(&row_t, a, pl, &patch, &w.p, up.normalize_or(Vec3::Y), up.length() * 1.5 + quad, quad, &mut out_p, &mut quads)?;
                // The neck hole in the middle of the grid's rows (the chest and the back climb
                // as far, and the rows in front and behind must cover both); the arm holes at
                // the sides, by row round the point nearest the + arm's ring.
                let row_near = |col: usize, c: Vec3, out_p: &[Vec3]| (0..=b).min_by(|&x, &y| out_p[ids[x][col] as usize].distance(c).total_cmp(&out_p[ids[y][col] as usize].distance(c))).unwrap_or(0);
                let i_n = a / 2 - a_n / 2;
                let j_n = ((b - b_n) / 2).clamp(1, b - 1 - b_n);
                let c_arm = rings[y.plus].pts.iter().copied().sum::<Vec3>() / rings[y.plus].pts.len().max(1) as f32;
                let (i_plus, i_minus) = if plus_high { (a - a_a, 0) } else { (0, a - a_a) };
                let j_a = row_near(i_plus + a_a / 2, c_arm, &out_p).saturating_sub(b_a / 2).clamp(1, b - 1 - b_a);
                let mut holes: Vec<usize> = yoke::hole_cells(a, i_n, j_n, a_n, b_n);
                holes.extend(yoke::hole_cells(a, i_plus, j_a, a_a, b_a));
                holes.extend(yoke::hole_cells(a, i_minus, j_a, a_a, b_a));
                let skip: HashSet<usize> = holes.into_iter().collect();
                let (bad, total) = yoke::inward(&cells, &skip, &patch, &w.p, &out_p, quad);
                if bad * 100 > total * inward_most {
                    return Err(format!("{bad} of {total} grid quads face inward"));
                }
                quads.extend(cells.iter().enumerate().filter(|(k, _)| !skip.contains(k)).map(|(_, c)| *c));
                // The neck row into its hole: the border from the front plane point, toward
                // the side the row's point 1 is on, half joined and mirrored.
                let lp = yoke::hole_border(&ids, i_n, j_n, a_n, b_n);
                let side1 = pl.distance(out_p[row[1] as usize]) > 0.0;
                let border = yoke::turned(&lp, a_n / 2, side1 != plus_high);
                if !join_sym(&row, &border, &mut out_p, &mut quads, pl) {
                    return Err("the neck row does not join its hole".into());
                }
                // The + arm's row into its hole, and the - arm as the mirror image.
                let grid_mirror: HashMap<u32, u32> = ids.iter().flat_map(|r| (0..=a).map(move |i| (r[i], r[a - i]))).collect();
                let bp = yoke::hole_border(&ids, i_plus, j_a, a_a, b_a);
                let bm: Vec<u32> = bp.iter().map(|v| grid_mirror[v]).collect();
                let (arm_plus, arm_minus) = (past_saddle(y.plus, y.n_arm), past_saddle(y.minus, y.n_arm));
                let row_plus = make_row(arm_plus, y.n_arm, None, None, &mut out_p);
                let row_minus = yoke::bridge_mirrored(&bp, &bm, &row_plus, pl, &mut out_p, &mut quads);
                if debug {
                    eprintln!("  body: yoke {a}x{b} on trunk {}, neck hole {a_n}x{b_n} at row {j_n}, arm holes {a_a}x{b_a} at row {j_a} ({} of {} grid quads inward{})", y.n_trunk, bad, total, if arm_plus != y.plus { "; arms from their second ring" } else { "" });
                }
                for r in [y.trunk, y.plus, y.minus] {
                    let run = chain_from(r);
                    for (k, c) in trunk_plan(&run).into_iter().enumerate() {
                        counts.insert(run[k], c);
                    }
                }
                torso.extend(row_t.iter().copied());
                stack.push((rings[y.trunk].to, row_t, true, y.trunk));
                stack.push((rings[arm_plus].to, row_plus, false, arm_plus));
                stack.push((rings[arm_minus].to, row_minus, false, arm_minus));
                Ok(())
            })();
            match built {
                Ok(()) => {
                    yokes += 1;
                    continue;
                }
                Err(e) => {
                    out_p.truncate(np0);
                    quads.truncate(nq0);
                    if debug {
                        eprintln!("  body: yoke not built ({e}); the rings below instead");
                    }
                }
            }
        }
        let k = kept(p);
        let row_pos: Vec<Vec3> = row.iter().map(|&i| out_p[i as usize]).collect();
        match k.len() {
            0 => {
                // The end of a limb: the tip past the rings kept (a finger's end is thinner
                // than they are).
                end_cap(&row, row_sym, tip_past(p), &mut out_p, &mut quads);
                caps += 1;
            }
            1 => {
                let r = k[0];
                // At the end of a limb whose last rings close round the tip (the toes of a
                // foot): when this row is flat and the rings left (three at most) are all
                // under 0.85 of its girth, the cap goes on this row and they are dropped. Not
                // when another part joins at one of them (a hand resting on a thigh): the
                // other row would wait for this one for ever.
                if !old_tips && row.len() >= 6 {
                    let rest = chain_from(r);
                    if rest.len() <= 3 && kept(rings[rest[rest.len() - 1]].to).is_empty() && !rest.iter().any(|&q| joins_at(rings[q].to)) {
                        let aspect = |pts: &[Vec3]| { let (_, _, _, b, s) = row_axes(pts); b / s.max(1e-9) };
                        let here = aspect(&row_pos);
                        let ahead: Vec<f32> = rest.iter().map(|&q| aspect(&rings[q].pts)).collect();
                        let closing = rest.iter().all(|&q| rings[q].girth < rings[in_ring].girth * 0.85);
                        let early = here >= flat_aspect && closing;
                        if debug {
                            eprintln!("    limb end near {:.3}: row of {} (girth {:.1}, aspect {here:.2}), {} rings left (girths {:?}, aspects {:?}){}", row_pos[0], row.len(), rings[in_ring].girth, rest.len(), rest.iter().map(|&q| (rings[q].girth * 10.0).round() / 10.0).collect::<Vec<_>>(), ahead.iter().map(|x| (x * 100.0).round() / 100.0).collect::<Vec<_>>(), if early { ": capped here" } else { "" });
                        }
                        if early {
                            end_cap(&row, row_sym, tip_past(p), &mut out_p, &mut quads);
                            caps += 1;
                            continue;
                        }
                    }
                }
                let g = rings[r].girth;
                let cut = ring_cross[r].is_some();
                let mut n = match counts.get(&r) {
                    Some(&c) => c,
                    None => {
                        let n0 = row.len();
                        let c = if g > n0 as f32 * drift || g < n0 as f32 / drift {
                            if cut { even4(g) } else { even(g) }
                        } else {
                            n0
                        };
                        if tubular(&chain_from(r)) { c.max(min_tube) } else { c }
                    }
                };
                // Before a split, the count it bridges into (the merges go in this band, not
                // in the split's) when within 1.8 times the girth (`ZR_SPLITRATIO`, 2.2, when
                // a part of the split is a thin tube given the `ZR_MINTUBE` floor). Not where a
                // yoke or a crotch cap takes the row: those keep the trunk's count.
                let nk = kept(rings[r].to);
                if nk.len() >= 2 && yoke_plan.is_none_or(|y| head_run[y.m] != r) && crotch_candidate(rings[r].to, r, n).is_none() {
                    if let Ok(sp) = split_plan(rings[r].to, &nk, &totals) {
                        let ratio = if nk.iter().any(|&q| floored(&chain_from(q))) { split_ratio } else { 1.8 };
                        if (sp.total as f32) < g * ratio && (sp.total as f32) > g / ratio {
                            n = sp.total;
                        }
                    }
                }
                // A band's merges take three edges each: a row over three times the other's
                // count would leave triangles, so the count is moved to the nearest it can
                // be, in the row's parity (`ZR_NOBANDGUARD` leaves it).
                if std::env::var_os("ZR_NOBANDGUARD").is_none() {
                    let (n0, step) = (row.len(), if cut { 4 } else { 2 });
                    let fixed = if n0 > 3 * n {
                        Some(n0.div_ceil(3).div_ceil(step) * step)
                    } else if n > 3 * n0 {
                        Some((3 * n0 / step * step).max(4))
                    } else {
                        None
                    };
                    if let Some(m) = fixed {
                        if debug {
                            eprintln!("    body: band {n0} -> {n} near {:.3} would need triangles, {m} instead", row_pos.iter().copied().sum::<Vec3>() / row_pos.len().max(1) as f32);
                        }
                        n = m;
                    }
                }
                let next = make_row(r, n, Some(&row_pos), Some(in_ring), &mut out_p);
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
                // The crotch as a cap with leg holes: a grid over this row settled down onto
                // the crotch, a hole each side bridged to the leg's row, the perineum strip
                // between them. If it does not lie on the surface, the split's strips as
                // before.
                if let Some((r_plus, r_minus, n_leg, dims)) = crotch_candidate(p, in_ring, row.len()) {
                    let pl = plane.unwrap();
                    let (np0, nq0) = (out_p.len(), quads.len());
                    let built = (|| -> Result<(), String> {
                        let yoke::Crotch { a, b, leg: (a_l, b_l) } = dims;
                        let ring_c = |r: usize| rings[r].pts.iter().copied().sum::<Vec3>() / rings[r].pts.len().max(1) as f32;
                        let c_in = row_pos.iter().copied().sum::<Vec3>() / row_pos.len().max(1) as f32;
                        let down = (ring_c(r_plus) + ring_c(r_minus)) * 0.5 - c_in;
                        let patch = yoke::Patch::new(&w.p, &w.t, &w.fnrm, &|v| comp[v as usize] == p, quad).ok_or("no surface under the crotch cap")?;
                        let (ids, cells, plus_high) = yoke::settle_cap(&row, a, pl, &patch, &w.p, down.normalize_or(Vec3::NEG_Y), down.length() * 1.5 + quad, quad, &mut out_p, &mut quads)?;
                        // The holes a column in from the sides, by row round the point nearest
                        // the + leg's ring.
                        let (i_plus, i_minus) = if plus_high { (a - 1 - a_l, 1) } else { (1, a - 1 - a_l) };
                        let c_leg = ring_c(r_plus);
                        let col = i_plus + a_l / 2;
                        let j_l = (0..=b).min_by(|&x, &y| out_p[ids[x][col] as usize].distance(c_leg).total_cmp(&out_p[ids[y][col] as usize].distance(c_leg))).unwrap_or(0).saturating_sub(b_l / 2).clamp(1, b - 1 - b_l);
                        let mut holes = yoke::hole_cells(a, i_plus, j_l, a_l, b_l);
                        holes.extend(yoke::hole_cells(a, i_minus, j_l, a_l, b_l));
                        let skip: HashSet<usize> = holes.into_iter().collect();
                        let (bad, total) = yoke::inward(&cells, &skip, &patch, &w.p, &out_p, quad);
                        if bad * 100 > total * inward_most {
                            return Err(format!("{bad} of {total} grid quads face inward"));
                        }
                        quads.extend(cells.iter().enumerate().filter(|(k, _)| !skip.contains(k)).map(|(_, c)| *c));
                        let grid_mirror: HashMap<u32, u32> = ids.iter().flat_map(|r| (0..=a).map(move |i| (r[i], r[a - i]))).collect();
                        let bp = yoke::hole_border(&ids, i_plus, j_l, a_l, b_l);
                        let bm: Vec<u32> = bp.iter().map(|v| grid_mirror[v]).collect();
                        let (leg_plus, leg_minus) = (past_saddle(r_plus, n_leg), past_saddle(r_minus, n_leg));
                        let row_plus = make_row(leg_plus, n_leg, None, None, &mut out_p);
                        let row_minus = yoke::bridge_mirrored(&bp, &bm, &row_plus, pl, &mut out_p, &mut quads);
                        if debug {
                            eprintln!("  body: crotch cap {a}x{b} on trunk {}, leg holes {a_l}x{b_l} at row {j_l} ({bad} of {total} grid quads inward{})", row.len(), if leg_plus != r_plus { "; legs from their second ring" } else { "" });
                        }
                        for r in [r_plus, r_minus] {
                            let run = chain_from(r);
                            for (k, c) in plan(&run).into_iter().enumerate() {
                                counts.insert(run[k], c);
                            }
                        }
                        stack.push((rings[leg_plus].to, row_plus, false, leg_plus));
                        stack.push((rings[leg_minus].to, row_minus, false, leg_minus));
                        Ok(())
                    })();
                    match built {
                        Ok(()) => {
                            yokes += 1;
                            continue;
                        }
                        Err(e) => {
                            out_p.truncate(np0);
                            quads.truncate(nq0);
                            if debug {
                                eprintln!("  body: crotch cap not built near {:.3} ({e}); split instead", w.p[p as usize]);
                            }
                        }
                    }
                }
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
                        let ids = make_row(sp.rings[i], sp.counts[i], None, None, &mut out_p);
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
                    eprintln!("    body split near {:.3}: ring {} round into {:?} (strips {:?}), bridged loop {}; runs of {:?} rings, girths {:?}", w.p[p as usize], row.len(), sp.counts, strip_pts.iter().map(|s| s.len() + 1).collect::<Vec<_>>(), seq.len(), sp.rings.iter().map(|&r| chain_from(r).len()).collect::<Vec<_>>(), sp.rings.iter().map(|&r| (rings[r].girth * 10.0).round() / 10.0).collect::<Vec<_>>());
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
        eprintln!("  body: {} quads, {} splits, {} yokes, {} joins, {} caps, {} rings", quads.len(), splits, yokes, joins, caps, rings.len());
        if !line_grid.is_empty() || hard_rings > 0 {
            eprintln!("  body: {} rows snapped to hard lines", snapped_rows.get());
        }
    }
    // Breasts (`ZR_NOBUMPS` off): on each run of mirrored rows of one count (the trunk), a bump
    // standing out from the grid smoothed is cut out as a rectangle of quads and capped like a
    // sphere: rings in from the rectangle's border, a grid at its middle; the other side the
    // mirror image.
    orient(&mut quads, &out_p);
    if let (Some(pl), None) = (plane, std::env::var_os("ZR_NOBUMPS")) {
        let made = bumps(&torso, &mut out_p, &mut quads, pl, quad, debug, &cap, &surface, 1.0, ("ZR_BUMP", 1.5, 0.4, 8.0, 0.6, 1.0));
        if debug {
            eprintln!("  body: {made} bumps capped");
        }
        if made > 0 {
            orient(&mut quads, &out_p);
        }
        // Eyes (`ZR_NOEYES` off): the sockets, the deepest dips on the front of the head,
        // ringed the same way.
        if std::env::var_os("ZR_NOEYES").is_none() {
            let eyes = bumps(&head, &mut out_p, &mut quads, pl, quad, debug, &cap, &surface, -1.0, ("ZR_EYE", 0.4, 0.5, 4.0, 0.5, 1.0));
            if debug {
                eprintln!("  body: {eyes} eye sockets ringed");
            }
            if eyes > 0 {
                orient(&mut quads, &out_p);
            }
        }
        // The mouth (`ZR_NOMOUTH` off): one ring across the centre line.
        if std::env::var_os("ZR_NOMOUTH").is_none() && mouth(&lower_face, &mut out_p, &mut quads, pl, quad, debug, &cap, &surface) {
            orient(&mut quads, &out_p);
        }
    }
    if let Some(pl) = plane {
        let lonely = symmetrize(&mut out_p, pl, quad);
        if debug {
            eprintln!("  body: mirrored, {lonely} of {} points without a mirror image", out_p.len());
            let pairs = mirror_pairs(&out_p, pl, quad);
            for v in (0..out_p.len()).filter(|&v| pairs[v].is_none()).take(12) {
                eprintln!("    lonely point at {:.3}", out_p[v]);
            }
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

/// Bumps on the front of the torso (breasts), inset like a box modeller does it. Each point's
/// height over the result smoothed many times; the highest torso point on the front of the +
/// side standing over `ZR_BUMP` (1.5) quads out is a bump's tip, the torso points round it over
/// `ZR_BUMPFRAC` (0.4) of that its region. A rectangle of the grid round the tip as wide as
/// the region is cut out and filled with rings in toward the tip and a grid at the middle
/// (four 5-poles at the rectangle's corners, four 3-poles at the grid's, the grid outside
/// untouched); the other side the mirror image. Returns how many were made.
/// `sign` −1 finds dips instead (eye sockets); `set` is the env prefix and defaults: (prefix,
/// least height in quads, region fraction, most reach in quads, least forward normal, pairs).
/// `ZR_DISC` cuts the disc of quads within reach instead and fills it with rings at the
/// border's count then a third of it (the old way, for comparison).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn bumps(torso: &HashSet<u32>, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, plane: MirrorPlane, quad: f32, debug: bool, cap: &dyn Fn(&[u32], bool, &mut Vec<Vec3>, &mut Vec<[u32; 4]>), surface: &dyn Fn(Vec3) -> Vec3, sign: f32, set: (&str, f32, f32, f32, f32, f32)) -> usize {
    let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
    let disc = std::env::var_os("ZR_DISC").is_some();
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
        if !disc {
            // A rectangle of the grid over the region: its strips run across the body (along
            // the plane's normal) through the tip's quad; it is one cell wider than the
            // region each way (the cells measured at the tip: the face's are wider than a
            // quad), shrunk a side at a time while none fits; of the places it can take
            // round the tip, the one centred nearest the region's middle. It stays off the
            // plane and its border on regular grid points; poles inside it are cut away.
            let adj = rect::Adj::new(quads, out_p.len());
            let mut along = Vec3::ZERO;
            along[plane.axis as usize] = 1.0;
            let Some((seed, u_edge)) = rect::seed_at(&adj, quads, out_p, tip as u32, along, &|q| plane.distance(q)) else {
                if debug {
                    eprintln!("    bump at {c:.3} ({hp:.1} quads out): its tip is not on a regular grid");
                }
                continue;
            };
            let (cu, cv) = rect::cell_sizes(quads, out_p, seed, u_edge);
            let vdir = (pos0[u_edge.1 as usize] - pos0[u_edge.0 as usize]).normalize_or_zero();
            let mut vs: Vec<u32> = region.iter().copied().collect();
            vs.sort_unstable();
            let mid = (vs.iter().fold(glam::DVec3::ZERO, |s, &v| s + pos0[v as usize].as_dvec3()) / vs.len().max(1) as f64).as_vec3();
            let extent = |d: Vec3| vs.iter().map(|&v| pos0[v as usize].dot(d)).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)));
            let (ulo, uhi) = extent(along);
            let (vlo, vhi) = extent(vdir);
            let most = (2.0 * max_r) as usize;
            let (mut a, mut b) = ((((uhi - ulo) / cu.max(1e-9)).ceil() as usize + 2).clamp(2, most), (((vhi - vlo) / cv.max(1e-9)).ceil() as usize + 2).clamp(2, most));
            if debug {
                eprintln!("    bump at {c:.3}: region of {} points, {:.1} x {:.1} cells of {:.2} x {:.2} quads, middle {mid:.3}: block {a} x {b}", vs.len(), (uhi - ulo) / cu, (vhi - vlo) / cv, cu / quad, cv / quad);
            }
            // (Its corners on regular points, so they become plain 5-poles; a merge on a side
            // keeps the valence it had.)
            let fits = |block: &[usize]| -> bool {
                block.iter().all(|&f| quads[f].iter().all(|&v| (v as usize) < np && plane.distance(pos0[v as usize]) > quad * 0.3)) && rect::rect_border(quads, block).is_some_and(|(border, corners)| corners.iter().all(|&m| adj.regular(border[m])))
            };
            let mut found: Option<(Vec<usize>, usize, usize, usize, usize)> = None;
            while a >= 2 && b >= 2 && found.is_none() {
                let mut best: Option<(f32, Vec<usize>, usize, usize)> = None;
                for (ia, ib) in rect::shifts(a, b, false) {
                    if let Some(block) = rect::grid_rect(&adj, quads, seed, u_edge, a, b, ia, ib) {
                        if fits(&block) {
                            let centre = (block.iter().flat_map(|&f| quads[f]).fold(glam::DVec3::ZERO, |s, v| s + pos0[v as usize].as_dvec3()) / (4 * block.len()) as f64).as_vec3();
                            let d = centre.distance(mid);
                            if best.as_ref().is_none_or(|bst| d < bst.0) {
                                best = Some((d, block, ia, ib));
                            }
                        }
                    }
                }
                if let Some((_, block, ia, ib)) = best {
                    found = Some((block, a, b, ia, ib));
                } else if a >= b {
                    a -= 1;
                } else {
                    b -= 1;
                }
            }
            let Some((block, a, b, ia, ib)) = found else {
                if debug {
                    let round: Vec<(u32, bool)> = adj.quads_at(tip as u32).iter().flat_map(|&f| quads[f as usize].iter().map(|&v| (v, adj.regular(v)))).collect();
                    eprintln!("    bump at {c:.3} ({hp:.1} quads out): no rectangle of the grid round it (seed {seed}, edge {u_edge:?}, points round the tip {round:?})");
                }
                continue;
            };
            // Its mirror image: each quad's centre reflected and looked up among the centres.
            let cell = quad.max(1e-6);
            let centre = |f: usize| quads[f].iter().map(|&v| out_p[v as usize]).sum::<Vec3>() * 0.25;
            let key = |q: Vec3| { let k = (q / cell).floor(); (k.x as i32, k.y as i32, k.z as i32) };
            let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::default();
            for f in 0..quads.len() {
                grid.entry(key(centre(f))).or_default().push(f);
            }
            let mut block2: Vec<usize> = Vec::with_capacity(block.len());
            for &f in &block {
                let m = plane.reflect(centre(f));
                let k = key(m);
                let mut best: Option<(f32, usize)> = None;
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for &g in grid.get(&(k.0 + dx, k.1 + dy, k.2 + dz)).into_iter().flatten() {
                                let d = centre(g).distance(m);
                                if best.is_none_or(|b| d < b.0 || (d == b.0 && g < b.1)) {
                                    best = Some((d, g));
                                }
                            }
                        }
                    }
                }
                match best {
                    Some((d, g)) if d <= quad * 0.3 => block2.push(g),
                    _ => break,
                }
            }
            let mut uniq = block2.clone();
            uniq.sort_unstable();
            uniq.dedup();
            let on_grid = block2.len() == block.len() && uniq.len() == block.len() && block2.iter().all(|&f| quads[f].iter().all(|&v| (v as usize) < np));
            let borders = if on_grid { rect::rect_border(quads, &block).zip(rect::rect_border(quads, &block2)) } else { None };
            let Some(((border, corners), (border2, _))) = borders else {
                if debug {
                    eprintln!("    bump at {c:.3}: mirror block not on the grid");
                }
                continue;
            };
            // The mirror border turned and run to match point for point. Its length is
            // checked first: the same quad count can make a different rectangle.
            let nbd = border.len();
            if border2.len() != nbd {
                if debug {
                    eprintln!("    bump at {c:.3}: mirror block not on the grid (its border has {} points to {nbd})", border2.len());
                }
                continue;
            }
            let img = |v: u32| plane.reflect(pos0[v as usize]);
            let start = (0..nbd).min_by(|&x, &y| pos0[border2[x] as usize].distance(img(border[0])).total_cmp(&pos0[border2[y] as usize].distance(img(border[0])))).unwrap();
            let fwd_ok = pos0[border2[(start + 1) % nbd] as usize].distance(img(border[1])) < pos0[border2[(start + nbd - 1) % nbd] as usize].distance(img(border[1]));
            let b2: Vec<u32> = (0..nbd).map(|m| if fwd_ok { border2[(start + m) % nbd] } else { border2[(start + nbd - m) % nbd] }).collect();
            if (0..nbd).any(|m| pos0[b2[m] as usize].distance(img(border[m])) > quad * 0.3) {
                if debug {
                    eprintln!("    bump at {c:.3}: mirror block not on the grid (its border does not line up)");
                }
                continue;
            }
            let block_verts: Vec<u32> = block.iter().flat_map(|&f| quads[f]).collect();
            let mut gone: Vec<usize> = block.iter().chain(block2.iter()).copied().collect();
            gone.sort_unstable();
            for &f in gone.iter().rev() {
                quads.swap_remove(f);
            }
            // Rings in toward the tip, a grid at the middle, lifted to the tip and put on the
            // surface: the right side built, the left its mirror image quad for quad.
            // (Toward the block's highest inner point: the tip itself unless the block had
            // to slide off it.)
            let peak = {
                let mut best: Option<u32> = None;
                for &v in &block_verts {
                    if !border.contains(&v) && best.is_none_or(|b| h[v as usize] > h[b as usize] || (h[v as usize] == h[b as usize] && v < b)) {
                        best = Some(v);
                    }
                }
                pos0[best.map_or(tip, |v| v as usize)]
            };
            // Rings from the block's longer side (`ZR_INSETSHORT`: the shorter, fewer rings).
            let side = if std::env::var_os("ZR_INSETSHORT").is_some() { a.min(b) } else { a.max(b) };
            let k = ((side as f32 / 2.0).round() as usize).clamp(1, 4);
            let (p_start, q_start) = (out_p.len(), quads.len());
            let rings = rect::inset_fill(&border, corners, k, 1.0, Some(peak), surface, out_p, quads);
            let last = rings.last().cloned().unwrap_or_else(|| border.clone());
            let from = out_p.len();
            rect::cap_dims(&last, corners[1], false, out_p, quads);
            rect::dome_cap(&last, from, Some(peak), surface, out_p);
            if debug {
                eprintln!("    bump at {c:.3}, {hp:.1} quads out: {a} x {b} block cut (the tip at column {ia}, row {ib}), {k} rings and a grid");
            }
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
            continue;
        }
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
pub(super) fn disc_border(quads: &[[u32; 4]], qs: &[usize]) -> Option<Vec<u32>> {
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
/// nose tip that stands out less than its neighbours above and below. A rectangle of the grid
/// across the plane there, `ZR_MOUTHR` (2.5) quads to each side and half as tall, is cut out
/// and filled with rings in from its border and a grid with a line along the plane, its own
/// mirror image. Whether one was made. `ZR_DISC` cuts the disc within reach instead (the old
/// way).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn mouth(face: &HashSet<u32>, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, plane: MirrorPlane, quad: f32, debug: bool, cap: &dyn Fn(&[u32], bool, &mut Vec<Vec3>, &mut Vec<[u32; 4]>), surface: &dyn Fn(Vec3) -> Vec3) -> bool {
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
    // (Not the line's last point: smoothed against neighbours above only, it stands out
    // nearly as far as the nose, and more once the neck below is stretched.)
    let Some(nose) = (0..line.len().saturating_sub(1)).min_by(|&a, &b| h[line[a]].total_cmp(&h[line[b]])) else { return false };
    // (The first such pause below the nose: the next one down is the crease above the chin.)
    let pause = (nose + 1..line.len().saturating_sub(1)).find(|&i| h[line[i]] > h[line[i - 1]] && h[line[i]] >= h[line[i + 1]]);
    let Some(pause) = pause else { return false };
    {
        let tip = line[pause];
        let hp = h[tip];
        let c = pos0[tip];
        if std::env::var_os("ZR_DISC").is_none() {
            // The block: strips across the plane through the quad below and right of the
            // mouth point (the edge down the centre line is the strip's edge, so the block
            // reaches a row farther down than up), an even count across so the walk crosses
            // the plane at its middle, half as many rows; narrowed while a strip meets a pole.
            let adj = rect::Adj::new(quads, np);
            let on_plane_v = |v: u32| (v as usize) < np && on_plane(v as usize);
            let mut nbs: Vec<u32> = Vec::new();
            for &f in adj.quads_at(tip as u32) {
                let q = quads[f as usize];
                for k in 0..4 {
                    if q[k] == tip as u32 {
                        for u in [q[(k + 1) % 4], q[(k + 3) % 4]] {
                            if on_plane_v(u) && !nbs.contains(&u) {
                                nbs.push(u);
                            }
                        }
                    }
                }
            }
            let below = nbs.into_iter().min_by(|&x, &y| pos0[x as usize].y.total_cmp(&pos0[y as usize].y).then(x.cmp(&y)));
            let seed = below.and_then(|u| {
                let centre = |f: u32| quads[f as usize].iter().map(|&w| pos0[w as usize]).sum::<Vec3>() * 0.25;
                adj.quads_on(tip as u32, u).iter().copied().max_by(|&x, &y| plane.distance(centre(x)).total_cmp(&plane.distance(centre(y))).then(y.cmp(&x))).map(|f| (f as usize, (tip as u32, u)))
            });
            let Some((seed, u_edge)) = seed else {
                if debug {
                    eprintln!("    mouth at {c:.3} ({hp:.2}): not on a regular grid");
                }
                return false;
            };
            // `ZR_MOUTHR` quads to each side in the cells at the mouth (the face's are wider
            // than a quad), an even count.
            let (cu, _) = rect::cell_sizes(quads, out_p, seed, u_edge);
            let mut a = ((quad * env("ZR_MOUTHR", 2.5) / cu.max(1e-9)).ceil() as usize).max(1) * 2;
            let mut found: Option<(Vec<usize>, usize, usize, usize, usize)> = None;
            'size: while a >= 2 {
                let b = (a / 2).max(2);
                for (ia, ib) in rect::shifts(a, b, true) {
                    if let Some(block) = rect::grid_rect(&adj, quads, seed, u_edge, a, b, ia, ib) {
                        if block.iter().all(|&f| quads[f].iter().all(|&v| (v as usize) < np)) && rect::rect_border(quads, &block).is_some_and(|(border, _)| border.iter().all(|&v| adj.regular(v))) {
                            found = Some((block, a, b, ia, ib));
                            break 'size;
                        }
                    }
                }
                a -= 2;
            }
            let Some((block, a, b, ia, ib)) = found else {
                if debug {
                    eprintln!("    mouth at {c:.3} ({hp:.2}): no rectangle of the grid round it");
                }
                return false;
            };
            let Some((mut border, mut corners)) = rect::rect_border(quads, &block) else { return false };
            let n = border.len();
            // Turned to start at a corner of a side across the plane (its middle on the plane).
            if !on_plane_v(border[a / 2]) {
                let s = corners[1];
                border.rotate_left(s);
                corners = [0, corners[2] - s, corners[3] - s, n - s];
            }
            if corners[1] != a || !on_plane_v(border[a / 2]) || !on_plane_v(border[a + b + a / 2]) || (0..n).filter(|&m| on_plane_v(border[m])).count() != 2 {
                if debug {
                    eprintln!("    mouth at {c:.3} ({hp:.2}): the block does not straddle the plane");
                }
                return false;
            }
            let mut gone = block.clone();
            gone.sort_unstable();
            for &f in gone.iter().rev() {
                quads.swap_remove(f);
            }
            // Two rings: the lip line and the loop round the lips.
            let k = 2;
            let rings = rect::inset_fill(&border, corners, k, 0.7, None, surface, out_p, quads);
            let last = rings.last().cloned().unwrap_or_else(|| border.clone());
            let from = out_p.len();
            let ids = rect::cap_dims(&last, a, false, out_p, quads);
            rect::dome_cap(&last, from, None, surface, out_p);
            // Its own mirror image exactly: ring point m and point a − m, grid column i and
            // column a − i.
            let pair = |out_p: &mut [Vec3], u: u32, v: u32| {
                if u == v {
                    out_p[u as usize] = plane.project(out_p[u as usize]);
                } else if plane.distance(out_p[u as usize]) > 0.0 {
                    out_p[v as usize] = plane.reflect(out_p[u as usize]);
                }
            };
            for ring in &rings {
                for m in 0..n {
                    pair(out_p, ring[m], ring[(a + n - m) % n]);
                }
            }
            for row in &ids {
                for i in 0..=a {
                    pair(out_p, row[i], row[a - i]);
                }
            }
            if debug {
                eprintln!("    mouth at {c:.3} ({hp:.2}): {a} x {b} block cut (the mouth at column {ia}, row {ib}), {k} rings and a grid");
            }
            return true;
        }
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

    /// An ellipse 3 by 1 in a tilted plane: axes along its long and short sides, the aspect
    /// 3, and the grid's first side (the point nearest the corner) starting on the long
    /// side whichever way round the points go.
    #[test]
    fn row_axes_and_corner_of_a_flat_ellipse() {
        let tilt = glam::Quat::from_axis_angle(Vec3::new(1.0, 2.0, 0.5).normalize(), 0.7);
        for reverse in [false, true] {
            let mut pts: Vec<Vec3> = (0..32).map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 32.0;
                tilt * Vec3::new(3.0 * a.cos(), 0.0, a.sin()) + Vec3::new(5.0, -2.0, 1.0)
            }).collect();
            if reverse {
                pts.reverse();
            }
            let (c, u, v, big, small) = row_axes(&pts);
            assert!(c.distance(Vec3::new(5.0, -2.0, 1.0)) < 1e-3, "centroid {c:?}");
            assert!((big / small - 3.0).abs() < 0.05, "aspect {}", big / small);
            assert!(u.dot(tilt * Vec3::X).abs() > 0.999, "long axis {u:?}");
            assert!(v.dot(tilt * Vec3::Z).abs() > 0.999, "short axis {v:?}");
            let k = corner_start(&pts, c, u, v, big, small);
            let n = pts.len();
            let (x0, x1) = ((pts[k] - c).dot(u).abs(), (pts[(k + n / 4) % n] - c).dot(u).abs());
            let (y0, y1) = ((pts[k] - c).dot(v).abs(), (pts[(k + n / 4) % n] - c).dot(v).abs());
            assert!((x0 - x1).abs() < 0.05 && (y0 - y1).abs() < 0.05, "a quarter of the row from the corner is the next corner: {x0} {x1} {y0} {y1}");
            let mid = pts[(k + n / 8) % n] - c;
            assert!(mid.dot(u).abs() < 0.2 && mid.dot(v).abs() > 0.9, "the first side runs along the long axis: {mid:?}");
        }
    }

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

#[cfg(test)]
mod hard_tests {
    use super::*;
    use crate::mesh::SculptMesh;

    /// A small figure: a trunk of radius 0.6 from y = 2 (round top) down to the crotch at
    /// y = 0, two legs of radius 0.28 round x = ±0.32 down to y = −2 with round ends. Rows
    /// 0.1 apart, 36 points round the trunk, 21 round each leg (the trunk's half plus two
    /// crotch points shared by both legs). Its own mirror image in X.
    fn figure() -> SculptMesh {
        let (m, rt, rl, cx) = (36usize, 0.6f32, 0.28f32, 0.32f32);
        let n = m / 2 + 3;
        let mut p: Vec<Vec3> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let snap = |x: f32| if x.abs() < 1e-6 { 0.0 } else { x };
        let add = |p: &mut Vec<Vec3>, q: Vec3| -> u32 {
            p.push(Vec3::new(snap(q.x), q.y, snap(q.z)));
            (p.len() - 1) as u32
        };
        // A band of quads between rows `a` (above) and `b` (below), the same count, the
        // diagonals alternating (one way throughout, the smoothing of the part function
        // would skew round the trunk).
        let band = |idx: &mut Vec<u32>, a: &[u32], b: &[u32], row: usize| {
            let k = a.len();
            for s in 0..k {
                let (a0, a1, b0, b1) = (a[s], a[(s + 1) % k], b[s], b[(s + 1) % k]);
                if (s + row) % 2 == 0 {
                    idx.extend([a0, b0, b1, a0, b1, a1]);
                } else {
                    idx.extend([a0, b0, a1, a1, b0, b1]);
                }
            }
        };
        let trunk_ring = |p: &mut Vec<Vec3>, r: f32, y: f32| -> Vec<u32> {
            (0..m).map(|s| {
                let a = std::f32::consts::TAU * s as f32 / m as f32;
                add(p, Vec3::new(r * a.sin(), y, r * a.cos()))
            }).collect()
        };
        // The round top: a pole, then rings down to the trunk's first.
        let top = add(&mut p, Vec3::new(0.0, 2.0 + rt, 0.0));
        let mut rows: Vec<Vec<u32>> = Vec::new();
        for k in (0..5).rev() {
            let th = k as f32 / 5.0 * std::f32::consts::FRAC_PI_2;
            rows.push(trunk_ring(&mut p, rt * th.cos(), 2.0 + rt * th.sin()));
        }
        for s in 0..m {
            idx.extend([top, rows[0][s], rows[0][(s + 1) % m]]);
        }
        for i in 1..=20 {
            rows.push(trunk_ring(&mut p, rt, 2.0 - 0.1 * i as f32));
        }
        for (i, w) in rows.windows(2).enumerate() {
            band(&mut idx, &w[0], &w[1], i);
        }
        let split = rows.last().unwrap().clone();
        let c_front = add(&mut p, Vec3::new(0.0, -0.03, 0.1));
        let c_back = add(&mut p, Vec3::new(0.0, -0.03, -0.1));
        // Each leg: its first ring is the trunk's half with the crotch points; the rings
        // below keep its points' angles round the leg, eased to even spacing over six rows.
        for right in [true, false] {
            let sign = if right { 1.0 } else { -1.0 };
            let first: Vec<u32> = if right {
                (0..=m / 2).map(|s| split[s]).chain([c_back, c_front]).collect()
            } else {
                (m / 2..=m).map(|s| split[s % m]).chain([c_front, c_back]).collect()
            };
            let centre = Vec3::new(sign * cx, 0.0, 0.0);
            // Angles round the leg's centre, unwrapped to increase from the first point.
            let mut ang: Vec<f32> = first.iter().map(|&v| { let d = p[v as usize] - centre; (sign * d.x).atan2(sign * d.z) }).collect();
            for j in 1..n {
                while ang[j] < ang[j - 1] {
                    ang[j] += std::f32::consts::TAU;
                }
            }
            let at = |j: usize, i: usize, y: f32| -> Vec3 {
                let even = ang[0] + std::f32::consts::TAU * j as f32 / n as f32;
                let b = ang[j] + (even - ang[j]) * (i as f32 / 6.0).min(1.0);
                centre + Vec3::new(sign * rl * b.sin(), y, sign * rl * b.cos())
            };
            let mut lrows: Vec<Vec<u32>> = vec![first];
            for i in 1..=20 {
                lrows.push((0..n).map(|j| add(&mut p, at(j, i, -0.1 * i as f32))).collect());
            }
            for k in 1..5 {
                let th = k as f32 / 5.0 * std::f32::consts::FRAC_PI_2;
                lrows.push((0..n).map(|j| {
                    let q = at(j, 20, -2.0 - rl * th.sin());
                    add(&mut p, centre + (q - centre - Vec3::new(0.0, q.y, 0.0)) * th.cos() + Vec3::new(0.0, q.y, 0.0))
                }).collect());
            }
            for (i, w) in lrows.windows(2).enumerate() {
                band(&mut idx, &w[0], &w[1], i);
            }
            let pole = add(&mut p, centre + Vec3::new(0.0, -2.0 - rl, 0.0));
            let last = lrows.last().unwrap();
            for j in 0..n {
                idx.extend([last[j], pole, last[(j + 1) % n]]);
            }
        }
        SculptMesh::from_raw(p, idx)
    }

    /// Whether an edge of the figure is hard: on one of the creased rings (the trunk at
    /// y = 1 and at the crotch y = 0; the right leg at y = −1, both legs when `both`), or on
    /// the outline of a plate on the back of the trunk (three rows by four columns of its
    /// grid, between y = 1.3 and 1.6 and 160° and 200° round).
    fn is_hard(a: Vec3, b: Vec3, both: bool) -> bool {
        let on = |q: Vec3, y: f32| (q.y - y).abs() < 1e-4;
        let ring = |y: f32| on(a, y) && on(b, y);
        let leg = ring(-1.0) && (both || (a.x > 0.02 && b.x > 0.02));
        let angle = |q: Vec3| q.x.atan2(q.z).to_degrees().rem_euclid(360.0);
        let in_plate = |q: Vec3| (1.3 - 1e-4..=1.6 + 1e-4).contains(&q.y) && (159.0..=201.0).contains(&angle(q));
        let column = |d: f32| (angle(a) - d).abs() < 0.5 && (angle(b) - d).abs() < 0.5;
        let plate = in_plate(a) && in_plate(b) && (ring(1.3) || ring(1.6) || column(160.0) || column(200.0));
        ring(1.0) || ring(0.0) || leg || plate
    }

    /// The output points on the ring at `y` (and on the side `side` of x, if any) form one
    /// closed loop of the output's edges: at least eight points, each with two edges to the
    /// others.
    fn ring_row(p: &[Vec3], quads: &[[u32; 4]], y: f32, side: Option<f32>) -> usize {
        let on: Vec<bool> = p.iter().map(|q| (q.y - y).abs() < 1e-4 && side.is_none_or(|s| q.x * s > 0.02)).collect();
        let count = on.iter().filter(|&&b| b).count();
        let mut deg = vec![0usize; p.len()];
        let mut edges: HashSet<(u32, u32)> = HashSet::default();
        for q in quads {
            for k in 0..4 {
                let (a, b) = (q[k], q[(k + 1) % 4]);
                if a != b && on[a as usize] && on[b as usize] && edges.insert((a.min(b), a.max(b))) {
                    deg[a as usize] += 1;
                    deg[b as usize] += 1;
                }
            }
        }
        assert!(count >= 8, "{count} points on the ring at y = {y}");
        assert_eq!(edges.len(), count, "the points on the ring at y = {y} are not one loop");
        assert!((0..p.len()).all(|v| !on[v] || deg[v] == 2), "a point on the ring at y = {y} has not two ring edges");
        count
    }

    #[test]
    fn creased_rings_of_a_figure_become_its_rows() {
        let m = figure();
        let quad = 0.2;
        let hard = |a: Vec3, b: Vec3, _: u16, _: u16| is_hard(a, b, false);
        let w = Work::new(&m, &hard, None).unwrap();
        let src = (0..w.p.len() as u32).max_by(|&a, &b| w.p[a as usize].y.total_cmp(&w.p[b as usize].y)).unwrap();
        let phi = part_distance(&w, src);
        let bf = BodyFeatures::new(&w, &phi, body_step(quad), Vec::new(), false);
        let kinds: Vec<LoopKind> = bf.loops.iter().map(|l| l.kind).collect();
        assert_eq!(kinds.iter().filter(|&&k| k == LoopKind::Transverse).count(), 3, "{kinds:?}");
        let plates: Vec<&HardLoop> = bf.loops.iter().filter(|l| l.kind == LoopKind::Plate).collect();
        assert_eq!(plates.len(), 1, "{kinds:?}");
        assert_eq!(plates[0].inside.len(), 24, "the plate's inside is three rows of four quads");
        assert!(bf.lines.is_empty());
        // Without the features the figure builds, and no row happens to lie on a crease.
        let (p0, q0) = sweep_body(&w, &phi, quad, src, None, &BodyFeatures::default()).unwrap();
        assert!(q0.len() > 300);
        let stray = p0.iter().filter(|q| (q.y - 1.0).abs() < 1e-4).count();
        assert!(stray < 4, "{stray} points on y = 1 by chance");
        // With them every creased ring is a row of the output, the crotch one included (its
        // ring borders the piece where the trunk splits).
        let (p, q) = sweep_body(&w, &phi, quad, src, None, &bf).unwrap();
        assert!(q.len() > 300);
        let trunk = ring_row(&p, &q, 1.0, None);
        let crotch = ring_row(&p, &q, 0.0, None);
        let leg = ring_row(&p, &q, -1.0, Some(1.0));
        eprintln!("rows of {trunk}, {crotch} and {leg} points on the creases; {} quads", q.len());
        assert!(trunk >= 12 && crotch >= 12 && leg >= 8);
        // The left leg has no crease: no row on it at y = −1.
        assert!(p.iter().filter(|q| (q.y + 1.0).abs() < 1e-4 && q.x < -0.02).count() < 4);
    }

    #[test]
    fn creased_rings_of_a_mirrored_figure_become_mirrored_rows() {
        let m = figure();
        let quad = 0.2;
        let pl = MirrorPlane { axis: Axis::X, at: 0.0 };
        let ms = mirrored_mesh(&m, pl);
        let hard = |a: Vec3, b: Vec3, _: u16, _: u16| is_hard(a, b, true);
        let w = Work::new(&ms, &hard, None).unwrap();
        let top = m.positions.iter().copied().max_by(|a, b| a.y.total_cmp(&b.y)).unwrap();
        let src = (0..w.p.len() as u32).filter(|&v| pl.distance(w.p[v as usize]) == 0.0 && !w.neighbours(v).is_empty()).min_by(|&a, &b| w.p[a as usize].distance(top).total_cmp(&w.p[b as usize].distance(top))).unwrap();
        let phi = part_distance(&w, src);
        let bf = BodyFeatures::new(&w, &phi, body_step(quad), Vec::new(), false);
        let kinds: Vec<LoopKind> = bf.loops.iter().map(|l| l.kind).collect();
        assert_eq!(kinds.iter().filter(|&&k| k == LoopKind::Transverse).count(), 4, "{kinds:?}");
        assert_eq!(kinds.iter().filter(|&&k| k == LoopKind::Plate).count(), 1, "{kinds:?}");
        let (p, q) = sweep_body(&w, &phi, quad, src, Some(pl), &bf).unwrap();
        assert!(q.len() > 300);
        ring_row(&p, &q, 1.0, None);
        ring_row(&p, &q, 0.0, None);
        let right = ring_row(&p, &q, -1.0, Some(1.0));
        let left = ring_row(&p, &q, -1.0, Some(-1.0));
        assert_eq!(right, left);
        // Its own mirror image: every point has its reflection among the points.
        let mut lonely = 0;
        for &a in &p {
            let want = pl.reflect(a);
            if !p.iter().any(|&b| b.distance(want) < 1e-5) {
                lonely += 1;
            }
        }
        assert_eq!(lonely, 0, "{lonely} of {} points without a mirror image", p.len());
    }
}
