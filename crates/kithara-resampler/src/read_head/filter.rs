pub(super) fn anti_alias_sample(
    enabled: bool,
    ratio: f64,
    interpolated: f32,
    previous: f32,
    center: f32,
    next: f32,
) -> f32 {
    if enabled && ratio > 1.0 {
        (previous.mul_add(0.25, center * 0.5) + next * 0.25 + interpolated) * 0.5
    } else {
        interpolated
    }
}
