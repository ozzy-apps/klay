// Per-chunk bounds for GPU culling (cull.wgsl): the box of every vertex the chunk's
// triangles use, and a cone holding the facing of all its triangles. One workgroup per
// listed chunk; run after the normal pass, on the positions as drawn.
//
// The cone (axis, cutoff) follows meshoptimizer's cluster cones: `cutoff` is the sine of
// the widest angle between the axis and a face normal, and 1 when the faces spread too wide
// for the chunk ever to be wholly back-facing.

struct Params {
    // Chunks to visit: `work[0..listed]`, or every chunk when `all` is 1.
    listed: u32,
    all: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Bounds {
    // xyz: box corner; min.w: cone cutoff.
    min: vec4<f32>,
    max: vec4<f32>,
    axis: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> positions: array<f32>;
@group(0) @binding(1) var<storage, read> indices: array<u32>;
@group(0) @binding(2) var<storage, read> chunk_tris: array<u32>;      // chunk -> first triangle, chunk_count + 1 entries
@group(0) @binding(3) var<storage, read> work: array<u32>;
@group(0) @binding(4) var<storage, read_write> bounds: array<Bounds>;
@group(0) @binding(5) var<uniform> params: Params;

const WG: u32 = 256u;
var<workgroup> red_lo: array<vec3<f32>, 256>;
var<workgroup> red_hi: array<vec3<f32>, 256>;
var<workgroup> red_n: array<vec3<f32>, 256>;
var<workgroup> red_dot: array<f32, 256>;

fn pos(i: u32) -> vec3<f32> {
    return vec3<f32>(positions[3u * i], positions[3u * i + 1u], positions[3u * i + 2u]);
}

// Unit face normal of triangle `t` (zero for a degenerate one).
fn face(t: u32) -> vec3<f32> {
    let a = pos(indices[3u * t]);
    let n = cross(pos(indices[3u * t + 1u]) - a, pos(indices[3u * t + 2u]) - a);
    let len = length(n);
    return select(vec3<f32>(0.0), n / len, len > 0.0);
}

@compute @workgroup_size(256)
fn cs_bounds(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let job = wg.x + wg.y * 65535u;
    if (params.all == 0u && job >= params.listed) {
        return;
    }
    var c = job;
    if (params.all == 0u) {
        c = work[job];
    }
    let t0 = chunk_tris[c];
    let t1 = chunk_tris[c + 1u];
    var lo = vec3<f32>(3.0e38);
    var hi = vec3<f32>(-3.0e38);
    var sum = vec3<f32>(0.0);
    for (var t = t0 + lid.x; t < t1; t += WG) {
        for (var k = 0u; k < 3u; k++) {
            let p = pos(indices[3u * t + k]);
            lo = min(lo, p);
            hi = max(hi, p);
        }
        sum += face(t);
    }
    red_lo[lid.x] = lo;
    red_hi[lid.x] = hi;
    red_n[lid.x] = sum;
    workgroupBarrier();
    for (var s = WG / 2u; s > 0u; s >>= 1u) {
        if (lid.x < s) {
            red_lo[lid.x] = min(red_lo[lid.x], red_lo[lid.x + s]);
            red_hi[lid.x] = max(red_hi[lid.x], red_hi[lid.x + s]);
            red_n[lid.x] += red_n[lid.x + s];
        }
        workgroupBarrier();
    }
    let total = red_n[0];
    let len = length(total);
    let axis = select(vec3<f32>(0.0, 0.0, 1.0), total / len, len > 0.0);
    var least = 1.0;
    for (var t = t0 + lid.x; t < t1; t += WG) {
        let n = face(t);
        if (dot(n, n) > 0.0) {
            least = min(least, dot(axis, n));
        }
    }
    red_dot[lid.x] = least;
    workgroupBarrier();
    for (var s = WG / 2u; s > 0u; s >>= 1u) {
        if (lid.x < s) {
            red_dot[lid.x] = min(red_dot[lid.x], red_dot[lid.x + s]);
        }
        workgroupBarrier();
    }
    if (lid.x == 0u) {
        let d = red_dot[0];
        // Faces spread past about 84 degrees from the axis: never back-facing as a whole.
        let cutoff = select(sqrt(max(1.0 - d * d, 0.0)), 1.0, d <= 0.1 || len <= 0.0);
        var b: Bounds;
        if (t1 > t0) {
            b.min = vec4<f32>(red_lo[0], cutoff);
            b.max = vec4<f32>(red_hi[0], 0.0);
        } else {
            // No triangles: an empty box, never drawn.
            b.min = vec4<f32>(vec3<f32>(3.0e38), 1.0);
            b.max = vec4<f32>(vec3<f32>(-3.0e38), 0.0);
        }
        b.axis = vec4<f32>(axis, 0.0);
        bounds[c] = b;
    }
}
