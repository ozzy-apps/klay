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
    // MatCaps (matcap.rs): unpainted colour; w = MatCap index, negative for the clays.
    material: vec4<f32>,
    cam_up: vec4<f32>,
    // Document › Back: rgb, w = Range; Center, Rate, border width (px); Border rgb.
    back: vec4<f32>,
    back_shape: vec4<f32>,
    border: vec4<f32>,
    // Symmetry (symmetry.rs): mirror bits X 1 Y 2 Z 4; radial axis (-1 off); radial count.
    sym: vec4<f32>,
    // Transpose preview stretch: unit axis, w = log of the factor (0 none).
    xf_stretch: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// Two layers per MatCap: diffuse (times the surface colour), then specular (added).
@group(0) @binding(1) var matcaps: texture_2d_array<f32>;
@group(0) @binding(2) var matcap_sampler: sampler;

// Live Boolean (boolean.rs): every operand's distance volume, one slab after another along
// z in one 3D texture (0.5 is the surface, less is inside).
struct BoolParams {
    // Operand count; the active subtool's slot (-1 none); points per volume; atlas depth.
    info: vec4<f32>,
    // World to the active subtool's own space (undoes its live Transpose preview).
    active_inv: mat4x4<f32>,
    // Per operand: volume origin and operation (0 Add, 1 Subtract, 2 Intersect); 1 / size.
    boxes: array<vec4<f32>, 40>,
};
@group(0) @binding(3) var bool_volumes: texture_3d<f32>;
@group(0) @binding(4) var bool_sampler: sampler;
@group(0) @binding(5) var<uniform> boolean: BoolParams;
// Mask of the active subtool, one byte per vertex packed in words.
@group(1) @binding(0) var<storage, read> mask: array<u32>;

fn mask_at(v: u32) -> f32 {
    return f32((mask[v >> 2u] >> ((v & 3u) * 8u)) & 255u) / 255.0;
}

// Rotate `v` by `angle` about the unit `axis` (Rodrigues).
// Stretch `v` along unit `axis` by `k`.
fn stretch(v: vec3<f32>, axis: vec3<f32>, k: f32) -> vec3<f32> {
    return v + axis * (dot(axis, v) * (k - 1.0));
}

// The live Transpose preview at weight `w` (1 - mask): stretch, rotation and scale about
// the centre, then the move.
fn preview_place(pos: vec3<f32>, w: f32) -> vec3<f32> {
    let c = u.xf_center.xyz;
    let s = stretch(pos - c, u.xf_stretch.xyz, exp(u.xf_stretch.w * w));
    return c + rotate(s, u.xf_rotation.xyz, u.xf_rotation.w * w) * pow(u.xf_move.w, w) + u.xf_move.xyz * w;
}

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
    // The border around the document frame: flat Border colour, nothing drawn on it.
    let inset = u.back_shape.z;
    let inner = position.xy - u.viewport.xy - vec2<f32>(inset);
    let size = u.viewport.zw - vec2<f32>(2.0 * inset);
    if (any(inner < vec2<f32>(0.0)) || any(inner >= size)) {
        return vec4<f32>(u.border.rgb, 1.0);
    }
    // Document › Back with its vertical gradient (DocumentLook::back_at).
    let v = inner.y / max(size.y, 1.0);
    let half = max(u.back_shape.y * 0.5, 0.001);
    let g = clamp((v - (u.back_shape.x - half)) / (2.0 * half), 0.0, 1.0);
    let light = 1.0 - g * g * (3.0 - 2.0 * g);
    var color = clamp(u.back.rgb + vec3<f32>((light - 0.5) * u.back.w * 0.5), vec3<f32>(0.0), vec3<f32>(1.0));
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
    // The drawn index buffer's first triangle of this draw (vs_active, culled draws), so
    // the polygroup pass can find its triangle from the primitive index.
    @location(5) @interpolate(flat) prim_base: u32,
};

// A vertex as drawn: the active subtool's moved by the live Transpose preview as far as its
// mask lets it.
fn vertex_out(pos: vec3<f32>, nrm: vec4<f32>, color: vec4<f32>, vertex: u32, is_active: bool) -> VsOut {
    var out: VsOut;
    var p = pos;
    var n = nrm.xyz;
    out.mask = 0.0;
    if (is_active) {
        // xf_center.w: a Transpose preview is on; display.w: the subtool has a mask.
        let previewing = u.xf_center.w > 0.5;
        var m = 0.0;
        if (u.display.w > 0.5) {
            m = mask_at(vertex);
        }
        out.mask = m;
        if (previewing) {
            let w = 1.0 - m;
            p = preview_place(pos, w);
            // Normals stretch the other way (inverse transpose), then turn.
            n = rotate(stretch(n, u.xf_stretch.xyz, exp(-u.xf_stretch.w * w)), u.xf_rotation.xyz, u.xf_rotation.w * w);
        }
    }
    out.clip = u.view_proj * vec4<f32>(p, 1.0);
    out.world_pos = p;
    out.normal = n;
    out.color = color;
    out.parked = 0u;
    out.prim_base = 0u;
    return out;
}

// Parked subtools' stand-ins: the instance from 1 on marks them (with Live Boolean,
// 1 + the operand slot).
@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec4<f32>,
    @location(2) color: vec4<f32>,
    @builtin(instance_index) instance: u32,
    @builtin(vertex_index) vertex: u32,
) -> VsOut {
    var out = vertex_out(pos, nrm, color, vertex, instance == 0u);
    out.parked = instance;
    return out;
}

// The active subtool, drawn in one draw per visible chunk: the instance is the draw's
// first triangle (0 when drawn whole).
@vertex
fn vs_active(
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec4<f32>,
    @location(2) color: vec4<f32>,
    @builtin(instance_index) instance: u32,
    @builtin(vertex_index) vertex: u32,
) -> VsOut {
    var out = vertex_out(pos, nrm, color, vertex, true);
    out.prim_base = instance;
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

// Whether `p` is inside operand `k`.
fn bool_inside(k: u32, p_world: vec3<f32>) -> bool {
    var p = p_world;
    if (i32(k) == i32(boolean.info.y)) {
        p = (boolean.active_inv * vec4<f32>(p_world, 1.0)).xyz;
    }
    let o = boolean.boxes[2u * k];
    let s = boolean.boxes[2u * k + 1u];
    let uvw = (p - o.xyz) * s.xyz;
    if (any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0))) {
        return false;
    }
    let slab = boolean.info.z;
    let half = 0.5 / slab;
    let xy = clamp(uvw.xy, vec2<f32>(half), vec2<f32>(1.0 - half));
    let z = (f32(k) * slab + clamp(uvw.z * slab, 0.5, slab - 0.5)) / boolean.info.w;
    return textureSampleLevel(bool_volumes, bool_sampler, vec3<f32>(xy, z), 0.0).r < 0.5;
}

// Whether `p` is inside the result, with operand `own` taken as `own_in` (top to bottom:
// Add joins, Subtract carves, Intersect keeps the overlap).
fn bool_result(own: u32, own_in: bool, p: vec3<f32>) -> bool {
    var r = false;
    let n = u32(boolean.info.x);
    for (var k = 0u; k < n; k = k + 1u) {
        var a = own_in;
        if (k != own) {
            a = bool_inside(k, p);
        }
        let op = u32(boolean.boxes[2u * k].w + 0.5);
        if (op == 0u) {
            r = r || a;
        } else if (op == 1u) {
            r = r && !a;
        } else {
            r = r && a;
        }
    }
    return r;
}

// Live Boolean for one fragment of a subtool's surface: 1 where it is the result's surface
// as drawn (front faces), -1 where it is the result's surface seen from its other side (a
// carved wall: back faces, normal turned round), 0 where it is not part of the result.
fn bool_side(in: VsOut, front: bool) -> f32 {
    var slot = i32(in.parked) - 1;
    if (in.parked == 0u) {
        slot = i32(boolean.info.y);
    }
    if (slot < 0 || slot >= i32(boolean.info.x)) {
        return select(0.0, 1.0, front);
    }
    let with_it = bool_result(u32(slot), true, in.world_pos);
    let without = bool_result(u32(slot), false, in.world_pos);
    if (with_it == without) {
        return 0.0;
    }
    if (with_it) {
        return select(0.0, 1.0, front);
    }
    return select(-1.0, 0.0, front);
}

@fragment
fn fs_bool(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let side = bool_side(in, front);
    if (side == 0.0) {
        discard;
    }
    var v = in;
    v.normal = in.normal * side;
    return surface(v);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return surface(in);
}

// The material (or polypaint) shaded.
fn surface(in: VsOut) -> vec4<f32> {
    var base = vec3<f32>(0.78, 0.62, 0.50);
    if (u.params.w > 0.5 && u.params.w < 1.5) { base = vec3<f32>(0.52, 0.58, 0.66); }
    if (u.params.w > 1.5 && u.params.w < 2.5) { base = vec3<f32>(0.62, 0.29, 0.20); }
    if (u.params.w > 2.5) { base = vec3<f32>(0.55, 0.43, 0.25); }
    if (u.material.w >= 0.0) { base = u.material.rgb; }
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
    // Light palette: light_dir.w is the key light's intensity, cam_up.w the ambient.
    let diff = max(dot(n, l), 0.0) * u.light_dir.w;
    let fill = max(dot(n, -l), 0.0) * 0.25;
    let ambient = (0.22 + 0.10 * (n.y * 0.5 + 0.5)) * u.cam_up.w;
    let h = normalize(l + v);
    let polish = select(0.18, 0.75, u.params.w > 2.5);
    let spec = pow(max(dot(n, h), 0.0), 40.0) * polish * u.light_dir.w;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.12;
    var color = base * (ambient + diff * 0.75 + fill) + vec3<f32>(spec + rim);
    if (u.material.w >= 0.0) {
        color = matcap(n, v, base);
    }

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
        // Symmetry: a ring at every image of the brush (as symmetry.rs makes them).
        if (u.display.y > 0.5) {
            d = min(d, symmetric_distance(in.world_pos, u.brush.xyz));
        }
        let r = u.brush.w;
        let px = u.params.z;
        let ring = smoothstep(px * 1.5, 0.0, abs(d - r));
        let inside = 1.0 - smoothstep(r * 0.95, r, d);
        // A small dot at the brush centre stands in for the hidden pointer.
        let dot = 1.0 - smoothstep(px * 1.2, px * 2.2, distance(in.world_pos, u.brush.xyz));
        color = mix(color, vec3<f32>(0.95, 0.25, 0.15), ring * 0.9);
        color = mix(color, vec3<f32>(0.95, 0.45, 0.25), inside * 0.12);
        color = mix(color, vec3<f32>(1.0, 0.85, 0.75), dot * 0.9);
    }

    // Swapchain is non-sRGB, so encode gamma here.
    return vec4<f32>(pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)), 1.0);
}

// A MatCap looked up by the view-space normal. The frame is built around this fragment's
// own view direction, so perspective does not slide the lighting across the model.
fn matcap(n: vec3<f32>, v: vec3<f32>, base: vec3<f32>) -> vec3<f32> {
    let right = normalize(cross(u.cam_up.xyz, v));
    let up = cross(v, right);
    var p = vec2<f32>(dot(n, right), dot(n, up));
    let len = length(p);
    if (len > 1.0) {
        p = p / len;
    }
    let uv = vec2<f32>(0.5 + 0.5 * p.x, 0.5 - 0.5 * p.y);
    let layer = i32(u.material.w + 0.5) * 2;
    let diffuse = textureSampleLevel(matcaps, matcap_sampler, uv, layer, 0.0).rgb;
    let specular = textureSampleLevel(matcaps, matcap_sampler, uv, layer + 1, 0.0).rgb;
    return base * diffuse + specular;
}

// The distance from `p` to the nearest symmetry image of `c`: every combination of the
// enabled mirrors, turned by each step of the radial symmetry.
fn symmetric_distance(p: vec3<f32>, c: vec3<f32>) -> f32 {
    let bits = u32(u.sym.x + 0.5);
    let axis = i32(round(u.sym.y));
    var turns = 1u;
    if (axis >= 0) {
        turns = max(u32(u.sym.z + 0.5), 1u);
    }
    var d = 1e9;
    for (var m = 0u; m < 8u; m = m + 1u) {
        if ((m & bits) != m) {
            continue;
        }
        var q = c;
        if ((m & 1u) != 0u) { q.x = -q.x; }
        if ((m & 2u) != 0u) { q.y = -q.y; }
        if ((m & 4u) != 0u) { q.z = -q.z; }
        for (var k = 0u; k < turns; k = k + 1u) {
            var r = q;
            if (axis >= 0) {
                var n = vec3<f32>(0.0, 1.0, 0.0);
                if (axis == 0) { n = vec3<f32>(1.0, 0.0, 0.0); }
                if (axis == 2) { n = vec3<f32>(0.0, 0.0, 1.0); }
                r = rotate(q, n, 6.2831853 * f32(k) / f32(turns));
            }
            d = min(d, distance(p, r));
        }
    }
    return d;
}
