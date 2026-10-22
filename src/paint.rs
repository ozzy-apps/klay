//! Premultiplied sRGB polypaint. Empty arrays mean unpainted clay.
use crate::mesh::SculptMesh;
use rayon::prelude::*;
pub type Color = [u8; 4];
pub type Chunks = Vec<(u32, Vec<Color>)>;

pub struct Snapshot {
    pub active: usize,
    pub levels: Vec<(usize, Chunks)>,
}

impl Snapshot {
    pub fn bytes(&self) -> usize {
        self.levels
            .iter()
            .flat_map(|(_, c)| c)
            .map(|(_, v)| v.len() * 4)
            .sum()
    }
}

pub fn at(mesh: &SculptMesh, v: usize) -> Color {
    mesh.colors.get(v).copied().unwrap_or([0; 4])
}

pub fn ensure(mesh: &mut SculptMesh) {
    if mesh.colors.is_empty() {
        mesh.colors = vec![[0; 4]; mesh.vertex_count()];
    }
}

pub fn snapshot(mesh: &SculptMesh, chunks: &[u32]) -> Chunks {
    chunks
        .iter()
        .map(|&c| {
            let (a, b) = mesh.chunks[c as usize].range();
            (
                c,
                if mesh.colors.is_empty() {
                    vec![[0; 4]; b - a]
                } else {
                    mesh.colors[a..b].to_vec()
                },
            )
        })
        .collect()
}

pub fn restore(mesh: &mut SculptMesh, data: Chunks) -> Chunks {
    ensure(mesh);
    let mut inverse = Vec::with_capacity(data.len());
    for (c, mut values) in data {
        let (a, b) = mesh.chunks[c as usize].range();
        assert_eq!(b - a, values.len());
        values.swap_with_slice(&mut mesh.colors[a..b]);
        inverse.push((c, values));
        mesh.chunks[c as usize].dirty_color = true;
    }
    inverse
}

pub fn blend(old: Color, rgb: [f32; 3], amount: f32) -> Color {
    let k = amount.clamp(0.0, 1.0);
    let a = (old[3] as f32 + (255.0 - old[3] as f32) * k).round() as u8;
    [
        ((old[0] as f32 * (1.0 - k) + rgb[0].clamp(0.0, 1.0) * 255.0 * k).round() as u8).min(a),
        ((old[1] as f32 * (1.0 - k) + rgb[1].clamp(0.0, 1.0) * 255.0 * k).round() as u8).min(a),
        ((old[2] as f32 * (1.0 - k) + rgb[2].clamp(0.0, 1.0) * 255.0 * k).round() as u8).min(a),
        a,
    ]
}

pub fn scatter(mesh: &mut SculptMesh, sel: &crate::mesh::Selection, values: &[Color]) {
    ensure(mesh);
    let ranges: Vec<_> = sel
        .groups
        .iter()
        .map(|g| mesh.chunks[g.chunk as usize].range())
        .collect();
    crate::mesh::split_ranges(&mut mesh.colors, ranges.iter().copied())
        .into_par_iter()
        .zip(sel.groups.par_iter())
        .zip(ranges.par_iter())
        .for_each(|((slice, g), &(start, _))| {
            for i in g.start as usize..g.end as usize {
                slice[sel.verts[i] as usize - start] = values[i];
            }
        });
    for g in &sel.groups {
        mesh.chunks[g.chunk as usize].dirty_color = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paint_coverage_is_premultiplied_and_bounded() {
        let p = blend([0; 4], [1.0, 0.0, 0.0], 0.5);
        assert_eq!(p, [128, 0, 0, 128]);
        assert_eq!(blend(p, [0.0, 1.0, 0.0], 1.0), [0, 255, 0, 255]);
        assert_eq!(blend(p, [0.0; 3], 0.0), p);
    }
}
