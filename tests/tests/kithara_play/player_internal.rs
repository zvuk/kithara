#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::sync::atomic::{AtomicU64, Ordering};

use kithara::{
    self,
    audio::ConsumerWakeMode,
    events::{Event, EventBus, EventReceiver, TrackId},
    platform::sync::{Arc, Mutex},
    play::{
        AllocatedSlot, Cmd, NodeInputs, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig,
        PlayerEvent, PlayerImpl, PlayerStatus, Reply, Resource, SeekOutcome, SessionBinding,
        SessionDispatcher, SessionDuckingMode, SessionSampleRate, SharedEq, SlotId,
        bridge::slot_channels,
    },
};
use kithara_integration_tests::{
    audio_mock::{MockReader, TestPcmReader},
    test_defaults::Consts,
};
use kithara_test_fixtures::integration_fixtures::constant_half;

use crate::bufpool_ext::{TestPools, pools};

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

fn make_resource(constant_half: &'static [u8], duration_secs: f64) -> Resource {
    Resource::from_reader(
        TestPcmReader::from_pcm(Consts::AUDIO_SPEC, duration_secs, constant_half),
        Some(Arc::from(format!("test-resource-{duration_secs}"))),
    )
}

/// A resource whose src carries `label`, so concurrent items in one test
/// stay distinguishable in failure output.
fn make_tagged_resource(
    constant_half: &'static [u8],
    label: &'static str,
    duration_secs: f64,
) -> Resource {
    Resource::from_reader(
        TestPcmReader::from_pcm(Consts::AUDIO_SPEC, duration_secs, constant_half),
        Some(Arc::from(format!("memory://{label}"))),
    )
}

struct FixtureSession {
    next_player: AtomicU64,
    next_slot: AtomicU64,
    nodes: Mutex<Vec<NodeInputs>>,
}

impl FixtureSession {
    fn new() -> Self {
        Self {
            next_player: AtomicU64::new(1),
            next_slot: AtomicU64::new(0),
            nodes: Mutex::default(),
        }
    }
}

impl SessionDispatcher<TestPools> for FixtureSession {
    fn exec(&self, cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
        let reply = match cmd {
            Cmd::RegisterPlayer { .. } => {
                Reply::PlayerRegistered(self.next_player.fetch_add(1, Ordering::Relaxed))
            }
            Cmd::AllocateSlot { .. } => {
                let slot = SlotId::new(self.next_slot.fetch_add(1, Ordering::Relaxed));
                let (inputs, control) = slot_channels(SharedEq::new(10));
                self.nodes.lock().push(inputs);
                Reply::SlotAllocated(AllocatedSlot::new(control, slot))
            }
            Cmd::QuerySampleRate => Reply::SampleRate(SessionSampleRate::new(
                None,
                Consts::NON_ZERO_SAMPLE_RATE.get(),
            )),
            Cmd::SessionDucking => Reply::SessionDucking(SessionDuckingMode::Off),
            _ => Reply::Ok,
        };
        Ok(reply)
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

fn fixture_session() -> Arc<dyn SessionDispatcher<TestPools>> {
    Arc::new(FixtureSession::new())
}

fn make_fixture_player(crossfade_duration: f32) -> (PlayerImpl<TestPools>, Arc<FixtureSession>) {
    let bus = EventBus::default();
    let session = Arc::new(FixtureSession::new());
    let player_config = PlayerConfig::builder()
        .bus(bus)
        .crossfade_duration(crossfade_duration)
        .sample_rate(Consts::NON_ZERO_SAMPLE_RATE)
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .session(SessionBinding::new(
            Arc::clone(&session) as Arc<dyn SessionDispatcher<TestPools>>,
            Consts::NON_ZERO_SAMPLE_RATE,
        ))
        .build();
    let player = PlayerImpl::new(player_config);
    (player, session)
}

fn prepared_player<const N: usize>(
    constant_half: &'static [u8],
    crossfade_duration: f32,
    labels: [&'static str; N],
) -> PlayerImpl<TestPools> {
    let (player, _session) = make_fixture_player(crossfade_duration);
    player.set_auto_advance_enabled(false);
    for label in labels {
        player.insert(
            make_tagged_resource(constant_half, label, 0.05),
            TrackId::allocate(),
            None,
        );
    }
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();
    player
}

fn default_player_config() -> PlayerConfig<TestPools> {
    PlayerConfig::builder()
        .sample_rate(Consts::NON_ZERO_SAMPLE_RATE)
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .session(SessionBinding::new(
            fixture_session(),
            Consts::NON_ZERO_SAMPLE_RATE,
        ))
        .build()
}

fn drain_player_events(player: &PlayerImpl<TestPools>, rx: &mut EventReceiver) -> Vec<PlayerEvent> {
    use kithara::platform::tokio::sync::broadcast::error::TryRecvError;
    player.process_notifications();
    let mut events = Vec::new();
    loop {
        match rx.try_recv().map(|env| env.event) {
            Ok(Event::Player(event)) => events.push(event),
            Ok(_) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
    events
}

#[kithara::test(tokio)]
#[case(InsertScenario::AppendTwice, 2)]
#[case(InsertScenario::InsertAtPosition, 3)]
async fn player_insert_scenarios(
    constant_half: &'static [u8],
    #[case] scenario: InsertScenario,
    #[case] expected_count: usize,
) {
    let player = PlayerImpl::new(default_player_config());
    player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
    player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
    if matches!(scenario, InsertScenario::InsertAtPosition) {
        player.insert(
            make_resource(constant_half, 3.0),
            TrackId::allocate(),
            Some(0),
        );
    }
    assert_eq!(player.item_count(), expected_count);
}

#[kithara::test(tokio)]
#[case(RemoveAtScenario::ExistingItem)]
#[case(RemoveAtScenario::OutOfBounds)]
#[case(RemoveAtScenario::ShiftCurrentIndex)]
async fn player_remove_at_scenarios(
    constant_half: &'static [u8],
    #[case] scenario: RemoveAtScenario,
) {
    let player = PlayerImpl::new(default_player_config());
    match scenario {
        RemoveAtScenario::ExistingItem => {
            player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
            player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
            let removed = player.remove_at(0);
            assert!(removed.is_some());
            assert_eq!(player.item_count(), 1);
        }
        RemoveAtScenario::OutOfBounds => {
            player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
            assert!(player.remove_at(5).is_none());
            assert_eq!(player.item_count(), 1);
        }
        RemoveAtScenario::ShiftCurrentIndex => {
            player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
            player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
            player.insert(make_resource(constant_half, 3.0), TrackId::allocate(), None);
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
async fn player_remove_all_resets_state(
    constant_half: &'static [u8],
    #[case] with_resources: bool,
) {
    let player = PlayerImpl::new(default_player_config());
    if with_resources {
        player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
        player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
        player.insert(make_resource(constant_half, 3.0), TrackId::allocate(), None);
        assert_eq!(player.item_count(), 3);
    }
    player.remove_all_items();
    assert_eq!(player.item_count(), 0);
    assert_eq!(player.current_index(), 0);
    assert_eq!(player.status(), PlayerStatus::Unknown);
}

#[kithara::test(tokio)]
async fn player_advance_through_queue(constant_half: &'static [u8]) {
    let player = PlayerImpl::new(default_player_config());
    player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
    player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
    player.insert(make_resource(constant_half, 3.0), TrackId::allocate(), None);
    assert_eq!(player.current_index(), 0);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 1);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 2);
    player.advance_to_next_item();
    assert_eq!(player.current_index(), 2);
}

#[kithara::test(tokio)]
async fn player_advance_emits_event(constant_half: &'static [u8]) {
    let player = PlayerImpl::new(default_player_config());
    player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
    player.insert(make_resource(constant_half, 2.0), TrackId::allocate(), None);
    let mut rx = player.subscribe();
    player.advance_to_next_item();
    let event = rx.try_recv().map(|env| env.event);
    assert!(matches!(
        event,
        Ok(Event::Player(PlayerEvent::CurrentItemChanged))
    ));
}

#[kithara::test]
fn replay_same_item_does_not_re_emit_current_item_changed(constant_half: &'static [u8]) {
    let (player, _session) = make_fixture_player(0.0);
    let item = make_tagged_resource(constant_half, "item-1", 0.05);
    player.insert(item, TrackId::allocate(), None);
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

/// `insert` is the queue entry that never passes `ConfigPrep`, so adoption
/// into the real-time arena is the only place a session wake policy can reach
/// the resource. A reader left on the direct-consumer default publishes its
/// reader events inline from the audio callback.
#[kithara::test(tokio)]
async fn an_inserted_resource_adopts_the_session_wake_mode() {
    let (player, _session) = make_fixture_player(0.0);
    let (reader, recorded) = MockReader::wake_mode_tracking(Consts::AUDIO_SPEC);

    player.insert(
        Resource::from_reader(reader, None),
        TrackId::allocate(),
        None,
    );
    player
        .ensure_engine_started()
        .expect("start the fixture engine");
    player.ensure_slot().expect("allocate the fixture slot");
    player
        .select_item(0, true)
        .expect("select the inserted item");

    let applied = *recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        applied,
        Some(ConsumerWakeMode::RealtimeDeferred),
        "adoption must apply the session wake mode to an inserted resource"
    );
}

#[kithara::test]
fn re_selecting_the_current_item_does_not_re_announce(constant_half: &'static [u8]) {
    // Centralization delta: re-selecting the already-current index (e.g. while
    // paused) must not re-announce — announce gates on identity, not on calls.
    let (player, _session) = make_fixture_player(0.0);
    let item = make_tagged_resource(constant_half, "item-1", 0.05);
    player.insert(item, TrackId::allocate(), None);
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
fn replacing_current_item_re_announces_on_next_play(constant_half: &'static [u8]) {
    // Dual of suppression: replacing the audio under the current index must
    // re-announce on the next play — index equality must not mask a change.
    let (player, _session) = make_fixture_player(0.0);
    let item = make_tagged_resource(constant_half, "item-1", 0.05);
    player.insert(item, TrackId::allocate(), None);
    let mut rx = player.subscribe();

    player.play();
    let _ = drain_player_events(&player, &mut rx);

    let replacement = make_tagged_resource(constant_half, "item-2", 0.05);
    player.replace_item(0, replacement, TrackId::allocate());
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

#[kithara::test]
fn arm_next_loads_item_and_returns_src(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 0.0, ["item-1", "item-2"]);

    let src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    assert_eq!(player.armed_next(), Some(1));
    assert_eq!(src.as_ref(), "memory://item-2");
}

#[kithara::test]
fn seek_seconds_updates_position_optimistically() {
    let (player, _session) = make_fixture_player(0.0);
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let outcome = player.seek_seconds(54.689_879_542).expect("seek must land");

    assert!(matches!(outcome, SeekOutcome::Landed { .. }));
    assert_eq!(player.position_seconds(), Some(54.689_879_542));
}

#[kithara::test]
fn arm_next_returns_none_for_empty_slot(constant_half: &'static [u8]) {
    let (player, _session) = make_fixture_player(0.0);
    player.set_auto_advance_enabled(false);
    player.reserve_slots(2);
    let first = make_tagged_resource(constant_half, "item-1", 0.05);
    player.replace_item(0, first, TrackId::allocate());
    player.ensure_engine_started().unwrap();
    player.ensure_slot().unwrap();

    let src = player.arm_next(1).expect("arm_next succeeds");
    assert!(src.is_none(), "empty slot must yield None");
    assert_eq!(player.armed_next(), None);
}

#[kithara::test]
fn arm_next_idempotent_for_same_index(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 0.0, ["item-1", "item-2"]);

    let first_src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    let second_src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    assert_eq!(first_src.as_ref(), second_src.as_ref());
    assert_eq!(player.armed_next(), Some(1));
}

#[kithara::test]
fn arm_next_replaces_previously_armed_slot(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 0.0, ["a", "b", "c"]);

    let first = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    let second = player
        .arm_next(2)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    assert_ne!(first.as_ref(), second.as_ref());
    assert_eq!(player.armed_next(), Some(2));
}

#[kithara::test]
fn commit_next_index_mismatch_returns_typed_error(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["a", "b"]);
    player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");

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
fn commit_next_advances_index_and_publishes_event(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["a", "b"]);
    player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    let mut rx = player.subscribe();

    player.commit_next(1).unwrap();
    assert_eq!(player.current_index(), 1);
    assert_eq!(player.armed_next(), None, "armed clears after commit");

    let mut saw_changed = false;
    for _ in 0..8 {
        match rx.try_recv().map(|env| env.event) {
            Ok(Event::Player(PlayerEvent::CurrentItemChanged)) => saw_changed = true,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(saw_changed, "commit_next must publish CurrentItemChanged");
}

#[kithara::test]
fn commit_next_idempotent_when_already_activated(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["a", "b"]);
    player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");

    player.commit_next(1).unwrap();
    player.commit_next(1).unwrap();
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn unarm_next_clears_when_not_activated_and_unloads(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 0.0, ["a", "b"]);
    let src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");

    player.unarm_next();
    assert_eq!(player.armed_next(), None);
    assert_eq!(src.as_ref(), "memory://b");
}

#[kithara::test]
fn unarm_next_preserves_activated_current(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["a", "b"]);
    player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    player.commit_next(1).unwrap();
    player.unarm_next();
    assert_eq!(player.armed_next(), None);
    assert_eq!(player.current_index(), 1);
}

#[kithara::test]
fn select_item_clears_pending_next_and_unloads_preloaded_track(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["item-1", "item-2", "item-3"]);
    let src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    assert_eq!(player.armed_next(), Some(1));

    player.select_item(2, true).unwrap();

    assert_eq!(player.armed_next(), None, "select_item must unarm");
    assert_eq!(src.as_ref(), "memory://item-2");
    assert_eq!(player.current_index(), 2);
}

/// Selecting the index that's already armed must promote the armed
/// slot, not unload-then-reload. Without this, the second user-driven
/// switch silently no-ops because `items[index]` was emptied by
/// `arm_next`'s `take()` and `enqueue_to_processor` returns `None`.
#[kithara::test]
fn select_item_on_armed_index_promotes_armed_slot(constant_half: &'static [u8]) {
    let player = prepared_player(constant_half, 1.0, ["item-1", "item-2"]);
    player.select_item(0, true).unwrap();
    let armed_src = player
        .arm_next(1)
        .expect("arm_next succeeds")
        .expect("populated slot returns src");
    assert_eq!(player.armed_next(), Some(1));

    player.select_item(1, true).unwrap();
    player.process_notifications();

    assert_eq!(player.current_index(), 1);
    assert_eq!(player.armed_next(), None, "armed slot consumed by select");
    assert_eq!(armed_src.as_ref(), "memory://item-2");
}
