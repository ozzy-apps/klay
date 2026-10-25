// Point splats for dense chunks (cull.wgsl marks them with mode 2): one workgroup per
// chunk, one thread per owned vertex. Each front-facing vertex lands in the pixel it
// projects to, and the nearest one wins: `(inverted depth << 32) | vertex` is kept with a
// 64-bit atomicMax, so a larger key is a nearer vertex. The resolve pass (`fs_splat` in
// shader.wgsl) shades the winners into the frame and the depth buffer.

struct Params {
    view_proj: mat4x4<f32>,
    // xyz: camera position; w: 1 for an orthographic camera.
    eye: vec4<f32>,
    // Viewing direction (orthographic cameras).
    forward: vec4<f32>,
    // The viewport in framebuffer pixels: x, y, width, height.
    viewport: vec4<f32>,
    // The document frame in framebuffer pixels: x0, y0, x1, y1 (points land only inside).
    frame: vec4<f32>,
    // Splat buffer width and height (the framebuffer's), and the level's chunk count.
    width: u32,
    height: u32,
    chunk_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> positions: array<f32>;
@group(0) @binding(1) var<storage, read> normals: array<u32>;
@group(0) @binding(2) var<storage, read> chunk_verts: array<u32>;
@group(0) @binding(3) var<storage, read> modes: array<u32>;
@group(0) @binding(4) var<storage, read_write> splats: array<atomic<u64>>;
@group(0) @binding(5) var<uniform> params: Params;

const WG: u32 = 256u;

fn keep(i: u32, key: u64) {
    //EARLY if (atomicLoad(&splats[i]) >= key) { return; }
    atomicMax(&splats[i], key);
}

@compute @workgroup_size(256)
fn cs_splat(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let c = wg.x + wg.y * 65535u;
    if (c >= params.chunk_count || modes[c] != 2u) {
        return;
    }
    let end = chunk_verts[c + 1u];
    for (var v = chunk_verts[c] + lid.x; v < end; v += WG) {
        let p = vec3<f32>(positions[3u * v], positions[3u * v + 1u], positions[3u * v + 2u]);
        let n = unpack4x8snorm(normals[v]).xyz;
        // Facing away: hidden behind the front surface, or past the silhouette.
        var view = p - params.eye.xyz;
        if (params.eye.w > 0.5) {
            view = params.forward.xyz;
        }
        if (dot(n, view) > 0.0) {
            continue;
        }
        let clip = params.view_proj * vec4<f32>(p, 1.0);
        if (clip.w <= 0.0) {
            continue;
        }
        let ndc = clip.xyz / clip.w;
        if (ndc.z < 0.0 || ndc.z > 1.0) {
            continue;
        }
        let px = params.viewport.xy + (ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5)) * params.viewport.zw;
        if (any(px < params.frame.xy) || any(px >= params.frame.zw)) {
            continue;
        }
        let x = min(u32(px.x), params.width - 1u);
        let y = min(u32(px.y), params.height - 1u);
        let key = (u64(0xffffffffu - bitcast<u32>(ndc.z)) << 32u) | u64(v);
        keep(y * params.width + x, key);
    }
}
