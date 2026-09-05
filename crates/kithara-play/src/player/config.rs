use std::{fmt, num::NonZeroU32};

use bon::Builder;
use kithara_abr::AbrController;
use kithara_decode::GaplessMode;
use kithara_events::EventBus;
use kithara_macros::Patch;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_warp::{BeatGridId, WarpConfig, WarpConfigPatch};

use crate::{
    PlayWorker,
    effects::eq::{EqBandConfig, generate_log_spaced_bands},
    session::SessionDispatcher,
};

fn allocate_grid_id() -> BeatGridId {
    let Ok(id) = BeatGridId::allocate() else {
        panic!("process-wide beat-grid identity space is exhausted");
    };
    id
}

/// Configuration for the player.
///
/// Holds the player's own tunables, the engine values it hands to the
/// [`EngineConfig`] it builds, and the per-call wiring a caller passes in.
/// [`PlayerConfigPatch`] is what a configuration document may say about it.
///
/// [`EngineConfig`]: crate::EngineConfig
#[derive(Builder, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct PlayerConfig<S> {
    /// Stable synchronization-group identity owned by this player.
    #[builder(default = allocate_grid_id())]
    #[patch(skip)]
    pub(crate) grid_id: BeatGridId,
    /// Per-deck Warp resources and live temporal controls. A document reaches
    /// them under `player.warp:`; the live [`StretchControls`] handle inside
    /// is shared with the deck and the UI and is not a document key.
    ///
    /// [`StretchControls`]: kithara_warp::StretchControls
    #[builder(default = WarpConfig::builder().build())]
    #[patch(nested)]
    pub(crate) warp: WarpConfig,
    /// Explicit shared playback worker. Its pools and cancellation lifetime
    /// are configured once in [`crate::PlayWorkerConfig`].
    #[patch(skip)]
    pub(crate) worker: PlayWorker<S>,
    /// How resources created for this player trim leading/trailing audio.
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    /// Make audio-thread reads block on a producer-ring underrun instead of
    /// zero-filling the block. Offline (faster-than-real-time) harnesses opt
    /// in so rendered output never stretches with inserted silence while the
    /// decode worker catches up. Real-time hosts must keep the default
    /// (`false`): the audio callback can never block. Not a document key:
    /// the shipped binary is a real-time host, and only the offline test
    /// harness sets this, from Rust.
    #[builder(default)]
    #[patch(skip)]
    pub block_on_underrun: bool,
    /// Built-in auto-advance handler. The queue overwrites this for every
    /// queue-driven player at construction, so it is not a document key.
    /// See `crates/kithara-play/CONTEXT.md` for the owning contract.
    #[builder(default = true)]
    #[patch(skip)]
    pub auto_advance_enabled: bool,
    /// Crossfade duration in seconds. Default: 1.0.
    #[builder(default = 1.0)]
    pub crossfade_duration: f32,
    /// Default playback-rate target (1.0 = normal). Default: 1.0.
    #[builder(default = 1.0)]
    pub default_rate: f32,
    /// Secondary lead time before EOF at which the next queued item is
    /// loaded. The queue overwrites this for every queue-driven player at
    /// construction, so it is not a document key. See
    /// `crates/kithara-play/CONTEXT.md` for the owning contract.
    #[builder(default = 3.5)]
    #[patch(skip)]
    pub prefetch_duration: f32,
    /// Initial output sample rate supplied by the owning session, handed on
    /// to the engine this player builds and to the player's own sync
    /// identity. Not a document key: `HostConfig` owns the rate, a Host
    /// rejects a player whose rate disagrees with its own, and the document
    /// names it once under `host`.
    #[patch(skip)]
    pub sample_rate: NonZeroU32,
    /// Maximum concurrent slots of the engine this player builds.
    /// Default: 4.
    #[builder(default = 4)]
    pub max_slots: usize,
    /// EQ band layout handed to the engine this player builds. Not a document
    /// key: every construction site derives it from a generator, and a custom
    /// layout is installed at runtime through `PlayerImpl::set_eq_layout`.
    #[builder(default = generate_log_spaced_bands(10))]
    #[patch(skip)]
    pub eq_layout: Vec<EqBandConfig>,
    /// Shared ABR controller. When `None`, a default one is created.
    #[patch(skip)]
    pub(crate) abr: Option<Arc<AbrController>>,
    /// Root event bus for this player.
    #[patch(skip)]
    pub(crate) bus: Option<EventBus>,
    /// Master cancel token for this player.
    #[patch(skip)]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional pre-bound session for isolated harnesses. Production players
    /// are constructed unbound and attached exactly once by their Host.
    #[patch(skip)]
    pub(crate) session: Option<Arc<dyn SessionDispatcher<S>>>,
}

impl<S> Clone for PlayerConfig<S> {
    fn clone(&self) -> Self {
        Self {
            grid_id: self.grid_id,
            warp: self.warp.clone(),
            worker: self.worker.clone(),
            gapless_mode: self.gapless_mode,
            block_on_underrun: self.block_on_underrun,
            auto_advance_enabled: self.auto_advance_enabled,
            crossfade_duration: self.crossfade_duration,
            default_rate: self.default_rate,
            prefetch_duration: self.prefetch_duration,
            sample_rate: self.sample_rate,
            max_slots: self.max_slots,
            eq_layout: self.eq_layout.clone(),
            abr: self.abr.clone(),
            bus: self.bus.clone(),
            cancel: self.cancel.clone(),
            session: self.session.clone(),
        }
    }
}

impl<S> fmt::Debug for PlayerConfig<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerConfig")
            .field("warp", &self.warp)
            .field("gapless_mode", &self.gapless_mode)
            .field("crossfade_duration", &self.crossfade_duration)
            .field("default_rate", &self.default_rate)
            .field("sample_rate", &self.sample_rate)
            .field("max_slots", &self.max_slots)
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{NonZeroU32, PlayerConfig};
    use crate::{
        PlayWorker, PlayWorkerConfig,
        test_pools::{TestPools, pools},
    };

    pub(super) fn config() -> PlayerConfig<TestPools> {
        PlayerConfig::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .sample_rate(NonZeroU32::new(44_100).expect("44100 is not zero"))
            .build()
    }

    #[kithara::test]
    fn defaults_match_the_documented_values() {
        let config = config();

        assert!(!config.block_on_underrun);
        assert!(config.auto_advance_enabled);
        assert!((config.crossfade_duration - 1.0).abs() < f32::EPSILON);
        assert!((config.default_rate - 1.0).abs() < f32::EPSILON);
        assert!((config.prefetch_duration - 3.5).abs() < f32::EPSILON);
        assert_eq!(config.max_slots, 4);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use super::{GaplessMode, PlayerConfigPatch, tests::config};

    /// `slot_ceiling` is not a prefix of any real field (unlike `max_slot`,
    /// which would pass this assertion vacuously because the error message
    /// lists the real `max_slots` field among the valid names).
    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("slot_ceiling: 8\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("slot_ceiling"), "{error}");
    }

    /// `prefetch_duration` is a real field on [`PlayerConfig`] but must not
    /// be document-reachable: the queue always overwrites it at construction
    /// (see the field's doc comment).
    ///
    /// [`PlayerConfig`]: super::PlayerConfig
    #[kithara::test(native, flash(false))]
    fn the_queue_owned_prefetch_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("prefetch_duration: 8.0\n")
            .expect_err("a queue-owned field must not be settable from a document");

        assert!(error.to_string().contains("prefetch_duration"), "{error}");
    }

    /// `block_on_underrun` is a real field on [`PlayerConfig`] but must not
    /// be document-reachable: the shipped binary is a real-time host whose
    /// audio callback can never block (see the field's doc comment).
    ///
    /// [`PlayerConfig`]: super::PlayerConfig
    #[kithara::test(native, flash(false))]
    fn the_realtime_unsafe_block_on_underrun_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("block_on_underrun: true\n")
            .expect_err("a field that can park the audio callback must not be document-settable");

        assert!(error.to_string().contains("block_on_underrun"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_crossfade_it_names() {
        let patch: PlayerConfigPatch =
            serde_yaml_ng::from_str("crossfade_duration: 2.0\n").expect("the document types");
        let mut config = config();
        // Seeded off the default (1.0) so a whole-struct `apply` that resets
        // every unnamed field to `Default::default()` cannot pass this
        // assertion by coincidence.
        config.default_rate = 2.5;

        config.apply(patch);

        assert!((config.crossfade_duration - 2.0).abs() < f32::EPSILON);
        assert!(
            (config.default_rate - 2.5).abs() < f32::EPSILON,
            "a silent field must keep its seeded value, not reset to default"
        );
    }

    /// `sample_rate` is a real field on [`PlayerConfig`] but must not be
    /// document-reachable: `HostConfig` owns the output rate, a Host refuses
    /// a player whose rate disagrees with its own, and the document names it
    /// once under `host` (see the field's doc comment).
    ///
    /// [`PlayerConfig`]: super::PlayerConfig
    #[kithara::test(native, flash(false))]
    fn the_host_owned_sample_rate_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("sample_rate: 48000\n")
            .expect_err("a host-owned field must not be settable from a player document");

        assert!(error.to_string().contains("sample_rate"), "{error}");
    }

    /// `gapless_mode` was skipped until `GaplessMode` derived `Deserialize`.
    /// Now that it does, a document naming it must reach the configuration
    /// without disturbing a sibling field.
    #[kithara::test(native, flash(false))]
    fn a_gapless_mode_patch_reaches_the_player() {
        let patch: PlayerConfigPatch = serde_yaml_ng::from_str("gapless_mode:\n  mode: disabled\n")
            .expect("the document types");
        let mut config = config();
        // `disabled` differs from the `MediaOnly` default, so only the patch
        // can produce it. The sibling is seeded off its own default (1.0) so a
        // whole-struct reset would go red here rather than pass by coincidence.
        config.crossfade_duration = 2.5;

        config.apply(patch);

        assert_eq!(config.gapless_mode, GaplessMode::Disabled);
        assert!(
            (config.crossfade_duration - 2.5).abs() < f32::EPSILON,
            "a sibling field must survive the patch"
        );
    }
}
