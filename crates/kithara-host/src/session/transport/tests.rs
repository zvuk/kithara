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
    Batch, Outcome, Port, Rejection, ScopedReceipt, ScopedSender, Seq, When, scoped_channel,
};
use kithara_config::{Config, ConfigOwner};
use kithara_platform::time::Duration;
use kithara_play::PlayError;
use kithara_render::{
    bridge::DeckProtocol,
    rt::{install_render_context, read_render_context},
};
use kithara_signal::{FrameCount, SessionEpoch, SessionFrame};
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
    HostSettled, MetronomeConfigChange, PassClock,
    api::{SessionBeat, SessionDuckingMode, SessionTransportSnapshot, Tempo, TransportRevision},
    consts,
    host::{HostSettings, HostSettingsChange, HostSettingsExec},
    session::{
        queue::{HostPart, HostProtocol, settle_root_receipts},
        state::{SessionState, SessionStream, ensure_ctx},
        tests::graph,
    },
};

type Harness = (
    SessionTransportProcessor,
    ProcExtra,
    Output<TransportObservation>,
    ScopedSender<HostProtocol, DeckProtocol>,
);

type Answer = (Seq, Result<SessionFrame, Rejection<PlayError>>);

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

pub(crate) fn ducking_store(changes: &[(usize, SessionDuckingMode)], frames: usize) -> ProcStore {
    let (mut extra, _observation, mut queue) = proc_extra();
    for &(frame, mode) in changes {
        queue
            .send(
                When::At(SessionFrame::new(i64::try_from(frame).expect("frame"))),
                Batch {
                    basis: Vec::new(),
                    commands: vec![HostPart::Settings(HostSettingsChange::Ducking(mode))],
                },
            )
            .expect("ducking command");
    }
    queue.publish().expect("publish ducking commands");
    let mut info = proc_info_at(0);
    info.frames = frames;
    process_transport(&info, &mut extra.store).expect("apply ducking spans");
    extra.store
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
    let mut state = graph::state_for(sample_rate, |_ctx, _sample_rate| {
        Ok(SessionStream::Realtime { _backend: () })
    });
    assert!(ensure_ctx(&mut state).is_ok());
    state
}

fn proc_store(state: &mut SessionState<(), TestPools>) -> &mut ProcStore {
    state
        .ctx
        .as_mut()
        .and_then(FirewheelContext::proc_store_mut)
        .expect("invariant: a context never started keeps its store")
}

fn publish(state: &mut SessionState<(), TestPools>) {
    if let Some(channel) = state.channel.as_mut() {
        channel.publish().expect("an open host channel publishes");
    }
}

/// Renders one transport block at `clock_samples` with the batches the owner
/// already published, leaving their receipts for the owner.
fn process(
    state: &mut SessionState<(), TestPools>,
    clock_samples: i64,
) -> Result<(), TransportProcessError> {
    process_transport(&proc_info_at(clock_samples), proc_store(state)).map(drop)
}

/// Publishes the owner's batches, renders one transport block at
/// `clock_samples`, then settles its receipts as the session owner does
/// between blocks.
fn render(
    state: &mut SessionState<(), TestPools>,
    clock_samples: i64,
) -> Result<(), TransportProcessError> {
    publish(state);
    let result = process(state, clock_samples);
    settle_root_receipts(state);
    result
}

/// Starts an owner pass at `now` with one block of delivery. The fixture
/// context never starts, so the pass gets the clock a running stream gives.
fn begin_pass_at(state: &mut SessionState<(), TestPools>, now: i64) {
    state.begin_iteration();
    let delivery = FrameCount::new(consts::TRANSPORT_BLOCK_FRAMES);
    state.iteration_clock = Some(PassClock {
        now: SessionFrame::new(now),
        delivery,
        deck_delivery: delivery,
    });
}

/// Ends the stream's frame axis as a route restart does: the owner drops its
/// pass clock, and the transport ends the axis when its stream stops.
fn restart_axis(state: &mut SessionState<(), TestPools>) {
    state.iteration_clock = None;
    let (mut logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    SessionTransportProcessor.stream_stopped(&mut ProcStreamCtx {
        store: proc_store(state),
        logger: &mut logger,
    });
}

fn configure(
    state: &mut SessionState<(), TestPools>,
    change: HostSettingsChange,
    when: When<SessionFrame>,
) -> Result<Option<Seq>, PlayError> {
    state.exec(change, when, &mut ())
}

/// The Seq the Host answers an accepted tempo with.
fn accepted(
    state: &mut SessionState<(), TestPools>,
    beats_per_minute: f64,
    when: When<SessionFrame>,
) -> Seq {
    configure(
        state,
        HostSettingsChange::Tempo(tempo(beats_per_minute)),
        when,
    )
    .expect("the Host accepts the tempo")
    .expect("a render graph carries the tempo")
}

fn at(frame: i64) -> When<SessionFrame> {
    When::At(SessionFrame::new(frame))
}

/// The frames of the accepted tempo steps after the map's anchor.
fn steps(state: &SessionState<(), TestPools>) -> Vec<SessionFrame> {
    let planned = state.timeline.trajectory();
    let base = planned.anchor_at(SessionFrame::new(i64::MIN)).frame();
    planned.changes_after(base).collect()
}

/// The tempo the accepted map heads for from `frame`, in beats per second.
fn target(state: &SessionState<(), TestPools>, frame: i64) -> f64 {
    state
        .timeline
        .trajectory()
        .anchor_at(SessionFrame::new(frame))
        .target_beats_per_second()
}

/// The answer the caller of `seq` got among `answered`.
fn answer_of(answered: &[Answer], seq: Seq) -> Option<&Result<SessionFrame, Rejection<PlayError>>> {
    answered
        .iter()
        .find(|(answered, _)| *answered == seq)
        .map(|(_, outcome)| outcome)
}

/// The answers the Host gave its callers since the last call.
fn answers(state: &mut SessionState<(), TestPools>) -> Vec<Answer> {
    std::mem::take(&mut state.settled)
        .into_iter()
        .map(|settled| {
            let HostSettled::Settings { seq, outcome, .. } = settled else {
                panic!("the Host settles only settings changes here");
            };
            (seq, outcome)
        })
        .collect()
}

fn transport(state: &mut SessionState<(), TestPools>) -> SessionTransportSnapshot {
    state
        .transport_observation
        .as_mut()
        .and_then(|observation| observation.read().snapshot())
        .expect("invariant: the rendered transport publishes a snapshot")
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
fn successive_tempo_changes_preserve_the_reached_tempo_and_beat() {
    let (mut processor, mut extra, mut output, mut queue) = active_harness();
    process_node(&mut processor, &proc_info_at(block_frame(1)), &mut extra);
    let first = SessionFrame::new(block_frame(2) + 100);
    let second = SessionFrame::new(i64::from(first) + 48);
    let observed = SessionFrame::new(i64::from(second) + 96);
    send_tempo(&mut queue, 60.0, When::At(first));
    send_tempo(&mut queue, 90.0, When::At(second));

    process_node(&mut processor, &proc_info_at(block_frame(2)), &mut extra);
    for frame in [first, second] {
        assert!(matches!(
            outcome(&mut queue),
            Outcome::Applied { at, .. } if at == frame
        ));
    }
    let applied = snapshot(&mut output);
    let anchor = applied.anchor();
    let rate = f64::from(sample_rate().get());
    let elapsed = 48.0 / rate;
    let after = 96.0 / rate;
    let reached = 1.0 + (-elapsed / 0.005).exp();
    let beat_at_second =
        f64::from(i32::try_from(i64::from(first)).expect("invariant: fixture frame fits i32"))
            * (2.0 / rate)
            + elapsed
            + 0.005 * -(-elapsed / 0.005).exp_m1();
    let beat_at_observed =
        beat_at_second + 1.5 * after + (reached - 1.5) * 0.005 * -(-after / 0.005).exp_m1();
    assert_eq!(anchor.frame(), second);
    assert_eq!(applied.tempo(), tempo(90.0));
    assert_eq!(anchor.tempo_at(second), reached);
    for (frame, expected) in [(second, beat_at_second), (observed, beat_at_observed)] {
        let beat = SessionBeat::new(expected).expect("invariant: fixture beat is finite");
        assert_eq!(
            anchor
                .frame_at(beat)
                .expect("invariant: fixture beat is representable"),
            frame
        );
        assert!(
            (f64::from(
                anchor
                    .beat_at(frame)
                    .expect("invariant: fixture frame is finite")
            ) - expected)
                .abs()
                <= f64::EPSILON
        );
    }
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
fn a_tempo_change_refused_for_the_next_block_is_placed_again_as_the_same_change() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    let seq = accepted(&mut state, 60.0, When::Next);

    assert_eq!(
        render(&mut state, 481),
        Err(TransportProcessError::FrameDiscontinuity)
    );
    assert_eq!(
        state.settings.config().tempo(),
        HostSettings::default().tempo()
    );
    assert!(answers(&mut state).is_empty());
    let pending: Vec<_> = state.settings.pending().collect();
    assert!(
        matches!(
            pending.as_slice(),
            [(batch, When::Next, HostSettingsChange::Tempo(sent))]
                if *batch != seq && *sent == tempo(60.0)
        ),
        "the refused change goes out again in a new batch: {pending:?}"
    );

    assert_eq!(render(&mut state, block_frame(1)), Ok(()));
    assert_eq!(state.settings.config().tempo(), tempo(60.0));
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(_))] if *answered == seq
    ));
}

#[kithara::test]
fn a_newer_tempo_change_for_the_next_block_decides_after_a_discontinuity() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    let older = accepted(&mut state, 60.0, When::Next);
    let newer = accepted(&mut state, 90.0, When::Next);

    assert_eq!(
        render(&mut state, 481),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Refused(PlayError::Superseded)))] if *answered == older
    ));
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
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(_))] if *answered == newer
    ));
}

#[kithara::test]
fn an_accepted_future_tempo_is_planned_before_the_transport_applies_it() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let step = block_frame(3);

    let seq = accepted(&mut state, 60.0, at(step));

    assert_eq!(steps(&state), [SessionFrame::new(step)]);
    assert_eq!(target(&state, step), tempo(60.0).beats_per_second());
    for block in 1..3 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
        assert_eq!(
            state.settings.config().tempo(),
            HostSettings::default().tempo()
        );
        assert_eq!(
            transport(&mut state).tempo(),
            HostSettings::default().tempo()
        );
        assert!(answers(&mut state).is_empty());
    }
    assert_eq!(render(&mut state, step), Ok(()));
    assert_eq!(state.settings.config().tempo(), tempo(60.0));
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == seq && *frame == SessionFrame::new(step)
    ));
}

#[kithara::test]
fn a_refused_tempo_changes_neither_the_accepted_map_nor_the_settings() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));

    assert!(matches!(
        configure(
            &mut state,
            HostSettingsChange::Tempo(tempo(60.0)),
            at(block_frame(1))
        ),
        Err(PlayError::Late)
    ));
    assert!(steps(&state).is_empty());
    assert_eq!(state.settings.pending().count(), 0);

    let room = i64::try_from(state.channel.as_ref().map_or(0, Port::available))
        .expect("invariant: the fixture queue room fits i64");
    let first = block_frame(2);
    for offset in 0..room {
        accepted(&mut state, 60.0, at(first + offset));
    }
    assert!(matches!(
        configure(
            &mut state,
            HostSettingsChange::Tempo(tempo(90.0)),
            at(first + room)
        ),
        Err(PlayError::Session(
            kithara_play::SessionError::HostQueueFull
        ))
    ));
    assert_eq!(
        steps(&state),
        (0..room)
            .map(|offset| SessionFrame::new(first + offset))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        state.settings.config().tempo(),
        HostSettings::default().tempo()
    );
}

/// A request for the next block, or for its own frame.
#[derive(Clone, Copy)]
enum Ask {
    Next,
    At(i64),
}

impl Ask {
    fn when(self) -> When<SessionFrame> {
        match self {
            Self::Next => When::Next,
            Self::At(frame) => at(frame),
        }
    }
}

#[kithara::test]
#[case::asap_takes_the_place_of_asap(Ask::Next, 1, Ask::Next, true)]
#[case::the_same_frame_takes_the_place_of_asap(Ask::Next, 1, Ask::At(block_frame(2)), true)]
#[case::asap_takes_the_place_of_its_own_frame(Ask::At(block_frame(2)), 1, Ask::Next, true)]
#[case::the_same_frame_takes_the_place_of_its_frame(
    Ask::At(block_frame(3)),
    1,
    Ask::At(block_frame(3)),
    true
)]
#[case::a_step_on_another_frame_is_kept(Ask::Next, 1, Ask::At(block_frame(2) + 128), false)]
#[case::asap_keeps_an_exact_step_on_another_frame(Ask::At(block_frame(3)), 1, Ask::Next, false)]
#[case::asap_keeps_a_step_inside_delivery(Ask::Next, 2, Ask::Next, false)]
fn a_newer_tempo_replaces_what_it_can(
    #[case] older: Ask,
    #[case] newer_pass: usize,
    #[case] newer: Ask,
    #[case] replaces: bool,
) {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let first = accepted(&mut state, 180.0, older.when());
    let [older_step] = steps(&state)[..] else {
        panic!("the older tempo takes one step");
    };
    for block in 1..newer_pass {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    begin_pass_at(&mut state, block_frame(newer_pass));

    let second = accepted(&mut state, 90.0, newer.when());

    let planned = steps(&state);
    assert_eq!(planned.len(), if replaces { 1 } else { 2 });
    let newer_step = planned
        .iter()
        .copied()
        .find(|frame| replaces || *frame != older_step)
        .expect("the newer tempo takes a step");
    let older_target = if replaces { 1.5 } else { 3.0 };
    assert_eq!(target(&state, i64::from(older_step)), older_target);
    assert_eq!(target(&state, i64::from(newer_step)), 1.5);

    for block in newer_pass..=4 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    let answered = answers(&mut state);
    let answer = |seq| {
        answered
            .iter()
            .find(|(answered, _)| *answered == seq)
            .map(|(_, outcome)| outcome)
    };
    assert_eq!(answered.len(), 2);
    assert!(match answer(first) {
        Some(Ok(frame)) => !replaces && *frame == older_step,
        Some(Err(Rejection::Refused(PlayError::Superseded))) => replaces,
        _ => false,
    });
    assert!(matches!(answer(second), Some(Ok(frame)) if *frame == newer_step));
    let last = planned.last().copied().expect("a step stands");
    assert_eq!(
        transport(&mut state).anchor(),
        state.timeline.trajectory().anchor_at(last),
        "the transport executes the accepted map"
    );
}

#[kithara::test]
fn an_applied_tempo_stays_the_base_of_the_next_accepted_one() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let first = block_frame(2);
    accepted(&mut state, 60.0, at(first));
    for block in 1..=2 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(answers(&mut state).as_slice(), [(_, Ok(_))]));

    begin_pass_at(&mut state, block_frame(3));
    assert_eq!(steps(&state), [SessionFrame::new(first)]);
    assert_eq!(target(&state, first), tempo(60.0).beats_per_second());
    let second = block_frame(4);
    accepted(&mut state, 90.0, at(second));
    for block in 3..=4 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }

    assert_eq!(
        steps(&state),
        [SessionFrame::new(first), SessionFrame::new(second)]
    );
    assert_eq!(
        transport(&mut state).anchor(),
        state
            .timeline
            .trajectory()
            .anchor_at(SessionFrame::new(second)),
        "the next tempo turns from the applied one as the transport does"
    );
}

#[kithara::test]
fn the_accepted_map_starts_at_beat_zero_from_the_host_settings() {
    let settings = HostSettings::builder()
        .tempo(tempo(96.0))
        .sample_rate(NonZeroU32::new(44_100).expect("invariant: fixture rate is non-zero"))
        .build();
    let origin = SessionFrame::new(0);

    let seed = settings.trajectory();

    assert_eq!(
        seed.beat_at(origin),
        SessionBeat::new(0.0).expect("invariant: beat zero is finite")
    );
    assert_eq!(seed.tempo_at(origin), settings.tempo());
    assert_eq!(seed.sample_rate(), settings.sample_rate());
    let bar = SessionBeat::new(f64::from(crate::api::SESSION_BEATS_PER_BAR.get()))
        .expect("invariant: the first bar line is finite");
    assert_eq!(seed.downbeats(origin).nth(1), Some(seed.frame_at(bar)));
    let state = owned_session();
    assert_eq!(
        state.timeline.trajectory().anchor_at(origin),
        state.settings.config().trajectory().anchor_at(origin)
    );
    assert!(steps(&state).is_empty());
}

#[kithara::test]
fn without_a_clock_a_tempo_moves_the_anchor_the_next_step_builds_on() {
    let mut state = owned_session();
    let origin = SessionFrame::new(0);

    accepted(&mut state, 60.0, When::Next);

    assert_eq!(state.timeline.trajectory().tempo_at(origin), tempo(60.0));
    assert!(steps(&state).is_empty());
    begin_pass_at(&mut state, 0);
    let step = block_frame(2);
    accepted(&mut state, 90.0, at(step));
    let beat = |frames: i64| {
        SessionBeat::new(
            f64::from(i32::try_from(frames).expect("invariant: fixture frame fits i32"))
                / f64::from(sample_rate().get()),
        )
        .expect("invariant: fixture beat is finite")
    };
    assert_eq!(
        state.timeline.trajectory().beat_at(SessionFrame::new(step)),
        beat(step),
        "the step turns from the anchor at 60 BPM"
    );
}

#[kithara::test]
fn a_restart_starts_the_accepted_map_from_the_confirmed_anchor() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let applied = block_frame(2);
    accepted(&mut state, 180.0, at(applied));
    for block in 1..=2 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(answers(&mut state).as_slice(), [(_, Ok(_))]));
    begin_pass_at(&mut state, block_frame(3));
    let asap = accepted(&mut state, 60.0, When::Next);
    let exact = accepted(&mut state, 90.0, at(block_frame(5)));
    assert_eq!(steps(&state).len(), 3);
    publish(&mut state);

    restart_axis(&mut state);
    settle_root_receipts(&mut state);

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Late))] if *answered == exact
    ));
    assert_eq!(
        steps(&state),
        [SessionFrame::new(applied)],
        "the applied step stands until the new axis is confirmed"
    );
    assert_eq!(render(&mut state, 0), Ok(()));
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(_))] if *answered == asap
    ));
    begin_pass_at(&mut state, block_frame(1));
    let confirmed = transport(&mut state);
    assert!(confirmed.session_epoch() > SessionEpoch::new(0));
    assert_eq!(
        state.timeline.trajectory().anchor_at(SessionFrame::new(0)),
        confirmed.anchor()
    );
    assert!(
        steps(&state).is_empty(),
        "no step of the old axis stands on the new one"
    );
}

#[kithara::test]
fn a_tempo_accepted_before_the_new_axis_renders_takes_its_step_on_it() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    restart_axis(&mut state);
    begin_pass_at(&mut state, 0);
    let step = block_frame(2);

    accepted(&mut state, 60.0, at(step));

    assert!(
        steps(&state).is_empty(),
        "the map still stands on the old axis"
    );
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    assert_eq!(steps(&state), [SessionFrame::new(step)]);
    for block in 1..=2 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert_eq!(
        transport(&mut state).anchor(),
        state
            .timeline
            .trajectory()
            .anchor_at(SessionFrame::new(step))
    );
}

#[kithara::test]
fn a_discontinuity_places_an_asap_tempo_again_and_ends_an_exact_one_late() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let asap = accepted(&mut state, 60.0, When::Next);
    let exact = accepted(&mut state, 90.0, at(block_frame(2) + 40));
    let [asap_step, _] = steps(&state)[..] else {
        panic!("both tempos take a step");
    };
    publish(&mut state);

    let discontinuous = block_frame(1) + 100;
    assert_eq!(
        process(&mut state, discontinuous),
        Err(TransportProcessError::FrameDiscontinuity)
    );
    // The stream's clock went past the block the transport refused.
    let now = discontinuous + block_frame(1);
    begin_pass_at(&mut state, now);
    settle_root_receipts(&mut state);

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Late))] if *answered == exact
    ));
    let moved_step = SessionFrame::new(now + block_frame(1));
    assert_eq!(steps(&state), [moved_step]);
    assert!(moved_step > asap_step);
    for block in 1..=3 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == asap && *frame == moved_step
    ));
    assert_eq!(state.settings.config().tempo(), tempo(60.0));
}

#[kithara::test]
fn a_delayed_owner_pass_places_an_asap_tempo_again_past_a_newer_exact_one_late_with_it() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let asap = accepted(&mut state, 60.0, When::Next);
    let exact = accepted(&mut state, 90.0, at(block_frame(2) + 40));
    for block in 1..=2 {
        assert_eq!(process(&mut state, block_frame(block)), Ok(()));
    }
    publish(&mut state);
    assert_eq!(process(&mut state, block_frame(3)), Ok(()));

    begin_pass_at(&mut state, block_frame(4));
    settle_root_receipts(&mut state);

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Late))] if *answered == exact
    ));
    let step = SessionFrame::new(block_frame(5));
    assert_eq!(steps(&state), [step]);
    for block in 4..=5 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == asap && *frame == step
    ));
    assert_eq!(state.settings.config().tempo(), tempo(60.0));
}

#[kithara::test]
fn a_restart_places_an_asap_metronome_change_again_on_the_new_axis() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let enable = HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true));
    let seq = configure(&mut state, enable, When::Next)
        .expect("the Host accepts the metronome change")
        .expect("a render graph carries it");
    assert!(matches!(
        state.settings.pending().collect::<Vec<_>>().as_slice(),
        [(_, When::At(_), _)]
    ));
    publish(&mut state);

    restart_axis(&mut state);
    settle_root_receipts(&mut state);

    assert!(answers(&mut state).is_empty());
    assert!(matches!(
        state.settings.pending().collect::<Vec<_>>().as_slice(),
        [(batch, When::Next, HostSettingsChange::Metronome(_))] if *batch != seq
    ));
    assert_eq!(render(&mut state, 0), Ok(()));
    assert!(state.settings.config().metronome().enabled());
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(_))] if *answered == seq
    ));
}

#[kithara::test]
fn a_delayed_owner_pass_places_a_late_asap_tempo_again() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let seq = accepted(&mut state, 60.0, When::Next);
    for block in 1..=2 {
        assert_eq!(process(&mut state, block_frame(block)), Ok(()));
    }
    publish(&mut state);
    assert_eq!(process(&mut state, block_frame(3)), Ok(()));

    begin_pass_at(&mut state, block_frame(4));
    settle_root_receipts(&mut state);

    assert!(answers(&mut state).is_empty());
    let step = block_frame(5);
    assert_eq!(steps(&state), [SessionFrame::new(step)]);
    for block in 4..=5 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == seq && *frame == SessionFrame::new(step)
    ));
}

#[kithara::test]
fn a_superseded_tempo_the_transport_played_keeps_its_step_when_its_replacement_is_late() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let frame = block_frame(2) + 40;
    let played = accepted(&mut state, 130.0, at(frame));
    publish(&mut state);
    let late = accepted(&mut state, 125.0, at(frame));
    for block in 1..=2 {
        assert_eq!(process(&mut state, block_frame(block)), Ok(()));
    }
    publish(&mut state);
    assert_eq!(process(&mut state, block_frame(3)), Ok(()));

    begin_pass_at(&mut state, block_frame(4));
    settle_root_receipts(&mut state);

    let answered = answers(&mut state);
    assert!(matches!(
        answered.as_slice(),
        [
            (first, Err(Rejection::Refused(PlayError::Superseded))),
            (second, Err(Rejection::Late)),
        ] if *first == played && *second == late
    ));
    assert_eq!(steps(&state), [SessionFrame::new(frame)]);
    assert_eq!(target(&state, frame), tempo(130.0).beats_per_second());
    assert_eq!(
        transport(&mut state).anchor(),
        state
            .timeline
            .trajectory()
            .anchor_at(SessionFrame::new(frame)),
        "the accepted map keeps the step the transport played"
    );
}

#[kithara::test]
fn a_late_asap_tempo_leaves_the_setting_to_a_newer_one_on_an_earlier_frame() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let older = accepted(&mut state, 60.0, When::Next);
    let frame = block_frame(3) + 40;
    let newer = accepted(&mut state, 90.0, at(frame));
    for block in 1..=2 {
        assert_eq!(process(&mut state, block_frame(block)), Ok(()));
    }
    publish(&mut state);
    assert_eq!(process(&mut state, block_frame(3)), Ok(()));

    begin_pass_at(&mut state, block_frame(4));
    settle_root_receipts(&mut state);
    for block in 4..=5 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }

    let answered = answers(&mut state);
    assert_eq!(answered.len(), 2);
    assert!(matches!(
        answer_of(&answered, older),
        Some(Err(Rejection::Refused(PlayError::Superseded)))
    ));
    assert!(matches!(
        answer_of(&answered, newer),
        Some(Ok(applied)) if *applied == SessionFrame::new(frame)
    ));
    assert_eq!(steps(&state), [SessionFrame::new(frame)]);
    assert_eq!(state.settings.config().tempo(), tempo(90.0));
}

#[kithara::test]
#[case::the_same_setting(MetronomeConfigChange::Level(0.5), true)]
#[case::another_setting(MetronomeConfigChange::Enabled(true), false)]
fn a_late_asap_metronome_change_leaves_its_setting_to_a_newer_one(
    #[case] newer: MetronomeConfigChange,
    #[case] decides: bool,
) {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let older = configure(
        &mut state,
        HostSettingsChange::Metronome(MetronomeConfigChange::Level(0.25)),
        When::Next,
    )
    .expect("the Host accepts the older change")
    .expect("a render graph carries it");
    for block in 1..=2 {
        assert_eq!(process(&mut state, block_frame(block)), Ok(()));
    }
    begin_pass_at(&mut state, block_frame(3));
    let newer_seq = configure(&mut state, HostSettingsChange::Metronome(newer), When::Next)
        .expect("the Host accepts the newer change")
        .expect("a render graph carries it");
    publish(&mut state);
    assert_eq!(process(&mut state, block_frame(3)), Ok(()));

    begin_pass_at(&mut state, block_frame(4));
    settle_root_receipts(&mut state);
    for block in 4..=5 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }

    let answered = answers(&mut state);
    let answer = |seq| answer_of(&answered, seq);
    assert_eq!(answered.len(), 2);
    assert!(matches!(answer(newer_seq), Some(Ok(_))));
    let metronome = state.settings.config().metronome();
    if decides {
        assert!(matches!(
            answer(older),
            Some(Err(Rejection::Refused(PlayError::Superseded)))
        ));
        assert_eq!(metronome.level(), 0.5, "the newer level decides");
    } else {
        assert!(matches!(answer(older), Some(Ok(_))));
        assert_eq!(metronome.level(), 0.25, "the older level still lands");
        assert!(metronome.enabled(), "the newer change sets its own field");
    }
}

#[kithara::test]
#[case::on_the_played_step(0)]
#[case::before_the_played_step(-40)]
fn a_tempo_at_or_before_a_played_step_is_late(#[case] offset: i64) {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let played = block_frame(2) + 80;
    accepted(&mut state, 60.0, at(played));
    for block in 1..=2 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(answers(&mut state).as_slice(), [(_, Ok(_))]));

    begin_pass_at(&mut state, block_frame(1));
    assert!(matches!(
        configure(
            &mut state,
            HostSettingsChange::Tempo(tempo(90.0)),
            at(played + offset)
        ),
        Err(PlayError::Late)
    ));
    assert_eq!(steps(&state), [SessionFrame::new(played)]);
    assert_eq!(target(&state, played), tempo(60.0).beats_per_second());
}

#[kithara::test]
fn a_refused_asap_tempo_yields_to_a_newer_accepted_asap_tempo() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let older = accepted(&mut state, 180.0, When::Next);
    assert_eq!(render(&mut state, block_frame(1)), Ok(()));
    begin_pass_at(&mut state, block_frame(2));
    let newer = accepted(&mut state, 90.0, When::Next);
    let newer_step = SessionFrame::new(block_frame(3));
    publish(&mut state);
    assert_eq!(
        process(&mut state, block_frame(2) - 10),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    begin_pass_at(&mut state, block_frame(3));
    settle_root_receipts(&mut state);

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Refused(PlayError::Superseded)))] if *answered == older
    ));
    assert_eq!(state.settings.pending().count(), 1);
    assert_eq!(steps(&state), [newer_step]);
    for block in 2..=4 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == newer && *frame == newer_step
    ));
    assert_eq!(state.settings.config().tempo(), tempo(90.0));
}

#[kithara::test]
fn a_stale_receipt_after_a_restart_leaves_the_new_axis_map() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let step = block_frame(3);
    let stale = accepted(&mut state, 60.0, at(step));
    publish(&mut state);
    restart_axis(&mut state);
    assert_eq!(process(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    assert!(transport(&mut state).session_epoch() > SessionEpoch::new(0));
    let fresh = accepted(&mut state, 90.0, at(step));
    assert_eq!(steps(&state), [SessionFrame::new(step)]);

    settle_root_receipts(&mut state);

    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Err(Rejection::Late))] if *answered == stale
    ));
    assert_eq!(steps(&state), [SessionFrame::new(step)]);
    assert_eq!(target(&state, step), tempo(90.0).beats_per_second());
    for block in 1..=3 {
        assert_eq!(render(&mut state, block_frame(block)), Ok(()));
    }
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(frame))] if *answered == fresh && *frame == SessionFrame::new(step)
    ));
}

#[kithara::test]
fn a_tempo_for_the_next_block_holds_the_old_axis_until_it_is_answered() {
    let mut state = owned_session();
    assert_eq!(render(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    let asap = accepted(&mut state, 60.0, When::Next);
    publish(&mut state);
    restart_axis(&mut state);
    settle_root_receipts(&mut state);
    assert!(answers(&mut state).is_empty());

    assert_eq!(process(&mut state, 0), Ok(()));
    begin_pass_at(&mut state, block_frame(1));
    assert_eq!(render(&mut state, block_frame(1)), Ok(()));
    assert!(matches!(
        answers(&mut state).as_slice(),
        [(answered, Ok(_))] if *answered == asap
    ));
    begin_pass_at(&mut state, block_frame(2));

    let confirmed = transport(&mut state);
    assert!(confirmed.session_epoch() > SessionEpoch::new(0));
    assert_eq!(
        state
            .timeline
            .trajectory()
            .anchor_at(SessionFrame::new(block_frame(2))),
        confirmed.anchor()
    );
    assert_eq!(
        target(&state, block_frame(3)),
        tempo(60.0).beats_per_second(),
        "the new axis carries the tempo the transport applied on it"
    );
}

#[kithara::test]
fn an_idle_teardown_answers_every_accepted_change_and_takes_its_steps_off_the_map() {
    let mut state = owned_session();
    begin_pass_at(&mut state, block_frame(1));
    let tempo_seq = accepted(&mut state, 60.0, When::Next);
    let metronome_seq = configure(
        &mut state,
        HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
        When::Next,
    )
    .expect("the Host accepts the metronome change")
    .expect("a render graph carries it");
    let step = SessionFrame::new(block_frame(2));
    assert_eq!(steps(&state), [step]);

    state.stream = None;
    assert!(crate::session::graph::drop_idle_context(&mut state).is_ok());

    let answered = answers(&mut state);
    assert_eq!(answered.len(), 2);
    for seq in [tempo_seq, metronome_seq] {
        assert!(
            answered.iter().any(|(answered, outcome)| *answered == seq
                && matches!(outcome, Err(Rejection::Unanswered))),
            "the caller of {seq:?} gets one answer"
        );
    }
    assert!(steps(&state).is_empty());

    assert!(ensure_ctx(&mut state).is_ok());
    begin_pass_at(&mut state, block_frame(1));
    accepted(&mut state, 90.0, When::Next);
    assert_eq!(steps(&state), [step]);
    assert_eq!(
        target(&state, block_frame(2)),
        tempo(90.0).beats_per_second()
    );
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
