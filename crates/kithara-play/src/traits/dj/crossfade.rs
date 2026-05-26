use kithara_platform::time::Duration;

mod kithara {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CrossfadeCurve {
    #[default]
    EqualPower,
    Linear,
    SCurve,
    ConstantPower,
    FastFadeIn,
    FastFadeOut,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CrossfadeConfig {
    pub curve: CrossfadeCurve,
    pub duration: Duration,
    pub beat_aligned: bool,
    pub cut_incoming_at: f32,
    pub cut_outgoing_at: f32,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            curve: CrossfadeCurve::default(),
            duration: Duration::from_secs(5),
            beat_aligned: false,
            cut_incoming_at: 0.0,
            cut_outgoing_at: 1.0,
        }
    }
}
