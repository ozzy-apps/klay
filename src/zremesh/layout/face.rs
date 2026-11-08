//! The face as a box modeller lays it: loops nested like the reference base meshes. A
//! goggle-shaped mask loop round both eyes crossing the centre line above the brows and at
//! the bridge, eye rings inside it with rows between the inner corners, and below it a loop
//! round the nose and mouth (the nasolabial loop) holding a mouth loop and a nose loop. Each
//! loop is a rectangle of the head's grid cut out, ringed in a cell at a time and capped with
//! a grid, so the only poles are at the rectangles' corners. Rectangles across the centre
//! line are their own mirror images; the eyes are a pair, the left the right's mirror image.

use super::*;
use crate::symmetry::Axis;

/// What every step needs: the mirror plane, the quad size, the working surface.
pub(super) struct Ctx<'a> {
    pub plane: MirrorPlane,
    pub quad: f32,
    pub debug: bool,
    pub surface: &'a dyn Fn(Vec3) -> Vec3,
}

/// The quads `fs` taken out (highest index first, so swap-removes do not move the rest).
fn remove(quads: &mut Vec<[u32; 4]>, fs: &[usize]) {
    let mut gone = fs.to_vec();
    gone.sort_unstable();
    gone.dedup();
    for &f in gone.iter().rev() {
        quads.swap_remove(f);
    }
}

/// `k` rings in from `border` toward its centroid, ring j `step` · j of the way (about a cell
/// a ring, where `rect::inset_fill` shrinks toward the middle), rounded toward a circle by
/// `round` · j / k, lifted toward `tip` as it goes in, put on the surface; each joined to the
/// one before by a band of quads. The rings, innermost last.
#[allow(clippy::too_many_arguments)]
fn rings_in(border: &[u32], k: usize, step: f32, round: f32, tip: Option<Vec3>, surface: &dyn Fn(Vec3) -> Vec3, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) -> Vec<Vec<u32>> {
    let n = border.len();
    let pts: Vec<Vec3> = border.iter().map(|&v| out_p[v as usize]).collect();
    let c = pts.iter().copied().sum::<Vec3>() / n.max(1) as f32;
    let rr = pts.iter().map(|p| p.distance(c)).sum::<f32>() / n.max(1) as f32;
    let mut rings = Vec::with_capacity(k);
    let mut prev = border.to_vec();
    for j in 1..=k {
        let t = (step * j as f32).min(0.9);
        let rounding = round * j as f32 / k.max(1) as f32;
        let s = j as f32 / (k + 1) as f32;
        let lift = tip.map(|tp| (tp - c) * (1.0 - (1.0 - s) * (1.0 - s)) * t);
        let ring: Vec<u32> = (0..n).map(|m| {
            let p = pts[m];
            let circle = c + (p - c).normalize_or_zero() * rr;
            let mut q = p.lerp(circle, rounding).lerp(c, t);
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

/// A rectangle across the centre line, `2 · half` columns (half each side) and `b` rows down
/// from the on-plane point `top`, cut out, ringed `k` times (`step`, `round`, `tip` as in
/// `rings_in`) and capped with a grid whose middle column runs along the plane; exactly its
/// own mirror image. The cap's grid of point ids (rows along the plane, column `half` on it).
#[allow(clippy::too_many_arguments)]
pub(super) fn straddle(c: &Ctx, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, top: u32, half: usize, b: usize, k: usize, round: f32, tip: Option<Vec3>, what: &str) -> Option<Vec<Vec<u32>>> {
    let (pl, quad) = (c.plane, c.quad);
    let np = out_p.len();
    let a = 2 * half;
    if half == 0 || b == 0 {
        return None;
    }
    let on = |v: u32, out_p: &[Vec3]| pl.distance(out_p[v as usize]).abs() < quad * 0.05;
    let adj = rect::Adj::new(quads, np);
    // The edge down the centre line from `top`, and the quad on its + side.
    let mut down: Option<u32> = None;
    for &f in adj.quads_at(top) {
        let q = quads[f as usize];
        for i in 0..4 {
            if q[i] == top {
                for u in [q[(i + 1) % 4], q[(i + 3) % 4]] {
                    if on(u, out_p) && out_p[u as usize].y < out_p[top as usize].y && down.is_none_or(|d| out_p[u as usize].y > out_p[d as usize].y) {
                        down = Some(u);
                    }
                }
            }
        }
    }
    let Some(u) = down else {
        if c.debug {
            eprintln!("    face: {what}: no edge down the centre line from {:.3}", out_p[top as usize]);
        }
        return None;
    };
    let centre = |f: usize, out_p: &[Vec3]| quads[f].iter().map(|&v| out_p[v as usize]).sum::<Vec3>() * 0.25;
    let seed = adj.quads_on(top, u).iter().copied().map(|f| f as usize).max_by(|&x, &y| pl.distance(centre(x, out_p)).total_cmp(&pl.distance(centre(y, out_p))).then(y.cmp(&x)))?;
    let ty = out_p[top as usize].y;
    let mut found: Option<Vec<usize>> = None;
    let mut why: Vec<String> = Vec::new();
    for ib in [0, b - 1] {
        match rect::grid_rect(&adj, quads, seed, (top, u), a, b, half, ib) {
            None => why.push(format!("row {ib}: the walk stops")),
            Some(block) => {
                // Down the face, not up (on average: the rows curve up toward its sides).
                let below = (block.iter().map(|&f| centre(f, out_p).y).sum::<f32>() / block.len() as f32) < ty;
                let irregular = rect::rect_border(quads, &block).map(|(border, _)| border.iter().filter(|&&v| !adj.regular(v)).count());
                if below && irregular == Some(0) {
                    found = Some(block);
                    break;
                }
                why.push(format!("row {ib}: below {below}, border poles {irregular:?}"));
            }
        }
    }
    let Some(block) = found else {
        if c.debug {
            eprintln!("    face: {what}: no {a} x {b} rectangle of the grid below {:.3} ({})", out_p[top as usize], why.join("; "));
        }
        return None;
    };
    let (mut border, mut corners) = rect::rect_border(quads, &block)?;
    let n = border.len();
    if !on(border[a / 2], out_p) {
        let s = corners[1];
        border.rotate_left(s);
        corners = [0, corners[2] - s, corners[3] - s, n - s];
    }
    if corners[1] != a || !on(border[a / 2], out_p) || !on(border[a + b + a / 2], out_p) || (0..n).filter(|&m| on(border[m], out_p)).count() != 2 {
        if c.debug {
            eprintln!("    face: {what}: the rectangle does not straddle the plane");
        }
        return None;
    }
    remove(quads, &block);
    let step = 1.0 / (half.min(b.div_ceil(2)) as f32 + 1.0);
    let rings = rings_in(&border, k, step, round, tip, c.surface, out_p, quads);
    let last = rings.last().cloned().unwrap_or_else(|| border.clone());
    let from = out_p.len();
    let ids = rect::cap_dims(&last, a, false, out_p, quads);
    rect::dome_cap(&last, from, tip, c.surface, out_p);
    // Its own mirror image: ring point m and a − m, grid column i and a − i.
    let pair = |out_p: &mut [Vec3], x: u32, y: u32| {
        if x == y {
            out_p[x as usize] = pl.project(out_p[x as usize]);
        } else if pl.distance(out_p[x as usize]) > 0.0 {
            out_p[y as usize] = pl.reflect(out_p[x as usize]);
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
    if c.debug {
        eprintln!("    face: {what}: {a} x {b} cut below {:.3}, {k} rings and a grid", out_p[top as usize]);
    }
    Some(ids)
}

/// A pair of rectangles, `a` columns (across the body) by `b` rows, the right one round the
/// grid point nearest `at` on the + side and the left its mirror image on the grid; cut out,
/// ringed `k` times toward `tip` and capped, the left built as the right's mirror image
/// quad for quad.
#[allow(clippy::too_many_arguments)]
pub(super) fn paired(c: &Ctx, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, among: &[u32], at: Vec3, a: usize, b: usize, k: usize, round: f32, tip: Option<Vec3>, what: &str) -> bool {
    let (pl, quad) = (c.plane, c.quad);
    let np = out_p.len();
    let adj = rect::Adj::new(quads, np);
    let Some(&v) = among.iter().filter(|&&v| pl.distance(out_p[v as usize]) > quad * 0.3 && adj.regular(v)).min_by(|&&x, &&y| out_p[x as usize].distance(at).total_cmp(&out_p[y as usize].distance(at)).then(x.cmp(&y))) else {
        if c.debug {
            eprintln!("    face: {what}: no grid point near {at:.3}");
        }
        return false;
    };
    let mut along = Vec3::ZERO;
    along[pl.axis as usize] = 1.0;
    let Some((seed, u_edge)) = rect::seed_at(&adj, quads, out_p, v, along, &|q| pl.distance(q)) else { return false };
    let centre = |f: usize, out_p: &[Vec3]| quads[f].iter().map(|&w| out_p[w as usize]).sum::<Vec3>() * 0.25;
    let fits = |block: &[usize], out_p: &[Vec3]| block.iter().all(|&f| quads[f].iter().all(|&w| pl.distance(out_p[w as usize]) > quad * 0.3)) && rect::rect_border(quads, block).is_some_and(|(border, _)| border.iter().all(|&w| adj.regular(w)));
    let mut best: Option<(f32, Vec<usize>)> = None;
    for (ia, ib) in rect::shifts(a, b, false) {
        if let Some(block) = rect::grid_rect(&adj, quads, seed, u_edge, a, b, ia, ib) {
            if fits(&block, out_p) {
                let m = block.iter().map(|&f| centre(f, out_p)).sum::<Vec3>() / block.len() as f32;
                let d = m.distance(at);
                if best.as_ref().is_none_or(|bb| d < bb.0) {
                    best = Some((d, block));
                }
            }
        }
    }
    let Some((_, block)) = best else {
        if c.debug {
            eprintln!("    face: {what}: no {a} x {b} rectangle of the grid round {at:.3}");
        }
        return false;
    };
    // The mirror block: each quad's centre reflected, looked up among the centres.
    let key = |q: Vec3| { let kk = (q / quad.max(1e-6)).floor(); (kk.x as i32, kk.y as i32, kk.z as i32) };
    let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::default();
    for f in 0..quads.len() {
        grid.entry(key(centre(f, out_p))).or_default().push(f);
    }
    let mut block2 = Vec::with_capacity(block.len());
    for &f in &block {
        let m = pl.reflect(centre(f, out_p));
        let kk = key(m);
        let mut hit: Option<(f32, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &g in grid.get(&(kk.0 + dx, kk.1 + dy, kk.2 + dz)).into_iter().flatten() {
                        let d = centre(g, out_p).distance(m);
                        if hit.is_none_or(|h| d < h.0 || (d == h.0 && g < h.1)) {
                            hit = Some((d, g));
                        }
                    }
                }
            }
        }
        match hit {
            Some((d, g)) if d <= quad * 0.3 => block2.push(g),
            _ => {
                if c.debug {
                    eprintln!("    face: {what}: the mirror rectangle is not on the grid");
                }
                return false;
            }
        }
    }
    let Some(((border, corners), (border2, _))) = rect::rect_border(quads, &block).zip(rect::rect_border(quads, &block2)) else { return false };
    let nbd = border.len();
    if border2.len() != nbd {
        return false;
    }
    let img = |w: u32, out_p: &[Vec3]| pl.reflect(out_p[w as usize]);
    let start = (0..nbd).min_by(|&x, &y| out_p[border2[x] as usize].distance(img(border[0], out_p)).total_cmp(&out_p[border2[y] as usize].distance(img(border[0], out_p)))).unwrap_or(0);
    let fwd_ok = out_p[border2[(start + 1) % nbd] as usize].distance(img(border[1], out_p)) < out_p[border2[(start + nbd - 1) % nbd] as usize].distance(img(border[1], out_p));
    let b2: Vec<u32> = (0..nbd).map(|m| if fwd_ok { border2[(start + m) % nbd] } else { border2[(start + nbd - m) % nbd] }).collect();
    if (0..nbd).any(|m| out_p[b2[m] as usize].distance(img(border[m], out_p)) > quad * 0.3) {
        if c.debug {
            eprintln!("    face: {what}: the mirror rectangle's border does not line up");
        }
        return false;
    }
    let both: Vec<usize> = block.iter().chain(block2.iter()).copied().collect();
    remove(quads, &both);
    let (p0, q0) = (out_p.len(), quads.len());
    let step = 1.0 / (corners[1].min(nbd / 2 - corners[1]).div_ceil(2) as f32 + 1.0);
    let rings = rings_in(&border, k, step, round, tip, c.surface, out_p, quads);
    let last = rings.last().cloned().unwrap_or_else(|| border.clone());
    let from = out_p.len();
    rect::cap_dims(&last, corners[1], false, out_p, quads);
    rect::dome_cap(&last, from, tip, c.surface, out_p);
    let mut mir: HashMap<u32, u32> = HashMap::default();
    for m in 0..nbd {
        mir.insert(border[m], b2[m]);
    }
    for w in p0..out_p.len() {
        let q = pl.reflect(out_p[w]);
        out_p.push(q);
        mir.insert(w as u32, (out_p.len() - 1) as u32);
    }
    for f in q0..quads.len() {
        let m = quads[f].map(|w| mir[&w]);
        quads.push([m[1], m[0], m[3], m[2]]);
    }
    if c.debug {
        eprintln!("    face: {what}: {a} x {b} pair cut round {at:.3}, {k} rings and a grid");
    }
    true
}

/// The face's landmarks on the plain head grid: the right eye socket's deepest point, and down
/// the centre line (on-plane points facing front, top to bottom) the nose tip, the mouth and
/// the chin, by their index on the line.
pub(super) struct Marks {
    pub line: Vec<u32>,
    pub eye: Vec3,
    /// How far the socket reaches from its middle across and up and down (the points at
    /// least 0.4 as deep).
    pub eye_r: (f32, f32),
    pub eye_row: usize,
    pub nose: usize,
    pub mouth: usize,
    pub chin: usize,
}

pub(super) fn marks(c: &Ctx, out_p: &[Vec3], quads: &[[u32; 4]], upper: &HashSet<u32>, lower: &HashSet<u32>) -> Result<Marks, String> {
    let (pl, quad) = (c.plane, c.quad);
    let np = out_p.len();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); np];
    let mut nrm = vec![Vec3::ZERO; np];
    for q in quads {
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
    let nrm: Vec<Vec3> = nrm.into_iter().map(|n| n.normalize_or_zero()).collect();
    let fwd = if pl.axis == Axis::Z { 0 } else { 2 };
    let front = |v: usize| nrm[v][fwd] > 0.5;
    let mut line: Vec<u32> = (0..np as u32).filter(|v| (upper.contains(v) || lower.contains(v)) && pl.distance(out_p[*v as usize]).abs() < quad * 0.05 && front(*v as usize)).collect();
    line.sort_by(|&a, &b| out_p[b as usize].y.total_cmp(&out_p[a as usize].y).then(a.cmp(&b)));
    if line.len() < 8 {
        return Err(format!("{} points down the centre line", line.len()));
    }
    let mut gaps: Vec<f32> = line.windows(2).map(|w| out_p[w[0] as usize].distance(out_p[w[1] as usize])).collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    let cell = gaps[gaps.len() / 2].max(1e-6);
    // The smoothing reaches as far on a finer head: its passes by the square of how much
    // finer its cells are than the quad.
    let reach = ((quad / cell).powi(2)).clamp(1.0, 6.0);
    // How far each point sinks below its smoothed neighbourhood, against the average round it,
    // with the smoothing passes scaled by `scale`.
    let dips = |scale: f32| -> Vec<f32> {
        let mut sm = out_p.to_vec();
        for _ in 0..(80.0 * scale) as usize {
            let prev = sm.clone();
            for v in 0..np {
                if !nb[v].is_empty() {
                    sm[v] = prev[v] * 0.5 + nb[v].iter().map(|&u| prev[u as usize]).sum::<Vec3>() / nb[v].len() as f32 * 0.5;
                }
            }
        }
        let h0: Vec<f32> = (0..np).map(|v| -(out_p[v] - sm[v]).dot(nrm[v]) / quad).collect();
        let mut avg = h0.clone();
        for _ in 0..(30.0 * scale) as usize {
            let prev = avg.clone();
            for v in 0..np {
                if !nb[v].is_empty() {
                    avg[v] = prev[v] * 0.5 + nb[v].iter().map(|&u| prev[u as usize]).sum::<f32>() / nb[v].len() as f32 * 0.5;
                }
            }
        }
        (0..np).map(|v| h0[v] - avg[v]).collect()
    };
    // The nose and the mouth down the centre line with the body's smoothing (more smooths the
    // pause at the mouth away); the eye with smoothing reaching as far on a finer head (its
    // passes by the square of how much finer the cells are than the quad).
    let h = dips(1.0);
    let h_eye = if reach > 1.05 { dips(reach) } else { h.clone() };
    let nose = (0..line.len() - 1).min_by(|&a, &b| h[line[a] as usize].total_cmp(&h[line[b] as usize])).ok_or("no nose")?;
    // Down the profile (how far forward each centre-line point stands): the nose's base, the
    // first point below the tip that stands less far than the one after it; the chin, the
    // point standing furthest out below that (past the lips), and down from it while within
    // two cells of it; the mouth a third of the way from the nose's base to the chin (the
    // proportion of a face; `ZR_MOUTHDIP` the deepest dip between them instead).
    let z = |i: usize| out_p[line[i] as usize][fwd];
    let base = (nose + 1..line.len() - 1).find(|&i| z(i) <= z(i + 1)).ok_or("no base under the nose")?;
    let lips_end = (base + 1..line.len()).find(|&i| out_p[line[i] as usize].y < out_p[line[base] as usize].y - (out_p[line[base] as usize].y - out_p[line[line.len() - 1] as usize].y) * 0.4).unwrap_or(base + 1);
    let chin_tip = (lips_end..line.len()).max_by(|&a, &b| z(a).total_cmp(&z(b)).then(b.cmp(&a))).ok_or("no chin")?;
    let chin = (chin_tip..line.len()).take_while(|&i| z(i) > z(chin_tip) - cell * 2.0).last().unwrap_or(chin_tip);
    let mouth = if std::env::var_os("ZR_MOUTHDIP").is_some() {
        (nose + 2..chin.saturating_sub(1)).max_by(|&a, &b| h[line[a] as usize].total_cmp(&h[line[b] as usize]).then(b.cmp(&a))).ok_or("no room for a mouth between nose and chin")?
    } else {
        let my = out_p[line[base] as usize].y - (out_p[line[base] as usize].y - out_p[line[chin] as usize].y) / 3.0;
        (base + 1..chin).min_by(|&a, &b| (out_p[line[a] as usize].y - my).abs().total_cmp(&(out_p[line[b] as usize].y - my).abs())).ok_or("no room for a mouth between nose and chin")?
    };
    let ny = out_p[line[nose] as usize].y;
    // The eye: the deepest dip above the nose, within four cells of the centre line (the two
    // two to eight cells apart) and no more than three cells behind the nose tip (not the
    // hollow in front of an ear).
    let nz = out_p[line[nose] as usize][fwd];
    let eye = (0..np).filter(|&v| {
        let d = pl.distance(out_p[v]);
        upper.contains(&(v as u32)) && d > cell * 0.5 && d < cell * 4.5 && out_p[v][fwd] > nz - cell * 3.0 && front(v) && out_p[v].y > ny
    }).max_by(|&a, &b| h_eye[a].total_cmp(&h_eye[b]).then(b.cmp(&a))).ok_or("no eye socket near the centre line")?;
    // The socket's middle: the dips round the deepest point (within three cells, at least
    // half as deep), weighted by depth; the deepest point is often the inner corner.
    let deep = h_eye[eye];
    let (mut sw, mut sp) = (0.0f64, glam::DVec3::ZERO);
    for v in 0..np {
        if upper.contains(&(v as u32)) && pl.distance(out_p[v]) > 0.0 && out_p[v].distance(out_p[eye]) < cell * 3.0 && h_eye[v] > deep * 0.5 {
            let wgt = h_eye[v] as f64;
            sw += wgt;
            sp += out_p[v].as_dvec3() * wgt;
        }
    }
    let eye_dip = if sw > 0.0 { (sp / sw).as_vec3() } else { out_p[eye] };
    // The face's half width at that height: out along the head to where the surface turns
    // to the side. The eye's middle across is `ZR_EYEX` (0.45) of it, its half width
    // `ZR_EYEW` (0.2): the hollow's deepest part is the inner corner, too near the nose.
    let envf = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
    let half_w = (0..np).filter(|&v| upper.contains(&(v as u32)) && (out_p[v].y - eye_dip.y).abs() < cell * 0.75 && pl.distance(out_p[v]) > 0.0 && nrm[v][fwd] > 0.25).map(|v| pl.distance(out_p[v])).fold(0.0f32, f32::max);
    let eye_at = if half_w > cell * 3.0 && std::env::var_os("ZR_EYEDIP").is_none() {
        let target = pl.distance(eye_dip).max(half_w * envf("ZR_EYEX", 0.45));
        // The grid point on that row nearest that far out (its position on the surface).
        (0..np).filter(|&v| upper.contains(&(v as u32)) && (out_p[v].y - eye_dip.y).abs() < cell * 0.75 && pl.distance(out_p[v]) > 0.0).min_by(|&a, &b| (pl.distance(out_p[a]) - target).abs().total_cmp(&(pl.distance(out_p[b]) - target).abs())).map_or(eye_dip, |v| out_p[v])
    } else {
        eye_dip
    };
    let eye_w = if half_w > cell * 3.0 { half_w * envf("ZR_EYEW", 0.2) } else { 0.0 };
    let socket: Vec<usize> = (0..np).filter(|&v| upper.contains(&(v as u32)) && pl.distance(out_p[v]) > 0.0 && out_p[v].distance(eye_at) < cell * 5.0 && h_eye[v] > deep * 0.4).collect();
    let eye_r = (
        socket.iter().map(|&v| (pl.distance(out_p[v]) - pl.distance(eye_at)).abs()).fold(cell, f32::max).max(eye_w),
        socket.iter().map(|&v| (out_p[v].y - eye_at.y).abs()).fold(cell, f32::max).min(eye_w.max(cell) * 0.8),
    );
    let ey = eye_at.y;
    let eye_row = (0..line.len()).min_by(|&a, &b| (out_p[line[a] as usize].y - ey).abs().total_cmp(&(out_p[line[b] as usize].y - ey).abs())).unwrap_or(0);
    // The eye pair must sit above the nose, the nose below their middle.
    if eye_row >= nose || pl.distance(eye_at) > (ey - ny).abs() * 3.0 + quad * 4.0 {
        return Err(format!("no eye pair over the nose (eye at {:.3}, nose at {:.3})", eye_at, out_p[line[nose] as usize]));
    }
    Ok(Marks { line, eye: eye_at, eye_r, eye_row, nose, mouth, chin })
}

/// The nest, laid out as the reference base meshes are (quadremesh-ref 3.jpg, 4.jpg, a.webp),
/// top to bottom on the plain head grid:
/// - the mask (the goggle loop) from above the brows to the row above the nose tip, as wide
///   as the eyes and their sockets and two columns more;
/// - the eyes filling its two halves, a column of bridge between them and a row clear of its
///   top and bottom, ringed up to three times toward the socket;
/// - the nose, a narrow loop from the mask down past the nostrils;
/// - the mouth, the largest nest: rings from the row under the nose to the chin, as wide as
///   the mask less a column, up to three, round a wide grid for the lips.
/// False (and nothing changed) when the landmarks are not found or the mask or the eyes do
/// not fit; the nose or the mouth that do not fit are left out with a log line.
pub(super) fn nest(c: &Ctx, out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, upper: &HashSet<u32>, lower: &HashSet<u32>, orient: &dyn Fn(&mut [[u32; 4]], &[Vec3])) -> bool {
    let pl = c.plane;
    let m = match marks(c, out_p, quads, upper, lower) {
        Ok(m) => m,
        Err(e) => {
            if c.debug {
                eprintln!("  face: {e}; the eyes and the mouth as before");
            }
            return false;
        }
    };
    let line = m.line.clone();
    let er = m.eye_row;
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(d);
    let cell = out_p[line[er] as usize].distance(out_p[line[(er + 1).min(line.len() - 1)] as usize]).max(1e-6);
    let col_eye = ((pl.distance(m.eye) / cell).round() as usize).max(1);
    let (rx, ry) = (((m.eye_r.0 / cell).round() as usize).clamp(1, 5), ((m.eye_r.1 / cell).round() as usize).clamp(1, 4));
    if c.debug {
        eprintln!("  face: eye at {:.3} (column {col_eye}, row {er}, reaching {rx} across, {ry} up and down), nose row {}, mouth row {}, chin row {}, cell {:.4}", m.eye, m.nose, m.mouth, m.chin, cell);
        eprintln!("  face: centre line heights {:?}", line.iter().map(|&v| (out_p[v as usize].y * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    }
    // The mask: from above the brows to the row above the nose tip.
    let mask_top = er.saturating_sub(env("ZR_MASKUP", ry + 2));
    let mask_bot = m.nose.saturating_sub(env("ZR_MASKNOSE", 1)).max(er + ry + 1);
    let mask_half = col_eye + rx + env("ZR_MASKW", 2);
    if mask_bot <= mask_top + 3 || mask_bot >= m.mouth {
        if c.debug {
            eprintln!("  face: no room for the mask (rows {mask_top} to {mask_bot}, the mouth at {}); as before", m.mouth);
        }
        return false;
    }
    let (keep_p, keep_q) = (out_p.clone(), quads.clone());
    let undo = |out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, why: &str| {
        *out_p = keep_p.clone();
        *quads = keep_q.clone();
        if c.debug {
            eprintln!("  face: {why}; the eyes and the mouth as before");
        }
    };
    let Some(mask) = straddle(c, out_p, quads, line[mask_top], mask_half, mask_bot - mask_top, 1, 0.5, None, "mask") else {
        undo(out_p, quads, "no mask");
        return false;
    };
    orient(quads, out_p);
    // The eyes filling its halves: past `ZR_BRIDGE` (1) columns each side of the plane to a
    // column short of the side, a row clear of the top and the bottom.
    let rows = mask.len().saturating_sub(1);
    let bridge = env("ZR_BRIDGE", 1);
    let among: Vec<u32> = mask.iter().flatten().copied().collect();
    let mut eyes = false;
    'fit: for shrink in 0..3usize {
        // As wide as the socket, at most the half less the bridge and a column; two thirds as
        // tall, a row clear of the mask's top and bottom.
        let a_e = (2 * rx + 1).min(mask_half.saturating_sub(bridge + 1)).saturating_sub(shrink).max(2);
        let b_e = ((2 * a_e).div_ceil(3)).min(rows.saturating_sub(2)).saturating_sub(shrink / 2).max(1);
        // Its middle: the mask's grid point that far out on the + side, half way down.
        let k = (a_e.min(b_e) / 2).clamp(1, 3);
        // (On the surface, not lifted toward the socket: the lift dug channels by the nose.)
        if paired(c, out_p, quads, &among, m.eye, a_e, b_e, k, 1.0, None, "eyes") {
            eyes = true;
            break 'fit;
        }
    }
    if !eyes {
        undo(out_p, quads, "the eyes do not fit in the mask");
        return false;
    }
    orient(quads, out_p);
    // The nose: from the mask's bottom down to two rows above the mouth (past the nostrils).
    // (Two columns each side: the nostrils' width.)
    let nose_half = env("ZR_NOSEW", 2);
    let nose_bot = m.mouth.saturating_sub(2).max(m.nose + 1);
    if nose_bot > mask_bot {
        if straddle(c, out_p, quads, line[mask_bot], nose_half, nose_bot - mask_bot, 1, 0.8, None, "nose").is_some() {
            orient(quads, out_p);
        }
    }
    // The mouth: from the row under the nose to the chin, out to a column past the eyes' middles.
    let top = nose_bot + 1;
    let rows_m = m.chin.saturating_sub(top);
    let mouth_half = env("ZR_MOUTHW", col_eye + 1).max(2);
    let k_m = (rows_m.saturating_sub(1) / 2).clamp(1, env("ZR_MOUTHK", 3));
    if rows_m >= 2 {
        // (Rounded all the way: the mouth's loops are ovals.)
        if straddle(c, out_p, quads, line[top], mouth_half, rows_m, k_m, 1.0, None, "mouth").is_some() {
            orient(quads, out_p);
        }
    } else if c.debug {
        eprintln!("  face: mouth: no room between the nose (row {nose_bot}) and the chin (row {})", m.chin);
    }
    true
}
