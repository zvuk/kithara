use std::num::NonZeroU32;

use firewheel::{
    clock::InstantSamples,
    dsp::{buffer::ChannelBuffer, declick::DeclickValues},
    event::{NodeEvent, NodeEventType, ProcEvents, ProcEventsIndex, ScheduledEventEntry},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNodeProcessor, NUM_SCRATCH_BUFFERS, NodeID, ProcBuffers, ProcExtra, ProcInfo,
        ProcStore, ProcStreamCtx, ProcessStatus, StreamStatus,
    },
};
use kithara_platform::time::Duration;
use kithara_play::rt::{install_render_context, read_render_context};
use kithara_test_utils::kithara;
use kithara_warp::{
    Beat, BeatGridId, BeatGridQuery, BeatsPerMinute, MapPoint, MapPosition, SessionEpoch,
    SessionFrame,
};
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{
        SessionGridGeneration, SessionTransportCommit, TransportCommitEvent, TransportCommitResult,
        TransportCommitStamp, TransportObservation, TransportProcessError,
    },
    node::SessionTransportProcessor,
    process::{
        TransportCommitState, TransportObservationInput, converge_transport_restart,
        process_transport,
    },
};
use crate::api::{SessionBeat, SessionTransportSnapshot, Tempo, TransportRevision};

const BLOCK_FRAMES: usize = 480;
const SAMPLE_RATE: u32 = 48_000;

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(SAMPLE_RATE).expect("invariant: static sample rate is non-zero")
}

fn second_revision() -> TransportRevision {
    TransportRevision::first()
        .checked_next()
        .expect("invariant: second transport revision exists")
}

fn commit(tempo: f64, playing: bool, revision: TransportRevision) -> SessionTransportCommit {
    SessionTransportCommit::new(
        Tempo::new(tempo).expect("invariant: test tempo is valid"),
        playing,
        revision,
    )
}

fn proc_info_at(clock_samples: i64) -> ProcInfo {
    ProcInfo {
        sample_rate: sample_rate(),
        frames: BLOCK_FRAMES,
        in_silence_mask: SilenceMask::default(),
        out_silence_mask: SilenceMask::default(),
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        prev_output_was_silent: true,
        sample_rate_recip: f64::from(SAMPLE_RATE).recip(),
        clock_samples: InstantSamples(clock_samples),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    }
}

fn block_frame(blocks: usize) -> i64 {
    i64::try_from(BLOCK_FRAMES * blocks).expect("invariant: test block frame fits i64")
}

fn proc_extra() -> (ProcExtra, Output<TransportObservation>) {
    let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    let session_grid = SessionGridGeneration::new(
        BeatGridId::allocate().expect("invariant: fixture grid identity space is available"),
    );
    let initial = TransportObservation::new(None, None, session_grid);
    let (observation_input, observation_output) = triple_buffer(&initial);
    let mut store = ProcStore::with_capacity(3);
    assert!(install_render_context(&mut store).is_ok());
    assert!(
        store
            .insert(TransportCommitState::new(session_grid))
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
            scratch_buffers: ChannelBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(BLOCK_FRAMES),
            declick_values: DeclickValues::new(
                NonZeroU32::new(16).expect("invariant: static fade is non-zero"),
            ),
        },
        observation_output,
    )
}

fn with_events<R>(
    first: Option<NodeEventType>,
    second: Option<NodeEventType>,
    run: impl FnOnce(&mut ProcEvents) -> R,
) -> R {
    let mut immediate = [
        first.map(|event| NodeEvent::new(NodeID::DANGLING, event)),
        second.map(|event| NodeEvent::new(NodeID::DANGLING, event)),
    ];
    let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
    let mut indices = Vec::with_capacity(2);
    for (index, event) in immediate.iter().enumerate() {
        if event.is_some() {
            indices.push(ProcEventsIndex::Immediate(
                u32::try_from(index).expect("invariant: immediate event index fits u32"),
            ));
        }
    }
    let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
    run(&mut events)
}

fn process_node(
    processor: &mut SessionTransportProcessor,
    info: &ProcInfo,
    extra: &mut ProcExtra,
    first: Option<NodeEventType>,
    second: Option<NodeEventType>,
) {
    let inputs: [&[f32]; 0] = [];
    let mut outputs: [&mut [f32]; 0] = [];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let status = with_events(first, second, |events| {
        processor.process(info, buffers, events, extra)
    });
    assert_eq!(status, ProcessStatus::ClearAllOutputs);
}

fn process_result(
    info: &ProcInfo,
    extra: &mut ProcExtra,
    first: Option<NodeEventType>,
    second: Option<NodeEventType>,
) -> Result<(), TransportProcessError> {
    with_events(first, second, |events| {
        process_transport(info, events, &mut extra.store).map(|_| ())
    })
}

fn stage_event(stamp: TransportCommitStamp) -> NodeEventType {
    NodeEventType::custom(TransportCommitEvent::Stage(stamp))
}

fn apply_event(revision: TransportRevision) -> NodeEventType {
    NodeEventType::custom(TransportCommitEvent::Apply(revision))
}

fn abort_event(revision: TransportRevision) -> NodeEventType {
    NodeEventType::custom(TransportCommitEvent::Abort(revision))
}

fn observation(output: &mut Output<TransportObservation>) -> TransportObservation {
    *output.read()
}

fn snapshot(output: &mut Output<TransportObservation>) -> SessionTransportSnapshot {
    observation(output)
        .snapshot()
        .expect("invariant: active transport publishes a snapshot")
}

fn active_harness() -> (
    SessionTransportProcessor,
    ProcExtra,
    Output<TransportObservation>,
    SessionTransportCommit,
) {
    let (mut extra, mut output) = proc_extra();
    let mut processor = SessionTransportProcessor;
    let active = commit(120.0, true, TransportRevision::first());
    let stamp = TransportCommitStamp::new(None, active, SessionFrame::new(0), sample_rate());
    process_node(
        &mut processor,
        &proc_info_at(0),
        &mut extra,
        Some(apply_event(TransportRevision::first())),
        Some(stage_event(stamp)),
    );
    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Applied(TransportRevision::first()))
    );
    (processor, extra, output, active)
}

#[kithara::test]
fn transport_frame_carries_the_exact_processed_musical_context() {
    let (_processor, mut extra, _output, active) = active_harness();
    let frame = with_events(None, None, |events| {
        process_transport(&proc_info_at(block_frame(1)), events, &mut extra.store)
    })
    .expect("invariant: the next contiguous transport block is valid");
    let beats = frame
        .session_beats
        .expect("invariant: the active playing transport has a beat range");

    assert_eq!(frame.session_epoch, SessionEpoch::new(0));
    assert_eq!(frame.transport_revision, Some(active.revision()));
    assert!((f64::from(beats.start) - 0.02).abs() <= f64::EPSILON);
    assert!((f64::from(beats.end) - 0.04).abs() <= f64::EPSILON);
}

#[kithara::test]
fn pre_process_publishes_the_exact_render_context() {
    let (_processor, extra, _output, active) = active_harness();
    let info = proc_info_at(0);
    let context = read_render_context(&extra.store, &info)
        .expect("invariant: the pre-process node published this exact block");

    assert_eq!(
        context.output_frames(),
        &(SessionFrame::new(0)..SessionFrame::new(block_frame(1)))
    );
    assert_eq!(context.sample_rate(), sample_rate());
    assert_eq!(context.session_epoch(), SessionEpoch::new(0));
    assert_eq!(context.transport_revision(), Some(active.revision()));
    let beats = context
        .session_beats()
        .expect("invariant: active transport carries a musical range");
    assert!(f64::from(beats.start).abs() <= f64::EPSILON);
    assert!((f64::from(beats.end) - 0.02).abs() <= f64::EPSILON);
}

#[kithara::test]
fn inactive_transport_is_a_valid_render_context() {
    let (mut extra, _output) = proc_extra();
    let info = proc_info_at(0);
    process_node(
        &mut SessionTransportProcessor,
        &info,
        &mut extra,
        None,
        None,
    );

    let context = read_render_context(&extra.store, &info)
        .expect("invariant: inactive transport still publishes the session axis");
    assert_eq!(context.session_epoch(), SessionEpoch::new(0));
    assert_eq!(context.transport_revision(), None);
    assert_eq!(context.session_beats(), None);
}

#[kithara::test]
fn stale_subblock_cannot_reuse_the_full_render_context() {
    let (_processor, extra, _output, _active) = active_harness();
    let mut subblock = proc_info_at(0);
    subblock.frames /= 2;

    assert_eq!(
        read_render_context(&extra.store, &subblock),
        Err("render context does not match the player process block")
    );
}

#[kithara::test]
fn invalid_transport_block_replaces_the_previous_render_context() {
    let (mut processor, mut extra, _output, _active) = active_harness();
    let info = proc_info_at(0);
    assert!(read_render_context(&extra.store, &info).is_ok());

    process_node(&mut processor, &info, &mut extra, None, None);

    assert_eq!(
        read_render_context(&extra.store, &info),
        Err("render context is invalid")
    );
}

#[kithara::test]
fn transport_commit_publishes_anchor_and_grid_stamp_atomically() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let before = snapshot(&mut output);
    let target = SessionBeat::new(3.25).expect("invariant: relocation target is finite");
    let next = SessionTransportCommit::relocate(active.tempo(), true, second_revision(), target);
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );

    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    let staged = snapshot(&mut output);
    assert_eq!(staged.anchor(), before.anchor());
    assert_eq!(staged.session_grid_stamp(), before.session_grid_stamp());
    assert_eq!(staged.session_epoch(), before.session_epoch());

    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );
    let applied = snapshot(&mut output);
    assert_eq!(applied.revision(), second_revision());
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
        Beat::new(3.25).expect("invariant: relocation beat is finite"),
    ));
    let BeatGridQuery::Resolved(position) = resolved else {
        panic!("expected relocated beat to resolve on the published session grid")
    };
    assert_eq!(
        *position.value().value(),
        MapPosition::Session(SessionFrame::new(block_frame(2)))
    );
}

#[kithara::test]
fn route_restart_advances_session_epoch_and_grid_revision() {
    let (mut processor, mut extra, mut output, _active) = active_harness();
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        None,
        None,
    );
    let before = snapshot(&mut output);

    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
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

    process_node(&mut processor, &proc_info_at(0), &mut extra, None, None);
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
fn reserved_route_restart_promotes_a_commit_rendered_before_stop() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let before = observation(&mut output).session_grid();
    let mut reserved = before;
    reserved
        .advance_restart()
        .expect("invariant: fixture route generation can advance");

    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );

    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
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

    let converged = converge_transport_restart(&mut extra.store, reserved)
        .expect("the reserved restart accepts a newer revision in its target epoch");
    assert_eq!(converged, stopped);
    assert_eq!(observation(&mut output).session_grid(), stopped);
}

#[kithara::test]
fn tempo_commit_waits_for_the_matching_render_boundary() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let before = snapshot(&mut output);
    let old_grid = before.session_grid();
    let old_position = MapPoint::new(
        old_grid.stamp(),
        MapPosition::Session(SessionFrame::new(block_frame(1))),
    );
    let old_tempo = BeatsPerMinute::try_from(120.0)
        .expect("invariant: fixture tempo is a positive finite value");
    assert!(matches!(
        old_grid.tempo_at(old_position),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == old_tempo
    ));
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );

    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    let staged = snapshot(&mut output);
    assert_eq!(staged.revision(), TransportRevision::first());
    assert_eq!(staged.tempo(), active.tempo());
    let staged_grid = staged.session_grid();
    assert_eq!(staged_grid, old_grid);
    assert!(matches!(
        staged_grid.tempo_at(old_position),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == old_tempo
    ));
    assert!((f64::from(staged.position()) - 0.04).abs() <= f64::EPSILON);

    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );
    let applied = observation(&mut output);
    assert_eq!(
        applied.completion(),
        Some(TransportCommitResult::Applied(second_revision()))
    );
    let applied = applied
        .snapshot()
        .expect("invariant: applied transport publishes a snapshot");
    assert_eq!(applied.revision(), second_revision());
    assert_eq!(applied.tempo(), next.tempo());
    assert!((f64::from(applied.position()) - 0.05).abs() <= f64::EPSILON);
    let new_grid = applied.session_grid();
    assert!(new_grid.revision() > old_grid.revision());
    assert_eq!(old_grid.revision(), before.session_grid_stamp().revision());
    assert!(matches!(
        old_grid.tempo_at(old_position),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == old_tempo
    ));
    assert!(matches!(
        new_grid.tempo_at(old_position),
        BeatGridQuery::Stale { expected, given }
            if expected == new_grid.stamp() && given == old_grid.stamp()
    ));
    let new_tempo = BeatsPerMinute::try_from(60.0)
        .expect("invariant: fixture tempo is a positive finite value");
    assert!(matches!(
        new_grid.tempo_at(MapPoint::new(
            new_grid.stamp(),
            MapPosition::Session(SessionFrame::new(block_frame(2))),
        )),
        BeatGridQuery::Resolved(estimate) if *estimate.value() == new_tempo
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
fn relocation_commit_reanchors_the_exact_target_beat() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let target = SessionBeat::new(3.25).expect("invariant: relocation target is finite");
    let next = SessionTransportCommit::relocate(active.tempo(), true, second_revision(), target);
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );

    let relocated = snapshot(&mut output);
    assert_eq!(relocated.revision(), second_revision());
    assert!((f64::from(relocated.position()) - 3.27).abs() <= f64::EPSILON);
    assert_eq!(
        relocated
            .anchor()
            .frame_at(target)
            .expect("invariant: relocation target is representable on its observed anchor"),
        SessionFrame::new(block_frame(2))
    );
}

#[kithara::test]
fn inactive_transport_publishes_a_frozen_position() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let paused = commit(active.tempo().beats_per_minute(), false, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        paused,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );
    let paused_snapshot = snapshot(&mut output);
    assert!(!paused_snapshot.is_playing());
    assert_eq!(paused_snapshot.revision(), second_revision());
    assert!((f64::from(paused_snapshot.position()) - 0.04).abs() <= f64::EPSILON);

    assert_eq!(
        process_result(&proc_info_at(block_frame(3)), &mut extra, None, None,),
        Ok(())
    );
    assert_eq!(snapshot(&mut output), paused_snapshot);
}

#[kithara::test]
fn late_transport_commit_is_rejected_without_changing_the_active_commit() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(2)),
        &mut extra,
        None,
        None,
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(3)),
        &mut extra,
        Some(apply_event(second_revision())),
        None,
    );

    let rejected = observation(&mut output);
    assert_eq!(
        rejected.completion(),
        Some(TransportCommitResult::Rejected(second_revision()))
    );
    let current = rejected
        .snapshot()
        .expect("invariant: rejection keeps the active snapshot");
    assert_eq!(current.revision(), TransportRevision::first());
    assert!((f64::from(current.position()) - 0.08).abs() <= f64::EPSILON);
}

#[kithara::test]
fn stale_transport_commit_is_rejected_without_breaking_the_clock() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let stale = commit(100.0, true, TransportRevision::first());
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(stale),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Rejected(second_revision()))
    );
    assert_eq!(
        process_result(
            &proc_info_at(block_frame(2)),
            &mut extra,
            Some(apply_event(second_revision())),
            None,
        ),
        Ok(())
    );
    let current = snapshot(&mut output);
    assert_eq!(current.revision(), active.revision());
    assert!((f64::from(current.position()) - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn transport_abort_is_idempotent() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );

    for (clock_samples, apply) in [
        (block_frame(2), Some(apply_event(second_revision()))),
        (block_frame(3), None),
    ] {
        assert_eq!(
            process_result(
                &proc_info_at(clock_samples),
                &mut extra,
                Some(abort_event(second_revision())),
                apply,
            ),
            Ok(())
        );
        assert_eq!(
            observation(&mut output).completion(),
            Some(TransportCommitResult::Aborted(second_revision()))
        );
        assert_eq!(snapshot(&mut output).revision(), TransportRevision::first());
    }
}

#[kithara::test]
fn route_reset_rejects_pending_commit_and_reanchors_the_active_beat() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );

    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Rejected(second_revision()))
    );
    process_node(&mut processor, &proc_info_at(0), &mut extra, None, None);
    let restarted = snapshot(&mut output);
    assert_eq!(restarted.revision(), TransportRevision::first());
    assert!((f64::from(restarted.position()) - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn route_reset_withdraws_snapshot_until_new_axis_is_reanchored() {
    let (mut processor, mut extra, mut output, _active) = active_harness();
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        None,
        None,
    );
    let before_restart = snapshot(&mut output);
    let preserved = before_restart.position();
    assert!((f64::from(preserved) - 0.04).abs() <= f64::EPSILON);

    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
    assert_eq!(observation(&mut output).snapshot(), None);

    process_node(&mut processor, &proc_info_at(0), &mut extra, None, None);
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
    let (mut processor, mut extra, mut output, _active) = active_harness();
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        None,
        None,
    );
    let preserved = snapshot(&mut output).position();

    for _ in 0..2 {
        processor.stream_stopped(&mut ProcStreamCtx {
            store: &mut extra.store,
            logger: &mut extra.logger,
        });
        assert_eq!(observation(&mut output).snapshot(), None);
    }

    process_node(&mut processor, &proc_info_at(0), &mut extra, None, None);
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
fn duplicate_stage_in_one_block_is_rejected() {
    let (mut extra, _output) = proc_extra();
    let active = commit(120.0, true, TransportRevision::first());
    let stamp = TransportCommitStamp::new(None, active, SessionFrame::new(0), sample_rate());
    assert_eq!(
        process_result(
            &proc_info_at(0),
            &mut extra,
            Some(stage_event(stamp)),
            Some(stage_event(stamp)),
        ),
        Err(TransportProcessError::DuplicateEvent)
    );
}

#[kithara::test]
fn foreign_event_is_rejected() {
    let (mut extra, _output) = proc_extra();
    assert_eq!(
        process_result(
            &proc_info_at(0),
            &mut extra,
            Some(NodeEventType::custom(0_u8)),
            None,
        ),
        Err(TransportProcessError::UnexpectedEvent)
    );
}

#[kithara::test]
fn discontinuous_block_start_is_rejected() {
    let (_processor, mut extra, _output, _active) = active_harness();
    assert_eq!(
        process_result(&proc_info_at(481), &mut extra, None, None),
        Err(TransportProcessError::FrameDiscontinuity)
    );
}

#[kithara::test]
fn stage_for_another_sample_rate_is_rejected() {
    let (_processor, mut extra, mut output, active) = active_harness();
    let foreign_rate =
        NonZeroU32::new(SAMPLE_RATE * 2).expect("invariant: doubled sample rate is non-zero");
    let stamp = TransportCommitStamp::new(
        Some(active),
        commit(60.0, true, second_revision()),
        SessionFrame::new(block_frame(2)),
        foreign_rate,
    );

    assert_eq!(
        process_result(
            &proc_info_at(block_frame(1)),
            &mut extra,
            Some(stage_event(stamp)),
            None,
        ),
        Ok(())
    );

    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Rejected(second_revision()))
    );
    assert_eq!(snapshot(&mut output).tempo(), active.tempo());
}

#[kithara::test]
fn apply_for_another_sample_rate_is_rejected() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let next = commit(60.0, true, second_revision());
    let stamp = TransportCommitStamp::new(
        Some(active),
        next,
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );

    let mut foreign_block = proc_info_at(block_frame(2));
    foreign_block.sample_rate =
        NonZeroU32::new(SAMPLE_RATE * 2).expect("invariant: doubled sample rate is non-zero");
    assert_eq!(
        process_result(
            &foreign_block,
            &mut extra,
            Some(apply_event(second_revision())),
            None
        ),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Rejected(second_revision()))
    );
}

#[kithara::test]
fn a_failing_block_rejects_the_pending_stamp_and_still_publishes() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let stamp = TransportCommitStamp::new(
        Some(active),
        commit(60.0, true, second_revision()),
        SessionFrame::new(block_frame(2)),
        sample_rate(),
    );
    process_node(
        &mut processor,
        &proc_info_at(block_frame(1)),
        &mut extra,
        Some(stage_event(stamp)),
        None,
    );
    assert_eq!(observation(&mut output).completion(), None);

    // A block that does not start on the expected boundary fails mid-process.
    assert_eq!(
        process_result(&proc_info_at(block_frame(1) + 1), &mut extra, None, None),
        Err(TransportProcessError::FrameDiscontinuity)
    );

    let observed = observation(&mut output);
    assert_eq!(
        observed.completion(),
        Some(TransportCommitResult::Rejected(second_revision())),
        "the control thread must learn about the dropped stamp from a failing block"
    );
    assert!(observed.snapshot().is_some());
}
