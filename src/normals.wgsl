// Recompute packed vertex normals for every vertex whose chunk is flagged dirty.

struct Params {
    vertex_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> positions: array<f32>;      // 3 floats per vertex
@group(0) @binding(1) var<storage, read> indices: array<u32>;        // 3 per triangle
@group(0) @binding(2) var<storage, read> vt_offsets: array<u32>;     // vertex -> triangle CSR
@group(0) @binding(3) var<storage, read> vt_tris: array<u32>;
@group(0) @binding(4) var<storage, read> vert_chunk: array<u32>;     // owning chunk per vertex
@group(0) @binding(5) var<storage, read> chunk_dirty: array<u32>;    // 1 = recompute
@group(0) @binding(6) var<storage, read_write> normals: array<u32>;  // packed snorm8x4
@group(0) @binding(7) var<uniform> params: Params;

fn pos(i: u32) -> vec3<f32> {
    return vec3<f32>(positions[3u * i], positions[3u * i + 1u], positions[3u * i + 2u]);
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // 2D dispatch so meshes above 16M vertices fit within the 65535 workgroup limit.
    let v = gid.x + gid.y * 65535u * 256u;
    if (v >= params.vertex_count) {
        return;
    }
    if (chunk_dirty[vert_chunk[v]] == 0u) {
        return;
    }
    var n = vec3<f32>(0.0);
    let t0 = vt_offsets[v];
    let t1 = vt_offsets[v + 1u];
    for (var k = t0; k < t1; k++) {
        let t = vt_tris[k];
        let a = pos(indices[3u * t]);
        let b = pos(indices[3u * t + 1u]);
        let c = pos(indices[3u * t + 2u]);
        n += cross(b - a, c - a);
    }
    let len = length(n);
    if (len > 0.0) {
        n = n / len;
    } else {
        n = vec3<f32>(0.0, 1.0, 0.0);
    }
    normals[v] = pack4x8snorm(vec4<f32>(n, 0.0));
}
