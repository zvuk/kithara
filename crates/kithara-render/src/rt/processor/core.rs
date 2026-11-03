use std::{marker::PhantomData, num::NonZeroU32, ops::Range};

use firewheel::{
    StreamInfo,
    node::{AudioNodeProcessor, NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_command::{LevelInbox, ScopeId, Target};
use kithara_dsp::param::SmootherConfig;
use kithara_effects::EffectPrepareError;
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use ringbuf::HeapProd;
use triple_buffer::Input;

use super::{
    super::{
        command::{Armed, answer_stop},
        context::read_render_context,
        tail::SlotTail,
        track::{PlayerTrack, RtSink},
    },
    shape::StreamShape,
};
use crate::{
    bridge::{
        DeckEvent, DeckMixSettings, DeckProtocol, DeckSnapshot, MixerInputs, RtMetrics,
        SessionInbox, Slot, SlotSnapshot, SlotState,
    },
    rt::{RenderPass, TrackSlots},
};

/// The realtime executor of one generation of a session's deck scope.
pub struct DeckMixer<E: SessionInbox> {
    pub(super) scope: ScopeId,
    pub(super) deck: Deck,
    scratch: [SampleBuffer; crate::consts::MIN_STEREO],
    capacity: usize,
    snapshot: Input<DeckSnapshot>,
    blocks: u64,
    recycle_per_block: usize,
    retired: bool,
    session: PhantomData<fn() -> E>,
}

pub(in crate::rt) struct Deck {
    pub(in crate::rt) tracks: TrackSlots,
    pub(in crate::rt) armed: Vec<Option<Armed>>,
    pub(in crate::rt) tails: Vec<SlotTail>,
    pub(in crate::rt) held: Vec<Option<SegmentId>>,
    pub(in crate::rt) recycle: Vec<usize>,
    pub(in crate::rt) declick_frames: usize,
    pub(in crate::rt) evict_frames: usize,
    pub(in crate::rt) mix: DeckMixSettings,
    pub(in crate::rt) render: RenderPass,
    events: HeapProd<DeckEvent>,
    pub(in crate::rt) metrics: RtMetrics,
    pub(in crate::rt) sample_rate: NonZeroU32,
    pub(in crate::rt) declick: SmootherConfig,
}

pub(in crate::rt) struct PreparedDeck {
    shape: StreamShape,
    scratch: [SampleBuffer; crate::consts::MIN_STEREO],
    tracks: TrackSlots,
    armed: Vec<Option<Armed>>,
    tails: Vec<SlotTail>,
    held: Vec<Option<SegmentId>>,
    recycle: Vec<usize>,
    render: RenderPass,
}

impl<E: SessionInbox> DeckMixer<E> {
    /// Allocates every slot tail and the fixed render scratch before stream processing.
    /// The scope snapshot stays unchanged until the first actual render.
    ///
    /// # Errors
    /// Returns the design or pool refusal before stream processing.
    pub fn new<S>(
        inputs: MixerInputs,
        shape: StreamShape,
        pools: &PoolRegion<S>,
    ) -> Result<Self, EffectPrepareError>
    where
        S: HasPool<f32>,
    {
        let prepared = Self::prepare(&inputs, shape, pools)?;
        Ok(Self::assemble(inputs, prepared))
    }

    pub(in crate::rt) fn prepare<S>(
        inputs: &MixerInputs,
        shape: StreamShape,
        pools: &PoolRegion<S>,
    ) -> Result<PreparedDeck, EffectPrepareError>
    where S: HasPool<f32>,
    {
        let config = inputs.config;
        let slots = config.slots();
        let capacity = usize::try_from(shape.max_block_frames.get()).unwrap_or(usize::MAX);
        let tail_frames = FrameCount::new(config.declick_frames(shape.sample_rate).get().max(config.evict_fade().get()));
        Ok(PreparedDeck {
            shape,
            tails: (0..slots.get())
                .map(|_| SlotTail::new(pools, tail_frames))
                .collect::<Result<Vec<_>, _>>()?,
            scratch: [pools.get_with_len::<f32>(capacity)?, pools.get_with_len::<f32>(capacity)?],
            render: RenderPass::new(pools, shape, config.mix().gain(), config)?,
            tracks: TrackSlots::new(slots),
            armed: vec![None; slots.get()],
            held: vec![None; slots.get()],
            recycle: vec![0; slots.get()],
        })
    }

    pub(in crate::rt) fn assemble(inputs: MixerInputs, prepared: PreparedDeck) -> Self {
        let shape = prepared.shape;
        let MixerInputs {
            scope,
            events,
            snapshot,
            config,
        } = inputs;
        let capacity = usize::try_from(shape.max_block_frames.get()).unwrap_or(usize::MAX);
        let declick_frames = config.declick_frames(shape.sample_rate).get();
        let evict_frames = config.evict_fade().get();
        let mix = config.mix();
        Self {
            scope,
            snapshot,
            scratch: prepared.scratch,
            capacity,
            blocks: 0,
            recycle_per_block: config.recycle_per_block().get(),
            retired: false,
            session: PhantomData,
            deck: Deck {
                tracks: prepared.tracks,
                armed: prepared.armed,
                tails: prepared.tails,
                held: prepared.held,
                recycle: prepared.recycle,
                declick_frames,
                evict_frames,
                render: prepared.render,
                mix,
                events,
                metrics: RtMetrics::default(),
                sample_rate: shape.sample_rate,
                declick: config.declick(),
            },
        }
    }

    #[must_use]
    pub fn track(&self, slot: Slot) -> Option<&PlayerTrack> {
        if self.retired {
            None
        } else {
            self.deck.tracks.at(slot)
        }
    }

    pub(super) fn render_block(
        &mut self,
        level: Option<LevelInbox<'_, DeckProtocol>>,
        context: Option<&RenderContext>,
        start: SessionFrame,
        buffers: &mut ProcBuffers,
        frames: usize,
    ) -> bool {
        for channel in buffers.outputs.iter_mut() {
            channel.fill(0.0);
        }
        let mut sounded = false;
        if !self.retired {
            if let Some(mut level) = level {
                sounded = self.render_block_in(&mut level, context, start, buffers, frames);
                if level.is_closing() && self.deck.tails_quiet() {
                    level.retire();
                    self.retired = true;
                }
            } else {
                self.retired = true;
            }
            if self.retired {
                for (_, track) in self.deck.tracks.iter_mut() {
                    track.shut();
                }
            }
        }
        self.blocks = self.blocks.saturating_add(1);
        let end = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(frames).unwrap_or(i64::MAX)),
        );
        self.publish(end);
        sounded
    }

    pub(super) fn render_block_in(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        context: Option<&RenderContext>,
        start: SessionFrame,
        buffers: &mut ProcBuffers,
        frames: usize,
    ) -> bool {
        self.deck.recycle.fill(self.recycle_per_block);
        self.deck.arrivals(level);
        let mut sounded = false;
        let mut cursor = 0;
        while cursor < frames {
            let at = SessionFrame::new(
                i64::from(start).saturating_add(i64::try_from(cursor).unwrap_or(i64::MAX)),
            );
            loop {
                let Some(due) = level.next_due(start, cursor.saturating_add(1)) else {
                    break;
                };
                self.deck.take_due(due, context.is_some());
                self.deck.resolve_armed(level, start, at);
            }
            self.deck.maintain();
            if context.is_some() {
                self.deck.observe_ends(cursor, start);
            }
            self.deck.fire_ended(level, start, at, context.is_some());
            let command_frames = level
                .frames_until_due(at)
                .and_then(|count| usize::try_from(count).ok())
                .unwrap_or(frames - cursor);
            let span = self
                .deck
                .tracks
                .iter_mut()
                .filter_map(|(_, track)| track.frames_until_boundary(at))
                .fold((frames - cursor).min(command_frames), usize::min)
                .max(1);
            let end = cursor.saturating_add(span).min(frames);
            let end = if cursor < self.capacity {
                end.min(self.capacity)
            } else {
                end
            };
            sounded |= self.deck.render_range(
                context,
                buffers,
                &mut self.scratch,
                self.capacity,
                cursor..end,
                start,
            );
            self.deck.advance(cursor..end, start, level);
            cursor = end;
        }
        self.deck.maintain();
        sounded
    }

    pub(super) fn publish(&mut self, at: SessionFrame) {
        let snapshot = self.snapshot.input_buffer_mut();
        for (entry, slot) in snapshot.slots.iter_mut().zip(self.deck.tracks.slots()) {
            *entry = if self.retired {
                SlotSnapshot::default()
            } else {
                self.deck
                    .tracks
                    .at(slot)
                    .map_or_else(SlotSnapshot::default, |track| slot_snapshot(track, at))
            };
        }
        snapshot.sample_rate = self.deck.sample_rate.get();
        snapshot.blocks = self.blocks;
        snapshot.metrics = self.deck.metrics.snapshot();
        let bands = self.deck.render.read_eq(snapshot.eq.gains_mut());
        snapshot.eq.set_bands(bands);
        self.snapshot.publish();
    }
}

fn slot_snapshot(track: &PlayerTrack, at: SessionFrame) -> SlotSnapshot {
    let position = track.position();
    SlotSnapshot {
        state: track.state(),
        mark: track.mark(at),
        position,
        source: track.source_info(),
        frontier: track.decoded_frontier().max(position),
        cached: track.cached_span(),
        gain: track.gain(),
    }
}

impl Deck {
    pub(in crate::rt) fn finish_underrun(&mut self, slot: Slot, at: SessionFrame) {
        if let Some(track) = self.tracks.at_mut(slot) {
            let mut sink = RtSink::new(&mut self.events, &self.metrics, slot, at);
            track.finish_underrun(0, &mut sink);
        }
    }

    fn advance(
        &mut self,
        range: Range<usize>,
        start: SessionFrame,
        level: &mut LevelInbox<'_, DeckProtocol>,
    ) {
        for (slot, track) in self.tracks.iter_mut() {
            let mut sink = RtSink::new(&mut self.events, &self.metrics, slot, start);
            track.advance(range.clone(), &mut sink);
            let Some((seq, resume)) = track.take_stop() else {
                continue;
            };
            if level
                .committed_mut(seq)
                .is_some_and(|commands| answer_stop(commands, slot, resume))
            {
                level.complete(seq, ());
            }
        }
    }

    fn maintain(&mut self) {
        for (slot, track) in self.tracks.iter_mut() {
            track.recycle_obsolete(&mut self.recycle[slot.index()]);
        }
    }

    fn observe_ends(&mut self, cursor: usize, start: SessionFrame) {
        for (slot, track) in self.tracks.iter_mut() {
            let mut sink = RtSink::new(&mut self.events, &self.metrics, slot, start);
            track.poll_end(cursor, &mut self.recycle[slot.index()], &mut sink);
        }
    }

    fn render_range(
        &mut self,
        context: Option<&RenderContext>,
        buffers: &mut ProcBuffers,
        scratch: &mut [SampleBuffer; crate::consts::MIN_STEREO],
        capacity: usize,
        range: Range<usize>,
        start: SessionFrame,
    ) -> bool {
        if context.is_none()
            || range.end > capacity
            || buffers.outputs.len() < crate::consts::MIN_STEREO
            || buffers
                .outputs
                .iter()
                .take(crate::consts::MIN_STEREO)
                .any(|channel| range.end > channel.len())
        {
            for tail in &mut self.tails {
                tail.advance(range.len());
            }
            return false;
        }
        if self.render.take_priming() {
            for (_, track) in self.tracks.iter_mut() {
                track.snap_gate();
            }
        }
        let [scratch_left, scratch_right] = scratch;
        let mut read = [
            &mut scratch_left[..capacity],
            &mut scratch_right[..capacity],
        ];
        let (out_left, out_right) = buffers.outputs.split_at_mut(1);
        let mut bus = [&mut out_left[0][..], &mut out_right[0][..]];
        let mut sounded = false;
        for (slot, track) in self.tracks.iter_mut() {
            if track.state() != SlotState::Playing {
                continue;
            }
            let mut sink = RtSink::new(&mut self.events, &self.metrics, slot, start);
            let outcome = track.render(
                context,
                &mut read,
                &mut bus,
                range.clone(),
                &mut self.recycle[slot.index()],
                &mut sink,
            );
            if let Some(offset) = outcome.ended_at(&range) {
                track.poll_end(offset, &mut self.recycle[slot.index()], &mut sink);
            }
            sounded = true;
        }
        let [left, right] = &mut bus;
        for tail in &mut self.tails {
            if tail.is_sounding() {
                tail.mix(left, right, range.clone());
                sounded = true;
            }
        }
        if sounded {
            self.render
                .finish(&mut left[range.clone()], &mut right[range]);
        } else {
            self.render.idle();
        }
        sounded
    }

    fn tails_quiet(&self) -> bool {
        self.tails.iter().all(|tail| !tail.is_sounding())
    }

    fn apply_prepared_host_sample_rate(&mut self) {
        let Some(sample_rate) = self.render.apply_prepared_sample_rate() else { return; };
        self.sample_rate = sample_rate;
        self.declick_frames =
            super::super::config::declick_frame_count(self.declick, sample_rate).get();
        for (_, track) in self.tracks.iter_mut() {
            track.set_host_sample_rate(sample_rate);
        }
    }
}

impl<E: SessionInbox> AudioNodeProcessor for DeckMixer<E> {
    fn prepare_new_stream(&mut self, stream_info: &StreamInfo) -> Result<(), NodeError> {
        self.deck.render.discard_prepared_sample_rate();
        let capacity = usize::try_from(stream_info.max_block_frames.get()).unwrap_or(usize::MAX);
        let declick_frames = super::super::config::declick_frame_count(self.deck.declick, stream_info.sample_rate).get();
        if capacity > self.capacity || self.deck.tails.iter().any(|tail| tail.capacity() < declick_frames.max(self.deck.evict_frames)) {
            return Err(NodeError(Box::new(EffectPrepareError::Shape)));
        }
        if self.deck.sample_rate != stream_info.sample_rate {
            self.deck.render.prepare_sample_rate(stream_info.sample_rate)
                .map_err(|error| NodeError(Box::new(EffectPrepareError::from(error))))?;
        }
        Ok(())
    }

    fn new_stream(&mut self, _stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.deck.apply_prepared_host_sample_rate();
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        mut buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let start = SessionFrame::new(info.clock_samples.0);
        let geometry_valid = info.frames <= self.capacity
            && buffers.outputs.len() >= crate::consts::MIN_STEREO
            && buffers
                .outputs
                .iter()
                .take(crate::consts::MIN_STEREO)
                .all(|channel| channel.len() >= info.frames)
            && self.deck.tails.iter().all(|tail| {
                tail.capacity() >= self.deck.declick_frames.max(self.deck.evict_frames)
            });
        let context = read_render_context(&extra.store, info)
            .ok()
            .filter(|_| geometry_valid)
            .cloned();
        let level = extra
            .store
            .try_get_mut::<E>()
            .and_then(|session| session.scope(self.scope));
        let sounded = self.render_block(level, context.as_ref(), start, &mut buffers, info.frames);
        if sounded {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}
