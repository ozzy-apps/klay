// Packed vertex normals for the chunks flagged dirty, in three passes over a work list of
// chunk ids (flagged chunks first, then the chunks that share an edge with them).
//
// The index buffer is grouped by chunk, so a chunk's triangles are one range, with the
// triangles that reach into another chunk at its front. A flagged vertex's triangles are
// all owned by its own chunk or by an edge neighbour, so:
// 1. `cs_own`, one workgroup per flagged chunk: walks all of the chunk's triangles and sums
//    the face normals of its own vertices in workgroup memory, in fixed point, then stores
//    the sums to the accumulators.
// 2. `cs_border`, one workgroup per listed chunk: walks only the chunk's border triangles
//    and adds their face normals to the accumulators of the vertices they touch in other,
//    flagged chunks (atomically: several chunks reach the same vertex).
// 3. `cs_normalize`, one workgroup per flagged chunk: packs the unit normals.
// Dispatches in one compute pass run in order, so each pass sees the previous one's writes.

struct Params {
    // Face normals are multiplied by this before the integer add.
    scale: f32,
    // Largest magnitude one face may contribute, in fixed point.
    cap: f32,
    chunk_count: u32,
    // Flagged chunks (the head of the work list) and listed chunks in all.
    flagged: u32,
    listed: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> positions: array<f32>;         // 3 floats per vertex
@group(0) @binding(1) var<storage, read> indices: array<u32>;           // 3 per triangle, grouped by chunk
@group(0) @binding(2) var<storage, read> chunk_verts: array<u32>;       // chunk -> first vertex, chunk_count + 1 entries
@group(0) @binding(3) var<storage, read> chunk_tris: array<u32>;        // chunk -> first triangle, chunk_count + 1 entries
@group(0) @binding(4) var<storage, read> chunk_border: array<u32>;      // chunk -> end of its border triangles
@group(0) @binding(5) var<storage, read> chunk_flag: array<u32>;        // 1 = recompute this chunk's normals
@group(0) @binding(6) var<storage, read> work: array<u32>;              // chunk ids, flagged ones first
@group(0) @binding(7) var<storage, read_write> acc: array<atomic<i32>>; // 3 per vertex, fixed point
@group(0) @binding(8) var<storage, read_write> normals: array<u32>;     // packed snorm8x4
@group(0) @binding(9) var<uniform> params: Params;

const WG: u32 = 256u;
// Three accumulators for each of the at most 4096 vertices of a chunk (mesh.rs CHUNK_TARGET).
var<workgroup> own_acc: array<atomic<i32>, 12288>;

fn pos(i: u32) -> vec3<f32> {
    return vec3<f32>(positions[3u * i], positions[3u * i + 1u], positions[3u * i + 2u]);
}

// Fixed-point face normal of triangle `t`.
fn face(t: u32) -> vec3<i32> {
    let a = pos(indices[3u * t]);
    var n = cross(pos(indices[3u * t + 1u]) - a, pos(indices[3u * t + 2u]) - a) * params.scale;
    let len = length(n);
    if (len > params.cap) {
        n *= params.cap / len;
    }
    return vec3<i32>(round(n));
}

// Owning chunk of vertex `v`: the last chunk whose first vertex is at or below it.
fn chunk_of(v: u32) -> u32 {
    var lo = 0u;
    var hi = params.chunk_count;
    while (hi - lo > 1u) {
        let mid = (lo + hi) / 2u;
        if (chunk_verts[mid] <= v) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    return lo;
}

fn add_own(v: u32, vs: u32, ve: u32, q: vec3<i32>) {
    if (v >= vs && v < ve) {
        let i = 3u * (v - vs);
        atomicAdd(&own_acc[i], q.x);
        atomicAdd(&own_acc[i + 1u], q.y);
        atomicAdd(&own_acc[i + 2u], q.z);
    }
}

fn add_other(v: u32, vs: u32, ve: u32, q: vec3<i32>) {
    if ((v < vs || v >= ve) && chunk_flag[chunk_of(v)] != 0u) {
        atomicAdd(&acc[3u * v], q.x);
        atomicAdd(&acc[3u * v + 1u], q.y);
        atomicAdd(&acc[3u * v + 2u], q.z);
    }
}

// 2D dispatch so lists above 65535 workgroups fit.
fn job_of(wg: vec3<u32>) -> u32 {
    return wg.x + wg.y * 65535u;
}

@compute @workgroup_size(256)
fn cs_own(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let job = job_of(wg);
    if (job >= params.flagged) {
        return;
    }
    let c = work[job];
    let vs = chunk_verts[c];
    let ve = chunk_verts[c + 1u];
    let n_own = 3u * (ve - vs);
    for (var i = lid.x; i < n_own; i += WG) {
        atomicStore(&own_acc[i], 0);
    }
    workgroupBarrier();
    let t_end = chunk_tris[c + 1u];
    for (var t = chunk_tris[c] + lid.x; t < t_end; t += WG) {
        let q = face(t);
        add_own(indices[3u * t], vs, ve, q);
        add_own(indices[3u * t + 1u], vs, ve, q);
        add_own(indices[3u * t + 2u], vs, ve, q);
    }
    workgroupBarrier();
    for (var i = lid.x; i < n_own; i += WG) {
        atomicStore(&acc[3u * vs + i], atomicLoad(&own_acc[i]));
    }
}

@compute @workgroup_size(256)
fn cs_border(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let job = job_of(wg);
    if (job >= params.listed) {
        return;
    }
    let c = work[job];
    let vs = chunk_verts[c];
    let ve = chunk_verts[c + 1u];
    let t_end = chunk_border[c];
    for (var t = chunk_tris[c] + lid.x; t < t_end; t += WG) {
        let q = face(t);
        add_other(indices[3u * t], vs, ve, q);
        add_other(indices[3u * t + 1u], vs, ve, q);
        add_other(indices[3u * t + 2u], vs, ve, q);
    }
}

@compute @workgroup_size(256)
fn cs_normalize(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let job = job_of(wg);
    if (job >= params.flagged) {
        return;
    }
    let c = work[job];
    let v_end = chunk_verts[c + 1u];
    for (var v = chunk_verts[c] + lid.x; v < v_end; v += WG) {
        var n = vec3<f32>(
            f32(atomicLoad(&acc[3u * v])),
            f32(atomicLoad(&acc[3u * v + 1u])),
            f32(atomicLoad(&acc[3u * v + 2u])),
        );
        let len = length(n);
        if (len > 0.0) {
            n = n / len;
        } else {
            n = vec3<f32>(0.0, 1.0, 0.0);
        }
        normals[v] = pack4x8snorm(vec4<f32>(n, 0.0));
    }
}
