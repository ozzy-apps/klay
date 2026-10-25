// Polygroups (ZBrush: PolyF), appended to shader.wgsl behind `enable primitive_index;` when
// the GPU supports it: each drawn triangle takes its polygroup's colour (two 16-bit ids per
// word, in draw order), and its edges are drawn as thin dark lines, the polyframe. The
// edges come from the triangle's corners, fetched by primitive index and projected to the
// screen; a quad's diagonal (a triangle's longest edge when clearly longer than the
// others) is left out, and lines fade where triangles are only a few pixels across.

// Group 1 is the mask (shader.wgsl).
@group(2) @binding(0) var<storage, read> drawn_groups: array<u32>;
@group(2) @binding(1) var<storage, read> drawn_indices: array<u32>;
@group(2) @binding(2) var<storage, read> positions: array<f32>;

// Spread around the hue circle in golden-ratio steps; matches `polygroup::group_color`.
fn group_color(g: u32) -> vec3<f32> {
    let h = fract(f32(g) * 0.618034 + 0.55) * 6.0;
    let s = 0.55;
    let v = 0.85;
    let c = v * s;
    let x = c * (1.0 - abs((h % 2.0) - 1.0));
    var rgb = vec3<f32>(c, 0.0, x);
    switch (u32(h)) {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: {}
    }
    return rgb + vec3<f32>(v - c);
}

// Where the vertex shader puts vertex `v` at `pos`: moved by the live Transpose preview as
// far as its mask lets it (as in vs_main).
fn place(pos: vec3<f32>, v: u32) -> vec3<f32> {
    if (u.xf_center.w < 0.5) {
        return pos;
    }
    var m = 0.0;
    if (u.display.w > 0.5) {
        m = mask_at(v);
    }
    return preview_place(pos, 1.0 - m);
}

// Corner `k` of drawn triangle `prim` in the world, as drawn.
fn corner_world(prim: u32, k: u32) -> vec3<f32> {
    let v = drawn_indices[3u * prim + k];
    return place(vec3<f32>(positions[3u * v], positions[3u * v + 1u], positions[3u * v + 2u]), v);
}

// A world point in framebuffer pixels, and its clip w (behind the camera when not positive).
fn to_screen(p: vec3<f32>) -> vec3<f32> {
    let c = u.view_proj * vec4<f32>(p, 1.0);
    let ndc = c.xy / c.w;
    return vec3<f32>(
        u.viewport.x + (ndc.x * 0.5 + 0.5) * u.viewport.z,
        u.viewport.y + (0.5 - ndc.y * 0.5) * u.viewport.w,
        c.w,
    );
}

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0);
    return length(p - (a + ab * t));
}

@fragment
fn fs_groups(in: VsOut, @builtin(primitive_index) prim: u32) -> @location(0) vec4<f32> {
    let word = drawn_groups[prim >> 1u];
    let g = (word >> ((prim & 1u) * 16u)) & 0xffffu;
    var color = shade(in, group_color(g));
    let wa = corner_world(prim, 0u);
    let wb = corner_world(prim, 1u);
    let wc = corner_world(prim, 2u);
    let a = to_screen(wa);
    let b = to_screen(wb);
    let c = to_screen(wc);
    if (a.z > 0.0 && b.z > 0.0 && c.z > 0.0) {
        // A quad's diagonal: the longest edge in the world when it is clearly the longest
        // (judged in the world, not on screen, where a face seen at an angle squashes it).
        let lab = length(wb - wa);
        let lbc = length(wc - wb);
        let lca = length(wa - wc);
        let longest = max(lab, max(lbc, lca));
        let middle = lab + lbc + lca - longest - min(lab, min(lbc, lca));
        let diagonal = longest > 1.1 * middle;
        let p = in.clip.xy;
        var d = 1e9;
        if (!(diagonal && lab == longest)) { d = min(d, segment_distance(p, a.xy, b.xy)); }
        if (!(diagonal && lbc == longest)) { d = min(d, segment_distance(p, b.xy, c.xy)); }
        if (!(diagonal && lca == longest)) { d = min(d, segment_distance(p, c.xy, a.xy)); }
        // Lines are discreet: they darken the surface by about half.
        let px = min(length(b.xy - a.xy), min(length(c.xy - b.xy), length(a.xy - c.xy)));
        let crisp = (1.0 - smoothstep(0.4, 1.4, d)) * 0.55;
        // Where polygons are only a few pixels across, single lines would shimmer or vanish:
        // draw the frame's average darkness instead (about two one-pixel lines per polygon),
        // capped so a dense mesh far away keeps a light, even frame rather than turning black.
        let average = min(0.55 * 2.0 / max(px, 0.5), 0.42);
        let line = mix(average, crisp, smoothstep(3.0, 7.0, px));
        color = vec4<f32>(mix(color.rgb, color.rgb * 0.3, line), 1.0);
    }
    return color;
}
