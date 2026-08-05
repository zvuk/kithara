/// Make a decoded sample safe to hand downstream: `NaN` and infinities become
/// silence, and so do denormals — they are inaudible (below -700 dBFS) but
/// carry a one- to two-order-of-magnitude arithmetic penalty for every stage
/// that touches them afterwards.
#[must_use]
#[inline]
pub fn sanitize_sample(sample: f32) -> f32 {
    if !sample.is_finite() || sample.abs() < f32::MIN_POSITIVE {
        return 0.0;
    }
    sample
}
