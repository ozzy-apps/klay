// GPU grab brush: positions[sel[i]] = orig[i] + delta * weight[i].
// A negative weight marks a vertex on the mirrored side of a symmetric stroke: it
// moves by |weight| with the X component of the delta flipped.

struct Params {
    delta: vec3<f32>,
    count: u32,
};

@group(0) @binding(0) var<storage, read_write> positions: array<f32>; // 3 floats per vertex
@group(0) @binding(1) var<storage, read> sel: array<u32>;
@group(0) @binding(2) var<storage, read> orig: array<f32>;            // 3 floats per selected vertex
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 256u;
    if (i >= params.count) {
        return;
    }
    let v = sel[i];
    let ws = weights[i];
    let w = abs(ws);
    positions[3u * v] = orig[3u * i] + params.delta.x * ws;
    positions[3u * v + 1u] = orig[3u * i + 1u] + params.delta.y * w;
    positions[3u * v + 2u] = orig[3u * i + 2u] + params.delta.z * w;
}
