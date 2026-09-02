mod lifecycle;
mod player;

use std::num::NonZeroUsize;

use delegate::delegate;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_decode::GaplessMode;
use kithara_platform::sync::{Arc, Mutex};
use kithara_warp::WarpConfig;
use tracing::debug;

use self::lifecycle::{CloseAdmission, PlayerLifecycle};
pub use self::player::PlayerImpl;
use super::state::{ItemQueue, PlayerParams, PlayerPhase};
use crate::{
    api::{PlayerEvent, PlayerStatus, TrackId},
    bridge::PlayerCmd,
    engine::EngineImpl,
    error::PlayError,
    resource::Resource,
    session::SessionBinding,
    worker::{EngineLoad, PlayWorker},
};

type EnqueuedItem = (TrackId, Arc<str>, f64);

/// Phase-neutral state shared across every player phase.
///
/// Field order is drop order: `items` and `engine` release every registered
/// track before this Player releases its [`PlayWorker`] clone.
pub(crate) struct PlayerCore<S> {
    /// Live shared cost meter of the audio engine (decode + effects).
    /// Constructed once and kept address-stable for the player's lifetime.
    pub(crate) engine_load: Arc<EngineLoad>,

    pub(crate) warp: WarpConfig,
    pub(crate) response_budget_frames: NonZeroUsize,
    /// Undelivered resources unregister before the worker owner drops.
    pub(crate) items: ItemQueue,
    /// Host lifecycle explicitly detaches the engine session lane before the
    /// worker owner drops.
    pub(crate) engine: EngineImpl<S>,
    /// Explicit shared playback worker. Declared after both resource owners.
    pub(crate) worker: PlayWorker<S>,
    pub(crate) gapless_mode: GaplessMode,
    /// Player-level underrun policy copied into every prepared resource.
    pub(crate) block_on_underrun: bool,
    /// Status kept explicit (not derived from phase): `set_status` emits
    /// `StatusChanged` only on change and its values are not 1:1 with phase.
    pub(crate) status: Mutex<PlayerStatus>,
    pub(crate) params: PlayerParams,
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`](crate::rt::track::PlayerResource), and sent
/// to the processor via `PlayerCmd::LoadTrack`.
///
/// Internally the player is a phase-split typestate: `phase` is a typed
/// `Mutex<PlayerPhase>` carrying the slot / ABR handle / armed-next, while
/// `core` holds the phase-neutral fields. `phase` is declared first so it
/// drops before `core.engine`.
#[doc(hidden)]
pub struct PlayerRuntime<S> {
    lifecycle: PlayerLifecycle,
    operations: Mutex<()>,
    pub(crate) phase: Mutex<PlayerPhase>,
    pub(crate) core: PlayerCore<S>,
}

impl<S> PlayerRuntime<S> {
    /// Minimum playback rate to prevent stalling.
    pub(crate) const MIN_PLAYBACK_RATE: f32 = PlayerParams::MIN_PLAYBACK_RATE;

    /// Rate the player's master bus runs at. Decoded frames handed to an
    /// observer use this axis after decoder-side conversion.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.core.engine.master_sample_rate()
    }

    delegate! {
        to self.lifecycle {
            fn begin_close(&self) -> Result<CloseAdmission, PlayError>;
            fn finish_close(&self);
            #[call(reopen)]
            fn reopen_controls(&self);
            pub(crate) fn is_closed(&self) -> bool;
        }
        to self.core.items {
            /// Advance to the next item in the queue.
            ///
            /// Does nothing if the current item is already the last one.
            pub fn advance_to_next_item(&self);
            /// Sole publisher of `CurrentItemChanged`: emits only when `index` differs
            /// from the last announced item, so a `play()` resume of the same item
            /// stays quiet.
            pub(crate) fn announce_current_item(&self, index: usize);
            /// Drop the resource at `index` so the auto-advance prefetch path
            /// (`arm_next`) cannot plant it into the audio thread.
            ///
            /// Used by the queue when a previously-loaded track is cancelled by
            /// a later `select` — without this, a slow track whose loader
            /// raced ahead of the override stays in `items` and the next
            /// `TrackRequested` notification near EOF would arm it for
            /// handover, surfacing as a barge-in.
            pub fn clear_item(&self, index: usize);
            /// Insert a resource under the queue's identity for it at a
            /// specific position, or append to the end.
            pub fn insert(&self, resource: Resource, item_id: TrackId, at_position: Option<usize>);
            /// Replace a consumed (or existing) resource at the given index,
            /// under the queue's identity for it. Every player event about
            /// the item reports this id back.
            pub fn replace_item(&self, index: usize, resource: Resource, item_id: TrackId);
            /// Pre-allocate empty slots so `replace_item` can fill them by index.
            pub fn reserve_slots(&self, count: usize);
        }
        to self.core.worker {
            /// Typed pool facade used for resources created by this player.
            #[must_use]
            pub fn pools(&self) -> &PoolRegion<S>;
        }
    }

    pub(super) fn attach_session(&self, binding: SessionBinding<S>) -> Result<(), PlayError> {
        self.with_open_result(|runtime| runtime.core.engine.attach_session(binding))
    }

    pub(super) fn with_open<T>(&self, operation: impl FnOnce(&Self) -> T) -> Result<T, PlayError> {
        let _admission = self.operations.lock();
        if self.is_closed() {
            return Err(PlayError::Closed);
        }
        Ok(operation(self))
    }

    pub(super) fn with_open_result<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, PlayError>,
    ) -> Result<T, PlayError> {
        self.with_open(operation)?
    }

    pub(super) fn close(&self) -> Result<(), PlayError> {
        let _admission = self.operations.lock();
        match self.begin_close()? {
            CloseAdmission::AlreadyClosed => return Ok(()),
            CloseAdmission::Begin => {}
        }
        if let Err(error) = self.core.engine.close() {
            self.reopen_controls();
            return Err(error);
        }
        self.finish_close();
        Ok(())
    }

    fn invalidate(&self) {
        let _admission = self.operations.lock();
        self.finish_close();
        self.core.engine.cancel();
    }

    pub(crate) fn enqueue_to_processor(
        &self,
        index: usize,
    ) -> Result<Option<EnqueuedItem>, PlayError>
    where
        S: HasPool<f32>,
    {
        let Some(item) = self.core.items.take_for_load(
            index,
            self.core.engine.master_sample_rate(),
            self.core.engine.pools(),
        )?
        else {
            return Ok(None);
        };
        self.phase.lock().set_abr_handle(item.abr_handle);
        let src = Arc::clone(item.player_resource.src());
        let _ = self.send_to_slot(PlayerCmd::LoadTrack {
            item_id: item.item_id,
            resource: Box::new(item.player_resource),
        });
        Ok(Some((item.item_id, src, item.duration_seconds)))
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self)
    where
        S: HasPool<f32>,
    {
        self.unarm_next();
        self.core.items.clear_all();
        self.set_status(PlayerStatus::Unknown);
        let _ = self.send_to_slot(PlayerCmd::Clear);
        self.enter_stopped();
        debug!("all items removed");
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of
    /// bounds or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource>
    where
        S: HasPool<f32>,
    {
        self.unarm_next();

        self.core
            .items
            .remove_at(index)
            .map(|queued| queued.resource)
    }

    /// Internal: set status and emit event if changed.
    pub(crate) fn set_status(&self, new_status: PlayerStatus) {
        let mut status = self.core.status.lock();
        if *status != new_status {
            *status = new_status;
            drop(status);
            self.core
                .engine
                .bus()
                .publish(PlayerEvent::StatusChanged { status: new_status });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::mpsc::{RecvTimeoutError, channel};

    use kithara_assets::AssetStore;
    use kithara_decode::GaplessMode;
    use kithara_events::{Envelope, Event};
    use kithara_platform::{CancelToken, time::Duration};
    use kithara_test_utils::kithara;
    use kithara_warp::{StretchControls, WarpConfig};

    use super::*;
    use crate::{
        PlayWorkerConfig,
        bridge::PlayerCmd,
        effects::eq::generate_log_spaced_bands,
        player::PlayerConfig,
        resource::{ResourceConfig, ResourceSrc},
        session::testing,
        test_pools::{TestPools, pools},
    };

    #[derive(Clone, Copy)]
    enum PlayerBasicScenario {
        AdvanceOnEmpty,
        EngineAccessor,
        QueueStartsEmpty,
        SendToSlotWithoutSlot,
        StartsPaused,
    }

    fn resource_config(input: &str) -> ResourceConfig<TestPools> {
        let pools = pools();
        let src = ResourceSrc::parse(input).expect("BUG: valid resource config source");
        ResourceConfig::for_src(src)
            .store(AssetStore::builder(pools).build())
            .build()
    }

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    fn player() -> PlayerImpl<TestPools> {
        PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .build(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn close_waits_for_an_admitted_operation() {
        let player = player();
        let runtime = Arc::clone(&player.runtime);
        let control = player.make_control();
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let operation = std::thread::spawn(move || {
            runtime
                .with_open(|_| {
                    entered_tx.send(()).expect("report admitted operation");
                    release_rx.recv().expect("release admitted operation");
                })
                .expect("operation remains admitted");
        });
        entered_rx
            .recv()
            .expect("operation entered the admission gate");

        let (attempting_tx, attempting_rx) = channel();
        let (closing_tx, closing_rx) = channel();
        let closer = std::thread::spawn(move || {
            attempting_tx.send(()).expect("report close attempt");
            closing_tx
                .send(control.close())
                .expect("report close result");
        });
        attempting_rx
            .recv()
            .expect("close reached the admission gate");
        assert!(matches!(
            closing_rx.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));

        release_tx.send(()).expect("release admitted operation");
        operation.join().expect("operation thread completed");
        closer.join().expect("close thread completed");
        closing_rx
            .recv()
            .expect("close result returned after the operation")
            .expect("close succeeds");
        assert!(player.runtime.is_closed());
        assert!(matches!(
            player.make_control().tick(),
            Err(PlayError::Closed)
        ));
        assert!(matches!(
            player.runtime.with_open(|_| ()),
            Err(PlayError::Closed)
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn player_lifecycle_admits_only_one_concurrent_close() {
        let lifecycle = Arc::new(PlayerLifecycle::open());
        assert!(
            matches!(lifecycle.begin_close(), Ok(CloseAdmission::Begin)),
            "first closer owns the transition"
        );

        let concurrent = Arc::clone(&lifecycle);
        let result = std::thread::spawn(move || concurrent.begin_close())
            .join()
            .expect("BUG: lifecycle probe thread panicked");
        assert!(matches!(result, Err(PlayError::Closed)));

        lifecycle.reopen();
        assert!(matches!(lifecycle.begin_close(), Ok(CloseAdmission::Begin)));
        lifecycle.finish_close();
        assert!(matches!(
            lifecycle.begin_close(),
            Ok(CloseAdmission::AlreadyClosed)
        ));
    }

    #[kithara::test]
    fn prepare_config_applies_player_gapless_mode() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .gapless_mode(GaplessMode::Disabled)
                .build(),
        );
        let mut config = resource_config("https://example.com/song.mp3");

        config = player
            .prepare_config(config)
            .expect("bound player reads its session stream shape");

        assert_eq!(config.decoder.gapless_mode(), GaplessMode::Disabled);
        assert!(
            config.cancel.is_some(),
            "prepare_config must inject a per-track cancel child"
        );
    }

    #[kithara::test]
    fn prepare_config_per_track_cancel_is_child_of_player_master() {
        let player = player();
        let mut rc = resource_config("https://example.com/song.mp3");
        rc = player
            .prepare_config(rc)
            .expect("bound player reads its session stream shape");

        let track_cancel = rc.cancel.expect("prepare_config must populate cancel");
        let observer = track_cancel.child();
        assert!(!observer.is_cancelled());

        drop(player);
        assert!(
            observer.is_cancelled(),
            "dropping the player must cancel the per-track child via the master"
        );
    }

    #[kithara::test]
    fn prepare_config_preserves_caller_supplied_master() {
        let parent_master = CancelToken::never();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .cancel(parent_master.clone())
                .build(),
        );
        let mut rc = resource_config("https://example.com/song.mp3");
        rc = player
            .prepare_config(rc)
            .expect("bound player reads its session stream shape");

        let track_cancel = rc.cancel.expect("prepare_config must populate cancel");
        let observer = track_cancel.child();
        assert!(!observer.is_cancelled());

        parent_master.cancel();
        assert!(observer.is_cancelled());
    }

    #[kithara::test]
    #[case(PlayerBasicScenario::StartsPaused)]
    #[case(PlayerBasicScenario::QueueStartsEmpty)]
    #[case(PlayerBasicScenario::AdvanceOnEmpty)]
    #[case(PlayerBasicScenario::EngineAccessor)]
    #[case(PlayerBasicScenario::SendToSlotWithoutSlot)]
    fn player_basic_behaviors(#[case] scenario: PlayerBasicScenario) {
        let player = player();
        match scenario {
            PlayerBasicScenario::StartsPaused => {
                assert!((player.rate() - 0.0).abs() < f32::EPSILON);
                assert_eq!(player.status(), PlayerStatus::Unknown);
            }
            PlayerBasicScenario::QueueStartsEmpty => {
                assert_eq!(player.item_count(), 0);
            }
            PlayerBasicScenario::AdvanceOnEmpty => {
                player.advance_to_next_item();
                assert_eq!(player.current_index(), 0);
            }
            PlayerBasicScenario::EngineAccessor => {
                assert!(!player.engine().is_running());
            }
            PlayerBasicScenario::SendToSlotWithoutSlot => {
                let result = player.send_to_slot(PlayerCmd::SetPaused(true));
                assert!(result.is_err());
            }
        }
    }

    #[kithara::test]
    fn player_pause_without_active_slot_keeps_rate_zero() {
        let player = player();
        player.pause();
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_volume_clamps() {
        let player = player();
        player.set_volume(2.0);
        assert!((player.volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(-1.0);
        assert!((player.volume() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_muted() {
        let player = player();
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
    }

    #[kithara::test]
    fn player_crossfade_duration() {
        let player = player();
        assert!((player.crossfade_duration() - 1.0).abs() < f32::EPSILON);
        player.set_crossfade_duration(3.0);
        assert!((player.crossfade_duration() - 3.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_prefetch_duration() {
        let player = player();
        assert!((player.prefetch_duration() - 3.5).abs() < f32::EPSILON);
        player.set_prefetch_duration(8.0);
        assert!((player.prefetch_duration() - 8.0).abs() < f32::EPSILON);
        player.set_prefetch_duration(-1.0);
        assert!((player.prefetch_duration() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_events_subscribe() {
        let player = player();
        let mut rx = player.subscribe();
        player.set_volume(0.5);
        let event = rx.try_recv();
        assert!(event.is_ok());
    }

    #[kithara::test]
    fn player_config_custom() {
        let config = PlayerConfig::builder()
            .worker(worker())
            .session(testing::test_session())
            .crossfade_duration(2.0)
            .prefetch_duration(5.0)
            .default_rate(0.5)
            .eq_layout(generate_log_spaced_bands(5))
            .gapless_mode(GaplessMode::MediaOnly)
            .max_slots(2)
            .sample_rate(NonZeroU32::new(44_100).expect("invariant: sample rate is non-zero"))
            .warp(
                WarpConfig::builder()
                    .stretch(StretchControls::new(1.0))
                    .build(),
            )
            .build();
        let player = PlayerImpl::new(config);
        assert!((player.crossfade_duration() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn eq_band_count_tracks_a_replacement_layout_before_start() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .eq_layout(generate_log_spaced_bands(3))
                .build(),
        );
        assert_eq!(player.eq_band_count(), 3);

        player.set_eq_layout(generate_log_spaced_bands(4)).unwrap();
        assert_eq!(player.eq_band_count(), 4);
    }

    #[kithara::test]
    fn player_config_builder() {
        let config = PlayerConfig::builder()
            .worker(worker())
            .session(testing::test_session())
            .max_slots(8)
            .default_rate(0.5)
            .crossfade_duration(2.5)
            .prefetch_duration(7.0)
            .eq_layout(generate_log_spaced_bands(5))
            .build();
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert!((config.prefetch_duration - 7.0).abs() < f32::EPSILON);
        assert_eq!(config.eq_layout.len(), 5);
    }

    #[kithara::test]
    fn player_default_rate_getter_setter() {
        let player = player();
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        player.set_default_rate(0.75);
        assert!((player.default_rate() - 0.75).abs() < f32::EPSILON);
        assert!((player.core.warp.stretch().speed() - 0.75).abs() < f32::EPSILON);
        assert_eq!(player.rate(), 0.0);
    }

    #[kithara::test(tokio)]
    async fn synchronous_player_events_remain_in_order() {
        let player = player();
        let mut rx = player.subscribe();

        player.set_volume(0.5);
        player.set_muted(true);
        player.set_rate(2.0);

        let e1 = rx.try_recv();
        let e2 = rx.try_recv();
        assert!(matches!(
            e1,
            Ok(Envelope {
                event: Event::Player(PlayerEvent::VolumeChanged { .. }),
                ..
            })
        ));
        assert!(matches!(
            e2,
            Ok(Envelope {
                event: Event::Player(PlayerEvent::MuteChanged { .. }),
                ..
            })
        ));
        assert!(
            rx.try_recv().is_err(),
            "rate feedback must wait for the RT processor"
        );
    }

    #[kithara::test(tokio)]
    async fn player_negative_crossfade_duration_clamped() {
        let player = player();
        player.set_crossfade_duration(-5.0);
        assert!((player.crossfade_duration() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_without_active_slot_updates_only_the_requested_target() {
        let player = player();
        player.set_rate(2.0);
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
        assert!((player.core.warp.stretch().speed() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn timestretch_is_address_stable_across_play_pause() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .build(),
        );
        let ptr_before = Arc::as_ptr(player.core.warp.stretch());
        player.play();
        player.pause();
        player.play();
        let ptr_after = Arc::as_ptr(player.core.warp.stretch());
        assert_eq!(
            ptr_before, ptr_after,
            "timestretch controls must stay address-stable across transitions"
        );
    }

    #[kithara::test]
    fn pause_from_idle_is_noop() {
        use super::super::state::phase::PlayerPhaseKind;

        let player = player();
        assert_eq!(player.phase_kind(), PlayerPhaseKind::Idle);
        player.pause();
        assert_eq!(
            player.phase_kind(),
            PlayerPhaseKind::Idle,
            "pause from Idle must not leak a phase transition"
        );
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn position_seconds_idle_is_none() {
        let player = player();
        assert!(player.position_seconds().is_none());
        assert!(player.duration_seconds().is_none());
        assert!(!player.is_playing());
        assert!(player.current_abr_handle().is_none());
        assert!(player.armed_next().is_none());
    }

    #[kithara::test]
    fn set_rate_without_rt_does_not_emit_rate_changed() {
        let player = player();
        let mut rx = player.subscribe();
        player.set_rate(2.0);
        assert!(rx.try_recv().is_err());
    }

    #[kithara::test]
    fn player_keeps_explicit_worker_and_shared_pools() {
        let worker = worker();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker.clone())
                .session(testing::test_session())
                .build(),
        );
        assert!(std::ptr::eq(player.worker().pools(), worker.pools()));
    }

    #[kithara::test]
    fn auto_advance_enabled_default_and_toggle() {
        let player = player();
        assert!(player.auto_advance_enabled(), "default must be on");
        player.set_auto_advance_enabled(false);
        assert!(!player.auto_advance_enabled());
        player.set_auto_advance_enabled(true);
        assert!(player.auto_advance_enabled());
    }

    #[kithara::test]
    fn auto_advance_disabled_via_config() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker())
                .session(testing::test_session())
                .auto_advance_enabled(false)
                .build(),
        );
        assert!(!player.auto_advance_enabled());
    }
}
