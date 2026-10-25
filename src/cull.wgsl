// GPU chunk culling: one thread per chunk of the shown level writes the indexed draw that
// draws it, empty when its box is outside the view (or the document frame) or all its
// triangles face away from the camera. The draws feed one `multi_draw_indexed_indirect`,
// in chunk order, so what is drawn comes out exactly as one draw of the whole level.
//
// A visible chunk whose vertices are packed denser on screen than `splat.y` per pixel is
// drawn as points instead (splat.wgsl): its draw stays empty and its mode says so.

struct Params {
    view_proj: mat4x4<f32>,
    // The viewport in framebuffer pixels: x, y, width, height.
    viewport: vec4<f32>,
    // x: 1 when dense chunks may be drawn as points; y: vertices per pixel from which
    // they are.
    splat: vec4<f32>,
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
    // 1: drop chunks outside the frustum.
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
// [0]: chunks drawn as triangles; [1]: triangles drawn; [2]: chunks drawn as points;
// [3]: their vertices.
@group(0) @binding(3) var<storage, read_write> counts: array<atomic<u32>, 4>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> chunk_verts: array<u32>;   // chunk -> first vertex, chunk_count + 1 entries
@group(0) @binding(6) var<storage, read> chunk_tris: array<u32>;    // chunk -> first triangle, chunk_count + 1 entries
// Per chunk: 0 not drawn, 1 triangles, 2 points.
@group(0) @binding(7) var<storage, read_write> modes: array<u32>;

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

// Whether chunk `c` with box `b` packs at least `splat.y` vertices into each pixel of its
// box's screen rectangle. The rectangle is larger than the surface it holds, so this errs
// towards triangles; a box reaching behind the camera never counts.
fn dense(c: u32, b: Bounds) -> bool {
    var lo = vec2<f32>(3.0e38);
    var hi = vec2<f32>(-3.0e38);
    for (var k = 0u; k < 8u; k++) {
        let corner = select(b.min.xyz, b.max.xyz, vec3<bool>((k & 1u) != 0u, (k & 2u) != 0u, (k & 4u) != 0u));
        let clip = params.view_proj * vec4<f32>(corner, 1.0);
        if (clip.w <= 1.0e-6) {
            return false;
        }
        let px = (clip.xy / clip.w) * vec2<f32>(0.5, -0.5) * params.viewport.zw;
        lo = min(lo, px);
        hi = max(hi, px);
    }
    let area = max(hi.x - lo.x, 1.0) * max(hi.y - lo.y, 1.0);
    let verts = f32(chunk_verts[c + 1u] - chunk_verts[c]);
    return verts >= params.splat.y * area;
}

@compute @workgroup_size(64)
fn cs_cull(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>) {
    let c = g.x + g.y * n.x * 64u;
    if (c >= params.chunk_count) {
        return;
    }
    let range = ranges[c];
    let b = bounds[c];
    let shown = range.y > 0u && visible(b);
    // Points only for chunks with nothing hidden (a point stands for all its triangles).
    let whole = range.y == chunk_tris[c + 1u] - chunk_tris[c];
    let points = shown && params.splat.x > 0.5 && whole && dense(c, b);
    let triangles = shown && !points;
    if (triangles) {
        atomicAdd(&counts[0], 1u);
        atomicAdd(&counts[1], range.y);
    }
    if (points) {
        atomicAdd(&counts[2], 1u);
        atomicAdd(&counts[3], chunk_verts[c + 1u] - chunk_verts[c]);
    }
    modes[c] = select(select(0u, 1u, triangles), 2u, points);
    let o = 5u * c;
    draws[o] = 3u * range.y;
    draws[o + 1u] = select(0u, 1u, triangles);
    draws[o + 2u] = 3u * range.x;
    draws[o + 3u] = 0u;
    draws[o + 4u] = select(0u, range.x, params.first_instance == 1u);
}
