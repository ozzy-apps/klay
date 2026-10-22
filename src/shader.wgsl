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
    // x = floor grid, y = symmetry, z = show mask, w = the active subtool has a mask
    display: vec4<f32>,
    // Live Transpose preview for the active subtool, each vertex moved as far as the mask
    // lets it (transform.rs, Partial): centre; rotation axis and angle; translation and scale.
    xf_center: vec4<f32>,
    xf_rotation: vec4<f32>,
    xf_move: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// Mask of the active subtool, one byte per vertex packed in words.
@group(1) @binding(0) var<storage, read> mask: array<u32>;

fn mask_at(v: u32) -> f32 {
    return f32((mask[v >> 2u] >> ((v & 3u) * 8u)) & 255u) / 255.0;
}

// Rotate `v` by `angle` about the unit `axis` (Rodrigues).
fn rotate(v: vec3<f32>, axis: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

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
    // 1 for a parked subtool (drawn as instance 1), 0 for the active one.
    @location(2) @interpolate(flat) parked: u32,
    @location(3) mask: f32,
    @location(4) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec4<f32>,
    @location(2) color: vec4<f32>,
    @builtin(instance_index) instance: u32,
    @builtin(vertex_index) vertex: u32,
) -> VsOut {
    var out: VsOut;
    var p = pos;
    var n = nrm.xyz;
    out.mask = 0.0;
    if (instance == 0u) {
        // xf_center.w: a Transpose preview is on; display.w: the subtool has a mask.
        let previewing = u.xf_center.w > 0.5;
        var m = 0.0;
        if (u.display.w > 0.5) {
            m = mask_at(vertex);
        }
        out.mask = m;
        if (previewing) {
            let w = 1.0 - m;
            let angle = u.xf_rotation.w * w;
            let c = u.xf_center.xyz;
            p = c + rotate(pos - c, u.xf_rotation.xyz, angle) * pow(u.xf_move.w, w) + u.xf_move.xyz * w;
            n = rotate(n, u.xf_rotation.xyz, angle);
        }
    }
    out.clip = u.view_proj * vec4<f32>(p, 1.0);
    out.world_pos = p;
    out.normal = n;
    out.parked = instance;
    out.color = color;
    return out;
}

struct PickOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) id: u32,
};

// Id pass: the instance index carries the id of the subtool being drawn.
@vertex
fn vs_pick(@location(0) pos: vec3<f32>, @builtin(instance_index) instance: u32) -> PickOut {
    var out: PickOut;
    out.clip = u.view_proj * vec4<f32>(pos, 1.0);
    out.id = instance;
    return out;
}

@fragment
fn fs_pick(in: PickOut) -> @location(0) u32 {
    return in.id;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var base = vec3<f32>(0.78, 0.62, 0.50);
    if (u.params.w > 0.5 && u.params.w < 1.5) { base = vec3<f32>(0.52, 0.58, 0.66); }
    if (u.params.w > 1.5 && u.params.w < 2.5) { base = vec3<f32>(0.62, 0.29, 0.20); }
    if (u.params.w > 2.5) { base = vec3<f32>(0.55, 0.43, 0.25); }
    if (in.color.a > 0.0001) {
        let rgb=clamp(in.color.rgb/in.color.a,vec3<f32>(0.0),vec3<f32>(1.0));
        base=mix(base,pow(rgb,vec3<f32>(2.2)),in.color.a);
    }
    return shade(in, base);
}

// Shading shared by the plain pass and the polygroup pass (groups.wgsl): `base` is the
// surface colour (the material's, or a polygroup's).
fn shade(in: VsOut, base: vec3<f32>) -> vec4<f32> {
    let n = normalize(in.normal);
    let v = normalize(u.cam_pos.xyz - in.world_pos);
    let l = normalize(u.light_dir.xyz);

    // Clay-like shading: key light, soft fill from the opposite side, rim and a small highlight.
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

    // Parked subtools: a little darker, and never under the brush.
    if (in.parked > 0u) {
        color *= 0.78;
    }

    // Mask (Ctrl): masked clay goes dark, as in ZBrush.
    if (u.display.z > 0.5) {
        color = mix(color, color * 0.18, in.mask * 0.9);
    }

    // Brush ring projected onto the surface.
    if (u.params.x > 0.5 && in.parked == 0u) {
        var d = distance(in.world_pos, u.brush.xyz);
        // Symmetry: a second ring at the mirror image across X = 0.
        if (u.display.y > 0.5) {
            let mirrored = vec3<f32>(-u.brush.x, u.brush.y, u.brush.z);
            d = min(d, distance(in.world_pos, mirrored));
        }
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
