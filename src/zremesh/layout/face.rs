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
    // The chin: the last point below the nose before the surface turns under the jaw.
    // (Past the nose's underside first, which faces down too.)
    let lip = (nose + 1..line.len()).find(|&i| nrm[line[i] as usize].y > -0.3).ok_or("no lip below the nose")?;
    let chin = (lip..line.len()).take_while(|&i| nrm[line[i] as usize].y > -0.85).last().ok_or("no chin below the nose")?;
    // The mouth: the deepest dip between the nose and the chin, two rows clear of each (the
    // first pause below the nose is the dip under it, on a finer head).
    let mouth = if std::env::var_os("ZR_MOUTHPAUSE").is_some() {
        (nose + 1..line.len() - 1).find(|&i| h[line[i] as usize] > h[line[i - 1] as usize] && h[line[i] as usize] >= h[line[i + 1] as usize]).ok_or("no mouth below the nose")?
    } else {
        (nose + 2..chin.saturating_sub(1)).max_by(|&a, &b| h[line[a] as usize].total_cmp(&h[line[b] as usize]).then(b.cmp(&a))).ok_or("no room for a mouth between nose and chin")?
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
    let eye_at = if sw > 0.0 { (sp / sw).as_vec3() } else { out_p[eye] };
    let socket: Vec<usize> = (0..np).filter(|&v| upper.contains(&(v as u32)) && pl.distance(out_p[v]) > 0.0 && out_p[v].distance(eye_at) < cell * 5.0 && h_eye[v] > deep * 0.4).collect();
    let eye_r = (
        socket.iter().map(|&v| (pl.distance(out_p[v]) - pl.distance(eye_at)).abs()).fold(cell, f32::max),
        socket.iter().map(|&v| (out_p[v].y - eye_at.y).abs()).fold(cell, f32::max),
    );
    let ey = eye_at.y;
    let eye_row = (0..line.len()).min_by(|&a, &b| (out_p[line[a] as usize].y - ey).abs().total_cmp(&(out_p[line[b] as usize].y - ey).abs())).unwrap_or(0);
    // The eye pair must sit above the nose, the nose below their middle.
    if eye_row >= nose || pl.distance(eye_at) > (ey - ny).abs() * 3.0 + quad * 4.0 {
        return Err(format!("no eye pair over the nose (eye at {:.3}, nose at {:.3})", eye_at, out_p[line[nose] as usize]));
    }
    Ok(Marks { line, eye: eye_at, eye_r, eye_row, nose, mouth, chin })
}

/// The nest, on the plain head grid (rows round the head, the centre line a grid line):
/// the mask from two rows above the eyes to three below, as wide as the eyes and two
/// columns more; the eyes as a pair inside its grid; the loop round nose and mouth from the
/// row under the mask to the row above the chin; the nose and the mouth inside its grid.
/// False (and nothing changed) when the landmarks are not found or the mask does not fit;
/// later steps that do not fit are left out with a log line.
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
    if er < 2 || er + 5 >= m.chin {
        if c.debug {
            eprintln!("  face: the eyes (row {er}) too near the top or the chin (row {}); as before", m.chin);
        }
        return false;
    }
    let cell = out_p[line[er] as usize].distance(out_p[line[er + 1] as usize]).max(1e-6);
    let col_eye = ((pl.distance(m.eye) / cell).round() as usize).max(1);
    // The socket's reach in cells, and the eye block over it (`ZR_EYEBLOCK` cells across: the
    // socket's width by default).
    let (rx, ry) = (((m.eye_r.0 / cell).round() as usize).clamp(1, 4), ((m.eye_r.1 / cell).round() as usize).clamp(1, 3));
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(d);
    if c.debug {
        eprintln!("  face: eye at {:.3} (column {col_eye}, row {er}), nose row {}, mouth row {}, chin row {}, cell {:.4}", m.eye, m.nose, m.mouth, m.chin, cell);
        eprintln!("  face: centre line heights {:?}", line.iter().map(|&v| (out_p[v as usize].y * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    }
    // Undone (the eyes and mouth then as before) when the eyes do not fit in the mask.
    let (keep_p, keep_q) = (out_p.clone(), quads.clone());
    // The mask (the goggle loop).
    let mask_half = col_eye + rx + env("ZR_MASKW", 1);
    // Down to two rows above the nose tip at most, so the nose is inside the loop below.
    let above = env("ZR_MASKUP", ry + 1).min(er);
    let below = env("ZR_MASKDOWN", ry + 2).min(m.nose.saturating_sub(er + 2)).max(1);
    let Some(mask) = straddle(c, out_p, quads, line[er - above], mask_half, above + below, 1, 0.6, None, "mask") else {
        return false;
    };
    orient(quads, out_p);
    // The eyes inside it: as many rows as the mask's grid leaves one above and below, three
    // columns (two if they do not fit).
    let among: Vec<u32> = mask.iter().flatten().copied().collect();
    let rows = mask.len().saturating_sub(1);
    let a0 = env("ZR_EYEBLOCK", 2 * rx + 1).clamp(2, 2 * mask_half);
    let b_e = (2 * ry + 1).min(rows.saturating_sub(2)).max(1);
    if c.debug {
        eprintln!("  face: socket reaches {rx} cells across, {ry} up and down; mask half {mask_half}, {above} rows up, {below} down");
    }
    let mut eyes = false;
    for a_e in (2..=a0).rev() {
        let k = if a_e >= 5 { 3 } else if a_e >= 3 { 2 } else { 1 };
        if paired(c, out_p, quads, &among, m.eye, a_e, b_e, k, 1.0, Some(m.eye), "eyes") {
            eyes = true;
            break;
        }
    }
    if !eyes {
        *out_p = keep_p;
        *quads = keep_q;
        if c.debug {
            eprintln!("  face: the eyes do not fit in the mask; the eyes and the mouth as before");
        }
        return false;
    }
    orient(quads, out_p);
    // The loop round the nose and the mouth: from the row under the mask to the row above
    // the chin.
    let top = er + below + 1;
    let bottom = m.chin.saturating_sub(1);
    if bottom <= top + 2 {
        if c.debug {
            eprintln!("  face: no room for the nose and mouth loop (rows {top} to {bottom})");
        }
        return true;
    }
    let muzzle_half = (col_eye + env("ZR_MUZZLEW", 1)).min(mask_half);
    let Some(muzzle) = straddle(c, out_p, quads, line[top], muzzle_half, bottom - top, env("ZR_MUZZLEK", 1), 0.6, None, "nose and mouth loop") else {
        return true;
    };
    orient(quads, out_p);
    // Inside it: its centre column top to bottom.
    let mut col: Vec<u32> = muzzle.iter().map(|r| r[muzzle_half]).collect();
    if out_p[col[0] as usize].y < out_p[col[col.len() - 1] as usize].y {
        col.reverse();
    }
    let my = out_p[line[m.mouth] as usize].y;
    let jm = (1..col.len().saturating_sub(1)).min_by(|&a, &b| (out_p[col[a] as usize].y - my).abs().total_cmp(&(out_p[col[b] as usize].y - my).abs())).unwrap_or(1);
    // The mouth: a row above it to a row below, two columns in from the loop's sides.
    // (Two rows when the loop has them below: from a row above the mouth to a row below.)
    let mouth_top = jm.saturating_sub(1).max(1).min(col.len().saturating_sub(4).max(1));
    let mouth_half = muzzle_half.saturating_sub(1).max(1);
    let mouth_rows = 2usize.min(col.len().saturating_sub(1 + mouth_top + 1)).max(1);
    // The nose first, centred on its tip: two rows above it (not above the loop's second
    // row) to a row clear of the mouth.
    let nose_tip = out_p[line[m.nose] as usize];
    let jn = (1..col.len().saturating_sub(1)).min_by(|&a, &b| (out_p[col[a] as usize].y - nose_tip.y).abs().total_cmp(&(out_p[col[b] as usize].y - nose_tip.y).abs())).unwrap_or(1);
    let nose_top = jn.saturating_sub(1);
    let nose_rows = (jn + 2).min(mouth_top.saturating_sub(1)).saturating_sub(nose_top);
    if nose_rows >= 2 {
        // (On the surface, not lifted toward the tip: the block is round and below it, and a
        // lift there raises a second tip.)
        if straddle(c, out_p, quads, col[nose_top], 1, nose_rows, 1, 0.8, None, "nose").is_some() {
            orient(quads, out_p);
        }
    } else if c.debug {
        eprintln!("    face: nose: no room between the loop's top and the mouth");
    }
    // (Two rings only in a block two rows tall: one row squeezes them into slivers.)
    let mouth_k = if mouth_half >= 2 && mouth_rows >= 2 { 2 } else { 1 };
    if straddle(c, out_p, quads, col[mouth_top], mouth_half, mouth_rows, mouth_k, 0.9, None, "mouth").is_some() {
        orient(quads, out_p);
    }
    true
}
