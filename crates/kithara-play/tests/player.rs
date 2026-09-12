use std::num::NonZeroU32;

use kithara_audio::SeekOutcome;
use kithara_bufpool::testing::{TestPools, pools};
use kithara_decode::GaplessMode;
use kithara_events::Envelope;
use kithara_platform::time::Duration;
use kithara_play::{
    PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerEvent, PlayerImpl, PlayerStatus,
    SelectTransition, StretchControls, effects::eq::generate_log_spaced_bands, mock,
    player::PlayerControlSource,
};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;
use kithara_warp::WarpConfig;

fn worker() -> PlayWorker<TestPools> {
    PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
}

fn player() -> PlayerImpl<TestPools> {
    PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(mock::SAMPLE_RATE)
            .worker(worker())
            .session(mock::session())
            .build(),
    )
}

#[derive(Clone, Copy)]
enum PlayerBasicScenario {
    AdvanceOnEmpty,
    EngineAccessor,
    QueueStartsEmpty,
    StartsPaused,
}

#[kithara::test]
#[case(PlayerBasicScenario::StartsPaused)]
#[case(PlayerBasicScenario::QueueStartsEmpty)]
#[case(PlayerBasicScenario::AdvanceOnEmpty)]
#[case(PlayerBasicScenario::EngineAccessor)]
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
    let mut rx = player.subscribe::<PlayerEvent>();
    player.set_volume(0.5);
    let event = rx.try_recv();
    assert!(event.is_ok());
}

#[kithara::test]
fn player_config_custom() {
    let config = PlayerConfig::builder()
        .sample_rate(mock::SAMPLE_RATE)
        .worker(worker())
        .session(mock::session())
        .crossfade_duration(2.0)
        .prefetch_duration(5.0)
        .default_rate(0.5)
        .gapless_mode(GaplessMode::MediaOnly)
        .eq_layout(generate_log_spaced_bands(5))
        .max_slots(2)
        .warp(
            WarpConfig::builder()
                .stretch(StretchControls::new(1.0))
                .build(),
        )
        .build();
    let player = PlayerImpl::new(config);
    assert!((player.crossfade_duration() - 2.0).abs() < f32::EPSILON);
}

/// `PlayerConfig::sample_rate` is the single place the value lives before
/// the `EngineConfig` it configures exists. This pins that the value the
/// player reports back out is the exact one the engine runs with, so the
/// rate the owning Host hands down cannot land somewhere the engine never
/// reads.
#[kithara::test]
fn a_configured_sample_rate_reaches_the_engine_it_prepares() {
    let sample_rate = NonZeroU32::new(48_000).expect("invariant: sample rate is non-zero");
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .worker(worker())
            .session(mock::session())
            .sample_rate(sample_rate)
            .build(),
    );

    assert_eq!(player.sample_rate(), sample_rate.get());
}

#[kithara::test]
fn eq_band_count_tracks_a_replacement_layout_before_start() {
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(mock::SAMPLE_RATE)
            .worker(worker())
            .session(mock::session())
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
        .sample_rate(mock::SAMPLE_RATE)
        .worker(worker())
        .session(mock::session())
        .default_rate(0.5)
        .crossfade_duration(2.5)
        .prefetch_duration(7.0)
        .max_slots(8)
        .eq_layout(generate_log_spaced_bands(5))
        .build();
    assert_eq!(config.max_slots, 8);
    assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
    assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
    assert!((config.prefetch_duration - 7.0).abs() < f32::EPSILON);
    assert_eq!(config.eq_layout.len(), 5);
}

#[kithara::test(tokio)]
async fn synchronous_player_events_remain_in_order() {
    let player = player();
    let mut rx = player.subscribe::<PlayerEvent>();

    player.set_volume(0.5);
    player.set_muted(true);
    player.set_rate(2.0);

    let e1 = rx.try_recv();
    let e2 = rx.try_recv();
    assert!(matches!(
        e1,
        Ok(Envelope {
            event: PlayerEvent::VolumeChanged { .. },
            ..
        })
    ));
    assert!(matches!(
        e2,
        Ok(Envelope {
            event: PlayerEvent::MuteChanged { .. },
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
    let mut rx = player.subscribe::<PlayerEvent>();
    player.set_rate(2.0);
    assert!(rx.try_recv().is_err());
}

#[kithara::test]
fn player_keeps_explicit_worker_and_shared_pools() {
    let worker = worker();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(mock::SAMPLE_RATE)
            .worker(worker.clone())
            .session(mock::session())
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
            .sample_rate(mock::SAMPLE_RATE)
            .worker(worker())
            .session(mock::session())
            .auto_advance_enabled(false)
            .build(),
    );
    assert!(!player.auto_advance_enabled());
}

#[kithara::test]
fn host_rejects_a_player_built_for_another_sample_rate() {
    let mut player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(mock::SAMPLE_RATE)
            .worker(worker())
            .build(),
    );
    let binding = mock::session_at(NonZeroU32::new(48_000).expect("48000 is not zero"));

    assert!(matches!(
        PlayerControlSource::attach_session(&mut player, binding),
        Err(PlayError::SessionSampleRateMismatch {
            player: 44_100,
            session: 48_000,
        })
    ));
}

#[kithara::test]
fn select_item_out_of_range_returns_typed_error() {
    let player = player();
    let err = player
        .select_item_with_crossfade(
            5,
            SelectTransition {
                autoplay: false,
                crossfade_seconds: 0.0,
            },
        )
        .expect_err("must error");
    assert!(matches!(
        err,
        PlayError::IndexOutOfRange { index: 5, len: 0 }
    ));
}

/// `enqueue_to_processor` takes the resource out of the slot, so a
/// select against an emptied (consumed) slot has nothing to load: it
/// must fail loudly instead of moving the playlist current index / announcing
/// `CurrentItemChanged` while the old audio keeps playing.
#[kithara::test]
fn select_item_on_consumed_slot_errors_without_bookkeeping() {
    let player = player();
    player.reserve_slots(2);
    let result = player.select_item_with_crossfade(
        1,
        SelectTransition {
            autoplay: false,
            crossfade_seconds: 0.0,
        },
    );
    assert!(result.is_err(), "selecting an emptied slot must fail");
    assert_eq!(
        player.current_index(),
        0,
        "bookkeeping must not move on a failed select"
    );
}

/// A player with nothing loaded still owns the position it is handed;
/// discarding it is what makes a restored position play from zero.
#[kithara::test]
fn seek_seconds_without_slot_holds_the_start_position() {
    let player = player();

    let outcome = player.seek_seconds(12.0).expect("must accept");

    assert!(matches!(
        outcome,
        SeekOutcome::Landed { target, landed_at }
            if target == Duration::from_secs(12) && landed_at == target
    ));
    assert_eq!(player.position_seconds(), Some(12.0));
}

/// A seek the player answered `Landed` must read back where the host draws
/// its progress from. Holding the target in a private field and still
/// reporting no position is what leaves a restored podcast at the head of
/// its track and makes every further scrub look dead.
#[kithara::test]
fn a_held_start_position_is_readable_at_the_position_readout() {
    let player = player();

    player.seek_seconds(12.0).expect("must accept");

    assert_eq!(player.position_seconds(), Some(12.0));
}

/// The held target is the latest one handed over, not the first: a host
/// resets to zero before it restores a stored position.
#[kithara::test]
fn held_start_position_keeps_the_latest_target() {
    let player = player();

    player.seek_seconds(0.0).expect("must accept");
    player.seek_seconds(30.0).expect("must accept");

    assert_eq!(player.position_seconds(), Some(30.0));
}

/// The held target names a place in the queued item, so it must not
/// outlive the queue and land on whatever is seeded next.
#[kithara::test]
fn clearing_the_queue_drops_the_held_start_position() {
    let player = player();
    player.seek_seconds(30.0).expect("must accept");

    player.remove_all_items();

    assert_eq!(player.position_seconds(), None);
}
