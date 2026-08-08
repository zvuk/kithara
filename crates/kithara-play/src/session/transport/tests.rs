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
use kithara_audio::{SessionAnchorCell, SessionBeat, SessionFrame};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{
        SessionTransportCommit, TransportCommitEvent, TransportCommitResult, TransportCommitStamp,
        TransportObservation, TransportProcessError,
    },
    node::SessionTransportProcessor,
    process::{TransportCommitState, TransportObservationInput, process_transport},
};
use crate::api::{SessionTransportSnapshot, Tempo, TransportRevision};

const BLOCK_FRAMES: usize = 480;
const SAMPLE_RATE: u32 = 48_000;

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(SAMPLE_RATE).expect("invariant: static sample rate is non-zero")
}

fn second_revision() -> TransportRevision {
    TransportRevision::FIRST
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
    let (observation_input, observation_output) = triple_buffer(&TransportObservation::default());
    let mut store = ProcStore::with_capacity(2);
    assert!(
        store
            .insert(TransportCommitState::new(SessionAnchorCell::new()))
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
        process_transport(info, events, &mut extra.store)
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
    let active = commit(120.0, true, TransportRevision::FIRST);
    let stamp = TransportCommitStamp::new(None, active, SessionFrame::new(0), sample_rate());
    process_node(
        &mut processor,
        &proc_info_at(0),
        &mut extra,
        Some(apply_event(TransportRevision::FIRST)),
        Some(stage_event(stamp)),
    );
    assert_eq!(
        observation(&mut output).completion(),
        Some(TransportCommitResult::Applied(TransportRevision::FIRST))
    );
    (processor, extra, output, active)
}

#[kithara::test]
fn tempo_commit_waits_for_the_matching_render_boundary() {
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
    let staged = snapshot(&mut output);
    assert_eq!(staged.revision(), TransportRevision::FIRST);
    assert_eq!(staged.tempo(), active.tempo());
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
    assert_eq!(current.revision(), TransportRevision::FIRST);
    assert!((f64::from(current.position()) - 0.08).abs() <= f64::EPSILON);
}

#[kithara::test]
fn stale_transport_commit_is_rejected_without_breaking_the_clock() {
    let (mut processor, mut extra, mut output, active) = active_harness();
    let stale = commit(100.0, true, TransportRevision::FIRST);
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
        assert_eq!(snapshot(&mut output).revision(), TransportRevision::FIRST);
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
    assert_eq!(restarted.revision(), TransportRevision::FIRST);
    assert!((f64::from(restarted.position()) - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn duplicate_stage_in_one_block_is_rejected() {
    let (mut extra, _output) = proc_extra();
    let active = commit(120.0, true, TransportRevision::FIRST);
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

/// Frames per beat of the harness commit: 48000 Hz at 120 BPM.
const FRAMES_PER_BEAT: i64 = 24_000;

/// Tier A. A stamped beat resolves to its own frame, so the offset a track is
/// started at is the frame the beat is on — not the block it falls in, not a
/// rounded boundary. Zero frames of error is the whole point of the anchor.
#[kithara::test]
fn a_stamped_beat_resolves_to_its_exact_frame() {
    let (_processor, extra, _output, _active) = active_harness();
    let state = extra
        .store
        .try_get::<TransportCommitState>()
        .expect("invariant: the harness installed the transport state");
    let block = i64::try_from(BLOCK_FRAMES).expect("invariant: the block fits i64");

    for beat in 1..5_i64 {
        let frame = beat * FRAMES_PER_BEAT;
        let block_start = frame - frame.rem_euclid(block);
        let target = SessionBeat::new(
            beat.to_f64()
                .ok_or(())
                .expect("invariant: the beat is representable"),
        )
        .expect("invariant: the beat is finite");

        let offset =
            state.offset_for_beat(&proc_info_at(block_start), target, TransportRevision::FIRST);

        assert_eq!(
            offset,
            Some(usize::try_from(frame - block_start).expect("invariant: the offset fits usize")),
            "beat {beat} must resolve to frame {frame}"
        );
    }
}

/// A beat that is not inside this block has no offset here. The render pass
/// asks again next block rather than starting early.
#[kithara::test]
fn a_beat_outside_the_block_has_no_offset() {
    let (_processor, extra, _output, _active) = active_harness();
    let state = extra
        .store
        .try_get::<TransportCommitState>()
        .expect("invariant: the harness installed the transport state");
    let target = SessionBeat::new(1.0).expect("invariant: the beat is finite");

    let offset = state.offset_for_beat(&proc_info_at(0), target, TransportRevision::FIRST);

    assert_eq!(offset, None);
}

/// A start planned against a superseded commit is dropped, not re-aimed: the
/// frame it was computed for is no longer the frame that beat lands on.
#[kithara::test]
fn a_stale_revision_yields_no_offset() {
    let (_processor, extra, _output, _active) = active_harness();
    let state = extra
        .store
        .try_get::<TransportCommitState>()
        .expect("invariant: the harness installed the transport state");
    let target = SessionBeat::new(1.0).expect("invariant: the beat is finite");
    let block = i64::try_from(BLOCK_FRAMES).expect("invariant: the block fits i64");
    let block_start = FRAMES_PER_BEAT - FRAMES_PER_BEAT.rem_euclid(block);

    let offset = state.offset_for_beat(&proc_info_at(block_start), target, second_revision());

    assert_eq!(offset, None);
}
