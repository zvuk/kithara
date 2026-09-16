use std::num::NonZeroUsize;

use bon::Builder;
use firewheel_core::{
    dsp::filter::smoothing_filter::DEFAULT_SETTLE_EPSILON, param::smoother::SmootherConfig,
};
use kithara_derive::Patch;
use kithara_platform::sync::Arc;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use kithara_stretch::{ElasticBackendConfig, ElasticBackendConfigPatch};

use crate::StretchControls;

struct Defaults;

impl Defaults {
    const ACTIVATION_BLEND_FRAMES: NonZeroUsize = match NonZeroUsize::new(40) {
        Some(frames) => frames,
        None => unreachable!(),
    };
    const RATE_SMOOTHING: SmootherConfig = SmootherConfig {
        smooth_seconds: 0.02,
        settle_epsilon: DEFAULT_SETTLE_EPSILON,
    };
    const SOURCE_BLOCK_FRAMES: NonZeroUsize = match NonZeroUsize::new(8192) {
        Some(frames) => frames,
        None => unreachable!(),
    };
}

/// Smoothing of the player's plain playback-rate multiplier.
pub const DEFAULT_RATE_SMOOTHING: SmootherConfig = Defaults::RATE_SMOOTHING;

/// Time constant a group's tempo approaches a new target over, in seconds.
///
/// A knob turned quickly lands every value it passes through; this is how fast
/// the group's beat line follows them.
pub const DEFAULT_TEMPO_SMOOTHING_SECONDS: f64 = 0.005;

/// Fixed resources used to construct one resident [`super::Warp`].
///
/// [`WarpConfigPatch`] is what a configuration document may say about it.
#[derive(Clone, Debug, Builder, Patch, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct WarpConfig {
    /// Output frames blended when a new region plan activates.
    #[builder(default = Defaults::ACTIVATION_BLEND_FRAMES)]
    #[field(get, copy)]
    activation_blend_frames: NonZeroUsize,
    /// Live temporal controls consumed by the resident Warp lane. Not a
    /// document key: this is the handle the UI and the deck already share, so
    /// a document naming a stretch ratio would be overwritten by the first
    /// gesture.
    #[builder(default = StretchControls::new(1.0))]
    #[field(get, deref = false)]
    #[patch(skip)]
    stretch: Arc<StretchControls>,
    /// Preparation geometry each compiled stretch backend is built with. Not
    /// the backend selection: which engine runs is a live control on
    /// [`StretchControls`], while this is the geometry the selected engine is
    /// prepared with, read again on every rebuild. Only a build that compiles
    /// a stretch backend has it, so a document naming it under a build that
    /// has none is refused rather than silently ignored.
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[builder(default)]
    #[field(get, copy)]
    #[patch(nested)]
    backends: ElasticBackendConfig,
    /// Maximum source frames admitted to one elastic render operation.
    #[builder(default = Defaults::SOURCE_BLOCK_FRAMES)]
    #[field(get, copy)]
    source_block_frames: NonZeroUsize,
    /// Plain multiplier smoothing in the player's RT render pass.
    /// Beat-derived ratios are not smoothed.
    #[builder(default = DEFAULT_RATE_SMOOTHING)]
    #[field(get, copy)]
    rate_smoothing: SmootherConfig,
    /// Seconds a sync group's tempo approaches a new target over.
    #[builder(default = DEFAULT_TEMPO_SMOOTHING_SECONDS)]
    #[field(get, copy)]
    tempo_smoothing_seconds: f64,
    /// Output-frame cap between samples of live temporal controls.
    /// Without a cap, standalone Warp consumes the complete source span.
    #[field(get, copy)]
    render_quantum_frames: Option<NonZeroUsize>,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::default(None, None)]
    #[case::configured(Some(64), Some(64))]
    fn render_quantum_is_configurable_in_frames(
        #[case] configured: Option<usize>,
        #[case] expected: Option<usize>,
    ) {
        let config = WarpConfig::builder()
            .maybe_render_quantum_frames(
                configured
                    .map(|frames| NonZeroUsize::new(frames).expect("fixture quantum is non-zero")),
            )
            .build();

        assert_eq!(
            config.render_quantum_frames().map(NonZeroUsize::get),
            expected
        );
    }

    #[kithara::test]
    fn activation_blend_is_configurable_in_output_frames() {
        let configured = NonZeroUsize::new(64).expect("fixture blend is non-zero");
        let default = WarpConfig::builder().build();
        let custom = WarpConfig::builder()
            .activation_blend_frames(configured)
            .build();

        assert_eq!(
            default.activation_blend_frames(),
            Defaults::ACTIVATION_BLEND_FRAMES
        );
        assert_eq!(custom.activation_blend_frames(), configured);
    }

    /// Backend geometry merges one engine at a time: a patch naming only
    /// Signalsmith must leave Bungee's built value standing, or a document
    /// tuning one engine would reset the other.
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[kithara::test]
    fn a_patch_naming_one_backend_leaves_the_other_standing() {
        use kithara_stretch::{BungeeConfig, ElasticBackendConfig, SignalsmithConfig};

        let mut config = WarpConfig::builder()
            .backends(
                ElasticBackendConfig::builder()
                    .bungee(
                        BungeeConfig::builder()
                            .log2_synthesis_hop_adjust(-2)
                            .build(),
                    )
                    .build(),
            )
            .build();
        let mut patch = WarpConfigPatch::default();
        patch.backends.signalsmith.block_frames = NonZeroUsize::new(512);
        patch.backends.signalsmith.interval_frames = NonZeroUsize::new(16);

        config.apply(patch);

        let backends = config.backends();
        assert_eq!(
            *backends.signalsmith(),
            SignalsmithConfig::builder()
                .block_frames(NonZeroUsize::new(512).expect("fixture block is non-zero"))
                .interval_frames(NonZeroUsize::new(16).expect("fixture interval is non-zero"))
                .build()
        );
        assert_eq!(
            backends.bungee().log2_synthesis_hop_adjust(),
            -2,
            "a patch that never names Bungee must not reset its geometry"
        );
    }
}
