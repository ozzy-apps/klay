struct Uniforms {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    light_dir: vec4<f32>,
    // xyz = brush hit position, w = radius
    brush: vec4<f32>,
    // x = brush visible, y = surface detail, z = pixel size at brush, w = material
    params: vec4<f32>,
    viewport: vec4<f32>,
    display: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs_background(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.999, 1.0);
}

@fragment
fn fs_background(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (position.xy - u.viewport.xy) / u.viewport.zw;
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let near_h = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let origin = near_h.xyz / near_h.w;
    let direction = normalize(far_h.xyz / far_h.w - origin);
    let safe_y = select(-0.00001, direction.y, abs(direction.y) > 0.00001);
    let t = (-1.12 - origin.y) / safe_y;
    let point = origin + direction * t;
    let grid_pos = point.xz * 4.0;
    let width = max(fwidth(grid_pos), vec2<f32>(0.001));
    let lines = abs(fract(grid_pos - 0.5) - 0.5) / width;
    let grid_line = 1.0 - min(min(lines.x, lines.y), 1.0);
    let fade = exp(-length(point.xz) * 0.30) * smoothstep(0.02, 0.25, abs(direction.y));
    let visible = select(0.0, 1.0, t > 0.0 && u.display.x > 0.5);
    let vignette = max(0.0, 1.0 - length((uv - 0.5) * 1.2));
    var color = vec3<f32>(24.0, 25.0, 28.0) / 255.0 + vignette * 0.014;
    color += vec3<f32>(grid_line * fade * visible * 0.045);
    return vec4<f32>(color, 1.0);
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(pos, 1.0);
    out.world_pos = pos;
    out.normal = nrm.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let v = normalize(u.cam_pos.xyz - in.world_pos);
    let l = normalize(u.light_dir.xyz);

    // Clay-like shading: key light, soft fill from the opposite side, rim and a small highlight.
    var base = vec3<f32>(0.78, 0.62, 0.50);
    if (u.params.w > 0.5 && u.params.w < 1.5) { base = vec3<f32>(0.52, 0.58, 0.66); }
    if (u.params.w > 1.5 && u.params.w < 2.5) { base = vec3<f32>(0.62, 0.29, 0.20); }
    if (u.params.w > 2.5) { base = vec3<f32>(0.55, 0.43, 0.25); }
    let diff = max(dot(n, l), 0.0);
    let fill = max(dot(n, -l), 0.0) * 0.25;
    let ambient = 0.22 + 0.10 * (n.y * 0.5 + 0.5);
    let h = normalize(l + v);
    let polish = select(0.18, 0.75, u.params.w > 2.5);
    let spec = pow(max(dot(n, h), 0.0), 40.0) * polish;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.12;
    var color = base * (ambient + diff * 0.75 + fill) + vec3<f32>(spec + rim);

    if (u.params.y > 0.5) {
        let slope = pow(1.0 - max(dot(n, v), 0.0), 2.0);
        color *= 1.0 - slope * 0.35;
    }

    // Brush ring projected onto the surface.
    if (u.params.x > 0.5) {
        let d = distance(in.world_pos, u.brush.xyz);
        let r = u.brush.w;
        let px = u.params.z;
        let ring = smoothstep(px * 1.5, 0.0, abs(d - r));
        let inside = 1.0 - smoothstep(r * 0.95, r, d);
        color = mix(color, vec3<f32>(0.95, 0.25, 0.15), ring * 0.9);
        color = mix(color, vec3<f32>(0.95, 0.45, 0.25), inside * 0.12);
    }

    // Swapchain is non-sRGB, so encode gamma here.
    return vec4<f32>(pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)), 1.0);
}
