use bon::Builder;

/// Policy for turning the beat model's raw logits into events.
///
/// The chunk geometry the model is run with is not here: it follows the
/// segmentation `beat_this` was trained on and is not a knob.
#[derive(Clone, Copy, Debug, Builder)]
#[non_exhaustive]
pub struct BeatConfig {
    /// Logit a frame must exceed to be a peak candidate. `0.0` is probability
    /// `0.5` after the sigmoid; lowering it admits quieter beats and the false
    /// positives that come with them.
    #[builder(default = 0.0)]
    pub peak_threshold: f32,
    /// Frames within which consecutive peaks collapse to their mean position.
    /// Absorbs the plateaus the model produces on a strong beat.
    #[builder(default = 1)]
    pub dedup_width: usize,
    /// Half-width, in model frames, of the max-pool window a frame must win to
    /// be a peak. The window spans `2 * peak_half_width + 1` frames, so this
    /// sets the shortest gap two beats may be reported at: at 50 fps the
    /// default of 3 keeps beats at least ~120 ms apart, which is 500 BPM.
    #[builder(default = 3)]
    pub peak_half_width: usize,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
