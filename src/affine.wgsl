// Map every vertex of a level by an affine matrix: the GPU side of a Transpose move, scale
// or rotate, so the shown level is not uploaded again after the CPU applied the same map.

struct Params {
    model: mat4x4<f32>,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read_write> positions: array<f32>; // 3 floats per vertex
@group(0) @binding(1) var<uniform> params: Params;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 256u;
    if (i >= params.count) {
        return;
    }
    let p = vec3<f32>(positions[3u * i], positions[3u * i + 1u], positions[3u * i + 2u]);
    let q = (params.model * vec4<f32>(p, 1.0)).xyz;
    positions[3u * i] = q.x;
    positions[3u * i + 1u] = q.y;
    positions[3u * i + 2u] = q.z;
}
