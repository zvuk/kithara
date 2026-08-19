// A generic authored fragment: nothing about it is specific to any control.
//
// Every uniform it reads is one the document bound by name, so a capture of
// this page answers whether an arbitrary document shader runs — not whether the
// one concrete visualiser still does.
//
// It is deliberately a function of `position` as well as of its uniforms. A
// shader that only mixed constants would draw the same flat field however badly
// a host mapped its pixels, and the coordinate bug this page was written to
// catch would have passed unseen.

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let unit = position.xy / max(kithara.viewport.xy, vec2<f32>(1.0, 1.0));
    let level = clamp(kithara.level.x, 0.0, 1.0);
    let energy = clamp(kithara.energy.x, 0.0, 1.0);
    let bands = fract(unit.x * (2.0 + level * 14.0));
    let ramp = smoothstep(0.0, 1.0, unit.y);
    return vec4<f32>(
        bands * (0.25 + energy * 0.75),
        ramp * (0.20 + level * 0.60),
        0.35 + energy * 0.45,
        1.0,
    );
}
