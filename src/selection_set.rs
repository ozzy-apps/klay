//! Named, topology-bound snapshots of a SubTool's selection data.
use crate::project::{In, Out};
use glam::{Vec2, Vec3};
use std::io::{self, Read, Write};

#[derive(Clone, Copy)]
pub enum Channel {
    Mask,
    Groups,
    Uvs,
    Morph,
}

#[derive(Clone)]
pub struct Set {
    pub name: String,
    pub level: usize,
    pub topology: u64,
    pub mask: Vec<u8>,
    pub groups: Vec<u16>,
    pub uvs: Vec<[Vec2; 3]>,
    pub uv_mapped: Vec<u8>,
    pub morph: Option<(usize, Vec<Vec3>)>,
}

pub fn topology(indices: &[u32], vertices: usize) -> u64 {
    indices
        .iter()
        .fold(vertices as u64 ^ 0xcbf29ce484222325, |h, &i| {
            (h ^ i as u64).wrapping_mul(0x100000001b3)
        })
}

pub fn write<W: Write>(sets: &[Set], out: &mut Out<W>) -> io::Result<()> {
    out.u32(sets.len() as u32)?;
    for s in sets {
        out.slice(s.name.as_bytes())?;
        out.u32(s.level as u32)?;
        out.u64(s.topology)?;
        out.slice(&s.mask)?;
        out.slice(&s.groups)?;
        out.slice(&s.uvs)?;
        out.slice(&s.uv_mapped)?;
        if let Some((level, positions)) = &s.morph {
            out.u8(1)?;
            out.u32(*level as u32)?;
            out.slice(positions)?;
        } else {
            out.u8(0)?;
        }
    }
    Ok(())
}

pub fn read<R: Read>(input: &mut In<R>) -> Result<Vec<Set>, String> {
    let count = input.u32()?;
    if count > 128 {
        return Err("Too many selection sets".into());
    }
    (0..count)
        .map(|_| {
            let name = String::from_utf8(input.vec()?).map_err(|_| "Invalid selection set name")?;
            let level = input.u32()? as usize;
            let topology = input.u64()?;
            let mask = input.vec()?;
            let groups = input.vec()?;
            let uvs: Vec<[Vec2; 3]> = input.vec()?;
            let uv_mapped: Vec<u8> = input.vec()?;
            let morph: Option<(usize, Vec<Vec3>)> = if input.u8()? == 0 {
                None
            } else {
                Some((input.u32()? as usize, input.vec()?))
            };
            if name.is_empty()
                || name.len() > 320
                || level > 16
                || uvs.iter().flatten().any(|p| !p.is_finite())
                || (!uv_mapped.is_empty()
                    && (uv_mapped.len() != uvs.len() || uv_mapped.iter().any(|&b| b > 7)))
                || morph
                    .as_ref()
                    .is_some_and(|(l, p)| *l > 16 || p.iter().any(|p| !p.is_finite()))
            {
                return Err("Invalid selection set".into());
            }
            Ok(Set {
                name,
                level,
                topology,
                mask,
                groups,
                uvs,
                uv_mapped,
                morph,
            })
        })
        .collect()
}
