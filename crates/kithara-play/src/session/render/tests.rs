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
use kithara_audio::{PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome};
use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use ringbuf::traits::Producer;
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{
        TransportCommitEvent, TransportCommitStamp, TransportCommitState, TransportObservation,
        TransportObservationInput,
    },
    context::{RenderContext, RenderFrame, SessionTransportCommit},
    node::{RenderContextProcessor, RenderContextSlot, RenderContextUnavailable},
    read_render_context,
};
use crate::{
    Resource, SessionBeat, Tempo,
    bridge::{PlayerCmd, SharedEq, SlotControl, slot_channels},
    player::{
        node::{ContextRequirement, PlayerNodeProcessor, StreamShape},
        track::PlayerResource,
    },
};

const BLOCK_FRAMES: usize = 480;
const SAMPLE_RATE: u32 = 48_000;

struct EofReader {
    bus: EventBus,
    spec: PcmSpec,
    metadata: TrackMetadata,
}

impl Default for EofReader {
    fn default() -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            spec: PcmSpec::new(2, sample_rate()),
        }
    }
}

impl PcmRead for EofReader {
    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl PcmSession for EofReader {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl PcmControl for EofReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
}

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(SAMPLE_RATE).expect("static sample rate")
}

fn proc_info() -> ProcInfo {
    proc_info_at(0)
}

fn proc_info_at(clock_samples: i64) -> ProcInfo {
    let sample_rate = sample_rate();
    ProcInfo {
        sample_rate,
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

fn proc_extra(beats_per_minute: Option<f64>) -> ProcExtra {
    proc_extra_with_observation(beats_per_minute).0
}

fn proc_extra_with_observation(
    beats_per_minute: Option<f64>,
) -> (ProcExtra, Output<TransportObservation>) {
    let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    let active = beats_per_minute.map(|beats_per_minute| {
        SessionTransportCommit::new(
            Tempo::new(beats_per_minute).expect("valid test tempo"),
            true,
            1,
        )
    });
    let transport_state = active.map_or_else(TransportCommitState::default, |active| {
        TransportCommitState::with_active(
            active,
            RenderFrame::new(0),
            SessionBeat::new(0.0).expect("finite beat"),
            sample_rate(),
        )
    });
    let (observation_input, observation_output) = triple_buffer(&TransportObservation::default());
    let mut store = ProcStore::with_capacity(3);
    assert!(store.insert(RenderContextSlot::default()).is_ok());
    assert!(store.insert(transport_state).is_ok());
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
            declick_values: DeclickValues::new(NonZeroU32::new(16).expect("static fade")),
        },
        observation_output,
    )
}

fn process_context(processor: &mut RenderContextProcessor, info: &ProcInfo, extra: &mut ProcExtra) {
    process_context_event(processor, info, extra, None);
}

fn process_context_event(
    processor: &mut RenderContextProcessor,
    info: &ProcInfo,
    extra: &mut ProcExtra,
    event: Option<NodeEventType>,
) {
    let inputs: [&[f32]; 0] = [];
    let mut outputs: [&mut [f32]; 0] = [];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let mut immediate = [event.map(|event| NodeEvent::new(NodeID::DANGLING, event))];
    let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
    let mut indices = Vec::new();
    if immediate[0].is_some() {
        indices.push(ProcEventsIndex::Immediate(0));
    }
    let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
    assert_eq!(
        processor.process(info, buffers, &mut events, extra),
        ProcessStatus::ClearAllOutputs
    );
}

fn player_processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let processor = PlayerNodeProcessor::with_context_requirement(
        inputs,
        StreamShape {
            sample_rate: sample_rate(),
            max_block_frames: NonZeroU32::new(
                u32::try_from(BLOCK_FRAMES).expect("block size fits u32"),
            )
            .expect("static block size"),
        },
        &PcmPool::default(),
        ContextRequirement::Session,
    );
    (processor, control)
}

fn load_playing_track(
    processor: &mut PlayerNodeProcessor,
    control: &mut SlotControl,
    src: &'static str,
) -> Arc<str> {
    let src = Arc::from(src);
    let resource = Resource::from_reader(EofReader::default(), None);
    let resource = Box::new(PlayerResource::new(
        resource,
        Arc::clone(&src),
        &PcmPool::default(),
    ));
    assert!(
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                binding: None,
                resource,
                item_id: None,
            })
            .is_ok()
    );
    assert!(control.cmd_tx.try_push(PlayerCmd::SetPaused(false)).is_ok());
    processor.drain_commands();
    processor.track_mut(&src).expect("loaded track").play();
    src
}

fn process_player(
    processor: &mut PlayerNodeProcessor,
    info: &ProcInfo,
    extra: &mut ProcExtra,
) -> ProcessStatus {
    let inputs: [&[f32]; 0] = [];
    let mut left = [0.0; BLOCK_FRAMES];
    let mut right = [0.0; BLOCK_FRAMES];
    let mut outputs = [&mut left[..], &mut right[..]];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let mut immediate: [Option<NodeEvent>; 0] = [];
    let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
    let mut indices = Vec::new();
    let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
    processor.process(info, buffers, &mut events, extra)
}

#[kithara::test]
fn two_player_nodes_receive_the_same_render_context() {
    let info = proc_info();
    let mut extra = proc_extra(Some(120.0));
    process_context(&mut RenderContextProcessor, &info, &mut extra);

    let (mut left, mut left_control) = player_processor();
    let (mut right, mut right_control) = player_processor();
    let left_src = load_playing_track(&mut left, &mut left_control, "left");
    let right_src = load_playing_track(&mut right, &mut right_control, "right");
    assert_eq!(
        process_player(&mut left, &info, &mut extra),
        ProcessStatus::OutputsModified
    );
    assert_eq!(
        process_player(&mut right, &info, &mut extra),
        ProcessStatus::OutputsModified
    );

    let context = read_render_context(&extra.store, &info).expect("stored context");
    let context_address = std::ptr::from_ref(context).addr();
    let (left_address, left_context) = left
        .track(&left_src)
        .and_then(|track| track.last_render_context())
        .expect("left track context");
    let (right_address, right_context) = right
        .track(&right_src)
        .and_then(|track| track.last_render_context())
        .expect("right track context");
    assert_eq!(left_address, context_address);
    assert_eq!(right_address, context_address);
    assert_eq!(left_context, context);
    assert_eq!(right_context, context);
    assert_eq!(context.render_frames().start.get(), 0);
    assert_eq!(context.render_frames().end.get(), BLOCK_FRAMES as i64);
    assert_eq!(context.sample_rate(), sample_rate());
    let commit = context.transport_commit().expect("active transport commit");
    assert_eq!(commit.tempo(), Tempo::new(120.0).expect("valid tempo"));
    assert_eq!(commit.revision(), 1);
    let beats = context.session_beats().expect("active transport");
    assert!(beats.start.get().abs() <= f64::EPSILON);
    assert!((beats.end.get() - 0.02).abs() <= f64::EPSILON);
}

#[kithara::test]
fn two_player_nodes_receive_the_same_committed_revision() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid old tempo"), true, 1);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid next tempo"), true, 2);
    let mut extra = proc_extra(Some(120.0));
    let mut context_processor = RenderContextProcessor;
    process_context(&mut context_processor, &proc_info_at(0), &mut extra);
    process_context_event(
        &mut context_processor,
        &proc_info_at(BLOCK_FRAMES as i64),
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(
            TransportCommitStamp::new(
                Some(old),
                next,
                RenderFrame::new((BLOCK_FRAMES * 2) as i64),
                sample_rate(),
            ),
        ))),
    );
    let matching = proc_info_at((BLOCK_FRAMES * 2) as i64);
    process_context_event(
        &mut context_processor,
        &matching,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Apply(2))),
    );

    let (mut left, mut left_control) = player_processor();
    let (mut right, mut right_control) = player_processor();
    let left_src = load_playing_track(&mut left, &mut left_control, "left");
    let right_src = load_playing_track(&mut right, &mut right_control, "right");
    assert_eq!(
        process_player(&mut left, &matching, &mut extra),
        ProcessStatus::OutputsModified
    );
    assert_eq!(
        process_player(&mut right, &matching, &mut extra),
        ProcessStatus::OutputsModified
    );

    let context = read_render_context(&extra.store, &matching).expect("committed context");
    let context_address = std::ptr::from_ref(context).addr();
    for (processor, src) in [(&left, &left_src), (&right, &right_src)] {
        let (address, observed) = processor
            .track(src)
            .and_then(|track| track.last_render_context())
            .expect("player track context");
        assert_eq!(address, context_address);
        assert_eq!(observed.transport_commit(), Some(next));
    }
}

#[kithara::test]
fn tempo_commit_waits_for_the_matching_render_boundary() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid old tempo"), true, 1);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid next tempo"), true, 2);
    let mut extra = proc_extra(Some(120.0));
    let mut processor = RenderContextProcessor;

    let first = proc_info_at(0);
    process_context(&mut processor, &first, &mut extra);

    let old_boundary = proc_info_at(BLOCK_FRAMES as i64);
    let stamp = TransportCommitStamp::new(
        Some(old),
        next,
        RenderFrame::new((BLOCK_FRAMES * 2) as i64),
        sample_rate(),
    );
    process_context_event(
        &mut processor,
        &old_boundary,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(stamp))),
    );
    let context =
        read_render_context(&extra.store, &old_boundary).expect("old commit remains valid");
    assert_eq!(context.transport_commit(), Some(old));

    let matching = proc_info_at((BLOCK_FRAMES * 2) as i64);
    process_context_event(
        &mut processor,
        &matching,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Apply(2))),
    );
    let context =
        read_render_context(&extra.store, &matching).expect("matching commit becomes valid");
    assert_eq!(context.transport_commit(), Some(next));
    let beats = context.session_beats().expect("matching beat range");
    assert!((beats.start.get() - 0.04).abs() <= f64::EPSILON);
}

#[kithara::test]
fn inactive_transport_is_a_valid_render_context() {
    let info = proc_info();
    let mut extra = proc_extra(None);
    process_context(&mut RenderContextProcessor, &info, &mut extra);

    let context = read_render_context(&extra.store, &info).expect("inactive context is valid");
    assert!(context.session_beats().is_none());
}

#[kithara::test]
fn late_transport_commit_is_rejected_without_changing_the_active_commit() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid tempo"), true, 1);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid tempo"), true, 2);
    let (mut extra, mut observation) = proc_extra_with_observation(Some(120.0));
    let mut processor = RenderContextProcessor;
    process_context(&mut processor, &proc_info(), &mut extra);

    let stage = proc_info_at(BLOCK_FRAMES as i64);
    process_context_event(
        &mut processor,
        &stage,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(
            TransportCommitStamp::new(
                Some(old),
                next,
                RenderFrame::new((BLOCK_FRAMES * 2) as i64),
                sample_rate(),
            ),
        ))),
    );
    process_context(
        &mut processor,
        &proc_info_at((BLOCK_FRAMES * 2) as i64),
        &mut extra,
    );
    let late = proc_info_at((BLOCK_FRAMES * 3) as i64);
    process_context_event(
        &mut processor,
        &late,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Apply(2))),
    );
    assert_eq!(
        observation.read().completion(),
        Some(super::commit::TransportCommitResult::Rejected(2))
    );
    let context = read_render_context(&extra.store, &late)
        .expect("late commit must leave the previous context valid");
    assert_eq!(context.transport_commit(), Some(old));
}

#[kithara::test]
fn stale_transport_commit_is_rejected_without_invalidating_the_context() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid tempo"), true, 1);
    let stale = SessionTransportCommit::new(Tempo::new(100.0).expect("valid tempo"), true, 0);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid tempo"), true, 2);
    let (mut extra, mut observation) = proc_extra_with_observation(Some(120.0));
    let mut processor = RenderContextProcessor;
    process_context(&mut processor, &proc_info(), &mut extra);

    let boundary = proc_info_at(BLOCK_FRAMES as i64);
    process_context_event(
        &mut processor,
        &boundary,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(
            TransportCommitStamp::new(
                Some(stale),
                next,
                RenderFrame::new((BLOCK_FRAMES * 2) as i64),
                sample_rate(),
            ),
        ))),
    );
    assert_eq!(
        observation.read().completion(),
        Some(super::commit::TransportCommitResult::Rejected(2))
    );
    assert_eq!(
        read_render_context(&extra.store, &boundary)
            .expect("previous commit remains active")
            .transport_commit(),
        Some(old)
    );
}

#[kithara::test]
fn transport_abort_is_idempotent() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid tempo"), true, 1);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid tempo"), true, 2);
    let (mut extra, mut observation) = proc_extra_with_observation(Some(120.0));
    let mut processor = RenderContextProcessor;
    process_context(&mut processor, &proc_info(), &mut extra);
    process_context_event(
        &mut processor,
        &proc_info_at(BLOCK_FRAMES as i64),
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(
            TransportCommitStamp::new(
                Some(old),
                next,
                RenderFrame::new((BLOCK_FRAMES * 2) as i64),
                sample_rate(),
            ),
        ))),
    );

    for clock_samples in [(BLOCK_FRAMES * 2) as i64, (BLOCK_FRAMES * 3) as i64] {
        process_context_event(
            &mut processor,
            &proc_info_at(clock_samples),
            &mut extra,
            Some(NodeEventType::custom(TransportCommitEvent::Abort(2))),
        );
        assert_eq!(
            observation.read().completion(),
            Some(super::commit::TransportCommitResult::Aborted(2))
        );
    }
}

#[kithara::test]
fn route_reset_rejects_pending_commit_and_reanchors_the_active_beat() {
    let old = SessionTransportCommit::new(Tempo::new(120.0).expect("valid tempo"), true, 1);
    let next = SessionTransportCommit::new(Tempo::new(60.0).expect("valid tempo"), true, 2);
    let (mut extra, mut observation) = proc_extra_with_observation(Some(120.0));
    let mut processor = RenderContextProcessor;
    process_context(&mut processor, &proc_info(), &mut extra);
    let old_boundary = proc_info_at(BLOCK_FRAMES as i64);
    process_context_event(
        &mut processor,
        &old_boundary,
        &mut extra,
        Some(NodeEventType::custom(TransportCommitEvent::Stage(
            TransportCommitStamp::new(
                Some(old),
                next,
                RenderFrame::new((BLOCK_FRAMES * 2) as i64),
                sample_rate(),
            ),
        ))),
    );

    processor.stream_stopped(&mut ProcStreamCtx {
        store: &mut extra.store,
        logger: &mut extra.logger,
    });
    assert_eq!(
        observation.read().completion(),
        Some(super::commit::TransportCommitResult::Rejected(2))
    );

    let restarted = proc_info();
    process_context(&mut processor, &restarted, &mut extra);
    let context = read_render_context(&extra.store, &restarted).expect("reanchored context");
    assert_eq!(context.transport_commit(), Some(old));
    let beats = context.session_beats().expect("active transport");
    assert!((beats.start.get() - 0.04).abs() <= f64::EPSILON);
    assert!((beats.end.get() - 0.06).abs() <= f64::EPSILON);
}

#[kithara::test]
fn render_context_rejects_beats_without_a_playing_commit() {
    let beats =
        SessionBeat::new(0.0).expect("finite beat")..SessionBeat::new(0.02).expect("finite beat");
    assert!(
        RenderContext::new(
            RenderFrame::new(0)..RenderFrame::new(BLOCK_FRAMES as i64),
            sample_rate(),
            Some(beats),
            None,
        )
        .is_none()
    );
}

#[kithara::test]
fn render_context_derives_exact_handover_subrange() {
    let info = proc_info();
    let mut extra = proc_extra(Some(120.0));
    process_context(&mut RenderContextProcessor, &info, &mut extra);

    let context = read_render_context(&extra.store, &info).expect("stored context");
    let second_half = context
        .for_output_range(BLOCK_FRAMES / 2..BLOCK_FRAMES)
        .expect("valid output subrange");
    assert_eq!(
        second_half.render_frames().start.get(),
        i64::try_from(BLOCK_FRAMES / 2).expect("static block size")
    );
    assert_eq!(
        second_half.render_frames().end.get(),
        i64::try_from(BLOCK_FRAMES).expect("static block size")
    );
    let beats = second_half.session_beats().expect("active transport");
    assert!((beats.start.get() - 0.01).abs() <= f64::EPSILON);
    assert!((beats.end.get() - 0.02).abs() <= f64::EPSILON);
}

#[kithara::test]
fn render_context_rejects_output_range_outside_callback() {
    let info = proc_info();
    let mut extra = proc_extra(Some(120.0));
    process_context(&mut RenderContextProcessor, &info, &mut extra);

    let context = read_render_context(&extra.store, &info).expect("stored context");
    assert!(
        context
            .for_output_range(BLOCK_FRAMES..BLOCK_FRAMES + 1)
            .is_none()
    );
}

#[kithara::test]
fn node_local_subblock_cannot_reuse_a_stale_render_context() {
    let info = proc_info();
    let mut extra = proc_extra(Some(120.0));
    process_context(&mut RenderContextProcessor, &info, &mut extra);

    let mut subblock = proc_info();
    subblock.frames /= 2;
    assert_eq!(
        read_render_context(&extra.store, &subblock),
        Err(RenderContextUnavailable::Stale)
    );
}

#[kithara::test]
fn invalid_render_context_replaces_the_previous_snapshot() {
    let mut extra = proc_extra(Some(120.0));
    let mut processor = RenderContextProcessor;
    process_context(&mut processor, &proc_info(), &mut extra);
    assert!(read_render_context(&extra.store, &proc_info()).is_ok());

    process_context(&mut processor, &proc_info(), &mut extra);
    assert_eq!(
        read_render_context(&extra.store, &proc_info()),
        Err(RenderContextUnavailable::Invalid)
    );
}
