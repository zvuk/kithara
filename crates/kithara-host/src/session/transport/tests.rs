use std::num::NonZeroU32;

use firewheel::{
    FirewheelContext,
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
        ProcStreamCtx, ProcessStatus, StreamStatus,
    },
};
use kithara_command::{
    Batch, Outcome, Port, Rejection, ScopedReceipt, ScopedSender, When, scoped_channel,
};
use kithara_config::{Config, ConfigOwner};
use kithara_platform::time::Duration;
use kithara_render::{
    bridge::DeckProtocol,
    rt::{install_render_context, read_render_context},
};
use kithara_signal::{SessionEpoch, SessionFrame};
use kithara_test_utils::{bufpool::TestPools, kithara};
use kithara_warp::{Beat, BeatGridId, BeatGridQuery, BeatsPerMinute, MapPoint, MapPosition};
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{SessionGridGeneration, TransportObservation, TransportProcessError},
    node::SessionTransportProcessor,
    process::{
        TransportObservationInput, TransportState, converge_transport_restart, process_transport,
    },
};
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo, TransportRevision},
    consts,
    host::{HostSettings, HostSettingsChange, HostSettingsExec},
    session::{
        queue::{HostPart, HostProtocol, settle_receipt},
        state::{SessionState, ensure_ctx},
        tests::graph,
    },
};

type Harness = (
    SessionTransportProcessor,
    ProcExtra,
    Output<TransportObservation>,
    ScopedSender<HostProtocol, DeckProtocol>,
);

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(consts::TRANSPORT_SAMPLE_RATE)
        .expect("invariant: static sample rate is non-zero")
}

fn second_revision() -> TransportRevision {
    TransportRevision::first()
        .checked_next()
        .expect("invariant: second transport revision exists")
}

fn tempo(beats_per_minute: f64) -> Tempo {
    Tempo::new(beats_per_minute).expect("invariant: test tempo is valid")
}

fn proc_info_at(clock_samples: i64) -> ProcInfo {
    ProcInfo {
        sample_rate: sample_rate(),
        frames: consts::TRANSPORT_BLOCK_FRAMES,
        in_silence_mask: SilenceMask::default(),
        out_silence_mask: SilenceMask::default(),
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        total_cpu_seconds_recip: 1.0,
        process_to_playback_delay: None,
        did_just_unbypass: false,
        last_marker_instant: InstantSamples(0),
        sample_rate_recip: f64::from(consts::TRANSPORT_SAMPLE_RATE).recip(),
        clock_samples: InstantSamples(clock_samples),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    }
}

fn block_frame(blocks: usize) -> i64 {
    i64::try_from(consts::TRANSPORT_BLOCK_FRAMES * blocks)
        .expect("invariant: test block frame fits i64")
}

fn proc_extra() -> (
    ProcExtra,
    Output<TransportObservation>,
    ScopedSender<HostProtocol, DeckProtocol>,
) {
    let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    let session_grid = SessionGridGeneration::new(
        BeatGridId::allocate().expect("invariant: fixture grid identity space is available"),
    );
    let initial = TransportObservation::new(None, session_grid);
    let (observation_input, observation_output) = triple_buffer(&initial);
    let config = crate::HostConfig::<TestPools>::builder()
        .build()
        .channel_config();
    let (queue, inbox) = scoped_channel(config);
    let mut store = ProcStore::with_capacity(3);
    assert!(install_render_context(&mut store).is_ok());
    assert!(
        store
            .insert(TransportState::new(
                inbox,
                HostSettings::default(),
                session_grid,
                config.values().root.values().capacity.get(),
            ))
            .is_ok()
    );
    assert!(
        store
            .insert(TransportObservationInput::new(observation_input))
            .is_ok()
    );
    (
        ProcExtra {
            logger,
            store,
            scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(
                consts::TRANSPORT_BLOCK_FRAMES,
            ),
            declick_values: DeclickValues::new(
                NonZeroU32::new(16).expect("invariant: static fade is non-zero"),
            ),
        },
        observation_output,
        queue,
    )
}

fn process_node(processor: &mut SessionTransportProcessor, info: &ProcInfo, extra: &mut ProcExtra) {
    let inputs: [&[f32]; 0] = [];
    let mut outputs: [&mut [f32]; 0] = [];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let status = processor.process(info, buffers, extra);
    assert_eq!(status, ProcessStatus::ClearAllOutputs);
}

fn stop_stream(processor: &mut SessionTransportProcessor, extra: &mut ProcExtra) {
    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
}

#[kithara::test]
fn offline_inbox_returns_only_after_the_entire_render_turn() {
    let (mut extra, _observation, _queue) = proc_extra();
    let mut owner = extra
        .store
        .try_get_mut::<TransportState>()
        .expect("transport state")
        .park_offline_inbox()
        .expect("park offline inbox");
    let mut processor = SessionTransportProcessor;
    let mut clock = 0;
    for _turn in 0..2 {
        owner.begin_render(21).expect("begin offline render");
        for frames in [3, 7, 11] {
            let mut info = proc_info_at(clock);
            info.frames = frames;
            process_node(&mut processor, &info, &mut extra);
            extra
                .store
                .try_get_mut::<TransportState>()
                .expect("transport state")
                .return_inbox(frames)
                .expect("finish graph block");
            clock += i64::try_from(frames).expect("fixture frame count");
        }
        owner
            .end_render()
            .expect("inbox returns after the final block");
        owner
            .retire_closing()
            .expect("owner holds the parked inbox");
    }
}

fn send_tempo(
    queue: &mut ScopedSender<HostProtocol, DeckProtocol>,
    beats_per_minute: f64,
    when: When<SessionFrame>,
) {
    let batch = Batch {
        basis: Vec::new(),
        commands: vec![HostPart::Settings(HostSettingsChange::Tempo(tempo(
            beats_per_minute,
        )))],
    };
    assert!(queue.send(when, batch).is_ok());
    queue.publish().expect("publish transport command");
}

fn outcome(queue: &mut ScopedSender<HostProtocol, DeckProtocol>) -> Outcome<HostProtocol> {
    let receipt = queue
        .receipt()
        .expect("invariant: the transport answered the batch");
    let ScopedReceipt::Root(receipt) = receipt else {
        panic!("the transport answers on the root channel");
    };
    let (outcome, _batch) = receipt.into();
    outcome
}

/// A session whose transport the test renders by hand: its context is built
/// and never started, so the transport store stays with the session owner.
fn owned_session() -> SessionState<(), TestPools> {
    let sample_rate = NonZeroU32::new(consts::TRANSPORT_SAMPLE_RATE)
        .expect("invariant: the fixture rate is non-zero");
    let mut state = graph::state_for(sample_rate, |_ctx, _sample_rate| Ok(()));
    assert!(ensure_ctx(&mut state).is_ok());
    state
}

/// Renders one transport block at `clock_samples`, then settles its receipts
/// as the session owner does between blocks.
fn render(
    state: &mut SessionState<(), TestPools>,
    clock_samples: i64,
) -> Result<(), TransportProcessError> {
    if let Some(channel) = state.channel.as_mut() {
        channel.publish().expect("an open host channel publishes");
    }
    let store = state
        .ctx
        .as_mut()
        .and_then(FirewheelContext::proc_store_mut)
        .expect("invariant: a context never started keeps its store");
    let result = process_transport(&proc_info_at(clock_samples), store).map(drop);
    while let Some(receipt) = state.channel.as_mut().and_then(ScopedSender::receipt) {
        let ScopedReceipt::Root(receipt) = receipt else {
            panic!("transport-only graph returns root receipts");
        };
        settle_receipt(state, &receipt);
    }
    result
}

fn configure_tempo(state: &mut SessionState<(), TestPools>, beats_per_minute: f64) {
    assert!(
        state
            .exec(
                HostSettingsChange::Tempo(tempo(beats_per_minute)),
                When::Next,
                &mut ()
            )
            .is_ok()
    );
}

fn observation(output: &mut Output<TransportObservation>) -> TransportObservation {
    *output.read()
}

fn snapshot(output: &mut Output<TransportObservation>) -> SessionTransportSnapshot {
    observation(output)
        .snapshot()
        .expect("invariant: active transport publishes a snapshot")
}

/// A transport that rendered its first block.
fn active_harness() -> Harness {
    let (mut extra, output, queue) = proc_extra();
    let mut processor = SessionTransportProcessor;
    process_node(&mut processor, &proc_info_at(0), &mut extra);
    (processor, extra, output, queue)
}

/// A transport whose tempo moved to 60 BPM at the start of the third block.
fn retargeted_harness() -> Harness {
    let (mut processor, mut extra, output, mut queue) = active_harness();
    send_tempo(
        &mut queue,
        60.0,
        When::At(SessionFrame::new(block_frame(2))),
    );
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);
    (processor, extra, output, queue)
}

#[kithara::test]
fn the_first_block_anchors_session_beat_zero_at_the_host_tempo() {
    let (_processor, _extra, mut output, _queue) = active_harness();
    let first = snapshot(&mut output);

    assert_eq!(first.tempo(), HostSettings::default().tempo());
    assert_eq!(first.revision(), TransportRevision::first());
    assert_eq!(
        first
            .anchor()
            .frame_at(SessionBeat::new(0.0).expect("invariant: beat zero is finite"))
            .expect("invariant: beat zero is representable on the first anchor"),
        SessionFrame::new(0)
    );
}

#[kithara::test]
fn transport_frame_carries_the_exact_processed_musical_context() {
    let (_processor, mut extra, _output, _queue) = active_harness();
    let frame = process_transport(&proc_info_at(block_frame(1)), &mut extra.store)
        .expect("invariant: the next contiguous transport block is valid");
    let beats = frame
        .trajectory
        .beat_at(SessionFrame::new(block_frame(1)))
        .expect("finite beat")
        ..frame
            .trajectory
            .beat_at(SessionFrame::new(block_frame(2)))
            .expect("finite beat");

    assert_eq!(frame.session_epoch, SessionEpoch::new(0));
    assert_eq!(frame.transport_revision, TransportRevision::first());
    assert!((f64::from(beats.start) - 0.02).abs() <= f64::EPSILON);
    assert!((f64::from(beats.end) - 0.04).abs() <= f64::EPSILON);
}

#[kithara::test]
fn pre_process_publishes_the_exact_render_context() {
    let (_processor, extra, _output, _queue) = active_harness();
    let info = proc_info_at(0);
    let context = read_render_context(&extra.store, &info)
        .expect("invariant: the pre-process node published this exact block");

    assert_eq!(
        context.output().output_frames(),
        &(SessionFrame::new(0)..SessionFrame::new(block_frame(1)))
    );
    assert_eq!(context.output().sample_rate(), sample_rate());
    assert_eq!(context.output().session_epoch(), SessionEpoch::new(0));
    assert_eq!(
        context.output().transport_revision(),
        Some(TransportRevision::first())
    );
    let beats = context
        .session_beats()
        .expect("invariant: active transport carries a musical range");
    assert!(f64::from(beats.start).abs() <= f64::EPSILON);
    assert!((f64::from(beats.end) - 0.02).abs() <= f64::EPSILON);
}

#[kithara::test]
fn stale_subblock_cannot_reuse_the_full_render_context() {
    let (_processor, extra, _output, _queue) = active_harness();
    let mut subblock = proc_info_at(0);
    subblock.frames /= 2;

    assert_eq!(
        read_render_context(&extra.store, &subblock),
        Err("render context does not match the player process block")
    );
}

#[kithara::test]
fn invalid_transport_block_replaces_the_previous_render_context() {
    let (mut processor, mut extra, _output, _queue) = active_harness();
    let info = proc_info_at(0);
    assert!(read_render_context(&extra.store, &info).is_ok());

    process_node(&mut processor, &info, &mut extra);

    assert_eq!(
        read_render_context(&extra.store, &info),
        Err("render context is invalid")
    );
}

#[kithara::test]
fn a_tempo_change_waits_for_its_frame_and_moves_anchor_and_grid_stamp_together() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    let before = snapshot(&mut output);
    send_tempo(
        &mut queue,
        60.0,
        When::At(SessionFrame::new(block_frame(2))),
    );

    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let waiting = snapshot(&mut output);
    assert_eq!(waiting.anchor(), before.anchor());
    assert_eq!(waiting.session_grid_stamp(), before.session_grid_stamp());
    assert_eq!(waiting.revision(), TransportRevision::first());
    assert_eq!(std::iter::from_fn(|| queue.receipt()).count(), 0);

    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);
    assert!(matches!(
        outcome(&mut queue),
        Outcome::Applied { at, data }
            if at == SessionFrame::new(block_frame(2)) && data == second_revision()
    ));
    let applied = snapshot(&mut output);
    assert_eq!(applied.revision(), second_revision());
    assert_eq!(applied.tempo(), tempo(60.0));
    assert_eq!(
        applied.session_grid_stamp().grid_id(),
        before.session_grid_stamp().grid_id()
    );
    assert!(applied.session_grid_stamp().revision() > before.session_grid_stamp().revision());
    assert_eq!(applied.session_epoch(), before.session_epoch());

    let session_grid = applied.session_grid();
    assert_eq!(session_grid.stamp(), applied.session_grid_stamp());
    let resolved = session_grid.position_at(MapPoint::new(
        session_grid.stamp(),
        Beat::new(0.04).expect("invariant: transition beat is finite"),
    ));
    let BeatGridQuery::Resolved(position) = resolved else {
        panic!("expected the transition beat to resolve on the published session grid")
    };
    assert_eq!(
        *position.value().value(),
        MapPosition::Session(SessionFrame::new(block_frame(2)))
    );
}

#[kithara::test]
fn a_tempo_change_smooths_from_the_old_tempo_on_its_frame() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let old_grid = snapshot(&mut output).session_grid();
    let old_position = MapPoint::new(
        old_grid.stamp(),
        MapPosition::Session(SessionFrame::new(block_frame(1))),
    );
    let old_tempo = BeatsPerMinute::try_from(120.0)
        .expect("invariant: fixture tempo is a positive finite value");
    send_tempo(&mut queue, 60.0, When::Next);

    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);
    let applied = snapshot(&mut output);
    let expected_beat = 0.05 + 0.005 * (1.0 - (-2.0_f64).exp());
    assert!((f64::from(applied.position()) - expected_beat).abs() <= f64::EPSILON);
    let new_grid = applied.session_grid();
    assert!(new_grid.revision() > old_grid.revision());
    assert!(matches!(
        old_grid.tempo_at(old_position),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == old_tempo
    ));
    assert!(matches!(
        new_grid.tempo_at(old_position),
        BeatGridQuery::Stale { expected, given }
            if expected == new_grid.stamp() && given == old_grid.stamp()
    ));
    assert!(matches!(
        new_grid.tempo_at(MapPoint::new(
            new_grid.stamp(),
            MapPosition::Session(SessionFrame::new(block_frame(2))),
        )),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == old_tempo
    ));
    let transition = SessionBeat::new(0.04).expect("invariant: transition beat is finite");
    assert_eq!(
        applied
            .anchor()
            .frame_at(transition)
            .expect("invariant: transition beat is representable on its observed anchor"),
        SessionFrame::new(block_frame(2))
    );
}

#[kithara::test]
fn a_tempo_change_inside_a_block_retargets_on_its_own_frame() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let before = snapshot(&mut output).anchor();
    let frame = SessionFrame::new(block_frame(2) + 100);
    send_tempo(&mut queue, 60.0, When::At(frame));

    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);
    assert!(matches!(
        outcome(&mut queue),
        Outcome::Applied { at, .. } if at == frame
    ));
    let retargeted = snapshot(&mut output).anchor();
    let beat = before
        .beat_at(frame)
        .expect("invariant: the change frame has a beat");
    assert_eq!(
        retargeted
            .frame_at(beat)
            .expect("invariant: the change beat is representable"),
        frame
    );
    assert!(
        (retargeted.tempo_at(frame) - before.tempo_at(frame)).abs() <= f64::EPSILON,
        "the tempo turns toward the new target from the frame it was asked for"
    );
}

#[kithara::test]
fn a_tempo_change_at_a_rendered_frame_comes_back_late() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    send_tempo(
        &mut queue,
        60.0,
        When::At(SessionFrame::new(block_frame(1))),
    );

    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);

    assert!(matches!(
        outcome(&mut queue),
        Outcome::Rejected(Rejection::Late)
    ));
    let current = snapshot(&mut output);
    assert_eq!(current.revision(), TransportRevision::first());
    assert_eq!(current.tempo(), HostSettings::default().tempo());
    assert!((f64::from(current.position()) - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn the_tempo_already_playing_applies_without_a_new_revision() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    let before = snapshot(&mut output);
    send_tempo(&mut queue, 120.0, When::Next);

    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);

    assert!(matches!(
        outcome(&mut queue),
        Outcome::Applied { data, .. } if data == TransportRevision::first()
    ));
    let current = snapshot(&mut output);
    assert_eq!(current.revision(), TransportRevision::first());
    assert_eq!(current.session_grid_stamp(), before.session_grid_stamp());
    assert_eq!(current.anchor(), before.anchor());
}

#[kithara::test]
fn route_restart_advances_session_epoch_and_grid_revision() {
    let (mut processor, mut extra, mut output, _queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let before = snapshot(&mut output);

    stop_stream(&mut processor, &mut extra);
    assert_eq!(
        read_render_context(&extra.store, &proc_info_at(0)),
        Err("render context is invalid")
    );
    let boundary = observation(&mut output);
    assert_eq!(boundary.snapshot(), None);
    let boundary_generation = boundary.session_grid();
    let boundary_stamp = boundary_generation
        .stamp()
        .expect("the route boundary has a grid revision");
    assert!(boundary_generation.epoch() > before.session_epoch());
    assert!(boundary_stamp.revision() > before.session_grid_stamp().revision());

    process_node(&mut processor, &proc_info_at(0), &mut extra);
    let restarted = snapshot(&mut output);
    assert_eq!(restarted.revision(), before.revision());
    assert_eq!(
        restarted.session_grid_stamp().grid_id(),
        before.session_grid_stamp().grid_id()
    );
    assert_eq!(restarted.session_epoch(), boundary_generation.epoch());
    assert!(restarted.session_grid_stamp().revision() > boundary_stamp.revision());

    let stale = restarted.session_grid().beat_at(MapPoint::new(
        before.session_grid_stamp(),
        MapPosition::Session(SessionFrame::new(0)),
    ));
    assert!(matches!(stale, BeatGridQuery::Stale { .. }));
}

#[kithara::test]
fn reserved_route_restart_promotes_a_change_rendered_before_stop() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    let mut reserved = observation(&mut output).session_grid();
    reserved
        .advance_restart()
        .expect("invariant: fixture route generation can advance");
    send_tempo(
        &mut queue,
        60.0,
        When::At(SessionFrame::new(block_frame(2))),
    );
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);

    stop_stream(&mut processor, &mut extra);
    let stopped = observation(&mut output).session_grid();
    assert_eq!(stopped.epoch(), reserved.epoch());
    assert!(
        stopped
            .stamp()
            .expect("the stopped generation has a revision")
            .revision()
            > reserved
                .stamp()
                .expect("the reserved generation has a revision")
                .revision()
    );

    let settings = HostSettings::builder()
        .tempo(Tempo::new(60.0).expect("fixture tempo"))
        .build();
    let converged = converge_transport_restart(&mut extra.store, settings, reserved)
        .expect("the reserved restart accepts a newer revision in its target epoch");
    assert_eq!(converged, stopped);
    assert_eq!(observation(&mut output).session_grid(), stopped);
}

#[kithara::test]
fn route_reset_reanchors_the_preserved_beat_at_the_applied_tempo() {
    let (mut processor, mut extra, mut output, _queue) = retargeted_harness();
    let applied = snapshot(&mut output);

    stop_stream(&mut processor, &mut extra);
    process_node(&mut processor, &proc_info_at(0), &mut extra);

    let restarted = snapshot(&mut output);
    assert_eq!(restarted.tempo(), tempo(60.0));
    assert_eq!(restarted.revision(), applied.revision());
    assert_eq!(
        restarted
            .anchor()
            .frame_at(applied.position())
            .expect("invariant: preserved beat is representable on the new axis"),
        SessionFrame::new(0)
    );
}

#[kithara::test]
fn route_reset_withdraws_snapshot_until_new_axis_is_reanchored() {
    let (mut processor, mut extra, mut output, _queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let before_restart = snapshot(&mut output);
    let preserved = before_restart.position();
    assert!((f64::from(preserved) - 0.04).abs() <= f64::EPSILON);

    stop_stream(&mut processor, &mut extra);
    assert_eq!(observation(&mut output).snapshot(), None);

    process_node(&mut processor, &proc_info_at(0), &mut extra);
    let restarted = snapshot(&mut output);
    assert_eq!(
        restarted
            .anchor()
            .frame_at(preserved)
            .expect("invariant: preserved beat is representable on the new axis"),
        SessionFrame::new(0)
    );
    assert!((f64::from(restarted.position()) - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn repeated_route_reset_preserves_the_beat_until_the_new_axis_renders() {
    let (mut processor, mut extra, mut output, _queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let preserved = snapshot(&mut output).position();

    for _ in 0..2 {
        stop_stream(&mut processor, &mut extra);
        assert_eq!(observation(&mut output).snapshot(), None);
    }

    process_node(&mut processor, &proc_info_at(0), &mut extra);
    let restarted = snapshot(&mut output);
    assert_eq!(
        restarted
            .anchor()
            .frame_at(preserved)
            .expect("invariant: preserved beat is representable on the new axis"),
        SessionFrame::new(0)
    );
}

#[kithara::test]
fn a_discontinuous_block_is_rejected_and_still_publishes() {
    let (_processor, mut extra, mut output, _queue) = active_harness();
    let before = snapshot(&mut output);

    assert_eq!(
        process_transport(&proc_info_at(481), &mut extra.store).map(drop),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    assert_eq!(observation(&mut output).snapshot(), Some(before));
}

#[kithara::test]
fn a_tempo_change_due_in_a_discontinuous_block_is_refused() {
    let (_processor, mut extra, mut output, mut queue) = active_harness();
    let before = snapshot(&mut output);
    send_tempo(&mut queue, 60.0, When::Next);

    assert_eq!(
        process_transport(&proc_info_at(481), &mut extra.store).map(drop),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    assert!(matches!(
        outcome(&mut queue),
        Outcome::Rejected(Rejection::Refused(
            TransportProcessError::FrameDiscontinuity
        ))
    ));
    assert_eq!(observation(&mut output).snapshot(), Some(before));
}

#[kithara::test]
fn a_restart_refuses_a_change_waiting_on_the_old_axis_and_keeps_the_next_one() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    send_tempo(
        &mut queue,
        60.0,
        When::At(SessionFrame::new(block_frame(4))),
    );
    send_tempo(&mut queue, 90.0, When::Next);

    stop_stream(&mut processor, &mut extra);

    assert!(matches!(
        outcome(&mut queue),
        Outcome::Rejected(Rejection::Refused(
            TransportProcessError::SessionAxisRestarted
        ))
    ));
    for block in 1..=5 {
        process_node(
            &mut processor,
            &proc_info_at(block_frame(block)),
            &mut extra,
        );
    }
    assert!(matches!(outcome(&mut queue), Outcome::Applied { .. }));
    assert!(queue.receipt().is_none());
    assert_eq!(snapshot(&mut output).tempo(), tempo(90.0));
}

#[kithara::test]
fn a_tempo_change_refused_for_the_next_block_is_sent_again() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    configure_tempo(&mut state, 60.0);
    let refused: Vec<_> = state.settings.pending().map(|(seq, ..)| seq).collect();

    assert_eq!(
        render(&mut state, 481),
        Err(TransportProcessError::FrameDiscontinuity)
    );
    assert_eq!(
        state.settings.config().tempo(),
        HostSettings::default().tempo()
    );
    let pending: Vec<_> = state.settings.pending().collect();
    assert!(
        matches!(
            pending.as_slice(),
            [(seq, When::Next, HostSettingsChange::Tempo(sent))]
                if !refused.contains(seq) && *sent == tempo(60.0)
        ),
        "the refused change goes out again as a new batch: {pending:?}"
    );

    assert_eq!(render(&mut state, block_frame(1)), Ok(()));
    assert_eq!(state.settings.config().tempo(), tempo(60.0));
}

#[kithara::test]
fn a_newer_tempo_change_for_the_next_block_decides_after_a_discontinuity() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    configure_tempo(&mut state, 60.0);
    configure_tempo(&mut state, 90.0);

    assert_eq!(
        render(&mut state, 481),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    let pending: Vec<_> = state.settings.pending().collect();
    assert!(
        matches!(
            pending.as_slice(),
            [(_, When::Next, HostSettingsChange::Tempo(sent))] if *sent == tempo(90.0)
        ),
        "only the newest refused change goes out again: {pending:?}"
    );
    assert_eq!(render(&mut state, block_frame(1)), Ok(()));
    assert_eq!(state.settings.config().tempo(), tempo(90.0));
}

#[kithara::test]
fn a_block_at_another_sample_rate_is_discontinuous() {
    let (_processor, mut extra, _output, _queue) = active_harness();
    let mut foreign_block = proc_info_at(block_frame(1));
    foreign_block.sample_rate = NonZeroU32::new(consts::TRANSPORT_SAMPLE_RATE * 2)
        .expect("invariant: doubled sample rate is non-zero");

    assert_eq!(
        process_transport(&foreign_block, &mut extra.store).map(drop),
        Err(TransportProcessError::FrameDiscontinuity)
    );
}

#[kithara::test]
fn transport_event_is_owned_by_kithara_host() {
    assert_eq!(
        ::core::any::type_name::<crate::TransportEvent>(),
        "kithara_host::session::transport::event::TransportEvent"
    );
}
