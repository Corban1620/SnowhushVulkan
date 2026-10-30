struct Uniforms {
    view_size: vec2<f32>,
    scale: f32,
    time: f32,
    column_width: f32,
    column_count: u32,
    occluder_count: u32,
    pile_top: f32,
    pile_bottom: f32,
    origin: vec2<f32>,
    height_offset: u32,
    light: vec4<f32>,
    shadow_tint: vec4<f32>,
    panel: vec4<f32>,
    occluders: array<vec4<f32>, 16>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// Let's use a 1D texture or buffer for heights. The original code mapped a 4096-wide 2D texture.
// With WGSL, we can just bind a 2D texture or a buffer. Let's use a buffer since it's Compute-friendly!
@group(0) @binding(1) var<storage, read> heights: array<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let corner = vec2<f32>(f32(vertex_index & 1u), f32(vertex_index >> 1u));
    let p = vec2<f32>(corner.x * f32(u.column_count) * u.column_width, mix(u.pile_bottom, u.pile_top, corner.y));
    out.position = vec4<f32>((p + u.origin) / u.view_size * 2.0 - 1.0, 0.0, 1.0);
    out.local = p;
    return out;
}

fn fetch(i: u32) -> f32 {
    return heights[i];
}

fn height_at(start: u32, x: f32) -> f32 {
    let f = x / u.column_width - 0.5;
    let base = floor(f);
    let i = clamp(u32(max(base, 0.0)), 0u, u.column_count - 1u);
    let j = min(i + 1u, u.column_count - 1u);
    return mix(fetch(start + i), fetch(start + j), clamp(f - base, 0.0, 1.0));
}

fn hash(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let world = in.local + u.origin;
    if (world.x >= u.panel.x && world.x < u.panel.z && world.y >= u.panel.y && world.y < u.panel.w) {
        discard;
    }

    for (var k: u32 = 0u; k < 16u; k++) {
        if (k >= u.occluder_count) {
            break;
        }
        let o = u.occluders[k];
        if (world.x >= o.x && world.x < o.z && world.y >= o.y && world.y < o.w) {
            discard;
        }
    }

    let x = in.local.x;
    let level = height_at(u.height_offset, x);
    let bed = height_at(u.height_offset + u.column_count, x);
    let depth = level - in.local.y;

    let coverage = clamp(depth * u.scale + 0.5, 0.0, 1.0)
        * clamp((in.local.y - bed) * u.scale + 0.5, 0.0, 1.0)
        * clamp((level - bed) / 0.75, 0.0, 1.0);

    if (coverage <= 0.0) {
        discard;
    }

    let slope = (height_at(u.height_offset, x + 2.0) - height_at(u.height_offset, x - 2.0)) * 0.25;
    let lit = dot(normalize(vec2<f32>(-slope, 1.0)), normalize(vec2<f32>(-0.5, 1.0)));

    let shadow = mix(vec3<f32>(0.72, 0.78, 0.88), u.shadow_tint.rgb, u.shadow_tint.a);
    let surface = mix(vec3<f32>(0.925, 0.94, 0.965), min(u.shadow_tint.rgb * 1.18, vec3<f32>(1.0)), u.shadow_tint.a * 0.2);
    let shade = (1.0 - exp(-depth / 30.0)) * 0.32 + (1.0 - lit) * 0.6 * exp(-depth / 5.0);
    var color = mix(surface, shadow, clamp(shade, 0.0, 1.0));
    color = mix(color, vec3<f32>(0.975, 0.982, 0.995), exp(-depth * 0.9) * 0.6);

    // wgpu framebuffer uses (0,0) as top-left.
    let frag_coord = vec2<f32>(in.position.x, u.view_size.y - in.position.y);
    color += (hash(floor(frag_coord.xy)) - 0.5) * 0.02;

    let seed = hash(floor(frag_coord.xy * 0.5) + 31.7);
    if (seed > 0.992) {
        let twinkle = pow(clamp(sin(u.time * (0.6 + seed * 1.8) + seed * 90.0), 0.0, 1.0), 28.0);
        color = mix(color, vec3<f32>(1.0), twinkle * exp(-depth / 7.0));
    }

    color *= u.light.rgb;
    return vec4<f32>(color * coverage, coverage);
}
