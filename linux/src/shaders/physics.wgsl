struct Flake {
    position: vec2<f32>,
    velocity: vec2<f32>,
    fall_speed: f32,
    size: f32,
    depth: f32,
    alpha: f32,
    soft: f32,
    phase: f32,
    sway_rate: f32,
    sway_amount: f32,
    settles: u32, // bool as u32
    mass: f32,
    tint: vec3<f32>,
    tint_amount: f32,
    blown: f32,
};

struct SimulationUniforms {
    dt: f32,
    time: f32,
    view_width: f32,
    view_height: f32,
    cursor: vec2<f32>,
    cursor_velocity: vec2<f32>,
    wind: f32,
    gravity: f32,
};

@group(0) @binding(0) var<uniform> u_sim: SimulationUniforms;
@group(0) @binding(1) var<storage, read_write> flakes: array<Flake>;
// We'd also have storage bindings for heights, grains, etc.

@compute @workgroup_size(64)
fn update_flakes(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let index = global_id.x;
    if (index >= arrayLength(&flakes)) {
        return;
    }

    var f = flakes[index];

    // Simplistic physics application as a placeholder for full simulation mapping
    f.position = f.position + f.velocity * u_sim.dt;
    f.velocity.y = f.velocity.y + u_sim.gravity * u_sim.dt;

    // Sway
    f.phase = f.phase + f.sway_rate * u_sim.dt;

    // Bounds check
    if (f.position.y > u_sim.view_height) {
        f.position.y = 0.0; // Wrap around for now
    }

    flakes[index] = f;
}
