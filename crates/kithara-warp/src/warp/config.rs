use std::num::NonZeroUsize;

use kithara_config::Config;
use kithara_derive::Patch;
use kithara_platform::sync::Arc;
use kithara_stretch::{
    ElasticBackendConfig, ElasticBackendConfigPatch, ElasticBackendConfigPatchError, StretchKind,
};

use crate::{RegionPlan, consts};

/// Fixed resources used to construct one resident [`super::Warp`].
///
/// [`WarpConfigPatch`] is what a configuration document may say about it.
#[derive(Clone, Debug, Patch, Config)]
#[config(builder(state_mod(vis = "pub")), patch(fallible), fields(value))]
#[non_exhaustive]
pub struct WarpConfig {
    /// Media seconds consumed per output second a renderer built from this
    /// configuration starts at. Not a document key: the render lane changes it
    /// on a frame.
    #[config(
        skip = "construction value the render lane changes live",
        builder(default = 1.0),
        get(copy),
        patch(skip)
    )]
    speed: f32,
    /// Whether a renderer built from this configuration starts on a
    /// pitch-preserving engine, where its backend has one.
    #[config(
        skip = "construction value the render lane changes live",
        builder(default),
        get(copy),
        patch(skip)
    )]
    keylock: bool,
    /// Stretch engine a renderer built from this configuration starts on.
    #[config(
        skip = "construction value the render lane changes live",
        builder(default),
        get(copy),
        patch(skip)
    )]
    backend: StretchKind,
    /// Per-region ratio corrections a renderer built from this configuration
    /// applies over source frames.
    #[config(skip = "shared immutable region plan", get(ref), patch(skip))]
    region_plan: Option<Arc<RegionPlan>>,
    /// Backend preparation geometry, independent of compiled engine choices.
    /// The render lane owns backend selection.
    #[config(nested, builder(default), get(copy), patch(nested, fallible))]
    backends: ElasticBackendConfig,
    /// Maximum source frames admitted to one elastic render operation.
    #[config(builder(default = consts::DEFAULT_SOURCE_BLOCK_FRAMES), get(copy))]
    source_block_frames: NonZeroUsize,
    /// Optional output-frame cap between samples of live temporal controls.
    /// Without a cap, Warp consumes the complete source span accepted by its backend.
    #[config(get(copy))]
    render_quantum_frames: Option<NonZeroUsize>,
}

impl WarpConfig {
    /// A copy whose renderer starts at `speed` on `backend`, keylocked where
    /// `keylock`: where a track's render lane stands when it opens, before
    /// the lane's first command.
    #[must_use]
    pub fn starting_at(&self, speed: f32, keylock: bool, backend: StretchKind) -> Self {
        Self {
            speed,
            keylock,
            backend,
            ..self.clone()
        }
    }
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

    /// Backend geometry merges one engine at a time: a patch naming only
    /// Signalsmith must leave Bungee's built value standing, or a document
    /// tuning one engine would reset the other.
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

        config.apply(patch).expect("valid backend geometry patch");

        let backends = config.backends();
        assert_eq!(
            *backends.signalsmith(),
            SignalsmithConfig::builder()
                .block_frames(NonZeroUsize::new(512).expect("fixture block is non-zero"))
                .interval_frames(NonZeroUsize::new(16).expect("fixture interval is non-zero"))
                .build()
                .expect("valid Signalsmith geometry")
        );
        assert_eq!(
            backends.bungee().log2_synthesis_hop_adjust(),
            -2,
            "a patch that never names Bungee must not reset its geometry"
        );
    }

    #[kithara::test]
    fn rejected_backend_geometry_keeps_the_entire_warp_config() {
        let mut config = WarpConfig::builder().build();
        let previous_source_limit = config.source_block_frames();
        let mut patch = WarpConfigPatch {
            source_block_frames: NonZeroUsize::new(64),
            ..WarpConfigPatch::default()
        };
        patch.backends.signalsmith.block_frames = NonZeroUsize::new(16);
        patch.backends.signalsmith.interval_frames = NonZeroUsize::new(32);

        assert!(matches!(
            config.apply(patch),
            Err(WarpConfigPatchError::Backends(_))
        ));
        assert_eq!(config.source_block_frames(), previous_source_limit);
        assert_eq!(config.backends().signalsmith().block_frames(), None);
        assert_eq!(config.backends().signalsmith().interval_frames(), None);
    }
}
