use std::{num::NonZeroU32, ops::Range, sync::atomic::Ordering};

use firewheel::{
    dsp::{
        fade::FadeCurve,
        mix::{Mix, MixDSP},
    },
    node::ProcBuffers,
    param::smoother::SmootherConfig,
};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_events::TrackId;
use kithara_sync::{ClaimError, SyncApplied, SyncExecutionReject};
use kithara_warp::{PresentationFrontier, RenderContext, WarpMapRevision};
use num_traits::cast::AsPrimitive;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Observer, Producer},
};
use smallvec::SmallVec;
use tracing::warn;

use super::{
    processor::{PlayerNodeProcessor, StreamShape},
    track::{RtSink, SyncFadeTail, TrackReadOutcome},
};
use crate::{
    CrossfadeCurve, CrossfadeSettings,
    bridge::{
        PlaybackShared, PlayerNotification, RtMetrics, SyncReceiptTx, TrackState,
        sync::{SyncReturn, SyncTicket},
    },
    rt::{TrackSlot, TrackSlots},
};

type ActiveTrackEntry = (usize, TrackSlot, bool);

/// The callback's pre-switch track order, including its original leaders.
struct TrackOrder {
    loaded: SmallVec<[(TrackSlot, TrackState); PlayerNodeProcessor::MAX_TRACKS]>,
    active: SmallVec<[ActiveTrackEntry; PlayerNodeProcessor::MAX_TRACKS]>,
}

impl TrackOrder {
    fn capture(tracks: &TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>) -> Self {
        let loaded: SmallVec<[(TrackSlot, TrackState); PlayerNodeProcessor::MAX_TRACKS]> = tracks
            .iter()
            .map(|(idx, track)| (idx, track.state()))
            .collect();
        let active = loaded
            .iter()
            .enumerate()
            .filter(|(_, (_, state))| state.is_playing())
            .map(|(loaded_idx, (idx, state))| (loaded_idx, *idx, state.is_leading()))
            .collect();
        Self { loaded, active }
    }
}

#[derive(Clone, Copy)]
struct Handover {
    offset: usize,
}

pub(crate) struct RenderTargets<'a> {
    pub(crate) notification_tx: &'a mut HeapProd<PlayerNotification>,
    pub(crate) metrics: &'a RtMetrics,
    pub(crate) tracks: &'a mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    /// Slot seek epoch published when this block started rendering.
    pub(crate) seek_epoch: u64,
    pub(crate) sync: SyncRender<'a>,
}

pub(crate) struct SyncRender<'a> {
    pub(crate) pending: &'a mut HeapCons<SyncTicket>,
    pub(crate) tail: &'a mut Option<SyncFadeTail>,
    pub(crate) receipts: &'a mut Option<SyncReceiptTx>,
    pub(crate) returns: &'a mut HeapProd<SyncReturn>,
    pub(crate) playback: &'a PlaybackShared,
}

enum SyncAttempt {
    None,
    Claimed {
        item_id: TrackId,
        outcome: Option<TrackReadOutcome>,
        handover_offset: Option<usize>,
    },
    PrefixRendered {
        item_id: TrackId,
        offset: usize,
        outcome: Option<TrackReadOutcome>,
    },
}

const SYNC_FADE: CrossfadeSettings = CrossfadeSettings {
    duration: 0.004,
    curve: CrossfadeCurve::Linear,
    depth: 1.0,
    position: 0.5,
};

pub(crate) struct RenderPass {
    gate: MixDSP,
    scratch_bufs: [SampleBuffer; Self::SCRATCH_BUF_COUNT],
    priming: bool,
    capacity: usize,
}

impl RenderPass {
    const GATE_CURVE: FadeCurve = FadeCurve::Linear;
    const MIN_STEREO: usize = 2;
    const SCRATCH_BUF_COUNT: usize = 4;

    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        gate_smoothing: SmootherConfig,
    ) -> Self
    where
        S: HasPool<f32>,
    {
        let mut pass = Self {
            scratch_bufs: std::array::from_fn(|_| pools.get::<f32>()),
            capacity: 0,
            priming: true,
            gate: MixDSP::new(
                Mix::FULLY_WET,
                Self::GATE_CURVE,
                gate_smoothing,
                shape.sample_rate,
            ),
        };
        pass.resize(shape.max_block_frames.get().as_());
        pass
    }

    /// Render audio for all active tracks into the output buffers.
    ///
    /// Frames are clamped rather than grown, since growing a pooled buffer here would allocate on
    /// the audio thread; frames past the clamp are already silence-filled.
    pub(crate) fn render_audio(
        &mut self,
        context: Option<&RenderContext>,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        let mut playback_started = false;

        if buffers.outputs.len() < Self::MIN_STEREO {
            return (false, None);
        }

        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        let frames = frames.min(self.capacity);

        self.gate.set_mix(
            if is_playing {
                Mix::FULLY_DRY
            } else {
                Mix::FULLY_WET
            },
            Self::GATE_CURVE,
        );
        if self.priming {
            self.priming = false;
            self.gate.reset_to_target();
        }
        if !is_playing && self.gate.has_settled() {
            return (false, None);
        }

        let (read, bus) = self.scratch_bufs.split_at_mut(Self::MIN_STEREO);
        let (read_buf0, read_buf1) = read.split_at_mut(1);
        let (bus_buf0, bus_buf1) = bus.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut bus_bufs = [&mut bus_buf0[0][..frames], &mut bus_buf1[0][..frames]];
        for ch_buffer in &mut bus_bufs {
            ch_buffer.fill(0.0);
        }
        let tracks = targets.tracks;
        let mut sink = RtSink::new(targets.notification_tx, targets.metrics, targets.seek_epoch);
        let mut sync = targets.sync;
        // Keep the ordinary leading traversal from the start of the block:
        // the old prefix can reach EOF before the attempted physical switch.
        let order = TrackOrder::capture(tracks);
        return_settled_tail(&mut sync);
        let mut attempt = if is_playing {
            render_sync_activation(
                context,
                frames,
                tracks,
                &mut sync,
                &mut read_bufs,
                &mut bus_bufs,
                &mut sink,
            )
        } else {
            SyncAttempt::None
        };
        if matches!(&attempt, SyncAttempt::Claimed { .. }) {
            playback_started = true;
        } else if let (Some(context), Some(tail)) = (context, sync.tail.as_mut()) {
            if !tail.settled() {
                tail.render(
                    context,
                    &mut read_bufs,
                    &mut bus_bufs,
                    0..frames,
                    targets.metrics,
                );
            }
            return_settled_tail(&mut sync);
        }
        let (rendered, leading_outcome_pos_dur) = render_active_tracks(
            context,
            tracks,
            &order,
            &mut attempt,
            &mut read_bufs,
            &mut bus_bufs,
            &mut sink,
        );
        playback_started |= rendered;

        let (out_left, out_right) = buffers.outputs.split_at_mut(1);
        self.gate.mix_dry_into_wet_stereo(
            bus_bufs[0],
            bus_bufs[1],
            &mut out_left[0][..frames],
            &mut out_right[0][..frames],
            frames,
        );

        (playback_started, leading_outcome_pos_dur)
    }

    pub(crate) fn resize(&mut self, max_frames: usize) {
        let mut capacity = usize::MAX;
        for buf in &mut self.scratch_bufs {
            if buf.ensure_len(max_frames).is_err() {
                warn!(
                    max_frames,
                    held = buf.len(),
                    "sample pool budget cannot afford the render scratch; blocks are clamped"
                );
            }
            buf.fill(0.0);
            capacity = capacity.min(buf.len());
        }
        self.capacity = capacity;
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.gate.update_sample_rate(sample_rate);
    }
}

/// Traverse the original leaders and handovers after any Sync cutover.
fn render_active_tracks(
    context: Option<&RenderContext>,
    tracks: &mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    order: &TrackOrder,
    attempt: &mut SyncAttempt,
    read_bufs: &mut [&mut [f32]],
    bus_bufs: &mut [&mut [f32]],
    sink: &mut RtSink<'_>,
) -> (bool, Option<(f64, f64)>) {
    let frames = read_bufs[0].len();
    let mut playback_started = false;
    let mut leading_outcome_pos_dur = None;
    let mut active_slots = [false; PlayerNodeProcessor::MAX_TRACKS];
    for (loaded_idx, _, _) in &order.active {
        active_slots[*loaded_idx] = true;
    }
    let mut skip_tracks = [false; PlayerNodeProcessor::MAX_TRACKS];

    for (track_idx, (_arena_slot, track_handle, was_leading)) in order.active.iter().enumerate() {
        if skip_tracks[track_idx] {
            continue;
        }

        let (mut read_outcome, handover_offset) = {
            let Some(track) = tracks.at_mut(*track_handle) else {
                continue;
            };
            let result = match attempt {
                SyncAttempt::Claimed {
                    item_id,
                    outcome,
                    handover_offset,
                } if *item_id == track.item_id() => {
                    let Some(outcome) = outcome.take() else {
                        unreachable!("claimed track retains its one render outcome");
                    };
                    (outcome, *handover_offset)
                }
                SyncAttempt::PrefixRendered {
                    item_id,
                    offset,
                    outcome,
                } if *item_id == track.item_id() => {
                    let prefix = outcome.take();
                    match prefix {
                        Some(TrackReadOutcome::Full { .. }) => {
                            let suffix =
                                track.render(context, read_bufs, bus_bufs, *offset..frames, sink);
                            extend_outcome(*offset, suffix)
                        }
                        Some(other) => (other, None),
                        None => (
                            track.render(context, read_bufs, bus_bufs, 0..frames, sink),
                            None,
                        ),
                    }
                }
                _ => (
                    track.render(context, read_bufs, bus_bufs, 0..frames, sink),
                    None,
                ),
            };
            playback_started = true;
            result
        };

        if *was_leading {
            if let Some(snapshot) = outcome_position_duration(&read_outcome) {
                leading_outcome_pos_dur = Some(snapshot);
            }

            let mut handover = handover_offset
                .map(|offset| Handover { offset })
                .or_else(|| initial_handover(&read_outcome));

            for (next_idx, (_, next_handle, next_is_leading)) in order.active.iter().enumerate() {
                let Some(handoff) = handover else {
                    break;
                };
                let offset = handoff.offset;
                if next_idx == track_idx || skip_tracks[next_idx] || !*next_is_leading {
                    continue;
                }
                if offset >= frames {
                    break;
                }

                let Some(outcome) = tracks
                    .at_mut(*next_handle)
                    .map(|track| track.render(context, read_bufs, bus_bufs, offset..frames, sink))
                else {
                    continue;
                };
                read_outcome = outcome;
                skip_tracks[next_idx] = true;

                if let Some(snapshot) = outcome_position_duration(&read_outcome) {
                    leading_outcome_pos_dur = Some(snapshot);
                }

                handover = next_handover(&read_outcome, offset);
            }

            if let Some(handoff) = handover
                && handoff.offset < frames
            {
                let offset = handoff.offset;
                for (next_arena_idx, (next_handle, next_state)) in order.loaded.iter().enumerate() {
                    if *next_state != TrackState::Preloading || active_slots[next_arena_idx] {
                        continue;
                    }

                    let Some(next_track) = tracks.at_mut(*next_handle) else {
                        continue;
                    };
                    next_track.play();
                    next_track.render(context, read_bufs, bus_bufs, offset..frames, sink);
                    break;
                }
            }
        }
    }

    (playback_started, leading_outcome_pos_dur)
}

fn return_settled_tail(sync: &mut SyncRender<'_>) {
    if !sync.tail.as_ref().is_some_and(SyncFadeTail::settled) || sync.returns.vacant_len() == 0 {
        return;
    }
    let Some(tail) = sync.tail.take() else {
        return;
    };
    if sync.returns.try_push(SyncReturn::Tail(tail)).is_err() {
        unreachable!("sole sync return producer retained its vacancy");
    }
}

fn reject_sync(sync: &mut SyncRender<'_>, reason: SyncExecutionReject) {
    if sync.returns.vacant_len() == 0 {
        return;
    }
    let Some(ticket) = sync.pending.try_peek() else {
        return;
    };
    let Some(receipts) = sync.receipts.as_mut() else {
        return;
    };
    if !receipts.publish_rejected(ticket.permit.stamp(), reason) {
        return;
    }
    let Some(ticket) = sync.pending.try_pop() else {
        return;
    };
    if sync.returns.try_push(SyncReturn::Ticket(ticket)).is_err() {
        unreachable!("sole sync return producer retained its vacancy");
    }
}

fn retire_withdrawn_sync(sync: &mut SyncRender<'_>) {
    if sync.returns.vacant_len() == 0 {
        return;
    }
    let Some(ticket) = sync.pending.try_pop() else {
        return;
    };
    if sync.returns.try_push(SyncReturn::Ticket(ticket)).is_err() {
        unreachable!("sole sync return producer retained its vacancy");
    }
}

fn claim_rejection(error: ClaimError) -> SyncExecutionReject {
    match error {
        ClaimError::Busy => SyncExecutionReject::Late,
        ClaimError::WrongMember => SyncExecutionReject::Geometry,
        ClaimError::Closed | ClaimError::CellRetired | ClaimError::StalePermit => {
            SyncExecutionReject::Cancelled
        }
    }
}

fn render_sync_activation(
    context: Option<&RenderContext>,
    frames: usize,
    tracks: &mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    sync: &mut SyncRender<'_>,
    read_bufs: &mut [&mut [f32]],
    bus_bufs: &mut [&mut [f32]],
    sink: &mut RtSink<'_>,
) -> SyncAttempt {
    let Some(ticket) = sync.pending.try_peek() else {
        return SyncAttempt::None;
    };
    if !ticket.gate.still_permits(&ticket.permit) {
        retire_withdrawn_sync(sync);
        return SyncAttempt::None;
    }
    let Some(context) = context else {
        reject_sync(sync, SyncExecutionReject::Geometry);
        return SyncAttempt::None;
    };
    let output = context.output();
    let start = i64::from(output.output_frames().start);
    let end = i64::from(output.output_frames().end);
    if output.session_epoch() != ticket.epoch {
        reject_sync(sync, SyncExecutionReject::Cancelled);
        return SyncAttempt::None;
    }
    if ticket
        .permit
        .stamp()
        .output_transport()
        .is_some_and(|revision| output.transport_revision() != Some(revision))
    {
        return SyncAttempt::None;
    }
    let activation = i64::from(ticket.activation);
    if activation >= end {
        return SyncAttempt::None;
    }
    if activation < start {
        reject_sync(sync, SyncExecutionReject::Late);
        return SyncAttempt::None;
    }
    let Ok(offset) = usize::try_from(activation - start) else {
        reject_sync(sync, SyncExecutionReject::Geometry);
        return SyncAttempt::None;
    };
    if offset >= frames || sync.tail.is_some() || sync.returns.vacant_len() < 2 {
        reject_sync(sync, SyncExecutionReject::Capacity);
        return SyncAttempt::None;
    }
    let source = ticket.first.source;
    if output.sample_rate() != ticket.output_rate
        || source.start() != ticket.source_start
        || source.output_frames() != 1
        || source.mapping_revision().map(WarpMapRevision::from) != Some(ticket.map)
        || !tracks.get(ticket.item_id).is_some_and(|track| {
            track.load() == Some(ticket.load)
                && track.state().is_leading()
                && track.output_sample_rate() == ticket.output_rate.get()
        })
    {
        reject_sync(sync, SyncExecutionReject::Geometry);
        return SyncAttempt::None;
    }
    claim_and_render_sync(
        context,
        offset..frames,
        tracks,
        sync,
        read_bufs,
        bus_bufs,
        sink,
    )
}

/// Reserve both receipts and the old prefix before claiming the first frame.
fn claim_and_render_sync(
    context: &RenderContext,
    window: Range<usize>,
    tracks: &mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    sync: &mut SyncRender<'_>,
    read_bufs: &mut [&mut [f32]],
    bus_bufs: &mut [&mut [f32]],
    sink: &mut RtSink<'_>,
) -> SyncAttempt {
    let offset = window.start;
    let frames = window.end;
    let Some(first_context) = context.for_output_range(offset..offset + 1) else {
        reject_sync(sync, SyncExecutionReject::Geometry);
        return SyncAttempt::None;
    };
    if context.for_output_range(offset..frames).is_none() {
        reject_sync(sync, SyncExecutionReject::Geometry);
        return SyncAttempt::None;
    }
    let Some(ticket) = sync.pending.try_peek() else {
        return SyncAttempt::None;
    };
    let stamp = ticket.permit.stamp();
    let source = ticket.first.source;
    let applied = SyncApplied::builder()
        .stamp(stamp)
        .frontier(
            PresentationFrontier::builder()
                .source(source.end())
                .output(first_context.output().output_frames().end)
                .build()
                .with_warp_map(Some(ticket.map)),
        )
        .build();
    let Some(receipts) = sync.receipts.as_mut() else {
        return SyncAttempt::None;
    };
    let Some(reservation) = receipts.reserve_pair(stamp, applied) else {
        return SyncAttempt::None;
    };
    let gate = ticket.gate.clone();
    let permit = ticket.permit;
    let item_id = ticket.item_id;

    // The ordinary resident serves the prefix before the claim. A losing
    // claim resumes that resident at offset without replaying prefix PCM.
    let prefix = if offset > 0 {
        let Some(track) = tracks.get_mut(item_id) else {
            unreachable!("the preflighted resident disappeared during one callback");
        };
        Some(track.render(Some(context), read_bufs, bus_bufs, 0..offset, sink))
    } else {
        None
    };
    if !tracks
        .get(item_id)
        .is_some_and(|track| track.state().is_leading())
        || prefix
            .as_ref()
            .is_some_and(|outcome| !matches!(outcome, TrackReadOutcome::Full { .. }))
    {
        drop(reservation);
        reject_sync(sync, SyncExecutionReject::Late);
        return SyncAttempt::PrefixRendered {
            item_id,
            offset,
            outcome: prefix,
        };
    }
    let claim = match gate.arbiter().try_claim(&permit, gate.cell()) {
        Ok(claim) => claim,
        Err(ClaimError::StalePermit | ClaimError::CellRetired) => {
            drop(reservation);
            retire_withdrawn_sync(sync);
            return SyncAttempt::PrefixRendered {
                item_id,
                offset,
                outcome: prefix,
            };
        }
        Err(error) => {
            drop(reservation);
            reject_sync(sync, claim_rejection(error));
            return SyncAttempt::PrefixRendered {
                item_id,
                offset,
                outcome: prefix,
            };
        }
    };

    // The sole consumer keeps this one-ticket ring occupied until the claim.
    let Some(ticket) = sync.pending.try_pop() else {
        unreachable!("the claimed sync ticket was removed without a consumer");
    };
    let SyncTicket {
        item_id,
        resource,
        first,
        output_rate,
        map,
        ..
    } = ticket;
    let Some(track) = tracks.get_mut(item_id) else {
        unreachable!("the preflighted resident disappeared during one callback");
    };
    let mut old = track.activate_sync(resource, first.source, map, output_rate, SYNC_FADE);
    track.render_first(&first, &first_context, read_bufs, bus_bufs, offset, sink);
    // Release-store after first PCM and before receipts makes the selected
    // binding visible whenever Host consumes Presented.
    sync.playback
        .active_sync_map
        .store(u64::from(map), Ordering::Release);
    reservation.publish();
    claim.finish_after_receipts();

    // Tail I/O and the remaining new-lane read happen after claim release.
    old.render(context, read_bufs, bus_bufs, offset..frames, sink.metrics());
    let (outcome, handover_offset) = if offset + 1 < frames {
        let suffix = track.render(Some(context), read_bufs, bus_bufs, offset + 1..frames, sink);
        extend_outcome(offset + 1, suffix)
    } else {
        (
            TrackReadOutcome::Full {
                position: track.position(),
                frames: offset + 1,
                duration: track.duration(),
                frames_until_eof: track.frames_until_eof(),
            },
            None,
        )
    };
    *sync.tail = Some(old);
    return_settled_tail(sync);
    SyncAttempt::Claimed {
        item_id,
        outcome: Some(outcome),
        handover_offset,
    }
}

/// Account for the PCM already rendered before a suffix read, so the normal
/// leading-track handover code sees one block-relative outcome.
fn extend_outcome(base: usize, suffix: TrackReadOutcome) -> (TrackReadOutcome, Option<usize>) {
    match suffix {
        TrackReadOutcome::Full {
            position,
            frames,
            duration,
            frames_until_eof,
        } => (
            TrackReadOutcome::Full {
                position,
                frames: base.saturating_add(frames),
                duration,
                frames_until_eof,
            },
            None,
        ),
        TrackReadOutcome::Partial { frames, duration } => (
            TrackReadOutcome::Partial {
                frames: base.saturating_add(frames),
                duration,
            },
            None,
        ),
        other @ (TrackReadOutcome::Eof | TrackReadOutcome::Failed(_)) if base > 0 => {
            (other, Some(base))
        }
        other => (other, None),
    }
}

const fn initial_handover(read_outcome: &TrackReadOutcome) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Partial { frames, .. } => Some(Handover { offset: *frames }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => Some(Handover { offset: 0 }),
        TrackReadOutcome::Full { .. } => None,
    }
}

const fn next_handover(read_outcome: &TrackReadOutcome, offset: usize) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Full { .. } => None,
        TrackReadOutcome::Partial { frames, .. } => Some(Handover {
            offset: offset.saturating_add(*frames),
        }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => Some(Handover { offset }),
    }
}

const fn outcome_position_duration(outcome: &TrackReadOutcome) -> Option<(f64, f64)> {
    match *outcome {
        TrackReadOutcome::Full {
            position, duration, ..
        } => Some((position, duration)),
        TrackReadOutcome::Partial { duration, .. } => Some((duration, duration)),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => None,
    }
}

pub(super) const fn eviction_priority(state: TrackState) -> u8 {
    const EVICT_PRELOADING: u8 = 2;
    const EVICT_FADING_IN: u8 = 3;
    const EVICT_PLAYING: u8 = 4;

    match state {
        TrackState::Finished => 0,
        TrackState::FadingOut => 1,
        TrackState::Preloading => EVICT_PRELOADING,
        TrackState::FadingIn => EVICT_FADING_IN,
        TrackState::Playing => EVICT_PLAYING,
    }
}

#[cfg(test)]
mod sync_tests {
    use std::{
        num::{NonZeroU32, NonZeroU64, NonZeroUsize},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_audio::{
        ReadOutcome as AudioReadOutcome,
        mock::{AudioControlMock, AudioReadMock, AudioSessionMock},
    };
    use kithara_decode::DecodeError;
    use kithara_events::EventBus;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{
        AudioSpec, OutputContext, SessionEpoch, SessionFrame, SourceSpan, TransportRevision,
    };
    use kithara_sync::{LoadGeneration, PermitCell, SyncArbiter, SyncGateBinding, SyncReceipt};
    use kithara_test_utils::kithara;
    use kithara_warp::{BeatGridId, RenderContext};
    use ringbuf::{
        HeapRb,
        traits::{Producer, Split},
    };
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        bridge::{
            PlaybackShared,
            sync::{PreparedFirst, sync_receipts},
        },
        resource::Resource,
        rt::sync_owner_fixture::prepared_entry,
        test_pools::pools,
    };

    #[derive(Clone, Copy, Debug)]
    enum ReaderMode {
        Silence,
        Eof,
        Failure,
        ShortThenEof,
    }

    fn resource(
        mode: ReaderMode,
        rate: NonZeroU32,
        read_required: bool,
        track: bool,
        on_read: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Box<super::super::track::PlayerResource> {
        let calls = AtomicUsize::new(0);
        let event_bus = AudioSessionMock::event_bus
            .each_call(matching!())
            .answers(&|mock| mock.make_ref(EventBus::new(1)));
        let spec = AudioReadMock::spec
            .each_call(matching!())
            .returns(AudioSpec::new(2, rate));
        let preload = AudioControlMock::preload
            .next_call(matching!())
            .returns(Ok(()));
        let reader = if read_required {
            let read = AudioReadMock::read_planar
                .each_call(matching!())
                .answers_arc(Arc::new(move |_, output| {
                    if let Some(on_read) = &on_read {
                        on_read();
                    }
                    match mode {
                        ReaderMode::Silence => {
                            let frames = output[0].len();
                            for channel in output.iter_mut() {
                                channel.fill(0.0);
                            }
                            Ok(AudioReadOutcome::Frames {
                                count: NonZeroUsize::new(frames).expect("nonempty callback read"),
                                position: Duration::ZERO,
                                source_span: None,
                            })
                        }
                        ReaderMode::Eof => Ok(AudioReadOutcome::Eof {
                            position: Duration::ZERO,
                        }),
                        ReaderMode::Failure => Err(DecodeError::InvalidData {
                            detail: "fixture fault",
                        }),
                        ReaderMode::ShortThenEof if calls.fetch_add(1, Ordering::Relaxed) == 0 => {
                            let count = output[0].len().min(16);
                            for channel in output.iter_mut() {
                                channel[..count].fill(0.5);
                            }
                            Ok(AudioReadOutcome::Frames {
                                count: NonZeroUsize::new(count).expect("fixture read has space"),
                                position: Duration::ZERO,
                                source_span: None,
                            })
                        }
                        ReaderMode::ShortThenEof => Ok(AudioReadOutcome::Eof {
                            position: Duration::ZERO,
                        }),
                    }
                }));
            if track {
                let duration = AudioSessionMock::duration
                    .each_call(matching!())
                    .returns(Some(Duration::from_secs(1)));
                Unimock::new((event_bus, duration, spec, preload, read))
            } else {
                Unimock::new((event_bus, spec, preload, read))
            }
        } else if track {
            let duration = AudioSessionMock::duration
                .each_call(matching!())
                .returns(Some(Duration::from_secs(1)));
            Unimock::new((event_bus, duration, spec, preload))
        } else {
            Unimock::new((event_bus, spec, preload))
        };
        let src: Arc<str> = Arc::from("fixture");
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        Box::new(
            super::super::track::PlayerResource::new(resource, src, &pools())
                .expect("fixture resource fits pool"),
        )
    }

    fn activation_with_output(
        old_mode: ReaderMode,
        new_mode: ReaderMode,
        dependency: Option<TransportRevision>,
        actual_revision: Option<TransportRevision>,
        output_start: i64,
        revoke_during_prefix: bool,
    ) -> (SyncAttempt, [f32; 128], Vec<SyncReceipt>, bool) {
        let rate = NonZeroU32::new(48_000).expect("fixture rate");
        let can_reach_activation = output_start < 32
            && dependency.is_none_or(|revision| actual_revision == Some(revision));
        let new_read_required = can_reach_activation
            && !revoke_during_prefix
            && matches!(old_mode, ReaderMode::Silence);
        let new_duration_required = new_read_required && matches!(new_mode, ReaderMode::Silence);
        let item_id = TrackId::allocate();
        let load = LoadGeneration::first();
        let member = BeatGridId::allocate().expect("member id");
        let group = BeatGridId::allocate().expect("group id");
        let (stamp, map) = prepared_entry(
            member,
            group,
            load,
            TransportRevision::first(),
            dependency,
            rate,
        );
        let arbiter = Arc::new(SyncArbiter::new());
        let cell = Arc::new(PermitCell::new(member));
        let owner = arbiter.try_control().expect("owner phase");
        let permit = owner.mint_permit(&cell, stamp).expect("exact permit");
        drop(owner);
        let revoke: Option<Arc<dyn Fn() + Send + Sync>> = revoke_during_prefix.then(|| {
            let arbiter = Arc::clone(&arbiter);
            let cell = Arc::clone(&cell);
            Arc::new(move || {
                let owner = arbiter
                    .try_control()
                    .expect("prefix runs before an audio claim");
                owner
                    .preflight_revoke(&cell)
                    .expect("fixture permit revision")
                    .revoke();
            }) as Arc<dyn Fn() + Send + Sync>
        });
        let gate = SyncGateBinding::new(arbiter, cell);
        let first = PreparedFirst {
            stereo: [0.0, 0.0],
            source: SourceSpan::new(0, 1, rate, 1)
                .expect("first source span")
                .with_mapping_revision(Some(NonZeroU64::MIN)),
        };
        let ticket = SyncTicket {
            item_id,
            load,
            resource: resource(
                new_mode,
                rate,
                new_read_required,
                new_duration_required,
                None,
            ),
            first,
            permit,
            gate,
            activation: SessionFrame::new(32),
            source_start: 0,
            epoch: SessionEpoch::new(1),
            output_rate: rate,
            map,
        };
        let mut track = super::super::track::PlayerTrack::builder()
            .sample_rate(rate)
            .item_id(item_id)
            .load(load)
            .build(resource(old_mode, rate, can_reach_activation, true, revoke));
        track.play();
        let mut tracks = TrackSlots::<{ PlayerNodeProcessor::MAX_TRACKS }>::default();
        assert!(tracks.insert(track).is_none());
        let (mut ticket_tx, mut pending) = HeapRb::<SyncTicket>::new(1).split();
        assert!(ticket_tx.try_push(ticket).is_ok());
        let (mut notification_tx, _notification_rx) = HeapRb::<PlayerNotification>::new(16).split();
        let (mut returns, _return_rx) = HeapRb::<SyncReturn>::new(2).split();
        let (receipt_tx, mut receipt_rx) = sync_receipts();
        let mut receipts = Some(receipt_tx);
        let mut tail = None;
        let playback = PlaybackShared::default();
        let metrics = RtMetrics::default();
        let output = OutputContext::new(
            SessionFrame::new(output_start)..SessionFrame::new(output_start + 128),
            rate,
            SessionEpoch::new(1),
            actual_revision,
        )
        .expect("fixture output");
        let context = RenderContext::new_linear(output, None).expect("fixture context");
        let mut read_left = [0.0; 128];
        let mut read_right = [0.0; 128];
        let mut bus_left = [0.0; 128];
        let mut bus_right = [0.0; 128];
        let mut read = [&mut read_left[..], &mut read_right[..]];
        let mut bus = [&mut bus_left[..], &mut bus_right[..]];
        let mut sink = RtSink::new(&mut notification_tx, &metrics, 0);
        let attempt = render_sync_activation(
            Some(&context),
            128,
            &mut tracks,
            &mut SyncRender {
                pending: &mut pending,
                tail: &mut tail,
                receipts: &mut receipts,
                returns: &mut returns,
                playback: &playback,
            },
            &mut read,
            &mut bus,
            &mut sink,
        );
        if matches!(&attempt, SyncAttempt::Claimed { .. }) {
            assert_eq!(
                playback.active_sync_map.load(Ordering::Acquire),
                u64::from(map)
            );
        }
        let mut delivered = Vec::new();
        while let Some(receipt) = receipt_rx.try_pop() {
            delivered.push(receipt);
        }
        let pending_remains = pending.try_peek().is_some();
        (attempt, bus_left, delivered, pending_remains)
    }

    fn activation(
        old_mode: ReaderMode,
        new_mode: ReaderMode,
    ) -> (SyncAttempt, [f32; 128], Vec<SyncReceipt>) {
        let (attempt, pcm, receipts, _) = activation_with_output(
            old_mode,
            new_mode,
            Some(TransportRevision::first()),
            Some(TransportRevision::first()),
            0,
            false,
        );
        (attempt, pcm, receipts)
    }

    #[kithara::test]
    fn host_ticket_waits_for_its_processed_revision_before_late_check() {
        let later = TransportRevision::first().checked_next().expect("revision");
        let (attempt, pcm, receipts, pending) = activation_with_output(
            ReaderMode::Silence,
            ReaderMode::Silence,
            Some(TransportRevision::first()),
            Some(later),
            128,
            false,
        );
        assert!(matches!(attempt, SyncAttempt::None));
        assert!(
            pending,
            "the owner must reissue or withdraw this parked ticket"
        );
        assert!(receipts.is_empty());
        assert_eq!(pcm, [0.0; 128]);
    }

    #[kithara::test]
    fn output_independent_ticket_can_claim_after_a_global_revision_change() {
        let later = TransportRevision::first().checked_next().expect("revision");
        let (attempt, _, receipts, pending) = activation_with_output(
            ReaderMode::Silence,
            ReaderMode::Silence,
            None,
            Some(later),
            0,
            false,
        );
        assert!(matches!(attempt, SyncAttempt::Claimed { .. }));
        assert!(!pending);
        assert_first_span_pair(&receipts);
    }

    #[kithara::test]
    fn owner_withdrawal_during_the_old_prefix_returns_ticket_without_stale_receipt() {
        let (attempt, _, receipts, pending) = activation_with_output(
            ReaderMode::Silence,
            ReaderMode::Silence,
            None,
            Some(TransportRevision::first()),
            0,
            true,
        );
        assert!(matches!(
            attempt,
            SyncAttempt::PrefixRendered { offset: 32, .. }
        ));
        assert!(!pending, "the withdrawn ticket leaves the callback ring");
        assert!(
            receipts.is_empty(),
            "the owner already withdrew this exact preparation"
        );
    }

    fn assert_first_span_pair(receipts: &[SyncReceipt]) {
        let [SyncReceipt::Armed(stamp), SyncReceipt::Presented(applied)] = receipts else {
            panic!("one consumed first frame must publish exactly Armed and Presented");
        };
        assert_eq!(*stamp, applied.stamp());
        assert_eq!(applied.frontier().source(), 1);
        assert_eq!(applied.frontier().output(), SessionFrame::new(33));
    }

    #[kithara::test]
    fn suffix_decode_failure_keeps_failure_position_and_exact_receipt_pair() {
        let (attempt, pcm, receipts) = activation(ReaderMode::Silence, ReaderMode::Failure);
        let SyncAttempt::Claimed {
            outcome: Some(outcome),
            handover_offset,
            ..
        } = attempt
        else {
            panic!("first PCM must claim");
        };
        assert!(matches!(outcome, TrackReadOutcome::Failed(_)));
        assert_eq!(handover_offset, Some(33));
        assert_eq!(outcome_position_duration(&outcome), None);
        assert_eq!(pcm, [0.0; 128]);
        assert_first_span_pair(&receipts);
    }

    #[kithara::test]
    #[case(ReaderMode::Eof)]
    #[case(ReaderMode::Failure)]
    fn preclaim_prefix_end_keeps_old_audio_and_never_presents(#[case] old: ReaderMode) {
        let (attempt, pcm, receipts) = activation(old, ReaderMode::Silence);
        assert!(matches!(attempt, SyncAttempt::PrefixRendered { .. }));
        assert_eq!(pcm, [0.0; 128]);
        assert!(matches!(
            receipts.as_slice(),
            [SyncReceipt::Rejected { .. }]
        ));
    }

    #[kithara::test]
    fn preclaim_prefix_partial_keeps_existing_pcm_without_claim() {
        let (attempt, pcm, receipts) = activation(ReaderMode::ShortThenEof, ReaderMode::Silence);
        assert!(matches!(
            attempt,
            SyncAttempt::PrefixRendered {
                outcome: Some(TrackReadOutcome::Partial { frames: 16, .. }),
                ..
            }
        ));
        assert!(pcm[..16].iter().all(|&sample| sample == 0.5));
        assert!(pcm[16..].iter().all(|&sample| sample == 0.0));
        assert!(matches!(
            receipts.as_slice(),
            [SyncReceipt::Rejected { .. }]
        ));
    }

    #[kithara::test]
    fn suffix_eof_keeps_handover_after_consumed_first_frame() {
        let (attempt, _, receipts) = activation(ReaderMode::Silence, ReaderMode::Eof);
        let SyncAttempt::Claimed {
            outcome: Some(outcome),
            handover_offset,
            ..
        } = attempt
        else {
            panic!("first PCM must claim");
        };
        assert!(matches!(outcome, TrackReadOutcome::Eof));
        assert_eq!(handover_offset, Some(33));
        assert_eq!(outcome_position_duration(&outcome), None);
        assert_first_span_pair(&receipts);
    }
}
