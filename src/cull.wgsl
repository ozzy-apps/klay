// GPU chunk culling: one thread per chunk of the shown level writes the indexed draw that
// draws it, empty when its box is outside the view (or the document frame) or all its
// triangles face away from the camera. The draws feed one `multi_draw_indexed_indirect`,
// in chunk order, so what is drawn comes out exactly as one draw of the whole level.

struct Params {
    // Frustum planes in world space (inside: dot(xyz, p) + w >= 0), already narrowed to
    // the document frame.
    planes: array<vec4<f32>, 6>,
    // xyz: camera position; w: 1 for an orthographic camera.
    eye: vec4<f32>,
    // Viewing direction (orthographic cameras).
    forward: vec4<f32>,
    chunk_count: u32,
    // 1: drop chunks whose triangles all face away (off for Live Boolean, which draws
    // back faces).
    cone: u32,
    // 1: drop chunks outside the frustum (off while a Transpose preview moves vertices in
    // the vertex shader).
    frustum: u32,
    // 1: each draw's first instance is its first triangle, for the polygroup pass's
    // primitive index (needs INDIRECT_FIRST_INSTANCE).
    first_instance: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
};

struct Bounds {
    min: vec4<f32>,
    max: vec4<f32>,
    axis: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> bounds: array<Bounds>;
// Per chunk: first triangle in the drawn index buffer and the triangle count.
@group(0) @binding(1) var<storage, read> ranges: array<vec2<u32>>;
// DrawIndexedIndirectArgs, five words each.
@group(0) @binding(2) var<storage, read_write> draws: array<u32>;
// [0]: chunks drawn; [1]: triangles drawn.
@group(0) @binding(3) var<storage, read_write> counts: array<atomic<u32>, 4>;
@group(0) @binding(4) var<uniform> params: Params;

fn visible(b: Bounds) -> bool {
    let lo = b.min.xyz;
    let hi = b.max.xyz;
    if (any(lo > hi)) {
        return false;
    }
    if (params.frustum == 1u) {
        for (var i = 0u; i < 6u; i++) {
            let pl = params.planes[i];
            // The box corner furthest along the plane's normal.
            let p = select(lo, hi, pl.xyz > vec3<f32>(0.0));
            if (dot(pl.xyz, p) + pl.w < 0.0) {
                return false;
            }
        }
    }
    let cutoff = b.min.w;
    if (params.cone == 1u && cutoff < 1.0) {
        let axis = b.axis.xyz;
        if (params.eye.w > 0.5) {
            if (dot(params.forward.xyz, axis) >= cutoff) {
                return false;
            }
        } else {
            let c = (lo + hi) * 0.5;
            let r = length(hi - lo) * 0.5;
            let v = c - params.eye.xyz;
            if (dot(v, axis) >= cutoff * length(v) + r) {
                return false;
            }
        }
    }
    return true;
}

@compute @workgroup_size(64)
fn cs_cull(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>) {
    let c = g.x + g.y * n.x * 64u;
    if (c >= params.chunk_count) {
        return;
    }
    let range = ranges[c];
    let shown = range.y > 0u && visible(bounds[c]);
    if (shown) {
        atomicAdd(&counts[0], 1u);
        atomicAdd(&counts[1], range.y);
    }
    let o = 5u * c;
    draws[o] = 3u * range.y;
    draws[o + 1u] = select(0u, 1u, shown);
    draws[o + 2u] = 3u * range.x;
    draws[o + 3u] = 0u;
    draws[o + 4u] = select(0u, range.x, params.first_instance == 1u);
}
