//! SubTool › Split, Merge and Extract (ZBrush): whole meshes made from parts of others. They
//! work on subtools without subdivision levels and build new meshes, so polygroups and
//! polypaint come along; masks and hidden polygons do not (the vertices are reordered into
//! new chunks).

use crate::{mesh::SculptMesh, symmetry::{Axis, MirrorPlane}};
use glam::Vec3;

/// The triangles of `mesh` that `keep` accepts, as a new mesh (vertices renumbered, groups and
/// paint kept), or `None` when it accepts none.
pub fn part(mesh: &SculptMesh, keep: impl Fn(u32) -> bool) -> Option<SculptMesh> {
    let mut map = vec![u32::MAX; mesh.vertex_count()];
    let (mut positions, mut colors, mut indices, mut groups, mut quads) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let painted = !mesh.colors.is_empty();
    for t in 0..mesh.triangle_count() as u32 {
        if !keep(t) {
            continue;
        }
        if let Some(&q) = mesh.quads.get(t as usize) {
            quads.push(q);
        }
        for k in 0..3 {
            let v = mesh.indices[3 * t as usize + k] as usize;
            if map[v] == u32::MAX {
                map[v] = positions.len() as u32;
                positions.push(mesh.positions[v]);
                if painted {
                    colors.push(mesh.colors[v]);
                }
            }
            indices.push(map[v]);
        }
        groups.push(mesh.group(t));
    }
    (!indices.is_empty()).then(|| SculptMesh::from_raw_full(positions, indices, groups, colors, quads))
}

/// Two meshes as one (SubTool › Merge Down). `b`'s polygroups are renumbered after `a`'s so
/// both keep their own; paint is kept where either has it.
pub fn merged(a: &SculptMesh, b: &SculptMesh) -> SculptMesh {
    let shift = a.groups.iter().copied().max().map_or(1, |g| g.saturating_add(1));
    let mut positions = a.positions.clone();
    positions.extend_from_slice(&b.positions);
    let base = a.vertex_count() as u32;
    let mut indices = a.indices.clone();
    indices.extend(b.indices.iter().map(|i| i + base));
    let mut groups: Vec<u16> = (0..a.triangle_count() as u32).map(|t| a.group(t)).collect();
    groups.extend((0..b.triangle_count() as u32).map(|t| b.group(t).saturating_add(shift)));
    let colors = if a.colors.is_empty() && b.colors.is_empty() {
        Vec::new()
    } else {
        let fill = |m: &SculptMesh| if m.colors.is_empty() { vec![[0; 4]; m.vertex_count()] } else { m.colors.clone() };
        let mut c = fill(a);
        c.extend(fill(b));
        c
    };
    let quads = if a.quads.is_empty() && b.quads.is_empty() {
        Vec::new()
    } else {
        let fill = |m: &SculptMesh| if m.quads.is_empty() { vec![crate::mesh::LONE; m.triangle_count()] } else { m.quads.clone() };
        let mut q = fill(a);
        q.extend(fill(b));
        q
    };
    SculptMesh::from_raw_full(positions, indices, groups, colors, quads)
}

/// SubTool › Extract: the masked surface (triangles whose corners are masked on average) as a
/// closed shell `thickness` thick, grown outward from the surface along its normals, with
/// walls along its open edges. One polygroup.
pub fn extracted(mesh: &SculptMesh, thickness: f32) -> Option<SculptMesh> {
    if mesh.mask.is_empty() {
        return None;
    }
    let masked = |t: u32| {
        let i = 3 * t as usize;
        mesh.visible(t) && mesh.indices[i..i + 3].iter().map(|&v| mesh.mask[v as usize] as u32).sum::<u32>() > 3 * 127
    };
    let mut map = vec![u32::MAX; mesh.vertex_count()];
    let (mut inner, mut tris) = (Vec::new(), Vec::new());
    for t in 0..mesh.triangle_count() as u32 {
        if !masked(t) {
            continue;
        }
        let mut tri = [0u32; 3];
        for k in 0..3 {
            let v = mesh.indices[3 * t as usize + k] as usize;
            if map[v] == u32::MAX {
                map[v] = inner.len() as u32;
                inner.push(v as u32);
            }
            tri[k] = map[v];
        }
        tris.push(tri);
    }
    if tris.is_empty() {
        return None;
    }
    let n = inner.len() as u32;
    // Normals of the piece itself, so its rim does not lean with the polygons left behind.
    let mut normals = vec![Vec3::ZERO; inner.len()];
    for t in &tris {
        let [a, b, c] = t.map(|i| mesh.positions[inner[i as usize] as usize]);
        let face = (b - a).cross(c - a);
        for &i in t {
            normals[i as usize] += face;
        }
    }
    let mut positions: Vec<Vec3> = inner.iter().map(|&v| mesh.positions[v as usize]).collect();
    let outer: Vec<Vec3> = positions.iter().zip(&normals).map(|(p, nrm)| *p + nrm.normalize_or_zero() * thickness).collect();
    positions.extend(outer);
    // Outer layer as it was, inner layer turned inside out, walls along the open edges.
    let mut indices = Vec::with_capacity(tris.len() * 6);
    for t in &tris {
        indices.extend_from_slice(&[t[0] + n, t[1] + n, t[2] + n]);
        indices.extend_from_slice(&[t[0], t[2], t[1]]);
    }
    let mut edges: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    for t in &tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if edges[&(a.min(b), a.max(b))] == 1 {
                // The open edge a → b of the outer layer meets its wall as b → a.
                indices.extend_from_slice(&[b + n, a + n, a]);
                indices.extend_from_slice(&[b + n, a, b]);
            }
        }
    }
    let groups = vec![0u16; indices.len() / 3];
    Some(SculptMesh::from_raw_painted(positions, indices, groups, Vec::new()))
}

/// Geometry › Close Holes: every open border loop gets a cap, a fan of triangles around a
/// vertex at the loop's middle, in a new polygroup of its own. `None` when there is no hole.
pub fn holes_closed(mesh: &SculptMesh) -> Option<SculptMesh> {
    use std::collections::HashMap;
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in mesh.indices.chunks(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    // Border edges in their triangle's direction: a → b.
    let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
    for t in mesh.indices.chunks(3) {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if count[&(a.min(b), a.max(b))] == 1 {
                next.entry(a).or_default().push(b);
            }
        }
    }
    if next.is_empty() {
        return None;
    }
    let mut positions = mesh.positions.clone();
    let mut colors = mesh.colors.clone();
    let mut indices = mesh.indices.clone();
    let mut groups: Vec<u16> = (0..mesh.triangle_count() as u32).map(|t| mesh.group(t)).collect();
    let mut group = groups.iter().copied().max().unwrap_or(0);
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut loops: Vec<Vec<u32>> = Vec::new();
    for start in starts {
        while let Some(first) = next.get_mut(&start).and_then(|v| v.pop()) {
            // Follow the border round to where it began. Where two holes touch at a vertex
            // the walk passes it twice (a figure eight): each time it comes back to a vertex
            // already on the walk, the stretch since then is a loop of its own. One fan over
            // the figure eight put four triangles on an edge (non-manifold: Divide refused).
            let mut lp = vec![start];
            let mut on_walk: HashMap<u32, usize> = HashMap::from([(start, 0)]);
            let mut at = first;
            while at != start {
                if let Some(&i) = on_walk.get(&at) {
                    let closed = lp.split_off(i);
                    for v in &closed {
                        on_walk.remove(v);
                    }
                    loops.push(closed);
                }
                on_walk.insert(at, lp.len());
                lp.push(at);
                let Some(n) = next.get_mut(&at).and_then(|v| v.pop()) else { break };
                at = n;
                if lp.len() > mesh.vertex_count() {
                    break;
                }
            }
            if at == start {
                loops.push(lp);
            }
        }
    }
    for lp in loops {
        if lp.len() < 3 {
            continue;
        }
        let centre = positions.len() as u32;
        positions.push(lp.iter().map(|&v| mesh.positions[v as usize]).sum::<Vec3>() / lp.len() as f32);
        if !colors.is_empty() {
            let sum = lp.iter().fold([0u32; 4], |acc, &v| {
                let c = mesh.colors[v as usize];
                [acc[0] + c[0] as u32, acc[1] + c[1] as u32, acc[2] + c[2] as u32, acc[3] + c[3] as u32]
            });
            colors.push(sum.map(|x| (x / lp.len() as u32) as u8));
        }
        group = group.saturating_add(1);
        for k in 0..lp.len() {
            let (a, b) = (lp[k], lp[(k + 1) % lp.len()]);
            // The border runs a → b in its triangle; the cap meets it as b → a.
            indices.extend_from_slice(&[b, a, centre]);
            groups.push(group);
        }
    }
    (indices.len() > mesh.indices.len()).then(|| SculptMesh::from_raw_painted(positions, indices, groups, colors))
}

/// Reflect a copy of the shown mesh. Swapping its last two corners fixes winding
/// without changing vertex or triangle order, so paint, masks and UVs stay attached.
pub fn mirror_copy(source: &crate::multires::Multires, axis: usize) -> crate::multires::Multires {
    let mesh = source.mesh();
    let positions = mesh.positions.iter().map(|&p| reflect(p, axis)).collect();
    let mut indices = mesh.indices.clone();
    for tri in indices.chunks_exact_mut(3) {
        tri.swap(1, 2);
    }
    let mut copy = SculptMesh::from_chunked(positions, indices, &mesh.chunk_ranges());
    copy.colors.clone_from(&mesh.colors);
    copy.set_groups(mesh.groups.clone());
    copy.set_hidden(mesh.hidden.clone());
    crate::mask::replace(&mut copy, mesh.mask.clone());
    copy.set_quads(mesh.quads.iter().map(|&q| if q < crate::mesh::LONE { 2 - q } else { q }).collect());
    copy.set_creases(mesh.creases.iter().map(|&bits| swap_outer_bits(bits)).collect());

    let level = source.active();
    let mut doc = crate::multires::Multires::new(copy, source.budget);
    let mut uvs = if level == 0 { source.uvs().to_vec() } else { source.subdivided_uvs(level).unwrap_or_default() };
    for uv in &mut uvs {
        uv.swap(1, 2);
    }
    doc.swap_uvs(uvs);
    if level == 0 {
        let mut layout = source.obj_layout.clone();
        for corner in &mut layout.boundary {
            let slot = *corner % 3;
            if slot != 0 {
                *corner = *corner - slot + (3 - slot);
            }
        }
        let (mut start, mut prev) = (0, 0);
        for &end in &layout.ends {
            let count = (end - prev + 2) as usize;
            layout.boundary[start..start + count].reverse();
            start += count;
            prev = end;
        }
        for bits in &mut layout.mapped {
            *bits = (*bits & 1) | (*bits & 2) << 1 | (*bits & 4) >> 1;
        }
        doc.obj_layout = layout;
    }
    doc
}

fn reflect(mut p: Vec3, axis: usize) -> Vec3 {
    p[axis] = -p[axis];
    p
}

fn swap_outer_bits(bits: u8) -> u8 {
    (bits & 1) << 2 | (bits & 2) | (bits & 4) >> 2
}

/// Keep the positive side of the SubTool center, reflect it, and weld the plane.
pub fn mirrored_and_welded(mesh: &SculptMesh, axis: usize) -> Option<SculptMesh> {
    let plane = MirrorPlane { axis: Axis::ALL[axis], at: mesh.bounds().center()[axis] };
    positive_half(mesh, plane).map(|half| mirror_half(&half, plane))
}

/// Cut the mesh at the plane and keep its positive side.
pub fn positive_half(mesh: &SculptMesh, plane: MirrorPlane) -> Option<SculptMesh> {
    use std::collections::HashMap;
    let eps = 1e-6;
    let side = |p: Vec3| plane.distance(p) > eps;
    let on = |p: Vec3| plane.distance(p).abs() <= eps;
    let painted = !mesh.colors.is_empty();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut kept: HashMap<u32, u32> = HashMap::new();
    let mut cuts: HashMap<(u32, u32), u32> = HashMap::new();
    let (mut indices, mut groups) = (Vec::new(), Vec::new());
    let mut keep = |v: u32, positions: &mut Vec<Vec3>, colors: &mut Vec<[u8; 4]>| -> u32 {
        *kept.entry(v).or_insert_with(|| {
            let mut p = mesh.positions[v as usize];
            if on(p) {
                p = plane.project(p);
            }
            positions.push(p);
            if painted {
                colors.push(mesh.colors[v as usize]);
            }
            positions.len() as u32 - 1
        })
    };
    for t in 0..mesh.triangle_count() as u32 {
        let tri: Vec<u32> = mesh.indices[3 * t as usize..3 * t as usize + 3].to_vec();
        let inside = |v: u32| side(mesh.positions[v as usize]) || on(mesh.positions[v as usize]);
        if !tri.iter().any(|&v| side(mesh.positions[v as usize])) {
            continue;
        }
        // Clip the triangle to the positive half-space (Sutherland–Hodgman), sharing cut points per edge.
        let mut poly: Vec<u32> = Vec::with_capacity(4);
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
            if inside(a) {
                poly.push(keep(a, &mut positions, &mut colors));
            }
            let (da, db) = (plane.distance(pa), plane.distance(pb));
            if (da > eps && db < -eps) || (da < -eps && db > eps) {
                let key = (a.min(b), a.max(b));
                let cut = *cuts.entry(key).or_insert_with(|| {
                    let (lo, hi) = (mesh.positions[key.0 as usize], mesh.positions[key.1 as usize]);
                    let (lo_dist, hi_dist) = (plane.distance(lo), plane.distance(hi));
                    let f = lo_dist / (lo_dist - hi_dist);
                    positions.push(plane.project(lo.lerp(hi, f)));
                    if painted {
                        let (c0, c1) = (mesh.colors[key.0 as usize], mesh.colors[key.1 as usize]);
                        colors.push(std::array::from_fn(|i| (c0[i] as f32 + (c1[i] as f32 - c0[i] as f32) * f).round() as u8));
                    }
                    positions.len() as u32 - 1
                });
                poly.push(cut);
            }
        }
        for k in 1..poly.len().saturating_sub(1) {
            indices.extend_from_slice(&[poly[0], poly[k], poly[k + 1]]);
            groups.push(mesh.group(t));
        }
    }
    if indices.is_empty() {
        return None;
    }
    Some(SculptMesh::from_raw_painted(positions, indices, groups, colors))
}

/// Reflect the positive half and share its vertices on the mirror plane.
pub fn mirror_half(half: &SculptMesh, plane: MirrorPlane) -> SculptMesh {
    let painted = !half.colors.is_empty();
    let mut positions = half.positions.clone();
    let mut colors = half.colors.clone();
    let mut indices = half.indices.clone();
    let mut groups: Vec<u16> = (0..half.triangle_count() as u32).map(|t| half.group(t)).collect();
    let mut quads = half.quads.clone();
    // The mirror image: vertices off the plane reflected, triangles turned round.
    let half = positions.len();
    let mut mirror = vec![0u32; half];
    for (v, slot) in mirror.iter_mut().enumerate() {
        let p = positions[v];
        *slot = if plane.distance(p) == 0.0 {
            v as u32
        } else {
            positions.push(plane.reflect(p));
            if painted {
                colors.push(colors[v]);
            }
            positions.len() as u32 - 1
        };
    }
    let n = indices.len();
    for k in (0..n).step_by(3) {
        let (a, b, c) = (indices[k], indices[k + 1], indices[k + 2]);
        indices.extend_from_slice(&[mirror[a as usize], mirror[c as usize], mirror[b as usize]]);
        groups.push(groups[k / 3]);
        // Turned round, edge 0 (a-b) becomes edge 2 (b-a) and edge 2 (c-a) edge 0 (a-c).
        if !quads.is_empty() {
            let q = quads[k / 3];
            quads.push(if q < crate::mesh::LONE { 2 - q } else { q });
        }
    }
    // Triangles lying in the plane would face both ways once mirrored: drop the degenerate.
    let mut out_i = Vec::with_capacity(indices.len());
    let mut out_g = Vec::with_capacity(groups.len());
    let mut out_q = Vec::with_capacity(quads.len());
    for (k, t) in indices.chunks(3).enumerate() {
        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
            out_i.extend_from_slice(t);
            out_g.push(groups[k]);
            if !quads.is_empty() {
                out_q.push(quads[k]);
            }
        }
    }
    SculptMesh::from_raw_full(positions, out_i, out_g, colors, out_q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> SculptMesh {
        let (p, i) = crate::mesh::cube_sphere(10, 1.0);
        SculptMesh::from_raw(p, i)
    }

    /// Signed volume (closed, outward-facing meshes are positive).
    fn volume(m: &SculptMesh) -> f32 {
        m.indices.chunks(3).map(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|v| m.positions[v as usize]);
            a.dot(b.cross(c)) / 6.0
        }).sum()
    }

    #[test]
    fn parts_and_merges_keep_triangles_and_groups() {
        let mut m = sphere();
        let groups: Vec<u16> = (0..m.triangle_count() as u32).map(|t| (t % 3) as u16).collect();
        m.set_groups(groups);
        let two = part(&m, |t| m.group(t) == 2).unwrap();
        assert_eq!(two.triangle_count(), (0..m.triangle_count()).filter(|t| t % 3 == 2).count());
        assert!(part(&m, |_| false).is_none());
        let both = merged(&m, &sphere());
        assert_eq!(both.triangle_count(), 2 * m.triangle_count());
        assert_eq!(both.group_count(), 4, "the second sphere's group comes after the first's three");
        assert!((volume(&both) - 2.0 * volume(&m)).abs() < 1e-3);
    }

    #[test]
    fn extract_makes_a_closed_outward_shell_of_the_mask() {
        let mut m = sphere();
        assert!(extracted(&m, 0.05).is_none(), "nothing masked");
        m.mask = m.positions.iter().map(|p| if p.z > 0.5 { 255 } else { 0 }).collect();
        let shell = extracted(&m, 0.05).unwrap();
        // Closed: every edge is shared by exactly two triangles.
        let mut edges = std::collections::HashMap::new();
        for t in shell.indices.chunks(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_insert(0u32) += 1;
            }
        }
        assert!(edges.values().all(|&c| c == 2));
        // Outward, and about the masked area times the thickness.
        let v = volume(&shell);
        let area = 2.0 * std::f32::consts::PI * (1.0 - 0.5); // a spherical cap above z = 0.5
        assert!(v > 0.0 && (v - area * 0.05).abs() < area * 0.05 * 0.35, "{v} vs {}", area * 0.05);
    }

    fn edge_counts(m: &SculptMesh) -> std::collections::HashMap<(u32, u32), u32> {
        let mut edges = std::collections::HashMap::new();
        for t in m.indices.chunks(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_insert(0u32) += 1;
            }
        }
        edges
    }

    #[test]
    fn close_holes_caps_every_border_loop_outward() {
        let m = sphere();
        assert!(holes_closed(&m).is_none(), "a sphere has no holes");
        // Cut two caps off the sphere: two holes.
        let open = part(&m, |t| {
            let i = 3 * t as usize;
            let z = m.indices[i..i + 3].iter().map(|&v| m.positions[v as usize].z).sum::<f32>() / 3.0;
            z.abs() < 0.7
        }).unwrap();
        let closed = holes_closed(&open).unwrap();
        assert!(edge_counts(&closed).values().all(|&c| c == 2), "closed");
        assert!(volume(&closed) > 0.0);
        assert_eq!(closed.group_count(), 3, "the band and one group per cap");
    }

    /// Two holes that touch at one vertex (Del Hidden leaves these): the border passes that
    /// vertex twice. Each hole needs its own cap, or the fan puts four triangles on an edge
    /// and Divide refuses the mesh.
    #[test]
    fn close_holes_caps_holes_that_touch_at_a_vertex_separately() {
        let m = sphere();
        let tri = |t: usize| [m.indices[3 * t], m.indices[3 * t + 1], m.indices[3 * t + 2]];
        let a = tri(0);
        let b = (1..m.triangle_count())
            .find(|&t| tri(t).iter().filter(|v| a.contains(v)).count() == 1)
            .unwrap();
        let open = part(&m, |t| t != 0 && t as usize != b).unwrap();
        let closed = holes_closed(&open).unwrap();
        assert!(edge_counts(&closed).values().all(|&c| c == 2), "closed and two-manifold");
        assert_eq!(closed.subdivision_blocker(), None);
    }

    #[test]
    fn mirror_and_weld_makes_a_closed_symmetric_mesh() {
        // A sphere pushed out on +X only.
        let (p, i) = crate::mesh::cube_sphere(10, 1.0);
        let p: Vec<Vec3> = p.into_iter().map(|q| if q.x > 0.0 { q + Vec3::new(q.x * 0.4, 0.0, 0.0) } else { q }).collect();
        let p = p.into_iter().map(|p| p + 4.0 * Vec3::X).collect();
        let m = SculptMesh::from_raw(p, i);
        let plane = MirrorPlane { axis: Axis::X, at: m.bounds().center().x };
        let w = mirrored_and_welded(&m, 0).unwrap();
        assert!(edge_counts(&w).values().all(|&c| c == 2), "welded shut");
        assert!(volume(&w) > 0.0);
        // Every vertex has its mirror image.
        for q in &w.positions {
            let mirror = plane.reflect(*q);
            assert!(w.positions.iter().any(|r| r.distance(mirror) < 1e-5), "{q:?}");
        }
        // The +X side kept its shape: the widest point is the pushed one.
        let max_x = w.positions.iter().map(|q| q.x).fold(f32::MIN, f32::max);
        assert_eq!(max_x, m.bounds().max.x);
    }
}
