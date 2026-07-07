#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::{num::NonZeroU32, sync::Arc};

use kithara::{
    self,
    decode::PcmSpec,
    events::{Event, EventBus, EventReceiver},
    play::{
        PlayError, PlayerConfig, PlayerEvent, PlayerImpl, PlayerStatus, Resource, SeekOutcome,
        SessionDispatcher,
    },
};
use kithara_integration_tests::{audio_mock::TestPcmReader, offline::OfflineSession};

#[derive(Clone, Copy)]
enum InsertScenario {
    AppendTwice,
    InsertAtPosition,
}

#[derive(Clone, Copy)]
enum RemoveAtScenario {
    ExistingItem,
    OutOfBounds,
    ShiftCurrentIndex,
}

fn mock_spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
}

fn make_resource(duration_secs: f64) -> Resource {
    Resource::from_reader(
        TestPcmReader::new(mock_spec(), duration_secs),
        Some(Arc::from(format!("test-resource-{duration_secs}"))),
    )
}

fn make_tagged_resource(item_id: &'static str, duration_secs: f64) -> (Resource, Arc<str>) {
    let item_id = Arc::<str>::from(item_id);
    (
        Resource::from_reader(
            TestPcmReader::new(mock_spec(), duration_secs),
            Some(Arc::from(format!("memory://{item_id}"))),
        ),
        item_id,
    )
}

fn make_offline_player(crossfade_duration: f32) -> (PlayerImpl, Arc<OfflineSession>) {
    let bus = EventBus::default();
    let session = Arc::new(OfflineSession::new_manual());
    let mut player_config = PlayerConfig::default();
    player_config.bus = Some(bus);
    player_config.crossfade_duration = crossfade_duration;
    player_config.session = Some(Arc::clone(&session) as Arc<dyn SessionDispatcher>);
    let player = PlayerImpl::new(player_config);
    (player, session)
}

fn drain_player_events(player: &PlayerImpl, rx: &mut EventReceiver) -> Vec<PlayerEvent> {
    use kithara::platform::tokio::sync::broadcast::error::TryRecvError;
    player.process_notifications();
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(Event::Player(event)) => events.push(event),
            Ok(_) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
    events
}

fn render_until_events(
    player: &PlayerImpl,
    session: &OfflineSession,
    rx: &mut EventReceiver,
    max_blocks: usize,
    mut done: impl FnMut(&[PlayerEvent]) -> bool,
) -> Vec<PlayerEvent> {
    const BLOCK_FRAMES: usize = 512;
    let mut events = Vec::new();
    for _ in 0..max_blocks {
        let _ = session.render(BLOCK_FRAMES);
        events.extend(drain_player_events(player, rx));
        if done(&events) {
            break;
        }
    }
    events
}

#[kithara::test(tokio)]
#[case(InsertScenario::AppendTwice, 2)]
#[case(InsertScenario::InsertAtPosition, 3)]
async fn player_insert_scenarios(#[case] scenario: InsertScenario, #[case] expected_count: usize) {
    let player = PlayerImpl::new(PlayerConfig::default());
    player.insert(make_resource(1.0), None, None);
    player.insert(make_resource(2.0), None, None);
    if matches!(scenario, InsertScenario::InsertAtPosition) {
        player.insert(make_resource(3.0), None, Some(0));
    }
    assert_eq!(player.item_count(), expected_count);
}

#[kithara::test(tokio)]
#[case(RemoveAtScenario::ExistingItem)]
#[case(RemoveAtScenario::OutOfBounds)]
#[case(RemoveAtScenario::ShiftCurrentIndex)]
async fn player_remove_at_scenarios(#[case] scenario: RemoveAtScenario) {
    let player = PlayerImpl::new(PlayerConfig::default());
    match scenario {
        RemoveAtScenario::ExistingItem => {
            player.insert(make_resource(1.0), None, None);
            player.insert(make_resource(2.0), None, None);
            let removed = player.remove_at(0);
            assert!(removed.is_some());
            assert_eq!(player.item_count(), 1);
        }
        RemoveAtScenario::OutOfBounds => {
            player.insert(make_resource(1.0), None, None);
            assert!(player.remove_at(5).is_none());
            assert_eq!(player.item_count(), 1);
        }
        RemoveAtScenario::ShiftCurrentIndex => {
            player.insert(make_resource(1.0), None, None);
            player.insert(make_resource(2.0), None, None);
            player.insert(make_resource(3.0), None, None);
            player.advance_to_next_item();
            player.advance_to_next_item();
            assert_eq!(player.current_index(), 2);
            player.remove_at(0);
            assert_eq!(player.current_index(), 1);
            assert_eq!(player.item_count(), 2);
        }
    }
}

#[kithara::test(tokio)]
#[case(false)]
#[case(true)]
async fn player_remove_all_resets_state(#[case] with_resources: bool) {
    let player = PlayerImpl::new(PlayerConfig::default());
    if with_resources {
        player.insert(make_resource(1.0), None, None);
        player.insert(make_resource(2.0), None, None);
        player.insert(make_resource(3.0), None, None);
        assert_eq!(player.item_count(), 3);
    }
    player.remove_all_items();
    assert_eq!(player.item_count(), 0);
    assert_eq!(player.current_index(), 0);
    assert_eq!(player.status(), PlayerStatus::Unknown);
}

#[kithara::test(tokio)]
async fn player_advance_through_queue() {
    let player = PlayerImpl::new(PlayerConfig::default());
    player.insert(make_resource(1.0), None, None);
    player.insert(make_resource(2.0), None, None);
    player.insert(make_resource(3.0), None, None);
    assert_eq!(player.current_index(), 0);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 1);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 2);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 2);
}

#[kithara::test(tokio)]
async fn player_advance_emits_event() {
    let player = PlayerImpl::new(PlayerConfig::default());
    player.insert(make_resource(1.0), None, None);
    player.insert(make_resource(2.0), None, None);
    let mut rx = player.subscribe();
    player.advance_to_next_item();
    let event = rx.try_recv();
    assert!(matches!(
        event,
        Ok(Event::Player(PlayerEvent::CurrentItemChanged))
    ));
}

#[kithara::test]
fn replay_same_item_does_not_re_emit_current_item_changed() {
    let (player, _session) = make_offline_player(0.0);
    let (item, id) = make_tagged_resource("item-1", 0.05);
    player.insert(item, Some(id), None);
    let mut rx = player.subscribe();

    player.play();
    let first = drain_player_events(&player, &mut rx);
    let first_count = first
        .iter()
        .filter(|e| matches!(e, PlayerEvent::CurrentItemChanged))
        .count();
    assert_eq!(
        first_count, 1,
        "first play announces the item once: {first:?}"
    );

    player.play();
    let second = drain_player_events(&player, &mut rx);
    let second_count = second
        .iter()
        .filter(|e| matches!(e, PlayerEvent::CurrentItemChanged))
        .count();
    assert_eq!(
        second_count, 0,
        "resuming the same item must not re-announce CurrentItemChanged: {second:?}"
    );
}

#[kithara::test]
fn re_selecting_the_current_item_does_not_re_announce() {
    // Centralization delta: re-selecting the already-current index (e.g. while
    // paused) must not re-announce — announce gates on identity, not on calls.
    let (player, _session) = make_offline_player(0.0);
    let (item, id) = make_tagged_resource("item-1", 0.05);
    player.insert(item, Some(id), None);
    let mut rx = player.subscribe();

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    player
        .select_item(0, false)
        .expect("re-select current index");
    let after = drain_player_events(&player, &mut rx);
    let announces = after
        .iter()
        .filter(|e| matches!(e, PlayerEvent::CurrentItemChanged))
        .count();
    assert_eq!(
        announces, 0,
        "re-selecting the current item must not re-announce: {after:?}"
    );
}

#[kithara::test]
fn replacing_current_item_re_announces_on_next_play() {
    // Dual of suppression: replacing the audio under the current index must
    // re-announce on the next play — index equality must not mask a change.
    let (player, _session) = make_offline_player(0.0);
    let (item, id) = make_tagged_resource("item-1", 0.05);
    player.insert(item, Some(id), None);
    let mut rx = player.subscribe();

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    let (replacement, _) = make_tagged_resource("item-2", 0.05);
    player.replace_item_tagged(0, replacement, Some(Arc::from("item-2")));
    player.play();
    let after = drain_player_events(&player, &mut rx);
    let announces = after
        .iter()
        .filter(|e| matches!(e, PlayerEvent::CurrentItemChanged))
        .count();
    assert_eq!(
        announces, 1,
        "replacing the current item must re-announce on the next play: {after:?}"
    );
}

#[kithara::test(tokio)]
async fn player_play_without_audio_hardware_logs_warning() {
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    );
    player.insert(make_resource(1.0), None, None);
    player.play();
}

#[kithara::test]
fn queue_auto_advance_cf_zero_emits_terminal_before_current_changed() {
    let (player, session) = make_offline_player(0.0);
    let mut rx = player.subscribe();
    let (first, first_id) = make_tagged_resource("item-1", 0.05);
    let (second, _) = make_tagged_resource("item-2", 0.05);
    player.insert(first, Some(Arc::clone(&first_id)), None);
    player.insert(second, Some(Arc::from("item-2")), None);

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    let events = render_until_events(&player, &session, &mut rx, 256, |evs| {
        evs.iter()
            .any(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }))
            && evs
                .iter()
                .filter(|e| matches!(e, PlayerEvent::CurrentItemChanged))
                .count()
                >= 1
    });

    let item_end = events
        .iter()
        .position(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }));
    let handover_changed = item_end.and_then(|end_idx| {
        events
            .iter()
            .enumerate()
            .skip(end_idx + 1)
            .find(|(_, e)| matches!(e, PlayerEvent::CurrentItemChanged))
            .map(|(i, _)| i)
    });

    assert!(item_end.is_some(), "no terminal event: {events:?}");
    assert!(
        handover_changed.is_some(),
        "no post-terminal CurrentItemChanged: {events:?}"
    );
    assert!(item_end.unwrap() < handover_changed.unwrap());
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn queue_auto_advance_cf_one_activates_before_first_terminal_event() {
    let (player, session) = make_offline_player(1.0);
    let mut rx = player.subscribe();
    let (first, _) = make_tagged_resource("item-1", 1.5);
    let (second, _) = make_tagged_resource("item-2", 1.5);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    let events = render_until_events(&player, &session, &mut rx, 512, |evs| {
        evs.iter()
            .any(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }))
    });

    let handover_changed = events
        .iter()
        .position(|e| matches!(e, PlayerEvent::CurrentItemChanged));
    let item_end = events
        .iter()
        .position(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }));

    assert!(handover_changed.is_some(), "no handover event: {events:?}");
    assert!(item_end.is_some(), "no terminal event: {events:?}");
    assert!(handover_changed.unwrap() < item_end.unwrap());
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn queue_auto_advance_cf_ge_prefetch_still_advances() {
    let (player, session) = make_offline_player(4.0);
    let mut rx = player.subscribe();
    let (first, _) = make_tagged_resource("item-1", 5.0);
    let (second, _) = make_tagged_resource("item-2", 5.0);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    let events = render_until_events(&player, &session, &mut rx, 1024, |evs| {
        evs.iter()
            .any(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, PlayerEvent::PrefetchRequested)),
        "PrefetchRequested must fire when cf >= prefetch (windows coincide); got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PlayerEvent::HandoverRequested)),
        "HandoverRequested must fire; got {events:?}"
    );
    assert_eq!(
        player.current_index(),
        1,
        "auto-advance must reach the second item: {events:?}"
    );
}

#[kithara::test]
fn arm_next_loads_item_and_returns_src() {
    let (player, session) = make_offline_player(0.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("item-1", 0.05);
    let (second, _) = make_tagged_resource("item-2", 0.05);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let src = player
        .arm_next(1)
        .expect("BUG: arm_next succeeds for items[1]");
    assert_eq!(player.armed_next(), Some(1));
    let _ = session.render(512);
    player.process_notifications();
    assert_eq!(src.as_ref(), "memory://item-2");
}

#[kithara::test]
fn seek_seconds_updates_position_optimistically() {
    let (player, _session) = make_offline_player(0.0);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let outcome = player.seek_seconds(54.689_879_542).expect("seek must land");

    assert!(matches!(outcome, SeekOutcome::Landed { .. }));
    assert_eq!(player.position_seconds(), Some(54.689_879_542));
}

#[kithara::test]
fn arm_next_returns_none_for_empty_slot() {
    let (player, _session) = make_offline_player(0.0);
    player.set_auto_advance_enabled(false);
    player.reserve_slots(2);
    let (first, _) = make_tagged_resource("item-1", 0.05);
    player.replace_item_tagged(0, first, Some(Arc::from("item-1")));
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    assert!(player.arm_next(1).is_none(), "empty slot must yield None");
    assert_eq!(player.armed_next(), None);
}

#[kithara::test]
fn arm_next_idempotent_for_same_index() {
    let (player, _session) = make_offline_player(0.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("item-1", 0.05);
    let (second, _) = make_tagged_resource("item-2", 0.05);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let first_src = player.arm_next(1).unwrap();
    let second_src = player.arm_next(1).unwrap();
    assert_eq!(first_src.as_ref(), second_src.as_ref());
    assert_eq!(player.armed_next(), Some(1));
}

#[kithara::test]
fn arm_next_replaces_previously_armed_slot() {
    let (player, session) = make_offline_player(0.0);
    player.set_auto_advance_enabled(false);
    let (a, _) = make_tagged_resource("a", 0.05);
    let (b, _) = make_tagged_resource("b", 0.05);
    let (c, _) = make_tagged_resource("c", 0.05);
    player.insert(a, Some(Arc::from("a")), None);
    player.insert(b, Some(Arc::from("b")), None);
    player.insert(c, Some(Arc::from("c")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let first = player.arm_next(1).unwrap();
    let _ = session.render(512);
    player.process_notifications();
    let second = player.arm_next(2).unwrap();
    assert_ne!(first.as_ref(), second.as_ref());
    assert_eq!(player.armed_next(), Some(2));

    let _ = session.render(512);
    player.process_notifications();
}

#[kithara::test]
fn commit_next_index_mismatch_returns_typed_error() {
    let (player, _session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("a", 0.05);
    let (second, _) = make_tagged_resource("b", 0.05);
    player.insert(first, Some(Arc::from("a")), None);
    player.insert(second, Some(Arc::from("b")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player.arm_next(1).unwrap();

    let err = player.commit_next(2).expect_err("mismatch");
    assert!(matches!(
        err,
        PlayError::ArmIndexMismatch {
            requested: 2,
            armed: 1
        }
    ));
}

#[kithara::test]
fn commit_next_advances_index_and_publishes_event() {
    let (player, _session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("a", 0.05);
    let (second, _) = make_tagged_resource("b", 0.05);
    player.insert(first, Some(Arc::from("a")), None);
    player.insert(second, Some(Arc::from("b")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player.arm_next(1).unwrap();
    let mut rx = player.subscribe();

    player.commit_next(1).unwrap();
    assert_eq!(player.current_index(), 1);
    assert_eq!(player.armed_next(), None, "armed clears after commit");

    let mut saw_changed = false;
    for _ in 0..8 {
        match rx.try_recv() {
            Ok(Event::Player(PlayerEvent::CurrentItemChanged)) => saw_changed = true,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(saw_changed, "commit_next must publish CurrentItemChanged");
}

#[kithara::test]
fn commit_next_idempotent_when_already_activated() {
    let (player, _session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("a", 0.05);
    let (second, _) = make_tagged_resource("b", 0.05);
    player.insert(first, Some(Arc::from("a")), None);
    player.insert(second, Some(Arc::from("b")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player.arm_next(1).unwrap();

    player.commit_next(1).unwrap();
    player.commit_next(1).unwrap();
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn unarm_next_clears_when_not_activated_and_unloads() {
    let (player, session) = make_offline_player(0.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("a", 0.05);
    let (second, _) = make_tagged_resource("b", 0.05);
    player.insert(first, Some(Arc::from("a")), None);
    player.insert(second, Some(Arc::from("b")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    let src = player.arm_next(1).unwrap();
    let _ = session.render(512);
    player.process_notifications();

    player.unarm_next();
    assert_eq!(player.armed_next(), None);
    let _ = session.render(512);
    player.process_notifications();
    assert_eq!(src.as_ref(), "memory://b");
}

#[kithara::test]
fn unarm_next_preserves_activated_current() {
    let (player, _session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("a", 0.05);
    let (second, _) = make_tagged_resource("b", 0.05);
    player.insert(first, Some(Arc::from("a")), None);
    player.insert(second, Some(Arc::from("b")), None);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player.arm_next(1).unwrap();
    player.commit_next(1).unwrap();
    player.unarm_next();
    assert_eq!(player.armed_next(), None);
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn select_item_clears_pending_next_and_unloads_preloaded_track() {
    let (player, session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("item-1", 0.05);
    let (second, _) = make_tagged_resource("item-2", 0.05);
    let (third, _) = make_tagged_resource("item-3", 0.05);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);
    player.insert(third, Some(Arc::from("item-3")), None);

    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    let src = player.arm_next(1).expect("BUG: arm_next loads items[1]");
    assert_eq!(player.armed_next(), Some(1));

    player.select_item(2, true).unwrap();
    let _ = session.render(512);
    player.process_notifications();

    assert_eq!(player.armed_next(), None, "select_item must unarm");
    assert_eq!(src.as_ref(), "memory://item-2");
    assert_eq!(player.current_index(), 2);
}

/// Selecting the index that's already armed must promote the armed
/// slot, not unload-then-reload. Without this, the second user-driven
/// switch silently no-ops because `items[index]` was emptied by
/// `arm_next`'s `take()` and `enqueue_to_processor` returns `None`.
#[kithara::test]
fn select_item_on_armed_index_promotes_armed_slot() {
    let (player, session) = make_offline_player(1.0);
    player.set_auto_advance_enabled(false);
    let (first, _) = make_tagged_resource("item-1", 0.05);
    let (second, _) = make_tagged_resource("item-2", 0.05);
    player.insert(first, Some(Arc::from("item-1")), None);
    player.insert(second, Some(Arc::from("item-2")), None);

    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player.select_item(0, true).unwrap();
    let _ = session.render(256);
    let armed_src = player.arm_next(1).expect("BUG: arm_next loads items[1]");
    assert_eq!(player.armed_next(), Some(1));

    player.select_item(1, true).unwrap();
    let _ = session.render(256);
    player.process_notifications();

    assert_eq!(player.current_index(), 1);
    assert_eq!(player.armed_next(), None, "armed slot consumed by select");
    assert_eq!(armed_src.as_ref(), "memory://item-2");
}
