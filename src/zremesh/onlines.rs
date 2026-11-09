//! Edge loops onto edge lines: for each line, the one chain of the new mesh's edges that runs
//! along it, its points to go onto the line.
//!
//! Moving every point near a line onto it (as before) pulls two rows onto one line where the
//! loops do not run exactly along it, and their quads collapse. Here each line gets a path
//! through the new edges from its start to its end, kept close to the line and along it, and
//! only the stretches of that path that run along the line (two edges or more, each within
//! 30° of the line and a third of a quad of it) go onto it.

use crate::fx::FxHashMap as HashMap;
use glam::Vec3;
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// One line, ready for nearest-point queries: its points, the length along it to each, and a
/// grid of its segments.
struct Line<'a> {
    p: &'a [Vec3],
    along: Vec<f32>,
    cell: f32,
    grid: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl<'a> Line<'a> {
    fn new(p: &'a [Vec3], cell: f32) -> Self {
        let mut along = vec![0.0f32; p.len()];
        for i in 1..p.len() {
            along[i] = along[i - 1] + p[i - 1].distance(p[i]);
        }
        let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
        for i in 0..p.len().saturating_sub(1) {
            let (a, b) = (p[i], p[i + 1]);
            let n = (a.distance(b) / (cell * 0.5)).ceil().max(1.0) as usize;
            for k in 0..=n {
                let c = (a.lerp(b, k as f32 / n as f32) / cell).floor();
                let list = grid
                    .entry((c.x as i32, c.y as i32, c.z as i32))
                    .or_default();
                if list.last() != Some(&(i as u32)) {
                    list.push(i as u32);
                }
            }
        }
        Self {
            p,
            along,
            cell,
            grid,
        }
    }

    fn len(&self) -> f32 {
        *self.along.last().unwrap_or(&0.0)
    }

    /// The nearest point of the line within a cell of `q`: its distance, the point, the length
    /// along the line to it, and the line's direction there.
    fn nearest(&self, q: Vec3) -> Option<Near> {
        let c = (q / self.cell).floor();
        let mut best: Option<Near> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &s in self
                        .grid
                        .get(&(c.x as i32 + dx, c.y as i32 + dy, c.z as i32 + dz))
                        .into_iter()
                        .flatten()
                    {
                        let (a, b) = (self.p[s as usize], self.p[s as usize + 1]);
                        let ab = b - a;
                        let l2 = ab.length_squared();
                        if l2 <= 0.0 {
                            continue;
                        }
                        let t = ((q - a).dot(ab) / l2).clamp(0.0, 1.0);
                        let at = a + ab * t;
                        let d = at.distance(q);
                        if best.is_none_or(|x| d < x.0) {
                            best = Some((
                                d,
                                at,
                                self.along[s as usize] + t * l2.sqrt(),
                                ab / l2.sqrt(),
                            ));
                        }
                    }
                }
            }
        }
        best
    }
}

type Near = (f32, Vec3, f32, Vec3);

/// The cheapest paths from `from` through the points `ok` allows (not those in `avoid`), an
/// edge costing its length, more when it runs across the line or away from it: each reached
/// point's cost and the point before it. Stops on reaching `to`.
fn search(
    p: &[Vec3],
    adj: &[Vec<u32>],
    from: u32,
    to: Option<u32>,
    ok: &HashMap<u32, Near>,
    avoid: &[u32],
    quad: f32,
) -> (HashMap<u32, f32>, HashMap<u32, u32>) {
    let mut dist: HashMap<u32, f32> = HashMap::default();
    let mut prev: HashMap<u32, u32> = HashMap::default();
    let mut heap = BinaryHeap::new();
    dist.insert(from, 0.0);
    heap.push((Reverse(0u32), from));
    while let Some((Reverse(d), v)) = heap.pop() {
        let d = f32::from_bits(d);
        if Some(v) == to {
            break;
        }
        if dist.get(&v).is_some_and(|&x| d > x) {
            continue;
        }
        for &u in &adj[v as usize] {
            if Some(u) != to && avoid.contains(&u) {
                continue;
            }
            let Some(&(du, _, _, tu)) = ok.get(&u) else {
                continue;
            };
            let dv = ok.get(&v).map_or(0.0, |x| x.0);
            let e = p[u as usize] - p[v as usize];
            let len = e.length();
            let across = (1.0 - (e.dot(tu).abs() / len.max(1e-12)).min(1.0)).max(0.0);
            let cost = len * (1.0 + 4.0 * across + 4.0 * (du + dv) * 0.5 / quad);
            let nd = d + cost;
            if dist.get(&u).is_none_or(|&x| nd < x) {
                dist.insert(u, nd);
                prev.insert(u, v);
                // Positive floats order as their bits.
                heap.push((Reverse(nd.to_bits()), u));
            }
        }
    }
    (dist, prev)
}

/// The path to `to` from where `prev` starts.
fn back(prev: &HashMap<u32, u32>, to: u32) -> Vec<u32> {
    let mut out = vec![to];
    let mut at = to;
    while let Some(&u) = prev.get(&at) {
        out.push(u);
        at = u;
    }
    out.reverse();
    out
}

/// The cheapest path from `from` to `to` (see [`search`]). None: there is none.
fn path(
    p: &[Vec3],
    adj: &[Vec<u32>],
    from: u32,
    to: u32,
    ok: &HashMap<u32, Near>,
    avoid: &[u32],
    quad: f32,
) -> Option<Vec<u32>> {
    let (dist, prev) = search(p, adj, from, Some(to), ok, avoid, quad);
    dist.contains_key(&to).then(|| back(&prev, to))
}

/// The points of the new mesh (`p`, joined as `adj`) to put onto the edge `lines`, and where:
/// on each line, the stretches of one path through the edges that run along it (two edges or
/// more, each within `angle` degrees of the line and both its ends within `near` quads of
/// it); a point within half a quad of one of the `corners` goes onto the corner.
pub(super) fn chains(
    p: &[Vec3],
    adj: &[Vec<u32>],
    lines: &[Vec<Vec3>],
    corners: &[Vec3],
    quad: f32,
    near: f32,
    angle: f32,
) -> Vec<(u32, Vec3)> {
    let cell = quad.max(1e-6);
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::default();
    for (v, q) in p.iter().enumerate() {
        if adj[v].is_empty() {
            continue;
        }
        let c = (*q / cell).floor();
        grid.entry((c.x as i32, c.y as i32, c.z as i32))
            .or_default()
            .push(v as u32);
    }
    let reach = quad * 0.6;
    let per_line: Vec<Vec<(u32, Vec3)>> = lines
        .par_iter()
        .map(|line| {
            let line = Line::new(line, cell);
            let total = line.len();
            if line.p.len() < 2 || total < quad {
                return Vec::new();
            }
            // The points near the line, with their nearest point on it.
            let mut ok: HashMap<u32, Near> = HashMap::default();
            let mut seen: HashMap<(i32, i32, i32), ()> = HashMap::default();
            for &q in line.p {
                let c = (q / cell).floor();
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            let k = (c.x as i32 + dx, c.y as i32 + dy, c.z as i32 + dz);
                            if seen.insert(k, ()).is_some() {
                                continue;
                            }
                            for &v in grid.get(&k).into_iter().flatten() {
                                if let Some(n) = line.nearest(p[v as usize]).filter(|n| n.0 < reach)
                                {
                                    ok.insert(v, n);
                                }
                            }
                        }
                    }
                }
            }
            // The ends: the points nearest the line's first and last points (a closed line
            // goes in two halves, from its start to its middle and back).
            let closed = line.p[0].distance(line.p[line.p.len() - 1]) < quad * 0.05;
            let end_near = |along: f32| -> Option<u32> {
                ok.iter()
                    .filter(|(_, n)| {
                        (n.2 - along).abs() < quad * 0.75
                            || (closed && (total - (n.2 - along).abs()) < quad * 0.75)
                    })
                    .min_by(|a, b| {
                        let key = |n: &Near| {
                            n.0 + (n.2 - along).abs().min(if closed {
                                total - (n.2 - along).abs()
                            } else {
                                f32::INFINITY
                            })
                        };
                        key(a.1).total_cmp(&key(b.1)).then(a.0.cmp(b.0))
                    })
                    .map(|(&v, _)| v)
            };
            let (cos30, run) = (angle.to_radians().cos(), 2);
            let closed_walk = || -> Option<Vec<u32>> {
                let (a, b) = (end_near(0.0)?, end_near(total * 0.5)?);
                if a == b {
                    return None;
                }
                let first = path(p, adj, a, b, &ok, &[], quad)?;
                let second = path(p, adj, b, a, &ok, &first[1..first.len() - 1], quad)?;
                Some(
                    first
                        .into_iter()
                        .chain(second.into_iter().skip(1))
                        .collect(),
                )
            };
            // Otherwise (an open line, or no way round) in pieces: from the point nearest the
            // line furthest back, the path to the point near it furthest on that can be
            // reached; then on from there (a line can cross a gap in the new mesh, or a stretch
            // of it no points are near).
            let walks: Vec<Vec<u32>> = match closed.then(closed_walk).flatten() {
                Some(w) => vec![w],
                None => {
                    let mut walks = Vec::new();
                    let mut past = f32::NEG_INFINITY;
                    let close = |n: &Near| n.0 < quad * near;
                    loop {
                        let Some((&a, na)) = ok
                            .iter()
                            .filter(|(_, n)| n.2 > past && close(n))
                            .min_by(|x, y| {
                                (x.1.2 + 2.0 * x.1.0)
                                    .total_cmp(&(y.1.2 + 2.0 * y.1.0))
                                    .then(x.0.cmp(y.0))
                            })
                        else {
                            break;
                        };
                        let (dist, prev) = search(p, adj, a, None, &ok, &[], quad);
                        let far = dist
                            .keys()
                            .filter(|v| close(&ok[v]) && ok[v].2 > na.2)
                            .max_by(|x, y| ok[x].2.total_cmp(&ok[y].2).then(y.cmp(x)));
                        match far {
                            Some(&b) => {
                                past = ok[&b].2;
                                walks.push(back(&prev, b));
                            }
                            None => past = na.2,
                        }
                    }
                    walks
                }
            };
            // The edges along the line: within 30° of it, both ends within a third of a quad.
            // (The line's direction at the edge's middle: at a corner of the line each end's
            // nearest stretch can be the other side's.)
            let along = |walk: &[u32], i: usize| -> bool {
                let (u, v) = (walk[i], walk[i + 1]);
                let (nu, nv) = (ok[&u], ok[&v]);
                let e = p[v as usize] - p[u as usize];
                let Some(mid) = line.nearest((p[u as usize] + p[v as usize]) * 0.5) else {
                    return false;
                };
                nu.0 < quad * near && nv.0 < quad * near && e.dot(mid.3).abs() >= cos30 * e.length()
            };
            let mut out = Vec::new();
            for walk in &walks {
                let mut i = 0;
                while i + 1 < walk.len() {
                    if !along(walk, i) {
                        i += 1;
                        continue;
                    }
                    let mut j = i;
                    while j + 1 < walk.len() && along(walk, j) {
                        j += 1;
                    }
                    // walk[i..=j] runs along the line (j - i edges).
                    if j - i >= run {
                        for &v in &walk[i..=j] {
                            out.push((v, ok[&v].1));
                        }
                    }
                    i = j;
                }
            }
            out
        })
        .collect();
    let mut taken: HashMap<u32, Vec3> = HashMap::default();
    for list in per_line {
        for (v, q) in list {
            taken.entry(v).or_insert(q);
        }
    }
    // Corners: the nearest taken point within half a quad of each goes onto it.
    for &c in corners {
        if let Some((&v, _)) = taken
            .iter()
            .filter(|(v, _)| p[**v as usize].distance(c) < quad * 0.5)
            .min_by(|a, b| {
                p[*a.0 as usize]
                    .distance(c)
                    .total_cmp(&p[*b.0 as usize].distance(c))
                    .then(a.0.cmp(b.0))
            })
        {
            taken.insert(v, c);
        }
    }
    let mut out: Vec<(u32, Vec3)> = taken.into_iter().collect();
    out.sort_unstable_by_key(|x| x.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid of `n` × `n` unit squares in the XY plane: its points and neighbours.
    fn grid(n: usize) -> (Vec<Vec3>, Vec<Vec<u32>>) {
        let id = |x: usize, y: usize| (y * (n + 1) + x) as u32;
        let mut p = Vec::new();
        let mut adj = vec![Vec::new(); (n + 1) * (n + 1)];
        for y in 0..=n {
            for x in 0..=n {
                p.push(Vec3::new(x as f32, y as f32, 0.0));
                if x < n {
                    adj[id(x, y) as usize].push(id(x + 1, y));
                    adj[id(x + 1, y) as usize].push(id(x, y));
                }
                if y < n {
                    adj[id(x, y) as usize].push(id(x, y + 1));
                    adj[id(x, y + 1) as usize].push(id(x, y));
                }
            }
        }
        (p, adj)
    }

    #[test]
    fn a_line_beside_a_row_takes_that_row_only() {
        let (p, adj) = grid(10);
        // A line a quarter of a quad above row 5, from x 1 to 9: row 5's points from 1 to 9
        // go onto it, nothing of rows 4 or 6.
        let line: Vec<Vec3> = (0..=16)
            .map(|i| Vec3::new(1.0 + i as f32 * 0.5, 5.25, 0.0))
            .collect();
        let got = chains(&p, &adj, &[line], &[], 1.0, 1.0 / 3.0, 30.0);
        let rows: Vec<(u32, u32)> = got.iter().map(|&(v, _)| (v % 11, v / 11)).collect();
        assert!(rows.iter().all(|&(_, y)| y == 5), "{rows:?}");
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            (1..=9).collect::<Vec<_>>()
        );
        assert!(got.iter().all(|&(_, q)| (q.y - 5.25).abs() < 1e-5));
    }

    #[test]
    fn a_slanted_line_takes_no_staircase() {
        let (p, adj) = grid(10);
        // At 45° no edge runs along the line: nothing goes onto it (a staircase pulled onto a
        // line folds its quads).
        let line: Vec<Vec3> = (0..=16)
            .map(|i| Vec3::new(1.0 + i as f32 * 0.5, 1.0 + i as f32 * 0.5, 0.0))
            .collect();
        assert!(chains(&p, &adj, &[line], &[], 1.0, 1.0 / 3.0, 30.0).is_empty());
    }

    #[test]
    fn a_closed_line_takes_its_ring() {
        let (p, adj) = grid(10);
        // A square loop through rows and columns 3 and 7: its 16 points.
        let mut line = Vec::new();
        for (a, b) in [
            ((3.0, 3.0), (7.0, 3.0)),
            ((7.0, 3.0), (7.0, 7.0)),
            ((7.0, 7.0), (3.0, 7.0)),
            ((3.0, 7.0), (3.0, 3.0)),
        ] {
            for i in 0..8 {
                let t = i as f32 / 8.0;
                line.push(Vec3::new(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, 0.0));
            }
        }
        line.push(line[0]);
        let corners = vec![
            Vec3::new(3.0, 3.0, 0.0),
            Vec3::new(7.0, 3.0, 0.0),
            Vec3::new(7.0, 7.0, 0.0),
            Vec3::new(3.0, 7.0, 0.0),
        ];
        let got = chains(&p, &adj, &[line], &corners, 1.0, 1.0 / 3.0, 30.0);
        assert_eq!(
            got.len(),
            16,
            "{:?}",
            got.iter()
                .map(|&(v, _)| (v % 11, v / 11))
                .collect::<Vec<_>>()
        );
        assert!(got.iter().all(|&(v, q)| p[v as usize].distance(q) < 1e-5));
    }
}
