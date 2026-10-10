use std::{cell::RefCell, num::NonZeroU32};

use audioadapter_buffers::direct::InterleavedSlice;
use firewheel::{
    ActivateInfo, FirewheelContext,
    backend::BackendProcessInfo,
    node::{NodeID, StreamStatus},
    processor::FirewheelProcessor,
};
use kithara_command::When;
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::{SessionEpoch, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::{
    Beat, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridState, BeatGridUnavailable, MapAxis,
    MapPoint, MapPosition, SessionAxis,
};

use crate::{
    api::{SessionTransportSnapshot, Tempo},
    consts,
    host::{HostSettingsChange, HostSettingsExec},
    session::{
        dispatch::{invalidate_audio_route, tick_session},
        tests::graph::{GraphSession, committed_transport, state as test_state},
    },
};

/// The process-wide output device, held by whichever stream owns it.
#[derive(Default)]
struct AudioDevice {
    processor: Option<FirewheelProcessor>,
    retired_processors: Vec<FirewheelProcessor>,
    defer_processor_drop: bool,
    next_stream: u64,
    owner: u64,
}

thread_local! {
    static DEVICE: RefCell<AudioDevice> = RefCell::new(AudioDevice::default());
}

fn device<R>(f: impl FnOnce(&mut AudioDevice) -> R) -> R {
    DEVICE.with(|cell| f(&mut cell.borrow_mut()))
}

/// A fixture stream. It owns nothing but its identity: the processor lives
/// in the thread-local device, and dropping the stream is what retires it.
struct TestStream {
    stream: u64,
}

type TestState = GraphSession<TestStream>;

impl Drop for TestStream {
    fn drop(&mut self) {
        device(|dev| {
            if dev.owner == self.stream {
                let processor = dev.processor.take();
                if dev.defer_processor_drop {
                    dev.retired_processors.extend(processor);
                }
            }
        });
    }
}

fn start_test_stream(ctx: &mut FirewheelContext, sample_rate: u32) -> Result<TestStream, String> {
    let stream = device(|dev| {
        dev.next_stream += 1;
        dev.next_stream
    });
    let sample_rate = NonZeroU32::new(sample_rate).unwrap_or(TestState::DEFAULT_SAMPLE_RATE);
    let max_block_frames = NonZeroU32::new(512).expect("invariant: fixture block size is non-zero");
    let processor = ctx
        .activate(ActivateInfo {
            sample_rate,
            max_block_frames,
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
        })
        .map_err(|err| err.to_string())?;
    device(|dev| {
        dev.owner = stream;
        dev.processor = Some(processor);
    });
    Ok(TestStream { stream })
}

/// `false` means no stream owns the device, which is what silence looks like.
fn deliver_one_block() -> bool {
    device(|dev| {
        let Some(processor) = dev.processor.as_mut() else {
            return false;
        };
        let mut output = [0.0_f32; consts::GRAPH_BLOCK_FRAMES * 2];
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0)
            .expect("invariant: an empty input adapter is well formed");
        let mut output = InterleavedSlice::new_mut(&mut output, 2, consts::GRAPH_BLOCK_FRAMES)
            .expect("invariant: the fixture output block is stereo");
        processor.process(
            &input,
            &mut output,
            BackendProcessInfo {
                frames: consts::GRAPH_BLOCK_FRAMES,
                // Firewheel stamps a block with its own clock type, so the
                // platform clock cannot be handed over here.
                process_timestamp: Some(bevy_platform::time::Instant::now()),
                duration_since_stream_start: Duration::ZERO,
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
                process_to_playback_delay: None,
            },
        );
        true
    })
}

fn processed_frames(state: &TestState) -> i64 {
    state
        .ctx
        .as_ref()
        .map_or(-1, |fw_ctx| fw_ctx.audio_clock().samples.0)
}

/// Attaches a deck, which the session registers and starts.
fn insert(state: &mut TestState) -> BeatGridId {
    let grid_id = BeatGridId::allocate().expect("fixture grid id");
    match state.install(grid_id, EventBus::default()) {
        Ok(_) => grid_id,
        Err(err) => panic!("the deck failed to start: {err}"),
    }
}

/// Stops the deck `grid_id` and removes it from the session.
fn remove(state: &mut TestState, grid_id: BeatGridId) {
    match state.remove(grid_id) {
        Ok(()) => {}
        Err(err) => panic!("the deck failed to leave: {err}"),
    }
}

fn slot_node(state: &TestState, grid_id: BeatGridId) -> NodeID {
    state
        .deck_nodes
        .iter()
        .find(|deck| deck.id == grid_id)
        .map(|deck| deck.node)
        .expect("a started deck has its slot node")
}

fn render_and_read_session_grid(state: &mut TestState) -> SessionTransportSnapshot {
    assert!(deliver_one_block(), "the transport must render a block");
    committed_transport(state).expect("the rendered block committed the transport")
}

#[kithara::test]
fn a_session_tick_publishes_the_session_grid_the_graph_committed() {
    device(|dev| *dev = AudioDevice::default());
    let mut state = test_state(start_test_stream);
    insert(&mut state);
    assert!(deliver_one_block(), "the transport must render a block");

    assert!(tick_session(&mut state).is_ok());

    let committed = state
        .transport_observation
        .as_mut()
        .expect("a running stream keeps the transport observation")
        .read()
        .snapshot()
        .expect("the rendered block committed the tempo")
        .session_grid();
    assert_eq!(
        state.root_view.grid(),
        committed,
        "with no synchronization command, the session tick publishes the committed grid"
    );
}

/// The Host keeps the output engaged only while it holds a deck: handing
/// back the last one releases the device, so the platform's audio session
/// can be deactivated.
#[kithara::test]
fn removing_the_last_deck_releases_the_output() {
    device(|dev| *dev = AudioDevice::default());
    let mut state = test_state(start_test_stream);
    let deck = insert(&mut state);

    remove(&mut state, deck);

    assert!(state.ctx.is_none());
}

/// The browser hands its output over once, on a user gesture, and a closed
/// `AudioContext` can never be resumed: releasing it on idle leaves every
/// later context suspended, so the player reports playback over silence.
/// The exception belongs to the session that declared it, not to the
/// target it happens to be compiled for — a mock backend on the same
/// target still releases its device above.
#[kithara::test]
fn a_session_that_retains_its_output_keeps_it_when_the_last_deck_leaves() {
    device(|dev| *dev = AudioDevice::default());
    let mut state = test_state(start_test_stream);
    state.retains_output = true;
    let deck = insert(&mut state);

    remove(&mut state, deck);

    assert!(
        state.ctx.is_some(),
        "a session whose device cannot be rebuilt must hold it while idle"
    );
}

/// A removed deck takes its slot node out of the graph the other decks
/// keep rendering.
#[kithara::test]
fn a_removed_deck_takes_its_slot_node_out_of_the_graph() {
    device(|dev| *dev = AudioDevice::default());
    let mut state = test_state(start_test_stream);
    let leaving = insert(&mut state);
    let staying = insert(&mut state);
    let (left, kept) = (slot_node(&state, leaving), slot_node(&state, staying));

    remove(&mut state, leaving);

    let ctx = state
        .ctx
        .as_ref()
        .expect("the deck still playing keeps the output");
    assert!(
        !ctx.contains_node(left),
        "a removed deck's slot node leaves the graph"
    );
    assert!(
        ctx.contains_node(kept),
        "the deck still playing keeps its slot node"
    );
}

#[kithara::test]
fn a_second_player_started_after_the_last_one_left_gets_a_processed_stream() {
    device(|dev| *dev = AudioDevice::default());
    let mut state = test_state(start_test_stream);

    let first = insert(&mut state);
    assert!(
        deliver_one_block(),
        "the first player's stream must own the output device"
    );

    remove(&mut state, first);

    assert!(
        state.ctx.is_none(),
        "the session must release the output device once no player feeds it"
    );

    insert(&mut state);

    let before = processed_frames(&state);
    assert!(
        deliver_one_block(),
        "the second player's stream must own the output device"
    );
    assert!(
        processed_frames(&state) > before,
        "the second player's stream delivered no processed callback"
    );
}

#[kithara::test]
fn idle_context_recreation_advances_generation_before_deferred_processor_drop() {
    device(|dev| {
        *dev = AudioDevice::default();
        dev.defer_processor_drop = true;
    });
    let mut state = test_state(start_test_stream);
    let initial = state.root.grid().clone();
    assert_eq!(initial.revision(), BeatGridRevision::first());
    assert_eq!(
        initial.state(),
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    );
    assert_eq!(
        initial.axis(),
        MapAxis::Session(SessionAxis::new(
            TestState::DEFAULT_SAMPLE_RATE,
            SessionEpoch::new(0),
        ))
    );
    assert_eq!(
        state
            .reserved_session_grid
            .expect("the session seeds its session-grid generation")
            .stamp()
            .expect("the initial session-grid revision is committed"),
        initial.stamp()
    );
    let first_player = insert(&mut state);
    let before = render_and_read_session_grid(&mut state);
    let first_live = state.root.grid().clone();
    assert_eq!(first_live, before.session_grid());
    assert_eq!(
        first_live.revision(),
        initial
            .revision()
            .checked_next()
            .expect("the fixture grid revision can advance")
    );
    let old_beat = MapPoint::new(
        before.session_grid_stamp(),
        Beat::new(1.0).expect("invariant: fixture beat is finite"),
    );

    invalidate_audio_route(&mut state, "deferred route before idle teardown")
        .expect("the route restarts");
    let route_boundary = state.root.grid().clone();
    assert_eq!(
        state
            .reserved_session_grid
            .expect("the deferred route owns a reserved generation")
            .stamp()
            .expect("the deferred route reservation has a revision"),
        route_boundary.stamp()
    );
    assert!(state.stream_needs_restart);

    remove(&mut state, first_player);
    let unavailable = state.root.grid().clone();
    assert_eq!(
        unavailable.revision(),
        first_live
            .revision()
            .checked_next()
            .and_then(BeatGridRevision::checked_next)
            .expect("the fixture grid revision can advance twice")
    );
    assert_eq!(
        unavailable.state(),
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    );
    assert_eq!(
        unavailable.axis(),
        MapAxis::Session(SessionAxis::new(
            TestState::DEFAULT_SAMPLE_RATE,
            SessionEpoch::new(2),
        ))
    );
    assert_eq!(
        state
            .reserved_session_grid
            .expect("idle teardown returns session-grid generation")
            .stamp()
            .expect("the restart boundary has a reserved revision"),
        unavailable.stamp()
    );
    assert!(
        state.ctx.is_none(),
        "idle teardown must destroy the context"
    );
    device(|dev| {
        assert_eq!(
            dev.retired_processors.len(),
            1,
            "old processor must still be alive while the new context starts"
        );
    });

    insert(&mut state);
    let after = render_and_read_session_grid(&mut state);
    let second_live = state.root.grid().clone();
    assert_eq!(second_live, after.session_grid());
    assert_eq!(
        second_live.revision(),
        unavailable
            .revision()
            .checked_next()
            .expect("the fixture grid revision can advance")
    );

    assert_eq!(
        before.session_grid_stamp().grid_id(),
        after.session_grid_stamp().grid_id(),
        "one session keeps one session-grid identity"
    );
    assert!(after.session_epoch() > before.session_epoch());
    assert!(after.session_grid_stamp().revision() > before.session_grid_stamp().revision());
    assert!(matches!(
        after.session_grid().position_at(old_beat),
        BeatGridQuery::Stale { expected, given }
            if expected == after.session_grid_stamp()
                && given == before.session_grid_stamp()
    ));
    device(|dev| {
        assert_eq!(dev.retired_processors.len(), 1);
        dev.retired_processors.clear();
        dev.defer_processor_drop = false;
    });
}

#[kithara::test]
fn deferred_route_restart_converges_before_unrendered_idle_shutdown() {
    device(|dev| {
        *dev = AudioDevice::default();
        dev.defer_processor_drop = true;
    });
    let mut state = test_state(start_test_stream);
    let player = insert(&mut state);
    let live = render_and_read_session_grid(&mut state);

    invalidate_audio_route(&mut state, "test route restart").expect("the route restarts");
    let reserved = state.root.grid().clone();
    assert!(reserved.revision() > live.session_grid_stamp().revision());
    assert_eq!(
        state
            .reserved_session_grid
            .expect("the delayed processor keeps an exact route reservation")
            .stamp()
            .expect("the route reservation has a revision"),
        reserved.stamp()
    );
    assert!(state.stream_needs_restart);
    let rate = state.root_view.sample_rate();
    assert_eq!(
        rate.measured, None,
        "a pending route restart publishes no measured stream"
    );
    assert_eq!(rate.requested, 44_100);
    assert_eq!(
        committed_transport(&mut state),
        None,
        "a route restart holding the session grid has nothing committed to read"
    );
    assert_eq!(
        state.root.grid().clone(),
        reserved,
        "a stale transport observation must not replace the route reservation"
    );
    let tempo = Tempo::new(121.0).expect("invariant: fixture tempo is valid");
    assert!(
        state
            .exec(HostSettingsChange::Tempo(tempo), When::Next, &mut ())
            .is_ok(),
        "a change for the next block waits in the queue across a route restart"
    );
    assert_eq!(
        state.root.grid().clone(),
        reserved,
        "a queued change must not touch an unfinished route boundary"
    );
    device(|dev| {
        assert_eq!(dev.retired_processors.len(), 1);
        dev.retired_processors.clear();
        dev.defer_processor_drop = false;
    });
    assert!(tick_session(&mut state).is_ok());
    assert!(!state.stream_needs_restart);
    assert!(state.reserved_session_grid.is_none());
    let converged = state
        .transport_observation
        .as_mut()
        .expect("the restarted stream keeps the transport observation")
        .read()
        .session_grid()
        .stamp()
        .expect("the restarted transport has a grid revision");
    assert_eq!(converged, reserved.stamp());
    let restart_frame = SessionFrame::new(
        state
            .ctx
            .as_ref()
            .expect("the restarted stream keeps its context")
            .audio_clock()
            .samples
            .0,
    );

    assert!(
        deliver_one_block(),
        "the restarted processor must render its preserved transport"
    );
    let restarted =
        committed_transport(&mut state).expect("the restarted transport committed its first block");
    let published = state.root.grid().clone();
    assert_eq!(published.state(), BeatGridState::Live);
    let MapAxis::Session(reserved_axis) = reserved.axis() else {
        panic!("the route reservation uses the session axis")
    };
    let MapAxis::Session(published_axis) = published.axis() else {
        panic!("the restarted grid uses the session axis")
    };
    assert_eq!(published_axis.epoch(), reserved_axis.epoch());
    assert!(published.revision() > reserved.revision());
    assert_eq!(published, restarted.session_grid());
    assert_eq!(
        restarted
            .anchor()
            .frame_at(live.position())
            .expect("the preserved beat is representable on the restarted axis"),
        restart_frame
    );
    let old_position = MapPoint::new(
        live.session_grid_stamp(),
        MapPosition::Session(SessionFrame::new(0)),
    );
    assert!(matches!(
        published.beat_at(old_position),
        BeatGridQuery::Stale { .. }
    ));

    remove(&mut state, player);

    let unavailable = state.root.grid().clone();
    assert!(unavailable.revision() > reserved.revision());
    assert_eq!(
        unavailable.state(),
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    );
    assert!(state.ctx.is_none());
}
