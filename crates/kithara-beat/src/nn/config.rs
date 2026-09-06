use bon::Builder;

/// Policy for turning the beat model's raw logits into events.
#[derive(Clone, Copy, Debug, Builder)]
#[non_exhaustive]
pub struct BeatConfig {
    /// Logit a frame must exceed to be a peak candidate; `0.0` is an even chance.
    #[builder(default = 0.0)]
    pub peak_threshold: f32,
    /// Frames within which consecutive peaks collapse to their mean position.
    #[builder(default = 1)]
    pub dedup_width: usize,
    /// Half-width, in model frames, of the max-pool window a frame must win.
    /// The default keeps beats at least 120 ms apart at 50 fps.
    #[builder(default = 3)]
    pub peak_half_width: usize,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
