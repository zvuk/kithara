struct Uniforms {
    resolution: vec2<f32>,
    origin: vec2<f32>,
    time: f32,
    level: f32,
    preset: u32,
    padding: u32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}

fn ported_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn neon_tunnel(frag_coord: vec2<f32>) -> vec3<f32> {
    let uv = (frag_coord - 0.5 * uniforms.resolution) / uniforms.resolution.y;
    let angle = atan2(uv.y, uv.x);
    let radius = length(uv) + 0.001;
    let distance = 0.3 / radius + uniforms.time * 1.2;
    let ring = 0.5 + 0.5 * sin(distance * 6.283);
    let segment = 0.5 + 0.5 * sin(angle * 8.0 + uniforms.time * 0.8);
    let value = ring * (0.35 + 0.65 * segment);
    var color = vec3<f32>(0.055, 0.055, 0.1);
    color += vec3<f32>(0.92, 0.16, 0.55)
        * ported_smoothstep(0.5, 1.0, value)
        * (0.45 + uniforms.level * 0.9);
    color += vec3<f32>(0.95, 0.82, 0.18)
        * ported_smoothstep(0.8, 1.0, ring)
        * segment
        * (0.25 + uniforms.level * 0.75);
    color += vec3<f32>(0.18, 0.78, 0.92)
        * (1.0 - ported_smoothstep(0.0, 0.45, radius))
        * (0.25 + 0.75 * uniforms.level);
    color *= ported_smoothstep(1.4, 0.35, radius);
    return color;
}

fn golden_plasma(frag_coord: vec2<f32>) -> vec3<f32> {
    let uv = frag_coord / uniforms.resolution;
    var plasma = sin(uv.x * 7.0 + uniforms.time)
        + sin(uv.y * 9.0 - uniforms.time * 1.3)
        + sin((uv.x + uv.y) * 11.0 + uniforms.time * 0.6)
        + sin(length(uv - vec2<f32>(0.5)) * 16.0 - uniforms.time * 2.0);
    plasma = plasma * 0.125 + 0.5;
    var color = mix(
        vec3<f32>(0.055, 0.055, 0.1),
        vec3<f32>(0.73, 0.58, 0.26),
        pow(clamp(plasma, 0.0, 1.0), 2.0) * (0.45 + uniforms.level * 0.9),
    );
    let line = ported_smoothstep(0.03, 0.0, abs(fract(plasma * 6.0) - 0.5) * 0.33);
    color += vec3<f32>(0.9, 0.86, 0.7) * line * 0.12;
    return color;
}

fn oscilloscope(frag_coord: vec2<f32>) -> vec3<f32> {
    let uv = frag_coord / uniforms.resolution - vec2<f32>(0.5);
    let grid_x = step(0.47, abs(fract(uv.x * 10.0) - 0.5) * 2.0);
    let grid_y = step(0.47, abs(fract(uv.y * 7.0) - 0.5) * 2.0);
    let wave_1 = sin(uv.x * 9.0 + uniforms.time * 3.0) * (0.1 + 0.22 * uniforms.level);
    let wave_2 = sin(uv.x * 14.0 - uniforms.time * 4.0 + 1.5)
        * (0.06 + 0.14 * uniforms.level);
    var color = vec3<f32>(0.043, 0.043, 0.075)
        + vec3<f32>(0.16, 0.16, 0.3) * max(grid_x, grid_y) * 0.35;
    color += vec3<f32>(0.92, 0.16, 0.55)
        * ported_smoothstep(0.02, 0.0, abs(uv.y - wave_1));
    color += vec3<f32>(0.18, 0.78, 0.92)
        * ported_smoothstep(0.015, 0.0, abs(uv.y - wave_2));
    color += vec3<f32>(0.95, 0.82, 0.18)
        * ported_smoothstep(0.012, 0.0, abs(uv.y))
        * (0.15 + 0.85 * uniforms.level);
    return color;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let local = position.xy - uniforms.origin;
    let frag_coord = vec2<f32>(local.x, uniforms.resolution.y - local.y);
    var color: vec3<f32>;
    switch uniforms.preset {
        case 1u: {
            color = golden_plasma(frag_coord);
        }
        case 2u: {
            color = oscilloscope(frag_coord);
        }
        default: {
            color = neon_tunnel(frag_coord);
        }
    }
    return vec4<f32>(color, 1.0);
}
