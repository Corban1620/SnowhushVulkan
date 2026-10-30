struct Sprite {
    @location(0) position: vec2<f32>,
    @location(1) velocity: vec2<f32>,
    @location(2) size: f32,
    @location(3) alpha: f32,
    @location(4) soft: f32,
    @location(5) tint_amount: f32,
    @location(6) tint_color: vec3<f32>,
    @location(7) lump: f32,
};

struct Uniforms {
    view_size: vec2<f32>,
    scale: f32,
    time: f32,
    light: vec4<f32>,
    panel: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) coord: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) soft: f32,
    @location(3) tint: vec4<f32>,
    @location(4) streak: vec2<f32>,
    @location(5) radius: f32,
    @location(6) lump: f32,
    @location(7) spin: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: Sprite,
) -> VertexOutput {
    var out: VertexOutput;

    let pixels = instance.size * u.scale;
    let travel = instance.velocity * 0.018;

    let reach_mul = select(2.2, 1.2, instance.lump > 0.0);
    let smear = min(length(travel), instance.size * reach_mul);
    let streak = select(vec2<f32>(0.0), normalize(travel) * smear * u.scale, smear > 0.25);

    let reach = select(1.0, 1.3, instance.lump > 0.0);
    let extent = max(pixels * reach + length(streak), 2.0);

    let corner = vec2<f32>(f32(vertex_index & 1u), f32(vertex_index >> 1u));
    let center = instance.position / u.view_size * 2.0 - 1.0;

    // wgsl clip space Y is up, unlike opengl.
    // In original GLSL:
    // gl_Position = vec4(center + (corner - 0.5) * extent * 2.0 / (viewSize * scale), 0.0, 1.0);
    // Y might need flipping if we change coordinate systems, but Wayland generally matches desktop conventions. Let's keep it math-identical first.
    out.position = vec4<f32>(center + (corner - vec2<f32>(0.5)) * extent * 2.0 / (u.view_size * u.scale), 0.0, 1.0);
    // Negate Y because wgpu clip space Y is up but our rendering assumes opengl (where Y was up, wait GL is Y up).
    // Actually wgpu is Y-up, OpenGL is Y-up, but Vulkan is Y-down. Wgpu abstracts this to Y-up.
    // However the fragment shader relies on it.

    out.coord = vec2<f32>(corner.x, 1.0 - corner.y);
    out.streak = streak / extent;
    out.radius = pixels / extent * 0.5;

    out.alpha = instance.alpha * min(pixels / 2.0, 1.0) * pixels / max(pixels + length(streak) * 0.6, 0.001);
    out.soft = instance.soft;
    out.tint = vec4<f32>(instance.tint_color, instance.tint_amount);
    out.lump = instance.lump;
    out.spin = select(0.0, u.time * (instance.lump - 0.5) * 9.0 + instance.lump * 40.0, instance.lump > 0.0);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // frag coord in WGPU: (0,0) is top-left. OpenGL was bottom-left.
    // Wait, wgpu standardizes on top-left (0,0) with Y down for framebuffer coordinates.
    // We'll need to adapt `at`. `u.view_size` has screen dimensions.
    let at = vec2<f32>(in.position.x, u.view_size.y - in.position.y); // simulate opengl gl_FragCoord.y if needed

    if (at.x >= u.panel.x && at.x < u.panel.z && at.y >= u.panel.y && at.y < u.panel.w) {
        discard;
    }

    let p = vec2<f32>(in.coord.x - 0.5, 0.5 - in.coord.y);
    let half_streak = in.streak * 0.5;
    let span = dot(half_streak, half_streak);
    let t = select(0.0, clamp(dot(p, half_streak) / span, -1.0, 1.0), span > 0.0);
    let d = p - half_streak * t;

    var r = length(d) / max(in.radius, 0.001);
    var shading = 0.0;

    if (in.lump > 0.0) {
        let angle = atan2(d.y, d.x) + in.spin;
        r /= 1.0 + 0.14 * sin(3.0 * angle) + 0.08 * sin(5.0 * angle + in.lump * 21.0) + 0.05 * sin(2.0 * angle + in.lump * 9.0);
        shading = clamp(dot(d / max(in.radius, 0.001), vec2<f32>(0.45, -0.6)), 0.0, 1.0) * 0.4;
    }

    let head = select(1.0, 0.15 + 0.85 * pow(t * 0.5 + 0.5, 1.5), span > 0.0);
    let a = (1.0 - smoothstep(mix(0.6, 0.0, in.soft), 1.0, r)) * in.alpha * head;

    if (a < 0.002) {
        discard;
    }

    let core = vec3<f32>(0.965, 0.975, 0.995);
    let rim = vec3<f32>(0.62, 0.67, 0.77);
    var color = mix(core, rim, clamp(smoothstep(0.35, 1.0, r) * 0.65 + shading, 0.0, 1.0));
    color = mix(color, in.tint.rgb, in.tint.a) * u.light.rgb;

    return vec4<f32>(color * a, a);
}
