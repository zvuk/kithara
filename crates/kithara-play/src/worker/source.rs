use kithara_audio::{AudioSource, Fetch, SourceDiscontinuity, SourceEnd, TrackStep};
use kithara_bufpool::{BufferRing, HasPool, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::SeekObserve;
use kithara_test_macros as kithara;

use crate::effects::{
    AudioEffect, EffectDrain, EffectDrainStep, apply_effects, held_source_frames, reset_effects,
};

#[derive(Clone, Copy)]
enum DrainState {
    Open,
    Warp(u64),
    Effects(u64),
    Exhausted(u64),
}

impl DrainState {
    const fn epoch(self) -> Option<u64> {
        match self {
            Self::Open => None,
            Self::Warp(epoch) | Self::Effects(epoch) | Self::Exhausted(epoch) => Some(epoch),
        }
    }
}

struct PendingInput {
    chunk: AudioChunk,
    consumed_frames: usize,
    epoch: u64,
}

#[derive(Clone, Copy)]
struct PreparedQuantum {
    meta: AudioChunkInfo,
    frames: usize,
    filled_frames: usize,
    samples: usize,
    whole_input: bool,
    epoch: u64,
}

#[derive(Clone, Copy)]
struct PreparedTerminal {
    meta: AudioChunkInfo,
    remaining_meta: Option<AudioChunkInfo>,
    remaining_frames: usize,
    remaining_samples: usize,
    samples: usize,
    total_samples: usize,
}

/// The sole producer-side Warp/effect stage before the play output ring.
pub(crate) struct WarpSource<T, S> {
    source: T,
    warp: kithara_warp::WarpRenderer<S>,
    effects: Vec<Box<dyn AudioEffect>>,
    drain: EffectDrain,
    seek: Arc<dyn SeekObserve>,
    discontinuity: Option<SourceDiscontinuity>,
    spec: AudioSpec,
    drain_state: DrainState,
    reset_epoch: Option<u64>,
    pools: PoolRegion<S>,
    pending_input: Option<PendingInput>,
    quantum_input: Option<SampleBuffer>,
    terminal_input: Option<BufferRing<SampleBuffer>>,
    terminal_chunk: Option<SampleBuffer>,
    prepared_terminal: Option<PreparedTerminal>,
    prepared_quantum: Option<PreparedQuantum>,
    retired_input: Option<AudioChunk>,
    quantum_failed: bool,
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    pub(crate) fn new(
        source: T,
        warp: kithara_warp::WarpRenderer<S>,
        effects: Vec<Box<dyn AudioEffect>>,
        drain: EffectDrain,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        let discontinuity = source.discontinuity();
        let seek = source.seek_observe();
        Self {
            source,
            warp,
            effects,
            drain,
            seek,
            discontinuity,
            spec,
            drain_state: DrainState::Open,
            reset_epoch: None,
            pools,
            pending_input: None,
            quantum_input: None,
            terminal_input: None,
            terminal_chunk: None,
            prepared_terminal: None,
            prepared_quantum: None,
            retired_input: None,
            quantum_failed: false,
        }
    }

    fn retire_pending_input(&mut self) {
        let Some(pending) = self.pending_input.take() else {
            return;
        };
        debug_assert!(self.retired_input.is_none());
        self.retired_input = Some(pending.chunk);
    }

    fn discard_pending_input(&mut self) {
        self.retire_pending_input();
        if let Some(input) = self.terminal_input.take() {
            self.quantum_input = Some(input.into_inner());
        }
        self.prepared_terminal = None;
        self.prepared_quantum = None;
    }

    fn has_staged_quantum(&self) -> bool {
        self.prepared_quantum
            .is_some_and(|prepared| prepared.filled_frames > 0)
    }

    fn sync_discontinuity(&mut self) {
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
        let already_reset = self.reset_epoch == Some(self.source.decode_epoch());
        if !revision_changed {
            return;
        }
        self.discard_pending_input();
        self.quantum_failed = false;
        if already_reset {
            self.reset_epoch = None;
        } else {
            self.reset_renderers();
        }
        self.drain.reset();
        self.drain_state = DrainState::Open;
    }

    fn reset_renderers(&mut self) {
        self.warp.reset();
        reset_effects(&mut self.effects);
    }

    fn prepare_renderers(&mut self, spec: AudioSpec) {
        self.spec = spec;
        if self.terminal_input.is_some() {
            self.warp.prepare_terminal();
        } else if self.prepared_terminal.is_none() {
            self.warp.prepare(spec);
        }
        for effect in &mut self.effects {
            effect.service_deferred(spec);
        }
    }

    fn cancel_stale_drain(&mut self) -> bool {
        let stale = self
            .drain_state
            .epoch()
            .is_some_and(|epoch| self.seek.epoch() != epoch || self.source.decode_epoch() != epoch);
        if !stale {
            return false;
        }
        let epoch = self.seek.epoch();
        self.discard_pending_input();
        self.reset_renderers();
        self.drain.reset();
        self.drain_state = DrainState::Open;
        self.reset_epoch = Some(epoch);
        self.quantum_failed = false;
        true
    }

    fn cancel_stale_input(&mut self) -> bool {
        let stale = self.pending_input.as_ref().is_some_and(|pending| {
            self.seek.epoch() != pending.epoch || self.source.decode_epoch() != pending.epoch
        }) || self.prepared_quantum.as_ref().is_some_and(|prepared| {
            prepared.filled_frames > 0
                && (self.seek.epoch() != prepared.epoch
                    || self.source.decode_epoch() != prepared.epoch)
        });
        if !stale {
            return false;
        }
        let epoch = self.seek.epoch();
        self.discard_pending_input();
        self.reset_renderers();
        self.drain.reset();
        self.drain_state = DrainState::Open;
        self.reset_epoch = Some(epoch);
        self.quantum_failed = false;
        true
    }
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    fn pending_span_meta(pending: &PendingInput, frames: usize) -> Option<AudioChunkInfo> {
        let original = pending.chunk.meta;
        let consumed = u64::try_from(pending.consumed_frames).ok()?;
        let frames = u32::try_from(frames).ok()?;
        let mut meta = original;
        meta.frame_offset = original.frame_offset.checked_add(consumed)?;
        meta.timestamp = original
            .timestamp
            .checked_add(original.spec.duration_for(consumed).ok()?)?;
        meta.frames = frames;
        meta.end_timestamp = meta
            .timestamp
            .checked_add(original.spec.duration_for(u64::from(frames)).ok()?)?;
        let total_frames = pending.chunk.frames();
        let span_end = pending
            .consumed_frames
            .checked_add(usize::try_from(frames).ok()?)?;
        if span_end == total_frames {
            meta.end_timestamp = original.end_timestamp;
        }
        if pending.consumed_frames > 0 || usize::try_from(frames).ok()? != total_frames {
            meta.source_byte_offset = None;
            meta.source_bytes = 0;
        }
        Some(meta)
    }

    fn terminal_span_meta(
        original: AudioChunkInfo,
        offset: usize,
        frames: usize,
    ) -> Option<AudioChunkInfo> {
        let offset = u64::try_from(offset).ok()?;
        let frames = u32::try_from(frames).ok()?;
        let mut meta = original;
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
        Some(meta)
    }

    fn prepared_terminal(
        original: AudioChunkInfo,
        frames: usize,
        total_samples: usize,
    ) -> Option<PreparedTerminal> {
        let channels = usize::from(original.spec.channels.max(1));
        if total_samples == 0 || !total_samples.is_multiple_of(channels) {
            return None;
        }
        let available_frames = total_samples / channels;
        if frames == 0 || frames > available_frames {
            return None;
        }
        let samples = frames.checked_mul(channels)?;
        let remaining_samples = total_samples.checked_sub(samples)?;
        if !remaining_samples.is_multiple_of(channels) {
            return None;
        }
        let remaining_frames = remaining_samples / channels;
        let remaining_meta = if remaining_frames > 0 {
            Some(Self::terminal_span_meta(
                original,
                frames,
                remaining_frames,
            )?)
        } else {
            None
        };
        Some(PreparedTerminal {
            meta: Self::terminal_span_meta(original, 0, frames)?,
            remaining_meta,
            remaining_frames,
            remaining_samples,
            samples,
            total_samples,
        })
    }

    fn prepare_quantum_shape(&mut self) -> Option<PreparedQuantum> {
        let pending = self.pending_input.as_ref()?;
        let total_frames = pending.chunk.frames();
        let remaining = total_frames.checked_sub(pending.consumed_frames)?;
        let current = Self::pending_span_meta(pending, remaining)?;
        let frames = self.warp.prepare_quantum(current, remaining)?.get();
        let samples = frames.checked_mul(usize::from(current.spec.channels.max(1)))?;
        Some(PreparedQuantum {
            meta: Self::pending_span_meta(pending, frames)?,
            frames,
            filled_frames: 0,
            samples,
            whole_input: pending.consumed_frames == 0 && frames == total_frames,
            epoch: pending.epoch,
        })
    }

    fn prepare_quantum_input(&mut self) {
        if self.quantum_failed {
            self.prepared_quantum = None;
            return;
        }
        if self.pending_input.is_none() {
            if !self.has_staged_quantum() {
                self.prepared_quantum = None;
            }
            return;
        }
        if !self.has_staged_quantum() {
            self.prepared_quantum = self.prepare_quantum_shape();
        }
        let Some(prepared) = self.prepared_quantum else {
            self.quantum_failed = true;
            return;
        };
        if prepared.whole_input {
            return;
        }
        let mut input = self
            .quantum_input
            .take()
            .unwrap_or_else(|| self.pools.get::<f32>());
        if input.ensure_len(prepared.samples).is_err() {
            self.quantum_input = Some(input);
            self.quantum_failed = true;
            return;
        }
        self.quantum_input = Some(input);
    }

    fn prepare_terminal_chunk(&mut self) {
        if self.prepared_terminal.is_some() {
            return;
        }
        let Some((prepared, available_samples)) = self
            .prepared_quantum
            .as_ref()
            .zip(self.terminal_input.as_ref().map(BufferRing::len))
        else {
            return;
        };
        let channels = usize::from(prepared.meta.spec.channels.max(1));
        if available_samples == 0 || !available_samples.is_multiple_of(channels) {
            self.quantum_failed = true;
            return;
        }
        let available_frames = available_samples / channels;
        let Some(frames) = self
            .warp
            .prepare_terminal_quantum(prepared.meta, available_frames)
            .filter(|frames| frames.get() > 0 && frames.get() <= available_frames)
        else {
            self.quantum_failed = true;
            return;
        };
        let Some(terminal) =
            Self::prepared_terminal(prepared.meta, frames.get(), available_samples)
        else {
            self.quantum_failed = true;
            return;
        };
        let mut input = self
            .terminal_chunk
            .take()
            .unwrap_or_else(|| self.pools.get::<f32>());
        if input.ensure_len(terminal.samples).is_err() {
            self.terminal_chunk = Some(input);
            self.quantum_failed = true;
            return;
        }
        self.terminal_chunk = Some(input);
        self.prepared_terminal = Some(terminal);
    }

    fn stage_pending(
        prepared: &mut PreparedQuantum,
        pending: &mut PendingInput,
        input: &mut SampleBuffer,
    ) -> Option<bool> {
        let channels = usize::from(prepared.meta.spec.channels.max(1));
        let pending_frame = pending
            .chunk
            .meta
            .frame_offset
            .checked_add(u64::try_from(pending.consumed_frames).ok()?)?;
        let expected_frame = prepared
            .meta
            .frame_offset
            .checked_add(u64::try_from(prepared.filled_frames).ok()?)?;
        if pending.epoch != prepared.epoch
            || pending.chunk.spec() != prepared.meta.spec
            || pending_frame != expected_frame
        {
            return None;
        }

        let available = pending
            .chunk
            .frames()
            .checked_sub(pending.consumed_frames)?;
        let needed = prepared.frames.checked_sub(prepared.filled_frames)?;
        let frames = available.min(needed);
        let samples = frames.checked_mul(channels)?;
        let source_start = pending.consumed_frames.checked_mul(channels)?;
        let source_end = source_start.checked_add(samples)?;
        let target_start = prepared.filled_frames.checked_mul(channels)?;
        let target_end = target_start.checked_add(samples)?;
        input
            .get_mut(target_start..target_end)?
            .copy_from_slice(pending.chunk.samples.get(source_start..source_end)?);
        pending.consumed_frames = pending.consumed_frames.checked_add(frames)?;
        prepared.filled_frames = prepared.filled_frames.checked_add(frames)?;
        Some(pending.consumed_frames == pending.chunk.frames())
    }
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    fn render_pending(&mut self) -> TrackStep<AudioChunk> {
        if self.quantum_failed {
            return TrackStep::Failed;
        }
        let Some(mut prepared) = self.prepared_quantum.take() else {
            return TrackStep::StateChanged;
        };
        if prepared.whole_input {
            let Some(pending) = self.pending_input.take() else {
                self.quantum_failed = true;
                return TrackStep::Failed;
            };
            debug_assert_eq!(pending.consumed_frames, 0);
            debug_assert_eq!(pending.chunk.frames(), prepared.frames);
            return self
                .render(pending.chunk, pending.epoch)
                .map_or(TrackStep::StateChanged, TrackStep::Produced);
        }
        let Some(mut input) = self.quantum_input.take() else {
            self.prepared_quantum = Some(prepared);
            return TrackStep::StateChanged;
        };
        if input.len() < prepared.samples {
            self.quantum_input = Some(input);
            self.prepared_quantum = Some(prepared);
            return TrackStep::StateChanged;
        }

        let Some(pending) = self.pending_input.as_mut() else {
            self.quantum_input = Some(input);
            self.prepared_quantum = Some(prepared);
            return TrackStep::Failed;
        };
        let Some(retire) = Self::stage_pending(&mut prepared, pending, &mut input) else {
            self.quantum_input = Some(input);
            self.prepared_quantum = Some(prepared);
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        if retire {
            self.retire_pending_input();
        }

        if prepared.filled_frames < prepared.frames {
            self.quantum_input = Some(input);
            self.prepared_quantum = Some(prepared);
            return TrackStep::StateChanged;
        }

        input.truncate(prepared.samples);
        self.render(AudioChunk::new(prepared.meta, input), prepared.epoch)
            .map_or(TrackStep::StateChanged, TrackStep::Produced)
    }

    fn render_terminal_quantum(&mut self) -> TrackStep<AudioChunk> {
        let Some(mut prepared) = self.prepared_quantum.take() else {
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        let epoch = prepared.epoch;
        let channels = usize::from(prepared.meta.spec.channels.max(1));
        if self.terminal_input.is_none() {
            let Some(staged) = self.quantum_input.take() else {
                self.prepared_quantum = Some(prepared);
                self.quantum_failed = true;
                return TrackStep::Failed;
            };
            let Some(readable) = prepared.filled_frames.checked_mul(channels) else {
                self.quantum_input = Some(staged);
                self.prepared_quantum = Some(prepared);
                self.quantum_failed = true;
                return TrackStep::Failed;
            };
            let input = match BufferRing::from_prefix(staged, readable) {
                Ok(input) => input,
                Err(staged) => {
                    self.quantum_input = Some(staged);
                    self.prepared_quantum = Some(prepared);
                    self.quantum_failed = true;
                    return TrackStep::Failed;
                }
            };
            self.terminal_input = Some(input);
            self.prepared_quantum = Some(prepared);
            return TrackStep::StateChanged;
        }
        let available_samples = self.terminal_input.as_ref().map_or(0, BufferRing::len);
        let Some(terminal) = self.prepared_terminal.take() else {
            self.prepared_quantum = Some(prepared);
            return TrackStep::StateChanged;
        };
        if available_samples != terminal.total_samples {
            self.prepared_quantum = Some(prepared);
            self.quantum_failed = true;
            return TrackStep::Failed;
        }
        let Some(mut input) = self.terminal_chunk.take() else {
            self.prepared_quantum = Some(prepared);
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        if input.len() < terminal.samples {
            self.terminal_chunk = Some(input);
            self.prepared_quantum = Some(prepared);
            self.quantum_failed = true;
            return TrackStep::Failed;
        }
        input.truncate(terminal.samples);
        if !self
            .terminal_input
            .as_mut()
            .is_some_and(|staged| staged.try_pop_into(&mut input))
        {
            self.terminal_chunk = Some(input);
            self.prepared_quantum = Some(prepared);
            self.quantum_failed = true;
            return TrackStep::Failed;
        }
        if let Some(remaining_meta) = terminal.remaining_meta {
            prepared.meta = remaining_meta;
            prepared.frames = terminal.remaining_frames;
            prepared.filled_frames = terminal.remaining_frames;
            prepared.samples = terminal.remaining_samples;
            prepared.whole_input = false;
            self.prepared_quantum = Some(prepared);
        } else if let Some(staged) = self.terminal_input.take() {
            self.quantum_input = Some(staged.into_inner());
        }

        self.render(AudioChunk::new(terminal.meta, input), epoch)
            .map_or(TrackStep::StateChanged, TrackStep::Produced)
    }
}

impl<T, S> WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32>,
{
    fn begin_drain(&mut self, epoch: u64) {
        self.drain_state = DrainState::Warp(epoch);
    }

    fn fetch(&self, data: AudioChunk, epoch: u64) -> Fetch<AudioChunk> {
        let source_end = self.warp.rendered_source_end().map(|(frame, sample_rate)| {
            SourceEnd::new(
                frame.saturating_sub(held_source_frames(&self.effects)),
                sample_rate,
            )
        });
        match source_end {
            Some(source_end) => Fetch::rendered(data, epoch, source_end),
            None => Fetch::data(data, epoch),
        }
    }

    fn render(&mut self, chunk: AudioChunk, epoch: u64) -> Option<Fetch<AudioChunk>> {
        let chunk = self.warp.render_quantum(chunk);
        let chunk = chunk?;
        let output = apply_effects(&mut self.effects, chunk)?;
        Some(self.fetch(output, epoch))
    }

    fn drain_step(&mut self) -> Option<TrackStep<AudioChunk>> {
        if let DrainState::Warp(epoch) = self.drain_state {
            if self.has_staged_quantum() {
                return Some(self.render_terminal_quantum());
            }
            if let Some(chunk) = self.warp.flush() {
                return Some(
                    apply_effects(&mut self.effects, chunk)
                        .map_or(TrackStep::StateChanged, |output| {
                            TrackStep::Produced(self.fetch(output, epoch))
                        }),
                );
            }
            self.drain_state = DrainState::Effects(epoch);
        }

        let DrainState::Effects(epoch) = self.drain_state else {
            return None;
        };
        Some(match self.drain.step(&mut self.effects) {
            EffectDrainStep::Produced(chunk) => TrackStep::Produced(self.fetch(chunk, epoch)),
            EffectDrainStep::Progress => TrackStep::StateChanged,
            EffectDrainStep::Exhausted => {
                self.drain_state = DrainState::Exhausted(epoch);
                TrackStep::Eof
            }
        })
    }
}

impl<T, S> AudioSource for WarpSource<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Chunk = AudioChunk;

    delegate::delegate! {
        to self.source {
            fn decode_epoch(&self) -> u64;
            fn commit_source_end(&mut self, source_end: SourceEnd, epoch: u64);
            fn retire_chunk(&self, chunk: AudioChunk);
            fn finish_deferred(&mut self);
            fn warm_up(&mut self);
        }
    }

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        self.discontinuity
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek)
    }

    #[kithara::measure]
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.sync_discontinuity();
        if self.cancel_stale_input() {
            return TrackStep::StateChanged;
        }
        if self.cancel_stale_drain() {
            return TrackStep::StateChanged;
        }
        if self.quantum_failed {
            return TrackStep::Failed;
        }

        if matches!(self.drain_state, DrainState::Exhausted(_)) {
            return TrackStep::Eof;
        }
        if let Some(step) = self.drain_step() {
            return step;
        }
        if self.pending_input.is_some() {
            return self.render_pending();
        }
        if !self.warp.accepts_input() {
            return TrackStep::Failed;
        }

        match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, epoch, .. }) => {
                let same_spec = data.spec() == self.spec;
                let staging = self.has_staged_quantum();
                self.pending_input = Some(PendingInput {
                    chunk: data,
                    consumed_frames: 0,
                    epoch,
                });
                if same_spec && !staging {
                    self.prepared_quantum = self.prepare_quantum_shape();
                    if self
                        .prepared_quantum
                        .is_some_and(|prepared| prepared.whole_input)
                    {
                        return self.render_pending();
                    }
                }
                if same_spec && staging {
                    return self.render_pending();
                }
                TrackStep::StateChanged
            }
            TrackStep::Produced(fetch) => TrackStep::Produced(fetch),
            TrackStep::Eof => {
                self.begin_drain(self.source.decode_epoch());
                if self.has_staged_quantum() {
                    self.render_terminal_quantum()
                } else {
                    TrackStep::StateChanged
                }
            }
            TrackStep::StateChanged => {
                self.sync_discontinuity();
                TrackStep::StateChanged
            }
            TrackStep::Blocked(reason) => TrackStep::Blocked(reason),
            TrackStep::Failed => TrackStep::Failed,
        }
    }

    #[kithara::measure]
    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        if let Some(chunk) = self.retired_input.take() {
            self.source.retire_chunk(chunk);
        }
        let spec = self.source.prepare_deferred();
        self.sync_discontinuity();
        self.prepare_renderers(spec.unwrap_or(self.spec));
        self.prepare_quantum_input();
        self.prepare_terminal_chunk();
        spec
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        num::{NonZeroU32, NonZeroUsize},
    };

    use kithara_audio::{Fetch, TrackStep, WaitingReason};
    use kithara_bufpool::PoolRegion;
    use kithara_platform::sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use kithara_signal::AudioChunkInfo;
    use kithara_stream::{SeekControl, SeekObserve, SeekState};
    use kithara_test_utils::kithara;
    use kithara_warp::{
        PresentationFrontier, RenderContext, RenderSnapshot, SessionEpoch, SessionFrame,
        StretchControls, StretchKind, TransportRevision,
    };

    use super::*;
    use crate::test_pools::{TestPools, pools, pools_with_budget};

    fn flush_deferred<S>(source: &mut S)
    where
        S: AudioSource,
    {
        let _ = source.prepare_deferred();
        source.finish_deferred();
    }

    fn source_stage<T>(
        pools: &PoolRegion<TestPools>,
        source: T,
        effects: Vec<Box<dyn AudioEffect>>,
        spec: AudioSpec,
    ) -> WarpSource<T, TestPools>
    where
        T: AudioSource<Chunk = AudioChunk>,
    {
        let config = kithara_warp::WarpConfig::builder()
            .render_quantum_frames(NonZeroUsize::new(64).expect("fixture quantum is non-zero"))
            .build();
        let warp = kithara_warp::Warp::new((), &config);
        let renderer = warp.quantum_renderer(spec, pools.clone());
        let drain = EffectDrain::new(effects.len(), pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        WarpSource::new(source, renderer, effects, drain, spec, pools.clone())
    }

    struct RawSource {
        chunks: VecDeque<AudioChunk>,
        head: Arc<AtomicU64>,
        seek: Arc<SeekState>,
    }

    struct TerminalRawSource {
        chunks: VecDeque<AudioChunk>,
        steps: Arc<AtomicU64>,
        seek: Arc<SeekState>,
    }

    impl AudioSource for TerminalRawSource {
        type Chunk = AudioChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            self.steps.fetch_add(1, Ordering::AcqRel);
            self.chunks.pop_front().map_or(TrackStep::Eof, |chunk| {
                TrackStep::Produced(Fetch::data(chunk, self.seek.epoch()))
            })
        }
    }

    impl AudioSource for RawSource {
        type Chunk = AudioChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            let Some(chunk) = self.chunks.pop_front() else {
                return TrackStep::Eof;
            };
            self.head.store(
                chunk
                    .meta
                    .frame_offset
                    .saturating_add(u64::from(chunk.meta.frames)),
                Ordering::Release,
            );
            TrackStep::Produced(Fetch::data(chunk, self.seek.epoch()))
        }
    }

    #[derive(Default)]
    struct BufferThenHalveFrames {
        buffered: Option<AudioChunk>,
    }

    impl AudioEffect for BufferThenHalveFrames {
        fn flush(&mut self) -> Option<AudioChunk> {
            self.buffered.take().and_then(halve_frames)
        }

        fn held_source_frames(&self) -> u64 {
            self.buffered
                .as_ref()
                .map_or(0, |chunk| u64::from(chunk.meta.frames))
        }

        fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
            self.buffered.replace(chunk).and_then(halve_frames)
        }

        fn reset(&mut self) {
            self.buffered = None;
        }
    }

    fn halve_frames(mut chunk: AudioChunk) -> Option<AudioChunk> {
        let frames = chunk.meta.frames / 2;
        let samples = usize::try_from(frames)
            .ok()?
            .checked_mul(usize::from(chunk.meta.spec.channels))?;
        chunk.samples.truncate(samples);
        chunk.meta.frames = frames;
        chunk.meta.end_timestamp = chunk
            .meta
            .spec
            .duration_for(chunk.meta.frame_offset.saturating_add(u64::from(frames)))
            .expect("fixture timestamp fits");
        Some(chunk)
    }

    struct DeferredSource {
        log: Arc<Mutex<Vec<&'static str>>>,
        seek: Arc<SeekState>,
        spec: AudioSpec,
    }

    impl AudioSource for DeferredSource {
        type Chunk = AudioChunk;

        fn prepare_deferred(&mut self) -> Option<AudioSpec> {
            self.log.lock().push("source.prepare");
            Some(self.spec)
        }

        fn finish_deferred(&mut self) {
            self.log.lock().push("source.finish");
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            TrackStep::Blocked(WaitingReason::Waiting)
        }
    }

    struct DeferredEffect {
        log: Arc<Mutex<Vec<&'static str>>>,
        serviced: Arc<Mutex<Option<AudioSpec>>>,
    }

    impl AudioEffect for DeferredEffect {
        fn service_deferred(&mut self, spec: AudioSpec) {
            self.log.lock().push("effect.service");
            *self.serviced.lock() = Some(spec);
        }

        fn flush(&mut self) -> Option<AudioChunk> {
            None
        }

        fn held_source_frames(&self) -> u64 {
            0
        }

        fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
            Some(chunk)
        }

        fn reset(&mut self) {}
    }

    struct RevisionSource {
        chunks: VecDeque<AudioChunk>,
        discontinuity: Arc<Mutex<SourceDiscontinuity>>,
        seek: Arc<SeekState>,
    }

    impl AudioSource for RevisionSource {
        type Chunk = AudioChunk;

        fn discontinuity(&self) -> Option<SourceDiscontinuity> {
            Some(*self.discontinuity.lock())
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            self.chunks
                .pop_front()
                .map_or(TrackStep::Blocked(WaitingReason::Waiting), |chunk| {
                    TrackStep::Produced(Fetch::data(chunk, 0))
                })
        }
    }

    struct ResetCounter {
        resets: Arc<AtomicU64>,
    }

    impl AudioEffect for ResetCounter {
        fn flush(&mut self) -> Option<AudioChunk> {
            None
        }

        fn held_source_frames(&self) -> u64 {
            0
        }

        fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
            Some(chunk)
        }

        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct CountingEofSource {
        discontinuity: Arc<Mutex<Option<SourceDiscontinuity>>>,
        steps: Arc<AtomicU64>,
        seek: Arc<SeekState>,
    }

    impl AudioSource for CountingEofSource {
        type Chunk = AudioChunk;

        fn discontinuity(&self) -> Option<SourceDiscontinuity> {
            *self.discontinuity.lock()
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            self.steps.fetch_add(1, Ordering::AcqRel);
            TrackStep::Eof
        }
    }

    struct CountingEmptyTail {
        flushes: Arc<AtomicU64>,
        resets: Arc<AtomicU64>,
    }

    impl AudioEffect for CountingEmptyTail {
        fn flush(&mut self) -> Option<AudioChunk> {
            self.flushes.fetch_add(1, Ordering::AcqRel);
            None
        }

        fn held_source_frames(&self) -> u64 {
            0
        }

        fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
            Some(chunk)
        }

        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct SeekApplyingSource {
        decode_epoch: u64,
        revision: u64,
        seek: Arc<SeekState>,
        spec: AudioSpec,
    }

    impl AudioSource for SeekApplyingSource {
        type Chunk = AudioChunk;

        fn decode_epoch(&self) -> u64 {
            self.decode_epoch
        }

        fn discontinuity(&self) -> Option<SourceDiscontinuity> {
            Some(SourceDiscontinuity::new(self.revision, self.spec))
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            let live_epoch = self.seek.epoch();
            if live_epoch != self.decode_epoch {
                self.decode_epoch = live_epoch;
                self.revision = self.revision.wrapping_add(1);
                TrackStep::StateChanged
            } else {
                TrackStep::Eof
            }
        }
    }

    struct ResettingTail {
        resets: Arc<AtomicU64>,
        tail: Option<AudioChunk>,
    }

    impl AudioEffect for ResettingTail {
        fn flush(&mut self) -> Option<AudioChunk> {
            self.tail.take()
        }

        fn held_source_frames(&self) -> u64 {
            self.tail
                .as_ref()
                .map_or(0, |chunk| u64::from(chunk.meta.frames))
        }

        fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
            Some(chunk)
        }

        fn reset(&mut self) {
            self.tail = None;
            self.resets.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn chunk(pools: &PoolRegion<TestPools>, spec: AudioSpec, frame_offset: u64) -> AudioChunk {
        const FRAMES: usize = 128;
        chunk_with_frames(
            pools,
            spec,
            frame_offset,
            u32::try_from(FRAMES).expect("fixture frames fit u32"),
            0.25,
        )
    }

    fn chunk_with_frames(
        pools: &PoolRegion<TestPools>,
        spec: AudioSpec,
        frame_offset: u64,
        frames: u32,
        sample: f32,
    ) -> AudioChunk {
        let samples = usize::try_from(frames)
            .expect("fixture frames fit usize")
            .checked_mul(usize::from(spec.channels))
            .expect("fixture sample count fits usize");
        let mut buffer = pools
            .get_with_len::<f32>(samples)
            .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
        buffer.fill(sample);
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames,
                frame_offset,
                timestamp: spec
                    .duration_for(frame_offset)
                    .expect("fixture timestamp fits"),
                end_timestamp: spec
                    .duration_for(frame_offset.saturating_add(u64::from(frames)))
                    .expect("fixture end timestamp fits"),
                ..Default::default()
            },
            buffer,
        )
    }

    #[kithara::test]
    #[case::stereo_44k(2, 44_100, 64)]
    #[case::mono_48k(1, 48_000, 64)]
    fn buffered_frame_changing_effect_tracks_live_and_flush_frontiers(
        #[case] channels: u16,
        #[case] sample_rate: u32,
        #[case] chunk_frames: u32,
    ) {
        let spec = AudioSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("test sample rate"),
        );
        let pools = pools();
        let head = Arc::new(AtomicU64::new(0));
        let next_offset = u64::from(chunk_frames);
        let final_offset = next_offset
            .checked_mul(2)
            .expect("fixture source extent fits u64");
        let output_frames = chunk_frames / 2;
        let source = RawSource {
            chunks: VecDeque::from([
                chunk_with_frames(&pools, spec, 0, chunk_frames, 0.25),
                chunk_with_frames(&pools, spec, next_offset, chunk_frames, 0.25),
            ]),
            head: Arc::clone(&head),
            seek: Arc::new(SeekState::new()),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::<BufferThenHalveFrames>::default()];
        let mut source = source_stage(&pools, source, effects, spec);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            head.load(Ordering::Acquire),
            next_offset,
            "one worker pass advances exactly one source transition"
        );
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data {
            data, source_end, ..
        }) = source.step_track()
        else {
            panic!("the second raw chunk must release the first buffered span");
        };

        assert_eq!(head.load(Ordering::Acquire), final_offset);
        assert_eq!(data.meta.frame_offset, 0);
        assert_eq!(data.meta.frames, output_frames);
        assert_eq!(
            source_end,
            Some(SourceEnd::new(
                next_offset,
                NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
            )),
            "the buffered second span remains outside the live source frontier"
        );

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        let TrackStep::Produced(Fetch::Data {
            data, source_end, ..
        }) = source.step_track()
        else {
            panic!("EOF drain must release the second buffered span");
        };

        assert_eq!(data.meta.frame_offset, next_offset);
        assert_eq!(data.meta.frames, output_frames);
        assert_eq!(
            source_end,
            Some(SourceEnd::new(
                final_offset,
                NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
            )),
            "terminal output releases the held source frontier"
        );
    }

    #[kithara::test]
    fn deferred_shell_services_effects_between_source_phases() {
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
        let pools = pools();
        let log = Arc::new(Mutex::new(Vec::new()));
        let serviced = Arc::new(Mutex::new(None));
        let source = DeferredSource {
            log: Arc::clone(&log),
            seek: Arc::new(SeekState::new()),
            spec,
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(DeferredEffect {
            log: Arc::clone(&log),
            serviced: Arc::clone(&serviced),
        })];
        let mut source = source_stage(&pools, source, effects, spec);

        flush_deferred(&mut source);

        assert_eq!(
            log.lock().as_slice(),
            ["source.prepare", "effect.service", "source.finish"]
        );
        assert_eq!(*serviced.lock(), Some(spec));
    }

    #[kithara::test]
    fn discontinuity_refreshes_spec_without_resetting_same_revision() {
        let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
        let changed = AudioSpec::new(1, NonZeroU32::new(48_000).expect("changed rate"));
        let pools = pools();
        let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
        let resets = Arc::new(AtomicU64::new(0));
        let source = RevisionSource {
            chunks: VecDeque::new(),
            discontinuity: Arc::clone(&discontinuity),
            seek: Arc::new(SeekState::new()),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResetCounter {
            resets: Arc::clone(&resets),
        })];
        let mut source = source_stage(&pools, source, effects, initial);

        *discontinuity.lock() = SourceDiscontinuity::new(7, changed);
        flush_deferred(&mut source);
        assert_eq!(
            source.discontinuity().map(|stamp| *stamp.spec()),
            Some(changed)
        );
        assert_eq!(resets.load(Ordering::Acquire), 0);

        *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
        flush_deferred(&mut source);
        assert_eq!(resets.load(Ordering::Acquire), 1);
    }

    #[kithara::test]
    #[case::stereo_to_mono(2, 44_100, 1, 48_000, 256, 512, 64)]
    #[case::mono_to_stereo(1, 48_000, 2, 44_100, 128, 384, 32)]
    fn unity_warp_preserves_samples_and_meta_across_discontinuity(
        #[case] initial_channels: u16,
        #[case] initial_sample_rate: u32,
        #[case] changed_channels: u16,
        #[case] changed_sample_rate: u32,
        #[case] first_offset: u64,
        #[case] second_offset: u64,
        #[case] chunk_frames: u32,
    ) {
        let initial = AudioSpec::new(
            initial_channels,
            NonZeroU32::new(initial_sample_rate).expect("initial rate"),
        );
        let changed = AudioSpec::new(
            changed_channels,
            NonZeroU32::new(changed_sample_rate).expect("changed rate"),
        );
        let pools = pools();
        let first = chunk_with_frames(&pools, initial, first_offset, chunk_frames, 0.25);
        let first_ptr = first.samples.as_ptr();
        let first_meta = first.meta;
        let first_samples = first.samples.to_vec();
        let mut second = chunk_with_frames(&pools, changed, second_offset, chunk_frames, 0.25);
        second.meta.segment_index = Some(3);
        second.meta.variant_index = Some(2);
        second.meta.epoch = 9;
        second.meta.source_byte_offset = Some(4096);
        second.meta.source_bytes = 1024;
        second.samples.fill(-0.25);
        let second_ptr = second.samples.as_ptr();
        let second_meta = second.meta;
        let second_samples = second.samples.to_vec();
        let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
        let source = RevisionSource {
            chunks: VecDeque::from([first, second]),
            discontinuity: Arc::clone(&discontinuity),
            seek: Arc::new(SeekState::new()),
        };
        let effects = Vec::new();
        let mut source = source_stage(&pools, source, effects, initial);

        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("initial unity span must pass through");
        };
        assert_eq!(data.meta, first_meta);
        assert_eq!(data.samples.as_ptr(), first_ptr);
        assert_eq!(&data.samples[..], &first_samples);

        *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("post-discontinuity unity span must pass through");
        };
        assert_eq!(data.meta, second_meta);
        assert_eq!(data.samples.as_ptr(), second_ptr);
        assert_eq!(&data.samples[..], &second_samples);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn live_rate_changes_stay_bounded_and_seek_discards_stale_input(#[case] backend: StretchKind) {
        const ACTIVE_FRAMES: u32 = 4096;
        const RATE_CHANGE_FRAMES: u32 = 4096;
        const SENTINEL_FRAMES: u32 = 512;

        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let first_active_end = u64::from(ACTIVE_FRAMES);
        let rate_change_end = first_active_end.saturating_add(u64::from(RATE_CHANGE_FRAMES));
        let sentinel_end = rate_change_end.saturating_add(u64::from(SENTINEL_FRAMES));

        let first_active = chunk_with_frames(&pools, spec, 0, ACTIVE_FRAMES, 0.25);
        let rate_change =
            chunk_with_frames(&pools, spec, first_active_end, RATE_CHANGE_FRAMES, -0.25);
        let sentinel = chunk_with_frames(&pools, spec, rate_change_end, SENTINEL_FRAMES, 0.75);
        let sentinel_samples = sentinel.samples.to_vec();

        let head = Arc::new(AtomicU64::new(0));
        let seek = Arc::new(SeekState::new());
        let raw = RawSource {
            chunks: VecDeque::from([first_active, rate_change, sentinel]),
            head: Arc::clone(&head),
            seek: Arc::clone(&seek),
        };
        let controls = StretchControls::new(0.5);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let config = kithara_warp::WarpConfig::builder()
            .stretch(Arc::clone(&controls))
            .build();
        let renderer = kithara_warp::Warp::new((), &config).quantum_renderer(spec, pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let mut source = WarpSource::new(raw, renderer, effects, drain, spec, pools.clone());

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(head.load(Ordering::Acquire), first_active_end);
        let mut active_steps = 0;
        while source.pending_input.is_some() {
            let consumed_before = source
                .pending_input
                .as_ref()
                .map_or(0, |pending| pending.consumed_frames);
            flush_deferred(&mut source);
            let step = source.step_track();
            if let TrackStep::Produced(Fetch::Data { data, .. }) = &step {
                assert!(
                    data.frames() <= 512,
                    "live Warp output stays quantum-bounded"
                );
            }
            assert!(matches!(
                step,
                TrackStep::Produced(_) | TrackStep::StateChanged
            ));
            assert!(
                source.pending_input.is_none()
                    || source
                        .pending_input
                        .as_ref()
                        .is_some_and(|pending| pending.consumed_frames > consumed_before),
                "each active step must advance the pending source span"
            );
            active_steps += 1;
            assert!(
                active_steps
                    <= usize::try_from(ACTIVE_FRAMES).expect("fixture frame count fits usize"),
                "bounded active input must converge"
            );
        }
        assert!(active_steps > 1, "fixture must span multiple Warp quanta");

        controls.set_speed(1.0);
        flush_deferred(&mut source);
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(head.load(Ordering::Acquire), rate_change_end);
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("the active engine must process exact-unity without a terminal drain");
        };
        assert!(data.frames() <= 512);

        controls.set_speed(2.0);
        let held_head = head.load(Ordering::Acquire);
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("the next quantum must observe the latest live rate");
        };
        assert!(data.frames() <= 512);
        assert_eq!(head.load(Ordering::Acquire), held_head);

        controls.set_speed(1.0);
        assert_eq!(
            seek.begin(kithara_platform::time::Duration::from_secs(1)),
            1
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(head.load(Ordering::Acquire), rate_change_end);

        flush_deferred(&mut source);
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(head.load(Ordering::Acquire), sentinel_end);
        let mut rendered = Vec::with_capacity(sentinel_samples.len());
        let mut frontier = rate_change_end;
        while source.pending_input.is_some() {
            flush_deferred(&mut source);
            match source.step_track() {
                TrackStep::Produced(Fetch::Data { data, .. }) => {
                    assert_eq!(data.meta.frame_offset, frontier);
                    frontier = frontier.saturating_add(u64::from(data.meta.frames));
                    rendered.extend_from_slice(&data.samples);
                }
                TrackStep::StateChanged => {}
                _ => panic!("playback must resume with the post-seek source chunk"),
            }
        }
        assert_eq!(frontier, sentinel_end);
        assert_eq!(rendered, sentinel_samples);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith, false)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_pool_failure(StretchKind::Signalsmith, true)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee, false))]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_pool_failure(StretchKind::Bungee, true)
    )]
    fn terminal_partial_quantum_preserves_source_and_sampled_intent(
        #[case] backend: StretchKind,
        #[case] exhaust_terminal_pool: bool,
    ) {
        const PRIMER_FRAMES: u32 = 8192;
        const TERMINAL_FRAMES: u32 = 64;
        const QUANTUM_FRAMES: usize = 64;

        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let terminal_start = u64::from(PRIMER_FRAMES);
        let terminal_end = terminal_start.saturating_add(u64::from(TERMINAL_FRAMES));
        let steps = Arc::new(AtomicU64::new(0));
        let raw = TerminalRawSource {
            chunks: VecDeque::from([
                chunk_with_frames(&pools, spec, 0, PRIMER_FRAMES, 0.25),
                chunk_with_frames(&pools, spec, terminal_start, TERMINAL_FRAMES, -0.25),
            ]),
            steps: Arc::clone(&steps),
            seek: Arc::new(SeekState::new()),
        };
        let controls = StretchControls::new(1.0);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let config = kithara_warp::WarpConfig::builder()
            .stretch(Arc::clone(&controls))
            .render_quantum_frames(
                NonZeroUsize::new(QUANTUM_FRAMES).expect("test quantum is non-zero"),
            )
            .build();
        let warp = kithara_warp::Warp::new((), &config);
        let publisher = warp.publisher();
        let renderer = warp.quantum_renderer(spec, pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let mut source = WarpSource::new(raw, renderer, effects, drain, spec, pools.clone());

        let primer_context = RenderContext::new(
            SessionFrame::new(0)..SessionFrame::new(i64::from(PRIMER_FRAMES)),
            spec.sample_rate,
            None,
            SessionEpoch::new(7),
            None,
        )
        .expect("primer context is valid");
        publisher.publish(
            &primer_context,
            PresentationFrontier::builder()
                .source(0)
                .output(SessionFrame::new(0))
                .build(),
        );
        let mut primer_frontier = 0;
        for _ in 0..=usize::try_from(PRIMER_FRAMES).expect("test primer fits usize") {
            match source.step_track() {
                TrackStep::Produced(Fetch::Data {
                    data, source_end, ..
                }) => {
                    assert!(data.frames() <= QUANTUM_FRAMES);
                    if let Some(source_end) = source_end {
                        primer_frontier = primer_frontier.max(source_end.frame());
                    }
                }
                TrackStep::StateChanged => {}
                _ => panic!("unity primer must establish passthrough history"),
            }
            if primer_frontier == terminal_start {
                break;
            }
            flush_deferred(&mut source);
        }
        assert_eq!(primer_frontier, terminal_start);

        let sampled_revision = controls.set_speed(StretchControls::MIN_SPEED);
        let sampled_transport = TransportRevision::first();
        let sampled = RenderContext::new(
            SessionFrame::new(10_000)..SessionFrame::new(20_000),
            spec.sample_rate,
            None,
            SessionEpoch::new(7),
            Some(sampled_transport),
        )
        .expect("sampled context is valid");
        publisher.publish(
            &sampled,
            PresentationFrontier::builder()
                .source(terminal_start)
                .output(SessionFrame::new(10_000))
                .build(),
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        flush_deferred(&mut source);
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        let staged = source
            .prepared_quantum
            .expect("terminal source must leave a staged quantum");
        assert_eq!(staged.filled_frames, TERMINAL_FRAMES as usize);
        assert!(
            staged.frames > staged.filled_frames,
            "fixture must end before the prepared activation quantum"
        );

        let later_revision = controls.set_speed(2.0);
        let later_transport = sampled_transport
            .checked_next()
            .expect("test transport revision advances");
        let later = RenderContext::new(
            SessionFrame::new(20_000)..SessionFrame::new(30_000),
            spec.sample_rate,
            None,
            SessionEpoch::new(7),
            Some(later_transport),
        )
        .expect("later context is valid");
        publisher.publish(
            &later,
            PresentationFrontier::builder()
                .source(terminal_start)
                .output(SessionFrame::new(20_000))
                .build(),
        );
        assert_ne!(sampled_revision, later_revision);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        if exhaust_terminal_pool {
            let held_samples = source
                .terminal_input
                .as_ref()
                .map(BufferRing::len)
                .expect("raw EOF must move staged samples into the ring");
            source.terminal_chunk = Some(pools_with_budget(0).get::<f32>());
            flush_deferred(&mut source);
            assert!(source.quantum_failed, "terminal scratch growth must fail");
            for _ in 0..3 {
                assert!(matches!(source.step_track(), TrackStep::Failed));
                assert_eq!(
                    source.terminal_input.as_ref().map(BufferRing::len),
                    Some(held_samples),
                    "failed preparation must not consume terminal samples"
                );
            }
            assert_eq!(steps.load(Ordering::Acquire), 3, "raw EOF is not repolled");
            return;
        }
        flush_deferred(&mut source);
        flush_deferred(&mut source);

        let mut final_frontier = terminal_start;
        let mut reached_eof = false;
        for _ in 0..256 {
            match source.step_track() {
                TrackStep::Produced(Fetch::Data {
                    data, source_end, ..
                }) => {
                    assert!(
                        data.frames() <= QUANTUM_FRAMES,
                        "terminal output must stay inside the configured render quantum"
                    );
                    assert_eq!(data.meta.render_revision, sampled_revision);
                    if let Some(source_end) = source_end {
                        final_frontier = final_frontier.max(source_end.frame());
                    }
                }
                TrackStep::StateChanged => {}
                TrackStep::Eof => {
                    reached_eof = true;
                    break;
                }
                _ => panic!("terminal drain failed"),
            }
            flush_deferred(&mut source);
        }

        assert!(reached_eof, "terminal Warp drain must converge");
        assert_eq!(final_frontier, terminal_end);
        assert_eq!(
            source.warp.render_snapshot().map(RenderSnapshot::context),
            Some(&sampled),
            "terminal replan must retain the context sampled with its rate"
        );
        for _ in 0..3 {
            assert!(matches!(source.step_track(), TrackStep::Eof));
        }
        assert_eq!(steps.load(Ordering::Acquire), 3, "raw EOF is polled once");
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn unavailable_warp_target_fails_before_pulling_source(#[case] backend: StretchKind) {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let source_pools = pools();
        let head = Arc::new(AtomicU64::new(0));
        let raw = RawSource {
            chunks: VecDeque::from([chunk(&source_pools, spec, 0)]),
            head: Arc::clone(&head),
            seek: Arc::new(SeekState::new()),
        };
        let controls = StretchControls::new(0.5);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let config = kithara_warp::WarpConfig::builder()
            .stretch(controls)
            .build();
        let target_pools = pools_with_budget(0);
        let renderer =
            kithara_warp::Warp::new((), &config).quantum_renderer(spec, target_pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &target_pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let mut source = WarpSource::new(raw, renderer, effects, drain, spec, target_pools.clone());

        for _ in 0..3 {
            flush_deferred(&mut source);
            assert!(matches!(source.step_track(), TrackStep::Failed));
            assert_eq!(head.load(Ordering::Acquire), 0);
        }
    }

    #[kithara::test]
    fn seek_cancels_stale_tail_and_resets_effects_once() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let seek = Arc::new(SeekState::new());
        let resets = Arc::new(AtomicU64::new(0));
        let source = SeekApplyingSource {
            decode_epoch: 0,
            revision: 0,
            seek: Arc::clone(&seek),
            spec,
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResettingTail {
            resets: Arc::clone(&resets),
            tail: Some(chunk(&pools, spec, 128)),
        })];
        let mut source = source_stage(&pools, source, effects, spec);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            seek.begin(kithara_platform::time::Duration::from_secs(1)),
            1
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            resets.load(Ordering::Acquire),
            1,
            "stale seek drain resets renderers before the source adopts the epoch"
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(resets.load(Ordering::Acquire), 1);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert!(matches!(source.step_track(), TrackStep::Eof));
    }

    #[kithara::test]
    fn every_effect_tail_precedes_the_single_eof() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let source = RawSource {
            chunks: VecDeque::new(),
            head: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![
            Box::new(ResettingTail {
                resets: Arc::new(AtomicU64::new(0)),
                tail: Some(chunk(&pools, spec, 128)),
            }),
            Box::new(ResettingTail {
                resets: Arc::new(AtomicU64::new(0)),
                tail: Some(chunk(&pools, spec, 256)),
            }),
        ];
        let mut source = source_stage(&pools, source, effects, spec);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        for expected in [128, 256] {
            let TrackStep::Produced(Fetch::Data {
                data, source_end, ..
            }) = source.step_track()
            else {
                panic!("effect tail must be emitted before EOF");
            };
            assert_eq!(data.meta.frame_offset, expected);
            assert_eq!(source_end, None, "effect-only tails do not advance source");
        }
        assert!(matches!(source.step_track(), TrackStep::Eof));
    }

    #[kithara::test]
    fn exhausted_drain_stays_terminal_for_the_decode_epoch() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let steps = Arc::new(AtomicU64::new(0));
        let flushes = Arc::new(AtomicU64::new(0));
        let resets = Arc::new(AtomicU64::new(0));
        let discontinuity = Arc::new(Mutex::new(None));
        let seek = Arc::new(SeekState::new());
        let source = CountingEofSource {
            discontinuity: Arc::clone(&discontinuity),
            steps: Arc::clone(&steps),
            seek: Arc::clone(&seek),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(CountingEmptyTail {
            flushes: Arc::clone(&flushes),
            resets: Arc::clone(&resets),
        })];
        let mut source = source_stage(&pools, source, effects, spec);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert!(matches!(source.step_track(), TrackStep::Eof));
        for _ in 0..3 {
            assert!(matches!(source.step_track(), TrackStep::Eof));
        }

        assert_eq!(steps.load(Ordering::Acquire), 1);
        assert_eq!(flushes.load(Ordering::Acquire), 1);

        assert_eq!(
            seek.begin(kithara_platform::time::Duration::from_secs(1)),
            1
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert!(matches!(source.step_track(), TrackStep::Eof));
        assert!(matches!(source.step_track(), TrackStep::Eof));
        assert_eq!(steps.load(Ordering::Acquire), 2);
        assert_eq!(flushes.load(Ordering::Acquire), 2);

        *discontinuity.lock() = Some(SourceDiscontinuity::new(1, spec));
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert!(matches!(source.step_track(), TrackStep::Eof));
        assert!(matches!(source.step_track(), TrackStep::Eof));
        assert_eq!(steps.load(Ordering::Acquire), 3);
        assert_eq!(flushes.load(Ordering::Acquire), 3);
        assert_eq!(resets.load(Ordering::Acquire), 1);
    }
}
