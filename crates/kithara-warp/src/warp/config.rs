use std::num::NonZeroUsize;

use kithara_derive::Patch;
use kithara_platform::sync::Arc;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use kithara_stretch::{ElasticBackendConfig, ElasticBackendConfigPatch};

use crate::{StretchControls, WarpPlanSlot};

const DEFAULT_SOURCE_BLOCK_FRAMES: NonZeroUsize = match NonZeroUsize::new(8192) {
    Some(frames) => frames,
    None => unreachable!(),
};

/// Fixed resources used to construct one resident [`super::Warp`].
///
/// [`WarpConfigPatch`] is what a configuration document may say about it.
#[kithara_config::config]
#[derive(Clone, Debug, Patch, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct WarpConfig {
    /// Explicit projected selection prepared by the musical policy owner.
    #[builder(default = Arc::new(WarpPlanSlot::default()))]
    #[field(get, deref = false)]
    #[patch(skip)]
    #[config(skip = "shared projected warp plan handle")]
    plan: Arc<WarpPlanSlot>,
    /// Live temporal controls consumed by the resident Warp lane. Not a
    /// document key: this is the handle the UI and the deck already share, so
    /// a document naming a stretch ratio would be overwritten by the first
    /// gesture.
    #[builder(default = StretchControls::new(1.0))]
    #[field(get, deref = false)]
    #[patch(skip)]
    #[config(skip = "shared live temporal control handle")]
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
    #[config(nested)]
    backends: ElasticBackendConfig,
    /// Maximum source frames admitted to one elastic render operation.
    #[builder(default = DEFAULT_SOURCE_BLOCK_FRAMES)]
    #[field(get, copy)]
    #[config(value)]
    source_block_frames: NonZeroUsize,
    /// Output-frame window used to smooth live rate changes.
    #[builder(default = NonZeroUsize::MIN)]
    #[field(get, copy)]
    #[config(value)]
    rate_smooth_frames: NonZeroUsize,
    /// Optional output-frame cap between samples of live temporal controls.
    /// Without a cap, Warp consumes the complete source span accepted by its backend.
    #[field(get, copy)]
    #[config(value)]
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
        use kithara_config::Config as _;

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
        let values = config.values();
        assert_eq!(
            values.render_quantum_frames.map(NonZeroUsize::get),
            expected
        );
        assert_eq!(values.source_block_frames, DEFAULT_SOURCE_BLOCK_FRAMES);
        assert_eq!(values.rate_smooth_frames, NonZeroUsize::MIN);
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
        use kithara_config::Config as _;
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
        patch.backends.signalsmith.block_frames = Some(NonZeroUsize::new(512));
        patch.backends.signalsmith.interval_frames = Some(NonZeroUsize::new(16));

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
        let values = config.values();
        assert_eq!(
            values.backends.signalsmith.block_frames,
            NonZeroUsize::new(512)
        );
        assert_eq!(values.backends.bungee.log2_synthesis_hop_adjust, -2);
    }
}
