use std::num::NonZeroUsize;

use kithara::stretch::{BungeeConfig, ElasticBackendConfig, SignalsmithConfig};

pub(crate) const SAMPLE_RATE: u32 = 44_100;
pub(crate) const CHANNELS: u16 = 2;
pub(crate) const TARGET_WINDOW_FRAMES: usize = 128;
pub(crate) const TONES_HZ: [f64; 4] = [440.0, 880.0, 1_760.0, 3_520.0];
pub(crate) const TONE_DOMINANCE_RATIO: f64 = 4.0;
pub(crate) const MIN_SIGNAL_RMS: f64 = 0.003;
pub(crate) const WARMUP_BLOCK_BUDGET: usize = 200;
pub(crate) fn response_backends() -> ElasticBackendConfig {
    ElasticBackendConfig::builder()
        .signalsmith(
            SignalsmithConfig::builder()
                .block_frames(NonZeroUsize::new(224).expect("case block is non-zero"))
                .interval_frames(NonZeroUsize::new(32).expect("case interval is non-zero"))
                .build()
                .expect("valid Signalsmith geometry"),
        )
        .bungee(
            BungeeConfig::builder()
                .log2_synthesis_hop_adjust(-4)
                .build(),
        )
        .build()
}

pub(crate) const MINIMUM: ResponseCase = ResponseCase {
    callback_frames: 128,
    source_block_frames: 4_096,
    render_quantum_frames: NonZeroUsize::new(16),
    smooth_frames: 1,
    response_budget_frames: 441,
    initial_rate: 1.0,
    initial_tone: 1,
    target_rate: 2.0,
    target_tone: 2,
    burst: 0,
};
pub(crate) const PRODUCT: ResponseCase = ResponseCase {
    callback_frames: 128,
    source_block_frames: 8_192,
    render_quantum_frames: NonZeroUsize::new(32),
    smooth_frames: 12,
    response_budget_frames: 441,
    initial_rate: 2.0,
    initial_tone: 2,
    target_rate: 0.5,
    target_tone: 0,
    burst: 0,
};
pub(crate) const EXTREME: ResponseCase = ResponseCase {
    callback_frames: 64,
    source_block_frames: 16_384,
    render_quantum_frames: NonZeroUsize::new(32),
    smooth_frames: 64,
    response_budget_frames: 441,
    initial_rate: 0.5,
    initial_tone: 0,
    target_rate: 4.0,
    target_tone: 3,
    burst: 64,
};

pub(crate) const RAMP: ResponseCase = ResponseCase {
    callback_frames: 128,
    source_block_frames: 8_192,
    render_quantum_frames: NonZeroUsize::new(32),
    smooth_frames: 882,
    response_budget_frames: 441,
    initial_rate: 2.0,
    initial_tone: 2,
    target_rate: 4.0,
    target_tone: 3,
    burst: 0,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ResponseCase {
    pub(crate) callback_frames: usize,
    pub(crate) source_block_frames: usize,
    pub(crate) render_quantum_frames: Option<NonZeroUsize>,
    pub(crate) smooth_frames: usize,
    pub(crate) response_budget_frames: usize,
    pub(crate) initial_rate: f32,
    pub(crate) initial_tone: usize,
    pub(crate) target_rate: f32,
    pub(crate) target_tone: usize,
    pub(crate) burst: usize,
}

impl ResponseCase {
    pub(crate) fn observation_frames(self) -> usize {
        self.response_budget_frames
            .saturating_add(self.source_block_frames.saturating_mul(2))
            .saturating_add(TARGET_WINDOW_FRAMES)
            .saturating_add(self.callback_frames.saturating_mul(2))
    }
}
