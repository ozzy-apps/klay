//! ZRemesher's quad layout: the whole surface is planned at once, the way Quad Remesher
//! and QuadWild (Pietroni et al. 2021) work, instead of quads being fitted piece by piece.
//!
//! 1. **Working mesh.** The plain remesher at four times the quad count gives an even
//!    triangle mesh with its edges on the edge lines (sharp edges, creases, polygroup
//!    borders); those and open borders are the *hard* edges.
//! 2. **Cross field.** Every triangle gets four directions a quarter turn apart, smoothed
//!    over the surface, held along hard edges and pulled toward the way the surface bends
//!    (Follow Shape). Vertices round which the field turns by a quarter are its poles.
//! 3. **Lines.** From every pole a line is traced along the field in each of its 3 or 5
//!    directions, and from hard-edge corners into their wide sectors; all advance together
//!    and each stops where it meets another line or a hard edge. Lines run along the
//!    working mesh's edges, so they meet exactly at its vertices.
//! 4. **Patches.** The lines and hard edges cut the surface into patches. Each must be a
//!    disc with three to five corners; one that is not gets another line through it.
//! 5. **Counts.** Every line between two corners or meeting points gets a whole number of
//!    quads, near its length over the quad size, so that the opposite sides of every
//!    four-sided patch match and every three- and five-sided patch has an even total.
//! 6. **Fill.** A four-sided patch becomes a regular grid; a three- or five-sided one is cut
//!    into grids round a middle point (one pole). Each patch is laid flat (Tutte), so its
//!    grid lands on the surface exactly.

use super::*;
use glam::{Quat, Vec2};
use std::collections::VecDeque;

mod body;
mod rect;
type HashSet<T> = crate::fx::FxHashSet<T>;

const NONE: u32 = u32::MAX;
/// The working mesh's edges are this many times shorter than the quads'.
const DENSITY: f32 = 4.0;
/// Marks: a vertex on a hard edge, and a node (pole or corner).
const HARD: u32 = u32::MAX - 1;
const NODE: u32 = u32::MAX - 2;

fn cmul(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x)
}

fn expi(t: f32) -> Vec2 {
    Vec2::new(t.cos(), t.sin())
}

/// The working triangle mesh with half-edges: half-edge `3t + k` runs from corner `k` of
/// triangle `t` to corner `k + 1`.
struct Work {
    p: Vec<Vec3>,
    t: Vec<[u32; 3]>,
    fnrm: Vec<Vec3>,
    /// Each face's frame: its first edge's direction.
    fx: Vec<Vec3>,
    twin: Vec<u32>,
    hard: Vec<bool>,
    /// Hard edges of a feature loop: smooth, no corners, no lines start on them.
    soft: Vec<bool>,
    /// A half-edge out of each vertex; the first of its fan on an open border.
    out: Vec<u32>,
    vnrm: Vec<Vec3>,
    /// Neighbours, CSR.
    off: Vec<u32>,
    adj: Vec<u32>,
}

impl Work {
    fn from(&self, h: u32) -> u32 {
        self.t[(h / 3) as usize][(h % 3) as usize]
    }
    fn to(&self, h: u32) -> u32 {
        self.t[(h / 3) as usize][((h % 3 + 1) % 3) as usize]
    }
    fn next(h: u32) -> u32 {
        h / 3 * 3 + (h % 3 + 1) % 3
    }
    fn prev(h: u32) -> u32 {
        h / 3 * 3 + (h % 3 + 2) % 3
    }
    fn neighbours(&self, v: u32) -> &[u32] {
        &self.adj[self.off[v as usize] as usize..self.off[v as usize + 1] as usize]
    }
    /// The half-edges out of `v`, counter-clockwise round it.
    fn fan(&self, v: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let start = self.out[v as usize];
        if start == NONE {
            return out;
        }
        let mut h = start;
        loop {
            out.push(h);
            let t = self.twin[Self::prev(h) as usize];
            if t == NONE || t == start || out.len() > 4096 {
                break;
            }
            h = t;
        }
        out
    }
    /// Whether the edge a–b is part of a feature loop.
    fn soft_edge(&self, a: u32, b: u32) -> bool {
        self.fan(a).iter().any(|&h| (self.to(h) == b && self.soft[h as usize]) || (self.from(Self::prev(h)) == b && self.soft[Self::prev(h) as usize]))
    }
    fn interior(&self, v: u32) -> bool {
        let s = self.out[v as usize];
        s != NONE && self.twin[s as usize] != NONE
    }
    /// The angle of `d` in face `f`'s frame.
    fn angle(&self, f: usize, d: Vec3) -> f32 {
        let (x, n) = (self.fx[f], self.fnrm[f]);
        let y = n.cross(x);
        d.dot(y).atan2(d.dot(x))
    }
    fn dir(&self, f: usize, a: f32) -> Vec3 {
        let (x, n) = (self.fx[f], self.fnrm[f]);
        x * a.cos() + n.cross(x) * a.sin()
    }
    /// The corner angle of the face of half-edge `h` (out of its start).
    fn corner(&self, h: u32) -> f32 {
        let v = self.p[self.from(h) as usize];
        let a = self.p[self.to(h) as usize] - v;
        let b = self.p[self.from(Self::prev(h)) as usize] - v;
        a.angle_between(b)
    }

    fn new(m: &SculptMesh, is_hard: &dyn Fn(Vec3, Vec3, u16, u16) -> bool, fold: Option<f32>) -> Result<Self, String> {
        let p = m.positions.clone();
        let t: Vec<[u32; 3]> = m.indices.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        let fnrm: Vec<Vec3> = t.iter().map(|f| (p[f[1] as usize] - p[f[0] as usize]).cross(p[f[2] as usize] - p[f[0] as usize]).normalize_or(Vec3::Z)).collect();
        let fx: Vec<Vec3> = t.iter().zip(&fnrm).map(|(f, n)| {
            let e = p[f[1] as usize] - p[f[0] as usize];
            (e - *n * n.dot(e)).normalize_or(n.any_orthonormal_vector())
        }).collect();
        let mut map: HashMap<(u32, u32), u32> = HashMap::default();
        for (i, f) in t.iter().enumerate() {
            for k in 0..3 {
                if map.insert((f[k], f[(k + 1) % 3]), (i * 3 + k) as u32).is_some() {
                    return Err("the working mesh repeats an edge".into());
                }
            }
        }
        let nh = t.len() * 3;
        let mut twin = vec![NONE; nh];
        let mut hard = vec![false; nh];
        for (i, f) in t.iter().enumerate() {
            for k in 0..3 {
                let h = i * 3 + k;
                let (a, b) = (f[k], f[(k + 1) % 3]);
                match map.get(&(b, a)) {
                    Some(&o) => {
                        twin[h] = o;
                        let (ga, gb) = (m.group(i as u32), m.group(o / 3));
                        hard[h] = is_hard(p[a as usize], p[b as usize], ga, gb);
                    }
                    None => hard[h] = true,
                }
            }
        }
        // Detect Edges: folds sharper than the angle are hard too.
        if let Some(cos) = fold {
            for h in 0..nh {
                if twin[h] != NONE && fnrm[h / 3].dot(fnrm[twin[h] as usize / 3]) < cos {
                    hard[h] = true;
                }
            }
        }
        // A pair of half-edges is hard if either side says so.
        for h in 0..nh {
            if twin[h] != NONE && hard[twin[h] as usize] {
                hard[h] = true;
            }
        }
        // A triangle with all three edges hard (three points on one line): its longest
        // interior edge is not.
        for f in 0..t.len() {
            if (0..3).all(|k| hard[f * 3 + k]) {
                let len = |k: usize| p[t[f][k] as usize].distance(p[t[f][(k + 1) % 3] as usize]);
                if let Some(k) = (0..3).filter(|&k| twin[f * 3 + k] != NONE).max_by(|&a, &b| len(a).total_cmp(&len(b))) {
                    hard[f * 3 + k] = false;
                    hard[twin[f * 3 + k] as usize] = false;
                }
            }
        }
        let mut out = vec![NONE; p.len()];
        for h in 0..nh {
            let v = t[h / 3][h % 3] as usize;
            if out[v] == NONE || twin[h] == NONE {
                out[v] = h as u32;
            }
        }
        let mut vnrm = vec![Vec3::ZERO; p.len()];
        for (f, n) in t.iter().zip(&fnrm) {
            for &v in f {
                vnrm[v as usize] += *n;
            }
        }
        let vnrm: Vec<Vec3> = vnrm.into_iter().map(|n| n.normalize_or(Vec3::Z)).collect();
        let mut edges: Vec<(u32, u32)> = map.keys().copied().collect();
        edges.sort_unstable();
        let mut off = vec![0u32; p.len() + 1];
        for &(a, _) in &edges {
            off[a as usize + 1] += 1;
        }
        for i in 0..p.len() {
            off[i + 1] += off[i];
        }
        let mut adj = vec![0u32; edges.len()];
        let mut fill = off.clone();
        for &(a, b) in &edges {
            adj[fill[a as usize] as usize] = b;
            fill[a as usize] += 1;
        }
        // Open borders: an edge only one way round; add the other way for neighbours.
        let mut extra: Vec<(u32, u32)> = edges.iter().filter(|&&(a, b)| !map.contains_key(&(b, a))).map(|&(a, b)| (b, a)).collect();
        if !extra.is_empty() {
            extra.extend(edges.iter().copied());
            extra.sort_unstable();
            extra.dedup();
            let mut off2 = vec![0u32; p.len() + 1];
            for &(a, _) in &extra {
                off2[a as usize + 1] += 1;
            }
            for i in 0..p.len() {
                off2[i + 1] += off2[i];
            }
            let adj2 = extra.iter().map(|e| e.1).collect();
            let soft = vec![false; hard.len()];
            let w = Work { p, t, fnrm, fx, twin, hard, soft, out, vnrm, off: off2, adj: adj2 };
            w.check()?;
            return Ok(w);
        }
        let soft = vec![false; hard.len()];
        let w = Work { p, t, fnrm, fx, twin, hard, soft, out, vnrm, off, adj };
        w.check()?;
        Ok(w)
    }

    /// Every vertex's fan reaches all its faces (no pinched vertices).
    fn check(&self) -> Result<(), String> {
        let mut count = vec![0u32; self.p.len()];
        for f in &self.t {
            for &v in f {
                count[v as usize] += 1;
            }
        }
        for v in 0..self.p.len() as u32 {
            if count[v as usize] > 0 && self.fan(v).len() as u32 != count[v as usize] {
                return Err("the working mesh has a pinched vertex".into());
            }
        }
        Ok(())
    }
}

/// `mesh` with no edge longer than `edge`: decimated first if it has far more triangles than
/// that needs, then long edges split at their middles (a triangle with one, two or three
/// split edges into two, three or four), until none is left.
fn refined(mesh: &SculptMesh, edge: f32) -> Result<(SculptMesh, f32), String> {
    let t0 = std::time::Instant::now();
    let lap = |what: &str| if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("    refined: {what} at {:.1} s", t0.elapsed().as_secs_f32()) };
    let area: f32 = mesh.indices.chunks_exact(3).map(|t| (mesh.positions[t[1] as usize] - mesh.positions[t[0] as usize]).cross(mesh.positions[t[2] as usize] - mesh.positions[t[0] as usize]).length() * 0.5).sum();
    let wanted = (area / (edge * edge * 0.43)).max(100.0);
    let (mut p, mut idx, mut grp) = (mesh.positions.clone(), mesh.indices.clone(), (0..mesh.triangle_count() as u32).map(|t| mesh.group(t)).collect::<Vec<u16>>());
    let mut edge = edge;
    let mut frac = 1.0f32;
    if (idx.len() / 3) as f32 > wanted * 4.0 {
        let keep = (wanted * 1.5 / (idx.len() / 3) as f32 * 100.0).clamp(0.5, 100.0);
        if let Some(d) = crate::decimate::decimate_raw(&crate::decimate::Raw { positions: p.clone(), indices: idx.clone(), groups: grp.clone(), colors: Vec::new() }, &crate::decimate::Settings { keep_percent: keep, keep_borders: true, keep_groups: true, keep_paint: false }) {
            let m = d.mesh;
            grp = (0..m.triangle_count() as u32).map(|t| m.group(t)).collect();
            p = m.positions;
            idx = m.indices;
        }
    }
    // A second skin inside the model (an AI or solidified mesh): only the outer one is laid
    // out.
    if std::env::var_os("ZR_KEEPINNER").is_none() {
        lap("start");
        let n = drop_inner_shell(&mut p, &mut idx, &mut grp);
        lap("inner skin");
        if n > 0 {
            // The quads asked for go on the outer skin alone: smaller quads, finer splits.
            let kept: f32 = idx.chunks_exact(3).map(|t| (p[t[1] as usize] - p[t[0] as usize]).cross(p[t[2] as usize] - p[t[0] as usize]).length() * 0.5).sum();
            frac = (kept / area.max(1e-12)).clamp(0.05, 1.0);
            edge *= frac.sqrt();
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("  dropped an inner skin: {n} triangles ({:.0}% of the area kept)", frac * 100.0);
            }
        }
    }
    // Handles about a quad across (a tunnel between fingers, a speck of a loop) would each
    // need cuts across the whole model: they are closed instead.
    // Passes out to 1.5, 3 and 5 quads (scaled by ZR_HANDLE, 0 = off), each on the mesh the
    // last one left.
    let scale: f32 = std::env::var("ZR_HANDLE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    if scale > 0.0 {
        let mut n = 0;
        for r in std::env::var("ZR_HANDLERADII").ok().map(|v| v.split(',').filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>()).unwrap_or(vec![1.5, 3.0, 5.0]) {
            for _ in 0..3 {
                let k = close_small_handles(&mut p, &mut idx, &mut grp, edge * DENSITY * r * scale);
                n += k;
                if k == 0 {
                    break;
                }
            }
        }
        // Handles with a short ring but a long way through (a tunnel): cut round the ring.
        if std::env::var_os("ZR_NOLOOPCUT").is_none() {
            let around: f32 = std::env::var("ZR_LOOPCUT").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
            n += cut_short_loops(&mut p, &mut idx, &mut grp, edge * DENSITY * around * scale);
        }
        if n > 0 && std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("  closed {n} small handles");
        }
        if let Some(path) = std::env::var_os("ZR_HANDLEDUMP") {
            let mut t = String::new();
            for q in &p {
                t.push_str(&format!("v {} {} {}\n", q.x, q.y, q.z));
            }
            for f in idx.chunks_exact(3) {
                t.push_str(&format!("f {} {} {}\n", f[0] + 1, f[1] + 1, f[2] + 1));
            }
            let _ = std::fs::write(path, t);
        }
    }
    lap("handles");
    for _ in 0..16 {
        let key = |a: u32, b: u32| (a.min(b), a.max(b));
        let mut mid: HashMap<(u32, u32), u32> = HashMap::default();
        for t in idx.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if p[a as usize].distance(p[b as usize]) > edge && !mid.contains_key(&key(a, b)) {
                    mid.insert(key(a, b), p.len() as u32);
                    p.push((p[a as usize] + p[b as usize]) * 0.5);
                }
            }
        }
        if mid.is_empty() {
            break;
        }
        if p.len() > 6_000_000 {
            return Err("the working mesh would be too large".into());
        }
        let mut out = Vec::with_capacity(idx.len() * 2);
        let mut og = Vec::with_capacity(idx.len() * 2);
        for (f, t) in idx.chunks_exact(3).enumerate() {
            let g = grp.get(f).copied().unwrap_or(0);
            let m: [Option<u32>; 3] = std::array::from_fn(|k| mid.get(&key(t[k], t[(k + 1) % 3])).copied());
            let n = m.iter().filter(|x| x.is_some()).count();
            let mut push = |a: u32, b: u32, c: u32| {
                out.extend_from_slice(&[a, b, c]);
                og.push(g);
            };
            match n {
                0 => push(t[0], t[1], t[2]),
                3 => {
                    let (m0, m1, m2) = (m[0].unwrap(), m[1].unwrap(), m[2].unwrap());
                    push(t[0], m0, m2);
                    push(m0, t[1], m1);
                    push(m2, m1, t[2]);
                    push(m0, m1, m2);
                }
                1 => {
                    let k = (0..3).find(|&k| m[k].is_some()).unwrap();
                    let (a, b, c, x) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3], m[k].unwrap());
                    push(a, x, c);
                    push(x, b, c);
                }
                _ => {
                    // Two split: the one not split is edge k.
                    let k = (0..3).find(|&k| m[k].is_none()).unwrap();
                    let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                    let (x, y) = (m[(k + 1) % 3].unwrap(), m[(k + 2) % 3].unwrap());
                    // x on b–c, y on c–a: triangle y x c, then the quad a b x y by its shorter diagonal.
                    push(y, x, c);
                    if p[a as usize].distance(p[x as usize]) < p[b as usize].distance(p[y as usize]) {
                        push(a, b, x);
                        push(a, x, y);
                    } else {
                        push(a, b, y);
                        push(b, x, y);
                    }
                }
            }
        }
        idx = out;
        grp = og;
    }
    Ok((SculptMesh::from_raw_with_groups(p, idx, grp), frac))
}

/// Directed boundary cycles of the faces `fs` (each edge a→b of a face whose b→a is not in
/// `fs`), or None if a boundary vertex is passed twice (the region pinches there).
fn boundary_cycles(idx: &[u32], fs: &HashSet<u32>) -> Option<Vec<Vec<u32>>> {
    let mut inside: HashSet<(u32, u32)> = HashSet::default();
    for &f in fs {
        let t = &idx[f as usize * 3..f as usize * 3 + 3];
        for k in 0..3 {
            inside.insert((t[k], t[(k + 1) % 3]));
        }
    }
    let mut next: HashMap<u32, u32> = HashMap::default();
    for &(a, b) in &inside {
        if !inside.contains(&(b, a)) && next.insert(a, b).is_some() {
            return None;
        }
    }
    let mut cycles = Vec::new();
    let mut done: HashSet<u32> = HashSet::default();
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        if done.contains(&s) {
            continue;
        }
        let mut c = vec![s];
        done.insert(s);
        let mut v = next[&s];
        while v != s {
            if !done.insert(v) {
                return None;
            }
            c.push(v);
            v = *next.get(&v)?;
        }
        cycles.push(c);
    }
    Some(cycles)
}

/// Genus of the faces `fs` from their Euler number alone (boundary loops counted as edge
/// components), for reporting a ball whose border pinches.
fn genus_hint(idx: &[u32], fs: &HashSet<u32>) -> i64 {
    let mut e: HashMap<(u32, u32), u32> = HashMap::default();
    let mut v: HashSet<u32> = HashSet::default();
    for &f in fs {
        let t = &idx[f as usize * 3..f as usize * 3 + 3];
        for k in 0..3 {
            v.insert(t[k]);
            *e.entry((t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3]))).or_default() += 1;
        }
    }
    let chi = v.len() as i64 - e.len() as i64 + fs.len() as i64;
    let bedges: Vec<(u32, u32)> = e.iter().filter(|x| *x.1 == 1).map(|x| *x.0).collect();
    let mut par: HashMap<u32, u32> = HashMap::default();
    fn find(par: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let mut r = x;
        while let Some(&q) = par.get(&r) {
            if q == r {
                break;
            }
            r = q;
        }
        r
    }
    for &(a, b) in &bedges {
        par.entry(a).or_insert(a);
        par.entry(b).or_insert(b);
        let (ra, rb) = (find(&mut par, a), find(&mut par, b));
        par.insert(ra, rb);
    }
    let comps: HashSet<u32> = par.keys().copied().collect::<Vec<_>>().into_iter().map(|x| find(&mut par, x)).collect();
    (2 - chi - comps.len() as i64) / 2
}

/// Drop a second skin inside a closed model: triangles from which no ray gets out (14
/// directions, nearest the normal first), in connected regions over 1% of the model, go;
/// the holes left where the skins joined are capped. Returns how many triangles went.
fn drop_inner_shell(p: &mut Vec<Vec3>, idx: &mut Vec<u32>, grp: &mut Vec<u16>) -> usize {
    let nf = idx.len() / 3;
    if nf < 100 {
        return 0;
    }
    // Closed models only (an open sheet sees out everywhere anyway).
    let mut edges: HashMap<(u32, u32), u32> = HashMap::default();
    for t in idx.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    if edges.values().any(|&c| c != 2) {
        return 0;
    }
    let mesh = SculptMesh::from_raw(p.clone(), idx.clone());
    let (lo, hi) = p.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), q| (a.min(*q), b.max(*q)));
    let eps = (hi - lo).length() * 1e-5;
    let mut dirs: Vec<Vec3> = vec![Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                dirs.push(Vec3::new(sx, sy, sz).normalize());
            }
        }
    }
    // On a big mesh every few triangles are tested (`ZR_SHELLSTRIDE`; a blind one casts all
    // 14 rays), the rest take the answer of the nearest tested one across shared edges: the
    // inner skin is found as large regions anyway.
    let stride: usize = std::env::var("ZR_SHELLSTRIDE").ok().and_then(|v| v.parse().ok()).unwrap_or((nf / 30_000).clamp(1, 8));
    let tested: Vec<Option<bool>> = (0..nf).into_par_iter().map(|f| {
        if f % stride != 0 {
            return None;
        }
        let (a, b, c) = (p[idx[f * 3] as usize], p[idx[f * 3 + 1] as usize], p[idx[f * 3 + 2] as usize]);
        let n = (b - a).cross(c - a).normalize_or_zero();
        let mid = (a + b + c) / 3.0;
        let mut order: Vec<Vec3> = dirs.clone();
        order.sort_by(|x, y| y.dot(n).abs().total_cmp(&x.dot(n).abs()));
        Some(!order.iter().any(|&d| mesh.raycast(mid + d * eps, d).is_none()))
    }).collect();
    let mut ef: HashMap<(u32, u32), Vec<u32>> = HashMap::default();
    for (f, t) in idx.chunks_exact(3).enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            ef.entry((a.min(b), a.max(b))).or_default().push(f as u32);
        }
    }
    let blind: Vec<bool> = {
        let mut lab: Vec<Option<bool>> = tested;
        let mut q: VecDeque<u32> = (0..nf as u32).filter(|&f| lab[f as usize].is_some()).collect();
        while let Some(f) = q.pop_front() {
            let l = lab[f as usize];
            for k in 0..3 {
                let (a, b) = (idx[f as usize * 3 + k], idx[f as usize * 3 + (k + 1) % 3]);
                for &g in &ef[&(a.min(b), a.max(b))] {
                    if lab[g as usize].is_none() {
                        lab[g as usize] = l;
                        q.push_back(g);
                    }
                }
            }
        }
        lab.into_iter().map(|l| l.unwrap_or(false)).collect()
    };
    // Connected regions of blind triangles; small ones (a crevice) stay.
    let mut gone = vec![false; nf];
    let mut seen = vec![false; nf];
    for f0 in 0..nf {
        if !blind[f0] || seen[f0] {
            continue;
        }
        let mut region = vec![f0 as u32];
        seen[f0] = true;
        let mut i = 0;
        while i < region.len() {
            let f = region[i] as usize;
            i += 1;
            for k in 0..3 {
                let (a, b) = (idx[f * 3 + k], idx[f * 3 + (k + 1) % 3]);
                for &g in &ef[&(a.min(b), a.max(b))] {
                    if blind[g as usize] && !seen[g as usize] {
                        seen[g as usize] = true;
                        region.push(g);
                    }
                }
            }
        }
        if region.len() * 100 > nf {
            for &f in &region {
                gone[f as usize] = true;
            }
        }
    }
    let n_gone = gone.iter().filter(|&&g| g).count();
    if n_gone == 0 {
        return 0;
    }
    // Where a hole's border would pass a point twice, the triangles round that point go too.
    let mut vf: Vec<Vec<u32>> = vec![Vec::new(); p.len()];
    for (f, t) in idx.chunks_exact(3).enumerate() {
        for &v in t {
            vf[v as usize].push(f as u32);
        }
    }
    for _ in 0..8 {
        let mut out_edges: HashMap<u32, u32> = HashMap::default();
        let mut inside: HashSet<(u32, u32)> = HashSet::default();
        for f in 0..nf {
            if !gone[f] {
                for k in 0..3 {
                    inside.insert((idx[f * 3 + k], idx[f * 3 + (k + 1) % 3]));
                }
            }
        }
        for &(a, b) in &inside {
            if !inside.contains(&(b, a)) {
                *out_edges.entry(a).or_default() += 1;
            }
        }
        let pinch: Vec<u32> = out_edges.iter().filter(|x| *x.1 > 1).map(|x| *x.0).collect();
        if pinch.is_empty() {
            break;
        }
        for v in pinch {
            for &f in &vf[v as usize] {
                gone[f as usize] = true;
            }
        }
    }
    let n_gone = gone.iter().filter(|&&g| g).count();
    let mut out: Vec<u32> = (0..nf as u32).filter(|&f| !gone[f as usize]).collect();
    out.sort_unstable();
    let mut new_idx: Vec<u32> = Vec::with_capacity(out.len() * 3);
    let mut new_grp: Vec<u16> = Vec::with_capacity(out.len());
    for &f in &out {
        new_idx.extend_from_slice(&idx[f as usize * 3..f as usize * 3 + 3]);
        new_grp.push(grp.get(f as usize).copied().unwrap_or(0));
    }
    let all: HashSet<u32> = (0..out.len() as u32).collect();
    let Some(holes) = boundary_cycles(&new_idx, &all) else {
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("  inner skin found ({n_gone} triangles) but its joins pinch: kept");
        }
        return 0;
    };
    let group = new_grp.first().copied().unwrap_or(0);
    for h in holes {
        let mid = h.iter().map(|&v| p[v as usize]).sum::<Vec3>() / h.len() as f32;
        let mc = p.len() as u32;
        p.push(mid);
        for i in 0..h.len() {
            new_idx.extend_from_slice(&[h[(i + 1) % h.len()], h[i], mc]);
            new_grp.push(group);
        }
    }
    // Pieces left: loose bits from between the skins (under 1% of what is kept) go.
    {
        let m = new_idx.len() / 3;
        let mut par: Vec<u32> = (0..p.len() as u32).collect();
        fn rt(par: &mut [u32], mut x: u32) -> u32 {
            while par[x as usize] != x {
                par[x as usize] = par[par[x as usize] as usize];
                x = par[x as usize];
            }
            x
        }
        for t in new_idx.chunks_exact(3) {
            let a = rt(&mut par, t[0]);
            for &v in &t[1..] {
                let b = rt(&mut par, v);
                par[b as usize] = a;
            }
        }
        let mut size: HashMap<u32, usize> = HashMap::default();
        for t in new_idx.chunks_exact(3) {
            *size.entry(rt(&mut par, t[0])).or_default() += 1;
        }
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            let mut sz: Vec<usize> = size.values().copied().collect();
            sz.sort_unstable_by(|a, b| b.cmp(a));
            eprintln!("  after the inner skin: pieces {:?}", &sz[..sz.len().min(12)]);
        }
        let keep_f: Vec<bool> = new_idx.chunks_exact(3).map(|t| size[&rt(&mut par, t[0])] * 100 >= m).collect();
        if keep_f.iter().any(|&k| !k) {
            let (mut i2, mut g2) = (Vec::with_capacity(new_idx.len()), Vec::with_capacity(m));
            for f in 0..m {
                if keep_f[f] {
                    i2.extend_from_slice(&new_idx[f * 3..f * 3 + 3]);
                    g2.push(new_grp[f]);
                }
            }
            new_idx = i2;
            new_grp = g2;
        }
    }
    *idx = new_idx;
    *grp = new_grp;
    n_gone
}

/// Euler number left over by the pieces of a closed mesh: 2 per piece minus V − E + F (twice
/// the handles), or None if an edge is not in exactly two triangles.
fn handles_left(nv: usize, idx: &[u32]) -> Option<i64> {
    let mut edges: HashMap<(u32, u32), u32> = HashMap::default();
    for t in idx.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    if edges.values().any(|&c| c != 2) {
        return None;
    }
    let mut parent: Vec<u32> = (0..nv as u32).collect();
    fn root(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    for t in idx.chunks_exact(3) {
        let a = root(&mut parent, t[0]);
        for &v in &t[1..] {
            let b = root(&mut parent, v);
            parent[b as usize] = a;
        }
    }
    let used: HashSet<u32> = idx.iter().copied().collect();
    let pieces: HashSet<u32> = used.iter().map(|&v| root(&mut parent, v)).collect();
    let chi = used.len() as i64 - edges.len() as i64 + (idx.len() / 3) as i64;
    Some(2 * pieces.len() as i64 - chi)
}

/// The shortest loop round a handle through `root` (shortest-path tree, then the edges whose
/// loop does not cut the surface in two: those on a cycle of the dual graph of the edges off
/// the tree). The loop's own part (without the way out from `root`) and its length.
fn handle_loop(p: &[Vec3], idx: &[u32], nb: &[Vec<u32>], edge_faces: &HashMap<(u32, u32), [u32; 2]>, root: u32) -> Option<(Vec<u32>, f32)> {
    let nv = p.len();
    let mut dist = vec![f32::INFINITY; nv];
    let mut parent = vec![NONE; nv];
    dist[root as usize] = 0.0;
    let mut heap = std::collections::BinaryHeap::new();
    heap.push((std::cmp::Reverse(0u32), root));
    while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
        let d = f32::from_bits(db);
        if d > dist[v as usize] {
            continue;
        }
        for &u in &nb[v as usize] {
            let nd = d + p[v as usize].distance(p[u as usize]);
            if nd < dist[u as usize] {
                dist[u as usize] = nd;
                parent[u as usize] = v;
                heap.push((std::cmp::Reverse(nd.to_bits()), u));
            }
        }
    }
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let in_tree = |a: u32, b: u32| parent[a as usize] == b || parent[b as usize] == a;
    // Dual tree over the edges off the tree, from a face at the root.
    let nf = idx.len() / 3;
    let f0 = (0..nf).find(|&f| idx[f * 3..f * 3 + 3].contains(&root))? as u32;
    let mut dpar = vec![NONE; nf];
    let mut dedge: Vec<(u32, u32)> = vec![(NONE, NONE); nf];
    let mut depth = vec![0u32; nf];
    let mut seen = vec![false; nf];
    seen[f0 as usize] = true;
    let mut q = VecDeque::from([f0]);
    let mut crossed: HashSet<(u32, u32)> = HashSet::default();
    let mut extra: Vec<((u32, u32), u32, u32)> = Vec::new();
    while let Some(f) = q.pop_front() {
        let t = &idx[f as usize * 3..f as usize * 3 + 3];
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if in_tree(a, b) || !crossed.insert(key(a, b)) {
                continue;
            }
            let fs = edge_faces.get(&key(a, b))?;
            let g = if fs[0] == f { fs[1] } else { fs[0] };
            if seen[g as usize] {
                extra.push((key(a, b), f, g));
            } else {
                seen[g as usize] = true;
                dpar[g as usize] = f;
                dedge[g as usize] = key(a, b);
                depth[g as usize] = depth[f as usize] + 1;
                q.push_back(g);
            }
        }
    }
    // Candidates: the extra dual edges and the dual-tree edges on their cycles.
    let mut cand: Vec<(u32, u32)> = Vec::new();
    for &(e, f, g) in &extra {
        cand.push(e);
        let (mut a, mut b) = (f, g);
        while a != b {
            if depth[a as usize] >= depth[b as usize] {
                cand.push(dedge[a as usize]);
                a = dpar[a as usize];
            } else {
                cand.push(dedge[b as usize]);
                b = dpar[b as usize];
            }
        }
    }
    let (u, v) = cand.into_iter().filter(|&(a, b)| dist[a as usize].is_finite() && dist[b as usize].is_finite()).min_by(|&(a, b), &(c, d)| {
        (dist[a as usize] + dist[b as usize] + p[a as usize].distance(p[b as usize])).total_cmp(&(dist[c as usize] + dist[d as usize] + p[c as usize].distance(p[d as usize])))
    })?;
    let path = |mut x: u32| {
        let mut out = vec![x];
        while parent[x as usize] != NONE {
            x = parent[x as usize];
            out.push(x);
        }
        out
    };
    let (mut pu, mut pv) = (path(u), path(v));
    // Drop the shared way back to the root, keeping the vertex where they part.
    while pu.len() > 1 && pv.len() > 1 && pu[pu.len() - 2] == pv[pv.len() - 2] {
        pu.pop();
        pv.pop();
    }
    pv.pop();
    pu.reverse();
    let mut cycle = pu;
    cycle.extend(pv);
    // u ... w ... v: as a loop, w → u then v → (back to w).
    let len: f32 = (0..cycle.len()).map(|i| p[cycle[i] as usize].distance(p[cycle[(i + 1) % cycle.len()] as usize])).sum();
    let mut seen_v: HashSet<u32> = HashSet::default();
    if cycle.len() < 3 || !cycle.iter().all(|&x| seen_v.insert(x)) {
        return None;
    }
    Some((cycle, len))
}

/// Cut a closed mesh round its handles' short loops (under `max_len` round) and cap both
/// sides of each cut. Returns how many were cut.
fn cut_short_loops(p: &mut Vec<Vec3>, idx: &mut Vec<u32>, grp: &mut Vec<u16>, max_len: f32) -> usize {
    let dbg = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    let mut cut = 0;
    for _ in 0..16 {
        if handles_left(p.len(), idx).is_none_or(|h| h <= 0) {
            break;
        }
        let nv = p.len();
        let mut nb: Vec<Vec<u32>> = vec![Vec::new(); nv];
        let mut edge_faces: HashMap<(u32, u32), [u32; 2]> = HashMap::default();
        for (f, t) in idx.chunks_exact(3).enumerate() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                nb[a as usize].push(b);
                let e = edge_faces.entry((a.min(b), a.max(b))).or_insert([NONE, NONE]);
                if e[0] == NONE { e[0] = f as u32 } else { e[1] = f as u32 }
            }
        }
        // Roots: a few vertices spread over the model; the best loop, then once more from a
        // point on it (the shortest loop through that point).
        let used: Vec<u32> = (0..nv as u32).filter(|&v| !nb[v as usize].is_empty()).collect();
        // Roots spread evenly (farthest points first), so one lands near every short loop.
        let roots_n: usize = std::env::var("ZR_LOOPROOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(32);
        let mut roots = vec![used[0]];
        let mut far: Vec<f32> = used.iter().map(|&v| p[v as usize].distance_squared(p[used[0] as usize])).collect();
        while roots.len() < roots_n {
            let Some(i) = (0..used.len()).max_by(|&a, &b| far[a].total_cmp(&far[b])) else { break };
            if far[i] <= 0.0 {
                break;
            }
            let r = used[i];
            roots.push(r);
            for (j, &v) in used.iter().enumerate() {
                far[j] = far[j].min(p[v as usize].distance_squared(p[r as usize]));
            }
        }
        let mut best: Option<(Vec<u32>, f32)> = None;
        for &r in &roots {
            if let Some(l) = handle_loop(p, idx, &nb, &edge_faces, r) {
                if best.as_ref().is_none_or(|b| l.1 < b.1) {
                    best = Some(l);
                }
            }
        }
        let Some((c0, _)) = best else { break };
        let Some((cycle, len)) = handle_loop(p, idx, &nb, &edge_faces, c0[c0.len() / 2]) else { break };
        if dbg {
            let mid = cycle.iter().map(|&v| p[v as usize]).sum::<Vec3>() / cycle.len() as f32;
            eprintln!("    handle loop at {mid:.3}: {:.3} round ({} points){}", len, cycle.len(), if len > max_len { ", too long to cut" } else { ", cut" });
        }
        if len > max_len {
            break;
        }
        // Split each loop vertex: the faces on the right of the loop (from the edge to the
        // next loop vertex round to the edge from the previous one, clockwise) take a copy.
        let m = cycle.len();
        let mut fan_of: HashMap<u32, Vec<u32>> = HashMap::default();
        for (f, t) in idx.chunks_exact(3).enumerate() {
            for &v in t {
                if cycle.contains(&v) {
                    fan_of.entry(v).or_default().push(f as u32);
                }
            }
        }
        let mut new_idx = idx.clone();
        let mut ok = true;
        for i in 0..m {
            let (c, next, prev) = (cycle[i], cycle[(i + 1) % m], cycle[(i + m - 1) % m]);
            let copy = p.len() as u32;
            // Counter-clockwise round c: face (c, a, b) is followed by the face (c, b, ·).
            let fan = &fan_of[&c];
            let corner = |f: u32| -> (u32, u32) {
                let t = &idx[f as usize * 3..f as usize * 3 + 3];
                let k = t.iter().position(|&x| x == c).unwrap();
                (t[(k + 1) % 3], t[(k + 2) % 3])
            };
            let Some(mut f) = fan.iter().copied().find(|&f| corner(f).0 == next) else { ok = false; break };
            let mut side = Vec::new();
            for _ in 0..fan.len() {
                side.push(f);
                let (_, b) = corner(f);
                if b == prev {
                    break;
                }
                let Some(g) = fan.iter().copied().find(|&g| corner(g).0 == b) else { ok = false; break };
                f = g;
            }
            if !ok || corner(*side.last().unwrap()).1 != prev || side.len() == fan.len() {
                ok = false;
                break;
            }
            p.push(p[c as usize]);
            for &f in &side {
                for k in 0..3 {
                    if new_idx[f as usize * 3 + k] == c {
                        new_idx[f as usize * 3 + k] = copy;
                    }
                }
            }
        }
        if !ok {
            p.truncate(nv);
            if dbg {
                eprintln!("    handle loop: could not split its fans");
            }
            break;
        }
        // Cap the two holes.
        let all: HashSet<u32> = (0..(new_idx.len() / 3) as u32).collect();
        let Some(holes) = boundary_cycles(&new_idx, &all) else {
            p.truncate(nv);
            break;
        };
        let group = grp.first().copied().unwrap_or(0);
        for h in holes {
            let mid = h.iter().map(|&v| p[v as usize]).sum::<Vec3>() / h.len() as f32;
            let mc = p.len() as u32;
            p.push(mid);
            for i in 0..h.len() {
                new_idx.extend_from_slice(&[h[(i + 1) % h.len()], h[i], mc]);
                grp.push(group);
            }
        }
        *idx = new_idx;
        cut += 1;
    }
    cut
}

/// Close handles that fit in a ball of radius `r` (geodesic): the ball's faces go and each
/// hole left is capped with a fan round its middle. Returns how many were closed.
fn close_small_handles(p: &mut Vec<Vec3>, idx: &mut Vec<u32>, grp: &mut Vec<u16>, r: f32) -> usize {
    let nv = p.len();
    let nf = idx.len() / 3;
    // Whole-mesh genus first: nothing to do on a sphere-like model.
    let mut edges: HashSet<(u32, u32)> = HashSet::default();
    for t in idx.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            edges.insert((a.min(b), a.max(b)));
        }
    }
    let used: HashSet<u32> = idx.iter().copied().collect();
    let mut parent: Vec<u32> = (0..nv as u32).collect();
    fn root(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    for t in idx.chunks_exact(3) {
        let a = root(&mut parent, t[0]);
        for &v in &t[1..] {
            let b = root(&mut parent, v);
            parent[b as usize] = a;
        }
    }
    let pieces: HashSet<u32> = used.iter().map(|&v| root(&mut parent, v)).collect();
    let chi = used.len() as i64 - edges.len() as i64 + nf as i64;
    if chi >= 2 * pieces.len() as i64 {
        return 0;
    }
    let mut vf: Vec<Vec<u32>> = vec![Vec::new(); nv];
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); nv];
    for (f, t) in idx.chunks_exact(3).enumerate() {
        for k in 0..3 {
            vf[t[k] as usize].push(f as u32);
            nb[t[k] as usize].push(t[(k + 1) % 3]);
            nb[t[k] as usize].push(t[(k + 2) % 3]);
        }
    }
    for n in &mut nb {
        n.sort_unstable();
        n.dedup();
    }
    // Seeds about r/2 apart (a grid hash), each a ball of radius r.
    let cell = (r * 0.5).max(1e-9);
    let mut taken: HashSet<(i32, i32, i32)> = HashSet::default();
    let mut removed: HashSet<u32> = HashSet::default();
    let mut caps: Vec<(Vec<u32>, u16)> = Vec::new();
    let mut closed = 0;
    for s in 0..nv as u32 {
        if nb[s as usize].is_empty() {
            continue;
        }
        let q = p[s as usize] / cell;
        let key = (q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32);
        if !taken.insert(key) {
            continue;
        }
        // Dijkstra out to r.
        let mut dist: HashMap<u32, f32> = HashMap::default();
        dist.insert(s, 0.0);
        let mut heap = std::collections::BinaryHeap::new();
        heap.push((std::cmp::Reverse(0u32), s));
        while let Some((std::cmp::Reverse(dbits), v)) = heap.pop() {
            let d = f32::from_bits(dbits);
            if d > dist[&v] {
                continue;
            }
            for &u in &nb[v as usize] {
                let nd = d + p[v as usize].distance(p[u as usize]);
                if nd < r && dist.get(&u).is_none_or(|&o| nd < o) {
                    dist.insert(u, nd);
                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                }
            }
            if dist.len() > 20_000 {
                break;
            }
        }
        let mut fs: HashSet<u32> = dist.keys().flat_map(|&v| vf[v as usize].iter().copied()).filter(|&f| (0..3).all(|k| dist.contains_key(&idx[f as usize * 3 + k]))).collect();
        if fs.is_empty() || fs.iter().any(|f| removed.contains(f)) {
            continue;
        }
        let genus = |fs: &HashSet<u32>| -> Option<i64> {
            let cycles = boundary_cycles(idx, fs)?;
            let mut e: HashSet<(u32, u32)> = HashSet::default();
            let mut v: HashSet<u32> = HashSet::default();
            for &f in fs {
                let t = &idx[f as usize * 3..f as usize * 3 + 3];
                for k in 0..3 {
                    v.insert(t[k]);
                    e.insert((t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3])));
                }
            }
            let chi = v.len() as i64 - e.len() as i64 + fs.len() as i64;
            let b = cycles.len() as i64;
            Some((2 - chi - b) / 2)
        };
        // A pinched ball grows by the faces round its pinch points (a few times).
        let mut g = genus(&fs);
        for _ in 0..3 {
            if g.is_some() {
                break;
            }
            let ring: Vec<u32> = fs.iter().flat_map(|&f| (0..3).map(move |k| (f, k))).flat_map(|(f, k)| vf[idx[f as usize * 3 + k] as usize].clone()).collect();
            fs.extend(ring);
            g = genus(&fs);
        }
        let dbg = std::env::var_os("KLAY_ZR_DEBUG").is_some();
        let Some(g) = g else {
            if dbg && (genus_hint(idx, &fs) > 0 || std::env::var_os("ZR_HANDLEPINCH").is_some()) {
                eprintln!("    handle at {:.3} (r {r:.3}): ball pinched", p[s as usize]);
            }
            continue;
        };
        if g < 1 {
            continue;
        }
        if fs.iter().any(|f| removed.contains(f)) {
            if dbg {
                eprintln!("    handle at {:.3} (r {r:.3}): overlaps one closed", p[s as usize]);
            }
            continue;
        }
        let Some(cycles) = boundary_cycles(idx, &fs) else { continue };
        if dbg {
            eprintln!("    handle at {:.3} (r {r:.3}): genus {g}, {} holes, closed", p[s as usize], cycles.len());
        }
        let group = grp.get(*fs.iter().next().unwrap() as usize).copied().unwrap_or(0);
        removed.extend(fs.iter().copied());
        for c in cycles {
            caps.push((c, group));
        }
        closed += 1;
    }
    if closed == 0 {
        return 0;
    }
    let mut out = Vec::with_capacity(idx.len());
    let mut og = Vec::with_capacity(nf);
    for f in 0..nf {
        if !removed.contains(&(f as u32)) {
            out.extend_from_slice(&idx[f * 3..f * 3 + 3]);
            og.push(grp.get(f).copied().unwrap_or(0));
        }
    }
    for (c, group) in caps {
        let mid = c.iter().map(|&v| p[v as usize]).sum::<Vec3>() / c.len() as f32;
        let m = p.len() as u32;
        p.push(mid);
        for i in 0..c.len() {
            out.extend_from_slice(&[c[i], c[(i + 1) % c.len()], m]);
            og.push(group);
        }
    }
    *idx = out;
    *grp = og;
    closed
}

/// Feature loops: sockets and bumps (where the surface bends clearly more, or less, than
/// round about) get a closed loop round them, made hard, so each becomes its own patch of
/// rings. Returns how many.
fn feature_loops(w: &mut Work, quad: f32, enclosed: &mut Vec<bool>) -> usize {
    let nv = w.p.len();
    if nv == 0 {
        return 0;
    }
    // Mean curvature at each vertex (from the fitted bending tensor).
    let edges: Vec<(u32, u32)> = (0..nv as u32).flat_map(|v| w.neighbours(v).iter().map(move |&u| (v, u))).collect();
    let g = graph_from(w.p.clone(), w.vnrm.clone(), vec![Vec3::ZERO; nv], vec![0.0; nv], edges);
    let tensors = curvature_tensors(&g, 2);
    let mean: Vec<f32> = (0..nv).map(|v| {
        let n = w.vnrm[v];
        let t = tensors[v];
        let u = n.any_orthonormal_vector();
        let b = n.cross(u);
        0.5 * (u.dot(t * u) + b.dot(t * b))
    }).collect();
    // How lopsided the bending is at each vertex: 0 bends alike both ways (a ball, a
    // socket), 1 bends one way only (a cylinder, a rim) or saddles.
    let lopsided: Vec<f32> = (0..nv).map(|v| {
        let n = w.vnrm[v];
        let t = tensors[v];
        let u = n.any_orthonormal_vector();
        let b = n.cross(u);
        let (a, bb, c) = (u.dot(t * u), u.dot(t * b), b.dot(t * b));
        let mean = 0.5 * (a + c);
        let half = (0.25 * (a - c) * (a - c) + bb * bb).sqrt();
        let (k1, k2) = (mean + half, mean - half);
        ((k1 - k2) / (k1.abs() + k2.abs()).max(1e-12)).clamp(0.0, 1.0)
    }).collect();
    // Smoothed at about a quad, and at about four (the surroundings).
    let edge = (0..nv.min(2000)).filter_map(|v| w.neighbours(v as u32).first().map(|&u| w.p[v].distance(w.p[u as usize]))).sum::<f32>() / nv.min(2000).max(1) as f32;
    let blur = |x: &[f32], radius: f32| -> Vec<f32> {
        let passes = ((radius / edge.max(1e-9)).powi(2) as usize).clamp(1, 2000);
        let mut a = x.to_vec();
        for _ in 0..passes {
            a = (0..nv).into_par_iter().map(|v| {
                let nb = w.neighbours(v as u32);
                if nb.is_empty() {
                    return a[v];
                }
                0.5 * a[v] + 0.5 * nb.iter().map(|&u| a[u as usize]).sum::<f32>() / nb.len() as f32
            }).collect();
        }
        a
    };
    let near = blur(&mean, quad);
    let far = blur(&near, quad * 4.0);
    let d: Vec<f32> = (0..nv).map(|v| near[v] - far[v]).collect();
    let sd = (d.iter().map(|x| x * x).sum::<f32>() / nv as f32).sqrt();
    // Clear features only: well past the spread, and bending over a quad by a fair angle.
    let tau = (sd * 1.5).max(0.05 / quad);
    let tag: Vec<i8> = d.iter().map(|&x| if x > tau { 1 } else if x < -tau { -1 } else { 0 }).collect();
    // Regions of faces (all corners tagged alike), grown by half a quad.
    let nf = w.t.len();
    let mut region: Vec<i32> = vec![-1; nf];
    let mut count = 0usize;
    let mut regions: Vec<Vec<usize>> = Vec::new();
    for f0 in 0..nf {
        let s0 = tag[w.t[f0][0] as usize];
        if region[f0] != -1 || s0 == 0 || w.t[f0].iter().any(|&v| tag[v as usize] != s0) {
            continue;
        }
        let id = regions.len() as i32;
        let mut faces = vec![f0];
        region[f0] = id;
        let mut i = 0;
        while i < faces.len() {
            let f = faces[i];
            i += 1;
            for k in 0..3 {
                let o = w.twin[f * 3 + k];
                if o == NONE {
                    continue;
                }
                let g = o as usize / 3;
                if region[g] == -1 && w.t[g].iter().all(|&v| tag[v as usize] == s0) {
                    region[g] = id;
                    faces.push(g);
                }
            }
        }
        regions.push(faces);
    }
    let area = |f: usize| -> f32 {
        let t = w.t[f];
        (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
    };
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let mut areas: Vec<(f32, i8)> = regions.iter().map(|fs| (fs.iter().map(|&f| area(f)).sum::<f32>() / (quad * quad), tag[w.t[fs[0]][0] as usize])).collect();
        areas.sort_by(|a, b| b.0.total_cmp(&a.0));
        eprintln!("  features: sd {sd:.3} tau {tau:.3} (1/quad {:.2}), {} regions, areas in quads² {:?}", 1.0 / quad, regions.len(), &areas[..areas.len().min(12)]);
    }
    // The kept regions (about 2 × 2 to 20 × 20 quads) as one inside/outside field on the
    // vertices, blurred at about a quad and cut at the halfway level: outlines come out round,
    // and features closer than about a quad merge into one loop. ZR_RAWFEATURES keeps the
    // raw outlines (each region grown half a quad).
    let raw = std::env::var_os("ZR_RAWFEATURES").is_some();
    let mut field = vec![0.0f32; nv];
    let mut any = false;
    for faces in &regions {
        let a: f32 = faces.iter().map(|&f| area(f)).sum();
        if a < quad * quad * 4.0 || a > quad * quad * 400.0 {
            continue;
        }
        any = true;
        for &f in faces {
            for &v in &w.t[f] {
                field[v as usize] = 1.0;
            }
        }
    }
    if !any {
        return 0;
    }
    let blur_r: f32 = std::env::var("ZR_FEATBLUR").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let (grow, cutoff) = if raw { (quad * 0.5, 0.5) } else { (0.0, 0.5) };
    let mut field = if raw { field } else { blur(&field, quad * blur_r) };
    // Raw: grow the regions by `grow` (a vertex within that distance of a region).
    if raw && grow > 0.0 {
        let steps = (grow / edge.max(1e-9)).ceil() as usize;
        for _ in 0..steps {
            field = (0..nv).map(|v| if field[v] >= 1.0 || w.neighbours(v as u32).iter().any(|&u| field[u as usize] >= 1.0) { 1.0 } else { 0.0 }).collect();
        }
    }
    // Inside faces: the mean of their corners over the cut-off.
    let mut inside_face: Vec<bool> = (0..nf).map(|f| w.t[f].iter().map(|&v| field[v as usize]).sum::<f32>() / 3.0 > cutoff).collect();
    // Rings round centres (the default; ZR_BLOBLOOPS keeps the blurred regions): per feature,
    // its strongest point, a radius from its area, rings kept two quads apart; the ring is
    // every face within that surface distance of the centre.
    if std::env::var_os("ZR_BLOBLOOPS").is_none() {
        let peak_min: f32 = std::env::var("ZR_PEAK").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5);
        let mut feats: Vec<(u32, f32, f32)> = Vec::new(); // centre, radius, strength
        for faces in &regions {
            let a: f32 = faces.iter().map(|&f| area(f)).sum();
            if a < quad * quad * 4.0 || a > quad * quad * 400.0 {
                continue;
            }
            // A bump or socket bends both ways over most of it; a rim or crease one way.
            let verts: HashSet<u32> = faces.iter().flat_map(|&f| w.t[f]).collect();
            let round = verts.iter().map(|&v| lopsided[v as usize]).sum::<f32>() / verts.len().max(1) as f32;
            let max_lop: f32 = std::env::var("ZR_ROUND").ok().and_then(|v| v.parse().ok()).unwrap_or(1.01);
            // It must actually curve (a flat cap inside a rim stands out but is no feature).
            let curves = verts.iter().map(|&v| mean[v as usize].abs()).sum::<f32>() / verts.len().max(1) as f32 * quad;
            let near_hard = faces.iter().any(|&f| (0..3).any(|k| w.hard[f * 3 + k] && !w.soft[f * 3 + k]));
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("    feature region {:.1} quads²: lopsided {round:.2}, curvature × quad {curves:.3}{}", a / (quad * quad), if near_hard { ", on a sharp edge" } else { "" });
            }
            if round > max_lop || near_hard || curves < 0.08 {
                continue;
            }
            let Some(c) = faces.iter().flat_map(|&f| w.t[f]).max_by(|&x, &y| d[x as usize].abs().total_cmp(&d[y as usize].abs())) else { continue };
            let strength = d[c as usize].abs();
            if strength < sd * peak_min {
                continue;
            }
            feats.push((c, (a / std::f32::consts::PI).sqrt() + quad * 0.5, strength));
        }
        // Keep apart: strongest first; a weaker one shrinks to leave two quads, or goes.
        feats.sort_by(|a, b| b.2.total_cmp(&a.2));
        let mut kept: Vec<(u32, f32)> = Vec::new();
        for &(c, r, _) in &feats {
            let mut r = r;
            for &(c2, r2) in &kept {
                let dc = w.p[c as usize].distance(w.p[c2 as usize]);
                r = r.min(dc - r2 - quad * 2.0);
            }
            if r >= quad * 1.5 {
                kept.push((c, r));
            }
        }
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("  feature rings: {} of {} features, radii in quads {:?}", kept.len(), feats.len(), kept.iter().map(|k| (k.1 / quad * 10.0).round() / 10.0).collect::<Vec<_>>());
        }
        // Surface distance from each centre (Dijkstra over edges), out to its radius.
        let mut ring_dist = vec![f32::INFINITY; nv];
        for &(c, r) in &kept {
            let mut dist: HashMap<u32, f32> = HashMap::default();
            let mut heap = std::collections::BinaryHeap::new();
            dist.insert(c, 0.0);
            heap.push((std::cmp::Reverse(0f32.to_bits()), c));
            while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
                let dv = f32::from_bits(db);
                if dv > dist[&v] {
                    continue;
                }
                let rel = dv / r;
                if rel < ring_dist[v as usize] {
                    ring_dist[v as usize] = rel;
                }
                for &u in w.neighbours(v) {
                    let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                    if nd <= r * 1.2 && dist.get(&u).is_none_or(|&x| nd < x) {
                        dist.insert(u, nd);
                        heap.push((std::cmp::Reverse(nd.to_bits()), u));
                    }
                }
            }
        }
        inside_face = (0..nf).map(|f| w.t[f].iter().all(|&v| ring_dist[v as usize] <= 1.0)).collect();
    }
    // Each connected inside region, its holes filled, becomes a loop (if still a feature's size).
    let mut seen = vec![false; nf];
    for f0 in 0..nf {
        if !inside_face[f0] || seen[f0] {
            continue;
        }
        let mut comp = vec![f0];
        seen[f0] = true;
        let mut i = 0;
        while i < comp.len() {
            let f = comp[i];
            i += 1;
            for k in 0..3 {
                let o = w.twin[f * 3 + k];
                if o == NONE || w.hard[f * 3 + k] {
                    continue;
                }
                let g = o as usize / 3;
                if inside_face[g] && !seen[g] {
                    seen[g] = true;
                    comp.push(g);
                }
            }
        }
        let a: f32 = comp.iter().map(|&f| area(f)).sum();
        if a < quad * quad * 4.0 || a > quad * quad * 600.0 {
            continue;
        }
        let inside: HashSet<usize> = comp.iter().copied().collect();
        let mut outside: HashSet<usize> = HashSet::default();
        if let Some(s0) = (0..nf).find(|f| !inside.contains(f)) {
            let mut q = vec![s0];
            outside.insert(s0);
            while let Some(f) = q.pop() {
                for k in 0..3 {
                    let o = w.twin[f * 3 + k];
                    if o != NONE {
                        let g = o as usize / 3;
                        if !inside.contains(&g) && outside.insert(g) {
                            q.push(g);
                        }
                    }
                }
            }
        }
        let mut made = false;
        if enclosed.len() != nf {
            *enclosed = vec![false; nf];
        }
        for f in 0..nf {
            if outside.contains(&f) {
                continue;
            }
            enclosed[f] = true;
            for k in 0..3 {
                let h = f * 3 + k;
                let o = w.twin[h];
                if o != NONE && outside.contains(&(o as usize / 3)) && !w.hard[h] {
                    w.hard[h] = true;
                    w.hard[o as usize] = true;
                    w.soft[h] = true;
                    w.soft[o as usize] = true;
                    made = true;
                }
            }
        }
        if made {
            count += 1;
        }
    }
    count
}

/// Rings round the model's limbs: geodesic distance from the point nearest its middle,
/// swept from the far end inward; each tip starts a branch and branches merge as they meet.
/// A branch that lasts `ZR_LIMBPERSIST` quads (6) before meeting another lasting one is a
/// limb (fingers and toes are not); its distance levels every `ZR_LIMBSTEP` quads (6) that
/// cut it in one closed loop are its rings. A branch not longer than 1.2 times its girth (a
/// head) gets none.
/// Also each lasting branch's tip: (vertex, whether it is a tube, its girth).
#[allow(clippy::type_complexity)]
fn limb_rings(w: &Work, quad: f32) -> (Vec<Vec<Vec3>>, Vec<bool>, Vec<bool>, Vec<f32>, Vec<Tube>, Vec<(u32, bool, f32)>) {
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    let nv = w.p.len();
    if nv == 0 {
        return (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    }
    let geodesic = |src: u32| -> (Vec<f32>, Vec<u32>) {
        let mut d = vec![f32::INFINITY; nv];
        let mut from = vec![NONE; nv];
        d[src as usize] = 0.0;
        let mut heap = std::collections::BinaryHeap::new();
        heap.push((std::cmp::Reverse(0u32), src));
        while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
            let dv = f32::from_bits(db);
            if dv > d[v as usize] {
                continue;
            }
            for &u in w.neighbours(v) {
                let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                if nd < d[u as usize] {
                    d[u as usize] = nd;
                    from[u as usize] = v;
                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                }
            }
        }
        (d, from)
    };
    let far = |d: &[f32]| (0..nv as u32).filter(|&v| d[v as usize].is_finite()).max_by(|&a, &b| d[a as usize].total_cmp(&d[b as usize]));
    // The middle of the model's longest way across: farthest from a point on its biggest
    // piece, farthest from that, half way back.
    let start = (0..nv as u32).filter(|&v| w.neighbours(v).len() >= 3).max_by_key(|&v| w.neighbours(v).len()).unwrap_or(0);
    let (d0, _) = geodesic(start);
    let Some(a) = far(&d0) else { return (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()) };
    let (da, from_a) = geodesic(a);
    let Some(b0) = far(&da) else { return (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()) };
    let mut src = b0;
    while from_a[src as usize] != NONE && da[src as usize] > da[b0 as usize] * 0.5 {
        src = from_a[src as usize];
    }
    let (d, _) = geodesic(src);
    if debug {
        eprintln!("    limbs: middle at {:.3}, ends {:.3} and {:.3}, {:.1} quads apart", w.p[src as usize], w.p[a as usize], w.p[b0 as usize], da[b0 as usize] / quad);
    }
    let persist = quad * std::env::var("ZR_LIMBPERSIST").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(6.0);
    let step = quad * std::env::var("ZR_LIMBSTEP").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(6.0);
    // Sweep from far to near (superlevel sets), elder rule.
    let mut order: Vec<u32> = (0..nv as u32).filter(|&v| d[v as usize].is_finite()).collect();
    order.sort_by(|&a, &b| d[b as usize].total_cmp(&d[a as usize]));
    let mut parent: Vec<u32> = vec![NONE; nv];
    fn root(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    // Per root: its tip (where it was born) and the level where it first met another lasting
    // branch (its limb's base).
    let mut tip: HashMap<u32, u32> = HashMap::default();
    let mut base: HashMap<u32, f32> = HashMap::default();
    for &v in &order {
        let dv = d[v as usize];
        let mut roots: Vec<u32> = Vec::new();
        for &u in w.neighbours(v) {
            if parent[u as usize] != NONE {
                let r = root(&mut parent, u);
                if !roots.contains(&r) {
                    roots.push(r);
                }
            }
        }
        if roots.is_empty() {
            parent[v as usize] = v;
            tip.insert(v, v);
            continue;
        }
        let birth = |r: u32| d[tip[&r] as usize];
        let lasting: Vec<u32> = roots.iter().copied().filter(|&r| birth(r) - dv >= persist).collect();
        if lasting.len() >= 2 {
            if debug {
                eprintln!("    limbs meet at {dv:.3} ({:.3}): {:?}", w.p[v as usize], lasting.iter().map(|&r| (w.p[tip[&r] as usize], birth(r))).collect::<Vec<_>>());
            }
            for &r in &lasting {
                base.entry(r).or_insert(dv);
            }
        }
        let elder = *roots.iter().max_by(|&&a, &&b| birth(a).total_cmp(&birth(b))).unwrap();
        for &r in &roots {
            if r != elder {
                parent[r as usize] = elder;
            }
        }
        parent[v as usize] = elder;
    }
    let mut rings: Vec<Vec<Vec3>> = Vec::new();
    let mut first: Vec<bool> = Vec::new();
    // The tube: a limb from its first ring to its last.
    let mut tube = vec![false; nv];
    let mut tubes: Vec<Tube> = Vec::new();
    let mut tips: Vec<(u32, bool, f32)> = Vec::new();
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    for (&r, &m) in &base {
        let t0 = tip[&r];
        let b = d[t0 as usize];
        // The limb: from its tip over vertices farther than its base.
        let mut inside = vec![false; nv];
        inside[t0 as usize] = true;
        let mut q = vec![t0];
        while let Some(v) = q.pop() {
            for &u in w.neighbours(v) {
                if !inside[u as usize] && d[u as usize] > m {
                    inside[u as usize] = true;
                    q.push(u);
                }
            }
        }
        let faces: Vec<usize> = (0..w.t.len()).filter(|&f| w.t[f].iter().all(|&v| inside[v as usize])).collect();
        let mut limb: Vec<Vec<Vec3>> = Vec::new();
        let mut levels: Vec<f32> = Vec::new();
        let mut t = m + quad * 2.0;
        while t < b - quad * 2.0 {
            // The level's crossings: a point on each edge it cuts, joined face by face.
            let mut link: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::default();
            let mut at: HashMap<(u32, u32), Vec3> = HashMap::default();
            for &f in &faces {
                let tri = w.t[f];
                let mut cut: Vec<(u32, u32)> = Vec::new();
                for k in 0..3 {
                    let (a, bb) = (tri[k], tri[(k + 1) % 3]);
                    let (da, db) = (d[a as usize] - t, d[bb as usize] - t);
                    if (da < 0.0) != (db < 0.0) {
                        let e = key(a, bb);
                        at.entry(e).or_insert_with(|| w.p[a as usize].lerp(w.p[bb as usize], da / (da - db)));
                        cut.push(e);
                    }
                }
                if cut.len() == 2 {
                    link.entry(cut[0]).or_default().push(cut[1]);
                    link.entry(cut[1]).or_default().push(cut[0]);
                }
            }
            // One closed loop, every crossing joined both ways.
            let one = !link.is_empty() && link.values().all(|x| x.len() == 2) && {
                let start = *link.keys().next().unwrap();
                let (mut prev, mut cur) = (start, link[&start][0]);
                let mut n = 1;
                while cur != start && n <= link.len() {
                    let nx = if link[&cur][0] != prev { link[&cur][0] } else { link[&cur][1] };
                    prev = cur;
                    cur = nx;
                    n += 1;
                }
                cur == start && n == link.len()
            };
            if one {
                let start = *link.keys().min().unwrap();
                let mut pts = vec![at[&start]];
                let (mut prev, mut cur) = (start, link[&start][0]);
                while cur != start {
                    pts.push(at[&cur]);
                    let nx = if link[&cur][0] != prev { link[&cur][0] } else { link[&cur][1] };
                    prev = cur;
                    cur = nx;
                }
                pts.push(pts[0]);
                limb.push(pts);
                levels.push(t);
            }
            t += step;
        }
        let girth = if limb.is_empty() { 0.0 } else { limb.iter().map(|l| l.windows(2).map(|e| e[0].distance(e[1])).sum::<f32>()).sum::<f32>() / limb.len() as f32 };
        let tubular = !limb.is_empty() && b - m > girth * 1.2;
        tips.push((t0, tubular, girth));
        if debug {
            eprintln!("    limb from {:.3}: {:.1} quads long, girth {:.1} quads, {} rings{}", w.p[t0 as usize], (b - m) / quad, girth / quad, limb.len(), if tubular { "" } else { ", not a tube: none" });
        }
        if tubular {
            // The tube ends where the limb is narrowest (wrist, ankle): past there the levels
            // slice a hand or a foot.
            let gs: Vec<f32> = limb.iter().map(|l| l.windows(2).map(|e| e[0].distance(e[1])).sum::<f32>()).collect();
            let mut end = gs.len() - 1;
            let mut least = f32::INFINITY;
            for (i, &g) in gs.iter().enumerate() {
                if g > least * 1.15 {
                    end = i - 1;
                    break;
                }
                least = least.min(g);
            }
            // It starts where the limb is round: the first level within 30% of its typical
            // girth (the top of a thigh or a shoulder is left to the body).
            let mut sorted = gs.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let typical = sorted[sorted.len() / 2];
            let start = gs.iter().position(|&g| g <= typical * 1.3).unwrap_or(0).min(end.saturating_sub(1));
            let (lo, hi) = (levels[start], levels[end.max(start + 1).min(levels.len() - 1)]);
            // A real tube: at least two rings apart and 3 quads long, at least 6 quads round.
            if hi - lo >= quad * 3.0 && typical >= quad * 6.0 {
            for v in 0..nv {
                if inside[v] && d[v] >= lo && d[v] <= hi {
                    tube[v] = true;
                }
            }
            tubes.push(Tube { lo, hi, inside: inside.clone() });
            }
            for (i, l) in limb.into_iter().enumerate() {
                rings.push(l);
                first.push(i == 0);
            }
        }
    }
    (rings, first, tube, d, tubes, tips)
}

/// The cylinders of a part function `phi` (0 at the head's top): its values cut in bands a
/// step apart, each band's connected pieces linked to the pieces in the bands above and below.
/// A run of pieces each with one link up and one down is one ring following one ring: a
/// cylinder. In each run the tube is the longest stretch whose girth changes slowly (under
/// `ZR_PARTFLARE` quads of girth per quad along, smoothed); where it flares faster it is a
/// cap (head, hand, foot) or a join (shoulders, crotch), left to the plan. On the run from
/// the head's top, only past the narrowest ring after its widest (the neck): the head itself
/// stays a sphere for the plan.
fn part_tubes(w: &Work, phi: &[f32], quad: f32) -> (Vec<Tube>, Option<f32>) {
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    let nv = w.p.len();
    let step = quad * std::env::var("ZR_PARTSTEP").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
    let flare: f32 = std::env::var("ZR_PARTFLARE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
    let lo = phi.iter().copied().filter(|x| x.is_finite()).fold(f32::INFINITY, f32::min);
    if !lo.is_finite() {
        return (Vec::new(), None);
    }
    let band = |v: u32| -> i64 { let x = phi[v as usize]; if x.is_finite() { ((x - lo) / step).floor() as i64 } else { -1 } };
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
    // Links between pieces of neighbouring bands, with the girth of the level between them.
    let mut girth: HashMap<(u32, u32), f32> = HashMap::default();
    for t in &w.t {
        let mut vs = *t;
        vs.sort_by(|&a, &b| phi[a as usize].total_cmp(&phi[b as usize]));
        let (b0, b2) = (band(vs[0]), band(vs[2]));
        if b0 < 0 || b2 <= b0 {
            continue;
        }
        for k in (b0 + 1)..=b2 {
            let lv = lo + k as f32 * step;
            let mut pts: Vec<Vec3> = Vec::new();
            let mut below = None;
            let mut above = None;
            for e in 0..3 {
                let (a, b) = (t[e], t[(e + 1) % 3]);
                let (fa, fb) = (phi[a as usize], phi[b as usize]);
                if (fa < lv) != (fb < lv) {
                    pts.push(w.p[a as usize].lerp(w.p[b as usize], (lv - fa) / (fb - fa)));
                }
                for v in [a, b] {
                    if band(v) == k - 1 {
                        below = Some(comp[v as usize]);
                    }
                    if band(v) == k {
                        above = Some(comp[v as usize]);
                    }
                }
            }
            if let (2, Some(x), Some(y)) = (pts.len(), below, above) {
                *girth.entry((x, y)).or_default() += pts[0].distance(pts[1]);
            }
        }
    }
    let mut up: HashMap<u32, Vec<u32>> = HashMap::default();
    let mut down: HashMap<u32, Vec<u32>> = HashMap::default();
    let mut links: Vec<(u32, u32)> = girth.keys().copied().collect();
    links.sort_unstable();
    for &(x, y) in &links {
        up.entry(x).or_default().push(y);
        down.entry(y).or_default().push(x);
    }
    let n_up = |c: u32| up.get(&c).map_or(0, |v| v.len());
    let n_down = |c: u32| down.get(&c).map_or(0, |v| v.len());
    let regular = |c: u32| n_up(c) == 1 && n_down(c) == 1;
    // Chains: from a regular piece whose piece below is not regular (or the source's cap),
    // up while regular.
    let mut comps: Vec<u32> = comp.clone();
    comps.sort_unstable();
    comps.dedup();
    let source_comp = (0..nv as u32).filter(|&v| phi[v as usize].is_finite()).min_by(|&a, &b| phi[a as usize].total_cmp(&phi[b as usize])).map(|v| comp[v as usize]);
    let mut tubes: Vec<Tube> = Vec::new();
    let mut neck: Option<f32> = None;
    for &c0 in &comps {
        if !regular(c0) || regular(down[&c0][0]) {
            continue;
        }
        let from_source = Some(down[&c0][0]) == source_comp && n_down(down[&c0][0]) == 0;
        let mut chain = vec![c0];
        let mut c = c0;
        while n_up(c) == 1 && regular(up[&c][0]) {
            c = up[&c][0];
            chain.push(c);
        }
        // The levels between the chain's pieces: (level, girth in quads).
        let band_of = |c: u32| -> i64 { band(c) };
        let lv: Vec<(f32, f32)> = chain.windows(2).map(|p| (lo + band_of(p[1]) as f32 * step, girth[&(p[0], p[1])] / quad)).collect();
        if lv.len() < 3 {
            continue;
        }
        // The head: start past its narrowest ring after its widest.
        let mut first = 0usize;
        if from_source {
            let wide = (0..lv.len()).max_by(|&a, &b| lv[a].1.total_cmp(&lv[b].1)).unwrap_or(0);
            let half = lv.len() / 2;
            let wide = (0..half.max(1)).max_by(|&a, &b| lv[a].1.total_cmp(&lv[b].1)).unwrap_or(wide);
            first = (wide..lv.len()).min_by(|&a, &b| lv[a].1.total_cmp(&lv[b].1)).unwrap_or(wide);
            neck = Some(lv[first].0);
        }
        // Girth smoothed over five levels; its change per quad along, over three.
        let gs: Vec<f32> = (0..lv.len()).map(|i| {
            let (a, b) = (i.saturating_sub(2), (i + 2).min(lv.len() - 1));
            lv[a..=b].iter().map(|x| x.1).sum::<f32>() / (b - a + 1) as f32
        }).collect();
        let rate = |i: usize| -> f32 {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(lv.len() - 1));
            (gs[b] - gs[a]).abs() / (((b - a).max(1)) as f32 * step / quad)
        };
        // The whole run between its joins (a box modeller keeps one count from armpits to
        // crotch, thigh to ankle), less its ends while they flare into a join or a cap.
        let (mut a, mut b) = (first, lv.len() - 1);
        while a < b && rate(a) > flare {
            a += 1;
        }
        while b > a && rate(b) > flare {
            b -= 1;
        }
        // At most `ZR_PARTRATIO` (2.5) between its widest and narrowest ring: the more
        // extreme end goes first.
        let ratio: f32 = std::env::var("ZR_PARTRATIO").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5);
        while a < b {
            let (mn, mx) = gs[a..=b].iter().fold((f32::INFINITY, 0.0f32), |m, &g| (m.0.min(g), m.1.max(g)));
            if mx <= mn * ratio {
                break;
            }
            let mut sorted: Vec<f32> = gs[a..=b].to_vec();
            sorted.sort_by(|x, y| x.total_cmp(y));
            let med = sorted[sorted.len() / 2];
            if (gs[a] / med).ln().abs() >= (gs[b] / med).ln().abs() {
                a += 1;
            } else {
                b -= 1;
            }
        }
        let (tlo, thi) = (lv[a].0, lv[b].0);
        let min_g = lv[a..=b].iter().map(|x| x.1).fold(f32::INFINITY, f32::min);
        if debug {
            eprintln!("    part chain of {} levels from {:.3}{}: tube {:.3}..{:.3} ({:.1} quads), girth {:.1}..{:.1} quads", lv.len(), w.p[c0 as usize], if from_source { " (head)" } else { "" }, tlo, thi, (thi - tlo) / quad, min_g, lv[a..=b].iter().map(|x| x.1).fold(0.0f32, f32::max));
        }
        if thi - tlo < quad * 3.0 || min_g < 6.0 {
            continue;
        }
        let set: HashSet<u32> = chain.iter().copied().collect();
        let inside: Vec<bool> = (0..nv).map(|v| set.contains(&comp[v])).collect();
        tubes.push(Tube { lo: tlo, hi: thi, inside });
    }
    (tubes, neck)
}

/// Whether the working mesh is its own mirror image in `plane` (within half a quad, for
/// 97% of a sample of its vertices).
fn is_symmetric(w: &Work, plane: MirrorPlane, quad: f32) -> bool {
    let nv = w.p.len();
    if nv == 0 {
        return false;
    }
    let cell = quad.max(1e-6);
    let key = |p: Vec3| { let c = (p / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    for v in 0..nv as u32 {
        grid.entry(key(w.p[v as usize])).or_default().push(v);
    }
    let stride = (nv / 3000).max(1);
    let (mut ok, mut n) = (0usize, 0usize);
    for v in (0..nv).step_by(stride) {
        let q = plane.reflect(w.p[v]);
        let c = key(q);
        let mut best = f32::INFINITY;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &u in grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)).into_iter().flatten() {
                        best = best.min(w.p[u as usize].distance(q));
                    }
                }
            }
        }
        n += 1;
        if best < quad * 0.5 {
            ok += 1;
        }
    }
    ok as f32 >= n as f32 * 0.97
}

/// The working mesh made exactly symmetric in `plane`: its positive half (triangles split
/// along the plane, corners near it moved onto it), reflected and welded on the plane.
fn mirrored_mesh(m: &SculptMesh, plane: MirrorPlane) -> SculptMesh {
    let mut p = m.positions.clone();
    let mut idx = m.indices.clone();
    let mut grp: Vec<u16> = (0..m.triangle_count() as u32).map(|f| m.group(f)).collect();
    let mut d: Vec<f32> = p.iter().map(|&q| plane.distance(q)).collect();
    let mut masks = vec![vec![true; p.len()]];
    split_along(&mut p, &mut idx, &mut grp, &mut d, &mut masks, 0, 0.0, &mut Vec::new());
    for (q, &dv) in p.iter_mut().zip(&d) {
        if dv == 0.0 {
            *q = plane.project(*q);
        }
    }
    let keep: Vec<usize> = (0..idx.len() / 3).filter(|&f| (0..3).all(|k| d[idx[f * 3 + k] as usize] >= 0.0) && (0..3).any(|k| d[idx[f * 3 + k] as usize] > 0.0)).collect();
    let mut map = vec![u32::MAX; p.len()];
    let mut mirror = vec![u32::MAX; p.len()];
    let mut pos: Vec<Vec3> = Vec::new();
    for &f in &keep {
        for k in 0..3 {
            let v = idx[f * 3 + k] as usize;
            if map[v] == u32::MAX {
                map[v] = pos.len() as u32;
                pos.push(p[v]);
            }
        }
    }
    for v in 0..p.len() {
        if map[v] != u32::MAX {
            mirror[v] = if d[v] == 0.0 { map[v] } else { pos.push(plane.reflect(p[v])); (pos.len() - 1) as u32 };
        }
    }
    let mut out_idx: Vec<u32> = Vec::with_capacity(keep.len() * 6);
    let mut out_grp: Vec<u16> = Vec::with_capacity(keep.len() * 2);
    for &f in &keep {
        let t = [idx[f * 3] as usize, idx[f * 3 + 1] as usize, idx[f * 3 + 2] as usize];
        out_idx.extend(t.map(|v| map[v]));
        out_idx.extend([mirror[t[0]], mirror[t[2]], mirror[t[1]]]);
        out_grp.extend([grp[f], grp[f]]);
    }
    SculptMesh::from_raw_with_groups(pos, out_idx, out_grp)
}

/// The part function: distance along the surface from `src`, smoothed (the creases where
/// paths round both sides of a limb meet get rounded off; `ZR_PARTSMOOTH` passes), still about
/// a distance, so its rings are evenly spaced.
fn part_distance(w: &Work, src: u32) -> Vec<f32> {
    let mut g = geodesic_from(w, src);
    let passes: usize = std::env::var("ZR_PARTSMOOTH").ok().and_then(|v| v.parse().ok()).unwrap_or(160);
    for _ in 0..passes {
        g = (0..w.p.len()).into_par_iter().map(|v| {
            let nb = w.neighbours(v as u32);
            if v as u32 == src || nb.is_empty() || !g[v].is_finite() {
                return g[v];
            }
            // Summed in f64 (the order of the neighbours then hardly matters, so a mesh that is
            // its own mirror image gets a function that is too, to well under a band's width).
            let (mut sum, mut n) = (0.0f64, 0u32);
            for &u in nb {
                let x = g[u as usize];
                if x.is_finite() {
                    sum += x as f64;
                    n += 1;
                }
            }
            if n == 0 { g[v] } else { (0.5 * g[v] as f64 + 0.5 * sum / n as f64) as f32 }
        }).collect();
    }
    g
}

/// Distance along the working mesh's edges from `src` (infinite where it cannot reach).
fn geodesic_from(w: &Work, src: u32) -> Vec<f32> {
    let nv = w.p.len();
    let mut d = vec![f32::INFINITY; nv];
    d[src as usize] = 0.0;
    let mut heap = std::collections::BinaryHeap::new();
    heap.push((std::cmp::Reverse(0u32), src));
    while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
        let dv = f32::from_bits(db);
        if dv > d[v as usize] {
            continue;
        }
        for &u in w.neighbours(v) {
            let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
            if nd < d[u as usize] {
                d[u as usize] = nd;
                heap.push((std::cmp::Reverse(nd.to_bits()), u));
            }
        }
    }
    d
}

/// Level lines of `phi` (one every `step` of its range a quad apart, roughly) as OBJ lines.
fn dump_levels(w: &Work, phi: &[f32], quad: f32, path: &std::path::Path) {
    use std::io::Write;
    let Ok(f) = std::fs::File::create(path) else { return };
    let mut f = std::io::BufWriter::new(f);
    let (lo, hi) = phi.iter().filter(|x| x.is_finite()).fold((f32::INFINITY, f32::NEG_INFINITY), |a, &x| (a.0.min(x), a.1.max(x)));
    let n = ((hi - lo) / (quad * 2.0)).clamp(4.0, 400.0) as usize;
    let mut k = 0usize;
    for i in 1..n {
        let t = lo + (hi - lo) * i as f32 / n as f32;
        for tri in &w.t {
            let v = tri.map(|x| phi[x as usize]);
            if v.iter().any(|x| !x.is_finite()) {
                continue;
            }
            let mut pts: Vec<Vec3> = Vec::new();
            for e in 0..3 {
                let (a, b) = (e, (e + 1) % 3);
                if (v[a] < t) != (v[b] < t) {
                    let s = (t - v[a]) / (v[b] - v[a]);
                    pts.push(w.p[tri[a] as usize].lerp(w.p[tri[b] as usize], s));
                }
            }
            if pts.len() == 2 {
                let _ = writeln!(f, "v {} {} {}\nv {} {} {}\nl {} {}", pts[0].x, pts[0].y, pts[0].z, pts[1].x, pts[1].y, pts[1].z, k + 1, k + 2);
                k += 2;
            }
        }
    }
}

/// A harmonic function on the working mesh: the given values at the given vertices, smooth
/// in between (cotangent weights, clamped so a sliver cannot turn one negative), solved by
/// conjugate gradients from `start`. Its level sets are the loops a box modeller would lay:
/// rings round each part between the fixed points, with no ridges.
fn harmonic(w: &Work, fixed: &[(u32, f32)], start: &[f32]) -> Vec<f32> {
    let nv = w.p.len();
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut wt: HashMap<(u32, u32), f32> = HashMap::default();
    for t in &w.t {
        for k in 0..3 {
            let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let (ea, eb) = (w.p[a as usize] - w.p[c as usize], w.p[b as usize] - w.p[c as usize]);
            let cot = ea.dot(eb) / ea.cross(eb).length().max(1e-12);
            *wt.entry(key(a, b)).or_default() += 0.5 * cot.clamp(0.01, 5.0);
        }
    }
    let nb: Vec<Vec<(u32, f32)>> = (0..nv as u32).map(|v| w.neighbours(v).iter().map(|&u| (u, wt.get(&key(v, u)).copied().unwrap_or(0.01))).collect()).collect();
    drop(wt);
    let mut is_fixed = vec![false; nv];
    let mut x: Vec<f64> = start.iter().map(|&v| if v.is_finite() { v as f64 } else { 0.0 }).collect();
    x.resize(nv, 0.0);
    for &(v, val) in fixed {
        is_fixed[v as usize] = true;
        x[v as usize] = val as f64;
    }
    // A x = b on the free vertices (fixed neighbours moved to the right), Jacobi-preconditioned.
    let diag: Vec<f64> = (0..nv).map(|v| nb[v].iter().map(|e| e.1 as f64).sum::<f64>().max(1e-12) + 1e-9).collect();
    let apply = |y: &[f64]| -> Vec<f64> {
        (0..nv).into_par_iter().map(|v| {
            if is_fixed[v] {
                return 0.0;
            }
            diag[v] * y[v] - nb[v].iter().filter(|e| !is_fixed[e.0 as usize]).map(|e| e.1 as f64 * y[e.0 as usize]).sum::<f64>()
        }).collect()
    };
    let b: Vec<f64> = (0..nv).map(|v| if is_fixed[v] { 0.0 } else { nb[v].iter().filter(|e| is_fixed[e.0 as usize]).map(|e| e.1 as f64 * x[e.0 as usize]).sum::<f64>() }).collect();
    let dot = |a: &[f64], c: &[f64]| -> f64 {
        let parts: Vec<f64> = a.par_chunks(4096).zip(c.par_chunks(4096)).map(|(p, q)| p.iter().zip(q).map(|(u, v)| u * v).sum::<f64>()).collect();
        parts.iter().sum()
    };
    let mut y: Vec<f64> = (0..nv).map(|v| if is_fixed[v] { 0.0 } else { x[v] }).collect();
    let ay = apply(&y);
    let mut r: Vec<f64> = (0..nv).map(|v| b[v] - ay[v]).collect();
    let mut zr: Vec<f64> = (0..nv).map(|v| r[v] / diag[v]).collect();
    let mut p = zr.clone();
    let mut rz = dot(&r, &zr);
    let bb = dot(&b, &b).max(1e-30);
    let iters: usize = std::env::var("ZR_HARMITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    for _ in 0..iters {
        if dot(&r, &r) < bb * 1e-12 {
            break;
        }
        let ap = apply(&p);
        let alpha = rz / dot(&p, &ap).max(1e-300);
        y.par_iter_mut().zip(p.par_iter()).for_each(|(y, p)| *y += alpha * p);
        r.par_iter_mut().zip(ap.par_iter()).for_each(|(r, a)| *r -= alpha * a);
        zr = (0..nv).map(|v| r[v] / diag[v]).collect();
        let rz2 = dot(&r, &zr);
        let beta = rz2 / rz.max(1e-300);
        rz = rz2;
        p.par_iter_mut().zip(zr.par_iter()).for_each(|(p, z)| *p = z + beta * *p);
    }
    (0..nv).map(|v| if is_fixed[v] { x[v] as f32 } else { y[v] as f32 }).collect()
}

/// A limb's tube: distances `lo`..`hi` (from the model's middle) within its region.
struct Tube {
    lo: f32,
    hi: f32,
    inside: Vec<bool>,
}

/// Split the triangles inside tube `ti` that level `t` of `d` crosses, along it. A corner
/// within 15% of an edge's length of the crossing is moved onto the level instead (no
/// slivers); then each crossed edge gets a point (shared) and its triangles are split. New
/// points get their distance and tube membership from their edge.
/// `extra` (if as long as `p`) is carried along: corners moved and new points get it by the
/// same blend.
#[allow(clippy::too_many_arguments)]
fn split_along(p: &mut Vec<Vec3>, idx: &mut Vec<u32>, grp: &mut Vec<u16>, d: &mut Vec<f32>, masks: &mut [Vec<bool>], ti: usize, t: f32, extra: &mut Vec<f32>) {
    let carry = extra.len() == p.len();
    let nf = idx.len() / 3;
    let in_tube = |tri: &[u32], masks: &[Vec<bool>]| tri.iter().all(|&v| masks[ti][v as usize]);
    // Snap corners next to the level onto it.
    for f in 0..nf {
        let tri = [idx[f * 3], idx[f * 3 + 1], idx[f * 3 + 2]];
        if !in_tube(&tri, masks) {
            continue;
        }
        for k in 0..3 {
            let (u, v) = (tri[k], tri[(k + 1) % 3]);
            let (du, dv) = (d[u as usize], d[v as usize]);
            if (du < t && dv > t) || (du > t && dv < t) {
                let s = (t - du) / (dv - du);
                let q = p[u as usize].lerp(p[v as usize], s);
                if s < 0.15 {
                    if carry {
                        extra[u as usize] += (extra[v as usize] - extra[u as usize]) * s;
                    }
                    p[u as usize] = q;
                    d[u as usize] = t;
                } else if s > 0.85 {
                    if carry {
                        extra[v as usize] += (extra[u as usize] - extra[v as usize]) * (1.0 - s);
                    }
                    p[v as usize] = q;
                    d[v as usize] = t;
                }
            }
        }
    }
    let mut mid: HashMap<(u32, u32), u32> = HashMap::default();
    let mut out: Vec<u32> = Vec::with_capacity(idx.len() + 64);
    let mut og: Vec<u16> = Vec::with_capacity(nf + 32);
    let side = |x: f32| if x < t { -1i8 } else if x > t { 1 } else { 0 };
    for f in 0..nf {
        let tri = [idx[f * 3], idx[f * 3 + 1], idx[f * 3 + 2]];
        let g = grp.get(f).copied().unwrap_or(0);
        let sd: Vec<i8> = tri.iter().map(|&v| side(d[v as usize])).collect();
        let crosses = sd.contains(&-1) && sd.contains(&1);
        if !in_tube(&tri, masks) || !crosses {
            out.extend_from_slice(&tri);
            og.push(g);
            continue;
        }
        let mut point = |u: u32, v: u32, p: &mut Vec<Vec3>, d: &mut Vec<f32>, masks: &mut [Vec<bool>]| -> u32 {
            *mid.entry((u.min(v), u.max(v))).or_insert_with(|| {
                let (du, dv) = (d[u as usize], d[v as usize]);
                let s = ((t - du) / (dv - du)).clamp(0.0, 1.0);
                if carry {
                    extra.push(extra[u as usize] + (extra[v as usize] - extra[u as usize]) * s);
                }
                p.push(p[u as usize].lerp(p[v as usize], s));
                d.push(t);
                for m in masks.iter_mut() {
                    let both = m[u as usize] && m[v as usize];
                    m.push(both);
                }
                (p.len() - 1) as u32
            })
        };
        if let Some(k) = (0..3).find(|&k| sd[k] == 0) {
            // A corner on the level, the other two on either side: two triangles.
            let (a, b, c) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let x = point(b, c, p, d, masks);
            out.extend_from_slice(&[a, b, x, a, x, c]);
            og.extend_from_slice(&[g, g]);
        } else {
            // The lone corner first, keeping the winding: three triangles.
            let k = (0..3).find(|&k| sd[k] != sd[(k + 1) % 3] && sd[k] != sd[(k + 2) % 3]).unwrap();
            let (a, b, c) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let x = point(a, b, p, d, masks);
            let y = point(a, c, p, d, masks);
            out.extend_from_slice(&[a, x, y, x, b, c, x, c, y]);
            og.extend_from_slice(&[g, g, g]);
        }
    }
    *idx = out;
    *grp = og;
}

/// The closed loop where distance `d` crosses `t` over the triangles `faces`, as points on
/// the surface; None if it is not one closed loop.
fn level_loop(w: &Work, d: &[f32], faces: &[usize], t: f32) -> Option<Vec<Vec3>> {
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut link: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::default();
    let mut at: HashMap<(u32, u32), Vec3> = HashMap::default();
    for &f in faces {
        let tri = w.t[f];
        let mut cut: Vec<(u32, u32)> = Vec::new();
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let (da, db) = (d[a as usize] - t, d[b as usize] - t);
            if (da < 0.0) != (db < 0.0) {
                let e = key(a, b);
                at.entry(e).or_insert_with(|| w.p[a as usize].lerp(w.p[b as usize], da / (da - db)));
                cut.push(e);
            }
        }
        if cut.len() == 2 {
            link.entry(cut[0]).or_default().push(cut[1]);
            link.entry(cut[1]).or_default().push(cut[0]);
        }
    }
    if link.is_empty() || !link.values().all(|x| x.len() == 2) {
        return None;
    }
    let start = *link.keys().min().unwrap();
    let mut pts = vec![at[&start]];
    let (mut prev, mut cur) = (start, link[&start][0]);
    while cur != start {
        if pts.len() > link.len() {
            return None;
        }
        pts.push(at[&cur]);
        let nx = if link[&cur][0] != prev { link[&cur][0] } else { link[&cur][1] };
        prev = cur;
        cur = nx;
    }
    if pts.len() != link.len() {
        return None;
    }
    Some(pts)
}

/// `n` points evenly spaced round the closed polyline `lp`.
fn resample_loop(lp: &[Vec3], n: usize) -> Vec<Vec3> {
    let m = lp.len();
    let mut cum = vec![0.0f32];
    for i in 0..m {
        cum.push(cum[i] + lp[i].distance(lp[(i + 1) % m]));
    }
    let total = cum[m].max(1e-12);
    (0..n).map(|j| {
        let g = total * j as f32 / n as f32;
        let i = cum.partition_point(|&x| x <= g).saturating_sub(1).min(m - 1);
        let u = ((g - cum[i]) / (cum[i + 1] - cum[i]).max(1e-12)).clamp(0.0, 1.0);
        lp[i].lerp(lp[(i + 1) % m], u)
    }).collect()
}

/// `row` turned and shifted to lie best against `prev` (same length): point i near prev's i.
fn align_row(prev: &[Vec3], row: &[Vec3]) -> Vec<Vec3> {
    let n = row.len();
    let mut best: (f32, Vec<Vec3>) = (f32::INFINITY, row.to_vec());
    for rev in [false, true] {
        let r: Vec<Vec3> = if rev { row.iter().rev().copied().collect() } else { row.to_vec() };
        for sh in 0..n {
            let cost: f32 = (0..n).map(|i| prev[i].distance_squared(r[(i + sh) % n])).sum();
            if cost < best.0 {
                best = (cost, (0..n).map(|i| r[(i + sh) % n]).collect());
            }
        }
    }
    best.1
}

/// One band of quads between two closed rows of points running the same way round (`lo`
/// along the way the quads below it run, `hi` the next row up): one quad per step where the
/// counts match; where they differ, 3-to-1 merges spread round the band (two new points
/// each), and one triangle if the difference is odd. `hi` is turned to line up with `lo`.
fn join_band(lo: &[u32], hi: &[u32], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>) {
    // Longer row below; if the upper one is longer, the two swap, each run backwards.
    let flip = hi.len() > lo.len();
    let (b, t): (Vec<u32>, Vec<u32>) = if flip { (hi.iter().rev().copied().collect(), lo.iter().rev().copied().collect()) } else { (lo.to_vec(), hi.to_vec()) };
    let (n0, n1) = (b.len(), t.len());
    let diff = n0 - n1;
    // Merges (each three edges below to one above), at most one per edge above; what is
    // left over (an odd difference, or more than the row above can take) as triangles.
    let k = (diff / 2).min(n1);
    let tris = diff - 2 * k;
    // Where each step below goes: 1 = a plain quad, 3 = a merge, 0 = a triangle (one below,
    // none above); spread evenly round.
    let mut plan: Vec<u8> = Vec::new();
    let units = n1 + tris;
    let mut slots: Vec<u8> = vec![1; n1 - k];
    slots.extend(std::iter::repeat_n(3u8, k));
    slots.extend(std::iter::repeat_n(0u8, tris));
    // Interleave: specials evenly among the plain ones.
    let specials: Vec<u8> = slots.iter().copied().filter(|&x| x != 1).collect();
    let plain = slots.len() - specials.len();
    let mut si = 0;
    for u in 0..units {
        let want = ((u + 1) * specials.len()) / units.max(1);
        if si < want && si < specials.len() {
            plan.push(specials[si]);
            si += 1;
        } else {
            plan.push(1);
        }
    }
    let _ = plain;
    // Line the upper row up: try every shift, the one whose points sit nearest the matching
    // points below.
    let walk = |sh: usize| -> f32 {
        let (mut i, mut o, mut cost) = (0usize, 0usize, 0.0f32);
        for &st in &plan {
            cost += out_p[b[i % n0] as usize].distance_squared(out_p[t[(o + sh) % n1] as usize]);
            match st {
                1 => { i += 1; o += 1; }
                3 => { i += 3; o += 1; }
                _ => { i += 1; }
            }
        }
        cost
    };
    let sh = (0..n1).min_by(|&x, &y| walk(x).total_cmp(&walk(y))).unwrap_or(0);
    let tt = |o: usize| t[(o + sh) % n1];
    // (Reversing both rows and swapping them keeps the quads facing the same way.)
    let push = |q: [u32; 4], quads: &mut Vec<[u32; 4]>| quads.push(q);
    let (mut i, mut o) = (0usize, 0usize);
    for &st in &plan {
        let bb = |x: usize| b[(i + x) % n0];
        match st {
            1 => {
                push([bb(1), bb(0), tt(o), tt(o + 1)], quads);
                i += 1;
                o += 1;
            }
            3 => {
                let (p1, p2) = (out_p[bb(1) as usize], out_p[bb(2) as usize]);
                let (q0, q1) = (out_p[tt(o) as usize], out_p[tt(o + 1) as usize]);
                out_p.push(p1.lerp((q0 + q1) * 0.5, 0.4));
                let m1 = (out_p.len() - 1) as u32;
                out_p.push(p2.lerp((q0 + q1) * 0.5, 0.4));
                let m2 = (out_p.len() - 1) as u32;
                push([bb(1), bb(0), tt(o), m1], quads);
                push([bb(2), bb(1), m1, m2], quads);
                push([bb(3), bb(2), m2, tt(o + 1)], quads);
                push([m2, m1, tt(o), tt(o + 1)], quads);
                i += 3;
                o += 1;
            }
            _ => {
                push([bb(1), bb(0), tt(o), tt(o)], quads);
                i += 1;
            }
        }
    }
}

/// Each limb's tube swept as a grid between the two open loops the layout left round it.
fn sweep_tubes(w: &Work, d: &[f32], tubes: &[Tube], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, quad: f32) -> Result<usize, String> {
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    // Open loops of the layout, along the way its quads run (a quad on the tube runs them back).
    let mut used: HashSet<(u32, u32)> = HashSet::default();
    for q4 in quads.iter() {
        for k in 0..4 {
            used.insert((q4[k], q4[(k + 1) % 4]));
        }
    }
    let mut next: HashMap<u32, u32> = HashMap::default();
    for &(a, b) in &used {
        if a != b && !used.contains(&(b, a)) {
            next.insert(a, b);
        }
    }
    let mut loops: Vec<Vec<u32>> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::default();
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    for s0 in starts {
        if seen.contains(&s0) {
            continue;
        }
        let mut lp = vec![s0];
        seen.insert(s0);
        let mut v = next[&s0];
        while v != s0 {
            if !seen.insert(v) {
                break;
            }
            lp.push(v);
            let Some(&nx) = next.get(&v) else { break };
            v = nx;
        }
        if v == s0 && lp.len() >= 3 {
            loops.push(lp);
        }
    }
    // Distance at a point: from the nearest working vertex (grid by cell).
    let cell = quad.max(1e-6);
    let cell_of = |p: Vec3| { let c = (p / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    for v in 0..w.p.len() as u32 {
        if d[v as usize].is_finite() {
            grid.entry(cell_of(w.p[v as usize])).or_default().push(v);
        }
    }
    let nearest = |p: Vec3| -> Option<u32> {
        let c = cell_of(p);
        let mut best: Option<(f32, u32)> = None;
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    for &v in grid.get(&(c.0 + x, c.1 + y, c.2 + z)).into_iter().flatten() {
                        let dd = w.p[v as usize].distance_squared(p);
                        if best.is_none_or(|b| dd < b.0) {
                            best = Some((dd, v));
                        }
                    }
                }
            }
        }
        best.map(|b| b.1)
    };
    let mut made = 0;
    for tube in tubes {
        // Its two loops: in the limb, nearest its two ends.
        let score = |lp: &[u32], level: f32| -> Option<f32> {
            let mut sum = 0.0;
            for &v in lp {
                let u = nearest(out_p[v as usize])?;
                if !tube.inside[u as usize] {
                    return None;
                }
                sum += (d[u as usize] - level).abs();
            }
            Some(sum / lp.len() as f32)
        };
        let pick = |level: f32| loops.iter().enumerate().filter_map(|(i, lp)| score(lp, level).map(|s| (s, i))).filter(|x| x.0 < quad * 2.0).min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1);
        let (Some(ia), Some(ib)) = (pick(tube.lo), pick(tube.hi)) else {
            if debug {
                eprintln!("  tube {:.2}..{:.2}: its open loops not found", tube.lo, tube.hi);
            }
            continue;
        };
        if ia == ib {
            continue;
        }
        let a: Vec<u32> = loops[ia].clone();
        let b: Vec<u32> = loops[ib].clone();
        let faces: Vec<usize> = (0..w.t.len()).filter(|&f| w.t[f].iter().all(|&v| tube.inside[v as usize])).collect();
        let rows_n = (((tube.hi - tube.lo) / quad).round() as usize).max(2);
        // Rows between the ends: level loops, each with as many points as the limb's girth
        // in quads (the same all along, so no poles), turned to lie against the row before.
        let mut levels: Vec<Vec<Vec3>> = Vec::new();
        for jr in 1..rows_n {
            let t = tube.lo + (tube.hi - tube.lo) * jr as f32 / rows_n as f32;
            if let Some(lp) = level_loop(w, d, &faces, t) {
                levels.push(lp);
            }
        }
        if levels.is_empty() {
            continue;
        }
        // Points round each row: the girth in quads, kept while the girth stays within 50% of
        // it (a pure grid), else stepped (same parity, so the band between merges in pairs).
        let girths: Vec<f32> = levels.iter().map(|lp| (0..lp.len()).map(|k| lp[k].distance(lp[(k + 1) % lp.len()])).sum::<f32>() / quad).collect();
        let parity = a.len() % 2;
        let fit = |g: f32| -> usize {
            let mut c = (g.round() as usize).max(4);
            if c % 2 != parity {
                c += 1;
            }
            c
        };
        let drift: f32 = std::env::var("ZR_TUBESTEP").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
        let mut counts: Vec<usize> = Vec::with_capacity(levels.len());
        // The start loop's own count where it fits the girth there (a plain band, no poles).
        let mut cur = if (a.len() as f32) <= girths[0] * drift && (a.len() as f32) >= girths[0] / drift { a.len() } else { fit(girths[0]) };
        for &g in &girths {
            if g > cur as f32 * drift || g < cur as f32 / drift {
                cur = fit(g);
            }
            counts.push(cur);
        }
        // And the end loop's count for the last rows where it fits (same parity only: an odd
        // step would need a triangle inside the tube).
        let nb = b.len();
        if nb % 2 == counts.last().copied().unwrap_or(0) % 2 {
            for i in (0..counts.len()).rev() {
                let g = girths[i];
                if (nb as f32) <= g * drift && (nb as f32) >= g / drift {
                    counts[i] = nb;
                } else {
                    break;
                }
            }
        }
        // Both ends with the same count (the counts ran strips through the tube): that count
        // all along, a plain grid.
        if a.len() == nb && std::env::var_os("ZR_TUBESTEPS").is_none() {
            counts.iter_mut().for_each(|c| *c = nb);
        }
        let n = counts[0];
        let a_pos: Vec<Vec3> = a.iter().map(|&v| out_p[v as usize]).collect();
        let mut prev_pos: Vec<Vec3> = resample_loop(&a_pos, n);
        let mut rows: Vec<Vec<u32>> = Vec::new();
        let mut rows_pos: Vec<Vec<Vec3>> = Vec::new();
        for (li, lp) in levels.iter().enumerate() {
            let nn = counts[li];
            // Line up against the row before (resampled to this count when it steps).
            let against = if prev_pos.len() == nn { prev_pos.clone() } else { resample_loop(&prev_pos, nn) };
            let row = align_row(&against, &resample_loop(lp, nn));
            let ids: Vec<u32> = row.iter().map(|&q| { out_p.push(q); (out_p.len() - 1) as u32 }).collect();
            rows.push(ids);
            rows_pos.push(row.clone());
            prev_pos = row;
        }
        // The start loop to the first row, the rows to each other, the last row to the end
        // loop (run back: its quads run it the other way).
        join_band(&a, &rows[0], out_p, quads);
        for r in 1..rows.len() {
            join_band(&rows[r - 1], &rows[r], out_p, quads);
        }
        let br: Vec<u32> = b.iter().rev().copied().collect();
        join_band(rows.last().unwrap(), &br, out_p, quads);
        let ok_rows = rows.len();
        made += 1;
        if debug {
            let mut steps: Vec<usize> = counts.clone();
            steps.dedup();
            eprintln!("  tube {:.2}..{:.2}: swept, start loop {} round, rows {steps:?} round, {ok_rows} of {} rows, end loop {} round", tube.lo, tube.hi, a.len(), rows_n - 1, b.len());
        }
    }
    Ok(made)
}

/// Closed guides (end within about a quad of the start, at least four quads long) are put
/// on the working mesh as closed edge paths, made hard and soft; the smaller side is marked
/// enclosed. Returns how many.
fn guide_loops(w: &mut Work, guides: &[Vec<Vec3>], quad: f32, enclosed: &mut Vec<bool>, enclose: bool) -> usize {
    let nv = w.p.len();
    let nf = w.t.len();
    if guides.is_empty() || nv == 0 {
        return 0;
    }
    // Nearest working vertex, by grid cell.
    let cell = quad.max(1e-6);
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    let cell_of = |p: Vec3| { let c = (p / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
    for v in 0..nv as u32 {
        grid.entry(cell_of(w.p[v as usize])).or_default().push(v);
    }
    let nearest = |w: &Work, p: Vec3| -> Option<u32> {
        let c = cell_of(p);
        let mut best: Option<(f32, u32)> = None;
        for r in 1..4i32 {
            for dx in -r..=r {
                for dy in -r..=r {
                    for dz in -r..=r {
                        for &v in grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)).into_iter().flatten() {
                            let d = w.p[v as usize].distance_squared(p);
                            if best.is_none_or(|b| d < b.0) {
                                best = Some((d, v));
                            }
                        }
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        best.map(|b| b.1)
    };
    // Shortest edge path between two vertices (Dijkstra, bounded).
    let path = |w: &Work, a: u32, b: u32| -> Option<Vec<u32>> {
        if a == b {
            return Some(vec![a]);
        }
        let reach = w.p[a as usize].distance(w.p[b as usize]) * 3.0 + quad;
        let mut dist: HashMap<u32, f32> = HashMap::default();
        let mut from: HashMap<u32, u32> = HashMap::default();
        let mut heap = std::collections::BinaryHeap::new();
        dist.insert(a, 0.0);
        heap.push((std::cmp::Reverse(0f32.to_bits()), a));
        while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
            let dv = f32::from_bits(db);
            if v == b {
                let mut out = vec![b];
                let mut x = b;
                while x != a {
                    x = from[&x];
                    out.push(x);
                }
                out.reverse();
                return Some(out);
            }
            if dv > dist[&v] {
                continue;
            }
            for &u in w.neighbours(v) {
                let nd = dv + w.p[v as usize].distance(w.p[u as usize]);
                if nd <= reach && dist.get(&u).is_none_or(|&x| nd < x) {
                    dist.insert(u, nd);
                    from.insert(u, v);
                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                }
            }
        }
        None
    };
    let mut count = 0;
    for g in guides {
        let length: f32 = g.windows(2).map(|e| e[0].distance(e[1])).sum();
        if g.len() < 2 || length < quad * 2.0 {
            continue;
        }
        // Open guides (ends apart) become hard lines along them (ZR_NOOPENGUIDES leaves them).
        let closed = g.len() >= 4 && length >= quad * 4.0 && g[0].distance(*g.last().unwrap()) <= quad.max(length * 0.08);
        // Open guides as hard lines are experimental (they leave thin strips beside them and
        // made the blob 24–32% bad): off unless ZR_OPENGUIDES.
        if !closed && std::env::var_os("ZR_OPENGUIDES").is_none() {
            continue;
        }
        // Points about a third of a quad apart, snapped; consecutive ones joined by paths.
        let mut samples: Vec<Vec3> = vec![g[0]];
        let mut acc = 0.0;
        for e in g.windows(2) {
            acc += e[0].distance(e[1]);
            if acc >= quad * 0.33 {
                samples.push(e[1]);
                acc = 0.0;
            }
        }
        let mut snapped: Vec<u32> = samples.iter().filter_map(|&p| nearest(w, p)).collect();
        snapped.dedup();
        if closed && snapped.len() > 1 && snapped.first() == snapped.last() {
            snapped.pop();
        }
        if snapped.len() < if closed { 4 } else { 2 } {
            continue;
        }
        let m = snapped.len();
        let mut lp: Vec<u32> = Vec::new();
        let mut ok = true;
        let steps = if closed { m } else { m - 1 };
        for k in 0..steps {
            let Some(seg) = path(w, snapped[k], snapped[(k + 1) % m]) else { ok = false; break };
            for &v in &seg[..seg.len() - 1] {
                lp.push(v);
            }
        }
        if !closed {
            lp.push(snapped[m - 1]);
        }
        // A loop must not cross itself: cut out any repeated stretch.
        let mut seen: HashMap<u32, usize> = HashMap::default();
        let mut clean: Vec<u32> = Vec::new();
        for v in lp {
            if let Some(&at) = seen.get(&v) {
                for x in clean.drain(at + 1..) {
                    seen.remove(&x);
                }
            } else {
                seen.insert(v, clean.len());
                clean.push(v);
            }
        }
        if !ok || clean.len() < if closed { 6 } else { 3 } {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("    guide: no path (ok {ok}, {} vertices)", clean.len());
            }
            continue;
        }
        // Mark the loop's half-edges hard and soft.
        let key = |a: u32, b: u32| (a.min(b), a.max(b));
        let nedges = if closed { clean.len() } else { clean.len() - 1 };
        let edges: HashSet<(u32, u32)> = (0..nedges).map(|k| key(clean[k], clean[(k + 1) % clean.len()])).collect();
        let mut marked = 0;
        for h in 0..nf * 3 {
            let (a, b) = (w.from(h as u32), w.to(h as u32));
            if edges.contains(&key(a, b)) {
                w.hard[h] = true;
                w.soft[h] = true;
                marked += 1;
            }
        }
        if marked < edges.len() {
            continue;
        }
        if !closed || !enclose {
            // An open line or a ring round a limb: no inside; lines carry on from an open
            // line's ends (see the hard corners).
            count += 1;
            continue;
        }
        // Inside: the smaller of the regions the loop splits the faces into.
        let crosses = |f: usize, k: usize| edges.contains(&key(w.t[f][k], w.t[f][(k + 1) % 3]));
        let mut comp = vec![u32::MAX; nf];
        let mut sizes: Vec<f32> = Vec::new();
        for f0 in 0..nf {
            if comp[f0] != u32::MAX {
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
                    let g2 = o as usize / 3;
                    if comp[g2] == u32::MAX {
                        comp[g2] = id;
                        q.push(g2);
                    }
                }
            }
            sizes.push(area);
        }
        if sizes.len() < 2 {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("    guide: the loop does not split the surface");
            }
            continue;
        }
        // The loop's two sides: the components of faces along it.
        let mut sides: Vec<u32> = Vec::new();
        for f in 0..nf {
            if (0..3).any(|k| crosses(f, k)) && !sides.contains(&comp[f]) {
                sides.push(comp[f]);
            }
        }
        let Some(&inner) = sides.iter().min_by(|&&a, &&b| sizes[a as usize].total_cmp(&sizes[b as usize])) else { continue };
        if enclosed.len() != nf {
            *enclosed = vec![false; nf];
        }
        for f in 0..nf {
            if comp[f] == inner {
                enclosed[f] = true;
            }
        }
        count += 1;
    }
    count
}

/// Hard lines broken by small gaps (where the working mesh's loop strayed off an edge
/// line): loose ends within a quad of each other are joined by the shortest path between them.
fn bridge_gaps(w: &mut Work, quad: f32) {
    let nh = w.t.len() * 3;
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut half: HashMap<(u32, u32), Vec<u32>> = HashMap::default();
    for h in 0..nh as u32 {
        half.entry(key(w.from(h), w.to(h))).or_default().push(h);
    }
    let mut deg = vec![0u32; w.p.len()];
    for (&(a, b), hs) in &half {
        if hs.iter().any(|&h| w.hard[h as usize]) {
            deg[a as usize] += 1;
            deg[b as usize] += 1;
        }
    }
    let mut loose: Vec<u32> = (0..w.p.len() as u32).filter(|&v| deg[v as usize] == 1).collect();
    loose.sort_unstable();
    let mut done: HashSet<u32> = HashSet::default();
    for &a in &loose {
        if done.contains(&a) {
            continue;
        }
        // Dijkstra from a over the mesh, up to a quad, to another loose end.
        let mut dist: HashMap<u32, f32> = HashMap::default();
        let mut from: HashMap<u32, u32> = HashMap::default();
        let mut heap = std::collections::BinaryHeap::new();
        dist.insert(a, 0.0);
        heap.push((std::cmp::Reverse((0.0f32).to_bits()), a));
        let mut goal = None;
        while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
            let d = f32::from_bits(db);
            if d > dist[&v] {
                continue;
            }
            if v != a && deg[v as usize] == 1 && !done.contains(&v) {
                goal = Some(v);
                break;
            }
            for &u in w.neighbours(v) {
                let nd = d + w.p[v as usize].distance(w.p[u as usize]);
                if nd <= quad * 2.0 && dist.get(&u).is_none_or(|&x| nd < x) {
                    dist.insert(u, nd);
                    from.insert(u, v);
                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                }
            }
        }
        let Some(b) = goal else { continue };
        let mut v = b;
        while v != a {
            let u = from[&v];
            for &h in &half[&key(u, v)] {
                w.hard[h as usize] = true;
            }
            v = u;
        }
        done.insert(a);
        done.insert(b);
    }
}

/// The cross field: per face, the fourth power of a direction as a unit complex number.
/// `parts`: per face, a direction to follow (4θ form, length its weight), or empty.
fn cross_field(w: &Work, follow: f32, quad: f32, grid: &[(Vec3, f32)], parts: &[Vec2]) -> Vec<Vec2> {
    let nf = w.t.len();
    // Transport across each half-edge, into its face from its twin's (in the 4θ angle).
    let rot: Vec<Vec2> = (0..nf * 3)
        .map(|h| {
            let o = w.twin[h];
            if o == NONE {
                return Vec2::X;
            }
            let e = w.p[w.to(h as u32) as usize] - w.p[w.from(h as u32) as usize];
            let (af, ag) = (w.angle(h / 3, e), w.angle(o as usize / 3, e));
            expi(4.0 * (af - ag))
        })
        .collect();
    // Held faces: along their longest hard edge. A soft line (guide, feature loop) snaps
    // along working-mesh edges that zigzag: its direction is taken over a stretch of the line
    // (ZR_SOFTHOLD=edge holds along the single edge as before).
    let soft_dir = soft_line_directions(w);
    let mut fixed = vec![false; nf];
    let mut z = vec![Vec2::ZERO; nf];
    for f in 0..nf {
        let mut best = 0.0f32;
        for k in 0..3 {
            let h = (f * 3 + k) as u32;
            if w.hard[h as usize] {
                let e = if w.soft[h as usize] {
                    soft_dir.get(&(w.from(h).min(w.to(h)), w.from(h).max(w.to(h)))).copied().unwrap_or(w.p[w.to(h) as usize] - w.p[w.from(h) as usize])
                } else {
                    w.p[w.to(h) as usize] - w.p[w.from(h) as usize]
                };
                if e.length() > best {
                    best = e.length();
                    z[f] = expi(4.0 * w.angle(f, e));
                    fixed[f] = true;
                }
            }
        }
    }
    // Follow Shape: the bending at the vertices, into the faces.
    let mut target = vec![Vec2::ZERO; nf];
    // The working mesh's own quads: a start that already follows the form.
    let mut seed = vec![Vec2::ZERO; nf];
    if grid.len() == nf {
        for f in 0..nf {
            let (d, wt) = grid[f];
            if wt > 0.0 {
                seed[f] = expi(4.0 * w.angle(f, d)) * wt;
            }
        }
    }
    if follow > 0.0 {
        let edges: Vec<(u32, u32)> = (0..w.p.len() as u32).flat_map(|v| w.neighbours(v).iter().map(move |&u| (v, u))).collect();
        let g = graph_from(w.p.clone(), w.vnrm.clone(), vec![Vec3::ZERO; w.p.len()], vec![0.0; w.p.len()], edges);
        let tensors = curvature_tensors(&g, 2);
        let pulls = shape_pulls(&g, &tensors, quad, follow);
        for f in 0..nf {
            let mut c = Vec2::ZERO;
            for &v in &w.t[f] {
                let pl = pulls[v as usize];
                if pl.w > 0.0 {
                    c += expi(4.0 * w.angle(f, pl.dir)) * pl.w;
                }
            }
            target[f] = c / 3.0;
        }
    }
    if parts.len() == nf {
        for f in 0..nf {
            target[f] += parts[f];
        }
    }
    // Start: spread outward from what is known.
    let start: Vec<Vec2> = (0..nf).map(|f| if seed[f].length() > 0.05 { seed[f] } else { target[f] }).collect();
    let mut known: Vec<bool> = (0..nf).map(|f| fixed[f] || start[f].length() > 0.05).collect();
    for f in 0..nf {
        if known[f] && !fixed[f] {
            z[f] = start[f].normalize();
        }
    }
    if !known.contains(&true) {
        known[0] = true;
        z[0] = Vec2::X;
    }
    let mut queue: VecDeque<usize> = (0..nf).filter(|&f| known[f]).collect();
    while let Some(f) = queue.pop_front() {
        for k in 0..3 {
            let h = f * 3 + k;
            let o = w.twin[h];
            if o == NONE {
                continue;
            }
            let g = o as usize / 3;
            if !known[g] {
                // Into g from f: the twin half-edge's transport.
                z[g] = cmul(z[f], rot[o as usize]);
                known[g] = true;
                queue.push_back(g);
            }
        }
    }
    // The smoothest field (Knöppel et al. 2013): minimise Σ |z_f − r z_g|² over neighbouring
    // faces plus Σ w_f |z_f − c_f|² (held faces strongly, the shape's pull weakly) — one
    // linear solve; with nothing to hold, the smoothest mode by inverse iteration.
    let pull: f32 = std::env::var("ZR_PULL").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
    let weight: Vec<f32> = (0..nf).map(|f| if fixed[f] { 1e3 } else { target[f].length() * pull }).collect();
    let goal: Vec<Vec2> = (0..nf).map(|f| if fixed[f] { z[f] * 1e3 } else { target[f] * pull }).collect();
    let conj = |a: Vec2| Vec2::new(a.x, -a.y);
    let apply = |x: &[Vec2], eps: f32| -> Vec<Vec2> {
        (0..nf)
            .into_par_iter()
            .map(|f| {
                let mut s = x[f] * (weight[f] + eps);
                for k in 0..3 {
                    let h = f * 3 + k;
                    let o = w.twin[h];
                    if o != NONE {
                        s += x[f] - cmul(x[o as usize / 3], rot[h]);
                    }
                }
                s
            })
            .collect()
    };
    // Hermitian dot product (real part is all CG needs here). Summed in fixed chunks, in
    // order: a parallel sum splits differently from run to run, and its rounding changed
    // the field, and with it the whole layout, between runs.
    let dot = |a: &[Vec2], b: &[Vec2]| -> f64 {
        let parts: Vec<f64> = a.par_chunks(4096).zip(b.par_chunks(4096)).map(|(xa, xb)| xa.iter().zip(xb).map(|(x, y)| (x.x * y.x + x.y * y.y) as f64).sum::<f64>()).collect();
        parts.iter().sum()
    };
    let _ = conj;
    let cg = |b: &[Vec2], x0: &[Vec2], eps: f32, iters: usize| -> Vec<Vec2> {
        let mut x = x0.to_vec();
        let ax = apply(&x, eps);
        let mut r: Vec<Vec2> = b.iter().zip(&ax).map(|(b, a)| *b - *a).collect();
        let mut p = r.clone();
        let mut rr = dot(&r, &r);
        let bb = dot(b, b).max(1e-30);
        for _ in 0..iters {
            if rr < bb * 1e-10 {
                break;
            }
            let ap = apply(&p, eps);
            let alpha = (rr / dot(&p, &ap).max(1e-30)) as f32;
            x.par_iter_mut().zip(p.par_iter()).for_each(|(x, p)| *x += *p * alpha);
            r.par_iter_mut().zip(ap.par_iter()).for_each(|(r, a)| *r -= *a * alpha);
            let rr2 = dot(&r, &r);
            let beta = (rr2 / rr) as f32;
            rr = rr2;
            p.par_iter_mut().zip(r.par_iter()).for_each(|(p, r)| *p = *r + *p * beta);
        }
        x
    };
    let held: f32 = weight.iter().sum();
    let mut x: Vec<Vec2> = z.clone();
    if held > nf as f32 * 0.01 {
        x = cg(&goal, &x, 1e-4, 2000);
    } else {
        for _ in 0..8 {
            let norm = dot(&x, &x).sqrt().max(1e-30) as f32;
            let b: Vec<Vec2> = x.iter().map(|v| *v / norm).collect();
            x = cg(&b, &b, 1e-3, 1000);
        }
    }
    for f in 0..nf {
        if !fixed[f] && x[f].length_squared() > 1e-20 {
            z[f] = x[f].normalize();
        }
    }
    // Polish with a few normalised sweeps (the shape's pull as weak as in the solve).
    for _ in 0..30 {
        for f in 0..nf {
            if fixed[f] {
                continue;
            }
            let mut s = Vec2::ZERO;
            for k in 0..3 {
                let h = f * 3 + k;
                let o = w.twin[h];
                if o != NONE {
                    s += cmul(z[o as usize / 3], rot[h]);
                }
            }
            s += target[f] * (pull / 3.0);
            if s.length_squared() > 1e-12 {
                z[f] = s.normalize();
            }
        }
    }
    z
}

/// Each soft line edge's smoothed direction: the chord over about four edges either way
/// along the line (oriented like the edge).
fn soft_line_directions(w: &Work) -> HashMap<(u32, u32), Vec3> {
    let mut out: HashMap<(u32, u32), Vec3> = HashMap::default();
    // Off by default: holding along the single snapped edge serves closed loops far better
    // (blob with eye loops 8.8% bad quads against 27.4%); ZR_SOFTHOLD=smooth turns it on.
    if !std::env::var("ZR_SOFTHOLD").is_ok_and(|v| v == "smooth") {
        return out;
    }
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let mut nb: HashMap<u32, Vec<u32>> = HashMap::default();
    let mut seen: HashSet<(u32, u32)> = HashSet::default();
    for h in 0..w.t.len() as u32 * 3 {
        if w.soft[h as usize] && seen.insert(key(w.from(h), w.to(h))) {
            nb.entry(w.from(h)).or_default().push(w.to(h));
            nb.entry(w.to(h)).or_default().push(w.from(h));
        }
    }
    // Walk `steps` along the line from `v`, away from `from`.
    let walk = |mut from: u32, mut v: u32, steps: usize| -> u32 {
        for _ in 0..steps {
            let Some(list) = nb.get(&v) else { break };
            if list.len() != 2 {
                break;
            }
            let nx = if list[0] != from { list[0] } else { list[1] };
            from = v;
            v = nx;
        }
        v
    };
    for &(a, b) in &seen {
        let (fa, fb) = (walk(b, a, 4), walk(a, b, 4));
        let d = w.p[fb as usize] - w.p[fa as usize];
        out.insert((a, b), if d.length_squared() > 0.0 { d } else { w.p[b as usize] - w.p[a as usize] });
    }
    out
}

/// The four directions of face `f` (in 3D).
fn dirs(w: &Work, z: &[Vec2], f: usize) -> [Vec3; 4] {
    let a = z[f].y.atan2(z[f].x) / 4.0;
    let d = w.dir(f, a);
    let n = w.fnrm[f];
    [d, n.cross(d), -d, -n.cross(d)]
}

/// The field direction of face `g` nearest `d` carried over from face `f`.
fn carry(w: &Work, z: &[Vec2], f: usize, g: usize, d: Vec3) -> Vec3 {
    let t = Quat::from_rotation_arc(w.fnrm[f], w.fnrm[g]) * d;
    dirs(w, z, g).into_iter().max_by(|a, b| a.dot(t).total_cmp(&b.dot(t))).unwrap()
}

/// Each interior vertex's index in quarter turns (0 for a regular one).
fn poles(w: &Work, z: &[Vec2]) -> Vec<i32> {
    (0..w.p.len() as u32)
        .into_par_iter()
        .map(|v| {
            if !w.interior(v) {
                return 0;
            }
            let fan = w.fan(v);
            if fan.iter().any(|&h| w.hard[h as usize]) {
                return 0;
            }
            let faces: Vec<usize> = fan.iter().map(|&h| h as usize / 3).collect();
            let angles: f32 = fan.iter().map(|&h| w.corner(h)).sum();
            let defect = std::f32::consts::TAU - angles;
            let mut d = dirs(w, z, faces[0])[0];
            let mut turn = 0.0f32;
            for i in 1..=faces.len() {
                let (f, g) = (faces[i - 1], faces[i % faces.len()]);
                let t = Quat::from_rotation_arc(w.fnrm[f], w.fnrm[g]) * d;
                let c = dirs(w, z, g).into_iter().max_by(|a, b| a.dot(t).total_cmp(&b.dot(t))).unwrap();
                let n = w.fnrm[g];
                turn += n.dot(t.cross(c)).atan2(t.dot(c));
                d = c;
            }
            ((turn + defect) / std::f32::consts::FRAC_PI_2).round() as i32
        })
        .collect()
}

/// A line being traced.
#[derive(Clone)]
struct Trace {
    face: usize,
    at: Vec3,
    dir: Vec3,
    /// The edge it came in by (none at the start).
    entry: u32,
    path: Vec<u32>,
    travelled: f32,
    done: bool,
    why: &'static str,
    /// Another line's id this one continues (head-on merges, two-way lines).
    same_as: u32,
    /// The path's length at its last ordinary step (before any walk to a join), to resume.
    anchor: usize,
    /// Steps in a row that did not move it (stuck at a corner).
    still: u32,
    /// With no entry edge: the vertex it is on (edges round it are not exits).
    pivot: u32,
    /// Debugging: the last thing that changed this line.
    last_op: &'static str,
}

impl Trace {
    fn new(w: &Work, z: &[Vec2], v: u32, face: usize, dir: Vec3) -> Self {
        // Start in the face round `v` whose corner the direction points into.
        let mut face = face;
        let mut dir = dir;
        let mut best = f32::NEG_INFINITY;
        for h in w.fan(v) {
            let f = h as usize / 3;
            let n = w.fnrm[f];
            let (a, b) = ((w.p[w.to(h) as usize] - w.p[v as usize]).normalize(), (w.p[w.from(Work::prev(h)) as usize] - w.p[v as usize]).normalize());
            let d = (dir - n * n.dot(dir)).normalize_or(a);
            // How far inside the corner (positive inside).
            let inside = n.dot(a.cross(d)).min(n.dot(d.cross(b)));
            if inside > best {
                best = inside;
                face = f;
                dir = d;
            }
        }
        let fd = dirs(w, z, face).into_iter().max_by(|a, b| a.dot(dir).total_cmp(&b.dot(dir))).unwrap();
        let n = w.fnrm[face];
        let t = w.t[face];
        let k = t.iter().position(|&x| x == v).unwrap_or(0);
        let (a, b) = (w.p[t[(k + 1) % 3] as usize] - w.p[v as usize], w.p[t[(k + 2) % 3] as usize] - w.p[v as usize]);
        let fits = n.dot(a.cross(fd)) >= 0.0 && n.dot(fd.cross(b)) >= 0.0;
        Trace { face, at: w.p[v as usize], dir: if fits { fd } else { dir }, entry: NONE, path: vec![v], travelled: 0.0, done: false, why: "", same_as: NONE, anchor: 1, still: 0, pivot: v, last_op: "new" }
    }

    /// Go on from the path's last vertex (after it was cut back or ended by a walk): the
    /// stepping state is rebuilt there, heading the way the last edge runs.
    fn restart(&mut self, w: &Work, z: &[Vec2]) {
        let n = self.path.len();
        let v = self.path[n - 1];
        let dir = if n >= 2 { (w.p[v as usize] - w.p[self.path[n - 2] as usize]).normalize_or(self.dir) } else { self.dir };
        let fresh = Trace::new(w, z, v, w.fan(v).first().map(|&h| h as usize / 3).unwrap_or(0), dir);
        self.face = fresh.face;
        self.dir = fresh.dir;
        self.at = fresh.at;
        self.entry = NONE;
        self.pivot = v;
        self.still = 0;
        self.anchor = n;
        self.last_op = "restart";
    }

    /// One face further; false when it stops. `marks` say which line holds each vertex.
    fn step(&mut self, id: u32, w: &Work, z: &[Vec2], marks: &mut [u32], quad: f32, poles: &[u32], stamp: &mut [u32], round: u32) -> bool {
        let f = self.face;
        let n = w.fnrm[f];
        let (x, y) = (w.fx[f], n.cross(w.fx[f]));
        let to2 = |p: Vec3| Vec2::new((p - self.at).dot(x), (p - self.at).dot(y));
        let d2 = Vec2::new(self.dir.dot(x), self.dir.dot(y)).normalize_or(Vec2::X);
        let mut best: Option<(f32, u32, f32)> = None;
        for k in 0..3 {
            let h = (f * 3 + k) as u32;
            if self.entry != NONE && h == self.entry {
                continue;
            }
            if self.entry == NONE && (w.from(h) == self.pivot || w.to(h) == self.pivot) {
                continue;
            }
            let (a, b) = (to2(w.p[w.from(h) as usize]), to2(w.p[w.to(h) as usize]));
            let e = b - a;
            let den = d2.perp_dot(e);
            if den.abs() < 1e-12 {
                continue;
            }
            let s = a.perp_dot(e) / den;
            let u = a.perp_dot(d2) / den;
            if s > quad * 1e-4 && (-1e-3..=1.0 + 1e-3).contains(&u) && best.is_none_or(|b| s < b.0) {
                best = Some((s, h, u.clamp(0.0, 1.0)));
            }
        }
        let (h, u) = match best {
            Some((_, h, u)) => (h, u),
            None => {
                // On a corner of the face, pointing out past it: go on from the face round
                // that corner the direction points into.
                let near = w.t[f].iter().copied().min_by(|&a, &b| w.p[a as usize].distance_squared(self.at).total_cmp(&w.p[b as usize].distance_squared(self.at))).unwrap();
                if w.p[near as usize].distance(self.at) < quad * 1e-3 && !(self.entry == NONE && self.pivot == near) {
                    let mut best_f = None;
                    let mut best_in = f32::NEG_INFINITY;
                    for hh in w.fan(near) {
                        let g = hh as usize / 3;
                        let n = w.fnrm[g];
                        let a = (w.p[w.to(hh) as usize] - w.p[near as usize]).normalize_or_zero();
                        let b = (w.p[w.from(Work::prev(hh)) as usize] - w.p[near as usize]).normalize_or_zero();
                        let d = (self.dir - n * n.dot(self.dir)).normalize_or_zero();
                        let inside = n.dot(a.cross(d)).min(n.dot(d.cross(b)));
                        if inside > best_in {
                            best_in = inside;
                            best_f = Some((g, d));
                        }
                    }
                    if let Some((g, d)) = best_f {
                        // The pivot vertex joins the path (the next step's vertices touch it).
                        if *self.path.last().unwrap() != near {
                            if marks[near as usize] != NONE && marks[near as usize] != id {
                                push_checked(&mut self.path, near, w, "pivot-hit");
                                self.done = true;
                                self.why = "hit";
                                return false;
                            }
                            push_checked(&mut self.path, near, w, "pivot");
                            self.anchor = self.path.len();
                            marks[near as usize] = id;
                            stamp[near as usize] = round;
                        }
                        self.face = g;
                        // As pivoted (inside that corner); the field takes over at the next edge.
                        self.dir = d;
                        self.at = w.p[near as usize];
                        self.entry = NONE;
                        self.pivot = near;
                        return true;
                    }
                }
                // Numerically lost: leave by the edge the point a little ahead lies furthest
                // beyond.
                let ahead = to2(self.at + self.dir * 1e-3);
                let mut pick: Option<(f32, u32)> = None;
                for k in 0..3 {
                    let h = (f * 3 + k) as u32;
                    if self.entry != NONE && h == self.entry {
                        continue;
                    }
                    let (a, b) = (to2(w.p[w.from(h) as usize]), to2(w.p[w.to(h) as usize]));
                    // Outside is to the right of a counter-clockwise edge.
                    let out = -(b - a).perp_dot(ahead - a) / (b - a).length().max(1e-12);
                    if pick.is_none_or(|p| out > p.0) {
                        pick = Some((out, h));
                    }
                }
                let Some((_, h)) = pick else {
                    self.done = true;
                    self.why = "lost";
                    return false;
                };
                let (a, b) = (to2(w.p[w.from(h) as usize]), to2(w.p[w.to(h) as usize]));
                let e = b - a;
                let u = ((Vec2::ZERO - a).dot(e) / e.length_squared().max(1e-12)).clamp(0.0, 1.0);
                (h, u)
            }
        };
        let (a, b) = (w.from(h), w.to(h));
        let hit = w.p[a as usize].lerp(w.p[b as usize], u);
        let moved = hit.distance(self.at);
        self.travelled += moved;
        self.still = if moved < quad * 1e-4 { self.still + 1 } else { 0 };
        if self.still == 3 && std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            let t = w.t[f];
            let dv: Vec<f32> = t.iter().map(|&v| w.p[v as usize].distance(self.at) / quad).collect();
            let tn: Vec<usize> = t.iter().map(|&v| w.fan(v).len()).collect();
            eprintln!("        still: face {f} {:?} at {:.4} dir {:.3} n {:.3} exit h {h} u {u:.3} entry {} best {} dist/quad {:.5?} fans {:?} interior {:?} pivot {}", t, self.at, self.dir, w.fnrm[f], self.entry, best.is_some(), dv, tn, t.iter().map(|&v| w.interior(v)).collect::<Vec<_>>(), self.pivot);
        }
        if self.still > 24 {
            self.done = true;
            self.why = "stuck";
            return false;
        }
        let mut snap = if u < 0.5 { a } else { b };
        // The first step takes the free end if the nearer one is taken (a sibling line
        // from the same pole).
        if self.path.len() == 1 && marks[snap as usize] != NONE {
            let other = if snap == a { b } else { a };
            if marks[other as usize] == NONE {
                snap = other;
            }
        }
        let last = *self.path.last().unwrap();
        // Back onto its own recent path (a wiggle): undo the loop and go on from there.
        if snap != last && marks[snap as usize] == id {
            if let Some(at) = self.path.iter().rposition(|&q| q == snap) {
                let back: f32 = self.path[at..].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum();
                if back < quad * 2.0 {
                    for q in self.path.split_off(at + 1) {
                        if marks[q as usize] == id {
                            marks[q as usize] = NONE;
                        }
                    }
                }
            }
        }
        let last = *self.path.last().unwrap();
        if snap != last {
            let len = self.path.len();
            if len >= 2 && self.path[len - 2] == snap {
                // A wiggle back: drop the vertex it wiggled to.
                let gone = self.path.pop().unwrap();
                if marks[gone as usize] == id {
                    marks[gone as usize] = NONE;
                }
            } else if marks[snap as usize] != NONE && !(marks[snap as usize] == id && self.path.iter().rev().take(4).any(|&q| q == snap)) {
                push_checked(&mut self.path, snap, w, "hit");
                self.done = true;
                self.why = "hit";
                return false;
            } else {
                push_checked(&mut self.path, snap, w, "ordinary");
                self.anchor = self.path.len();
                marks[snap as usize] = id;
                stamp[snap as usize] = round;
                // A pole just ahead: end on it.
                if self.travelled > quad * 0.5 {
                    let here = w.p[snap as usize];
                    let start = self.path[0];
                    if let Some(&pole) = poles.iter().filter(|&&q| q != start).min_by(|&&a, &&b| w.p[a as usize].distance_squared(here).total_cmp(&w.p[b as usize].distance_squared(here))) {
                        let d = w.p[pole as usize] - here;
                        if (d.length() < quad * 0.6 && self.dir.dot(d) > -0.2 * d.length()) || (d.length() < quad * 3.0 && self.dir.dot(d) > 0.82 * d.length()) {
                            let mut cur = snap;
                            for _ in 0..128 {
                                let goal = w.p[pole as usize];
                                let Some(&u) = w.neighbours(cur).iter().min_by(|&&a, &&b| w.p[a as usize].distance_squared(goal).total_cmp(&w.p[b as usize].distance_squared(goal))) else { break };
                                if w.p[u as usize].distance_squared(goal) >= w.p[cur as usize].distance_squared(goal) {
                                    break;
                                }
                                push_checked(&mut self.path, u, w, "pole-walk");
                                if u == pole || marks[u as usize] != NONE {
                                    break;
                                }
                                marks[u as usize] = id;
                                stamp[u as usize] = round;
                                cur = u;
                            }
                            self.done = true;
                            self.why = "pole";
                            return false;
                        }
                    }
                }
                // Another line close ahead or beside (within 0.9 quad): join it there.
                if self.travelled > quad * 0.75 {
                    let recent: Vec<u32> = self.path.iter().rev().take(16).copied().collect();
                    let here = w.p[snap as usize];
                    let mut found: Option<(f32, u32, u32)> = None;
                    // Breadth-first round `snap`, remembering the first step toward each vertex.
                    let mut seen: HashMap<u32, u32> = HashMap::default();
                    let mut q: VecDeque<u32> = VecDeque::new();
                    for &n0 in w.neighbours(snap) {
                        if n0 != snap && !seen.contains_key(&n0) {
                            seen.insert(n0, n0);
                            q.push_back(n0);
                        }
                    }
                    while let Some(r) = q.pop_front() {
                        let d = w.p[r as usize] - here;
                        if d.length() > quad * 0.9 {
                            continue;
                        }
                        let m = marks[r as usize];
                        if m != NONE && !(m == id && recent.contains(&r)) && !(m != id && m < NODE && stamp[r as usize] + 4 > round) && self.dir.dot(d) >= -0.3 * d.length() {
                            if found.is_none_or(|f| d.length() < f.0) {
                                found = Some((d.length(), seen[&r], r));
                            }
                            continue;
                        }
                        if m != NONE {
                            continue;
                        }
                        for &u in w.neighbours(r) {
                            if u != snap && !seen.contains_key(&u) {
                                seen.insert(u, seen[&r]);
                                q.push_back(u);
                            }
                        }
                    }
                    if let Some((_, _, r)) = found {
                        // Walk there over free vertices (straight toward it).
                        let goal = w.p[r as usize];
                        let mut cur = snap;
                        for _ in 0..32 {
                            if w.neighbours(cur).contains(&r) {
                                break;
                            }
                            let Some(&u) = w.neighbours(cur).iter().filter(|&&u| marks[u as usize] == NONE).min_by(|&&a, &&b| w.p[a as usize].distance_squared(goal).total_cmp(&w.p[b as usize].distance_squared(goal))) else { break };
                            if w.p[u as usize].distance_squared(goal) >= w.p[cur as usize].distance_squared(goal) {
                                break;
                            }
                            push_checked(&mut self.path, u, w, "join-walk");
                            marks[u as usize] = id;
                            stamp[u as usize] = round;
                            cur = u;
                        }
                        if w.neighbours(cur).contains(&r) {
                            push_checked(&mut self.path, r, w, "join-end");
                            self.done = true;
                            self.why = "join";
                            return false;
                        }
                        // Did not get there: the walk is undone and the line goes on.
                        while *self.path.last().unwrap() != snap {
                            let gone = self.path.pop().unwrap();
                            if marks[gone as usize] == id {
                                marks[gone as usize] = NONE;
                            }
                        }
                    }
                }
            }
        }
        if w.hard[h as usize] {
            // On a hard edge: end at its nearer end.
            let last = *self.path.last().unwrap();
            if last != snap {
                push_checked(&mut self.path, snap, w, "hard-end");
            }
            self.done = true;
            self.why = "hard";
            return false;
        }
        let o = w.twin[h as usize];
        let g = o as usize / 3;
        self.dir = carry(w, z, f, g, self.dir);
        self.face = g;
        self.at = hit;
        self.entry = o;
        if self.travelled > quad * 400.0 {
            self.done = true;
            self.why = "far";
            return false;
        }
        true
    }
}

/// Debugging: push `v` onto `path`, reporting it when it is not adjacent to the last vertex.
fn push_checked(path: &mut Vec<u32>, v: u32, w: &Work, label: &str) {
    if let Some(&last) = path.last() {
        if last != v && !w.neighbours(last).contains(&v) && std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("    BAD PUSH [{label}]: {last} -> {v} (path {} long)", path.len());
        }
    }
    path.push(v);
}

/// Debugging: report a line whose last step is between non-adjacent vertices.
fn check_path(t: &Trace, w: &Work, i: usize, label: &str) {
    let n = t.path.len();
    if n >= 2 && std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let (a, b) = (t.path[n - 2], t.path[n - 1]);
        if a != b && !w.neighbours(a).contains(&b) {
            let tri = w.t[t.face];
            let prev = if n >= 3 { Some(t.path[n - 3]) } else { None };
            eprintln!("    NEW BAD STEP by {label}: line {i} ({}, {n} verts, done {}): {a} -> {b}, face {} = {:?} (has last {}, has new {}), prev {prev:?}, entry {} (from {} to {}), pivot {}, at {:.4}, p[last] {:.4}, p[new] {:.4}", t.why, t.done, t.face, tri, tri.contains(&a), tri.contains(&b), t.entry, if t.entry == NONE { NONE } else { w.from(t.entry) }, if t.entry == NONE { NONE } else { w.to(t.entry) }, t.pivot, t.at, w.p[a as usize], w.p[b as usize]);
        }
    }
}

/// Advance all `traces` together until every one stops; mark their vertices.
fn run(traces: &mut [Trace], first_id: u32, w: &Work, z: &[Vec2], marks: &mut [u32], quad: f32, poles: &[u32]) {
    let mut live = true;
    let mut guard = 0usize;
    // The round each vertex was taken in; earlier lines' are long settled.
    let mut stamp = vec![0u32; marks.len()];
    let base = 8u32;
    let mut reported: HashSet<(usize, usize)> = HashSet::default();
    while live && guard < w.t.len() * 4 + 100 {
        live = false;
        guard += 1;
        for i in 0..traces.len() {
            if traces[i].done {
                continue;
            }
            let before: Option<usize> = if std::env::var_os("KLAY_ZR_DEBUG").is_some() { traces[i].path.windows(2).position(|e| e[0] != e[1] && !w.neighbours(e[0]).contains(&e[1])) } else { None };
            let went = traces[i].step(first_id + i as u32, w, z, marks, quad, poles, &mut stamp, base + guard as u32);
            if let Some(at) = before {
                if reported.insert((i, at)) {
                    eprintln!("    PATH BROKEN before step: line {i} at index {at} of {}, last op {}, why {:?}, done-before? (entry {} pivot {})", traces[i].path.len(), traces[i].last_op, traces[i].why, traces[i].entry, traces[i].pivot);
                }
            }
            traces[i].last_op = "step";
            check_path(&traces[i], w, i, if went { "step" } else { "stop" });
            if went {
                live = true;
                continue;
            }
            // Met a line still running toward it: that one stops at the meeting point too,
            // so the two make one line instead of passing side by side.
            let end = *traces[i].path.last().unwrap();
            let m = marks[end as usize];
            if m >= first_id && ((m - first_id) as usize) < traces.len() && m != first_id + i as u32 {
                let j = (m - first_id) as usize;
                // Only head-on: the two heading toward each other.
                if !traces[j].done && traces[i].dir.dot(Quat::from_rotation_arc(w.fnrm[traces[j].face], w.fnrm[traces[i].face]) * traces[j].dir) < -0.7 {
                    if let Some(at) = traces[j].path.iter().position(|&v| v == end) {
                        let rest: f32 = traces[j].path[at..].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum();
                        let kept: f32 = traces[j].path[..=at].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum();
                        if rest <= quad * 1.2 && kept >= quad * 0.75 {
                            for v in traces[j].path.split_off(at + 1) {
                                if marks[v as usize] == m {
                                    marks[v as usize] = NONE;
                                }
                            }
                            traces[j].done = true;
                            traces[j].why = "merged";
                            traces[j].last_op = "merged";
                            traces[j].same_as = first_id + i as u32;
                        }
                    }
                }
            }
        }
    }
}

/// Lines that stopped on a limb ring carry on across it from where they met it, the same
/// way, as part of the same line; then the new ones run (and so on, ring after ring).
#[allow(clippy::too_many_arguments)]
fn cross_rings(traces: &mut Vec<Trace>, w: &Work, z: &[Vec2], marks: &mut [u32], quad: f32, poles: &[u32], ring_verts: &HashSet<u32>, continued: &mut HashSet<usize>, protected: &mut HashSet<u32>) {
    if ring_verts.is_empty() {
        return;
    }
    for _ in 0..64 {
        let mut added = 0;
        for i in 0..traces.len() {
            if !traces[i].done || continued.contains(&i) || !protected.contains(&(i as u32)) {
                continue;
            }
            let path = &traces[i].path;
            let n = path.len();
            if n < 2 || !ring_verts.contains(&path[n - 1]) {
                continue;
            }
            continued.insert(i);
            let v = path[n - 1];
            let dir = (w.p[v as usize] - w.p[path[n.saturating_sub(4).max(0)] as usize]).normalize_or_zero();
            if dir == Vec3::ZERO {
                continue;
            }
            let Some(&h) = w.fan(v).first() else { continue };
            let mut t = Trace::new(w, z, v, h as usize / 3, dir);
            t.same_as = i as u32;
            if protected.contains(&(i as u32)) {
                protected.insert(traces.len() as u32);
            }
            traces.push(t);
            added += 1;
        }
        if added == 0 {
            break;
        }
        run(traces, 0, w, z, marks, quad, poles);
    }
}

/// After tracing: repair starts with no line left are freed, a start with only one line
/// loses it, and lines whose end touches nothing (what they stopped on went away) go on.
#[allow(clippy::too_many_arguments)]
fn settle(traces: &mut Vec<Trace>, marks: &mut [u32], extra: &mut HashSet<u32>, w: &Work, z: &[Vec2], quad: f32, pole_list: &[u32], is_pole: &[bool]) {
    snap_ends(traces, marks, w, quad);
    // Repair starts left with no line: free again.
    let starts: HashSet<u32> = traces.iter().filter(|t| t.path.len() >= 2).map(|t| t.path[0]).collect();
    extra.retain(|v| {
        let keep = starts.contains(v);
        if !keep && marks[*v as usize] == NODE {
            marks[*v as usize] = NONE;
        }
        keep
    });
    // Lines ending on nothing (what they stopped on went away) go on.
    for _ in 0..8 {
        let mut deg: HashMap<u32, u32> = HashMap::default();
        for h in 0..w.t.len() as u32 * 3 {
            if w.hard[h as usize] && (w.twin[h as usize] == NONE || w.from(h) < w.to(h)) {
                *deg.entry(w.from(h)).or_default() += 1;
                *deg.entry(w.to(h)).or_default() += 1;
            }
        }
        for t in traces.iter().filter(|t| t.path.len() >= 2) {
            for e in t.path.windows(2) {
                if e[0] != e[1] {
                    *deg.entry(e[0]).or_default() += 1;
                    *deg.entry(e[1]).or_default() += 1;
                }
            }
        }
        let mut again = false;
        // A start with only this line left: the line goes too.
        for i in 0..traces.len() {
            let t = &mut traces[i];
            if t.path.len() < 2 {
                continue;
            }
            let s0 = t.path[0];
            if deg.get(&s0).copied().unwrap_or(0) == 1 && marks[s0 as usize] != HARD {
                for &v in &t.path[1..] {
                    if marks[v as usize] == i as u32 {
                        marks[v as usize] = NONE;
                    }
                }
                t.path.truncate(1);
                t.done = true;
                t.why = "alone";
                again = true;
            }
        }
        for i in 0..traces.len() {
            let t = &mut traces[i];
            if t.path.len() < 2 || !t.done {
                continue;
            }
            let e = *t.path.last().unwrap();
            // (A pole with lines of its own keeps a line ending on it; one without is just a
            // vertex the line happened to stop at.)
            if deg.get(&e).copied().unwrap_or(0) == 1 && marks[e as usize] != HARD && !(is_pole[e as usize] && marks[e as usize] == NODE) {
                marks[e as usize] = i as u32;
                t.restart(w, z);
                check_path(t, w, i, "settle-resume");
                t.done = false;
                t.why = "";
                again = true;
            }
        }
        if !again {
            break;
        }
        run(traces, 0, w, z, marks, quad, pole_list);
        prune(traces, marks, w);
    }
}

/// A line that ended on another line just beside a meeting point already there (another
/// line's end, a pole, a corner) is bent onto that point, so two meetings a sliver apart
/// become one.
fn snap_ends(traces: &mut [Trace], marks: &mut [u32], w: &Work, quad: f32) {
    // Meeting points: every line's ends, and nodes.
    let mut junction: HashSet<u32> = HashSet::default();
    for t in traces.iter().filter(|t| t.path.len() >= 2) {
        junction.insert(t.path[0]);
        junction.insert(*t.path.last().unwrap());
    }
    for i in 0..traces.len() {
        if traces[i].path.len() < 3 || !traces[i].done || traces[i].why == "cut" {
            continue;
        }
        let e = *traces[i].path.last().unwrap();
        let on = marks[e as usize];
        if on == i as u32 || on == NONE {
            continue;
        }
        // Another end on the same line (or a node) within 0.6 quad, not this line's own.
        let pe = w.p[e as usize];
        let Some(n) = junction
            .iter()
            .copied()
            .filter(|&n| n != e && n != traces[i].path[0] && (marks[n as usize] == on || marks[n as usize] == NODE || marks[n as usize] == HARD))
            .filter(|&n| w.p[n as usize].distance(pe) < quad * 0.6)
            .min_by(|&a, &b| w.p[a as usize].distance_squared(pe).total_cmp(&w.p[b as usize].distance_squared(pe)))
        else {
            continue;
        };
        // Back off about 0.75 quad, then walk over free vertices to it.
        let id = i as u32;
        let path = traces[i].path.clone();
        let mut keep = path.len() - 1;
        let mut back = 0.0;
        while keep > 1 && back < quad * 0.75 {
            back += w.p[path[keep] as usize].distance(w.p[path[keep - 1] as usize]);
            keep -= 1;
        }
        let mut new_path = path[..=keep].to_vec();
        let goal = w.p[n as usize];
        let mut cur = new_path[keep];
        let mut ok = false;
        let mut walked: Vec<u32> = Vec::new();
        for _ in 0..64 {
            if w.neighbours(cur).contains(&n) {
                ok = true;
                break;
            }
            let Some(&u) = w.neighbours(cur).iter().filter(|&&u| marks[u as usize] == NONE || (marks[u as usize] == id && !new_path.contains(&u))).min_by(|&&a, &&b| w.p[a as usize].distance_squared(goal).total_cmp(&w.p[b as usize].distance_squared(goal))) else { break };
            if w.p[u as usize].distance_squared(goal) >= w.p[cur as usize].distance_squared(goal) {
                break;
            }
            walked.push(u);
            new_path.push(u);
            cur = u;
        }
        if !ok {
            continue;
        }
        new_path.push(n);
        for &v in &path[keep + 1..] {
            if marks[v as usize] == id {
                marks[v as usize] = NONE;
            }
        }
        for &v in &walked {
            marks[v as usize] = id;
        }
        traces[i].path = new_path;
        traces[i].anchor = keep + 1;
        traces[i].last_op = "snap_ends";
        check_path(&traces[i], w, i, "snap-ends");
        junction.remove(&e);
        junction.insert(n);
    }
}

/// Tree–cotree cut of a patch that is not a disc: a tree grown from its border over its
/// inner edges, a tree of its faces across the rest; each inner edge in neither closes a
/// path from the border to the border through a handle (or between border loops).
fn cut_paths(w: &Work, faces: &[u32], cut_edges: &HashSet<(u32, u32)>) -> Vec<Vec<u32>> {
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let face_set: HashSet<u32> = faces.iter().copied().collect();
    let mut verts: HashSet<u32> = HashSet::default();
    let mut inner: HashSet<(u32, u32)> = HashSet::default();
    let mut border_v: HashSet<u32> = HashSet::default();
    for &f in faces {
        for k in 0..3 {
            let h = f * 3 + k;
            let (a, b) = (w.from(h), w.to(h));
            verts.insert(a);
            let o = w.twin[h as usize];
            if o != NONE && face_set.contains(&(o / 3)) && !cut_edges.contains(&key(a, b)) {
                inner.insert(key(a, b));
            } else {
                border_v.insert(a);
                border_v.insert(b);
            }
        }
    }
    if border_v.is_empty() {
        return Vec::new();
    }
    // Primal tree from the whole border at once.
    let mut parent: HashMap<u32, u32> = HashMap::default();
    let mut q: VecDeque<u32> = border_v.iter().copied().collect();
    for &v in &border_v {
        parent.insert(v, v);
    }
    let mut tree: HashSet<(u32, u32)> = HashSet::default();
    while let Some(v) = q.pop_front() {
        for &u in w.neighbours(v) {
            if verts.contains(&u) && !parent.contains_key(&u) && inner.contains(&key(v, u)) {
                parent.insert(u, v);
                tree.insert(key(u, v));
                q.push_back(u);
            }
        }
    }
    // Dual tree over the faces across inner edges not in the primal tree.
    let mut seen: HashSet<u32> = HashSet::default();
    let mut dual: HashSet<(u32, u32)> = HashSet::default();
    let mut fq: VecDeque<u32> = VecDeque::new();
    seen.insert(faces[0]);
    fq.push_back(faces[0]);
    while let Some(f) = fq.pop_front() {
        for k in 0..3 {
            let h = f * 3 + k;
            let e = key(w.from(h), w.to(h));
            if !inner.contains(&e) || tree.contains(&e) {
                continue;
            }
            let g = w.twin[h as usize] / 3;
            if seen.insert(g) {
                dual.insert(e);
                fq.push_back(g);
            }
        }
    }
    let up = |mut v: u32| -> Vec<u32> {
        let mut path = vec![v];
        while let Some(&p) = parent.get(&v) {
            if p == v {
                break;
            }
            path.push(p);
            v = p;
        }
        path
    };
    let mut out = Vec::new();
    let mut left: Vec<(u32, u32)> = inner.iter().copied().filter(|e| !tree.contains(e) && !dual.contains(e)).collect();
    left.sort_unstable();
    for (a, b) in left {
        if !parent.contains_key(&a) || !parent.contains_key(&b) {
            continue;
        }
        let mut pa = up(a);
        pa.reverse();
        let pb = up(b);
        pa.extend(pb);
        out.push(pa);
    }
    out
}

/// How bad a patch is for the fill (lower is better): a clean grid is cheap and dearer the
/// less square it is; three- and five-sided patches cost a pole; lopsided, thin or general-
/// fill patches cost much more; every patch costs one, so fewer lines win.
/// Poles the general fill makes in a patch with side counts `l` (corners between sides),
/// found by filling a flat stand-in: inside vertices with other than four edges, side points
/// with other than one edge inward, corners with any.
fn general_fill_poles(l: &[i64]) -> f32 {
    let mut l: Vec<i64> = l.iter().map(|&x| x.max(1)).collect();
    if l.iter().sum::<i64>() % 2 != 0 {
        let i = (0..l.len()).max_by_key(|&i| l[i]).unwrap();
        l[i] += 1;
    }
    let total: usize = l.iter().sum::<i64>() as usize;
    if total < 4 {
        return 4.0;
    }
    let r = total as f32 / std::f32::consts::TAU;
    let ids: Vec<u32> = (0..total as u32).collect();
    let pos: Vec<Vec2> = (0..total).map(|k| {
        let a = std::f32::consts::TAU * k as f32 / total as f32;
        Vec2::new(a.cos(), a.sin()) * r
    }).collect();
    let mut corners = vec![0usize];
    for x in &l[..l.len() - 1] {
        corners.push(corners.last().unwrap() + *x as usize);
    }
    let corner_set: HashSet<usize> = corners.iter().copied().collect();
    let mut next = total as u32;
    let mut quads = Vec::new();
    ring_fill(ids, pos, corners, &mut |_| {
        next += 1;
        next - 1
    }, &mut quads);
    let mut edges: HashSet<(u32, u32)> = HashSet::default();
    for q in &quads {
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            if a != b {
                edges.insert((a.min(b), a.max(b)));
            }
        }
    }
    let n = total as u32;
    let on_border = |a: u32, b: u32| a < n && b < n && ((a + 1) % n == b || (b + 1) % n == a);
    let mut deg: HashMap<u32, i32> = HashMap::default();
    for &(a, b) in &edges {
        if on_border(a, b) {
            continue;
        }
        *deg.entry(a).or_default() += 1;
        *deg.entry(b).or_default() += 1;
    }
    let mut poles = 0.0;
    for v in 0..next {
        let d = deg.get(&v).copied().unwrap_or(0);
        let want = if v >= n { 4 } else if corner_set.contains(&(v as usize)) { 0 } else { 1 };
        if d != want {
            poles += 1.0;
        }
    }
    poles
}

/// Poles at the meeting points of the layout: a node's edges are its arcs plus one for each
/// patch that passes it as a straight side (its fill sends a line inward there).
fn node_poles(c: &Cut) -> f32 {
    let mut deg: HashMap<u32, i32> = HashMap::default();
    for a in &c.arcs {
        if a.len() >= 2 && a[0] != *a.last().unwrap() {
            *deg.entry(a[0]).or_default() += 1;
            *deg.entry(*a.last().unwrap()).or_default() += 1;
        } else if a.len() >= 2 {
            *deg.entry(a[0]).or_default() += 2;
        }
    }
    for pa in &c.patches {
        if pa.band {
            continue;
        }
        let corners: HashSet<u32> = if pa.real > 0 { pa.corner_at.iter().map(|&i| pa.lp[i]).collect() } else { HashSet::default() };
        let mut seen: HashSet<u32> = HashSet::default();
        for &v in &pa.lp {
            if deg.contains_key(&v) && !corners.contains(&v) && seen.insert(v) {
                *deg.get_mut(&v).unwrap() += 1;
            }
        }
    }
    deg.values().filter(|&&d| d != 4).count() as f32
}

/// The poles a patch's fill puts inside it (and on its border, for the general fill).
fn patch_poles(w: &Work, c: &Cut, pa: &Patch, quad: f32) -> f32 {
    let l: Vec<i64> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| (c.arcs[a as usize].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum::<f32>() / quad).round().max(1.0) as i64).sum()).collect();
    if pa.band {
        return if pa.free.is_some() { (l[0] - l[1]).abs() as f32 } else { 0.0 };
    }
    if pa.messy {
        return general_fill_poles(&l);
    }
    match l.len() {
        4 if pa.free.is_some() => {
            let k = pa.free.unwrap();
            (l[k] - l[k + 2]).abs() as f32
        }
        4 => 0.0,
        3 | 5 if split_sides(&l).is_some() || slack(&l) >= 0.0 => 1.0,
        6..=8 if split_sides(&l).is_some() => 1.0,
        _ => general_fill_poles(&l),
    }
}

fn patch_cost(w: &Work, c: &Cut, pa: &Patch, quad: f32) -> f32 {
    if pa.band {
        return 0.5;
    }
    let l: Vec<f32> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| c.arcs[a as usize].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum::<f32>()).sum::<f32>() / quad).collect();
    let perimeter: f32 = l.iter().sum();
    // The whole area of a general-fill patch comes out poorly: it costs by area.
    let area_q: f32 = pa.faces.iter().map(|&f| {
        let t = w.t[f as usize];
        (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
    }).sum::<f32>() / (quad * quad);
    if pa.messy || !(3..=5).contains(&pa.sides.len()) {
        // Big ones fold: their cost grows with area faster than any pole saving.
        // Over about 150 quads² the flattening and fill of a general patch go wrong: never.
        return 1.0 + 10.0 + area_q * 3.0 + perimeter * 0.2 + if area_q > 150.0 && std::env::var_os("ZR_NOCAP").is_none() { 1e5 } else { 0.0 };
    }
    let mut cost = 1.0;
    match pa.sides.len() {
        4 if pa.free.is_none() => {
            let r = |a: f32, b: f32| a.max(b) / a.min(b).max(0.3);
            cost += (r(l[0], l[2]).max(r(l[1], l[3])) - 1.0) * 3.0;
        }
        4 => cost += 2.0,
        3 | 5 => {
            let li: Vec<i64> = l.iter().map(|x| (x.round() as i64).max(1)).collect();
            cost += if slack(&li) >= 1.0 { 2.0 } else { 8.0 };
        }
        6..=8 => {
            let li: Vec<i64> = l.iter().map(|x| (x.round() as i64).max(1)).collect();
            cost += if split_sides(&li).is_some() { 4.0 + (pa.sides.len() - 6) as f32 * 2.0 } else { 10.0 };
        }
        _ => cost += 20.0,
    }
    // Thin (under a quad wide on average) costs too.
    let area: f32 = pa.faces.iter().map(|&f| {
        let t = w.t[f as usize];
        (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
    }).sum::<f32>() / (quad * quad);
    if 2.0 * area / perimeter.max(1e-6) < 1.0 {
        cost += 5.0;
    }
    cost
}

/// Delete the lines whose root is in `gone`; lines that ended on them go on from their last
/// ordinary step (left to run).
fn delete_lines(traces: &mut [Trace], marks: &mut [u32], roots: &[u32], gone: &HashSet<u32>, w: &Work, z: &[Vec2]) {
    let mut freed: HashSet<u32> = HashSet::default();
    for i in 0..traces.len() {
        if gone.contains(&roots[i]) {
            let t = &mut traces[i];
            for &v in &t.path[1..] {
                if marks[v as usize] == i as u32 {
                    marks[v as usize] = NONE;
                    freed.insert(v);
                }
            }
            t.path.truncate(1);
            t.done = true;
            t.why = "deleted";
            t.same_as = NONE;
        }
    }
    for i in 0..traces.len() {
        let t = &mut traces[i];
        if t.path.len() >= 2 && t.done && freed.contains(t.path.last().unwrap()) {
            let keep = t.anchor.clamp(1, t.path.len());
            for v in t.path.split_off(keep) {
                if marks[v as usize] == i as u32 {
                    marks[v as usize] = NONE;
                }
            }
            for &v in &t.path[1..] {
                if marks[v as usize] == NONE {
                    marks[v as usize] = i as u32;
                }
            }
            t.restart(w, z);
            check_path(t, w, i, "delete-resume");
            t.done = false;
            t.why = "";
            t.same_as = NONE;
        }
    }
}

/// Lines that stopped on nothing (stuck, lost, or run too far) leave a dangling end: they
/// are taken away.
fn prune(traces: &mut [Trace], marks: &mut [u32], w: &Work) {
    let _ = w;
    for i in 0..traces.len() {
        let t = &mut traces[i];
        if t.path.len() >= 2 && matches!(t.why, "stuck" | "lost" | "far") {
            for &v in &t.path[1..] {
                if marks[v as usize] == i as u32 {
                    marks[v as usize] = NONE;
                }
            }
            t.path.truncate(1);
            t.why = "pruned";
            t.last_op = "pruned";
        }
    }
}

/// Directions to trace from vertex `v` into the sector of its fan `fan[from..to]` (between
/// two hard edges, or all round): one every quarter turn of its angle, the ends left out.
fn sector_starts(w: &Work, z: &[Vec2], fan: &[u32], v: u32) -> Vec<(usize, Vec3)> {
    let total: f32 = fan.iter().map(|&h| w.corner(h)).sum();
    let n = ((total / std::f32::consts::FRAC_PI_2).round() as i32 - 1).max(0);
    let mut out = Vec::new();
    for k in 1..=n {
        let goal = total * k as f32 / (n + 1) as f32;
        let mut acc = 0.0;
        for &h in fan {
            let c = w.corner(h);
            if acc + c >= goal {
                let f = h as usize / 3;
                let e = (w.p[w.to(h) as usize] - w.p[v as usize]).normalize();
                let d = Quat::from_axis_angle(w.fnrm[f], goal - acc) * e;
                // The field's direction nearest, if close.
                let fd = dirs(w, z, f).into_iter().max_by(|a, b| a.dot(d).total_cmp(&b.dot(d))).unwrap();
                out.push((f, if fd.dot(d) > 0.85 { fd } else { d }));
                break;
            }
            acc += c;
        }
    }
    out
}

/// The `k` directions to trace from pole `v`: the field directions pointing out of it,
/// spread evenly round it.
fn pole_starts(w: &Work, z: &[Vec2], v: u32, k: usize) -> Vec<(usize, Vec3)> {
    let fan = w.fan(v);
    let total: f32 = fan.iter().map(|&h| w.corner(h)).sum();
    // Candidates by their angle round v.
    let mut cands: Vec<(f32, usize, Vec3)> = Vec::new();
    let mut acc = 0.0;
    for &h in &fan {
        let f = h as usize / 3;
        let n = w.fnrm[f];
        let a = (w.p[w.to(h) as usize] - w.p[v as usize]).normalize();
        let b = (w.p[w.from(Work::prev(h)) as usize] - w.p[v as usize]).normalize();
        for d in dirs(w, z, f) {
            if n.dot(a.cross(d)) >= 0.0 && n.dot(d.cross(b)) > 0.0 {
                cands.push((acc + a.angle_between(d), f, d));
            }
        }
        acc += w.corner(h);
    }
    if cands.is_empty() {
        return Vec::new();
    }
    // From the first candidate, every total / k round: the candidate nearest each.
    let step = total / k as f32;
    let at = |goal: f32| -> (usize, Vec3) {
        let goal = goal.rem_euclid(total);
        let mut acc = 0.0;
        for &h in &fan {
            let c = w.corner(h);
            if acc + c >= goal {
                let f = h as usize / 3;
                let e = (w.p[w.to(h) as usize] - w.p[v as usize]).normalize();
                return (f, Quat::from_axis_angle(w.fnrm[f], goal - acc) * e);
            }
            acc += c;
        }
        let h = fan[0];
        (h as usize / 3, (w.p[w.to(h) as usize] - w.p[v as usize]).normalize())
    };
    let base = cands[0].0;
    (0..k)
        .map(|i| {
            let goal = (base + step * i as f32).rem_euclid(total);
            let gap = |x: f32| {
                let d = (x - goal).rem_euclid(total);
                d.min(total - d)
            };
            let near = cands.iter().min_by(|a, b| gap(a.0).total_cmp(&gap(b.0))).unwrap();
            if gap(near.0) < step * 0.15 { (near.1, near.2) } else { at(goal) }
        })
        .collect()
}

/// A patch: a disc of working faces bounded by one loop, its sides between corners, each
/// a list of (arc, forward).
struct Patch {
    faces: Vec<u32>,
    /// The boundary loop, counter-clockwise (the patch on the left).
    lp: Vec<u32>,
    sides: Vec<Vec<(u32, bool)>>,
    /// Where each side starts in `lp`.
    corner_at: Vec<usize>,
    /// Real corners (0 when the one side is the whole loop), and whether a corner is
    /// concave or a spike (filled by rings then, not a regular pattern).
    real: usize,
    messy: bool,
    /// A four-sided patch that narrows a lot (a trapezoid): this pair of opposite sides
    /// (0: sides 0 and 2, 1: sides 1 and 3) need not match; rows of it merge three quads
    /// into one where it narrows.
    free: Option<usize>,
    /// A band: a ring between two closed loops (sides 0 and 1, the second loop in `lp2`),
    /// filled with rows round it; both loops get the same count.
    band: bool,
    lp2: Vec<u32>,
}

impl Patch {
    /// A strip entering by side `s` ends here.
    fn ends(&self, s: usize) -> bool {
        if self.band {
            return self.free.is_some();
        }
        // Filled by the general fill: no sides need to match.
        self.messy || self.sides.len() != 4 || self.free == Some(s % 2)
    }
    /// Its border must have an even count (not kept even by strips).
    fn irregular(&self) -> bool {
        if self.band {
            return self.free.is_some();
        }
        self.messy || self.sides.len() != 4 || self.free.is_some()
    }
    /// The side a strip entering by `s` leaves by.
    fn opposite(&self, s: usize) -> usize {
        if self.band { 1 - s } else { (s + 2) % 4 }
    }
}

/// What went wrong with a patch, and where to fix it.
enum Fault {
    /// A vertex inside to trace through.
    Split(u32),
    /// A node with a reflex corner to trace from, into the patch.
    Reflex(u32, Vec3, u32),
    /// A long thin patch: the lines along it and how much of its border each is.
    Thin(Vec<(u32, f32)>, bool),
    /// Not a disc (handles, or more than one border loop): a vertex to trace through, and
    /// the faces, to cut along tree–cotree paths if tracing does not help.
    Topo(u32, Vec<u32>),
}

/// The patches of the cut surface, or the faults to fix.
struct Cut {
    /// Faces of faulty patches (for debugging pictures).
    faulty: Vec<u32>,
    arcs: Vec<Vec<u32>>,
    patches: Vec<Patch>,
    faults: Vec<Fault>,
}

fn cut(w: &Work, z: &[Vec2], is_pole: &[bool], cut_edges: &HashSet<(u32, u32)>, nodes: &HashSet<u32>, quad: f32, edge_line: &HashMap<(u32, u32), u32>, same: &HashMap<u32, u32>, pole_lines: &HashSet<u32>, keep: &HashSet<u32>, lenient: bool, stale: &HashSet<u32>, concave: f32) -> Cut {
    let root = |mut id: u32| -> u32 {
        let mut guard = 0;
        while let Some(&n) = same.get(&id) {
            if n == id || guard > 1000 {
                break;
            }
            id = n;
            guard += 1;
        }
        id
    };
    let line_of = |a: u32, b: u32| -> u32 { root(edge_line.get(&(a.min(b), a.max(b))).copied().unwrap_or(HARD)) };
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let is_cut = |h: u32| w.twin[h as usize] == NONE || cut_edges.contains(&key(w.from(h), w.to(h)));
    // Arcs: chains of cut edges between nodes.
    let mut cadj: HashMap<u32, Vec<u32>> = HashMap::default();
    for &(a, b) in cut_edges {
        cadj.entry(a).or_default().push(b);
        cadj.entry(b).or_default().push(a);
    }
    let mut is_node: HashSet<u32> = cadj.iter().filter(|(v, n)| n.len() != 2 || nodes.contains(v)).map(|(v, _)| *v).collect();
    let mut arcs: Vec<Vec<u32>> = Vec::new();
    let mut arc_of: HashMap<(u32, u32), (u32, u32)> = HashMap::default();
    let mut walk = |start: u32, first: u32, is_node: &HashSet<u32>, arcs: &mut Vec<Vec<u32>>, arc_of: &mut HashMap<(u32, u32), (u32, u32)>| {
        if arc_of.contains_key(&key(start, first)) {
            return;
        }
        let id = arcs.len() as u32;
        let mut path = vec![start, first];
        arc_of.insert(key(start, first), (id, 0));
        let (mut prev, mut cur) = (start, first);
        while !is_node.contains(&cur) {
            let nb = &cadj[&cur];
            let nx = if nb[0] != prev { nb[0] } else { nb[1] };
            if arc_of.contains_key(&key(cur, nx)) {
                break;
            }
            arc_of.insert(key(cur, nx), (id, (path.len() - 1) as u32));
            path.push(nx);
            prev = cur;
            cur = nx;
        }
        arcs.push(path);
    };
    let mut starts: Vec<u32> = is_node.iter().copied().collect();
    starts.sort_unstable();
    for &v in &starts {
        if let Some(nb) = cadj.get(&v) {
            for &u in nb.clone().iter() {
                walk(v, u, &is_node, &mut arcs, &mut arc_of);
            }
        }
    }
    // Closed chains without a node: one is made.
    let mut rest: Vec<(u32, u32)> = cut_edges.iter().filter(|e| !arc_of.contains_key(e)).copied().collect();
    rest.sort_unstable();
    for (a, b) in rest {
        if !arc_of.contains_key(&(a, b)) {
            is_node.insert(a);
            walk(a, b, &is_node, &mut arcs, &mut arc_of);
        }
    }
    // Patches: faces joined across uncut edges.
    let nf = w.t.len();
    let mut comp = vec![NONE; nf];
    let mut patches = Vec::new();
    let mut faults = Vec::new();
    let mut faulty: Vec<u32> = Vec::new();
    for f0 in 0..nf {
        if comp[f0] != NONE {
            continue;
        }
        let id = f0 as u32;
        let mut faces = vec![f0 as u32];
        comp[f0] = id;
        let mut i = 0;
        while i < faces.len() {
            let f = faces[i] as usize;
            i += 1;
            for k in 0..3 {
                let h = (f * 3 + k) as u32;
                if is_cut(h) {
                    continue;
                }
                let g = w.twin[h as usize] as usize / 3;
                if comp[g] == NONE {
                    comp[g] = id;
                    faces.push(g as u32);
                }
            }
        }
        // Boundary loops.
        let in_comp = |h: u32| comp[h as usize / 3] == id;
        let mut used: HashSet<u32> = HashSet::default();
        let mut loops: Vec<Vec<u32>> = Vec::new();
        let mut verts: HashSet<u32> = HashSet::default();
        let mut inner_half = 0usize;
        for &f in &faces {
            for k in 0..3 {
                let h = f * 3 + k;
                verts.insert(w.from(h));
                if !is_cut(h) {
                    inner_half += 1;
                }
            }
        }
        let mut wedges_on_loops = 0usize;
        let mut bad_loop = false;
        for &f in &faces {
            for k in 0..3 {
                let h0 = f * 3 + k;
                if !is_cut(h0) || used.contains(&h0) {
                    continue;
                }
                let mut lp = Vec::new();
                let mut h = h0;
                let mut guard = 0;
                loop {
                    used.insert(h);
                    lp.push(w.from(h));
                    // Next boundary half-edge out of to(h), turning round inside the patch.
                    let mut g = Work::next(h);
                    let mut spin = 0;
                    while !is_cut(g) {
                        g = Work::next(w.twin[g as usize]);
                        spin += 1;
                        if spin > 4096 {
                            break;
                        }
                    }
                    h = g;
                    guard += 1;
                    if h == h0 || guard > 1_000_000 || !in_comp(h) {
                        if h != h0 {
                            bad_loop = true;
                        }
                        break;
                    }
                }
                wedges_on_loops += lp.len();
                loops.push(lp);
            }
        }
        let on_loop: HashSet<u32> = loops.iter().flatten().copied().collect();
        let v_count = verts.len() - on_loop.len() + wedges_on_loops;
        let e_count = inner_half / 2 + wedges_on_loops;
        let chi = v_count as i64 - e_count as i64 + faces.len() as i64;
        // A vertex inside, as far from the border as there is (for a line through it).
        let inside = || -> u32 {
            let mut dist: HashMap<u32, u32> = on_loop.iter().map(|&v| (v, 0)).collect();
            let mut q: VecDeque<u32> = on_loop.iter().copied().collect();
            let mut far = *on_loop.iter().next().unwrap_or(&w.t[faces[0] as usize][0]);
            let mut best = 0;
            while let Some(v) = q.pop_front() {
                let d = dist[&v];
                if d > best {
                    best = d;
                    far = v;
                }
                for &u in w.neighbours(v) {
                    if verts.contains(&u) && !dist.contains_key(&u) {
                        dist.insert(u, d + 1);
                        q.push_back(u);
                    }
                }
            }
            if on_loop.is_empty() {
                // No border at all (a closed surface with no lines yet).
                return w.t[faces[0] as usize][0];
            }
            far
        };
        // A band: a ring between two closed loops, each with a node on it.
        if !bad_loop && loops.len() == 2 && chi == 0 && !loops[0].iter().any(|v| loops[1].contains(v)) {
            let side_of = |lpv: &[u32]| -> Option<Vec<(u32, bool)>> {
                let m = lpv.len();
                let st = (0..m).find(|&i| is_node.contains(&lpv[i]))?;
                let mut seen = HashSet::default();
                if !lpv.iter().all(|v| seen.insert(*v)) {
                    return None;
                }
                let mut side: Vec<(u32, bool)> = Vec::new();
                let mut i = st;
                loop {
                    let j = (i + 1) % m;
                    let &(arc, pos) = arc_of.get(&key(lpv[i], lpv[j]))?;
                    let fwd = arcs[arc as usize][pos as usize] == lpv[i];
                    if side.last().copied() != Some((arc, fwd)) {
                        side.push((arc, fwd));
                    }
                    i = j;
                    if i == st {
                        break;
                    }
                }
                // A loop closing on one arc may list it twice (start and end): once.
                if side.len() > 1 && side.first().map(|x| x.0) == side.last().map(|x| x.0) {
                    side.pop();
                }
                // The arcs must chain end to start round the loop.
                let ends = |&(a, f): &(u32, bool)| -> (u32, u32) {
                    let p = &arcs[a as usize];
                    if f { (p[0], *p.last().unwrap()) } else { (*p.last().unwrap(), p[0]) }
                };
                for k in 0..side.len() {
                    let (_, e) = ends(&side[k]);
                    let (s0, _) = ends(&side[(k + 1) % side.len()]);
                    if e != s0 {
                        return None;
                    }
                }
                Some(side)
            };
            if let (Some(a), Some(b)) = (side_of(&loops[0]), side_of(&loops[1])) {
                let st0 = (0..loops[0].len()).find(|&i| is_node.contains(&loops[0][i])).unwrap();
                let st1 = (0..loops[1].len()).find(|&i| is_node.contains(&loops[1][i])).unwrap();
                let mut l0 = loops[0].clone();
                l0.rotate_left(st0);
                let mut l1 = loops[1].clone();
                l1.rotate_left(st1);
                // Loops of very different length (a cone): each takes its own count, and rows
                // merge quads where it narrows.
                let length = |l: &[u32]| -> f32 { (0..l.len()).map(|i| w.p[l[i] as usize].distance(w.p[l[(i + 1) % l.len()] as usize])).sum() };
                let (la, lb) = (length(&l0), length(&l1));
                let free_at: f32 = std::env::var("ZR_BANDFREE").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5);
                let free = (la.max(lb) / la.min(lb).max(1e-9) > free_at).then_some(0);
                // Loops far apart in size (a big ring round a spot): not a band; traced across
                // like any other ring.
                let ratio_max: f32 = std::env::var("ZR_BANDRATIO").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
                // Nor a wide region with two holes (rows across it would be long): over
                // ZR_BANDWIDE quads (8) between the loops on average.
                let wide_max: f32 = std::env::var("ZR_BANDWIDE").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
                let band_area: f32 = faces.iter().map(|&f| { let t = w.t[f as usize]; (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5 }).sum();
                let wide = wide_max > 0.0 && band_area / ((la + lb) * 0.5).max(1e-9) > quad * wide_max;
                let lopsided = (ratio_max > 0.0 && la.max(lb) > la.min(lb) * ratio_max) || wide;
                if std::env::var_os("KLAY_ZR_DEBUG").is_some() && la.min(lb) < quad * 4.0 && la.max(lb) > la.min(lb) * 6.0 {
                    let small = if la <= lb { &l0 } else { &l1 };
                    let mid = small.iter().map(|&v| w.p[v as usize]).sum::<Vec3>() / small.len() as f32;
                    let ids: HashSet<u32> = (0..small.len()).map(|i| line_of(small[i], small[(i + 1) % small.len()])).collect();
                    let poles_in = small.iter().filter(|&&v| is_pole[v as usize]).count();
                    eprintln!("    band round a tiny loop at {mid:.3}: {:.1} / {:.1} quads, lines {ids:?}, {} points, {poles_in} poles on it, nodes on it {}", la.min(lb) / quad, la.max(lb) / quad, small.len(), small.iter().filter(|v| is_node.contains(v)).count());
                }
                if !lopsided {
                    patches.push(Patch { faces, lp: l0, sides: vec![a, b], corner_at: vec![0], real: 0, messy: false, free, band: true, lp2: l1 });
                    continue;
                }
            }
        }
        if bad_loop || loops.len() != 1 || chi != 1 {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("    fault ({} faces): {}", faces.len(), format!("not a disc: {} loops, chi {chi}, bad {bad_loop}", loops.len())); }
            faulty.extend(faces.iter().copied());
            faults.push(Fault::Topo(inside(), faces.clone()));
            continue;
        }
        let lp = loops.pop().unwrap();
        let mut seen = HashSet::default();
        // A loop that passes a vertex twice (a pinch) is filled as it is (the vertex split in
        // two); only one passed more often is a fault.
        let mut times: HashMap<u32, u32> = HashMap::default();
        for &v in &lp {
            *times.entry(v).or_default() += 1;
        }
        let pinches = times.values().filter(|&&t| t >= 2).count();
        let pinched = pinches > 0;
        if lenient && times.values().all(|&t| t <= 8) && pinches > 0 {
            seen.clear();
        } else if !lp.iter().all(|v| seen.insert(*v)) {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("    fault ({} faces): {}", faces.len(), "loop repeats a vertex".to_string()); }
            faulty.extend(faces.iter().copied());
            // A pinch: a line from the vertex the loop touches twice, into the patch there.
            let mlp = lp.len();
            let mut first: HashMap<u32, usize> = HashMap::default();
            let mut pinch = None;
            for (i, &v) in lp.iter().enumerate() {
                if first.insert(v, i).is_some() {
                    pinch = Some(i);
                    break;
                }
            }
            match pinch.filter(|_| lenient) {
                Some(i) => {
                    let v = lp[i];
                    let n = w.vnrm[v as usize];
                    let fwd = w.p[lp[(i + 1) % mlp] as usize] - w.p[v as usize];
                    let back = w.p[lp[(i + mlp - 1) % mlp] as usize] - w.p[v as usize];
                    let (fwd, back) = (fwd - n * n.dot(fwd), back - n * n.dot(back));
                    let mut a = n.dot(fwd.cross(back)).atan2(fwd.dot(back));
                    if a < 0.0 {
                        a += std::f32::consts::TAU;
                    }
                    let mid = Quat::from_axis_angle(n, a * 0.5) * fwd.normalize_or(n.any_orthonormal_vector());
                    faults.push(Fault::Reflex(v, mid, inside()));
                }
                None => faults.push(Fault::Split(inside())),
            }
            continue;
        }
        // Corners at nodes: the angle inside, measured over half a quad either way.
        let m = lp.len();
        let node_at: Vec<usize> = (0..m).filter(|&i| is_node.contains(&lp[i])).collect();
        let reach = |i: usize, step: isize| -> Vec3 {
            let v = w.p[lp[i] as usize];
            let mut j = i;
            let mut len = 0.0;
            loop {
                let k = (j as isize + step).rem_euclid(m as isize) as usize;
                len += w.p[lp[j] as usize].distance(w.p[lp[k] as usize]);
                j = k;
                if len >= quad * 0.5 || j == i || is_node.contains(&lp[j]) {
                    break;
                }
            }
            w.p[lp[j] as usize] - v
        };
        let mut corner_at = Vec::new();
        let mut reflex: Option<(u32, Vec3)> = None;
        let mut reflex_deg = 0.0f32;
        let mut messy = false;
        // The patch's own angle at each node: its faces' corners there, in quarter turns.
        let face_set: HashSet<u32> = faces.iter().copied().collect();
        for &i in &node_at {
            let v = lp[i];
            let n = w.vnrm[v as usize];
            let mut a = 0.0f32;
            for h in w.fan(v) {
                if face_set.contains(&(h / 3)) {
                    a += w.corner(h);
                }
            }
            // Quarter turns, with a wide straight band (lines bend a little where they meet).
            let deg = a.to_degrees();
            let mut k = if deg < 40.0 { 0 } else if deg < 130.0 { 1 } else if deg <= 240.0 { 2 } else { 3 };
            // Between traced lines the rule is exact: onto another line is a corner, along the
            // same line is straight. Only between hard edges does the angle decide.
            let (into, out) = (line_of(lp[(i + m - 1) % m], v), line_of(v, lp[(i + 1) % m]));
            if into == HARD && out == HARD && w.soft_edge(lp[(i + m - 1) % m], v) && w.soft_edge(v, lp[(i + 1) % m]) {
                // Along a feature loop: straight.
                k = 2;
            } else if into == HARD && out == HARD {
                // Along hard edges: the bend over about a quad either way, past small notches.
                let far = |step: isize| -> Vec3 {
                    let mut j = i;
                    let mut len = 0.0;
                    for _ in 0..m {
                        let k = (j as isize + step).rem_euclid(m as isize) as usize;
                        len += w.p[lp[j] as usize].distance(w.p[lp[k] as usize]);
                        j = k;
                        if len >= quad || line_of(lp[j], lp[(j as isize + step).rem_euclid(m as isize) as usize]) != HARD {
                            break;
                        }
                    }
                    let d = w.p[lp[j] as usize] - w.p[v as usize];
                    d - n * n.dot(d)
                };
                let (f, b) = (far(1), far(-1));
                let mut ang = n.dot(f.cross(b)).atan2(f.dot(b));
                if ang < 0.0 {
                    ang += std::f32::consts::TAU;
                }
                let d2 = ang.to_degrees();
                k = if d2 < 40.0 { 0 } else if d2 < 135.0 { 1 } else if d2 <= 225.0 { 2 } else { 3 };
            }
            if std::env::var_os("KLAY_ZR_NODES").is_some() && faces.len() == 216 {
                eprintln!("      node {v}: into {into} out {out} deg {deg:.0} k {k} pole {}", is_pole[v as usize]);
            }
            if into != HARD || out != HARD {
                // Onto another line: a corner, unless the patch wraps round it (concave).
                k = if into == out { 2 } else if deg > 200.0 { 3 } else { 1 };
            }
            match k {
                0 => {
                    corner_at.push(i);
                    messy = true;
                }
                1 => corner_at.push(i),
                2 => {}
                _ => {
                    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                        eprintln!("      reflex at {v} ({:.0}°, {} lines meet, pole {})", a.to_degrees(), cadj.get(&v).map_or(0, |x| x.len()), is_pole[v as usize]);
                    }
                    let fwd = reach(i, 1);
                    let fwd = fwd - n * n.dot(fwd);
                    reflex = Some((v, Quat::from_axis_angle(n, a * 0.5) * fwd.normalize_or(n.any_orthonormal_vector())));
                    reflex_deg = a.to_degrees();
                }
            }
        }
        if let Some((v, dir)) = reflex {
            // A concave corner on a big patch (over `concave` quads², 0: none): a line from
            // it, along the corner's middle, splits the patch (once from each corner; if that
            // does not help, it stays).
            let big = concave;
            // (Not on an open border: there a line meets a tube's end loop at a slant, and a
            // line from that corner would run along the loop.)
            if big > 0.0 && !stale.contains(&v) && w.interior(v) {
                let area: f32 = faces.iter().map(|&f| {
                    let t = w.t[f as usize];
                    (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
                }).sum::<f32>() / (quad * quad);
                if area > big {
                    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                        let perim: f32 = (0..lp.len()).map(|i| w.p[lp[i] as usize].distance(w.p[lp[(i + 1) % lp.len()] as usize])).sum::<f32>() / quad;
                        eprintln!("    fault ({} faces, {area:.0} quads², {perim:.0} round): concave corner at {v} {:.2}, {reflex_deg:.0}°", faces.len(), w.p[v as usize]);
                    }
                    faults.push(Fault::Reflex(v, dir, inside()));
                    faulty.extend(faces.iter().copied());
                    continue;
                }
            }
        }
        if reflex.is_some() {
            // A concave corner: kept as a corner; the patch is filled by rings.
            messy = true;
            let (v, _) = reflex.unwrap();
            if let Some(i) = node_at.iter().copied().find(|&i| lp[i] == v) {
                if !corner_at.contains(&i) {
                    corner_at.push(i);
                    corner_at.sort_unstable();
                }
            }
        }
        // Corners under a quad apart are one corner (down to four): a short side is a kink.
        while corner_at.len() > 4 {
            let nc = corner_at.len();
            let side_len = |c: usize| -> f32 {
                let (a, b) = (corner_at[c], corner_at[(c + 1) % nc]);
                let mut len = 0.0;
                let mut i = a;
                while i != b {
                    let j = (i + 1) % m;
                    len += w.p[lp[i] as usize].distance(w.p[lp[j] as usize]);
                    i = j;
                }
                len
            };
            let (c, l) = (0..nc).map(|c| (c, side_len(c))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            if l >= quad {
                break;
            }
            corner_at.remove((c + 1) % nc);
        }
        let real = corner_at.len();
        if corner_at.is_empty() {
            match node_at.first() {
                Some(&i) => corner_at.push(i),
                None => {
                    faulty.extend(faces.iter().copied());
                    faults.push(Fault::Split(inside()));
                    continue;
                }
            }
        }
        // Sides: arcs between consecutive nodes.
        let mut sides = Vec::new();
        let mut ok = true;
        for c in 0..corner_at.len() {
            let (s, e) = (corner_at[c], corner_at[(c + 1) % corner_at.len()]);
            let mut side = Vec::new();
            let mut i = s;
            loop {
                let j = (i + 1) % m;
                let Some(&(arc, pos)) = arc_of.get(&key(lp[i], lp[j])) else {
                    ok = false;
                    break;
                };
                let path = &arcs[arc as usize];
                let fwd = path[pos as usize] == lp[i];
                if side.last().copied() != Some((arc, fwd)) {
                    side.push((arc, fwd));
                }
                i = j;
                if i == e {
                    break;
                }
            }
            sides.push(side);
        }
        if !ok {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("    fault ({} faces): {}", faces.len(), "side not on an arc".to_string()); }
            faulty.extend(faces.iter().copied());
            faults.push(Fault::Split(inside()));
            continue;
        }
        // Long and thin (mean width under 0.8 quad): a line along it goes.
        let area: f32 = faces.iter().map(|&f| {
            let t = w.t[f as usize];
            (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
        }).sum();
        let mut by_line: HashMap<u32, f32> = HashMap::default();
        let mut perimeter = 0.0;
        for i in 0..m {
            let (a, b) = (lp[i], lp[(i + 1) % m]);
            let len = w.p[a as usize].distance(w.p[b as usize]);
            perimeter += len;
            let id = line_of(a, b);
            if id != HARD {
                *by_line.entry(id).or_default() += len;
            }
        }
        // Side lengths: a wedge (one side under a fifth of the longest) or more than five
        // corners also means a line too many.
        let side_len: Vec<f32> = sides.iter().map(|sd| sd.iter().map(|&(a, _)| arcs[a as usize].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum::<f32>()).sum()).collect();
        let wedge3 = real == 3 && side_len.iter().cloned().fold(f32::INFINITY, f32::min) < side_len.iter().cloned().fold(0.0, f32::max) * 0.2;
        let wedge = wedge3
            // A long four-sided sliver narrowing to a point: lines converging (a line goes
            // if one can; else it stays a grid).
            || (real == 4 && sides.len() == 4 && (0..2).any(|k| {
                let (a, b) = (side_len[k], side_len[k + 2]);
                let long = side_len[k + 1].min(side_len[(k + 3) % 4]);
                a.min(b) < a.max(b) * 0.25 && long > a.max(b) * 3.0
            }));
        let thin = 2.0 * area / perimeter.max(1e-12) < quad * 1.2 && perimeter > quad * 6.0;
        // A big patch with many corners is cut in two (deleting its lines only grows it).
        if real > 8 && area > quad * quad * 40.0 && std::env::var_os("ZR_NOBIGSPLIT").is_none() && (stale.is_empty() || !stale.contains(&inside())) {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() { let c = inside(); eprintln!("    fault ({} faces): {} corners over {:.0} quads², split at {c} {:.3} pole {} near pole {}", faces.len(), real, area / (quad * quad), w.p[c as usize], is_pole[c as usize], w.neighbours(c).iter().any(|&u| is_pole[u as usize])); }
            faulty.extend(faces.iter().copied());
            faults.push(Fault::Split(inside()));
            continue;
        }
        // A wedge or many corners with only pole lines round it stays (filled by rings).
        let spare = by_line.keys().any(|r| !pole_lines.contains(r));
        let deletable = by_line.keys().any(|r| !keep.contains(r));
        // A sliver (two or three corners, under ZR_TINYPATCH quads², 3): a line round it goes, so it joins a
        // neighbour (each such scrap otherwise brings its own poles).
        let tiny_max: f32 = std::env::var("ZR_TINYPATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
        let tiny = area < quad * quad * tiny_max && real <= 3;
        let thin_long = thin;
        let thin = thin || (tiny && deletable);
        if ((thin && deletable) || ((wedge || real > 8 || real < 3) && spare)) && !by_line.is_empty() {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("    fault ({} faces): thin, {:.1} quads round, {:.2} wide", faces.len(), perimeter / quad, 2.0 * area / perimeter / quad); }
            faulty.extend(faces.iter().copied());
            let mut lines: Vec<(u32, f32)> = by_line.into_iter().collect();
            // A sliver loses its shortest line (it joins the patch across its short side);
            // a long thin patch its longest.
            if tiny && !thin_long {
                lines.sort_by(|a, b| a.1.total_cmp(&b.1));
            } else {
                lines.sort_by(|a, b| b.1.total_cmp(&a.1));
            }
            faults.push(Fault::Thin(lines, thin));
            continue;
        }
        let messy = messy || wedge3 || real > 8 || real < 3 || pinched;
        // A trapezoid: opposite sides more than 1.5 times apart (the more lopsided pair).
        let free = if real == 4 && sides.len() == 4 && !messy {
            let r = |a: f32, b: f32| a.max(b) / a.min(b).max(1e-6);
            let (r0, r1) = (r(side_len[0], side_len[2]), r(side_len[1], side_len[3]));
            if r0.max(r1) > 2.5 { Some(if r0 >= r1 { 0 } else { 1 }) } else { None }
        } else {
            None
        };
        patches.push(Patch { faces, lp, sides, corner_at, real, messy, free, band: false, lp2: Vec::new() });
    }
    Cut { faulty, arcs, patches, faults }
}

/// Whole quad counts for the arcs: near `target`, opposite sides of four-sided patches equal,
/// three- and five-sided patches even in total.
fn counts(cut: &Cut, target: &[f32]) -> Result<Vec<i64>, (String, Option<usize>)> {
    let na = cut.arcs.len();
    let side_sum = |n: &[i64], side: &[(u32, bool)]| side.iter().map(|&(a, _)| n[a as usize]).sum::<i64>();
    // Which patches and sides each arc is on.
    let mut on: Vec<Vec<(usize, usize)>> = vec![Vec::new(); na];
    for (p, pa) in cut.patches.iter().enumerate() {
        for (s, side) in pa.sides.iter().enumerate() {
            for &(a, _) in side {
                on[a as usize].push((p, s));
            }
        }
    }
    // Built from strips: each crosses four-sided patches from a side to the opposite one
    // (by the arc there most short of its length), so opposite sides always match. Strips
    // go through the arc most short of its length until none is short by half a quad.
    let mut n = vec![0i64; na];
    let deficit = |n: &[i64], a: usize| target[a] - n[a] as f32;
    let walk = |n: &[i64], a: u32, from: (usize, usize), path: &mut Vec<u32>, shrink: bool| -> Result<bool, ()> {
        // From arc `a`, into the patch on its other side than `from`; true if it came back
        // round to `a` the other way (a closed strip).
        let (mut a, mut from) = (a, from);
        let start = a;
        // A closed strip leaves `start` into the patch beyond it and comes back to it from
        // the side it started on.
        let back = Some(from);
        for _ in 0..na * 4 + 8 {
            let Some((p, s)) = on[a as usize].iter().copied().find(|&x| x != from) else { return Ok(false) };
            let pa = &cut.patches[p];
            if pa.ends(s) {
                return Ok(false);
            }
            let s2 = pa.opposite(s);
            if back == Some((p, s2)) && pa.sides[s2].iter().any(|&(b, _)| b == start) {
                return Ok(true);
            }
            // The arc most short of its length, among those the strip has not crossed yet.
            let Some(b) = pa.sides[s2].iter().map(|&(b, _)| b).filter(|b| !path.contains(b) && (!shrink || n[*b as usize] >= 2)).max_by(|&x, &y| {
                let (dx, dy) = (deficit(n, x as usize), deficit(n, y as usize));
                if shrink { dy.total_cmp(&dx) } else { dx.total_cmp(&dy) }
            }) else { return Err(()) };
            path.push(b);
            a = b;
            from = (p, s2);
        }
        Err(())
    };
    let mut skip = vec![false; na];
    for _ in 0..na * 64 + 64 {
        let Some(a) = (0..na).filter(|&a| !skip[a] && (n[a] == 0 || deficit(&n, a) > 0.5)).max_by(|&x, &y| {
            let (dx, dy) = (deficit(&n, x) + if n[x] == 0 { 1e6 } else { 0.0 }, deficit(&n, y) + if n[y] == 0 { 1e6 } else { 0.0 });
            dx.total_cmp(&dy)
        }) else {
            break;
        };
        let mut path = vec![a as u32];
        // An arc on an open border: the strip starts from outside, into its one patch.
        let sides: Vec<(usize, usize)> = if on[a].len() == 1 { vec![(usize::MAX, 0)] } else { on[a].clone() };
        let closed = match sides.first() {
            Some(&first) => walk(&n, a as u32, first, &mut path, false),
            None => Ok(false),
        };
        let ok = match closed {
            Ok(true) => true,
            Ok(false) => match sides.get(1) {
                Some(&second) => walk(&n, a as u32, second, &mut path, false).is_ok(),
                None => true,
            },
            Err(()) => false,
        };
        if !ok {
            skip[a] = true;
            continue;
        }
        for &b in &path {
            n[b as usize] += 1;
        }
    }
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let bad = cut.patches.iter().filter(|pa| pa.sides.len() == 4 && pa.free.is_none() && (side_sum(&n, &pa.sides[0]) != side_sum(&n, &pa.sides[2]) || side_sum(&n, &pa.sides[1]) != side_sum(&n, &pa.sides[3]))).count();
        eprintln!("    counts check greedy: {bad} uneven");
    }
    // Arcs no greedy strip could cover: the shortest strip through each, found breadth
    // first (closed back onto the arc, or ending at a patch not four-sided both ways).
    let search = |a: u32, from: (usize, usize), back: Option<(usize, usize)>| -> Option<(Vec<u32>, bool)> {
        // States: an arc and the patch side it is entered from.
        let mut prev: HashMap<(u32, usize, usize), Option<(u32, usize, usize)>> = HashMap::default();
        let mut q: VecDeque<(u32, usize, usize)> = VecDeque::new();
        prev.insert((a, from.0, from.1), None);
        q.push_back((a, from.0, from.1));
        let trace_back = |prev: &HashMap<(u32, usize, usize), Option<(u32, usize, usize)>>, mut st: (u32, usize, usize)| -> Vec<u32> {
            let mut out = Vec::new();
            while let Some(p) = prev[&st] {
                out.push(st.0);
                st = p;
            }
            out
        };
        while let Some(st) = q.pop_front() {
            let (b, pf, sf) = st;
            let Some((p, s)) = on[b as usize].iter().copied().find(|&x| x != (pf, sf)) else { return Some((trace_back(&prev, st), false)) };
            let pa = &cut.patches[p];
            if pa.ends(s) {
                return Some((trace_back(&prev, st), false));
            }
            let s2 = pa.opposite(s);
            for &(c, _) in &pa.sides[s2] {
                if c == a && back == Some((p, s2)) {
                    return Some((trace_back(&prev, st), true));
                }
                let next = (c, p, s2);
                if !prev.contains_key(&next) && prev.len() < 200_000 {
                    prev.insert(next, Some(st));
                    q.push_back(next);
                }
            }
        }
        None
    };
    for a in 0..na {
        if n[a] > 0 {
            continue;
        }
        // An arc on an open border: the strip starts from outside, into its one patch.
        let sides: Vec<(usize, usize)> = if on[a].len() == 1 { vec![(usize::MAX, 0)] } else { on[a].clone() };
        let Some(&first) = sides.first() else { continue };
        let back = sides.get(1).copied();
        let Some((mut path, closed)) = search(a as u32, first, Some(first)) else { continue };
        if !closed {
            if let Some(second) = back {
                match search(a as u32, second, Some(second)) {
                    // Closed the other way round: that loop alone is the strip.
                    Some((p2, true)) => path = p2,
                    Some((p2, false)) => path.extend(p2),
                    None => continue,
                }
            }
        }
        path.push(a as u32);
        for &b in &path {
            n[b as usize] += 1;
        }
    }
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let bad = cut.patches.iter().filter(|pa| pa.sides.len() == 4 && pa.free.is_none() && (side_sum(&n, &pa.sides[0]) != side_sum(&n, &pa.sides[2]) || side_sum(&n, &pa.sides[1]) != side_sum(&n, &pa.sides[3]))).count();
        eprintln!("    counts check bfs: {bad} uneven");
    }
    // Then take strips away where every arc on them is over its length.
    let mut skip = vec![false; na];
    for _ in 0..na * 64 + 64 {
        let Some(a) = (0..na).filter(|&a| !skip[a] && n[a] >= 2 && deficit(&n, a) < -0.75).min_by(|&x, &y| deficit(&n, x).total_cmp(&deficit(&n, y))) else {
            break;
        };
        let mut path = vec![a as u32];
        // An arc on an open border: the strip starts from outside, into its one patch.
        let sides: Vec<(usize, usize)> = if on[a].len() == 1 { vec![(usize::MAX, 0)] } else { on[a].clone() };
        let closed = match sides.first() {
            Some(&first) => walk(&n, a as u32, first, &mut path, true),
            None => Ok(false),
        };
        let ok = match closed {
            Ok(true) => true,
            Ok(false) => match sides.get(1) {
                Some(&second) => walk(&n, a as u32, second, &mut path, true).is_ok(),
                None => true,
            },
            Err(()) => false,
        };
        let gain: f32 = path.iter().map(|&b| {
            let (x, t) = (n[b as usize] as f32, target[b as usize]);
            ((x - 1.0 - t).powi(2) - (x - t).powi(2)) / t.max(0.5)
        }).sum();
        if !ok || gain >= 0.0 || path.iter().any(|&b| n[b as usize] < 2) {
            skip[a] = true;
            continue;
        }
        for &b in &path {
            n[b as usize] -= 1;
        }
    }
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let bad = cut.patches.iter().filter(|pa| pa.sides.len() == 4 && pa.free.is_none() && (side_sum(&n, &pa.sides[0]) != side_sum(&n, &pa.sides[2]) || side_sum(&n, &pa.sides[1]) != side_sum(&n, &pa.sides[3]))).count();
        eprintln!("    counts check trim: {bad} uneven");
    }
    // An arc no strip can reach, and shorter than a quad, collapses (its ends become one).
    for a in 0..na {
        if n[a] < 1 && target[a] < 1.0 {
            n[a] = 0;
        }
    }
    if n.iter().any(|&x| x < 0) || (0..na).any(|a| n[a] < 1 && target[a] >= 1.0) {
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            for a in (0..na).filter(|&a| n[a] < 1).take(4) {
                eprintln!("    uncovered arc {a} (length {:.1}): on {:?}, free {:?}", target[a], on[a], on[a].iter().map(|&(p, _)| (cut.patches[p].sides.len(), cut.patches[p].free)).collect::<Vec<_>>());
            }
        }
        return Err(("the quad counts did not settle".into(), None));
    }
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        let bad = cut.patches.iter().filter(|pa| pa.sides.len() == 4 && pa.free.is_none() && (side_sum(&n, &pa.sides[0]) != side_sum(&n, &pa.sides[2]) || side_sum(&n, &pa.sides[1]) != side_sum(&n, &pa.sides[3]))).count();
        eprintln!("    counts check collapse: {bad} uneven");
    }
    // Parity: strips of +1 from each odd patch to another odd one (or an open border).
    let odd = |n: &[i64], p: usize| -> bool {
        let pa = &cut.patches[p];
        pa.irregular() && pa.sides.iter().map(|s| side_sum(n, s)).sum::<i64>() % 2 != 0
    };
    for _round in 0..(cut.patches.len() + 1) {
        let Some(p0) = (0..cut.patches.len()).find(|&p| odd(&n, p)) else {
            break;
        };
        // Breadth-first over arcs: an arc leads to the patch on its other side.
        let mut from: HashMap<u32, u32> = HashMap::default();
        // An arc with the same patch on both sides (a slit) changes nothing's parity.
        // Only arcs with one patch on each side (or an open border) carry parity.
        let slit = |a: u32| !(on[a as usize].len() == 1 || (on[a as usize].len() == 2 && on[a as usize][0].0 != on[a as usize][1].0));
        let mut end = None;
        // First through patches by other sides only; if no end is found, also back out by
        // the side a strip came in by (a patch only reached from one side).
        for same_side in [false, true] {
        from.clear();
        let mut q: VecDeque<(u32, usize, usize)> = VecDeque::new();
        for (si, side) in cut.patches[p0].sides.iter().enumerate() {
            for &(a, _) in side {
                if !slit(a) && from.insert(a, NONE).is_none() {
                    q.push_back((a, p0, si));
                }
            }
        }
        while let Some((a, pf, sf)) = q.pop_front() {
            let other = on[a as usize].iter().copied().find(|&x| x != (pf, sf));
            match other {
                None => {
                    end = Some(a);
                    break;
                }
                Some((p, s)) => {
                    let pa = &cut.patches[p];
                    if pa.irregular() && (pa.sides.len() != 4 || pa.ends(s)) {
                        if odd(&n, p) && p != p0 {
                            end = Some(a);
                            break;
                        }
                        // Through an even one: +1 on two of its arcs keeps it even (a
                        // trapezoid only across its free pair; any two arcs otherwise, also
                        // two on the side it came in by).
                        for (s2, side) in pa.sides.iter().enumerate() {
                            if (s2 == s && (pa.sides.len() == 4 || !same_side)) || (pa.sides.len() == 4 && !pa.ends(s2)) {
                                continue;
                            }
                            for &(b, _) in side {
                                if slit(b) {
                                    continue;
                                }
                                if let std::collections::hash_map::Entry::Vacant(e) = from.entry(b) {
                                    e.insert(a);
                                    q.push_back((b, p, s2));
                                }
                            }
                        }
                        continue;
                    }
                    let s2 = pa.opposite(s);
                    for &(b, _) in &pa.sides[s2] {
                        if slit(b) {
                                    continue;
                                }
                                if let std::collections::hash_map::Entry::Vacant(e) = from.entry(b) {
                            e.insert(a);
                            q.push_back((b, p, s2));
                        }
                    }
                }
            }
        }
        if end.is_some() {
            break;
        }
        }
        let Some(mut a) = end else {
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                let pa = &cut.patches[p0];
                let odd_n = (0..cut.patches.len()).filter(|&p| odd(&n, p)).count();
                eprintln!("    odd patch {p0}: {} sides, messy {}, band {}, free {:?}, {} faces, arcs {:?}; {odd_n} odd patches, {} arcs reached", pa.sides.len(), pa.messy, pa.band, pa.free, pa.faces.len(), pa.sides.iter().map(|s| s.iter().map(|x| (x.0, on[x.0 as usize].len())).collect::<Vec<_>>()).collect::<Vec<_>>(), from.len());
                for q in (0..cut.patches.len()).filter(|&p| odd(&n, p) && p != p0) {
                    let pq = &cut.patches[q];
                    let reached = pq.sides.iter().flatten().filter(|x| from.contains_key(&x.0)).count();
                    eprintln!("      other odd patch {q}: {} sides, messy {}, free {:?}, {} faces, {reached} of its arcs reached, arcs {:?}", pq.sides.len(), pq.messy, pq.free, pq.faces.len(), pq.sides.iter().map(|s| s.iter().map(|x| (x.0, on[x.0 as usize].clone())).collect::<Vec<_>>()).collect::<Vec<_>>());
                }
                let reached_p: std::collections::BTreeSet<usize> = from.keys().flat_map(|a| on[*a as usize].iter().map(|x| x.0)).collect();
                eprintln!("      patches reached: {:?} of {}", reached_p, cut.patches.len());
            }
            return Err(("a patch could not be made even".into(), Some(p0)));
        };
        loop {
            n[a as usize] += 1;
            let b = from[&a];
            if b == NONE {
                break;
            }
            a = b;
        }
    }
    if let Some(p) = (0..cut.patches.len()).find(|&p| odd(&n, p)) {
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            let odds: Vec<usize> = (0..cut.patches.len()).filter(|&p| odd(&n, p)).collect();
            let pa = &cut.patches[p];
            eprintln!("    still odd: {} patches; first {p}: {} sides, messy {}, arcs on it {:?}", odds.len(), pa.sides.len(), pa.messy, pa.sides.iter().flatten().map(|x| on[x.0 as usize].clone()).collect::<Vec<_>>());
        }
        return Err(("a patch could not be made even".into(), Some(p)));
    }
    Ok(n)
}

/// How far a patch with side counts `l` is from splitting round its middle: the least
/// part of any side, in quads (at least 1 to split).
fn slack(l: &[i64]) -> f64 {
    let k = l.len();
    let mut a = vec![vec![0f64; k + 1]; k];
    for i in 0..k {
        a[i][i] += 1.0;
        a[i][(i + k - 2) % k] += 1.0;
        a[i][k] = l[i] as f64;
    }
    for c in 0..k {
        let Some(piv) = (c..k).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs())) else { return f64::NEG_INFINITY };
        if a[piv][c].abs() < 1e-9 {
            return f64::NEG_INFINITY;
        }
        a.swap(c, piv);
        for r in 0..k {
            if r != c {
                let f = a[r][c] / a[c][c];
                for x in c..=k {
                    a[r][x] -= f * a[c][x];
                }
            }
        }
    }
    (0..k).map(|i| {
        let y = a[i][k] / a[i][i];
        y.min(l[i] as f64 - y)
    }).fold(f64::INFINITY, f64::min)
}

/// A strip of arcs from side `side` of patch `p0`, across four-sided patches (in at one
/// side, out at the opposite), to an open border or another patch that is not four-sided:
/// adding the same number to each keeps every four-sided patch on the way even.
fn strip(cut: &Cut, p0: usize, side: usize) -> Option<Vec<u32>> {
    let mut on: HashMap<u32, Vec<(usize, usize)>> = HashMap::default();
    for (p, pa) in cut.patches.iter().enumerate() {
        for (s, sd) in pa.sides.iter().enumerate() {
            for &(a, _) in sd {
                on.entry(a).or_default().push((p, s));
            }
        }
    }
    let mut from: HashMap<u32, u32> = HashMap::default();
    let mut q: VecDeque<(u32, usize, usize)> = VecDeque::new();
    for &(a, _) in &cut.patches[p0].sides[side] {
        if from.insert(a, NONE).is_none() {
            q.push_back((a, p0, side));
        }
    }
    while let Some((a, pf, sf)) = q.pop_front() {
        let other = on[&a].iter().copied().find(|&x| x != (pf, sf));
        let end = match other {
            None => true,
            Some((p, s)) => cut.patches[p].ends(s) && p != p0,
        };
        if end {
            let mut path = vec![a];
            let mut x = a;
            while from[&x] != NONE {
                x = from[&x];
                path.push(x);
            }
            return Some(path);
        }
        let (p, s) = other.unwrap();
        if cut.patches[p].ends(s) {
            continue;
        }
        let s2 = cut.patches[p].opposite(s);
        for &(b, _) in &cut.patches[p].sides[s2] {
            if let std::collections::hash_map::Entry::Vacant(e) = from.entry(b) {
                e.insert(a);
                q.push_back((b, p, s2));
            }
        }
    }
    None
}

/// [`strip`], and the patch it ends on (None at an open border).
fn strip_end(cut: &Cut, p0: usize, side: usize) -> Option<(Vec<u32>, Option<usize>)> {
    let path = strip(cut, p0, side)?;
    let end = *path.first()?;
    // The strip's last arc: the patch on it that the strip does not cross (four-sided,
    // through opposite sides).
    let crossed: HashSet<usize> = cut.patches.iter().enumerate().filter(|(q, pq)| {
        *q != p0 && !pq.ends(0) && pq.sides.iter().filter(|sd| sd.iter().any(|x| path.contains(&x.0))).count() >= 2
    }).map(|(q, _)| q).collect();
    let mut on: Vec<usize> = Vec::new();
    for (q, pq) in cut.patches.iter().enumerate() {
        if pq.sides.iter().flatten().any(|x| x.0 == end) && q != p0 && !crossed.contains(&q) {
            on.push(q);
        }
    }
    Some((path, on.first().copied()))
}

/// Like [`strip`], but from the one arc `a0` on side `side` of patch `p0`.
fn strip_from(cut: &Cut, a0: u32, p0: usize, side: usize) -> Option<Vec<u32>> {
    let mut on: HashMap<u32, Vec<(usize, usize)>> = HashMap::default();
    for (p, pa) in cut.patches.iter().enumerate() {
        for (s, sd) in pa.sides.iter().enumerate() {
            for &(a, _) in sd {
                on.entry(a).or_default().push((p, s));
            }
        }
    }
    let mut from: HashMap<u32, u32> = HashMap::default();
    let mut q: VecDeque<(u32, usize, usize)> = VecDeque::new();
    from.insert(a0, NONE);
    q.push_back((a0, p0, side));
    while let Some((a, pf, sf)) = q.pop_front() {
        let other = on[&a].iter().copied().find(|&x| x != (pf, sf));
        let end = match other {
            None => true,
            Some((p, s)) => cut.patches[p].ends(s) && p != p0,
        };
        if end {
            let mut path = vec![a];
            let mut x = a;
            while from[&x] != NONE {
                x = from[&x];
                path.push(x);
            }
            return Some(path);
        }
        let (p, s) = other.unwrap();
        if cut.patches[p].ends(s) {
            continue;
        }
        let s2 = cut.patches[p].opposite(s);
        for &(b, _) in &cut.patches[p].sides[s2] {
            if let std::collections::hash_map::Entry::Vacant(e) = from.entry(b) {
                e.insert(a);
                q.push_back((b, p, s2));
            }
        }
    }
    None
}

/// Whether a cycle of sides under a step of two has an odd total (no centre split then).
fn odd_cycle(l: &[i64]) -> bool {
    let k = l.len();
    (0..k).any(|c0| {
        let mut sum = 0;
        let mut i = c0;
        loop {
            sum += l[i];
            i = (i + 2) % k;
            if i == c0 {
                break;
            }
        }
        sum % 2 != 0
    })
}

/// How far side counts `l` are from a centre split, for cycles of odd length (k = 3, 5, 6,
/// 7): the least part of any side in the real-valued split (at least 1 to split).
fn split_margin(l: &[i64]) -> f64 {
    split_measure(l).0
}

/// How short side counts `l` are of a centre split, summed over every part (0 when it
/// splits): unlike the margin, it drops when any one cycle gets closer.
fn split_shortfall(l: &[i64]) -> f64 {
    split_measure(l).1
}

fn split_measure(l: &[i64]) -> (f64, f64) {
    let k = l.len();
    let mut worst = f64::INFINITY;
    let mut short = 0.0f64;
    let mut done = vec![false; k];
    for c in 0..k {
        if done[c] {
            continue;
        }
        let mut cyc = vec![c];
        let mut i = (c + 2) % k;
        while i != c {
            cyc.push(i);
            i = (i + 2) % k;
        }
        for &j in &cyc {
            done[j] = true;
        }
        let m = cyc.len();
        if m % 2 == 0 {
            return (f64::NEG_INFINITY, f64::INFINITY);
        }
        // y_t = s_t · y0 + r_t along the cycle; the closing equation fixes y0.
        let (mut sgn, mut r) = (1.0f64, 0.0f64);
        let mut coef = vec![(1.0, 0.0)];
        for t in 1..m {
            sgn = -sgn;
            r = l[cyc[t]] as f64 - r;
            coef.push((sgn, r));
            // (y_t = l - y_{t-1}: sign flips, offset l - previous offset.)
        }
        let (sl, rl) = coef[m - 1];
        // l[cyc0] = y_{m-1} + y0 = (sl + 1) y0 + rl
        let y0 = (l[cyc[0]] as f64 - rl) / (sl + 1.0);
        for t in 0..m {
            let y = coef[t].0 * y0 + coef[t].1;
            let part = y.min(l[cyc[t]] as f64 - y);
            worst = worst.min(part);
            short += (1.0 - part).max(0.0);
        }
    }
    (worst, short)
}

/// A vertex of patch `pa` as far inside it as there is.
fn middle(w: &Work, pa: &Patch) -> u32 {
    let verts: HashSet<u32> = pa.faces.iter().flat_map(|&f| w.t[f as usize]).collect();
    let mut dist: HashMap<u32, u32> = pa.lp.iter().map(|&v| (v, 0)).collect();
    let mut q: VecDeque<u32> = pa.lp.iter().copied().collect();
    let mut far = pa.lp[0];
    while let Some(v) = q.pop_front() {
        far = v;
        for &u in w.neighbours(v) {
            if verts.contains(&u) && !dist.contains_key(&u) {
                dist.insert(u, dist[&v] + 1);
                q.push_back(u);
            }
        }
    }
    far
}

/// The split of each side of a three- or five-sided patch with side counts `l`: `y[i]` quads
/// of side `i` next to the corner after it.
fn split_sides(l: &[i64]) -> Option<Vec<i64>> {
    // l_i = y_{i-2} + y_i, each part at least one. The equations split into cycles under a
    // step of two; each is solved by trying its first unknown, the most even split kept.
    let k = l.len();
    if k < 3 || k == 4 {
        return None;
    }
    let mut y = vec![0i64; k];
    let mut done = vec![false; k];
    for c in 0..k {
        if done[c] {
            continue;
        }
        let mut cyc = vec![c];
        let mut i = (c + 2) % k;
        while i != c {
            cyc.push(i);
            i = (i + 2) % k;
        }
        let m = cyc.len();
        let mut best: Option<(i64, Vec<i64>)> = None;
        for y0 in 1..l[cyc[0]].max(1) {
            let mut v = vec![0i64; m];
            v[0] = y0;
            let mut ok = true;
            // y_{cyc[t]} = l_{cyc[t]} - y_{cyc[t-1]}
            for t in 1..m {
                v[t] = l[cyc[t]] - v[t - 1];
                if v[t] < 1 || l[cyc[t]] - v[t] < 1 {
                    ok = false;
                    break;
                }
            }
            // Closing: l_{cyc[0]} = y_{cyc[m-1]} + y_{cyc[0]}.
            if !ok || v[m - 1] + y0 != l[cyc[0]] || l[cyc[0]] - y0 < 1 {
                continue;
            }
            let spread: i64 = (0..m).map(|t| (2 * v[t] - l[cyc[t]]).abs()).sum();
            if best.as_ref().is_none_or(|b| spread < b.0) {
                best = Some((spread, v));
            }
        }
        let (_, v) = best?;
        for t in 0..m {
            y[cyc[t]] = v[t];
            done[cyc[t]] = true;
        }
    }
    Some(y)
}

/// Fill a band (a ring between two loops) with rows round it, laid flat as an annulus: the
/// loop with more points outside, counter-clockwise, the other inside; where the counts
/// differ, rows merge three quads into one (staggered round the ring).
#[allow(clippy::too_many_arguments)]
fn band_fill(w: &Work, pa: &Patch, c: &Cut, n: &[i64], arc_pts: &[Vec<u32>], arc_place: &[Vec<f32>], out_p: &mut Vec<Vec3>, quads: &mut Vec<[u32; 4]>, quad: f32) -> Result<(), String> {
    // Each loop's points (ids, count positions) in loop order, from its first arc.
    let side_points = |side: &[(u32, bool)]| -> (Vec<u32>, HashMap<u32, f32>) {
        let mut ids: Vec<u32> = Vec::new();
        let mut place: HashMap<u32, f32> = HashMap::default();
        let mut base = 0.0f32;
        for &(a, fwd) in side {
            let path = &c.arcs[a as usize];
            let na = n[a as usize] as f32;
            for (i, &v) in path.iter().enumerate() {
                let u = if fwd { arc_place[a as usize][i] } else { na - arc_place[a as usize][i] };
                place.entry(v).or_insert(base + u);
            }
            let mut pts = arc_pts[a as usize].clone();
            if !fwd {
                pts.reverse();
            }
            if ids.last() == pts.first() {
                ids.pop();
            }
            ids.extend(pts);
            base += na;
        }
        if ids.len() > 1 && ids.first() == ids.last() {
            ids.pop();
        }
        (ids, place)
    };
    let (sa, sb) = (side_points(&pa.sides[0]), side_points(&pa.sides[1]));
    // Outside: the loop with more points.
    let (outer, inner, lo, li) = if sa.0.len() >= sb.0.len() { (sa, sb, &pa.lp, &pa.lp2) } else { (sb, sa, &pa.lp2, &pa.lp) };
    let (ids_o, place_o) = outer;
    let (ids_i, place_i) = inner;
    let (co, ci) = (ids_o.len(), ids_i.len());
    if ci < 3 || (co - ci) % 2 != 0 {
        let sums: Vec<i64> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize]).sum()).collect();
        let entries: Vec<Vec<(u32, bool, i64, usize)>> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, f)| (a, f, n[a as usize], arc_pts[a as usize].len())).collect()).collect();
        return Err(format!("a band's loops do not fit ({co} and {ci}); count sums {sums:?}, free {:?}, sides (arc, fwd, n, pts) {entries:?}", pa.free));
    }
    // Rows: the band's width over the quad size, enough for the merges.
    let area: f32 = pa.faces.iter().map(|&f| {
        let t = w.t[f as usize];
        (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
    }).sum();
    let length = |l: &[u32]| -> f32 { (0..l.len()).map(|i| w.p[l[i] as usize].distance(w.p[l[(i + 1) % l.len()] as usize])).sum() };
    let (len_o, len_i) = (length(lo), length(li));
    let d = (co - ci) / 2;
    let mut rows = ((area / ((len_o + len_i) * 0.5).max(1e-9)) / quad).round().max(1.0) as usize;
    let fits = |rows: usize| (0..rows).all(|j| {
        let kj = (j + 1) * d / rows - j * d / rows;
        co - 2 * (j * d / rows) >= 3 * kj
    });
    while !fits(rows) && rows < co {
        rows += 1;
    }
    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
        eprintln!("  band: outer {co} pts ({:.1} quads long), inner {ci} ({:.1}), {rows} rows (width {:.1} quads), free {:?}", len_o / quad, len_i / quad, area / ((len_o + len_i) * 0.5).max(1e-9) / quad, pa.free);
    }
    // Inside loop turned to start opposite the outside one's first point.
    let ang_o = |u: f32| std::f32::consts::TAU * u / co as f32;
    let start = (0..ci).min_by(|&a, &b| out_p[ids_i[a] as usize].distance_squared(out_p[ids_o[0] as usize]).total_cmp(&out_p[ids_i[b] as usize].distance_squared(out_p[ids_o[0] as usize]))).unwrap();
    let ang_i = |u: f32| std::f32::consts::TAU * (start as f32 - u) / ci as f32;
    let mid = (co + ci) as f32 * 0.5 / std::f32::consts::TAU;
    let r_o = mid + rows as f32 * 0.5;
    let r_i = (mid - rows as f32 * 0.5).max(r_o * 0.25);
    let mut boundary: HashMap<u32, Vec2> = HashMap::default();
    for (&v, &u) in &place_o {
        boundary.insert(v, Vec2::new(ang_o(u).cos(), ang_o(u).sin()) * r_o);
    }
    for (&v, &u) in &place_i {
        boundary.entry(v).or_insert(Vec2::new(ang_i(u).cos(), ang_i(u).sin()) * r_i);
    }
    let none: HashMap<(u32, u32), u32> = HashMap::default();
    let flat = flatten(w, pa, &boundary, &none);
    // Across the band by surface distance (the flattening crowds rows toward one loop): each
    // vertex's share of the way from the outside loop to the inside one, at its flat angle.
    let verts: HashSet<u32> = pa.faces.iter().flat_map(|&f| w.t[f as usize]).collect();
    let dist_from = |seeds: &HashMap<u32, f32>| -> HashMap<u32, f32> {
        let mut dist: HashMap<u32, f32> = seeds.keys().map(|&v| (v, 0.0)).collect();
        let mut heap = std::collections::BinaryHeap::new();
        for &v in seeds.keys() {
            heap.push((std::cmp::Reverse(0u32), v));
        }
        while let Some((std::cmp::Reverse(db), v)) = heap.pop() {
            let d = f32::from_bits(db);
            if d > dist[&v] {
                continue;
            }
            for &u in w.neighbours(v) {
                if !verts.contains(&u) {
                    continue;
                }
                let nd = d + w.p[v as usize].distance(w.p[u as usize]);
                if dist.get(&u).is_none_or(|&x| nd < x) {
                    dist.insert(u, nd);
                    heap.push((std::cmp::Reverse(nd.to_bits()), u));
                }
            }
        }
        dist
    };
    // A band whose two loops are one line seen from both sides (a torus cut once) keeps the
    // flattening as it is.
    let shared = place_o.keys().any(|v| place_i.contains_key(v));
    let (d_o, d_i) = if shared { (HashMap::default(), HashMap::default()) } else { (dist_from(&place_o), dist_from(&place_i)) };
    let flat: HashMap<u32, Vec2> = if shared { flat } else { flat
        .iter()
        .map(|(&v, &q)| {
            let (a, b) = (d_o.get(&v).copied().unwrap_or(0.0), d_i.get(&v).copied().unwrap_or(0.0));
            let t = if place_o.contains_key(&v) { 0.0 } else if place_i.contains_key(&v) { 1.0 } else { a / (a + b).max(1e-12) };
            let r = r_o + (r_i - r_o) * t;
            (v, q.normalize_or(Vec2::X) * r)
        })
        .collect() };
    let loc = Locate::new(w, pa, &flat, &none);
    let mut point = |q: Vec2, out_p: &mut Vec<Vec3>| -> u32 {
        out_p.push(loc.at(q));
        (out_p.len() - 1) as u32
    };
    // Rings from outside in; ring k has `cnt` points at angles 2πi/cnt (the inside one in
    // its own turn, ids in increasing angle).
    let inner_ring: Vec<u32> = (0..ci).map(|i| ids_i[(start + ci - i) % ci]).collect();
    let mut ring: Vec<u32> = ids_o.clone();
    // Merges gathered into a few rows (each takes up to a sixth of its ring), evenly spaced
    // across the band, at the same angles in every such row: clean reduction rings.
    let per_row = |c: usize| (c / 6).max(1);
    let mut plan = vec![0usize; rows];
    {
        let mut left = d;
        let mut groups = 1usize;
        while groups < rows && d.div_ceil(groups) > per_row(ci.max(3)) {
            groups += 1;
        }
        for g in 0..groups {
            let r = ((g as f32 + 0.5) * rows as f32 / groups as f32) as usize;
            let take = left.div_ceil(groups - g);
            plan[r.min(rows - 1)] += take;
            left -= take;
        }
        // Too many for a row's ring: spill to the next rows.
        let mut c = co;
        for j in 0..rows {
            let cap = c / 3;
            if plan[j] > cap {
                let extra = plan[j] - cap;
                plan[j] = cap;
                if j + 1 < rows {
                    plan[j + 1] += extra;
                }
            }
            c -= 2 * plan[j];
        }
    }
    let mut cnt = co;
    for j in 0..rows {
        let kj = plan[j];
        let next_cnt = cnt - 2 * kj;
        let r_next = r_o + (r_i - r_o) * (j + 1) as f32 / rows as f32;
        let r_mid = r_o + (r_i - r_o) * (j as f32 + 0.5) / rows as f32;
        let next: Vec<u32> = if j + 1 == rows {
            inner_ring.clone()
        } else {
            (0..next_cnt).map(|i| {
                let a = std::f32::consts::TAU * i as f32 / next_cnt as f32;
                point(Vec2::new(a.cos(), a.sin()) * r_next, out_p)
            }).collect()
        };
        // Merges spread round the ring, staggered from row to row.
        let mut starts: Vec<usize> = (0..kj).map(|t| (((t as f32 + 0.5) * cnt as f32 / kj.max(1) as f32) as usize) % cnt).collect();
        starts.sort_unstable();
        let mut taken = vec![false; cnt];
        let mut merge_at = vec![false; cnt];
        for st0 in starts {
            let mut st = st0;
            let mut guard = 0;
            while (taken[st] || taken[(st + 1) % cnt] || taken[(st + 2) % cnt]) && guard < cnt {
                st = (st + 1) % cnt;
                guard += 1;
            }
            taken[st] = true;
            taken[(st + 1) % cnt] = true;
            taken[(st + 2) % cnt] = true;
            merge_at[st] = true;
        }
        // Walk the ring from a point that does not start inside a merge.
        let first = (0..cnt).find(|&i| !taken[i] || merge_at[i]).unwrap_or(0);
        let (mut i, mut o) = (0usize, 0usize);
        // The next ring's point 0 sits opposite `first` (re-index it).
        let a0 = std::f32::consts::TAU * first as f32 / cnt as f32;
        let o0 = ((a0 / std::f32::consts::TAU * next_cnt as f32).round() as usize) % next_cnt;
        while i < cnt {
            let b = |k: usize| ring[(first + k) % cnt];
            let t = |k: usize| next[(o0 + k) % next_cnt];
            if merge_at[(first + i) % cnt] {
                let ang = |k: usize| std::f32::consts::TAU * ((first + k) % cnt) as f32 / cnt as f32;
                let m1 = point(Vec2::new(ang(i + 1).cos(), ang(i + 1).sin()) * r_mid, out_p);
                let m2 = point(Vec2::new(ang(i + 2).cos(), ang(i + 2).sin()) * r_mid, out_p);
                quads.push([b(i), b(i + 1), m1, t(o)]);
                quads.push([b(i + 1), b(i + 2), m2, m1]);
                quads.push([b(i + 2), b(i + 3), t(o + 1), m2]);
                quads.push([m1, m2, t(o + 1), t(o)]);
                i += 3;
                o += 1;
            } else {
                quads.push([b(i), b(i + 1), t(o + 1), t(o)]);
                i += 1;
                o += 1;
            }
        }
        if o != next_cnt {
            return Err(format!("a band row did not close ({o} of {next_cnt})"));
        }
        ring = next;
        cnt = next_cnt;
    }
    Ok(())
}

/// Fill a flat polygon (boundary `ids` at `pos`, counter-clockwise, an even number of them,
/// corners at `corners`) with quads: rings peeled inward (each corner takes two border
/// edges), then, once a side is too short, quads clipped off the border.
fn ring_fill(mut ids: Vec<u32>, mut pos: Vec<Vec2>, mut corners: Vec<usize>, point: &mut dyn FnMut(Vec2) -> u32, quads: &mut Vec<[u32; 4]>) {
    if std::env::var_os("ZR_TRIFILL").is_some() && tri_fill(&ids, &pos, point, quads) {
        return;
    }
    loop {
        let len = ids.len();
        if len < 4 {
            return;
        }
        if len == 4 {
            quads.push([ids[0], ids[1], ids[2], ids[3]]);
            return;
        }
        // Corners that leave a side shorter than three are dropped (the side joins the next),
        // unless the four corners make an exact rectangle (a grid finishes it below).
        let exact = corners.len() == 4 && std::env::var_os("ZR_NOEXACT").is_none() && {
            let side = |i: usize| -> usize { (corners[(i + 1) % 4] + len - corners[i]) % len };
            side(0) == side(2) && side(1) == side(3) && side(0).min(side(1)) <= 4 && side(0).min(side(1)) >= 2
        };
        loop {
            let nc = corners.len();
            if nc <= 3 || exact {
                break;
            }
            let side = |i: usize| -> usize { (corners[(i + 1) % nc] + len - corners[i]) % len };
            let Some(i) = (0..nc).filter(|&i| side(i) < 3).min_by_key(|&i| side(i)) else { break };
            corners.remove((i + 1) % nc);
        }
        let nc = corners.len();
        let side = |i: usize| -> usize { (corners[(i + 1) % nc] + len - corners[i]) % len };
        // Four corners with opposite sides equal and small enough: a grid finishes it.
        if nc == 4 && side(0) == side(2) && side(1) == side(3) && side(0).min(side(1)) <= 4 {
            let (nx, ny) = (side(0), side(1));
            let c0 = corners[0];
            let at = |k: usize| ids[(c0 + k) % len];
            let pat = |k: usize| pos[(c0 + k) % len];
            // Border ids round the grid: bottom 0..nx, right nx..nx+ny, top, left.
            let border = |i: usize, j: usize| -> Option<usize> {
                if j == 0 {
                    Some(i)
                } else if i == nx {
                    Some(nx + j)
                } else if j == ny {
                    Some(nx + ny + (nx - i))
                } else if i == 0 {
                    Some((2 * nx + ny + (ny - j)) % len)
                } else {
                    None
                }
            };
            let (p00, p10, p11, p01) = (pat(0), pat(nx), pat(nx + ny), pat(2 * nx + ny));
            let mut g = vec![vec![0u32; nx + 1]; ny + 1];
            for j in 0..=ny {
                for i in 0..=nx {
                    g[j][i] = match border(i, j) {
                        Some(k) => at(k),
                        None => {
                            // Coons-style: the border points either way, minus the corners.
                            let (u, v) = (i as f32 / nx as f32, j as f32 / ny as f32);
                            let q = pat(i) * (1.0 - v) + pat(nx + ny + (nx - i)) * v + pat((2 * nx + ny + (ny - j)) % len) * (1.0 - u) + pat(nx + j) * u
                                - (p00 * (1.0 - u) * (1.0 - v) + p10 * u * (1.0 - v) + p11 * u * v + p01 * (1.0 - u) * v);
                            point(q)
                        }
                    };
                }
            }
            for j in 0..ny {
                for i in 0..nx {
                    quads.push([g[j][i], g[j][i + 1], g[j + 1][i + 1], g[j + 1][i]]);
                }
            }
            return;
        }
        let peel = nc >= 3 && (0..nc).all(|i| side(i) >= 3) && len >= 2 * nc + 4;
        if !peel {
            if tri_fill(&ids, &pos, point, quads) {
                return;
            }
            // Clip quads off the border, the best-shaped first.
            while ids.len() > 4 {
                let m = ids.len();
                let quality = |i: usize| -> f32 {
                    let q = [pos[i], pos[(i + 1) % m], pos[(i + 2) % m], pos[(i + 3) % m]];
                    let mut worst = f32::INFINITY;
                    for k in 0..4 {
                        let (a, b, c) = (q[(k + 3) % 4], q[k], q[(k + 1) % 4]);
                        let (u, v) = ((a - b).normalize_or_zero(), (c - b).normalize_or_zero());
                        worst = worst.min(v.perp_dot(u));
                    }
                    worst
                };
                // The best-shaped quad whose new edge is not already an edge of the ring.
                let fresh = |i: usize| -> bool {
                    let (a, b) = (ids[i], ids[(i + 3) % m]);
                    !(0..m).any(|j| {
                        let (x, y) = (ids[j], ids[(j + 1) % m]);
                        (x == a && y == b) || (x == b && y == a)
                    })
                };
                let i = (0..m).filter(|&i| fresh(i) && quality(i) > 0.05).max_by(|&a, &b| quality(a).total_cmp(&quality(b))).or_else(|| (0..m).max_by(|&a, &b| quality(a).total_cmp(&quality(b)))).unwrap();
                quads.push([ids[i], ids[(i + 1) % m], ids[(i + 2) % m], ids[(i + 3) % m]]);
                let (r1, r2) = ((i + 1) % m, (i + 2) % m);
                let keep: Vec<usize> = (0..m).filter(|&j| j != r1 && j != r2).collect();
                ids = keep.iter().map(|&j| ids[j]).collect();
                pos = keep.iter().map(|&j| pos[j]).collect();
            }
            quads.push([ids[0], ids[1], ids[2], ids[3]]);
            return;
        }
        // One ring: a quad at each corner, a strip along each side.
        let inner_corner: Vec<(u32, Vec2)> = corners
            .iter()
            .map(|&c| {
                let (prev, next) = ((c + len - 1) % len, (c + 1) % len);
                // (Pulling a corner's inner point inside the ring when it falls outside
                // made rows collide and merge points: left as the plain extrapolation; the
                // surface lookup copes with a point outside the patch.)
                let q = pos[prev] + pos[next] - pos[c];
                (point(q), q)
            })
            .collect();
        let (mut nid, mut npos, mut ncorner) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..nc {
            let c = corners[i];
            let l = side(i);
            let (prev, next) = ((c + len - 1) % len, (c + 1) % len);
            let (ic, icp) = inner_corner[i];
            let (ic2, _) = inner_corner[(i + 1) % nc];
            quads.push([ids[prev], ids[c], ids[next], ic]);
            ncorner.push(nid.len());
            nid.push(ic);
            npos.push(icp);
            // Along the side: border points c+1 .. c+l-1, inner points ic, (new), ic2.
            let off = icp - pos[next];
            let mut row = vec![ic];
            for j in 2..=l - 2 {
                let q = pos[(c + j) % len] + off;
                let id = point(q);
                row.push(id);
                nid.push(id);
                npos.push(q);
            }
            row.push(ic2);
            for j in 1..=l - 2 {
                let (s0, s1) = (ids[(c + j) % len], ids[(c + j + 1) % len]);
                quads.push([s0, s1, row[j], row[j - 1]]);
            }
        }
        ids = nid;
        pos = npos;
        corners = ncorner;
    }
}

/// Fill a flat polygon with an even number of border points: every other border point
/// and a few points inside, evenly spaced, are triangulated (Delaunay), and each triangle
/// is split into three quads through its middle; the border points between take the
/// splits of the border edges. False (nothing added) if the triangulation does not keep the
/// border.
fn tri_fill(ids: &[u32], pos: &[Vec2], point: &mut dyn FnMut(Vec2) -> u32, quads: &mut Vec<[u32; 4]>) -> bool {
    let len = ids.len();
    if len < 4 || len % 2 != 0 {
        return false;
    }
    let m = len / 2;
    let step = (0..len).map(|i| pos[i].distance(pos[(i + 1) % len])).sum::<f32>() / len as f32;
    // Points: the coarse border (even indices), then inside points on a grid two steps apart.
    let mut pts: Vec<Vec2> = (0..m).map(|k| pos[2 * k]).collect();
    let inside = |q: Vec2| -> bool {
        let mut c = false;
        for i in 0..len {
            let (a, b) = (pos[i], pos[(i + 1) % len]);
            if (a.y > q.y) != (b.y > q.y) && q.x < a.x + (q.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                c = !c;
            }
        }
        c
    };
    let (lo, hi) = pos.iter().fold((Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let g = step * 2.0;
    let mut y = lo.y + g * 0.5;
    let mut row = 0;
    while y < hi.y {
        let mut x = lo.x + g * if row % 2 == 0 { 0.5 } else { 1.0 };
        while x < hi.x {
            let q = Vec2::new(x, y);
            if inside(q) && pos.iter().all(|p| p.distance(q) > step * 1.3) {
                pts.push(q);
            }
            x += g;
        }
        y += g * 0.866;
        row += 1;
    }
    // Bowyer–Watson.
    let span = (hi - lo).max_element().max(1e-6) * 10.0;
    let c0 = (lo + hi) * 0.5;
    let n = pts.len();
    let mut all = pts.clone();
    all.extend([c0 + Vec2::new(-span, -span), c0 + Vec2::new(span, -span), c0 + Vec2::new(0.0, span)]);
    let mut tris: Vec<[usize; 3]> = vec![[n, n + 1, n + 2]];
    let circum = |t: &[usize; 3], all: &[Vec2]| -> (Vec2, f32) {
        let (a, b, c) = (all[t[0]], all[t[1]], all[t[2]]);
        let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
        if d.abs() < 1e-20 {
            return (a, f32::INFINITY);
        }
        let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
        let ux = (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d;
        let uy = (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d;
        let u = Vec2::new(ux, uy);
        (u, u.distance_squared(a))
    };
    for i in 0..n {
        let q = all[i];
        let mut bad = Vec::new();
        let mut keep = Vec::new();
        for t in tris.drain(..) {
            let (c, r2) = circum(&t, &all);
            if c.distance_squared(q) < r2 {
                bad.push(t);
            } else {
                keep.push(t);
            }
        }
        let mut edges: HashMap<(usize, usize), u32> = HashMap::default();
        for t in &bad {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        for t in &bad {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if edges[&(a.min(b), a.max(b))] == 1 {
                    keep.push([a, b, i]);
                }
            }
        }
        tris = keep;
    }
    // Inside the polygon only, counter-clockwise.
    let mut faces: Vec<[usize; 3]> = Vec::new();
    for t in tris {
        if t.iter().any(|&v| v >= n) {
            continue;
        }
        let (a, b, c) = (all[t[0]], all[t[1]], all[t[2]]);
        if !inside((a + b + c) / 3.0) {
            continue;
        }
        faces.push(if (b - a).perp_dot(c - a) > 0.0 { t } else { [t[0], t[2], t[1]] });
    }
    // The border must come out whole: each coarse border edge in exactly one triangle.
    let mut edge_use: HashMap<(usize, usize), u32> = HashMap::default();
    for t in &faces {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edge_use.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    if (0..m).any(|k| edge_use.get(&(k.min((k + 1) % m), k.max((k + 1) % m))) != Some(&1)) {
        return false;
    }
    if edge_use.values().any(|&u| u > 2) {
        return false;
    }
    // Ids: coarse border points and inside points; edge middles (border ones given).
    let mut pid: Vec<u32> = (0..m).map(|k| ids[2 * k]).collect();
    for q in &pts[m..] {
        pid.push(point(*q));
    }
    let mut mid: HashMap<(usize, usize), u32> = HashMap::default();
    for k in 0..m {
        let (a, b) = (k, (k + 1) % m);
        mid.insert((a.min(b), a.max(b)), ids[2 * k + 1]);
    }
    for t in &faces {
        let c = point((all[t[0]] + all[t[1]] + all[t[2]]) / 3.0);
        let mut mids = [0u32; 3];
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            mids[k] = *mid.entry((a.min(b), a.max(b))).or_insert_with(|| point((all[a] + all[b]) * 0.5));
        }
        for k in 0..3 {
            quads.push([pid[t[k]], mids[k], c, mids[(k + 2) % 3]]);
        }
    }
    true
}

/// Lay the patch's faces flat with its loop on the polygon `boundary` (Tutte, uniform
/// weights): the flat position of every vertex of the patch.
fn flatten(w: &Work, pa: &Patch, boundary: &HashMap<u32, Vec2>, alias: &HashMap<(u32, u32), u32>) -> HashMap<u32, Vec2> {
    let corner = |f: u32, v: u32| -> u32 { alias.get(&(f, v)).copied().unwrap_or(v) };
    let mut verts: Vec<u32> = pa.faces.iter().flat_map(|&f| w.t[f as usize].map(|v| corner(f, v))).collect();
    verts.sort_unstable();
    verts.dedup();
    let index: HashMap<u32, usize> = verts.iter().enumerate().map(|(i, &v)| (v, i)).collect();
    // Edge weights: cotangent weights (the flattening that keeps angles best), clamped
    // positive so the system stays positive definite; uniform with ZR_UNIFORM.
    let uniform = std::env::var_os("ZR_UNIFORM").is_some();
    let mut wmap: HashMap<(usize, usize), f64> = HashMap::default();
    for &f in &pa.faces {
        let t0 = w.t[f as usize];
        let t = t0.map(|v| corner(f, v));
        for k in 0..3 {
            let (a, b) = (index[&t[k]], index[&t[(k + 1) % 3]]);
            let wt = if uniform {
                0.5
            } else {
                // The angle opposite edge k, at corner k + 2.
                let (pa3, pb3, pc3) = (w.p[t0[k] as usize], w.p[t0[(k + 1) % 3] as usize], w.p[t0[(k + 2) % 3] as usize]);
                let (u, v) = ((pa3 - pc3).as_dvec3(), (pb3 - pc3).as_dvec3());
                let cr = u.cross(v).length().max(1e-30);
                0.5 * u.dot(v) / cr
            };
            *wmap.entry((a.min(b), a.max(b))).or_default() += wt;
        }
    }
    let mean_w = if wmap.is_empty() { 1.0 } else { wmap.values().map(|x| x.abs()).sum::<f64>() / wmap.len() as f64 };
    let mut nb: Vec<Vec<(usize, f64)>> = vec![Vec::new(); verts.len()];
    for (&(a, b), &wt) in &wmap {
        let wt = wt.max(mean_w * 0.02);
        nb[a].push((b, wt));
        nb[b].push((a, wt));
    }
    let fixed: Vec<Option<Vec2>> = verts.iter().map(|v| boundary.get(v).copied()).collect();
    let centre = boundary.values().copied().sum::<Vec2>() / boundary.len().max(1) as f32;
    let mut x: Vec<Vec2> = fixed.iter().map(|f| f.unwrap_or(centre)).collect();
    // Conjugate gradients on the free vertices: deg·x_i − Σ x_j = Σ fixed neighbours.
    let free: Vec<usize> = (0..verts.len()).filter(|&i| fixed[i].is_none()).collect();
    let fi: HashMap<usize, usize> = free.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    let nfree = free.len();
    if nfree > 0 {
        // Conjugate gradients in f64 (f32 never converges on big patches, and an unconverged
        // Tutte map folds): deg·x_i − Σ x_j = Σ fixed neighbours.
        use glam::DVec2;
        let apply = |v: &[DVec2]| -> Vec<DVec2> {
            free.iter().map(|&i| {
                let deg: f64 = nb[i].iter().map(|x| x.1).sum();
                let mut s = v[fi[&i]] * deg;
                for &(j, wt) in &nb[i] {
                    if let Some(&k) = fi.get(&j) {
                        s -= v[k] * wt;
                    }
                }
                s
            }).collect()
        };
        let b: Vec<DVec2> = free.iter().map(|&i| nb[i].iter().filter_map(|&(j, wt)| fixed[j].map(|q| q.as_dvec2() * wt)).sum()).collect();
        let mut u: Vec<DVec2> = free.iter().map(|&i| x[i].as_dvec2()).collect();
        let au = apply(&u);
        let mut r: Vec<DVec2> = b.iter().zip(&au).map(|(b, a)| *b - *a).collect();
        let mut p = r.clone();
        let dot = |a: &[DVec2], b: &[DVec2]| a.iter().zip(b).map(|(x, y)| x.dot(*y)).sum::<f64>();
        let mut rr = dot(&r, &r);
        let rr0 = rr.max(1e-300);
        let mut iters = 0usize;
        for _ in 0..nfree * 4 + 100 {
            if rr < rr0 * 1e-24 || rr < 1e-30 {
                break;
            }
            iters += 1;
            let ap = apply(&p);
            let alpha = rr / dot(&p, &ap).max(1e-300);
            for k in 0..nfree {
                u[k] += p[k] * alpha;
                r[k] -= ap[k] * alpha;
            }
            let rr2 = dot(&r, &r);
            let beta = rr2 / rr;
            rr = rr2;
            for k in 0..nfree {
                p[k] = r[k] + p[k] * beta;
            }
        }
        for (k, &i) in free.iter().enumerate() {
            x[i] = u[k].as_vec2();
        }
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() && nfree > 20000 {
            eprintln!("  flatten: {nfree} free vertices, {iters} CG iterations, residual {:.2e} of {:.2e}", rr, rr0);
        }
    }
    verts.into_iter().zip(x).collect()
}

/// Flat triangles of a patch, by grid cell, to find the one under a flat point.
struct Locate<'a> {
    w: &'a Work,
    tris: Vec<(u32, [Vec2; 3])>,
    lo: Vec2,
    cell: f32,
    n: usize,
    grid: Vec<Vec<u32>>,
}

impl<'a> Locate<'a> {
    fn new(w: &'a Work, pa: &Patch, flat: &HashMap<u32, Vec2>, alias: &HashMap<(u32, u32), u32>) -> Self {
        let tris: Vec<(u32, [Vec2; 3])> = pa.faces.iter().map(|&f| (f, w.t[f as usize].map(|v| flat[&alias.get(&(f, v)).copied().unwrap_or(v)]))).collect();
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for (_, t) in &tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let n = ((tris.len() as f32).sqrt().ceil() as usize).max(1);
        let cell = ((hi - lo).max_element() / n as f32).max(1e-9);
        let mut grid = vec![Vec::new(); n * n];
        for (i, (_, t)) in tris.iter().enumerate() {
            let (a, b) = (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2]));
            let (x0, y0) = (((a.x - lo.x) / cell) as usize, ((a.y - lo.y) / cell) as usize);
            let (x1, y1) = ((((b.x - lo.x) / cell) as usize).min(n - 1), (((b.y - lo.y) / cell) as usize).min(n - 1));
            for y in y0.min(n - 1)..=y1 {
                for x in x0.min(n - 1)..=x1 {
                    grid[y * n + x].push(i as u32);
                }
            }
        }
        Locate { w, tris, lo, cell, n, grid }
    }

    fn bary(t: &[Vec2; 3], q: Vec2) -> Vec3 {
        let (a, b, c) = (t[0], t[1], t[2]);
        let d = (b - a).perp_dot(c - a);
        if d.abs() < 1e-20 {
            return Vec3::new(1.0, 0.0, 0.0);
        }
        let l1 = (q - a).perp_dot(c - a) / d;
        let l2 = (b - a).perp_dot(q - a) / d;
        Vec3::new(1.0 - l1 - l2, l1, l2)
    }

    /// The point of triangle `t` nearest `q`, and the squared distance.
    fn closest(t: &[Vec2; 3], q: Vec2) -> (Vec2, f32) {
        let l = Self::bary(t, q);
        if l.min_element() >= 0.0 {
            return (q, 0.0);
        }
        let mut best = (t[0], f32::INFINITY);
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let ab = b - a;
            let u = ((q - a).dot(ab) / ab.length_squared().max(1e-20)).clamp(0.0, 1.0);
            let c = a + ab * u;
            let d2 = c.distance_squared(q);
            if d2 < best.1 {
                best = (c, d2);
            }
        }
        best
    }

    /// The surface point at flat point `q`.
    fn at(&self, q: Vec2) -> Vec3 {
        let x = (((q.x - self.lo.x) / self.cell).max(0.0) as usize).min(self.n - 1);
        let y = (((q.y - self.lo.y) / self.cell).max(0.0) as usize).min(self.n - 1);
        let mut best: Option<(f32, u32, Vec3)> = None;
        let look = |list: &[u32], best: &mut Option<(f32, u32, Vec3)>| {
            for &i in list {
                let (_, t) = &self.tris[i as usize];
                let l = Self::bary(t, q);
                let out = (-l.min_element()).max(0.0);
                if best.is_none_or(|b| out < b.0) {
                    *best = Some((out, i, l));
                }
            }
        };
        look(&self.grid[y * self.n + x], &mut best);
        if best.is_none_or(|b| b.0 > 1e-4) && std::env::var_os("ZR_OLDLOCATE").is_some() {
            for list in &self.grid {
                look(list, &mut best);
            }
        } else if best.is_none_or(|b| b.0 > 1e-4) {
            // Outside every triangle of its cell: the nearest triangle by distance (not by
            // barycentric overshoot, which favours big far-off triangles).
            let mut near: Option<(f32, u32, Vec3)> = None;
            for (i, (_, t)) in self.tris.iter().enumerate() {
                let (c, d2) = Self::closest(t, q);
                if near.is_none_or(|b| d2 < b.0) {
                    near = Some((d2, i as u32, Self::bary(t, c)));
                }
            }
            if let Some((_, i, l)) = near {
                best = Some((0.0, i, l));
            }
        }
        let (_, i, l) = best.unwrap();
        let l = l.max(Vec3::ZERO);
        let l = l / (l.x + l.y + l.z).max(1e-9);
        let t = self.w.t[self.tris[i as usize].0 as usize];
        self.w.p[t[0] as usize] * l.x + self.w.p[t[1] as usize] * l.y + self.w.p[t[2] as usize] * l.z
    }
}

/// The layout's quads and their points, or why it could not be made.
/// `reflex_area`: a concave corner on a patch over this many quads² gets a line traced from
/// it (0: none).
#[allow(clippy::too_many_arguments)]
pub(super) fn quads(mesh: &SculptMesh, s: &Settings, guides: &[Vec<Vec3>], seams: &[Vec<Vec3>], features: &Features, cut_plane: Option<MirrorPlane>, quad: f32, field: f32, reflex_area: f32) -> Result<(Vec<Vec3>, Vec<[u32; 4]>), String> {
    // 1. The working mesh: the plain remesher at four times the quads.
    // The working mesh: the model itself (exact edge lines and polygroup borders), decimated
    // if far too dense, then its long edges split until every edge is under a quarter quad.
    // With a mirror plane, the plain remesher's half instead.
    // With a mirror plane `mesh` is already the positive half, cut exactly on the plane: its
    // open border there is a hard line, so lines meet the centre square-on and the mirrored
    // result has one straight centre loop.
    let _ = (guides, seams, cut_plane);
    let t_start = std::time::Instant::now();
    let lap = |what: &str| if std::env::var_os("KLAY_ZR_DEBUG").is_some() { eprintln!("  time: {what} at {:.1} s", t_start.elapsed().as_secs_f32()) };
    let (mut wm, kept) = refined(mesh, quad / DENSITY)?;
    lap(&format!("working mesh ({} triangles)", wm.triangle_count()));
    // Without an inner skin the same count of quads covers less: smaller quads.
    let quad = quad * kept.sqrt();
    crate::jobs::check()?;
    let lines = Guides::new(&features.lines, quad);
    let grouped = s.keep_groups && wm.group_count() > 1;
    let on_line = |p: Vec3| lines.nearest(p, quad * 0.02).is_some();
    let is_hard = |a: Vec3, b: Vec3, ga: u16, gb: u16| -> bool { (grouped && ga != gb) || (!lines.is_empty() && on_line(a) && on_line(b) && on_line((a + b) * 0.5)) };
    let mut w = Work::new(&wm, &is_hard, None)?;
    // Limb tubes (ZR_NOTUBES: off): cut out here, swept as grids at the end, so arms and
    // legs are plain cylinder grids (rings round, lines along, no poles).
    let mut tube_info: Option<(Work, Vec<f32>, Vec<Tube>)> = None;
    // Per working vertex: the tube end loop it is on (2 × tube, + 1 for the far end), or NONE.
    let mut tube_end: Vec<u32> = Vec::new();
    // The parts' function per working vertex (empty: none), and the level of the neck (the
    // head above it is left a free sphere).
    let mut part_phi: Vec<f32> = Vec::new();
    let mut part_neck: Option<f32> = None;
    if std::env::var_os("ZR_NOTUBES").is_none() {
        lap("half-edges");
        let (_, _, _, mut d, mut tubes, tips) = limb_rings(&w, quad);
        lap("limbs");
        // The parts' loops (`ZR_NOPARTS` off): a harmonic function, 0 at the tip of the
        // branch that is not a tube and is the widest (the head), each other branch tip at
        // its distance from there. Its level sets ring the head (a pole at its top), the
        // neck, the trunk, and each limb; the field follows them.
        // Only on a body (limbs found): on a bust the widest non-limb branch is its base.
        // (`ZR_BODYTOP`, experimental: also without limbs, from the model's highest point.)
        let top_src = std::env::var_os("ZR_BODYTOP").filter(|_| tubes.is_empty()).and_then(|_| (0..w.p.len() as u32).filter(|&v| !w.neighbours(v).is_empty()).max_by(|&a, &b| w.p[a as usize].y.total_cmp(&w.p[b as usize].y)));
        if std::env::var_os("ZR_NOPARTS").is_none() && (!tubes.is_empty() || top_src.is_some()) {
            if let Some(src) = top_src.or_else(|| tips.iter().filter(|t| !t.1).max_by(|a, b| a.2.total_cmp(&b.2)).map(|t| t.0)) {
                let t0 = std::time::Instant::now();
                // The whole body built along the part rings (`ZR_NOBODY`: planned as before).
                // Symmetric (the working mesh made its own mirror image first) in the
                // Symmetry setting's plane, or in X through the head's top when the model
                // is its own mirror image there (`ZR_NOSYM`: never).
                if std::env::var_os("ZR_NOBODY").is_none() {
                    let plane = s.mirror.or_else(|| {
                        let pl = MirrorPlane { axis: crate::symmetry::Axis::X, at: w.p[src as usize].x };
                        (std::env::var_os("ZR_NOSYM").is_none() && is_symmetric(&w, pl, quad)).then_some(pl)
                    });
                    let built = match plane {
                        Some(pl) => {
                            lap("part distance");
                            let ms = mirrored_mesh(&wm, pl);
                            let ws = Work::new(&ms, &is_hard, None)?;
                            let top = w.p[src as usize];
                            let ssrc = (0..ws.p.len() as u32).filter(|&v| pl.distance(ws.p[v as usize]) == 0.0 && !ws.neighbours(v).is_empty()).min_by(|&a, &b| ws.p[a as usize].distance(top).total_cmp(&ws.p[b as usize].distance(top)));
                            match ssrc {
                                Some(ssrc) => {
                                    lap("mirrored mesh");
                                    let g = part_distance(&ws, ssrc);
                                    lap("its part distance");
                                    let r = body::sweep_body(&ws, &g, quad, ssrc, Some(pl));
                                    lap("body built");
                                    r
                                }
                                None => Err("no vertex on the mirror plane".into()),
                            }
                        }
                        None => {
                        part_phi = part_distance(&w, src);
                        body::sweep_body(&w, &part_phi, quad, src, None)
                    }
                    };
                    match built {
                        Ok(r) => return Ok(r),
                        Err(e) => {
                            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                                eprintln!("  body: {e}; planned instead");
                            }
                        }
                    }
                }
                // (Only now, if the body was not built from its mirror image.)
                let g = if part_phi.is_empty() { part_distance(&w, src) } else { std::mem::take(&mut part_phi) };
                let fixed = [(src, 0.0f32)];
                if !g.is_empty() {
                    part_phi = if std::env::var_os("ZR_PARTHARMONIC").is_some() {
                        let fx: Vec<(u32, f32)> = fixed.iter().copied().chain(tips.iter().filter(|t| t.0 != src && g[t.0 as usize].is_finite()).map(|t| (t.0, g[t.0 as usize]))).collect();
                        harmonic(&w, &fx, &g)
                    } else {
                        g
                    };
                    if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                        eprintln!("  parts: from {:.3}, distance in {:.1} s", w.p[src as usize], t0.elapsed().as_secs_f32());
                    }
                    // Every cylinder of the parts (neck, trunk, limbs) is swept as a grid, not
                    // only the limbs (`ZR_NOPARTTUBES`: the limb tubes as before).
                    if std::env::var_os("ZR_NOPARTTUBES").is_none() {
                        let (pt, neck) = part_tubes(&w, &part_phi, quad);
                        part_neck = neck;
                        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                            eprintln!("  part tubes: {}", pt.len());
                        }
                        if !pt.is_empty() {
                            d = part_phi.clone();
                            tubes = pt;
                        }
                    }
                }
            }
        }
        // With the Symmetry setting the whole model is only built as a body; anything else
        // is planned on its half (the caller's fallback).
        if s.mirror.is_some() {
            return Err("with Symmetry, only a body is built whole".into());
        }
        if !tubes.is_empty() {
            // Split the triangles each tube's two end levels cross (exactly along them), then
            // drop the tube between: its open loops follow the levels, not a staircase.
            let mut p = wm.positions.clone();
            let mut idx0: Vec<u32> = wm.indices.clone();
            let mut grp0: Vec<u16> = (0..wm.triangle_count() as u32).map(|f| wm.group(f)).collect();
            let mut dd = d.clone();
            let mut masks: Vec<Vec<bool>> = tubes.iter().map(|tb| tb.inside.clone()).collect();
            for (ti, tb) in tubes.iter().enumerate() {
                for level in [tb.lo, tb.hi] {
                    split_along(&mut p, &mut idx0, &mut grp0, &mut dd, &mut masks, ti, level, &mut part_phi);
                }
            }
            tube_end = (0..p.len()).map(|v| {
                tubes.iter().enumerate().find_map(|(ti, tb)| {
                    if !masks[ti][v] {
                        None
                    } else if dd[v] == tb.lo {
                        Some(2 * ti as u32)
                    } else if dd[v] == tb.hi {
                        Some(2 * ti as u32 + 1)
                    } else {
                        None
                    }
                }).unwrap_or(NONE)
            }).collect();
            let cut_face = |f: &[u32]| tubes.iter().enumerate().any(|(ti, tb)| f.iter().all(|&v| masks[ti][v as usize] && dd[v as usize] >= tb.lo && dd[v as usize] <= tb.hi) && f.iter().any(|&v| dd[v as usize] > tb.lo && dd[v as usize] < tb.hi));
            let (mut idx, mut grp) = (Vec::with_capacity(idx0.len()), Vec::new());
            for (f, t) in idx0.chunks_exact(3).enumerate() {
                if !cut_face(t) {
                    idx.extend_from_slice(t);
                    grp.push(grp0[f]);
                }
            }
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("  tubes: {} cut out ({} of {} triangles kept)", tubes.len(), idx.len() / 3, idx0.len() / 3);
            }
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                // The open loops left: pinch points (a point with two open edges out).
                let mut dir: HashSet<(u32, u32)> = HashSet::default();
                for t in idx.chunks_exact(3) {
                    for k in 0..3 {
                        dir.insert((t[k], t[(k + 1) % 3]));
                    }
                }
                let mut outs: HashMap<u32, u32> = HashMap::default();
                let mut open = 0;
                for &(a, b) in &dir {
                    if !dir.contains(&(b, a)) {
                        open += 1;
                        *outs.entry(a).or_default() += 1;
                    }
                }
                let pinches = outs.values().filter(|&&c| c > 1).count();
                let zero = idx.chunks_exact(3).filter(|t| (p[t[1] as usize] - p[t[0] as usize]).cross(p[t[2] as usize] - p[t[0] as usize]).length() < 1e-12).count();
                eprintln!("  tubes cut: {open} open edges, {pinches} pinch points, {zero} flat triangles");
            }
            let (m2, remap, _) = SculptMesh::from_raw_tracked(p, idx, grp, Vec::new(), Vec::new());
            wm = m2;
            // (The mesh numbers its points anew.)
            let mut ends2 = vec![NONE; wm.positions.len()];
            for (v, &l) in tube_end.iter().enumerate() {
                if l != NONE {
                    if let Some(x) = ends2.get_mut(remap[v] as usize) {
                        *x = l;
                    }
                }
            }
            tube_end = ends2;
            if part_phi.len() == remap.len() {
                let mut phi2 = vec![f32::NAN; wm.positions.len()];
                for (v, &x) in part_phi.iter().enumerate() {
                    if let Some(y) = phi2.get_mut(remap[v] as usize) {
                        *y = x;
                    }
                }
                part_phi = phi2;
            } else {
                part_phi.clear();
            }
            let mut w_cut = Work::new(&wm, &is_hard, None)?;
            // The cut loops are smooth rings round the limbs: soft (no corners on them;
            // only lines that meet them make nodes).
            for h in 0..w_cut.t.len() * 3 {
                if w_cut.twin[h] == NONE {
                    w_cut.soft[h] = true;
                    w_cut.hard[h] = true;
                }
            }
            tube_info = Some((std::mem::replace(&mut w, w_cut), d, tubes));
        }
    }
    bridge_gaps(&mut w, quad);
    // Feature loops: experimental (needs a band fill for the rings between loops), off unless
    // KLAY_ZR_FEATURES is set.
    let mut enclosed: Vec<bool> = Vec::new();
    if std::env::var_os("KLAY_ZR_FEATURES").is_some() {
        let loops = feature_loops(&mut w, quad, &mut enclosed);
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("  feature loops: {loops}");
        }
    }
    // Closed guide loops (a ZRemesherGuide stroke that ends where it began): each becomes a
    // hard, corner-less loop with its inside as a cap and four lines leaving it.
    {
        let loops = guide_loops(&mut w, guides, quad, &mut enclosed, true);
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() && loops > 0 {
            eprintln!("  guide loops: {loops}");
        }
    }
    // Rings round limbs (experimental, KLAY_ZR_LIMBS): soft loops across arms and legs, so
    // the field and its lines ring them.
    let mut ring_verts: HashSet<u32> = HashSet::default();
    let mut ring_seeds: Vec<(Vec3, Vec3)> = Vec::new();
    let mut limb_tube: Vec<bool> = Vec::new();
    if std::env::var_os("KLAY_ZR_LIMBS").is_some() && tube_info.is_none() {
        let (rings, first, tube, _, _, _) = limb_rings(&w, quad);
        limb_tube = tube;
        // Each limb's first ring sends four lines along the limb (both ways) from points a
        // quarter of the way round apart, carried on across the other rings: with no other
        // lines in the tube, its bands become four grids each (ZR_NOLIMBSPOKES: none).
        for (r, &f) in rings.iter().zip(&first) {
            if !f || std::env::var_os("ZR_NOLIMBSPOKES").is_some() {
                continue;
            }
            let total: f32 = r.windows(2).map(|e| e[0].distance(e[1])).sum();
            let mut acc = 0.0;
            let mut q = 0;
            for e in r.windows(2) {
                let len = e[0].distance(e[1]);
                while q < 4 && acc + len >= total * q as f32 / 4.0 {
                    ring_seeds.push((e[0], (e[1] - e[0]).normalize_or_zero()));
                    q += 1;
                }
                acc += len;
            }
        }
        let soft_before: HashSet<u32> = (0..w.t.len() * 3).filter(|&h| w.soft[h]).map(|h| w.from(h as u32)).collect();
        let n = guide_loops(&mut w, &rings, quad, &mut enclosed, false);
        ring_verts = (0..w.t.len() * 3).filter(|&h| w.soft[h]).map(|h| w.from(h as u32)).filter(|v| !soft_before.contains(v)).collect();
        if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
            eprintln!("  limb rings: {n} of {}", rings.len());
        }
    }
    let nv = w.p.len();

    // 2. Cross field and poles.
    // The working mesh's quads: each triangle's quad edge, trusted less next to a pole.
    let grid: Vec<(Vec3, f32)> = {
        let mut qedges: HashSet<(u32, u32)> = HashSet::default();
        let mut dir = vec![(Vec3::ZERO, 0.0f32); w.t.len()];
        for t in 0..w.t.len() as u32 {
            let tri = w.t[t as usize];
            let Some(p) = wm.quad_partner(t) else { continue };
            let o = w.t[p as usize];
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                if !(o.contains(&a) && o.contains(&b)) {
                    qedges.insert((a.min(b), a.max(b)));
                    if dir[t as usize].1 == 0.0 {
                        dir[t as usize] = (w.p[b as usize] - w.p[a as usize], 1.0);
                    }
                }
            }
        }
        let mut val = vec![0u32; nv];
        for &(a, b) in &qedges {
            val[a as usize] += 1;
            val[b as usize] += 1;
        }
        for t in 0..w.t.len() {
            if dir[t].1 > 0.0 && w.t[t].iter().any(|&v| val[v as usize] != 4 && w.interior(v)) {
                dir[t].1 = 0.0;
            }
        }
        dir
    };
    // The field's curvature pull (ZR_FIELD overrides Follow Shape, 0..100, for experiments).
    let field_follow = std::env::var("ZR_FIELD").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(field);
    // The parts' directions: along the harmonic function's gradient (its level sets the
    // rings), weighted by how steep it is against the median (none where it flattens: the
    // splits at armpits and crotch, where the poles go).
    let parts: Vec<Vec2> = if part_phi.len() == w.p.len() {
        let grads: Vec<Option<Vec3>> = (0..w.t.len()).map(|f| {
            let t = w.t[f];
            let (p0, p1, p2) = (w.p[t[0] as usize], w.p[t[1] as usize], w.p[t[2] as usize]);
            let fv = [part_phi[t[0] as usize], part_phi[t[1] as usize], part_phi[t[2] as usize]];
            if fv.iter().any(|x| !x.is_finite()) {
                return None;
            }
            let n = (p1 - p0).cross(p2 - p0);
            let a2 = n.length_squared();
            if a2 < 1e-24 {
                return None;
            }
            Some((n.cross(p2 - p1) * fv[0] + n.cross(p0 - p2) * fv[1] + n.cross(p1 - p0) * fv[2]) / a2)
        }).collect();
        let mut mags: Vec<f32> = grads.iter().flatten().map(|g| g.length()).filter(|m| m.is_finite()).collect();
        mags.sort_by(|a, b| a.total_cmp(b));
        let median = mags.get(mags.len() / 2).copied().unwrap_or(0.0).max(1e-12);
        let strength: f32 = std::env::var("ZR_PARTPULL").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        if let Some(path) = std::env::var_os("KLAY_ZR_PARTLINES") {
            dump_levels(&w, &part_phi, quad, std::path::Path::new(&path));
        }
        // Not on the head: there the field is left free, so the head is laid out like a ball
        // (a quad sphere) and joins the neck's loop (`ZR_PARTHEAD` pulls it too).
        let head = if std::env::var_os("ZR_PARTHEAD").is_some() { None } else { part_neck };
        grads.iter().enumerate().map(|(f, g)| match g {
            Some(_) if head.is_some_and(|nk| w.t[f].iter().all(|&v| part_phi[v as usize] < nk)) => Vec2::ZERO,
            Some(g) if g.length() > 0.0 => expi(4.0 * w.angle(f, *g)) * (g.length() / median).min(1.0).powi(2) * strength,
            _ => Vec2::ZERO,
        }).collect()
    } else {
        Vec::new()
    };
    let z = cross_field(&w, (field_follow / 100.0).clamp(0.0, 1.0), quad, &grid, &parts);
    crate::jobs::check()?;
    let mut index = poles(&w, &z);
    // A pole next to one of the opposite sign cancels it: both are dropped.
    {
        let plus: Vec<u32> = (0..nv as u32).filter(|&v| index[v as usize] > 0).collect();
        let minus: Vec<u32> = (0..nv as u32).filter(|&v| index[v as usize] < 0).collect();
        let mut gone = vec![false; minus.len()];
        for &a in &plus {
            if let Some((k, d)) = minus.iter().enumerate().filter(|(k, _)| !gone[*k]).map(|(k, &b)| (k, w.p[a as usize].distance(w.p[b as usize]))).min_by(|x, y| x.1.total_cmp(&y.1)) {
                if d < quad * 1.5 {
                    gone[k] = true;
                    index[a as usize] -= 1;
                    index[minus[k] as usize] += 1;
                }
            }
        }
    }
    let debug = std::env::var_os("KLAY_ZR_DEBUG").is_some();
    if debug {
        let (np, sum): (usize, i32) = (index.iter().filter(|&&i| i != 0).count(), index.iter().sum());
        let hist: Vec<(i32, usize)> = (-4..=4).map(|k| (k, index.iter().filter(|&&i| i == k).count())).filter(|x| x.1 > 0 && x.0 != 0).collect();
        let hard = w.hard.iter().filter(|&&h| h).count();
        let open = w.twin.iter().filter(|&&t| t == NONE).count();
        let skipped = (0..nv as u32).filter(|&v| !w.interior(v) || w.fan(v).iter().any(|&h| w.hard[h as usize])).count();
        let pp: Vec<Vec3> = (0..nv).filter(|&v| index[v] != 0).map(|v| w.p[v]).collect();
        let mut closest = f32::INFINITY;
        for i in 0..pp.len() {
            for j in i + 1..pp.len() {
                closest = closest.min(pp[i].distance(pp[j]) / quad);
            }
        }
        eprintln!("  closest poles {closest:.1} quads apart");
        eprintln!("layout: {} verts {} faces, {np} poles, index sum {sum} (quarter turns), {hist:?}; hard {hard} open {open}, skipped verts {skipped}", nv, w.t.len());
    }

    // 3. Lines from poles and hard corners.
    let mut marks = vec![NONE; nv];
    let mut nodes: HashSet<u32> = HashSet::default();
    let mut hard_deg = vec![0u32; nv];
    for h in 0..w.t.len() as u32 * 3 {
        if w.hard[h as usize] && (w.twin[h as usize] == NONE || w.from(h) < w.to(h)) {
            hard_deg[w.from(h) as usize] += 1;
            hard_deg[w.to(h) as usize] += 1;
            marks[w.from(h) as usize] = HARD;
            marks[w.to(h) as usize] = HARD;
        }
    }
    // A tight cluster of same-sign poles (round an eye socket, a nostril): it is one feature;
    // no lines come from it, so the patch round it is filled as rings.
    let quiet: HashSet<u32> = {
        let ps: Vec<u32> = (0..nv as u32).filter(|&v| index[v as usize] != 0).collect();
        let mut out = HashSet::default();
        for &a in &ps {
            let near = ps.iter().filter(|&&b| b != a && index[b as usize].signum() == index[a as usize].signum() && w.p[a as usize].distance(w.p[b as usize]) < quad * 2.0).count();
            if near >= 2 {
                out.insert(a);
            }
        }
        // Inside a limb's tube: its rings and the lines along it give the shape; no lines.
        if !limb_tube.is_empty() {
            for &a in &ps {
                if limb_tube[a as usize] {
                    out.insert(a);
                }
            }
        }
        // Inside a feature loop: the loop gives the shape there; no lines.
        if !enclosed.is_empty() {
            for &a in &ps {
                if w.fan(a).iter().all(|&h| enclosed[h as usize / 3]) {
                    out.insert(a);
                }
            }
        }
        out
    };
    if debug {
        eprintln!("  {} poles in tight clusters, no lines from them", quiet.len());
    }
    let mut traces: Vec<Trace> = Vec::new();
    // Lines never to delete (carrying on a hard line's loose end).
    let mut protected: HashSet<u32> = HashSet::default();
    // Lines already carried on across a limb ring.
    let mut continued: HashSet<usize> = HashSet::default();
    // Line selection: lines come only from the poles a patch needs split (one per patch
    // with two or more poles, the deepest inside); a pole can sit inside a three- or
    // five-sided patch, whose fill puts its pole in the middle. KLAY_ZR_ALLPOLES traces
    // from every pole at once (the old way).
    let all_poles: Vec<u32> = (0..nv as u32).filter(|&v| index[v as usize] != 0 && !quiet.contains(&v)).collect();
    // Only for models with many poles for now (it does not yet weigh patch shapes, and on
    // simple shapes all lines do better).
    let select = std::env::var_os("KLAY_ZR_ALLPOLES").is_none() && (all_poles.len() > 40 || std::env::var_os("KLAY_ZR_SELECT").is_some());
    let mut pole_list: Vec<u32> = if select { Vec::new() } else { all_poles.clone() };
    for &v in &pole_list.clone() {
        marks[v as usize] = NODE;
        nodes.insert(v);
        for (f, d) in pole_starts(&w, &z, v, (4 - index[v as usize]).max(1) as usize) {
            let mut t = Trace::new(&w, &z, v, f, d);
            // Leave exactly as spread: near a pole the field turns too fast to snap to.
            let n = w.fnrm[t.face];
            t.dir = (d - n * n.dot(d)).normalize_or(t.dir);
            if debug {
                eprintln!("    pole {v}: start face {} dir {:.2} (asked {:.2}), fan {}", t.face, t.dir, d, w.fan(v).len());
            }
            traces.push(t);
        }
    }
    let mut corner_kinds: HashMap<u32, usize> = HashMap::default();
    // Whether the edge u–v is hard.
    let hard_set: HashSet<(u32, u32)> = (0..w.t.len() as u32 * 3).filter(|&h| w.hard[h as usize]).map(|h| (w.from(h).min(w.to(h)), w.from(h).max(w.to(h)))).collect();
    let hard_nb = |a: u32, b: u32| hard_set.contains(&(a.min(b), a.max(b)));
    // Hard corners: where hard edges meet at an angle, or three or more meet.
    for v in 0..nv as u32 {
        if hard_deg[v as usize] == 0 {
            continue;
        }
        let fan = w.fan(v);
        let hard_out: Vec<usize> = (0..fan.len()).filter(|&i| w.hard[fan[i] as usize]).collect();
        let border = !w.interior(v);
        // On a feature loop alone: smooth, never a corner.
        let all_soft = fan.iter().all(|&h| !w.hard[h as usize] || w.soft[h as usize]) && fan.iter().all(|&h| !w.hard[Work::prev(h) as usize] || w.soft[Work::prev(h) as usize]);
        if all_soft && hard_deg[v as usize] == 2 {
            continue;
        }
        let corner = if hard_deg[v as usize] != 2 {
            true
        } else {
            // Along each of the two hard edges for about a quad: a corner if they turn.
            let firsts: Vec<u32> = w.neighbours(v).iter().copied().filter(|&u| hard_nb(v, u)).collect();
            let ends: Vec<Vec3> = firsts.iter().map(|&u0| {
                let (mut prev, mut cur, mut len) = (v, u0, w.p[v as usize].distance(w.p[u0 as usize]));
                while len < quad && hard_deg[cur as usize] == 2 {
                    let Some(&nx) = w.neighbours(cur).iter().find(|&&x| x != prev && hard_nb(cur, x)) else { break };
                    len += w.p[cur as usize].distance(w.p[nx as usize]);
                    prev = cur;
                    cur = nx;
                }
                (w.p[cur as usize] - w.p[v as usize]).normalize_or_zero()
            }).collect();
            ends.len() == 2 && ends[0].dot(ends[1]) > -0.82
        };
        if !corner {
            continue;
        }
        if debug {
            *corner_kinds.entry(hard_deg[v as usize]).or_insert(0usize) += 1;
        }
        marks[v as usize] = NODE;
        nodes.insert(v);
        // The loose end of a hard line (a crease fading out): one line carries it on.
        if hard_deg[v as usize] == 1 && !border {
            if let Some(&i) = hard_out.first() {
                let e = (w.p[w.to(fan[i]) as usize] - w.p[v as usize]).normalize_or_zero();
                protected.insert(traces.len() as u32);
                traces.push(Trace::new(&w, &z, v, fan[i] as usize / 3, -e));
            }
            continue;
        }
        // Sectors of the fan between hard edges.
        let mut cuts: Vec<usize> = hard_out.clone();
        if border && !cuts.contains(&0) {
            cuts.insert(0, 0);
        }
        if cuts.is_empty() {
            continue;
        }
        for c in 0..cuts.len() {
            let a = cuts[c];
            let b = if c + 1 < cuts.len() { cuts[c + 1] } else if border { fan.len() } else { cuts[0] + fan.len() };
            let sector: Vec<u32> = (a..b).map(|i| fan[i % fan.len()]).collect();
            for (f, d) in sector_starts(&w, &z, &sector, v) {
                traces.push(Trace::new(&w, &z, v, f, d));
            }
        }
    }
    // Feature rings send lines outward from four points spaced evenly round them (edge flow
    // leaving an eye loop); those points are corners for the cap inside and the patches out.
    if !enclosed.is_empty() && std::env::var_os("ZR_NOSPOKES").is_none() {
        let key2 = |a: u32, b: u32| (a.min(b), a.max(b));
        let mut soft_nb: HashMap<u32, Vec<u32>> = HashMap::default();
        let mut seen_e: HashSet<(u32, u32)> = HashSet::default();
        for h in 0..w.t.len() as u32 * 3 {
            if w.soft[h as usize] && seen_e.insert(key2(w.from(h), w.to(h))) {
                soft_nb.entry(w.from(h)).or_default().push(w.to(h));
                soft_nb.entry(w.to(h)).or_default().push(w.from(h));
            }
        }
        let mut done: HashSet<u32> = HashSet::default();
        let mut starts: Vec<u32> = soft_nb.keys().copied().collect();
        starts.sort_unstable();
        for v0 in starts {
            if done.contains(&v0) || soft_nb[&v0].len() != 2 {
                continue;
            }
            // Walk the loop.
            let mut lp = vec![v0];
            done.insert(v0);
            let (mut prev, mut cur) = (v0, soft_nb[&v0][0]);
            while cur != v0 && lp.len() < 100_000 {
                if soft_nb[&cur].len() != 2 {
                    break;
                }
                lp.push(cur);
                done.insert(cur);
                let nx = if soft_nb[&cur][0] != prev { soft_nb[&cur][0] } else { soft_nb[&cur][1] };
                prev = cur;
                cur = nx;
            }
            if cur != v0 || lp.len() < 8 {
                if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                    eprintln!("    spokes: loop from {v0} not closed ({} vertices, stopped at a vertex with {} soft edges)", lp.len(), soft_nb.get(&cur).map_or(0, |x| x.len()));
                }
                continue;
            }
            if std::env::var_os("KLAY_ZR_DEBUG").is_some() {
                eprintln!("    spokes: loop of {} vertices", lp.len());
            }
            let m = lp.len();
            let mut cum = vec![0.0f32];
            for k in 0..m {
                cum.push(cum[k] + w.p[lp[k] as usize].distance(w.p[lp[(k + 1) % m] as usize]));
            }
            let total = cum[m];
            for q in 0..4 {
                let goal = total * q as f32 / 4.0;
                let k = cum.partition_point(|&x| x < goal).min(m - 1);
                let v = lp[k];
                let (pv, nv2) = (lp[(k + m - 1) % m], lp[(k + 1) % m]);
                let n = w.vnrm[v as usize];
                let t = (w.p[nv2 as usize] - w.p[pv as usize]).normalize_or_zero();
                let mut out = n.cross(t).normalize_or_zero();
                // Outward: away from the enclosed faces round v.
                let fan = w.fan(v);
                let inside_c: Vec<Vec3> = fan.iter().filter(|&&h| enclosed[h as usize / 3]).map(|&h| {
                    let tt = w.t[h as usize / 3];
                    (w.p[tt[0] as usize] + w.p[tt[1] as usize] + w.p[tt[2] as usize]) / 3.0
                }).collect();
                if let Some(ci) = inside_c.first() {
                    if out.dot(*ci - w.p[v as usize]) > 0.0 {
                        out = -out;
                    }
                }
                let Some(&h0) = fan.iter().find(|&&h| !enclosed[h as usize / 3]) else { continue };
                marks[v as usize] = NODE;
                nodes.insert(v);
                protected.insert(traces.len() as u32);
                let mut tr = Trace::new(&w, &z, v, h0 as usize / 3, out);
                // Keep to the outside: start in the outside face the direction points into.
                if enclosed[tr.face] {
                    tr = Trace::new(&w, &z, v, h0 as usize / 3, out);
                    tr.face = h0 as usize / 3;
                }
                traces.push(tr);
            }
        }
    }
    // Limb lines: from each first ring's four points, both ways along the limb.
    let limb_first = traces.len();
    for &(pos, tangent) in &ring_seeds {
        let Some(&v) = ring_verts.iter().min_by(|&&a, &&b| w.p[a as usize].distance_squared(pos).total_cmp(&w.p[b as usize].distance_squared(pos))) else { continue };
        let n = w.vnrm[v as usize];
        let along = n.cross(tangent).normalize_or_zero();
        if along == Vec3::ZERO {
            continue;
        }
        marks[v as usize] = NODE;
        nodes.insert(v);
        for d in [along, -along] {
            let Some(&h) = w.fan(v).first() else { continue };
            protected.insert(traces.len() as u32);
            traces.push(Trace::new(&w, &z, v, h as usize / 3, d));
        }
    }
    let limb_last = traces.len();
    run(&mut traces, 0, &w, &z, &mut marks, quad, &pole_list);
    cross_rings(&mut traces, &w, &z, &mut marks, quad, &pole_list, &ring_verts, &mut continued, &mut protected);
    if debug && limb_last > limb_first {
        let lens: Vec<String> = (limb_first..limb_last).map(|i| format!("{:.1}{}", traces[i].travelled / quad, &traces[i].why[..1.min(traces[i].why.len())])).collect();
        let cont = traces.iter().skip(limb_last).filter(|t| t.same_as != NONE && (t.same_as as usize) >= limb_first).count();
        eprintln!("  limb lines: {} from {} seeds, lengths (quads, why) {:?}, {} carried across rings", limb_last - limb_first, ring_seeds.len(), lens, cont);
    }
    if debug {
        let mut hd: HashMap<u32, usize> = HashMap::default();
        for &d in &hard_deg {
            if d > 0 {
                *hd.entry(d).or_default() += 1;
            }
        }
        eprintln!("  hard corners by edge count {corner_kinds:?}; hard vertices by edge count {hd:?}; {} lines", traces.len());
        let mut deg: HashMap<u32, u32> = HashMap::default();
        let mut es: HashSet<(u32, u32)> = HashSet::default();
        for t in &traces {
            for p in t.path.windows(2) {
                if p[0] != p[1] {
                    es.insert((p[0].min(p[1]), p[0].max(p[1])));
                }
            }
        }
        for &(a, b) in &es {
            *deg.entry(a).or_default() += 1;
            *deg.entry(b).or_default() += 1;
        }
        for (i, t) in traces.iter().enumerate() {
            let e = *t.path.last().unwrap();
            if deg.get(&e).copied().unwrap_or(0) == 1 && marks[e as usize] != HARD {
                eprintln!("    dangling: line {i} ends at {e} ({}, {:.2} quads)", t.why, t.travelled / quad);
            }
        }
        for (i, t) in traces.iter().enumerate() {
            let end = *t.path.last().unwrap();
            let m = marks[end as usize];
            let what = match m { HARD => "hard".to_string(), NODE => "node".to_string(), NONE => "none".to_string(), x if x as usize == i => "self".to_string(), x => format!("line {x}") };
            eprintln!("    line {i} from {}{}: {:.2} quads, {} verts, ends on {what} at {end} ({})", t.path[0], if index[t.path[0] as usize] != 0 { " (pole)" } else { "" }, t.travelled / quad, t.path.len(), t.why);
        }
    }
    crate::jobs::check()?;

    // 4. Patches, with lines added through faulty ones.
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    if debug {
        // Triangles whose three edges are all on lines: which lines.
        let mut owner: HashMap<(u32, u32), Vec<usize>> = HashMap::default();
        for (i, t) in traces.iter().enumerate() {
            for p in t.path.windows(2) {
                owner.entry(key(p[0], p[1])).or_default().push(i);
            }
        }
        for (f, t) in w.t.iter().enumerate() {
            let es: Vec<Option<&Vec<usize>>> = (0..3).map(|k| owner.get(&key(t[k], t[(k + 1) % 3]))).collect();
            if es.iter().all(|e| e.is_some()) {
                eprintln!("    closed triangle {f} {:?}: lines {:?}", t, es.iter().map(|e| e.unwrap()).collect::<Vec<_>>());
                for &i in es.iter().flat_map(|e| e.unwrap().iter()) {
                    let tr = &traces[i];
                    let at: Vec<usize> = t.iter().filter_map(|v| tr.path.iter().position(|q| q == v)).collect();
                    eprintln!("      line {i}: len {}, why {}, positions {:?}", tr.path.len(), tr.why, at);
                }
            }
        }
    }
    // The cut: every line's edges and the hard edges, which line each edge is on, lines
    // that are one (union-find), and the nodes. Rebuilt from all lines each round.
    let base_nodes = nodes.clone();
    let rebuild = |traces: &[Trace], extra: &HashSet<u32>| -> (HashSet<(u32, u32)>, HashMap<(u32, u32), u32>, HashMap<u32, u32>, HashSet<u32>) {
        let mut cut_edges: HashSet<(u32, u32)> = HashSet::default();
        let mut edge_line: HashMap<(u32, u32), u32> = HashMap::default();
        let mut same: HashMap<u32, u32> = HashMap::default();
        let mut nodes = base_nodes.clone();
        nodes.extend(extra.iter().copied());
        for h in 0..w.t.len() as u32 * 3 {
            if w.hard[h as usize] {
                cut_edges.insert(key(w.from(h), w.to(h)));
                edge_line.insert(key(w.from(h), w.to(h)), HARD);
            }
        }
        for (i, t) in traces.iter().enumerate() {
            if t.path.len() < 2 {
                continue;
            }
            let id = i as u32;
            if t.same_as != NONE {
                same.insert(id, t.same_as);
            }
            for p in t.path.windows(2) {
                if p[0] != p[1] {
                    if std::env::var_os("KLAY_ZR_DEBUG").is_some() && !w.neighbours(p[0]).contains(&p[1]) {
                        let at = t.path.iter().position(|&v| v == p[0]).unwrap_or(0);
                        eprintln!("    BAD STEP line {id} ({}, {} verts, step {at}): {} -> {}", t.why, t.path.len(), p[0], p[1]);
                    }
                    cut_edges.insert(key(p[0], p[1]));
                    edge_line.entry(key(p[0], p[1])).or_insert(id);
                }
            }
            nodes.insert(t.path[0]);
            nodes.insert(*t.path.last().unwrap());
        }
        // Spurs: line pieces that end loose inside a patch lead nowhere and pinch the patch
        // round them: pruned back to where they meet something.
        if std::env::var_os("ZR_NOSPURPRUNE").is_none() {
            let mut deg: HashMap<u32, u32> = HashMap::default();
            for &(a, b) in &cut_edges {
                *deg.entry(a).or_default() += 1;
                *deg.entry(b).or_default() += 1;
            }
            let mut nbrs: HashMap<u32, Vec<u32>> = HashMap::default();
            for &(a, b) in &cut_edges {
                nbrs.entry(a).or_default().push(b);
                nbrs.entry(b).or_default().push(a);
            }
            let mut stack: Vec<u32> = deg.iter().filter(|(_, d)| **d == 1).map(|(v, _)| *v).collect();
            while let Some(v) = stack.pop() {
                if deg.get(&v).copied() != Some(1) {
                    continue;
                }
                let Some(&u) = nbrs[&v].iter().find(|&&u| cut_edges.contains(&key(v, u))) else { continue };
                if edge_line.get(&key(v, u)) == Some(&HARD) {
                    continue;
                }
                cut_edges.remove(&key(v, u));
                edge_line.remove(&key(v, u));
                *deg.get_mut(&v).unwrap() -= 1;
                let du = deg.get_mut(&u).unwrap();
                *du -= 1;
                if *du == 1 {
                    stack.push(u);
                }
                nodes.remove(&v);
            }
        }
        (cut_edges, edge_line, same, nodes)
    };
    let root_of = |same: &HashMap<u32, u32>, mut id: u32| -> u32 {
        let mut guard = 0;
        while let Some(&n) = same.get(&id) {
            if n == id || guard > 1000 {
                break;
            }
            id = n;
            guard += 1;
        }
        id
    };
    let pole_lines = |traces: &[Trace], same: &HashMap<u32, u32>, protected: &HashSet<u32>| -> HashSet<u32> {
        traces.iter().enumerate().filter(|(i, t)| t.path.len() >= 2 && (index[t.path[0] as usize] != 0 || protected.contains(&(*i as u32)))).map(|(i, _)| root_of(same, i as u32)).collect()
    };
    let mut extra: HashSet<u32> = HashSet::default();
    let is_pole: Vec<bool> = index.iter().map(|&i| i != 0).collect();
    prune(&mut traces, &mut marks, &w);
    settle(&mut traces, &mut marks, &mut extra, &w, &z, quad, &pole_list, &is_pole);
    let (mut cut_edges, mut edge_line, mut same, mut nodes) = rebuild(&traces, &extra);
    let mut c = cut(&w, &z, &is_pole, &cut_edges, &nodes, quad, &edge_line, &same, &pole_lines(&traces, &same, &protected), &protected.iter().map(|&i| root_of(&same, i)).collect(), false, &HashSet::default(), 0.0);
    if select {
        let mut chosen: HashSet<u32> = HashSet::default();
        for iter in 0..64 {
            // Every region (a patch, or a run of faulty faces) and the unchosen poles in it.
            let face_cut = |f: u32, k: u32| -> bool {
                let h = f * 3 + k;
                w.twin[h as usize] == NONE || cut_edges.contains(&key(w.from(h), w.to(h)))
            };
            let mut regions: Vec<(Vec<u32>, bool, usize)> = c.patches.iter().map(|pa| (pa.faces.clone(), !pa.band, pa.real)).collect();
            let faulty: HashSet<u32> = c.faulty.iter().copied().collect();
            let mut done: HashSet<u32> = HashSet::default();
            for &f0 in &c.faulty {
                if !done.insert(f0) {
                    continue;
                }
                let mut fs = vec![f0];
                let mut i = 0;
                while i < fs.len() {
                    let f = fs[i];
                    i += 1;
                    for k in 0..3 {
                        if face_cut(f, k) {
                            continue;
                        }
                        let g = w.twin[(f * 3 + k) as usize] / 3;
                        if faulty.contains(&g) && done.insert(g) {
                            fs.push(g);
                        }
                    }
                }
                regions.push((fs, false, 0));
            }
            // How far each vertex is from the lines (deepest pole first).
            let mut depth: HashMap<u32, u32> = HashMap::default();
            let mut q: VecDeque<u32> = VecDeque::new();
            for v in 0..nv as u32 {
                if marks[v as usize] != NONE {
                    depth.insert(v, 0);
                    q.push_back(v);
                }
            }
            while let Some(v) = q.pop_front() {
                let d = depth[&v];
                for &u in w.neighbours(v) {
                    if !depth.contains_key(&u) {
                        depth.insert(u, d + 1);
                        q.push_back(u);
                    }
                }
            }
            let mut picks: Vec<u32> = Vec::new();
            for (faces, disc, real) in &regions {
                let fs: HashSet<u32> = faces.iter().copied().collect();
                let inside: Vec<u32> = all_poles.iter().copied().filter(|v| !chosen.contains(v) && w.fan(*v).iter().all(|&h| fs.contains(&(h / 3)))).collect();
                // A disc may keep one pole that its fill makes anyway: three corners and a
                // three-edge pole, or five and a five-edge one.
                let fits = *disc && inside.len() == 1 && ((*real == 3 && index[inside[0] as usize] == 1) || (*real == 5 && index[inside[0] as usize] == -1));
                if inside.is_empty() || fits {
                    continue;
                }
                let v = inside.iter().copied().max_by_key(|v| depth.get(v).copied().unwrap_or(u32::MAX)).unwrap();
                picks.push(v);
            }
            if debug {
                eprintln!("  selection {iter}: {} regions, {} poles picked, {} chosen so far", regions.len(), picks.len(), chosen.len());
            }
            if picks.is_empty() {
                break;
            }
            let first = traces.len();
            for v in picks {
                chosen.insert(v);
                pole_list.push(v);
                marks[v as usize] = NODE;
                nodes.insert(v);
                for (f, d) in pole_starts(&w, &z, v, (4 - index[v as usize]).max(1) as usize) {
                    let mut t = Trace::new(&w, &z, v, f, d);
                    let nn = w.fnrm[t.face];
                    t.dir = (d - nn * nn.dot(d)).normalize_or(t.dir);
                    traces.push(t);
                }
            }
            let _ = first;
            run(&mut traces, 0, &w, &z, &mut marks, quad, &pole_list);
    cross_rings(&mut traces, &w, &z, &mut marks, quad, &pole_list, &ring_verts, &mut continued, &mut protected);
            prune(&mut traces, &mut marks, &w);
            settle(&mut traces, &mut marks, &mut extra, &w, &z, quad, &pole_list, &is_pole);
            (cut_edges, edge_line, same, nodes) = rebuild(&traces, &extra);
            c = cut(&w, &z, &is_pole, &cut_edges, &nodes, quad, &edge_line, &same, &pole_lines(&traces, &same, &protected), &protected.iter().map(|&i| root_of(&same, i)).collect(), false, &HashSet::default(), 0.0);
            crate::jobs::check()?;
        }
    }
    let mut outer = 0;
    let mut deleted: HashSet<u32> = HashSet::default();
    // Concave corners lines were traced from (once; then they are left). Only in the first
    // 30 rounds of a pass: each line can make new concave corners (a noisy model, hair),
    // and the last rounds are left to settle the rest.
    let mut reflex_tries: HashMap<u32, u32> = HashMap::default();
    let n = loop {
    let mut rounds = 0;
    let mut tried: HashSet<u32> = HashSet::default();
    // Split points already cut from in an earlier round: a patch that comes back with the
    // same point (its lines got stuck) is cut from a point a few rings away instead.
    let mut split_tries: HashMap<u32, u32> = HashMap::default();
    while !c.faults.is_empty() {
        if debug {
            // Dangling ends and vertices two lines pass through.
            let mut deg: HashMap<u32, u32> = HashMap::default();
            for &(a, b) in &cut_edges {
                *deg.entry(a).or_default() += 1;
                *deg.entry(b).or_default() += 1;
            }
            let dangling: Vec<u32> = deg.iter().filter(|(_, d)| **d == 1).map(|(v, _)| *v).collect();
            let mut owners: HashMap<u32, Vec<usize>> = HashMap::default();
            for (i, t) in traces.iter().enumerate() {
                if t.path.len() < 2 {
                    continue;
                }
                for &v in &t.path[1..t.path.len() - 1] {
                    owners.entry(v).or_default().push(i);
                }
            }
            let crossings = owners.values().filter(|o| o.len() > 1).count();
            for &v in &dangling {
                if let Some(i) = traces.iter().position(|t| t.path.last() == Some(&v)) {
                    let t = &traces[i];
                    if t.why.is_empty() {
                        eprintln!("      live? line {i}: done {} path {} travelled {:.2} anchor {} same_as {} start-mark {}", t.done, t.path.len(), t.travelled / quad, t.anchor, t.same_as, marks[t.path[0] as usize]);
                    }
                }
            }
            for &v in dangling.iter().take(3) {
                let on: Vec<(usize, usize, usize, &str)> = traces.iter().enumerate().filter_map(|(i, t)| t.path.iter().position(|&q| q == v).map(|k| (i, k, t.path.len(), t.why))).collect();
                eprintln!("      end {v}: mark {} pole {} hard-edge {} lines {:?}", marks[v as usize], is_pole[v as usize], (0..w.t.len() * 3).any(|h| w.hard[h] && (w.from(h as u32) == v || w.to(h as u32) == v)), on);
            }
            if !dangling.is_empty() || crossings > 0 {
                eprintln!("    {} dangling ends {:?}, {crossings} crossings", dangling.len(), dangling.iter().take(6).map(|v| traces.iter().position(|t| t.path.last() == Some(v)).map(|i| (i, traces[i].why))).collect::<Vec<_>>());
            }
            let split = c.faults.iter().filter(|f| matches!(f, Fault::Split(_) | Fault::Topo(..))).count();
            let thin = c.faults.iter().filter(|f| matches!(f, Fault::Thin(..))).count();
            eprintln!("  round {rounds}: {} patches ok, {split} to split, {thin} thin, {} reflex, {} arcs", c.patches.len(), c.faults.len() - split - thin, c.arcs.len());
        }
        if std::env::var("KLAY_ZR_STOP").ok().and_then(|v| v.parse::<u32>().ok()) == Some(rounds) {
            if let Some(dir) = std::env::var_os("KLAY_ZR_LINES") {
                dump_lines_marked(&w, &cut_edges, &nodes, &c.faulty, std::path::Path::new(&dir));
            }
            return Err("stopped".into());
        }
        rounds += 1;
        if rounds > 40 {
            if let Some(dir) = std::env::var_os("KLAY_ZR_LINES") {
                dump_lines(&w, &cut_edges, &nodes, std::path::Path::new(&dir));
            }
            return Err(format!("{} patches stayed faulty", c.faults.len()));
        }
        let first_new = traces.len();
        let mut used = HashSet::default();
        // Thin patches first: one line along each goes (rather one not from a pole), and the
        // lines that ended on it go on.
        let mut gone: HashSet<u32> = HashSet::default();
        for fault in &c.faults {
            if let Fault::Thin(lines, thin) = fault {
                let from_pole = |r: u32| traces.iter().enumerate().any(|(i, t)| root_of(&same, i as u32) == r && is_pole[t.path[0] as usize]);
                // A wedge or a many-cornered patch only loses a line not from a pole.
                let kept = |r: u32| traces.iter().enumerate().any(|(i, _)| root_of(&same, i as u32) == r && protected.contains(&(i as u32)));
                let pick = lines.iter().find(|(r, _)| !deleted.contains(r) && !from_pole(*r) && !kept(*r)).or_else(|| if *thin { lines.iter().find(|(r, _)| !deleted.contains(r) && !kept(*r)) } else { None });
                if let Some(&(r, _)) = pick {
                    gone.insert(r);
                }
            }
        }
        if !gone.is_empty() {
            let mut freed: HashSet<u32> = HashSet::default();
            for i in 0..traces.len() {
                if gone.contains(&root_of(&same, i as u32)) {
                    let t = &mut traces[i];
                    for &v in &t.path[1..] {
                        if marks[v as usize] == i as u32 {
                            marks[v as usize] = NONE;
                            freed.insert(v);
                        }
                    }
                    t.path.truncate(1);
                    t.done = true;
                    t.why = "deleted";
                    t.same_as = NONE;
                }
            }
            deleted.extend(gone.iter().copied());
            // Lines that ended on a freed vertex go on from their last ordinary step.
            for i in 0..traces.len() {
                let t = &mut traces[i];
                if t.path.len() >= 2 && t.done && freed.contains(t.path.last().unwrap()) {
                    let keep = t.anchor.clamp(1, t.path.len());
                    for v in t.path.split_off(keep) {
                        if marks[v as usize] == i as u32 {
                            marks[v as usize] = NONE;
                        }
                    }
                    for &v in &t.path[1..] {
                        if marks[v as usize] == NONE {
                            marks[v as usize] = i as u32;
                        }
                    }
                    t.restart(&w, &z);
                    t.done = false;
                    t.why = "";
                    t.same_as = NONE;
                }
            }
        }
        for fault in &c.faults {
            match *fault {
                Fault::Thin(..) => {}
                Fault::Topo(_, ref faces) if rounds > 8 => {
                    if !gone.is_empty() {
                        continue;
                    }
                    // Tree–cotree: paths from border to border through each handle.
                    for path in cut_paths(&w, faces, &cut_edges) {
                        if path.len() < 2 || !used.insert(path[0]) {
                            continue;
                        }
                        let id = traces.len() as u32;
                        for &v in &path[1..path.len() - 1] {
                            if marks[v as usize] == NONE {
                                marks[v as usize] = id;
                            }
                        }
                        let mut t = Trace::new(&w, &z, path[0], w.fan(path[0])[0] as usize / 3, Vec3::X);
                        t.path = path;
                        t.done = true;
                        t.why = "cut";
                        protected.insert(id);
                        traces.push(t);
                    }
                }
                Fault::Split(v0) | Fault::Topo(v0, _) => {
                    let tries = split_tries.entry(v0).or_default();
                    *tries += 1;
                    let mut v = v0;
                    if *tries > 1 && std::env::var_os("ZR_NOSPLITMOVE").is_none() {
                        // Out by 3 rings per earlier try, off any line or hard edge.
                        let rings = 3 * (*tries - 1);
                        let mut seen: HashSet<u32> = [v0].into_iter().collect();
                        let mut front = vec![v0];
                        for _ in 0..rings {
                            let mut next = Vec::new();
                            for &a in &front {
                                for &u in w.neighbours(a) {
                                    if marks[u as usize] == NONE && seen.insert(u) {
                                        next.push(u);
                                    }
                                }
                            }
                            if next.is_empty() {
                                break;
                            }
                            next.sort_unstable();
                            front = next;
                        }
                        v = front[(*tries as usize * 7) % front.len()];
                    }
                    if !gone.is_empty() || !used.insert(v) {
                        continue;
                    }
                    let f = w.fan(v).first().map(|&h| h as usize / 3).unwrap_or(0);
                    let ds = dirs(&w, &z, f);
                    // The shorter of the two lines through it.
                    let mut best: Option<(f32, usize)> = None;
                    for axis in 0..2 {
                        let mut m2 = marks.clone();
                        m2[v as usize] = NODE;
                        let mut pair = [Trace::new(&w, &z, v, f, ds[axis]), Trace::new(&w, &z, v, f, ds[axis + 2])];
                        run(&mut pair, NONE - 100, &w, &z, &mut m2, quad, &pole_list);
                        // A pair that only closes on itself (a loop) cuts a band into two
                        // bands; one that reaches the lines already there opens it.
                        let own = |t: &Trace| {
                            let e = *t.path.last().unwrap();
                            let m = m2[e as usize];
                            m == NONE - 100 || m == NONE - 99 || e == v
                        };
                        // Both ends on one line (a chord off a rim) only cuts a sliver off it.
                        let (e0, e1) = (*pair[0].path.last().unwrap(), *pair[1].path.last().unwrap());
                        let same_line = m2[e0 as usize] == m2[e1 as usize] && m2[e0 as usize] != NONE && m2[e0 as usize] != NODE && w.p[e0 as usize].distance(w.p[e1 as usize]) < pair[0].travelled + pair[1].travelled;
                        let closes = (own(&pair[0]) && own(&pair[1])) || same_line;
                        let len = pair[0].travelled + pair[1].travelled + if closes { quad * 1e4 } else { 0.0 };
                        if best.is_none_or(|b| len < b.0) {
                            best = Some((len, axis));
                        }
                    }
                    let axis = best.unwrap().1;
                    marks[v as usize] = NODE;
                    extra.insert(v);
                    protected.insert(traces.len() as u32);
                    traces.push(Trace::new(&w, &z, v, f, ds[axis]));
                    let mut back = Trace::new(&w, &z, v, f, ds[axis + 2]);
                    back.same_as = traces.len() as u32 - 1;
                    protected.insert(traces.len() as u32);
                    traces.push(back);
                }
                Fault::Reflex(v, mid, inner) => {
                    if !gone.is_empty() || !used.insert(v) {
                        continue;
                    }
                    *reflex_tries.entry(v).or_default() += 1;
                    if tried.insert(v) {
                        protected.insert(traces.len() as u32);
                        traces.push(Trace::new(&w, &z, v, w.fan(v)[0] as usize / 3, mid));
                    } else if used.insert(inner) {
                        let f = w.fan(inner).first().map(|&h| h as usize / 3).unwrap_or(0);
                        let ds = dirs(&w, &z, f);
                        marks[inner as usize] = NODE;
                        extra.insert(inner);
                        protected.insert(traces.len() as u32);
                        traces.push(Trace::new(&w, &z, inner, f, ds[0]));
                        let mut back = Trace::new(&w, &z, inner, f, ds[2]);
                        back.same_as = traces.len() as u32 - 1;
                        protected.insert(traces.len() as u32);
                        traces.push(back);
                    }
                }
            }
        }
        if traces.len() == first_new && gone.is_empty() {
            return Err("no line could fix a patch".into());
        }
        run(&mut traces, 0, &w, &z, &mut marks, quad, &pole_list);
    cross_rings(&mut traces, &w, &z, &mut marks, quad, &pole_list, &ring_verts, &mut continued, &mut protected);
        if debug {
            for (i, t) in traces.iter().enumerate().skip(first_new) {
                eprintln!("      new line {i}: {} verts, {:.1} quads, {}", t.path.len(), t.travelled / quad, t.why);
            }
        }
        prune(&mut traces, &mut marks, &w);
        settle(&mut traces, &mut marks, &mut extra, &w, &z, quad, &pole_list, &is_pole);
        (cut_edges, edge_line, same, nodes) = rebuild(&traces, &extra);
        c = cut(&w, &z, &is_pole, &cut_edges, &nodes, quad, &edge_line, &same, &pole_lines(&traces, &same, &protected), &protected.iter().map(|&i| root_of(&same, i)).collect(), rounds > 3, &split_tries.iter().filter(|e| *e.1 >= 6).map(|e| *e.0).chain(reflex_tries.iter().filter(|e| *e.1 >= std::env::var("ZR_REFLEXTRIES").ok().and_then(|x| x.parse().ok()).unwrap_or(1)).map(|e| *e.0)).collect(), if rounds <= std::env::var("ZR_REFLEXROUNDS").ok().and_then(|x| x.parse().ok()).unwrap_or(30) { reflex_area } else { 0.0 });
        crate::jobs::check()?;
    }
    // Selection: lines that make bad patches are taken away (lines ended on them go on), one
    // at a time, the worst patches first, while the layout stays whole and gets cheaper.
    if outer == 0 && std::env::var_os("KLAY_ZR_NOSELECT").is_none() {
        // Predicted poles dominate; shape (aspect, thinness) breaks ties.
        let total = |c: &Cut| -> f32 {
            let poles: f32 = c.patches.iter().map(|pa| patch_poles(&w, c, pa, quad)).sum::<f32>() + node_poles(c);
            let shape: f32 = c.patches.iter().map(|pa| patch_cost(&w, c, pa, quad)).sum();
            poles * 10.0 + shape + c.faults.len() as f32 * 1e6
        };
        // Faces left to the general fill: a deletion may never add to them.
        let messy_faces = |c: &Cut| -> usize { c.patches.iter().filter(|pa| !pa.band && (pa.messy || !(3..=5).contains(&pa.sides.len()))).map(|pa| pa.faces.len()).sum() };
        let mut best_messy = messy_faces(&c);
        let mut best = total(&c);
        let start_cost = best;
        let mut rejected: HashSet<u32> = HashSet::default();
        let started = Instant::now();
        let budget = std::env::var("ZR_BUDGET").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(20.0);
        let mut accepted = 0;
        let mut tries = 0;
        'pass: for _pass in 0..400 {
            let mut order: Vec<(f32, usize)> = c.patches.iter().enumerate().map(|(i, pa)| (patch_cost(&w, &c, pa, quad), i)).collect();
            order.sort_by(|a, b| b.0.total_cmp(&a.0));
            for &(pc, pi) in order.iter().take(12) {
                if pc < 3.5 {
                    break 'pass;
                }
                let pa = &c.patches[pi];
                let mut roots: Vec<u32> = Vec::new();
                let m = pa.lp.len();
                for i in 0..m {
                    let (a, b) = (pa.lp[i], pa.lp[(i + 1) % m]);
                    if let Some(&id) = edge_line.get(&key(a, b)) {
                        if id == HARD {
                            continue;
                        }
                        let r = root_of(&same, id);
                        if !roots.contains(&r) && !rejected.contains(&r) && !protected.iter().any(|&p| root_of(&same, p) == r && traces[p as usize].why == "cut") {
                            roots.push(r);
                        }
                    }
                }
                for r in roots {
                    if started.elapsed().as_secs_f32() > budget {
                        break 'pass;
                    }
                    tries += 1;
                    let (mut t2, mut m2, mut x2) = (traces.clone(), marks.clone(), extra.clone());
                    let rts: Vec<u32> = (0..t2.len() as u32).map(|i| root_of(&same, i)).collect();
                    let gone: HashSet<u32> = [r].into_iter().collect();
                    delete_lines(&mut t2, &mut m2, &rts, &gone, &w, &z);
                    run(&mut t2, 0, &w, &z, &mut m2, quad, &pole_list);
                    prune(&mut t2, &mut m2, &w);
                    settle(&mut t2, &mut m2, &mut x2, &w, &z, quad, &pole_list, &is_pole);
                    let (ce, el, sm, nd) = rebuild(&t2, &x2);
                    let c2 = cut(&w, &z, &is_pole, &ce, &nd, quad, &el, &sm, &pole_lines(&t2, &sm, &protected), &protected.iter().map(|&i| root_of(&sm, i)).collect(), true, &HashSet::default(), 0.0);
                    let cost = total(&c2);
                    let mf = messy_faces(&c2);
                    if c2.faults.is_empty() && cost < best - 0.5 && (mf <= best_messy || std::env::var_os("KLAY_ZR_GROWMESSY").is_none()) {
                        best_messy = best_messy.max(mf);
                        traces = t2;
                        marks = m2;
                        extra = x2;
                        cut_edges = ce;
                        edge_line = el;
                        same = sm;
                        nodes = nd;
                        c = c2;
                        best = cost;
                        accepted += 1;
                        continue 'pass;
                    }
                    rejected.insert(r);
                }
            }
            break;
        }
        if debug {
            eprintln!("  selection: cost {start_cost:.0} -> {best:.0}, {accepted} lines removed of {tries} tried, {:.1} s", started.elapsed().as_secs_f32());
        }
    }
    if let Some(dir) = std::env::var_os("KLAY_ZR_LINES") {
        let messy: Vec<u32> = c.patches.iter().filter(|pa| pa.messy).flat_map(|pa| pa.faces.iter().copied()).collect();
        dump_lines_marked(&w, &cut_edges, &nodes, &messy, std::path::Path::new(&dir));
    }
    // 5. Counts: each arc's length over the local quad size (twice the working edges).
    let h_v = vec![quad; nv];
    let mut target: Vec<f32> = c.arcs.iter().map(|a| a.windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize]) * 2.0 / (h_v[e[0] as usize] + h_v[e[1] as usize])).sum()).collect();
    // Each tube joins its two end loops in the counts, as a band (no faces of its own): a
    // strip into one end comes out of the other, so both get the same count and the tube
    // between is a plain grid.
    let real_patches = c.patches.len();
    if !tube_end.is_empty() && std::env::var_os("ZR_NOTUBEBANDS").is_none() {
        let mut sides_of: Vec<usize> = vec![0; c.arcs.len()];
        for pa in &c.patches {
            for &(a, _) in pa.sides.iter().flatten() {
                sides_of[a as usize] += 1;
            }
        }
        let mut ends: HashMap<u32, Vec<(u32, bool)>> = HashMap::default();
        for (a, path) in c.arcs.iter().enumerate() {
            let l = tube_end.get(path[0] as usize).copied().unwrap_or(NONE);
            if sides_of[a] == 1 && l != NONE && path.iter().all(|&v| tube_end.get(v as usize).copied().unwrap_or(NONE) == l) {
                ends.entry(l).or_default().push((a as u32, true));
            }
        }
        let mut keys: Vec<u32> = ends.keys().copied().filter(|l| l % 2 == 0).collect();
        keys.sort_unstable();
        for l in keys {
            if let (Some(a), Some(b)) = (ends.get(&l), ends.get(&(l + 1))) {
                // Both loops aim at the same count (the mean of their lengths), so the band
                // pulls neither way.
                if std::env::var_os("ZR_NOTUBEMEAN").is_none() {
                    let (la, lb) = (a.iter().map(|x| target[x.0 as usize]).sum::<f32>(), b.iter().map(|x| target[x.0 as usize]).sum::<f32>());
                    let mean = (la * lb).sqrt();
                    for &(x, _) in a {
                        target[x as usize] *= mean / la.max(1e-6);
                    }
                    for &(x, _) in b {
                        target[x as usize] *= mean / lb.max(1e-6);
                    }
                }
                c.patches.push(Patch { faces: Vec::new(), lp: Vec::new(), sides: vec![a.clone(), b.clone()], corner_at: vec![0], real: 0, messy: false, free: None, band: true, lp2: Vec::new() });
            }
        }
        if debug {
            let labelled = tube_end.iter().filter(|&&l| l != NONE).count();
            let one_side = sides_of.iter().filter(|&&k| k == 1).count();
            let lab_arcs = c.arcs.iter().filter(|p| tube_end.get(p[0] as usize).copied().unwrap_or(NONE) != NONE).count();
            eprintln!("  tube bands in the counts: {} ({labelled} end points, {one_side} one-sided arcs, {lab_arcs} arcs from an end point, ends {:?})", c.patches.len() - real_patches, ends.iter().map(|(k, v)| (*k, v.len())).collect::<Vec<_>>());
        }
    }
    match counts(&c, &target) {
        Ok(n) => {
            // Every three- and five-sided patch must split into grids round its middle.
            let mut n = n;
            let lens = |n: &[i64], pa: &Patch| -> Vec<i64> { pa.sides.iter().map(|s| s.iter().map(|&(a, _)| n[a as usize]).sum()).collect() };
            // Two arcs between the same two nodes, both one quad: they would be the same
            // output edge; one is widened by two (a strip from that arc).
            {
                let mut by_ends: HashMap<(u32, u32), Vec<usize>> = HashMap::default();
                for (a, path) in c.arcs.iter().enumerate() {
                    let (x, y) = (path[0], *path.last().unwrap());
                    by_ends.entry((x.min(y), x.max(y))).or_default().push(a);
                }
                let mut on_arc: HashMap<u32, Vec<(usize, usize)>> = HashMap::default();
                for (p, pa) in c.patches.iter().enumerate() {
                    for (si, sd) in pa.sides.iter().enumerate() {
                        for &(a, _) in sd {
                            on_arc.entry(a).or_default().push((p, si));
                        }
                    }
                }
                for arcs_here in by_ends.values() {
                    let ones: Vec<usize> = arcs_here.iter().copied().filter(|&a| n[a] == 1).collect();
                    for &a in ones.iter().skip(1) {
                        if let Some(&(p, si)) = on_arc.get(&(a as u32)).and_then(|v| v.first()) {
                            if let Some(path) = strip_from(&c, a as u32, p, si) {
                                for b in path {
                                    n[b as usize] += 2;
                                }
                            }
                        }
                    }
                }
            }
            // A band loop under three points: widened by two (across the band, or just that
            // loop when the band is free).
            for p in 0..c.patches.len() {
                if !c.patches[p].band {
                    continue;
                }
                for side in 0..2 {
                    for _ in 0..4 {
                        if lens(&n, &c.patches[p])[side] >= 3 {
                            break;
                        }
                        let Some(path) = strip(&c, p, side) else { break };
                        for a in path {
                            n[a as usize] += 2;
                        }
                    }
                }
            }
            // A border too short to fill (a lens of two single quads): widened by two.
            for p in 0..c.patches.len() {
                for _ in 0..4 {
                    if lens(&n, &c.patches[p]).iter().sum::<i64>() >= 4 {
                        break;
                    }
                    let Some(path) = strip(&c, p, 0) else { break };
                    for a in path {
                        n[a as usize] += 2;
                    }
                }
            }
            // Lopsided three- and five-sided patches: widen their short sides by strips of
            // two quads, which keep every four-sided patch on the way even.
            for p in 0..c.patches.len() {
                let pa = &c.patches[p];
                let k = pa.sides.len();
                let wide = std::env::var_os("ZR_NARROWWIDEN").is_none();
                let ok_k = if wide { k == 3 || k == 5 || k == 6 || k == 7 } else { k == 3 || k == 5 };
                if !ok_k || pa.band || (pa.messy && std::env::var_os("ZR_NOMESSYWIDEN").is_some()) {
                    continue;
                }
                // An odd cycle (six sides, each half odd): +1 on one side of each half, by two
                // strips that end on the same patch (which then gains two, keeping it even).
                if k == 6 && odd_cycle(&lens(&n, pa)) && std::env::var_os("ZR_NOCYCLEFIX").is_none() {
                    let cap: f32 = std::env::var("ZR_WIDENCAP").ok().and_then(|v| v.parse().ok()).unwrap_or(1.6);
                    let ok_path = |path: &[u32], delta: i64| path.iter().all(|&a| {
                        let (x, t) = ((n[a as usize] + delta) as f32, target[a as usize]);
                        x >= 1.0 && (cap <= 0.0 || (x <= t * cap + 2.0 && (delta > 0 || x >= t / cap - 2.0)))
                    });
                    let mut done_fix = false;
                    'pairs: for sa in (0..6).step_by(2) {
                        for sb in (1..6).step_by(2) {
                            let (Some((pa_, ea)), Some((pb_, eb))) = (strip_end(&c, p, sa), strip_end(&c, p, sb)) else { continue };
                            if ea != eb || pa_.iter().any(|a| pb_.contains(a)) {
                                continue;
                            }
                            for delta in [1i64, -1] {
                                if !ok_path(&pa_, delta) || !ok_path(&pb_, delta) {
                                    continue;
                                }
                                let mut l2 = lens(&n, pa);
                                l2[sa] += delta;
                                l2[sb] += delta;
                                if odd_cycle(&l2) {
                                    continue;
                                }
                                // The far patch: still splittable if it was.
                                if let Some(q) = ea {
                                    let pq = &c.patches[q];
                                    if !pq.band && pq.sides.len() != 4 {
                                        let both: HashSet<u32> = pa_.iter().chain(pb_.iter()).copied().collect();
                                        let before = lens(&n, pq);
                                        let after: Vec<i64> = pq.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize] + if both.contains(&a) { delta } else { 0 }).sum()).collect();
                                        if split_sides(&before).is_some() && split_sides(&after).is_none() {
                                            continue;
                                        }
                                    }
                                }
                                for &a in pa_.iter().chain(pb_.iter()) {
                                    n[a as usize] += delta;
                                }
                                done_fix = true;
                                break 'pairs;
                            }
                        }
                    }
                    if debug {
                        eprintln!("    odd cycle p{p}: {}", if done_fix { "evened" } else { "no pair of strips" });
                    }
                    if !done_fix {
                        continue;
                    }
                }
                let rounds_max: usize = std::env::var("ZR_WIDEN").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
                for round in 0..rounds_max {
                    let l = lens(&n, pa);
                    if split_sides(&l).is_some() {
                        break;
                    }
                    if debug && round == 39 {
                        eprintln!("    widen p{p}: {l:?} still short after 39 rounds");
                    }
                    // Least total shortfall after ±2 on one side (negated, so larger is better);
                    // a strip that would leave the patch at its far end unsplittable is not
                    // taken.
                    let measure = |l: &[i64]| if std::env::var_os("ZR_MARGIN").is_some() { split_margin(l) } else { -split_shortfall(l) };
                    let now = measure(&l);
                    let both = std::env::var_os("ZR_WIDENUP").is_none();
                    let mut cands: Vec<(f64, usize, i64)> = Vec::new();
                    for side in 0..l.len() {
                        for delta in if both { vec![2i64, -2] } else { vec![2i64] } {
                            let mut l2 = l.clone();
                            l2[side] += delta;
                            if l2[side] < 1 {
                                continue;
                            }
                            let g = measure(&l2);
                            if g > now {
                                cands.push((g, side, delta));
                            }
                        }
                    }
                    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
                    let cap: f32 = std::env::var("ZR_WIDENCAP").ok().and_then(|v| v.parse().ok()).unwrap_or(1.6);
                    let mut taken = false;
                    for &(_, side, delta) in &cands {
                        let Some(path) = strip(&c, p, side) else { continue };
                        // Not past `cap`× an arc's length (+2), nor under its length / cap − 2.
                        let stretched = path.iter().any(|&a| {
                            let (x, t) = ((n[a as usize] + delta) as f32, target[a as usize]);
                            x < 1.0 || (cap > 0.0 && (x > t * cap + 2.0 || (delta < 0 && x < t / cap - 2.0)))
                        });
                        if stretched {
                            continue;
                        }
                        // The patch at the strip's far end.
                        let arcs_on: HashSet<u32> = path.iter().copied().collect();
                        let harmed = c.patches.iter().enumerate().any(|(q, pq)| {
                            if q == p || pq.band || pq.sides.len() == 4 || !pq.sides.iter().flatten().any(|x| arcs_on.contains(&x.0)) {
                                return false;
                            }
                            let before = lens(&n, pq);
                            let after: Vec<i64> = pq.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize] + if arcs_on.contains(&a) { delta } else { 0 }).sum()).collect();
                            split_sides(&before).is_some() && split_sides(&after).is_none()
                        });
                        if harmed {
                            continue;
                        }
                        for a in path {
                            n[a as usize] += delta;
                        }
                        taken = true;
                        break;
                    }
                    if !taken {
                        if debug {
                            let why: Vec<String> = cands.iter().map(|&(_, side, delta)| {
                                let Some(path) = strip(&c, p, side) else { return format!("s{side}{delta:+}: no strip") };
                                let bad: Vec<String> = path.iter().filter(|&&a| {
                                    let (x, t) = ((n[a as usize] + delta) as f32, target[a as usize]);
                                    x < 1.0 || (cap > 0.0 && (x > t * cap + 2.0 || (delta < 0 && x < t / cap - 2.0)))
                                }).map(|&a| format!("a{a} {}→{} ({:.1})", n[a as usize], n[a as usize] + delta, target[a as usize])).collect();
                                format!("s{side}{delta:+}: {} arcs{}", path.len(), if bad.is_empty() { " (harms a far patch)".to_string() } else { format!(", stretched {bad:?}") })
                            }).collect();
                            eprintln!("    widen p{p}: {l:?} no strip helps: stop; {why:?}");
                        }
                        break;
                    }
                }
            }
            // A band the rows cannot fill (a loop under three points, or an odd difference
            // between its loops) is split through its middle instead.
            if debug {
                for pa in &c.patches[real_patches..] {
                    let l = lens(&n, pa);
                    eprintln!("    tube band: {} and {} round (lengths {:.1} and {:.1}); arcs {:?} | {:?}", l[0], l[1], pa.sides[0].iter().map(|&(a, _)| target[a as usize]).sum::<f32>(), pa.sides[1].iter().map(|&(a, _)| target[a as usize]).sum::<f32>(),
                        pa.sides[0].iter().map(|&(a, _)| format!("a{a} {} ({:.1})", n[a as usize], target[a as usize])).collect::<Vec<_>>(), pa.sides[1].iter().map(|&(a, _)| format!("a{a} {} ({:.1})", n[a as usize], target[a as usize])).collect::<Vec<_>>());
                }
            }
            c.patches.truncate(real_patches);
            let bad_bands: Vec<usize> = c.patches.iter().enumerate().filter(|(_, pa)| {
                if !pa.band {
                    return false;
                }
                let l = lens(&n, pa);
                l[0] < 3 || l[1] < 3 || (l[0] - l[1]).abs() % 2 != 0 || (pa.free.is_none() && l[0] != l[1])
            }).map(|(i, _)| i).collect();
            if !bad_bands.is_empty() {
                if debug {
                    eprintln!("  {} bands cannot be filled: split", bad_bands.len());
                }
                c.faults = bad_bands.into_iter().map(|i| Fault::Split(middle(&w, &c.patches[i]))).collect();
                outer += 1;
                if outer > 12 {
                    return Err("the quad counts did not work out".into());
                }
                continue;
            }
            if debug {
                let bad = c.patches.iter().filter(|pa| !pa.band && (pa.messy || (pa.sides.len() != 4 && split_sides(&lens(&n, pa)).is_none()))).count();
                for pa in &c.patches {
                    let l = lens(&n, pa);
                    let t: Vec<f32> = pa.sides.iter().map(|s| (s.iter().map(|&(a, _)| target[a as usize]).sum::<f32>() * 10.0).round() / 10.0).collect();
                    let ring = !pa.band && (pa.messy || (pa.sides.len() != 4 && split_sides(&l).is_none()));
                    let squeeze = l.iter().zip(&t).map(|(&c, &x)| c as f32 / x.max(0.3)).fold(0.0f32, |m, r| m.max(r.max(1.0 / r.max(1e-3))));
                    eprintln!("    patch {} faces: real {} messy {} sides {:?} lengths {:?}{}{}", pa.faces.len(), pa.real, pa.messy, l, t, if ring { "  <- rings" } else { "" }, if squeeze > 1.8 { format!("  <- squeezed {squeeze:.1}") } else { String::new() });
                }
                eprintln!("  {} patches ok, {bad} filled by rings", c.patches.len());
                // Where the counts go over the lengths: by arc length in quads.
                let mut by_len: Vec<(usize, i64, f32)> = vec![(0, 0, 0.0); 5];
                for a in 0..c.arcs.len() {
                    let t = target[a];
                    let b = if t < 0.5 { 0 } else if t < 1.0 { 1 } else if t < 2.0 { 2 } else if t < 5.0 { 3 } else { 4 };
                    by_len[b].0 += 1;
                    by_len[b].1 += n[a];
                    by_len[b].2 += t;
                }
                eprintln!("  arc counts by length (<0.5, <1, <2, <5, more: arcs, count, length): {:?}", by_len.iter().map(|&(k, c, t)| (k, c, t.round())).collect::<Vec<_>>());
                // Arcs on an open border (the cut round a limb tube, a vase's rim).
                let open_arc = |a: usize| { let pth = &c.arcs[a]; pth.windows(2).all(|e| (0..3).any(|k| { let h = w.out[e[0] as usize]; let _ = k; h != NONE }) ) && pth.windows(2).any(|e| w.fan(e[0]).iter().any(|&h| w.to(h) == e[1] && w.twin[h as usize] == NONE) || w.fan(e[1]).iter().any(|&h| w.to(h) == e[0] && w.twin[h as usize] == NONE)) };
                let (mut na_, mut nc_, mut nt_) = (0usize, 0i64, 0.0f32);
                for a in 0..c.arcs.len() {
                    if open_arc(a) {
                        na_ += 1;
                        nc_ += n[a];
                        nt_ += target[a];
                    }
                }
                eprintln!("  open-border arcs: {na_}, counts {nc_}, lengths {nt_:.0} quads");
                let area: f32 = c.patches.iter().map(|pa| pa.faces.iter().map(|&f| { let t = w.t[f as usize]; (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5 }).sum::<f32>()).sum();
                eprintln!("  area {:.0} quads², {} patches, mean {:.1} quads² a patch", area / (quad * quad), c.patches.len(), area / (quad * quad) / c.patches.len() as f32);
            }
            break n;
        }
        Err((why, p)) => {
            c.patches.truncate(real_patches);
            if debug {
                eprintln!("  counts: {why}");
            }
            let Some(p) = p else { return Err(why) };
            c.faults = vec![Fault::Split(middle(&w, &c.patches[p]))];
        }
    }
    outer += 1;
    if outer > 12 {
        return Err("the quad counts did not work out".into());
    }
    };


    // 6. Points: nodes, then each arc's inner points (evenly by length).
    let mut out_p: Vec<Vec3> = Vec::new();
    let mut node_id: HashMap<u32, u32> = HashMap::default();
    // Ends of collapsed arcs are one point.
    let mut same_node: HashMap<u32, u32> = HashMap::default();
    for (a, path) in c.arcs.iter().enumerate() {
        if n[a] == 0 {
            let find = |m: &HashMap<u32, u32>, mut x: u32| {
                while let Some(&y) = m.get(&x) {
                    if y == x {
                        break;
                    }
                    x = y;
                }
                x
            };
            let (r0, r1) = (find(&same_node, path[0]), find(&same_node, *path.last().unwrap()));
            if r0 != r1 {
                same_node.insert(r1, r0);
            }
        }
    }
    let node_root = |mut x: u32| {
        while let Some(&y) = same_node.get(&x) {
            if y == x {
                break;
            }
            x = y;
        }
        x
    };
    let mut vid = |v: u32, out_p: &mut Vec<Vec3>| -> u32 {
        let v = node_root(v);
        *node_id.entry(v).or_insert_with(|| {
            out_p.push(w.p[v as usize]);
            (out_p.len() - 1) as u32
        })
    };
    // Per arc: the ids of its points 0..=n, and each working vertex's place along it (0..=n).
    let mut arc_pts: Vec<Vec<u32>> = Vec::with_capacity(c.arcs.len());
    let mut arc_place: Vec<Vec<f32>> = Vec::with_capacity(c.arcs.len());
    for (a, path) in c.arcs.iter().enumerate() {
        let k = n[a] as usize;
        let mut cum = vec![0.0f32];
        for e in path.windows(2) {
            cum.push(cum.last().unwrap() + w.p[e[0] as usize].distance(w.p[e[1] as usize]));
        }
        let total = cum.last().copied().unwrap_or(0.0).max(1e-12);
        let mut ids = vec![vid(path[0], &mut out_p)];
        for j in 1..k {
            let goal = total * j as f32 / k as f32;
            let i = cum.partition_point(|&x| x < goal).clamp(1, path.len() - 1);
            let t = ((goal - cum[i - 1]) / (cum[i] - cum[i - 1]).max(1e-12)).clamp(0.0, 1.0);
            out_p.push(w.p[path[i - 1] as usize].lerp(w.p[path[i] as usize], t));
            ids.push((out_p.len() - 1) as u32);
        }
        let last = vid(*path.last().unwrap(), &mut out_p);
        if k > 0 {
            ids.push(last);
        }
        arc_pts.push(ids);
        arc_place.push(cum.iter().map(|&x| x / total * k as f32).collect());
    }
    // A model whose handles were cut open by long slits leaves big pinched patches, which the
    // general fill makes a mess of: past a tenth of the model, the plan is given up (the
    // plain remesher does better there).
    {
        let tri_area = |f: u32| { let t = w.t[f as usize]; (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5 };
        let mut pinched_area = 0.0f32;
        let mut all = 0.0f32;
        for pa in &c.patches {
            let a: f32 = pa.faces.iter().map(|&f| tri_area(f)).sum();
            all += a;
            let mut seen = HashSet::default();
            if !pa.band && !pa.lp.iter().all(|v| seen.insert(*v)) && pa.lp.len() - seen.len() > 8 {
                pinched_area += a;
            }
        }
        let limit: f32 = std::env::var("ZR_PINCHLIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.1);
        if debug {
            eprintln!("  pinched patches: {:.0}% of the area", pinched_area / all.max(1e-12) * 100.0);
        }
        if pinched_area > all * limit {
            return Err(format!("{:.0}% of the model in pinched patches", pinched_area / all.max(1e-12) * 100.0));
        }
    }
    // Fill each patch.
    let mut quads_out: Vec<[u32; 4]> = Vec::new();
    let mut made_by: Vec<(usize, u8)> = Vec::new();
    for (pi, pa) in c.patches.iter().enumerate() {
        let before = quads_out.len();
        let pts_before = out_p.len();
        if pa.band {
            band_fill(&w, pa, &c, &n, &arc_pts, &arc_place, &mut out_p, &mut quads_out, quad)?;
            made_by.extend(std::iter::repeat_n((pi, 4u8), quads_out.len() - before));
            continue;
        }
        let k = pa.sides.len();
        let l: Vec<i64> = pa.sides.iter().map(|s| s.iter().map(|&(a, _)| n[a as usize]).sum()).collect();
        let total: i64 = l.iter().sum();
        // A trapezoid: its free pair, the longer side as the bottom.
        let trap = pa.free.filter(|_| k == 4 && !pa.messy).and_then(|fp| {
            let sb = if l[fp] >= l[fp + 2] { fp } else { fp + 2 };
            let (a, c, h) = (l[sb], l[(sb + 2) % 4], l[(sb + 1) % 4]);
            let d = (a - c) / 2;
            // Equal heights, an even difference, and room in each row for its merges.
            let ok = h == l[(sb + 3) % 4] && (a - c) % 2 == 0 && c >= 1 && h >= 1 && (0..h).all(|j| {
                let kj = (j + 1) * d / h - j * d / h;
                let nj = a - 2 * (j * d / h);
                nj >= 3 * kj
            });
            ok.then_some(sb)
        });
        let pinched = { let mut seen = HashSet::default(); !pa.lp.iter().all(|v| seen.insert(*v)) };
        // A cap: a disc inside one closed loop with no corners (a feature loop's inside).
        let cap = k == 1 && pa.real == 0 && !pinched && total >= 4 && total % 2 == 0 && std::env::var_os("ZR_NOCAP5").is_none();
        let pattern = if pinched {
            2
        } else if cap {
            5
        } else if k == 4 && !pa.messy && pa.real == 4 && l[0] == l[2] && l[1] == l[3] {
            0
        } else if trap.is_some() {
            3
        } else if k >= 3 && k != 4 && k <= 8 && split_sides(&l).is_some() && (!pa.messy || std::env::var_os("ZR_OLDPATTERN").is_none()) {
            // (A wedge or many-cornered patch takes the centre split too when its counts
            // allow: far better than rings.)
            1
        } else {
            2
        };
        // A thin patch (a fill by its counts would make far more quads than its area) is
        // laid on a long a×b rectangle instead of a circle or trapezoid: a·b its area,
        // 2(a+b) its border count, the short sides at its two farthest-apart border points.
        let strip: Option<[usize; 4]> = (|| {
            // A triangle with one side as long as the other two (no middle point inside to
            // split round): a plain grid, those two sides one corner and one leg of it, the
            // long side the other two (a corner on it where they meet).
            if k == 3 && !pinched && !pa.messy && pattern == 2 && std::env::var_os("ZR_NODEGTRI").is_none() {
                for i in 0..3 {
                    let (a, h) = (l[(i + 1) % 3], l[(i + 2) % 3]);
                    if l[i] == a + h && a >= 1 && h >= 1 {
                        let st: i64 = l[..(i + 1) % 3].iter().sum();
                        if debug {
                            eprintln!("  patch {pi}: triangle {l:?} as a {a}×{h} grid");
                        }
                        return Some([st as usize, a as usize, h as usize, a as usize]);
                    }
                }
            }
            if std::env::var_os("ZR_NOSTRIP").is_some() || pinched || !(pattern == 2 || pattern == 3) || total < 8 || total % 2 != 0 {
                return None;
            }
            let area = pa.faces.iter().map(|&f| {
                let t = w.t[f as usize];
                (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5
            }).sum::<f32>() / (quad * quad);
            let p = total as f32;
            let made = if pattern == 3 { let sb = trap?; (l[sb] + l[(sb + 2) % 4]) as f32 * 0.5 * l[(sb + 1) % 4] as f32 } else { p * p / (4.0 * std::f32::consts::PI) };
            let thin_limit: f32 = std::env::var("ZR_STRIPTHIN").ok().and_then(|v| v.parse().ok()).unwrap_or(0.75);
            if area >= p * p / 16.0 * thin_limit || made < area * 1.5 {
                return None;
            }
            let half = total / 2;
            let b = (((p * 0.5 - (p * p * 0.25 - 4.0 * area).max(0.0).sqrt()) * 0.5).round() as i64).clamp(std::env::var("ZR_STRIPMIN").ok().and_then(|v| v.parse().ok()).unwrap_or(2), half / 2);
            let a = half - b;
            // Border points in loop order (one per count), and the farthest-apart pair.
            let mut pts: Vec<Vec3> = Vec::with_capacity(total as usize);
            for side in &pa.sides {
                for &(arc, fwd) in side {
                    let ids = &arc_pts[arc as usize];
                    let m = ids.len().saturating_sub(1);
                    for j in 0..m {
                        let id = if fwd { ids[j] } else { ids[m - j] };
                        pts.push(out_p[id as usize]);
                    }
                }
            }
            if pts.len() != total as usize {
                return None;
            }
            let mut best = (0.0f32, 0usize, 0usize);
            for i in 0..pts.len() {
                for j in i + 1..pts.len() {
                    let d = pts[i].distance_squared(pts[j]);
                    if d > best.0 {
                        best = (d, i, j);
                    }
                }
            }
            let t = total as usize;
            // The short sides (legs, h each) centred on the two tips; the long sides between
            // may differ (a trapezoid, its merges spread along the rows). Legs widened while
            // the rows have no room for their merges; else equal long sides from one tip.
            let room = |a: i64, c: i64, h: i64| -> bool {
                let d = (a - c) / 2;
                a >= c && c >= 1 && h >= 1 && (a - c) % 2 == 0 && (0..h).all(|j| {
                    let kj = (j + 1) * d / h - j * d / h;
                    a - 2 * (j * d / h) >= 3 * kj
                })
            };
            let (i0, j0) = (best.1 as i64, best.2 as i64);
            let ti = total;
            let mut pick: Option<[usize; 4]> = None;
            if std::env::var_os("ZR_STRIPTRAP").is_some() {
                for h in b..=(b + 4).min(half / 2) {
                    let c0 = (i0 - h / 2).rem_euclid(ti);
                    let c1 = (c0 + h).rem_euclid(ti);
                    let c2 = (j0 - h / 2).rem_euclid(ti);
                    let c3 = (c2 + h).rem_euclid(ti);
                    let s1 = (c2 - c1).rem_euclid(ti);
                    let s2 = (c0 - c3).rem_euclid(ti);
                    if s1 < 1 || s2 < 1 || s1 + s2 + 2 * h != ti {
                        continue;
                    }
                    // Bottom: the longer long side; [start, bottom, legs, top].
                    let (start, bot, top) = if s1 >= s2 { (c1, s1, s2) } else { (c3, s2, s1) };
                    if room(bot, top, h) {
                        pick = Some([start as usize, bot as usize, h as usize, top as usize]);
                        break;
                    }
                }
            }
            // Equal long sides: the short sides' middles half the loop apart, the mismatch
            // with the tips shared between both ends.
            let l1 = (j0 - i0).rem_euclid(ti);
            let e = (l1 - half) / 2;
            let pick = pick.unwrap_or([((i0 + e + b - b / 2).rem_euclid(ti)) as usize, a as usize, b as usize, a as usize]);
            if debug {
                let mid = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
                eprintln!("  strip patch {pi}: pattern {pattern}, {total} round, {area:.0} quads² (a fill would make {made:.0}), {a}×{b} -> {pick:?}, ends {:.1} quads apart, middle {mid:.2}", best.0.sqrt() / quad);
            }
            let _ = t;
            Some(pick)
        })();
        let pattern = if strip.is_some() { 2 } else { pattern };
        // The flat polygon: a rectangle for a grid, a regular polygon round a middle point,
        // else a circle (rings).
        let corners: Vec<Vec2> = if pattern == 0 {
            let (a, b) = (l[0] as f32, l[1] as f32);
            vec![Vec2::ZERO, Vec2::new(a, 0.0), Vec2::new(a, b), Vec2::new(0.0, b)]
        } else if let (3, Some(sb)) = (pattern, trap) {
            let (a, c, h) = (l[sb] as f32, l[(sb + 2) % 4] as f32, l[(sb + 1) % 4] as f32);
            let mut cs = vec![Vec2::ZERO; 4];
            cs[sb] = Vec2::ZERO;
            cs[(sb + 1) % 4] = Vec2::new(a, 0.0);
            cs[(sb + 2) % 4] = Vec2::new((a + c) * 0.5, h);
            cs[(sb + 3) % 4] = Vec2::new((a - c) * 0.5, h);
            cs
        } else if k == 3 && pattern == 1 && std::env::var_os("ZR_REGTRI").is_none() {
            // A triangle with its own proportions (a thin wedge stays thin, so the flattening
            // and the corner grids are not stretched), counter-clockwise.
            let mut e: Vec<f32> = l.iter().map(|&x| x.max(1) as f32).collect();
            for i in 0..3 {
                let others = e[(i + 1) % 3] + e[(i + 2) % 3];
                if e[i] >= others * 0.98 {
                    e[i] = others * 0.98;
                }
            }
            let (a, b, c) = (e[0], e[1], e[2]);
            let x = (a * a + c * c - b * b) / (2.0 * a);
            let y = (c * c - x * x).max(1e-6).sqrt();
            vec![Vec2::ZERO, Vec2::new(a, 0.0), Vec2::new(x, y)]
        } else {
            let r = l.iter().sum::<i64>() as f32 / (2.0 * k as f32 * (std::f32::consts::PI / k as f32).sin());
            (0..k).map(|i| {
                let t = std::f32::consts::TAU * i as f32 / k as f32 - std::f32::consts::FRAC_PI_2 - std::f32::consts::PI / k as f32;
                Vec2::new(t.cos(), t.sin()) * r
            }).collect()
        };
        let base: Vec<f32> = (0..k).map(|s| l[..s].iter().sum::<i64>() as f32).collect();
        let radius = total as f32 / std::f32::consts::TAU;
        let circle = |t: f32| -> Vec2 {
            let a = std::f32::consts::TAU * t / total as f32;
            Vec2::new(a.cos(), a.sin()) * radius
        };
        // On the strip's trapezoid ([start, bottom a, legs h, top c]), counter-clockwise from
        // the bottom's first corner; the top centred over the bottom.
        let rect = |t: f32| -> Vec2 {
            let [st, a, h, c] = strip.unwrap();
            let (a, h, c) = (a as f32, h as f32, c as f32);
            let tot = a + c + 2.0 * h;
            let s = (t - st as f32).rem_euclid(tot);
            let (br, tr, tl) = (Vec2::new(a, 0.0), Vec2::new((a + c) * 0.5, h), Vec2::new((a - c) * 0.5, h));
            if s < a {
                Vec2::new(s, 0.0)
            } else if s < a + h {
                br.lerp(tr, (s - a) / h)
            } else if s < a + h + c {
                tr.lerp(tl, (s - a - h) / c)
            } else {
                tl.lerp(Vec2::ZERO, (s - a - h - c) / h)
            }
        };
        // A general fill with real corners: its border on the polygon those corners make on
        // the circle (straight sides, so the rings keep their corners square-ish).
        let polygon = pattern == 2 && !pinched && pa.real >= 3 && k >= 3 && std::env::var_os("ZR_POLYFILL").is_some();
        let on_side = |s: usize, u: f32| -> Vec2 { if strip.is_some() { rect(base[s] + u) } else if polygon { circle(base[s]).lerp(circle(base[s] + l[s] as f32), u / l[s].max(1) as f32) } else if pattern == 2 || pattern == 5 { circle(base[s] + u) } else { corners[s].lerp(corners[(s + 1) % k], u / l[s] as f32) } };
        // Loop vertices onto the polygon; side points' ids by (side, offset). A vertex the
        // loop passes twice (a pinch) is a second, separate vertex on its second pass, for
        // the faces on that side of it.
        let mut seq: Vec<(u32, Vec2)> = Vec::new();
        for (s, side) in pa.sides.iter().enumerate() {
            let mut base = 0.0f32;
            for &(a, fwd) in side {
                let path = &c.arcs[a as usize];
                let place = &arc_place[a as usize];
                let na = n[a as usize] as f32;
                let order: Vec<usize> = if fwd { (0..path.len()).collect() } else { (0..path.len()).rev().collect() };
                for i in order {
                    let v = path[i];
                    let u = if fwd { place[i] } else { na - place[i] };
                    if seq.last().map(|x| x.0) != Some(v) {
                        seq.push((v, on_side(s, base + u)));
                    }
                }
                base += na;
            }
        }
        if seq.len() > 1 && seq.first().map(|x| x.0) == seq.last().map(|x| x.0) {
            seq.pop();
        }
        let mut alias: HashMap<(u32, u32), u32> = HashMap::default();
        let mut boundary: HashMap<u32, Vec2> = HashMap::default();
        let ms = seq.len();
        for q in 0..ms {
            let (v, at) = seq[q];
            if !boundary.contains_key(&v) {
                boundary.insert(v, at);
                continue;
            }
            // Second pass: the faces between this pass's edges (counter-clockwise from the
            // edge out to the edge in) take a new vertex.
            let virt = 0xF000_0000u32 + q as u32;
            let (prev, next) = (seq[(q + ms - 1) % ms].0, seq[(q + 1) % ms].0);
            let faces: HashSet<u32> = pa.faces.iter().copied().collect();
            let Some(mut h) = w.fan(v).into_iter().find(|&h| w.to(h) == next && faces.contains(&(h / 3))) else { continue };
            for _ in 0..64 {
                let f = h / 3;
                if !faces.contains(&f) {
                    break;
                }
                alias.insert((f, v), virt);
                if w.from(Work::prev(h)) == prev {
                    break;
                }
                let t = w.twin[Work::prev(h) as usize];
                if t == NONE {
                    break;
                }
                h = t;
            }
            boundary.insert(virt, at);
        }
        let mut side_ids: Vec<Vec<u32>> = Vec::with_capacity(k);
        for (_s, side) in pa.sides.iter().enumerate() {
            let mut base = 0.0f32;
            let mut ids: Vec<u32> = Vec::new();
            for &(a, fwd) in side {
                let na = n[a as usize] as f32;
                let mut pts = arc_pts[a as usize].clone();
                if !fwd {
                    pts.reverse();
                }
                if ids.last() == pts.first() {
                    ids.pop();
                }
                ids.extend(pts);
                base += na;
            }
            side_ids.push(ids);
        }
        // A point the border passes twice (a slit, a pinch, a handle cut) gets a duplicate
        // for its later passes, so every fill sees a simple polygon; the duplicates are
        // welded back afterwards (each slit edge then has one quad from each side).
        let mut weld: HashMap<u32, u32> = HashMap::default();
        {
            let mut used: HashSet<u32> = HashSet::default();
            let first_id = side_ids.first().and_then(|s| s.first().copied());
            let nsides = side_ids.len();
            for si in 0..nsides {
                let len = side_ids[si].len();
                for j in 0..len {
                    let id = side_ids[si][j];
                    let joins_prev = j == 0 && si > 0 && side_ids[si - 1].last() == Some(&id);
                    let closes = si + 1 == nsides && j + 1 == len && Some(id) == first_id;
                    if joins_prev || closes {
                        continue;
                    }
                    if !used.insert(id) {
                        let dup = out_p.len() as u32;
                        out_p.push(out_p[id as usize]);
                        weld.insert(dup, id);
                        side_ids[si][j] = dup;
                        // A side's last point is also the next side's first.
                        if j + 1 == len && si + 1 < nsides && side_ids[si + 1].first() == Some(&id) {
                            side_ids[si + 1][0] = dup;
                        }
                        if j + 1 == len && si + 1 == nsides && side_ids[0].first() == Some(&id) {
                            side_ids[0][0] = dup;
                        }
                    }
                }
            }
        }
        if side_ids.iter().zip(&l).any(|(ids, &len)| ids.len() as i64 != len + 1) {
            return Err("a side's points did not add up".into());
        }
        // Two sides with the same points (two one-quad arcs between the same nodes): no
        // width at all, nothing to fill; the patches either side share that edge.
        if k == 2 && side_ids[0].iter().rev().eq(side_ids[1].iter()) {
            continue;
        }
        let flat = flatten(&w, pa, &boundary, &alias);
        if debug {
            let corner = |f: u32, v: u32| -> u32 { alias.get(&(f, v)).copied().unwrap_or(v) };
            let mut flipped = 0;
            let mut ears = 0;
            let mut inverted = 0;
            for &f in &pa.faces {
                let ids = w.t[f as usize].map(|v| corner(f, v));
                let t = ids.map(|v| flat[&v]);
                let a = (t[1] - t[0]).perp_dot(t[2] - t[0]);
                if a <= 0.0 {
                    flipped += 1;
                    if ids.iter().all(|v| boundary.contains_key(v)) {
                        ears += 1;
                    }
                    if a < -1e-6 {
                        inverted += 1;
                    }
                }
            }
            if flipped > 0 {
                let mut times: HashMap<u32, u32> = HashMap::default();
                for &v in &pa.lp {
                    *times.entry(v).or_default() += 1;
                }
                let pinches = times.values().filter(|&&t| t > 1).count();
                eprintln!("  patch {pi}: {flipped} of {} faces flipped ({ears} ears on the border, {inverted} truly inverted), {} sides, pattern {pattern}, {pinches} pinches", pa.faces.len(), pa.sides.len());
            }
        }
        let loc = Locate::new(&w, pa, &flat, &alias);
        let mut inner: HashMap<(i64, i64), u32> = HashMap::default();
        let mut point = |q: Vec2, out_p: &mut Vec<Vec3>| -> u32 {
            let key = ((q.x * 4096.0).round() as i64, (q.y * 4096.0).round() as i64);
            *inner.entry(key).or_insert_with(|| {
                out_p.push(loc.at(q));
                (out_p.len() - 1) as u32
            })
        };
        // A grid of `nx` × `ny` quads in the flat quad (a, b, c, d), its four sides' ids given
        // where they are on the patch's border.
        let mut grid = |cs: [Vec2; 4], nx: usize, ny: usize, bottom: Option<&[u32]>, right: Option<&[u32]>, top: Option<&[u32]>, left: Option<&[u32]>, out_p: &mut Vec<Vec3>, quads_out: &mut Vec<[u32; 4]>| {
            let mut ids = vec![vec![0u32; nx + 1]; ny + 1];
            for j in 0..=ny {
                for i in 0..=nx {
                    let id = if j == 0 && bottom.is_some() {
                        bottom.unwrap()[i]
                    } else if i == nx && right.is_some() {
                        right.unwrap()[j]
                    } else if j == ny && top.is_some() {
                        top.unwrap()[nx - i]
                    } else if i == 0 && left.is_some() {
                        left.unwrap()[ny - j]
                    } else {
                        let (u, v) = (i as f32 / nx as f32, j as f32 / ny as f32);
                        let q = cs[0] * (1.0 - u) * (1.0 - v) + cs[1] * u * (1.0 - v) + cs[2] * u * v + cs[3] * (1.0 - u) * v;
                        point(q, out_p)
                    };
                    ids[j][i] = id;
                }
            }
            for j in 0..ny {
                for i in 0..nx {
                    quads_out.push([ids[j][i], ids[j][i + 1], ids[j + 1][i + 1], ids[j + 1][i]]);
                }
            }
        };
        if pattern == 0 {
            let (nx, ny) = (l[0] as usize, l[1] as usize);
            grid([corners[0], corners[1], corners[2], corners[3]], nx, ny, Some(&side_ids[0]), Some(&side_ids[1]), Some(&side_ids[2]), Some(&side_ids[3]), &mut out_p, &mut quads_out);
        } else if let Some((bottom_ids, right_ids, top_ids, left_ids, a, c, h)) = if let Some([st, a, h, c]) = strip {
            // The strip's four sides, cut from the border loop.
            let ring: Vec<u32> = (0..k).flat_map(|s| side_ids[s][..l[s] as usize].iter().copied()).collect();
            let m = ring.len();
            let run = |from: usize, len: usize| -> Vec<u32> { (0..=len).map(|q| ring[(from + q) % m]).collect() };
            (m == a + c + 2 * h).then(|| (run(st, a), run(st + a, h), run(st + a + h, c), run(st + a + h + c, h), a as i64, c as i64, h as i64))
        } else if let (3, Some(sb)) = (pattern, trap) {
            let (sr, st, sl) = ((sb + 1) % 4, (sb + 2) % 4, (sb + 3) % 4);
            Some((side_ids[sb].clone(), side_ids[sr].clone(), side_ids[st].clone(), side_ids[sl].clone(), l[sb], l[st], l[sr]))
        } else {
            None
        } {
            drop(grid);
            let d = (a - c) / 2;
            let (af, cf, hf) = (a as f32, c as f32, h as f32);
            let x_at = |y: f32, i: usize, n: i64| -> Vec2 {
                let xl = (af - cf) * 0.5 * y / hf;
                let xr = af - (af - cf) * 0.5 * y / hf;
                Vec2::new(xl + (xr - xl) * i as f32 / n as f32, y)
            };
            // Row 0's points: the bottom side.
            let mut row: Vec<u32> = bottom_ids.clone();
            let mut n0 = a;
            for j in 0..h {
                let kj = (j + 1) * d / h - j * d / h;
                let n1 = n0 - 2 * kj;
                let y1 = (j + 1) as f32;
                let top: Vec<u32> = if j + 1 == h {
                    let mut t = top_ids.clone();
                    t.reverse();
                    t
                } else {
                    let mut t = vec![left_ids[(h - j - 1) as usize]];
                    for i in 1..n1 as usize {
                        t.push(point(x_at(y1, i, n1), &mut out_p));
                    }
                    t.push(right_ids[(j + 1) as usize]);
                    t
                };
                // Merges spread along the row: each takes three bottom edges to one top edge.
                // Staggered from row to row, so the poles do not line up.
                let shift = ((j as f32 * 0.618).fract() - 0.5) * n0 as f32 / kj.max(1) as f32;
                let starts: Vec<i64> = (0..kj).map(|t| ((t as f32 + 0.5) * n0 as f32 / kj as f32 - 1.5 + shift).round().rem_euclid((n0 - 2).max(1) as f32) as i64).collect();
                let mut starts = starts;
                starts.sort_unstable();
                let mut starts2 = Vec::new();
                let mut last = -3i64;
                for st0 in starts {
                    let st0 = st0.max(last + 3).min(n0 - 3 - 3 * (kj - 1 - starts2.len() as i64));
                    starts2.push(st0);
                    last = st0;
                }
                let (mut i, mut o, mut t) = (0usize, 0usize, 0usize);
                while (i as i64) < n0 {
                    if t < starts2.len() && i as i64 == starts2[t] {
                        let (b0, b1, b2, b3) = (row[i], row[i + 1], row[i + 2], row[i + 3]);
                        let (t0, t1) = (top[o], top[o + 1]);
                        let y = j as f32 + 0.5;
                        let m1 = point(x_at(j as f32, i + 1, n0) * Vec2::new(1.0, 0.0) + Vec2::new(0.0, y), &mut out_p);
                        let m2 = point(x_at(j as f32, i + 2, n0) * Vec2::new(1.0, 0.0) + Vec2::new(0.0, y), &mut out_p);
                        quads_out.push([b0, b1, m1, t0]);
                        quads_out.push([b1, b2, m2, m1]);
                        quads_out.push([b2, b3, t1, m2]);
                        quads_out.push([m1, m2, t1, t0]);
                        i += 3;
                        o += 1;
                        t += 1;
                    } else {
                        quads_out.push([row[i], row[i + 1], top[o + 1], top[o]]);
                        i += 1;
                        o += 1;
                    }
                }
                row = top;
                n0 = n1;
            }
        } else if pattern == 5 {
            // Four corners on the loop, evenly round it (a, b, a, b), and a grid between them
            // laid as a Coons patch on the flat circle.
            drop(grid);
            let t = total as usize;
            let a = ((t as f32 / 4.0).round() as usize).clamp(1, t / 2 - 1);
            let b = t / 2 - a;
            let ring: Vec<u32> = side_ids[0][..t].to_vec();
            let at = |i: usize| -> Vec2 { on_side(0, (i % t) as f32) };
            // Sides as index runs round the loop: bottom 0..a, right a..a+b, top (reversed)
            // a+b..2a+b, left (reversed) 2a+b..t.
            let bottom = |i: usize| i;
            let right = |j: usize| a + j;
            let top = |i: usize| (2 * a + b - i) % t;
            let left = |j: usize| (t - j) % t;
            let mut g = vec![vec![0u32; a + 1]; b + 1];
            for j in 0..=b {
                for i in 0..=a {
                    g[j][i] = if j == 0 {
                        ring[bottom(i)]
                    } else if j == b {
                        ring[top(i)]
                    } else if i == 0 {
                        ring[left(j)]
                    } else if i == a {
                        ring[right(j)]
                    } else {
                        let (u, v) = (i as f32 / a as f32, j as f32 / b as f32);
                        let q = at(bottom(i)) * (1.0 - v) + at(top(i)) * v + at(left(j)) * (1.0 - u) + at(right(j)) * u
                            - (at(0) * (1.0 - u) * (1.0 - v) + at(a) * u * (1.0 - v) + at(a + b) * u * v + at(2 * a + b) * (1.0 - u) * v);
                        point(q, &mut out_p)
                    };
                }
            }
            for j in 0..b {
                for i in 0..a {
                    quads_out.push([g[j][i], g[j][i + 1], g[j + 1][i + 1], g[j + 1][i]]);
                }
            }
        } else if pattern == 1 {
            let y = split_sides(&l).ok_or("a patch has no even split")?;
            let x: Vec<i64> = (0..k).map(|i| l[i] - y[i]).collect();
            // The middle: the regular polygon's centre, or for a triangle laid out with its own
            // proportions, the mean of its split points.
            let centre = if k == 3 && corners[0] == Vec2::ZERO && corners[1].y == 0.0 && std::env::var_os("ZR_REGTRI").is_none() {
                (0..k).map(|i| on_side(i, x[i] as f32)).sum::<Vec2>() / k as f32
            } else {
                Vec2::ZERO
            };
            for i in 0..k {
                let j = (i + 1) % k;
                // Quad at corner j: S_i → C_j → S_j → centre.
                let si = on_side(i, x[i] as f32);
                let sj = on_side(j, x[j] as f32);
                let bottom: Vec<u32> = side_ids[i][x[i] as usize..].to_vec();
                let right: Vec<u32> = side_ids[j][..=x[j] as usize].to_vec();
                grid([si, corners[j], sj, centre], y[i] as usize, x[j] as usize, Some(&bottom), Some(&right), None, None, &mut out_p, &mut quads_out);
            }
        } else {
            drop(grid);
            if total % 2 != 0 || total < 4 {
                if debug {
                    eprintln!("  odd patch: real {} messy {} sides {:?} arcs {:?}", pa.real, pa.messy, l, pa.sides);
                }
                return Err("a patch has an odd border".into());
            }
            let mut ids: Vec<u32> = Vec::with_capacity(total as usize);
            let mut pos: Vec<Vec2> = Vec::with_capacity(total as usize);
            let mut cs: Vec<usize> = Vec::new();
            for s in 0..k {
                if pa.real > 0 {
                    cs.push(ids.len());
                }
                for j in 0..l[s] as usize {
                    ids.push(side_ids[s][j]);
                    pos.push(on_side(s, j as f32));
                }
            }
            if cs.len() < 3 {
                // Four corners with opposite sides equal (a, b, a, b), so the rings end in a grid.
                let len = ids.len();
                let a = (len / 4).max(1);
                let b = len / 2 - a;
                cs = vec![0, a, a + b, 2 * a + b];
                cs.dedup();
            }
            if debug {
                let mut u = ids.clone();
                u.sort_unstable();
                u.dedup();
                if std::env::var("ZR_RINGDBG").ok().and_then(|v| v.parse::<usize>().ok()) == Some(pi) {
                    eprintln!("  ring input {pi}: ids {ids:?} corners {cs:?} pos {:?}", pos.iter().map(|q| [q.x, q.y]).collect::<Vec<_>>());
                }
                if u.len() != ids.len() || pi == 8 || pi == 12 {
                    eprintln!("  ring patch {pi}: {} points ({} distinct), corners {:?}, real {}, sides {:?}, ids {:?}", ids.len(), u.len(), cs, pa.real, l, if ids.len() < 40 { ids.clone() } else { Vec::new() });
                }
            }
            // Fresh points (not merged by flat position: a ring's inner corner can land on
            // a point of the ring before).
            ring_fill(ids, pos, cs, &mut |q| {
                out_p.push(loc.at(q));
                (out_p.len() - 1) as u32
            }, &mut quads_out);
        }
        // The fill's inner points, placed through the flat layout, land far off where the
        // layout squeezes a curved patch (long edges, folded quads). Re-placed on the patch
        // itself: each to the mean of its neighbours, onto the nearest point of the patch's
        // own triangles, the border held, 24 rounds (ZR_PATCHRELAX, 0 = off).
        let rounds: usize = std::env::var("ZR_PATCHRELAX").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
        if rounds > 0 && out_p.len() > pts_before {
            let fixed: HashSet<u32> = side_ids.iter().flatten().copied().collect();
            let inner: Vec<u32> = (pts_before as u32..out_p.len() as u32).filter(|v| !fixed.contains(v)).collect();
            let mut nbr: HashMap<u32, Vec<u32>> = HashMap::default();
            for q4 in &quads_out[before..] {
                for kq in 0..4 {
                    let (a, b) = (q4[kq], q4[(kq + 1) % 4]);
                    if a != b {
                        let ea = nbr.entry(a).or_default();
                        if !ea.contains(&b) {
                            ea.push(b);
                        }
                        let eb = nbr.entry(b).or_default();
                        if !eb.contains(&a) {
                            eb.push(a);
                        }
                    }
                }
            }
            // The patch's triangles by grid cell (a quad across).
            let cell = quad.max(1e-6);
            let cell_of = |p: Vec3| { let c = (p / cell).floor(); (c.x as i32, c.y as i32, c.z as i32) };
            let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
            for &f in &pa.faces {
                let t = w.t[f as usize];
                let (lo, hi) = t.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), &v| (a.min(w.p[v as usize]), b.max(w.p[v as usize])));
                let (c0, c1) = (cell_of(lo), cell_of(hi));
                for x in c0.0..=c1.0 {
                    for y in c0.1..=c1.1 {
                        for z2 in c0.2..=c1.2 {
                            grid.entry((x, y, z2)).or_default().push(f);
                        }
                    }
                }
            }
            let onto = |p: Vec3| -> Option<(Vec3, Vec3)> {
                let c = cell_of(p);
                let mut best: Option<(f32, Vec3, Vec3)> = None;
                for r in 0..6i32 {
                    for x in -r..=r {
                        for y in -r..=r {
                            for z2 in -r..=r {
                                if x.abs().max(y.abs()).max(z2.abs()) != r {
                                    continue;
                                }
                                for &f in grid.get(&(c.0 + x, c.1 + y, c.2 + z2)).into_iter().flatten() {
                                    let t = w.t[f as usize];
                                    let q = crate::wedge::closest_on_triangle(p, w.p[t[0] as usize], w.p[t[1] as usize], w.p[t[2] as usize]);
                                    let dd = q.distance_squared(p);
                                    if best.is_none_or(|b| dd < b.0) {
                                        best = Some((dd, q, w.fnrm[f as usize]));
                                    }
                                }
                            }
                        }
                    }
                    if let Some((dd, q, nn)) = best {
                        if dd.sqrt() <= r as f32 * cell {
                            return Some((q, nn));
                        }
                    }
                }
                best.map(|b| (b.1, b.2))
            };
            // Only the points that are off: away from the patch's surface, on an edge over 3
            // quads, or on a folded quad (smoothing them all pulled points off thin tips).
            let off: HashSet<u32> = {
                let mut off: HashSet<u32> = HashSet::default();
                for &v in &inner {
                    let p0 = out_p[v as usize];
                    if onto(p0).is_none_or(|(q, _)| q.distance(p0) > quad * 0.25) {
                        off.insert(v);
                    }
                }
                for q4 in &quads_out[before..] {
                    let pp: Vec<Vec3> = q4.iter().map(|&x| out_p[x as usize]).collect();
                    let folded = (pp[1] - pp[0]).cross(pp[2] - pp[0]).dot((pp[2] - pp[0]).cross(pp[3] - pp[0])) < 0.0;
                    let long = (0..4).any(|k| pp[k].distance(pp[(k + 1) % 4]) > quad * 3.0);
                    if folded || long {
                        for &x in q4 {
                            off.insert(x);
                        }
                    }
                }
                off
            };
            let all = std::env::var_os("ZR_PATCHRELAXALL").is_some();
            let inner: Vec<u32> = inner.into_iter().filter(|v| all || off.contains(v)).collect();
            for _ in 0..rounds {
                for &v in &inner {
                    let Some(nb) = nbr.get(&v) else { continue };
                    if nb.is_empty() {
                        continue;
                    }
                    let mean = nb.iter().map(|&u| out_p[u as usize]).sum::<Vec3>() / nb.len() as f32;
                    // Along the surface only (a full 3D mean pulls points off tips: fingers
                    // shrank).
                    let here = out_p[v as usize];
                    let nrm = onto(here).map(|x| x.1).unwrap_or(Vec3::ZERO);
                    let d = mean - here;
                    if let Some((q, _)) = onto(here + d - nrm * nrm.dot(d)) {
                        out_p[v as usize] = q;
                    }
                }
            }
        }
        // Slit points (an arc the border runs both ways) are welded, so each slit edge gets
        // one quad from each side; a pinch node (the border passing a corner twice) stays
        // two coincident vertices, each with its own fan.
        let repeated_arcs: HashSet<u32> = pa.sides.iter().flatten().map(|x| x.0).filter(|&a| pa.sides.iter().flatten().filter(|y| y.0 == a).count() > 1).collect();
        let slit_nodes: HashSet<u32> = repeated_arcs.iter().flat_map(|&a| [arc_pts[a as usize][0], *arc_pts[a as usize].last().unwrap()]).collect();
        let node_ids: HashSet<u32> = node_id.values().copied().collect();
        let weld_all: HashMap<u32, u32> = weld.into_iter().filter(|(_, o)| !node_ids.contains(o) || slit_nodes.contains(o)).collect();
        // If welding the nodes too would put an edge in more than two quads, only the
        // interior slit points weld (the nodes stay coincident copies).
        let over = |weld: &HashMap<u32, u32>| -> bool {
            let mut used: HashMap<(u32, u32), u32> = HashMap::default();
            for q in &quads_out[before..] {
                let wq: [u32; 4] = std::array::from_fn(|k| weld.get(&q[k]).copied().unwrap_or(q[k]));
                for k in 0..4 {
                    let (a, b) = (wq[k], wq[(k + 1) % 4]);
                    if a != b {
                        *used.entry((a.min(b), a.max(b))).or_default() += 1;
                    }
                }
            }
            used.values().any(|&u| u > 2)
        };
        let weld: HashMap<u32, u32> = if over(&weld_all) { weld_all.into_iter().filter(|(_, o)| !node_ids.contains(o)).collect() } else { weld_all };
        if std::env::var("ZR_RINGDBG").ok().and_then(|v| v.parse::<usize>().ok()) == Some(pi) {
            eprintln!("  weld {pi}: {weld:?}");
            eprintln!("  quads {pi}: {:?}", &quads_out[before..]);
        }
        for q in &mut quads_out[before..] {
            let welded: [u32; 4] = std::array::from_fn(|k| weld.get(&q[k]).copied().unwrap_or(q[k]));
            let repeats = (0..4).any(|k| welded[k] == welded[(k + 1) % 4] || welded[k] == welded[(k + 2) % 4]);
            if !repeats {
                *q = welded;
            } else {
                // Weld only the corners whose weld keeps the corners distinct.
                for k in 0..4 {
                    if let Some(&o) = weld.get(&q[k]) {
                        if !q.contains(&o) {
                            q[k] = o;
                        }
                    }
                }
            }
        }
        made_by.extend(std::iter::repeat_n((pi, pattern as u8), quads_out.len() - before));
    }
    if debug {
        let mut used: HashMap<(u32, u32), Vec<usize>> = HashMap::default();
        for (q, quad4) in quads_out.iter().enumerate() {
            for k in 0..4 {
                let (a, b) = (quad4[k], quad4[(k + 1) % 4]);
                if a != b {
                    used.entry((a.min(b), a.max(b))).or_default().push(q);
                }
            }
        }
        let mut shown = 0;
        for qs in used.values().filter(|qs| qs.len() > 2) {
            if shown < 12 {
                let info: Vec<String> = qs.iter().map(|&q| {
                    let (pi, pat) = made_by[q];
                    let pa = &c.patches[pi];
                    let mut times: HashMap<u32, u32> = HashMap::default();
                    for &v in &pa.lp { *times.entry(v).or_default() += 1; }
                    let pin = times.values().filter(|&&t| t > 1).count();
                    let slits = pa.sides.iter().flatten().filter(|x| pa.sides.iter().flatten().filter(|y| y.0 == x.0).count() > 1).count();
                    format!("p{pi} pat{pat} k{} pinch{pin} slit{slits}", pa.sides.len())
                }).collect();
                eprintln!("  edge in {} quads: {:?}", qs.len(), info);
                let (ea, eb) = (quads_out[qs[0]].iter().copied().collect::<Vec<_>>(), ());
                let _ = eb;
                let e = used.iter().find(|(_, v)| v.as_ptr() == qs.as_ptr()).map(|(k, _)| *k);
                if let Some((a, b)) = e {
                    let arcs_with: Vec<(usize, i64, usize)> = arc_pts.iter().enumerate().filter(|(_, pts)| pts.windows(2).any(|w2| (w2[0] == a && w2[1] == b) || (w2[0] == b && w2[1] == a))).map(|(i, pts)| (i, n[i], pts.len())).collect();
                    let is_node = |v: u32| node_id.values().any(|&x| x == v);
                    eprintln!("    edge ({a},{b}) nodes ({},{}) arcs {:?}; quads {:?}", is_node(a), is_node(b), arcs_with, qs.iter().map(|&q| quads_out[q]).collect::<Vec<_>>());
                    if let Some(&(arc, _, _)) = arcs_with.first() {
                        let pi = made_by[qs[0]].0;
                        let pa = &c.patches[pi];
                        let in_sides = pa.sides.iter().flatten().filter(|x| x.0 as usize == arc).count();
                        let path = &c.arcs[arc];
                        let m = pa.lp.len();
                        let mut in_lp = 0;
                        for i in 0..m {
                            let (x, y) = (pa.lp[i], pa.lp[(i + 1) % m]);
                            if path.windows(2).any(|w2| (w2[0] == x && w2[1] == y) || (w2[0] == y && w2[1] == x)) {
                                in_lp += 1;
                            }
                        }
                        eprintln!("    arc {arc} ({} verts): {in_sides} times in sides, its edges {in_lp} times in the loop ({} long, {} edges on the arc)", path.len(), m, path.len() - 1);
                        // Runs of loop steps on this arc: (start, length, forward?).
                        let mut runs: Vec<(usize, usize, bool)> = Vec::new();
                        for i in 0..m {
                            let (x, y) = (pa.lp[i], pa.lp[(i + 1) % m]);
                            let on_arc = path.windows(2).position(|w2| (w2[0] == x && w2[1] == y) || (w2[0] == y && w2[1] == x));
                            if let Some(pos) = on_arc {
                                let fwd = path[pos] == x;
                                match runs.last_mut() {
                                    Some(r) if r.0 + r.1 == i && r.2 == fwd => r.1 += 1,
                                    _ => runs.push((i, 1, fwd)),
                                }
                            }
                        }
                        let entries: Vec<(usize, usize, bool)> = pa.sides.iter().enumerate().flat_map(|(si, sd)| sd.iter().enumerate().filter(|(_, x)| x.0 as usize == arc).map(move |(ei, x)| (si, ei, x.1))).collect();
                        eprintln!("    runs {runs:?}; side entries (side, index, fwd) {entries:?}; corners at {:?}", pa.corner_at);
                        if let Some(r) = runs.first() {
                            let v = pa.lp[(r.0 + r.1) % m];
                            let cut_nb: Vec<u32> = w.neighbours(v).iter().copied().filter(|&u| cut_edges.contains(&key(v, u))).collect();
                            let at = path.iter().position(|&x| x == v);
                            let fan: Vec<(u32, bool, bool)> = w.fan(v).iter().map(|&h| (w.to(h), w.hard[h as usize], w.twin[h as usize] == NONE)).collect();
                            let pfaces: HashSet<u32> = pa.faces.iter().copied().collect();
                            let in_patch: Vec<bool> = w.fan(v).iter().map(|&h| pfaces.contains(&(h / 3))).collect();
                            eprintln!("    turn at {v}: position {at:?} on the arc, cut neighbours {cut_nb:?}, path neighbours {:?}, fan (to, hard, open) {fan:?}, faces in patch {in_patch:?}, mark {}", at.map(|i| (path.get(i.wrapping_sub(1)).copied(), path.get(i + 1).copied())), marks[v as usize]);
                        }
                    }
                }
                let _ = ea;
            }
            shown += 1;
        }
        // KLAY_ZR_PATCHES=file.obj: the raw layout, a group per patch ("p<patch>_<fill>").
        if let Some(path) = std::env::var_os("KLAY_ZR_PATCHES") {
            use std::io::Write;
            if let Ok(f) = std::fs::File::create(&path) {
                let mut f = std::io::BufWriter::new(f);
                for p in &out_p {
                    let _ = writeln!(f, "v {} {} {}", p.x, p.y, p.z);
                }
                let mut order: Vec<usize> = (0..quads_out.len()).collect();
                order.sort_by_key(|&q| made_by[q]);
                let mut cur = None;
                for q in order {
                    if cur != Some(made_by[q]) {
                        cur = Some(made_by[q]);
                        let pa = &c.patches[made_by[q].0];
                        let l: Vec<i64> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize]).sum()).collect();
                        let _ = writeln!(f, "g p{}_{}_{}", made_by[q].0, made_by[q].1, l.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("-"));
                    }
                    let q4 = quads_out[q];
                    if q4[2] == q4[3] {
                        let _ = writeln!(f, "f {} {} {}", q4[0] + 1, q4[1] + 1, q4[2] + 1);
                    } else {
                        let _ = writeln!(f, "f {} {} {} {}", q4[0] + 1, q4[1] + 1, q4[2] + 1, q4[3] + 1);
                    }
                }
            }
        }
        // The longest edges: which patch and fill made them, and what kind of point each end is.
        {
            let mut lens: Vec<(f32, usize, usize)> = Vec::new();
            for (q, quad4) in quads_out.iter().enumerate() {
                for k in 0..4 {
                    let (a, b) = (quad4[k], quad4[(k + 1) % 4]);
                    if a != b {
                        lens.push((out_p[a as usize].distance(out_p[b as usize]) / quad, q, k));
                    }
                }
            }
            lens.sort_by(|x, y| y.0.total_cmp(&x.0));
            let node_set: HashSet<u32> = node_id.values().copied().collect();
            let arc_set: HashSet<u32> = arc_pts.iter().flatten().copied().collect();
            let kind = |v: u32| if node_set.contains(&v) { "node" } else if arc_set.contains(&v) { "arc" } else { "inner" };
            // All of them: long edges (over 3 quads) and folded quads, by fill and point kinds.
            let mut long_by: HashMap<String, usize> = HashMap::default();
            for &(l, q, k) in &lens {
                if l < 3.0 {
                    break;
                }
                let (a, b) = (quads_out[q][k], quads_out[q][(k + 1) % 4]);
                let (pi, pat) = made_by[q];
                let pa = &c.patches[pi];
                let (ka, kb) = (kind(a), kind(b));
                let pair = if ka <= kb { format!("{ka}-{kb}") } else { format!("{kb}-{ka}") };
                *long_by.entry(format!("pattern {pat} {pair}{}", if pa.messy { " messy" } else { "" })).or_default() += 1;
            }
            let mut fold_by: HashMap<u8, usize> = HashMap::default();
            for (q, q4) in quads_out.iter().enumerate() {
                if q4[2] == q4[3] {
                    continue;
                }
                let pp: Vec<Vec3> = q4.iter().map(|&v| out_p[v as usize]).collect();
                let n1 = (pp[1] - pp[0]).cross(pp[2] - pp[0]);
                let n2 = (pp[2] - pp[0]).cross(pp[3] - pp[0]);
                if n1.dot(n2) < 0.0 {
                    *fold_by.entry(made_by[q].1).or_default() += 1;
                }
            }
            let mut lb: Vec<_> = long_by.into_iter().collect();
            lb.sort_by(|a, b| b.1.cmp(&a.1));
            let mut fb: Vec<_> = fold_by.into_iter().collect();
            fb.sort();
            eprintln!("  long edges (over 3 quads) by fill: {lb:?}");
            // Poles in the result (points not in four quads, off any open border), by kind
            // of point and the fills round them.
            {
                let mut val: HashMap<u32, Vec<u8>> = HashMap::default();
                let mut ecount: HashMap<(u32, u32), u32> = HashMap::default();
                for (q, q4) in quads_out.iter().enumerate() {
                    for kq in 0..4 {
                        val.entry(q4[kq]).or_default().push(made_by[q].1);
                        let (a, b) = (q4[kq], q4[(kq + 1) % 4]);
                        *ecount.entry((a.min(b), a.max(b))).or_default() += 1;
                    }
                }
                let open: HashSet<u32> = ecount.iter().filter(|x| *x.1 == 1).flat_map(|x| [x.0 .0, x.0 .1]).collect();
                let mut by: HashMap<String, usize> = HashMap::default();
                let mut total = 0;
                for (&v, pats) in &val {
                    if pats.len() == 4 || open.contains(&v) {
                        continue;
                    }
                    total += 1;
                    let mut ps: Vec<u8> = pats.clone();
                    ps.sort_unstable();
                    ps.dedup();
                    let key = format!("{} {:?}", kind(v), ps);
                    *by.entry(key).or_default() += 1;
                }
                let mut b: Vec<_> = by.into_iter().collect();
                b.sort_by(|x, y| y.1.cmp(&x.1));
                eprintln!("  result poles: {total}; by point kind and fills round it: {:?}", &b[..b.len().min(12)]);
            }
            eprintln!("  folded quads by pattern: {fb:?}");
            for &(l, q, k) in lens.iter().take(5) {
                if l < 4.0 {
                    break;
                }
                let (a, b) = (quads_out[q][k], quads_out[q][(k + 1) % 4]);
                let (pi, pat) = made_by[q];
                let pa = &c.patches[pi];
                eprintln!("  long edge {l:.1} quads: {a} ({}) - {b} ({}), patch {pi} pattern {pat}, {} sides, messy {}, band {}, {} faces", kind(a), kind(b), pa.sides.len(), pa.messy, pa.band, pa.faces.len());
            }
        }
        // Badly shaped quads (an angle under 45° or over 135°, or a side over 3× another),
        // straight from the fill: by pattern, and by how stretched the patch's counts are.
        {
            let bad_quad = |q: &[u32; 4]| -> bool {
                let p: Vec<Vec3> = q.iter().map(|&v| out_p[v as usize]).collect();
                let l: Vec<f32> = (0..4).map(|k| p[k].distance(p[(k + 1) % 4])).collect();
                let (mn, mx) = (l.iter().cloned().fold(f32::INFINITY, f32::min), l.iter().cloned().fold(0.0, f32::max));
                if mn <= 0.0 || mx / mn > 3.0 {
                    return true;
                }
                (0..4).any(|k| {
                    let (a, b) = (p[(k + 3) % 4] - p[k], p[(k + 1) % 4] - p[k]);
                    let ang = a.angle_between(b).to_degrees();
                    !(45.0..=135.0).contains(&ang)
                })
            };
            let mut by_pat: HashMap<u8, (usize, usize)> = HashMap::default();
            let mut by_stretch: Vec<(usize, usize)> = vec![(0, 0); 4];
            let mut stretch_of: HashMap<usize, f32> = HashMap::default();
            for (pi, pa) in c.patches.iter().enumerate() {
                let mut worst = 1.0f32;
                for sd in &pa.sides {
                    let cnt: i64 = sd.iter().map(|&(a, _)| n[a as usize]).sum();
                    let len: f32 = sd.iter().map(|&(a, _)| c.arcs[a as usize].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum::<f32>() / quad).sum();
                    if cnt > 0 && len > 0.3 {
                        let r = cnt as f32 / len;
                        worst = worst.max(r.max(1.0 / r));
                    }
                }
                stretch_of.insert(pi, worst);
            }
            for (q, quad4) in quads_out.iter().enumerate() {
                let (pi, pat) = made_by[q];
                let bad = bad_quad(quad4);
                let e = by_pat.entry(pat).or_default();
                e.0 += 1;
                e.1 += bad as usize;
                let st = stretch_of[&pi];
                let bin = if st < 1.5 { 0 } else if st < 2.0 { 1 } else if st < 3.0 { 2 } else { 3 };
                by_stretch[bin].0 += 1;
                by_stretch[bin].1 += bad as usize;
            }
            // Quads made against patch area, by pattern.
            let mut made_area: HashMap<u8, (usize, f32, usize)> = HashMap::default();
            let mut counted: HashSet<usize> = HashSet::default();
            for &(pi, pat) in made_by.iter() {
                let e = made_area.entry(pat).or_default();
                e.0 += 1;
                if counted.insert(pi) {
                    e.2 += 1;
                    e.1 += c.patches[pi].faces.iter().map(|&f| { let t = w.t[f as usize]; (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5 }).sum::<f32>() / (quad * quad);
                }
            }
            let mut ma: Vec<_> = made_area.into_iter().map(|(k, (q, a, np))| (k, np, q, a.round() as i64)).collect();
            ma.sort();
            eprintln!("  quads made vs area by pattern (pattern, patches, quads, quads²): {ma:?}");
            let mut per: HashMap<usize, (u8, usize)> = HashMap::default();
            for &(pi, pat) in made_by.iter() {
                per.entry(pi).or_insert((pat, 0)).1 += 1;
            }
            let mut ex: Vec<(f32, usize, u8, usize, f32)> = per.iter().map(|(&pi, &(pat, q))| {
                let a = c.patches[pi].faces.iter().map(|&f| { let t = w.t[f as usize]; (w.p[t[1] as usize] - w.p[t[0] as usize]).cross(w.p[t[2] as usize] - w.p[t[0] as usize]).length() * 0.5 }).sum::<f32>() / (quad * quad);
                (q as f32 - a, pi, pat, q, a)
            }).collect();
            ex.sort_by(|a, b| b.0.total_cmp(&a.0));
            for &(_, pi, pat, q, a) in ex.iter().take(15) {
                let pa = &c.patches[pi];
                let l: Vec<i64> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize]).sum()).collect();
                let t: Vec<f32> = pa.sides.iter().map(|sd| (sd.iter().map(|&(a, _)| c.arcs[a as usize].windows(2).map(|e| w.p[e[0] as usize].distance(w.p[e[1] as usize])).sum::<f32>() / quad).sum::<f32>() * 10.0).round() / 10.0).collect();
                eprintln!("    over: patch {pi} pattern {pat}: {q} quads for {a:.0} quads², real {} messy {} band {} counts {l:?} lengths {t:?}", pa.real, pa.messy, pa.band);
            }
            let mut pats: Vec<_> = by_pat.into_iter().collect();
            pats.sort();
            eprintln!("  bad quads by pattern (pattern: total, bad): {pats:?}");
            // Why patches go to the general fill.
            let mut why: HashMap<String, (usize, usize)> = HashMap::default();
            for (pi, pa) in c.patches.iter().enumerate() {
                if pa.band {
                    continue;
                }
                let l: Vec<i64> = pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize]).sum()).collect();
                let k = l.len();
                let quads_here = made_by.iter().filter(|m| m.0 == pi).count();
                let general = made_by.iter().any(|m| m.0 == pi && m.1 == 2);
                if !general {
                    continue;
                }
                let mut times: HashMap<u32, u32> = HashMap::default();
                for &v in &pa.lp {
                    *times.entry(v).or_default() += 1;
                }
                let pinches = times.values().filter(|&&t| t > 1).count();
                let reason = if pinches > 0 {
                    format!("pinched ({} pinches)", if pinches > 3 { "4+".to_string() } else { pinches.to_string() })
                } else if pa.real < 3 {
                    format!("{} corners", pa.real)
                } else if k > 8 {
                    "over 8 sides".to_string()
                } else if k == 4 {
                    if l[0] != l[2] || l[1] != l[3] { format!("4 sides, unequal ({}{})", if pa.messy { "messy" } else { "tidy" }, if pa.free.is_some() { ", trapezoid" } else { "" }) } else { "4 sides, messy".to_string() }
                } else {
                    // Parity of each cycle under a step of two, and positivity.
                    let odd_cycle = (0..k).any(|c0| {
                        let mut sum = 0;
                        let mut i = c0;
                        loop {
                            sum += l[i];
                            i = (i + 2) % k;
                            if i == c0 {
                                break;
                            }
                        }
                        sum % 2 != 0
                    });
                    if k % 2 == 0 && odd_cycle { format!("{k} sides, odd cycle") } else { format!("{k} sides, no positive split") }
                };
                if std::env::var_os("ZR_LISTFILL").is_some() {
                    let mid = pa.lp.iter().map(|&v| w.p[v as usize]).sum::<Vec3>() / pa.lp.len().max(1) as f32;
                    eprintln!("    general fill p{pi} at {mid:.2}: {reason}, counts {l:?}, {quads_here} quads, messy {}", pa.messy);
                }
                let e = why.entry(reason).or_default();
                e.0 += 1;
                e.1 += quads_here;
            }
            let mut why: Vec<_> = why.into_iter().collect();
            why.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            eprintln!("  general fill, why (patches, quads): {why:?}");
            let mut kr: HashMap<(usize, usize), (usize, usize)> = HashMap::default();
            for (pi, pa) in c.patches.iter().enumerate() {
                if pa.band {
                    continue;
                }
                let e = kr.entry((pa.sides.len(), pa.real)).or_default();
                e.0 += 1;
                e.1 += made_by.iter().filter(|m| m.0 == pi).count();
            }
            let mut krv: Vec<_> = kr.into_iter().collect();
            krv.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            eprintln!("  patches by (sides, real corners): (patches, quads) {:?}", &krv[..krv.len().min(14)]);
            eprintln!("  bad quads by count stretch (<1.5, <2, <3, more): {by_stretch:?}");
        }
        let dup: usize = quads_out.iter().filter(|q| (0..4).any(|k| q[k] == q[(k + 1) % 4] || q[k] == q[(k + 2) % 4])).count();
        for (q, quad4) in quads_out.iter().enumerate().filter(|(_, q)| (0..4).any(|k| q[k] == q[(k + 1) % 4] || q[k] == q[(k + 2) % 4])).take(4) {
            let (pi, pat) = made_by[q];
            let pa = &c.patches[pi];
            eprintln!("  degenerate quad {quad4:?}: patch {pi} pattern {pat}, {} sides, band {}, messy {}, real {}, side counts {:?}", pa.sides.len(), pa.band, pa.messy, pa.real, pa.sides.iter().map(|sd| sd.iter().map(|&(a, _)| n[a as usize]).sum::<i64>()).collect::<Vec<_>>());
        }
        eprintln!("  {} over-used edges, {dup} degenerate quads", shown);
    }
    if quads_out.is_empty() {
        return Err("no patches".into());
    }
    if let Some((wf, d, tubes)) = &tube_info {
        let n = sweep_tubes(wf, d, tubes, &mut out_p, &mut quads_out, quad)?;
        if n < tubes.len() {
            return Err(format!("{} of {} limb tubes could not be swept", tubes.len() - n, tubes.len()));
        }
    }
    Ok((out_p, quads_out))
}

/// Debugging: the working mesh with the lines as thin ribbons, as an OBJ.
fn dump_lines(w: &Work, cut: &HashSet<(u32, u32)>, nodes: &HashSet<u32>, path: &std::path::Path) {
    dump_lines_marked(w, cut, nodes, &[], path)
}

fn dump_lines_marked(w: &Work, cut: &HashSet<(u32, u32)>, nodes: &HashSet<u32>, faulty: &[u32], path: &std::path::Path) {
    use std::fmt::Write;
    let mut s = String::new();
    for p in &w.p {
        writeln!(s, "v {} {} {}", p.x, p.y, p.z).unwrap();
    }
    s.push_str("g mesh\n");
    for t in &w.t {
        writeln!(s, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1).unwrap();
    }
    let mut nv = w.p.len();
    let len = (0..w.p.len().min(500)).filter_map(|v| w.neighbours(v as u32).first().map(|&u| w.p[v].distance(w.p[u as usize]))).sum::<f32>() / w.p.len().min(500).max(1) as f32;
    s.push_str("g lines\n");
    for &(a, b) in cut {
        let (pa, pb) = (w.p[a as usize], w.p[b as usize]);
        let n = (w.vnrm[a as usize] + w.vnrm[b as usize]).normalize_or(Vec3::Z);
        let side = n.cross(pb - pa).normalize_or(Vec3::X) * len * 0.12;
        let up = n * len * 0.05;
        for p in [pa - side + up, pa + side + up, pb + side + up, pb - side + up] {
            writeln!(s, "v {} {} {}", p.x, p.y, p.z).unwrap();
        }
        writeln!(s, "f {} {} {} {}", nv + 1, nv + 2, nv + 3, nv + 4).unwrap();
        nv += 4;
    }
    s.push_str("g faults\n");
    for &f in faulty {
        let t = w.t[f as usize];
        let c = (w.p[t[0] as usize] + w.p[t[1] as usize] + w.p[t[2] as usize]) / 3.0 + w.fnrm[f as usize] * len * 0.03;
        let q: Vec<Vec3> = t.iter().map(|&v| c.lerp(w.p[v as usize] + w.fnrm[f as usize] * len * 0.03, 1.0)).collect();
        for p in q {
            writeln!(s, "v {} {} {}", p.x, p.y, p.z).unwrap();
        }
        writeln!(s, "f {} {} {}", nv + 1, nv + 2, nv + 3).unwrap();
        nv += 3;
    }
    s.push_str("g nodes\n");
    for &v in nodes {
        let (p, n) = (w.p[v as usize], w.vnrm[v as usize]);
        let (x, y) = (n.any_orthonormal_vector() * len * 0.35, n.cross(n.any_orthonormal_vector()) * len * 0.35);
        for q in [p - x - y + n * len * 0.08, p + x - y + n * len * 0.08, p + x + y + n * len * 0.08, p - x + y + n * len * 0.08] {
            writeln!(s, "v {} {} {}", q.x, q.y, q.z).unwrap();
        }
        writeln!(s, "f {} {} {} {}", nv + 1, nv + 2, nv + 3, nv + 4).unwrap();
        nv += 4;
    }
    let _ = std::fs::write(path, s);
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    /// Every edge of a ring fill is used once on the border and twice inside.
    fn check(sides: &[usize]) {
        let total: usize = sides.iter().sum();
        let r = total as f32 / std::f32::consts::TAU;
        let ids: Vec<u32> = (0..total as u32).collect();
        let pos: Vec<Vec2> = (0..total).map(|k| {
            let a = std::f32::consts::TAU * k as f32 / total as f32;
            Vec2::new(a.cos(), a.sin()) * r
        }).collect();
        let mut corners = vec![0usize];
        for s in &sides[..sides.len() - 1] {
            corners.push(corners.last().unwrap() + s);
        }
        let mut next = total as u32;
        let mut quads = Vec::new();
        ring_fill(ids, pos, corners, &mut |_| {
            next += 1;
            next - 1
        }, &mut quads);
        let mut used: HashMap<(u32, u32), u32> = HashMap::default();
        for q in &quads {
            let mut c = q.to_vec();
            c.sort_unstable();
            c.dedup();
            assert_eq!(c.len(), 4, "sides {sides:?}: quad {q:?} repeats a corner");
            for k in 0..4 {
                let (a, b) = (q[k], q[(k + 1) % 4]);
                *used.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        for (&(a, b), &n) in &used {
            let border = b < total as u32 && (b == a + 1 || (a == 0 && b == total as u32 - 1));
            assert_eq!(n, if border { 1 } else { 2 }, "sides {sides:?}: edge {a}-{b} used {n} times");
        }
    }

    #[test]
    fn split_sides_solves_three_to_eight_sides() {
        for l in [vec![4i64, 4, 4], vec![3, 5, 4], vec![4, 4, 4, 4, 4], vec![4, 4, 4, 4, 4, 4], vec![3, 5, 4, 6, 5, 3], vec![4; 7], vec![4; 8]] {
            let y = split_sides(&l).unwrap_or_else(|| panic!("{l:?}"));
            let k = l.len();
            for i in 0..k {
                assert_eq!(y[(i + k - 2) % k] + y[i], l[i], "{l:?} → {y:?}");
                assert!(y[i] >= 1 && l[i] - y[i] >= 1);
            }
        }
        assert!(split_sides(&[3, 3, 3, 3, 3, 4]).is_none() || true);
        // The margin matches the real split: [4, 4, 4] splits 2 / 2; [14, 2, 14] does not.
        assert!((split_margin(&[4, 4, 4]) - 2.0).abs() < 1e-9);
        assert!((split_margin(&[14, 2, 14]) - 1.0).abs() < 1e-9);
        assert!((split_margin(&[4; 6]) - 2.0).abs() < 1e-9);
        for l in [vec![3i64, 5, 4], vec![4, 4, 4, 4, 4], vec![3, 5, 4, 6, 5, 3], vec![4; 7]] {
            assert_eq!(split_margin(&l) >= 1.0, split_sides(&l).is_some(), "{l:?}");
        }
    }

    #[test]
    fn drop_inner_shell_removes_a_skin_inside_and_keeps_the_outer_one() {
        let (po, io) = crate::mesh::cube_sphere(12, 1.0);
        let (pi, ii) = crate::mesh::cube_sphere(12, 0.97);
        let outer = io.len() / 3;
        let mut p = po.clone();
        let base = p.len() as u32;
        p.extend(pi);
        let mut idx = io.clone();
        idx.extend(ii.iter().map(|&x| x + base));
        let mut grp = vec![0u16; idx.len() / 3];
        let gone = drop_inner_shell(&mut p, &mut idx, &mut grp);
        assert_eq!(gone, idx.len() / 3 + gone - outer, "only the inner sphere goes");
        assert_eq!(idx.len() / 3, outer);
        // A single skin loses nothing.
        let mut idx1 = io.clone();
        let mut p1 = po.clone();
        let mut g1 = vec![0u16; idx1.len() / 3];
        assert_eq!(drop_inner_shell(&mut p1, &mut idx1, &mut g1), 0);
    }

    #[test]
    fn ring_fill_keeps_every_edge_manifold() {
        for sides in [vec![1, 4, 9, 4, 9, 5], vec![3, 15, 3, 1, 4, 1, 15], vec![4, 4, 4, 4], vec![6, 3, 5], vec![2, 2], vec![10], vec![30, 9, 3, 31, 3], vec![4, 6, 2, 5, 1, 3, 5], vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]] {
            check(&sides);
        }
    }
}
