// Polygroup colours (ZBrush: PolyF), appended to shader.wgsl behind `enable primitive_index;`
// when the GPU supports it. The colour of each drawn triangle comes from its polygroup, two
// 16-bit ids per word in draw order.

// Group 1 is the mask (shader.wgsl).
@group(2) @binding(0) var<storage, read> drawn_groups: array<u32>;

// Spread around the hue circle in golden-ratio steps; matches `polygroup::group_color`.
fn group_color(g: u32) -> vec3<f32> {
    let h = fract(f32(g) * 0.618034 + 0.1) * 6.0;
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

@fragment
fn fs_groups(in: VsOut, @builtin(primitive_index) prim: u32) -> @location(0) vec4<f32> {
    let word = drawn_groups[prim >> 1u];
    let g = (word >> ((prim & 1u) * 16u)) & 0xffffu;
    return shade(in, group_color(g));
}
