use std::{
    num::NonZeroU32,
    ops::ControlFlow,
    task::{Context, Poll},
};

use kithara_audio::{
    AudioReadError, AudioSource, Fetch, SeekOutcome, SourceDiscontinuity, SourceEnd,
    TrackFailureKind, TrackStep, WaitingReason,
};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_effects::{AudioEffect, EffectDrain, EffectDrainStep, apply_effects, reset_effects};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount};
use kithara_warp::WarpRenderError;

use crate::{
    LaneFrame, LaneSetup,
    lane::{Lane, LaneChange},
};

#[derive(Clone, Copy)]
pub(super) enum DrainState {
    Open,
    LiveWarp,
    Warp,
    Effects,
    Exhausted,
}

pub(super) struct PendingInput {
    pub(super) chunk: AudioChunk,
    pub(super) consumed_frames: usize,
}

/// The sole producer-side Warp/effect stage before the play output ring.
pub struct WarpSource<T, S> {
    pub(super) spec: AudioSpec,
    pub(super) drain_state: DrainState,
    drain: EffectDrain,
    discontinuity: Option<SourceDiscontinuity>,
    lane: Lane,
    pub(super) pending_input: Option<PendingInput>,
    pub(super) prepared_frames: Option<usize>,
    pub(super) render_input: Option<SampleBuffer>,
    pub(super) retired_input: Option<AudioChunk>,
    staged_meta: Option<AudioChunkInfo>,
    pools: PoolRegion<S>,
    pub(super) source: T,
    pub(super) effects: Vec<Box<dyn AudioEffect>>,
    pub(super) warp: kithara_warp::WarpRenderer<S>,
    pub(super) quantum_failed: bool,
    terminal_failure: Option<TrackFailureKind>,
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    /// Builds the stage over a decoded source, its Warp renderer, and the
    /// effect chain with the drain that flushes it; `lane` brings the lane
    /// commands it executes at frames of its output.
    pub fn new(
        source: T,
        warp: kithara_warp::WarpRenderer<S>,
        effects: Vec<Box<dyn AudioEffect>>,
        drain: EffectDrain,
        spec: AudioSpec,
        pools: PoolRegion<S>,
        lane: LaneSetup,
    ) -> Self {
        let discontinuity = source.discontinuity();
        Self {
            source,
            warp,
            effects,
            drain,
            discontinuity,
            spec,
            pools,
            lane: Lane::new(lane.inbox, lane.preload_chunks, lane.declick),
            drain_state: DrainState::Open,
            pending_input: None,
            staged_meta: None,
            prepared_frames: None,
            render_input: None,
            retired_input: None,
            quantum_failed: false,
            terminal_failure: None,
        }
    }

    delegate::delegate! {
        to self.lane {
            /// Current segment-relative output frame.
            #[must_use]
            pub const fn cursor(&self) -> LaneFrame;
            /// Exact source position represented by the lane output cursor, when established.
            #[must_use]
            pub const fn position(&self) -> Option<Duration>;
            /// Records successful admission of the current segment's chunk.
            pub fn admitted(&mut self);
            pub(crate) fn upstream_parked(&mut self);
            pub(crate) fn finish_preload(&mut self);
            pub(crate) fn finish_segment(&mut self);
            /// Whether admission has made the current segment ready for playback.
            #[must_use]
            pub fn is_preloaded(&self) -> bool;
            /// Registers the owning task's wake for lane command arrivals.
            pub fn poll_commands(&mut self, context: &mut Context<'_>) -> Poll<()>;
        }
    }

    /// Exact output latency of the prepared engine.
    #[must_use]
    pub fn engine_latency(&self) -> FrameCount {
        self.warp.engine_latency()
    }

    /// Output frames of this lane's Jump ramp at its current sample rate.
    #[must_use]
    pub fn declick_frames(&self) -> FrameCount {
        self.lane.declick_frames(self.spec.sample_rate)
    }

    /// Executes commands at the output cursor before producing more samples.
    /// Returns whether a command reset the decoded source.
    ///
    /// # Errors
    /// Returns the source's seek classification or a render failure.
    pub fn service_commands(&mut self) -> Result<bool, TrackFailureKind> {
        let changed = self
            .lane
            .execute_due(&mut self.source, &mut self.warp, self.spec)?;
        if changed == LaneChange::Source {
            self.discard_staged_input();
            reset_effects(&mut self.effects);
            self.drain.reset();
            self.drain_state = DrainState::Open;
            self.discontinuity = self.source.discontinuity();
            self.spec = self.discontinuity.map_or(self.spec, |stamp| *stamp.spec());
        } else if changed == LaneChange::Controls {
            self.prepared_frames = None;
        }
        self.prepare_renderers(self.spec);
        Ok(changed == LaneChange::Source)
    }

    fn fail(&mut self, failure: TrackFailureKind) -> TrackStep<AudioChunk> {
        TrackStep::Failed(*self.terminal_failure.get_or_insert(failure))
    }

    fn clear_staging(&mut self) {
        if let Some(input) = self.render_input.as_mut() {
            input.clear();
        }
        self.staged_meta = None;
        self.prepared_frames = None;
    }

    fn discard_staged_input(&mut self) {
        self.retire_pending_input();
        self.clear_staging();
        self.quantum_failed = false;
    }

    pub(super) fn prepare_staging(&mut self) {
        if self.quantum_failed || self.lane.output_limit() == 0 {
            return;
        }
        let pending_span = self.pending_input.as_ref().and_then(|pending| {
            let remaining = pending
                .chunk
                .frames()
                .checked_sub(pending.consumed_frames)?;
            Some((
                Self::span_meta(pending.chunk.meta, pending.consumed_frames, remaining)?,
                remaining,
            ))
        });
        let staged = self.staged_frames();
        let span = self.staged_meta.map_or(pending_span, |meta| {
            Some((
                meta,
                staged.saturating_add(pending_span.map_or(0, |(_, remaining)| remaining)),
            ))
        });
        let Some((meta, remaining)) = span else {
            return;
        };
        let frames = match self.prepare_quantum(meta, remaining) {
            Ok(frames) => frames,
            Err(WarpRenderError::NeedsService) => {
                if self.warp.transition_pending() {
                    self.drain_state = DrainState::LiveWarp;
                }
                return;
            }
            Err(_) => {
                self.quantum_failed = true;
                return;
            }
        };
        let frames = frames.get();
        let Some(required) = frames.checked_mul(usize::from(self.spec.channels.max(1))) else {
            self.quantum_failed = true;
            return;
        };

        let mut input = self
            .render_input
            .take()
            .unwrap_or_else(|| self.pools.get::<f32>());
        let staged_samples = input.len();
        if input.ensure_len(required.max(staged_samples)).is_err() {
            self.render_input = Some(input);
            self.quantum_failed = true;
            return;
        }
        input.truncate(staged_samples);
        self.render_input = Some(input);
        if frames == 0 {
            self.staged_meta = Self::span_meta(meta, 0, 0);
        }
        self.prepared_frames = Some(frames);
    }

    fn retire_pending_input(&mut self) {
        let Some(pending) = self.pending_input.take() else {
            return;
        };
        if let Some(chunk) = self.retired_input.take() {
            self.source.retire_chunk(chunk);
        }
        self.retired_input = Some(pending.chunk);
    }

    fn span_meta(original: AudioChunkInfo, offset: usize, frames: usize) -> Option<AudioChunkInfo> {
        let offset = u64::try_from(offset).ok()?;
        let frames = u32::try_from(frames).ok()?;
        let mut meta = original;
        if let Some(span) = original.source_span {
            let end = offset.checked_add(u64::from(frames))?;
            let span = span.for_output_range(offset..end)?;
            meta.frame_offset = span.start();
            meta.timestamp = span.position_at(0)?;
            meta.end_timestamp = span.position_at(span.output_frames())?;
            meta.frames = frames;
            meta.source_span = Some(span);
            meta.source_byte_offset = None;
            meta.source_bytes = 0;
            meta.end_of_track = original.end_of_track && end == u64::from(original.frames);
            return Some(meta);
        }
        meta.frame_offset = original.frame_offset.checked_add(offset)?;
        meta.timestamp = original
            .timestamp
            .checked_add(original.spec.duration_for(offset).ok()?)?;
        meta.frames = frames;
        meta.end_timestamp = meta
            .timestamp
            .checked_add(original.spec.duration_for(u64::from(frames)).ok()?)?;
        meta.source_byte_offset = None;
        meta.source_bytes = 0;
        meta.end_of_track = original.end_of_track
            && offset.checked_add(u64::from(frames))? == u64::from(original.frames);
        Some(meta)
    }

    pub(super) fn stage_pending(&mut self) -> bool {
        let channels = usize::from(self.spec.channels.max(1));
        let Some(capacity) = self
            .prepared_frames
            .and_then(|frames| frames.checked_mul(channels))
        else {
            return false;
        };
        let staged = self.render_input.as_ref().map_or(0, |input| input.len());
        let staged_frames = staged / channels;
        let Some(pending) = self.pending_input.as_mut() else {
            return false;
        };
        if pending.chunk.spec() != self.spec {
            self.quantum_failed = true;
            return false;
        }

        let pending_frame = u64::try_from(pending.consumed_frames)
            .ok()
            .and_then(|consumed| pending.chunk.meta.frame_offset.checked_add(consumed));
        let expected_frame = self.staged_meta.and_then(|meta| {
            meta.frame_offset
                .checked_add(u64::try_from(staged_frames).ok()?)
        });
        if staged > 0 && expected_frame != pending_frame {
            self.quantum_failed = true;
            return false;
        }

        let remaining_frames = pending
            .chunk
            .frames()
            .saturating_sub(pending.consumed_frames);
        let free_frames = capacity.saturating_sub(staged) / channels;
        let frames = remaining_frames.min(free_frames);
        let source_start = pending.consumed_frames.saturating_mul(channels);
        let samples = frames.saturating_mul(channels);
        let source_end = source_start.saturating_add(samples);
        let Some(source) = pending.chunk.samples.get(source_start..source_end) else {
            self.quantum_failed = true;
            return false;
        };
        if staged == 0 {
            self.staged_meta = Self::span_meta(pending.chunk.meta, pending.consumed_frames, frames);
        } else if let Some(meta) = self.staged_meta.as_mut() {
            let Some(next) = Self::span_meta(pending.chunk.meta, pending.consumed_frames, frames)
            else {
                self.quantum_failed = true;
                return false;
            };
            if let Some(span) = meta.source_span {
                let Some(span) = next.source_span.and_then(|next| span.followed_by(next)) else {
                    self.quantum_failed = true;
                    return false;
                };
                meta.source_span = Some(span);
            }
            meta.end_timestamp = next.end_timestamp;
            let Ok(total) = u32::try_from(staged_frames.saturating_add(frames)) else {
                self.quantum_failed = true;
                return false;
            };
            meta.frames = total;
            meta.end_of_track = next.end_of_track;
        }
        let Some(input) = self.render_input.as_mut() else {
            self.quantum_failed = true;
            return false;
        };
        let end = staged.saturating_add(samples);
        if end > input.capacity() || input.try_extend_from_slice(source).is_err() {
            self.quantum_failed = true;
            return false;
        }
        pending.consumed_frames = pending.consumed_frames.saturating_add(frames);
        let consumed = pending.consumed_frames == pending.chunk.frames();
        if consumed {
            self.retire_pending_input();
        }
        consumed
    }

    fn staged_frames(&self) -> usize {
        let channels = usize::from(self.spec.channels.max(1));
        self.render_input.as_ref().map_or(0, |input| input.len()) / channels
    }
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    fn begin_drain(&mut self) {
        self.drain_state = DrainState::Warp;
    }

    fn drain_step(&mut self) -> Option<TrackStep<AudioChunk>> {
        if let DrainState::LiveWarp = self.drain_state {
            let Ok(chunk) = self.warp.drain(self.lane.output_limit()) else {
                self.quantum_failed = true;
                return Some(TrackStep::StateChanged);
            };
            if !self.warp.transition_pending() {
                self.drain_state = DrainState::Open;
            }
            return Some(
                chunk
                    .and_then(|chunk| apply_effects(&mut self.effects, chunk))
                    .and_then(|output| self.fetch(output))
                    .map_or(TrackStep::StateChanged, TrackStep::Produced),
            );
        }

        if let DrainState::Warp = self.drain_state {
            let Ok(chunk) = self.warp.drain(self.lane.output_limit()) else {
                self.quantum_failed = true;
                return Some(TrackStep::StateChanged);
            };
            if let Some(chunk) = chunk {
                return Some(
                    apply_effects(&mut self.effects, chunk)
                        .and_then(|output| self.fetch(output))
                        .map_or(TrackStep::StateChanged, TrackStep::Produced),
                );
            }
            self.drain_state = DrainState::Effects;
        }

        let DrainState::Effects = self.drain_state else {
            return None;
        };
        Some(match self.drain.step(&mut self.effects) {
            EffectDrainStep::Produced(chunk) => {
                let source_end = chunk.meta.source_span.map(|span| {
                    SourceEnd::new(span.end(), span.sample_rate())
                        .with_mapping_revision(span.mapping_revision())
                });
                self.emit_output(*chunk, source_end)
                    .map_or(TrackStep::StateChanged, TrackStep::Produced)
            }
            EffectDrainStep::Progress => TrackStep::StateChanged,
            EffectDrainStep::Exhausted => {
                self.drain_state = DrainState::Exhausted;
                TrackStep::Eof
            }
        })
    }

    fn fetch(&mut self, data: AudioChunk) -> Option<Fetch<AudioChunk>> {
        let Some(span) = data.meta.source_span else {
            self.quantum_failed = true;
            return None;
        };
        let source_end = SourceEnd::new(span.end(), span.sample_rate())
            .with_mapping_revision(span.mapping_revision());
        self.emit_output(data, Some(source_end))
    }

    pub(super) fn emit_output(
        &mut self,
        mut output: AudioChunk,
        source_end: Option<SourceEnd>,
    ) -> Option<Fetch<AudioChunk>> {
        if output.frames() > self.lane.output_limit() {
            self.quantum_failed = true;
            return None;
        }
        if let Some(span) = output.meta.source_span {
            output.meta.timestamp = span.position_at(0)?;
            output.meta.end_timestamp = span.position_at(span.output_frames())?;
        }
        self.lane.stamp(&mut output);
        Some(match source_end {
            Some(end) => Fetch::rendered(output, end),
            None => Fetch::data(output),
        })
    }

    /// Executes the lane batches due at its cursor, then prepares the quantum
    /// that starts there, ending it at the next batch's frame.
    fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
    ) -> Result<FrameCount, WarpRenderError> {
        self.warp
            .prepare_quantum(meta, remaining, self.lane.output_limit())
    }

    fn prepare_renderers(&mut self, spec: AudioSpec) {
        self.spec = spec;
        self.warp.prepare(spec);
        if self.warp.transition_pending() {
            match self.warp.prepare_engine_latency(spec) {
                Ok(_) | Err(WarpRenderError::NeedsService) => {}
                Err(_) => {
                    self.quantum_failed = true;
                    return;
                }
            }
        }
        if self.warp.transition_pending() && matches!(self.drain_state, DrainState::Open) {
            self.drain_state = DrainState::LiveWarp;
        }
        if !self.warp.transition_pending() {
            self.prepare_staging();
        }
        for effect in &mut self.effects {
            effect.service_deferred(spec);
        }
    }

    fn render_full_quantum(&mut self) -> Option<TrackStep<AudioChunk>> {
        let frames = self.prepared_frames?;
        (self.staged_frames() >= frames).then(|| self.render_staged(frames))
    }

    fn render_quantum(
        &mut self,
        chunk: AudioChunk,
    ) -> ControlFlow<AudioChunk, Option<Fetch<AudioChunk>>> {
        let output = self.warp.render_quantum(chunk)?;
        if self.warp.transition_pending() {
            self.drain_state = DrainState::LiveWarp;
        }
        let output = output.and_then(|chunk| apply_effects(&mut self.effects, chunk));
        ControlFlow::Continue(output.and_then(|output| self.fetch(output)))
    }

    fn render_source_quantum(&mut self, chunk: AudioChunk) -> Option<Fetch<AudioChunk>> {
        match self.render_quantum(chunk) {
            ControlFlow::Continue(output) => output,
            ControlFlow::Break(input) => {
                debug_assert!(self.retired_input.is_none());
                self.retired_input = Some(input);
                self.quantum_failed = true;
                None
            }
        }
    }

    pub(super) fn render_staged(&mut self, frames: usize) -> TrackStep<AudioChunk> {
        if self.quantum_failed {
            return self.fail(TrackFailureKind::Render);
        }
        let channels = usize::from(self.spec.channels.max(1));
        let Some(samples) = frames.checked_mul(channels) else {
            self.quantum_failed = true;
            return self.fail(TrackFailureKind::Render);
        };
        let Some(staged_meta) = self.staged_meta else {
            self.quantum_failed = true;
            return self.fail(TrackFailureKind::Render);
        };
        let Some(meta) = Self::span_meta(staged_meta, 0, frames) else {
            self.quantum_failed = true;
            return self.fail(TrackFailureKind::Render);
        };
        let Some(mut input) = self.render_input.take() else {
            return TrackStep::StateChanged;
        };
        if input.len() < samples {
            self.render_input = Some(input);
            self.quantum_failed = true;
            return self.fail(TrackFailureKind::Render);
        }
        self.staged_meta = None;
        self.prepared_frames = None;
        if input.len() > samples {
            let mut prefix = self.pools.get::<f32>();
            if prefix.ensure_len(samples).is_err() {
                self.render_input = Some(input);
                self.quantum_failed = true;
                return self.fail(TrackFailureKind::Render);
            }
            prefix.copy_from_slice(&input[..samples]);
            drop(input.drain(..samples));
            self.staged_meta = Self::span_meta(staged_meta, frames, input.len() / channels);
            self.render_input = Some(input);
            input = prefix;
        }
        match self.render_quantum(AudioChunk::new(meta, input)) {
            ControlFlow::Continue(output) => {
                output.map_or(TrackStep::StateChanged, TrackStep::Produced)
            }
            ControlFlow::Break(input) => {
                self.render_input = Some(input.samples);
                self.quantum_failed = true;
                self.fail(TrackFailureKind::Render)
            }
        }
    }

    fn render_whole_pending(&mut self) -> Option<TrackStep<AudioChunk>> {
        if self.staged_frames() != 0 {
            return None;
        }
        let prepared = self.prepared_frames?;
        let pending = self.pending_input.as_ref()?;
        if pending.consumed_frames != 0 || pending.chunk.frames() != prepared {
            return None;
        }
        let pending = self.pending_input.take()?;
        self.prepared_frames = None;
        Some(
            self.render_source_quantum(pending.chunk)
                .map_or(TrackStep::StateChanged, TrackStep::Produced),
        )
    }

    fn reset_renderers(&mut self) {
        self.warp.reset();
        reset_effects(&mut self.effects);
    }

    fn sync_discontinuity(&mut self) -> bool {
        let next = self.source.discontinuity();
        let revision_changed = next.as_ref().map(SourceDiscontinuity::revision)
            != self
                .discontinuity
                .as_ref()
                .map(SourceDiscontinuity::revision);
        if let Some(discontinuity) = next.as_ref() {
            self.spec = *discontinuity.spec();
        }
        self.discontinuity = next;
        if !revision_changed {
            return false;
        }
        self.discard_staged_input();
        self.reset_renderers();
        self.drain.reset();
        self.drain_state = DrainState::Open;
        true
    }
}

impl<T, S> AudioSource for WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Chunk = AudioChunk;

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        self.discontinuity
    }

    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        if let Some(chunk) = self.retired_input.take() {
            self.source.retire_chunk(chunk);
        }
        let spec = self.source.prepare_deferred();
        self.sync_discontinuity();
        self.prepare_renderers(spec.unwrap_or(self.spec));
        spec
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if let Some(failure) = self.terminal_failure {
            return TrackStep::Failed(failure);
        }
        match self.service_commands() {
            Ok(true) => return TrackStep::StateChanged,
            Ok(false) => {}
            Err(error) => return self.fail(error),
        }
        if self.sync_discontinuity() {
            return TrackStep::StateChanged;
        }
        if self.quantum_failed {
            return self.fail(TrackFailureKind::Render);
        }

        if matches!(self.drain_state, DrainState::Exhausted) {
            return TrackStep::Eof;
        }
        if let Some(step) = self.drain_step() {
            return step;
        }
        if let Some(step) = self.render_whole_pending() {
            return step;
        }
        if let Some(step) = self.render_full_quantum() {
            return step;
        }
        if self.pending_input.is_some() {
            if self.prepared_frames.is_none() {
                return TrackStep::Blocked(WaitingReason::Waiting);
            }
            self.stage_pending();
            return self
                .render_full_quantum()
                .unwrap_or(TrackStep::StateChanged);
        }
        if !self.warp.accepts_input() {
            return self.fail(TrackFailureKind::Render);
        }

        match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                if data.spec() == self.spec
                    && self.prepared_frames.is_none()
                    && self
                        .prepare_quantum(data.meta, data.frames())
                        .is_ok_and(|frames| frames.get() == data.frames())
                {
                    return self
                        .render_source_quantum(data)
                        .map_or(TrackStep::StateChanged, TrackStep::Produced);
                }
                self.pending_input = Some(PendingInput {
                    chunk: data,
                    consumed_frames: 0,
                });
                TrackStep::StateChanged
            }
            TrackStep::Produced(Fetch::Failure { failure }) | TrackStep::Failed(failure) => {
                self.fail(failure)
            }
            TrackStep::Produced(fetch) => TrackStep::Produced(fetch),
            TrackStep::Eof => {
                self.begin_drain();
                let frames = self.staged_frames();
                if frames == 0 {
                    TrackStep::StateChanged
                } else {
                    let Some(frames) = self.warp.prepare_terminal_quantum(frames) else {
                        self.quantum_failed = true;
                        return self.fail(TrackFailureKind::Render);
                    };
                    self.prepared_frames = Some(frames.get());
                    self.render_staged(frames.get())
                }
            }
            TrackStep::StateChanged => {
                self.sync_discontinuity();
                TrackStep::StateChanged
            }
            TrackStep::Blocked(reason) => TrackStep::Blocked(reason),
        }
    }

    delegate::delegate! {
        to self.source {
            fn commit_source_end(&mut self, source_end: SourceEnd, meta: AudioChunkInfo);
            fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
            fn host_sample_rate(&self) -> Option<NonZeroU32>;
            fn retire_chunk(&self, chunk: AudioChunk);
            fn finish_deferred(&mut self);
            fn warm_up(&mut self);
        }
    }
}
