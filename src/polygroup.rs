//! Polygroups and visibility, ZBrush style: a group id per triangle and a hidden flag per
//! triangle on every subdivision level (see `SculptMesh::groups` / `hidden`).
//!
//! This module holds what is independent of the level stack: the compact run-length form
//! used by parked subtools, project files and undo, and the rectangle test of SelectRect.

use crate::mesh::SculptMesh;
use glam::{Mat4, Vec2, Vec3, Vec4};
use rayon::prelude::*;

const BLOCK: usize = 1 << 20;

/// Run-length form of per-triangle hidden flags: lengths of alternating runs of visible and
/// hidden triangles, starting with visible (that first run may be 0). Empty when nothing is
/// hidden.
pub fn runs_of_flags(flags: &[u8]) -> Vec<u32> {
    if flags.iter().all(|&f| f == 0) {
        return Vec::new();
    }
    let runs = runs_by_block(flags, |&f| (f != 0) as u16);
    let mut out = Vec::with_capacity(runs.len() + 1);
    if runs[0].1 != 0 {
        out.push(0);
    }
    out.extend(runs.iter().map(|r| r.0));
    out
}

/// Hidden flags back from [`runs_of_flags`]; `len` is the triangle count of the level.
pub fn flags_of_runs(runs: &[u32], len: usize) -> Result<Vec<u8>, String> {
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    if runs.iter().map(|&r| r as u64).sum::<u64>() != len as u64 {
        return Err("hidden triangle runs do not cover the level".into());
    }
    let mut out = vec![0u8; len];
    let mut at = 0;
    for (i, &r) in runs.iter().enumerate() {
        out[at..at + r as usize].fill((i % 2) as u8);
        at += r as usize;
    }
    Ok(out)
}

/// Run-length form of per-triangle polygroups. Empty when the whole level is group 0.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct GroupRuns {
    pub lens: Vec<u32>,
    pub values: Vec<u16>,
}

impl GroupRuns {
    pub fn of(groups: &[u16]) -> Self {
        if groups.is_empty() {
            return Self::default();
        }
        let runs = runs_by_block(groups, |&g| g);
        if runs.len() == 1 && runs[0].1 == 0 {
            return Self::default();
        }
        Self {
            lens: runs.iter().map(|r| r.0).collect(),
            values: runs.iter().map(|r| r.1).collect(),
        }
    }

    /// The groups back; `len` is the triangle count of the level.
    pub fn expand(&self, len: usize) -> Result<Vec<u16>, String> {
        if self.lens.is_empty() {
            return Ok(Vec::new());
        }
        if self.lens.len() != self.values.len()
            || self.lens.iter().map(|&r| r as u64).sum::<u64>() != len as u64
        {
            return Err("polygroup runs do not cover the level".into());
        }
        let mut out = vec![0u16; len];
        let mut at = 0;
        for (&r, &g) in self.lens.iter().zip(&self.values) {
            out[at..at + r as usize].fill(g);
            at += r as usize;
        }
        Ok(out)
    }

    pub fn bytes(&self) -> usize {
        self.lens.len() * 6
    }
}

/// `(length, value)` runs of `key` over `data`, computed in parallel blocks and joined.
fn runs_by_block<T: Sync>(data: &[T], key: impl Fn(&T) -> u16 + Sync) -> Vec<(u32, u16)> {
    let blocks: Vec<Vec<(u32, u16)>> = data
        .par_chunks(BLOCK)
        .map(|block| {
            let mut runs: Vec<(u32, u16)> = Vec::new();
            for x in block {
                let k = key(x);
                match runs.last_mut() {
                    Some(last) if last.1 == k => last.0 += 1,
                    _ => runs.push((1, k)),
                }
            }
            runs
        })
        .collect();
    let mut out: Vec<(u32, u16)> = Vec::new();
    for b in blocks {
        for r in b {
            match out.last_mut() {
                Some(last) if last.1 == r.1 => last.0 += r.0,
                _ => out.push(r),
            }
        }
    }
    out
}

/// A colour for polygroup `g`, spread around the hue circle (golden ratio steps) so
/// neighbouring ids differ clearly. Must match `group_color` in `groups.wgsl`.
#[allow(dead_code)]
pub fn group_color(g: u16) -> [f32; 3] {
    // Offset so group 0, the whole of a new model, is a clear blue, far from the clay.
    let h = (g as f32 * 0.618_034 + 0.55).fract() * 6.0;
    let (s, v) = (0.55, 0.85);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, gr, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, gr + m, b + m]
}

/// Which triangles of `mesh` lie in the screen rectangle `[lo, hi]` (pixels on a canvas of
/// size `viewport`, seen through `view_proj`): 1 per triangle whose centre projects inside
/// and in front of the camera. Like ZBrush's selection it goes through the model, front and
/// back alike. Chunks wholly outside or inside the rectangle are decided from their bounds.
pub fn tris_in_rect(mesh: &SculptMesh, view_proj: Mat4, viewport: Vec2, lo: Vec2, hi: Vec2) -> Vec<u8> {
    let to_px = |p: Vec3| -> Option<Vec2> {
        let c: Vec4 = view_proj * p.extend(1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let n = c.truncate() / c.w;
        Some(Vec2::new((n.x + 1.0) * 0.5 * viewport.x, (1.0 - n.y) * 0.5 * viewport.y))
    };
    let inside = |q: Vec2| q.cmpge(lo).all() && q.cmple(hi).all();
    let mut out = vec![0u8; mesh.triangle_count()];
    let ranges = mesh.chunks.iter().map(|c| (c.tri_start as usize, c.tri_end as usize));
    crate::mesh::split_ranges(&mut out, ranges)
        .into_par_iter()
        .zip(mesh.chunks.par_iter())
        .for_each(|(slice, ch)| {
            // The eight corners of the chunk's bounds decide most chunks at once.
            let b = ch.aabb;
            let corners: Vec<Option<Vec2>> = (0..8)
                .map(|k| {
                    to_px(Vec3::new(
                        if k & 1 == 0 { b.min.x } else { b.max.x },
                        if k & 2 == 0 { b.min.y } else { b.max.y },
                        if k & 4 == 0 { b.min.z } else { b.max.z },
                    ))
                })
                .collect();
            if corners.iter().all(|c| c.is_some_and(inside)) {
                slice.fill(1);
                return;
            }
            if corners.iter().all(Option::is_some) {
                let (mut cmin, mut cmax) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
                for c in corners.iter().flatten() {
                    cmin = cmin.min(*c);
                    cmax = cmax.max(*c);
                }
                if cmax.x < lo.x || cmax.y < lo.y || cmin.x > hi.x || cmin.y > hi.y {
                    return;
                }
            }
            for (i, t) in ch.tris().enumerate() {
                let tri = &mesh.indices[3 * t as usize..3 * t as usize + 3];
                let c = (mesh.positions[tri[0] as usize] + mesh.positions[tri[1] as usize] + mesh.positions[tri[2] as usize]) / 3.0;
                slice[i] = to_px(c).is_some_and(inside) as u8;
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_round_trip() {
        let flags: Vec<u8> = (0..3_000_017u32).map(|i| ((i / 1000) % 3 == 1 || i % 777_777 == 5) as u8).collect();
        let runs = runs_of_flags(&flags);
        assert_eq!(flags_of_runs(&runs, flags.len()).unwrap(), flags);
        assert!(runs_of_flags(&vec![0; 10]).is_empty());
        assert_eq!(runs_of_flags(&[1, 1, 0]), vec![0, 2, 1]);
        assert_eq!(flags_of_runs(&[0, 2, 1], 3).unwrap(), vec![1, 1, 0]);
        assert!(flags_of_runs(&[0, 2, 1], 4).is_err());

        let groups: Vec<u16> = (0..2_500_003u32).map(|i| ((i / 4096) % 7) as u16).collect();
        let r = GroupRuns::of(&groups);
        assert_eq!(r.expand(groups.len()).unwrap(), groups);
        // Runs cross the parallel blocks and are joined there.
        assert!(r.lens.iter().all(|&l| l == 4096 || l < 4096));
        assert_eq!(r.lens.len(), groups.len().div_ceil(4096));
        assert_eq!(GroupRuns::of(&vec![0; 5]), GroupRuns::default());
        assert!(GroupRuns::of(&vec![3; 5]).lens == vec![5]);
        assert!(r.expand(groups.len() + 1).is_err());
    }

    #[test]
    fn group_0_does_not_look_like_clay() {
        // Warm clay, the default material (shader.wgsl).
        let clay = [0.78, 0.62, 0.50];
        let c = group_color(0);
        let d: f32 = c.iter().zip(&clay).map(|(x, y)| (x - y).abs()).sum();
        assert!(d > 0.4, "group 0 {c:?} is too close to the clay");
    }

    #[test]
    fn colours_differ_for_neighbouring_groups() {
        for g in 0..64u16 {
            let (a, b) = (group_color(g), group_color(g + 1));
            let d: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
            assert!(d > 0.15, "groups {g} and {} look alike", g + 1);
        }
    }
}

// ---------------------------------------------------------------- Grow / Shrink, Auto Groups

/// Corners of triangle `t`.
fn corners(mesh: &SculptMesh, t: u32) -> [u32; 3] {
    let i = 3 * t as usize;
    [mesh.indices[i], mesh.indices[i + 1], mesh.indices[i + 2]]
}

/// Visibility › Grow: hidden polygons touching a visible one become visible (one ring).
pub fn grown_visibility(mesh: &SculptMesh) -> Vec<u8> {
    if mesh.hidden.is_empty() {
        return Vec::new();
    }
    (0..mesh.triangle_count() as u32)
        .into_par_iter()
        .map(|t| (!mesh.visible(t) && !corners(mesh, t).iter().any(|&v| mesh.shown[v as usize] != 0)) as u8)
        .collect()
}

/// Visibility › Shrink: visible polygons touching a hidden one are hidden (one ring). With
/// nothing hidden there is no edge to shrink from.
pub fn shrunk_visibility(mesh: &SculptMesh) -> Vec<u8> {
    if mesh.hidden.is_empty() {
        return Vec::new();
    }
    let touches_hidden: Vec<bool> = (0..mesh.vertex_count() as u32)
        .into_par_iter()
        .map(|v| mesh.incident(v).iter().any(|&t| !mesh.visible(t)))
        .collect();
    (0..mesh.triangle_count() as u32)
        .into_par_iter()
        .map(|t| (!mesh.visible(t) || corners(mesh, t).iter().any(|&v| touches_hidden[v as usize])) as u8)
        .collect()
}

/// Number visible regions from 0 in the order they are found, keeping hidden polygons'
/// groups; `joins(a, b)` says whether neighbouring visible triangles `a`, `b` (sharing an
/// edge) belong together, `seed` is the first triangle of the region. More regions than
/// group ids share the last id.
fn regions(mesh: &SculptMesh, joins: impl Fn(u32, u32, u32) -> bool) -> Vec<u16> {
    let n = mesh.triangle_count();
    const NONE: u32 = u32::MAX;
    let mut region = vec![NONE; n];
    let mut count = 0u32;
    let mut stack = Vec::new();
    for seed in 0..n as u32 {
        if region[seed as usize] != NONE || !mesh.visible(seed) {
            continue;
        }
        region[seed as usize] = count;
        stack.push(seed);
        while let Some(t) = stack.pop() {
            let c = corners(mesh, t);
            for k in 0..3 {
                let (a, b) = (c[k], c[(k + 1) % 3]);
                for &u in mesh.incident(a) {
                    if u == t || region[u as usize] != NONE || !mesh.visible(u) || !corners(mesh, u).contains(&b) {
                        continue;
                    }
                    if joins(seed, t, u) {
                        region[u as usize] = count;
                        stack.push(u);
                    }
                }
            }
        }
        count += 1;
    }
    region
        .iter()
        .enumerate()
        .map(|(t, &r)| if r == NONE { mesh.group(t as u32) } else { r.min(u16::MAX as u32) as u16 })
        .collect()
}

/// Polygroups › Auto Groups: one polygroup per connected piece of visible polygons.
pub fn auto_groups(mesh: &SculptMesh) -> Vec<u16> {
    regions(mesh, |_, _, _| true)
}

/// Polygroups › Group By Normals: regions whose polygons all face within `max_angle`
/// degrees of the region's first polygon, grown across shared edges (flat faces of a hard
/// surface each become a group; a curved surface splits into patches).
pub fn groups_by_normals(mesh: &SculptMesh, max_angle: f32) -> Vec<u16> {
    let normals: Vec<Vec3> = (0..mesh.triangle_count() as u32)
        .into_par_iter()
        .map(|t| {
            let [a, b, c] = corners(mesh, t).map(|v| mesh.positions[v as usize]);
            (b - a).cross(c - a).normalize_or_zero()
        })
        .collect();
    let cos = max_angle.to_radians().cos();
    regions(mesh, |seed, _, u| normals[seed as usize].dot(normals[u as usize]) >= cos)
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::primitive::{Primitive, Settings};

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(12, 1.0);
        SculptMesh::from_raw(p, i)
    }

    fn visible(hidden: &[u8]) -> usize {
        hidden.iter().filter(|&&h| h == 0).count()
    }

    #[test]
    fn grow_and_shrink_visibility_move_by_one_ring() {
        let mut m = sphere();
        assert!(grown_visibility(&m).is_empty() && shrunk_visibility(&m).is_empty(), "nothing hidden");
        // Only the polygons around vertex 0 visible.
        let star = m.incident(0).to_vec();
        m.set_hidden((0..m.triangle_count() as u32).map(|t| (!star.contains(&t)) as u8).collect());
        let grown = grown_visibility(&m);
        assert!(visible(&grown) > star.len());
        // Every newly shown polygon touches the old visible ones.
        for t in 0..m.triangle_count() as u32 {
            if grown[t as usize] == 0 && !star.contains(&t) {
                assert!(corners(&m, t).iter().any(|&v| m.shown[v as usize] != 0));
            }
        }
        m.set_hidden(grown);
        let back = shrunk_visibility(&m);
        assert!(visible(&back) < visible(&m.hidden) && visible(&back) <= star.len());
    }

    #[test]
    fn auto_groups_give_each_piece_its_own_group() {
        let (p, i) = crate::mesh::cube_sphere(8, 1.0);
        let n = p.len() as u32;
        let mut positions = p.clone();
        positions.extend(p.iter().map(|q| *q + Vec3::X * 3.0));
        let mut indices = i.clone();
        indices.extend(i.iter().map(|k| k + n));
        let m = SculptMesh::from_raw(positions, indices);
        let groups = auto_groups(&m);
        let mut ids: Vec<u16> = groups.clone();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 2);
        // Each piece is one group: its triangles share a side of X = 1.5.
        for t in 0..m.triangle_count() as u32 {
            let right = m.positions[corners(&m, t)[0] as usize].x > 1.5;
            assert_eq!(groups[t as usize], groups[0] ^ (right != (m.positions[corners(&m, 0)[0] as usize].x > 1.5)) as u16);
        }
    }

    #[test]
    fn group_by_normals_splits_a_cube_by_face_and_a_sphere_into_patches() {
        let (p, i, _) = Settings { kind: Primitive::Cube, ..Default::default() }.build(1.0);
        let cube = SculptMesh::from_raw(p, i);
        let count = |g: &[u16]| {
            let mut ids = g.to_vec();
            ids.sort();
            ids.dedup();
            ids.len()
        };
        assert_eq!(count(&groups_by_normals(&cube, 20.0)), 6);
        let s = sphere();
        let (fine, coarse) = (count(&groups_by_normals(&s, 15.0)), count(&groups_by_normals(&s, 60.0)));
        assert!(fine > coarse && coarse > 1, "{fine} {coarse}");
        // Hidden polygons keep their group.
        let mut s = sphere();
        s.set_groups(vec![7; s.triangle_count()]);
        s.set_hidden((0..s.triangle_count()).map(|t| (t % 2) as u8).collect());
        let g = groups_by_normals(&s, 30.0);
        assert!((0..s.triangle_count()).filter(|t| t % 2 == 1).all(|t| g[t] == 7));
    }
}
