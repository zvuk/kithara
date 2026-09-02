use std::num::NonZeroUsize;

use bon::Builder;
use kithara_platform::sync::Arc;

use crate::StretchControls;

struct Consts;

impl Consts {
    const DEFAULT_RATE_SMOOTH_FRAMES: NonZeroUsize = match NonZeroUsize::new(12) {
        Some(frames) => frames,
        None => unreachable!(),
    };
    const DEFAULT_RENDER_QUANTUM_FRAMES: NonZeroUsize = match NonZeroUsize::new(32) {
        Some(frames) => frames,
        None => unreachable!(),
    };
}

/// Fixed resources used to construct one resident [`super::Warp`].
#[derive(Clone, Debug, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct WarpConfig {
    /// Live temporal controls consumed by the resident Warp lane.
    #[builder(default = StretchControls::new(1.0))]
    #[field(get, deref = false)]
    stretch: Arc<StretchControls>,
    /// Time-stretch rate smoothing window in output frames.
    #[builder(default = Consts::DEFAULT_RATE_SMOOTH_FRAMES)]
    #[field(get, copy)]
    rate_smooth_frames: NonZeroUsize,
    /// Maximum output frames planned before live temporal controls are sampled again.
    #[builder(default = Consts::DEFAULT_RENDER_QUANTUM_FRAMES)]
    #[field(get, copy)]
    render_quantum_frames: NonZeroUsize,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::defaults(None, None, 12, 32)]
    #[case::custom(Some(1), Some(64), 1, 64)]
    fn config_expresses_the_frame_contract(
        #[case] rate_smooth_frames: Option<usize>,
        #[case] render_quantum_frames: Option<usize>,
        #[case] expected_rate_smooth_frames: usize,
        #[case] expected_render_quantum_frames: usize,
    ) {
        let config = WarpConfig::builder()
            .maybe_rate_smooth_frames(
                rate_smooth_frames.map(|frames| {
                    NonZeroUsize::new(frames).expect("fixture smoothing is non-zero")
                }),
            )
            .maybe_render_quantum_frames(
                render_quantum_frames
                    .map(|frames| NonZeroUsize::new(frames).expect("fixture quantum is non-zero")),
            )
            .build();

        assert_eq!(
            config.rate_smooth_frames().get(),
            expected_rate_smooth_frames
        );
        assert_eq!(
            config.render_quantum_frames().get(),
            expected_render_quantum_frames
        );
    }
}
