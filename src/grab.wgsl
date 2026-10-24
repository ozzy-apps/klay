// GPU grab brush: positions[sel[i]] = orig[i] + deltas[image of i] * weight[i]. With
// symmetry each selected vertex belongs to one image of the brush (symmetry.rs) and moves
// with that image's move; without it every vertex is image 0.

struct Params {
    count: u32,
    deltas: array<vec4<f32>, 64>,
};

@group(0) @binding(0) var<storage, read_write> positions: array<f32>; // 3 floats per vertex
@group(0) @binding(1) var<storage, read> sel: array<u32>;
@group(0) @binding(2) var<storage, read> orig: array<f32>;            // 3 floats per selected vertex
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> images: array<u32>;          // 4 image indices per word

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 256u;
    if (i >= params.count) {
        return;
    }
    let v = sel[i];
    let w = weights[i];
    let k = (images[i >> 2u] >> ((i & 3u) * 8u)) & 255u;
    let d = params.deltas[k].xyz * w;
    positions[3u * v] = orig[3u * i] + d.x;
    positions[3u * v + 1u] = orig[3u * i + 1u] + d.y;
    positions[3u * v + 2u] = orig[3u * i + 2u] + d.z;
}
