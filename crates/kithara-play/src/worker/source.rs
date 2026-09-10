use kithara_audio::{AudioSource, Fetch, SourceDiscontinuity, SourceEnd, TrackStep};
use kithara_bufpool::{BufferRing, HasPool, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::SeekObserve;

use crate::effects::{
    AudioEffect, EffectDrain, EffectDrainStep, apply_effects, held_source_frames, reset_effects,
};

#[derive(Clone, Copy)]
enum DrainState {
    Open,
    LiveWarp(u64),
    Warp(u64),
    Effects(u64),
    Exhausted(u64),
}

impl DrainState {
    const fn epoch(self) -> Option<u64> {
        match self {
            Self::Open => None,
            Self::LiveWarp(epoch)
            | Self::Warp(epoch)
            | Self::Effects(epoch)
            | Self::Exhausted(epoch) => Some(epoch),
        }
    }
}

struct PendingInput {
    chunk: AudioChunk,
    epoch: u64,
    consumed_frames: usize,
}

/// The sole producer-side Warp/effect stage before the play output ring.
pub(crate) struct WarpSource<T, S> {
    seek: Arc<dyn SeekObserve>,
    spec: AudioSpec,
    drain_state: DrainState,
    drain: EffectDrain,
    discontinuity: Option<SourceDiscontinuity>,
    pending_input: Option<PendingInput>,
    prepared_frames: Option<usize>,
    render_input: Option<SampleBuffer>,
    reset_epoch: Option<u64>,
    retired_input: Option<AudioChunk>,
    staged_epoch: Option<u64>,
    staged_meta: Option<AudioChunkInfo>,
    staging: Option<BufferRing<SampleBuffer>>,
    pools: PoolRegion<S>,
    source: T,
    effects: Vec<Box<dyn AudioEffect>>,
    warp: kithara_warp::WarpRenderer<S>,
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
            pools,
            drain_state: DrainState::Open,
            reset_epoch: None,
            pending_input: None,
            staging: None,
            staged_meta: None,
            staged_epoch: None,
            prepared_frames: None,
            render_input: None,
            retired_input: None,
            quantum_failed: false,
        }
    }

    fn clear_staging(&mut self) {
        let Some(staging) = self.staging.take() else {
            self.staged_meta = None;
            self.staged_epoch = None;
            self.prepared_frames = None;
            return;
        };
        let buffer = staging.into_inner();
        self.staging = BufferRing::from_prefix(buffer, 0).ok();
        self.staged_meta = None;
        self.staged_epoch = None;
        self.prepared_frames = None;
    }

    fn discard_staged_input(&mut self) {
        self.retire_pending_input();
        self.clear_staging();
        self.quantum_failed = false;
    }

    fn prepare_staging(&mut self) {
        if self.quantum_failed {
            return;
        }
        if !self.warp.requires_staging() || self.prepared_frames.is_some() {
            return;
        }
        let Some((meta, remaining)) = self.pending_input.as_ref().and_then(|pending| {
            let remaining = pending
                .chunk
                .frames()
                .checked_sub(pending.consumed_frames)?;
            Some((
                Self::span_meta(pending.chunk.meta, pending.consumed_frames, remaining)?,
                remaining,
            ))
        }) else {
            return;
        };
        let Some(frames) = self.warp.prepare_quantum(meta, remaining) else {
            self.quantum_failed = true;
            return;
        };
        let frames = frames.get();
        let Some(required) = frames.checked_mul(usize::from(self.spec.channels.max(1))) else {
            self.quantum_failed = true;
            return;
        };

        let needs_staging = self
            .staging
            .as_ref()
            .is_none_or(|staging| staging.capacity() != required);
        if needs_staging {
            let mut buffer = self
                .staging
                .take()
                .map_or_else(|| self.pools.get::<f32>(), BufferRing::into_inner);
            if buffer.ensure_len(required).is_err() {
                self.quantum_failed = true;
                return;
            }
            buffer.truncate(required);
            let Ok(staging) = BufferRing::from_prefix(buffer, 0) else {
                self.quantum_failed = true;
                return;
            };
            self.staging = Some(staging);
        }

        let mut input = self
            .render_input
            .take()
            .unwrap_or_else(|| self.pools.get::<f32>());
        if input.ensure_len(required).is_err() {
            self.render_input = Some(input);
            self.quantum_failed = true;
            return;
        }
        self.render_input = Some(input);
        self.prepared_frames = Some(frames);
    }

    fn retire_pending_input(&mut self) {
        let Some(pending) = self.pending_input.take() else {
            return;
        };
        debug_assert!(self.retired_input.is_none());
        self.retired_input = Some(pending.chunk);
    }

    fn span_meta(original: AudioChunkInfo, offset: usize, frames: usize) -> Option<AudioChunkInfo> {
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

    fn stage_pending(&mut self) -> bool {
        let channels = usize::from(self.spec.channels.max(1));
        let Some(capacity) = self.staging.as_ref().map(BufferRing::capacity) else {
            return false;
        };
        let staged = self.staging.as_ref().map_or(0, BufferRing::len);
        let staged_frames = staged / channels;
        let Some(pending) = self.pending_input.as_mut() else {
            return false;
        };
        if pending.chunk.spec() != self.spec || pending.epoch != self.seek.epoch() {
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
        if staged > 0
            && (self.staged_epoch != Some(pending.epoch) || expected_frame != pending_frame)
        {
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
            self.staged_meta = Self::span_meta(pending.chunk.meta, pending.consumed_frames, 0);
            self.staged_epoch = Some(pending.epoch);
        }
        if !self
            .staging
            .as_mut()
            .is_some_and(|staging| staging.try_push(source))
        {
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
        self.staging.as_ref().map_or(0, BufferRing::len) / channels
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

    fn cancel_stale_drain(&mut self) -> bool {
        let stale = self
            .drain_state
            .epoch()
            .is_some_and(|epoch| self.seek.epoch() != epoch || self.source.decode_epoch() != epoch);
        if !stale {
            return false;
        }
        let epoch = self.seek.epoch();
        self.reset_renderers();
        self.drain.reset();
        self.drain_state = DrainState::Open;
        self.reset_epoch = Some(epoch);
        true
    }

    fn cancel_stale_input(&mut self) -> bool {
        let stale = self.pending_input.as_ref().is_some_and(|pending| {
            self.seek.epoch() != pending.epoch || self.source.decode_epoch() != pending.epoch
        }) || self
            .staged_epoch
            .is_some_and(|epoch| self.seek.epoch() != epoch || self.source.decode_epoch() != epoch);
        if !stale {
            return false;
        }
        let epoch = self.seek.epoch();
        self.discard_staged_input();
        self.reset_renderers();
        self.drain.reset();
        self.drain_state = DrainState::Open;
        self.reset_epoch = Some(epoch);
        true
    }

    fn drain_step(&mut self) -> Option<TrackStep<AudioChunk>> {
        if let DrainState::LiveWarp(epoch) = self.drain_state {
            let chunk = self.warp.flush();
            if !self.warp.transition_pending() {
                self.drain_state = DrainState::Open;
            }
            return Some(
                chunk
                    .and_then(|chunk| apply_effects(&mut self.effects, chunk))
                    .map_or(TrackStep::StateChanged, |output| {
                        TrackStep::Produced(self.fetch(output, epoch))
                    }),
            );
        }

        if let DrainState::Warp(epoch) = self.drain_state {
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

    fn prepare_renderers(&mut self, spec: AudioSpec) {
        self.spec = spec;
        self.warp.prepare(spec);
        self.prepare_staging();
        for effect in &mut self.effects {
            effect.service_deferred(spec);
        }
    }

    fn render(&mut self, chunk: AudioChunk, epoch: u64) -> Option<Fetch<AudioChunk>> {
        let chunk = self.warp.render(chunk);
        if self.warp.transition_pending() {
            self.drain_state = DrainState::LiveWarp(epoch);
        }
        let chunk = chunk?;
        let output = apply_effects(&mut self.effects, chunk)?;
        Some(self.fetch(output, epoch))
    }

    fn render_full_quantum(&mut self) -> Option<TrackStep<AudioChunk>> {
        let frames = self.prepared_frames?;
        (self.staged_frames() == frames).then(|| self.render_staged(frames))
    }

    fn render_quantum(&mut self, chunk: AudioChunk, epoch: u64) -> Option<Fetch<AudioChunk>> {
        let chunk = self.warp.render_quantum(chunk);
        if self.warp.transition_pending() {
            self.drain_state = DrainState::LiveWarp(epoch);
        }
        let chunk = chunk?;
        let output = apply_effects(&mut self.effects, chunk)?;
        Some(self.fetch(output, epoch))
    }

    fn render_staged(&mut self, frames: usize) -> TrackStep<AudioChunk> {
        if self.quantum_failed || frames == 0 {
            return TrackStep::Failed;
        }
        let channels = usize::from(self.spec.channels.max(1));
        let Some(samples) = frames.checked_mul(channels) else {
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        let Some(meta) = self
            .staged_meta
            .and_then(|meta| Self::span_meta(meta, 0, frames))
        else {
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        let Some(epoch) = self.staged_epoch else {
            self.quantum_failed = true;
            return TrackStep::Failed;
        };
        let Some(mut input) = self.render_input.take() else {
            return TrackStep::StateChanged;
        };
        if input.len() < samples {
            self.render_input = Some(input);
            self.quantum_failed = true;
            return TrackStep::Failed;
        }
        input.truncate(samples);
        if !self
            .staging
            .as_mut()
            .is_some_and(|staging| staging.try_pop_into(&mut input))
        {
            self.render_input = Some(input);
            self.quantum_failed = true;
            return TrackStep::Failed;
        }
        if self.staging.as_ref().is_none_or(BufferRing::is_empty) {
            self.staged_meta = None;
            self.staged_epoch = None;
        }
        self.prepared_frames = None;
        self.render_quantum(AudioChunk::new(meta, input), epoch)
            .map_or(TrackStep::StateChanged, TrackStep::Produced)
    }

    fn render_whole_pending(&mut self) -> Option<TrackStep<AudioChunk>> {
        let prepared = self.prepared_frames?;
        let pending = self.pending_input.as_ref()?;
        if pending.consumed_frames != 0 || pending.chunk.frames() != prepared {
            return None;
        }
        let pending = self.pending_input.take()?;
        self.prepared_frames = None;
        Some(
            self.render_quantum(pending.chunk, pending.epoch)
                .map_or(TrackStep::StateChanged, TrackStep::Produced),
        )
    }

    fn reset_renderers(&mut self) {
        self.warp.reset();
        reset_effects(&mut self.effects);
    }

    fn step_direct(&mut self) -> TrackStep<AudioChunk> {
        match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, epoch, .. }) => self
                .render(data, epoch)
                .map_or(TrackStep::StateChanged, TrackStep::Produced),
            TrackStep::Produced(fetch) => TrackStep::Produced(fetch),
            TrackStep::Eof => {
                self.begin_drain(self.source.decode_epoch());
                TrackStep::StateChanged
            }
            TrackStep::StateChanged => {
                self.sync_discontinuity();
                TrackStep::StateChanged
            }
            TrackStep::Blocked(reason) => TrackStep::Blocked(reason),
            TrackStep::Failed => TrackStep::Failed,
        }
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
        self.discard_staged_input();
        if already_reset {
            self.reset_epoch = None;
        } else {
            self.reset_renderers();
        }
        self.drain.reset();
        self.drain_state = DrainState::Open;
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

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek)
    }

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
        if !self.warp.requires_staging() {
            return self.step_direct();
        }
        if let Some(step) = self.render_whole_pending() {
            return step;
        }
        if let Some(step) = self.render_full_quantum() {
            return step;
        }
        if self.pending_input.is_some() {
            if self.prepared_frames.is_none() {
                return TrackStep::StateChanged;
            }
            self.stage_pending();
            return self
                .render_full_quantum()
                .unwrap_or(TrackStep::StateChanged);
        }
        if !self.warp.accepts_input() {
            return TrackStep::Failed;
        }

        match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, epoch, .. }) => {
                if data.spec() == self.spec
                    && self.prepared_frames.is_none()
                    && self
                        .warp
                        .prepare_quantum(data.meta, data.frames())
                        .is_some_and(|frames| frames.get() == data.frames())
                {
                    return self
                        .render_quantum(data, epoch)
                        .map_or(TrackStep::StateChanged, TrackStep::Produced);
                }
                self.pending_input = Some(PendingInput {
                    epoch,
                    chunk: data,
                    consumed_frames: 0,
                });
                TrackStep::StateChanged
            }
            TrackStep::Produced(fetch) => TrackStep::Produced(fetch),
            TrackStep::Eof => {
                self.begin_drain(self.source.decode_epoch());
                let frames = self.staged_frames();
                if frames == 0 {
                    TrackStep::StateChanged
                } else {
                    let Some(meta) = self.staged_meta else {
                        self.quantum_failed = true;
                        return TrackStep::Failed;
                    };
                    let Some(frames) = self.warp.prepare_terminal_quantum(meta, frames) else {
                        self.quantum_failed = true;
                        return TrackStep::Failed;
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
            TrackStep::Failed => TrackStep::Failed,
        }
    }

    delegate::delegate! {
        to self.source {
            fn decode_epoch(&self) -> u64;
            fn commit_source_end(&mut self, source_end: SourceEnd, epoch: u64);
            fn retire_chunk(&self, chunk: AudioChunk);
            fn finish_deferred(&mut self);
            fn warm_up(&mut self);
        }
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
    use kithara_test_fixtures::play_fixtures::{
        half, negative_half, negative_quarter, quarter, three_quarter,
    };
    use kithara_test_utils::kithara;
    use kithara_warp::{
        PresentationFrontier, RenderContext, SessionEpoch, SessionFrame, StretchControls,
        StretchKind, SyncMode, Warp,
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
        source_stage_with_quantum(pools, source, effects, spec, 128)
    }

    fn source_stage_with_quantum<T>(
        pools: &PoolRegion<TestPools>,
        source: T,
        effects: Vec<Box<dyn AudioEffect>>,
        spec: AudioSpec,
        quantum_frames: usize,
    ) -> WarpSource<T, TestPools>
    where
        T: AudioSource<Chunk = AudioChunk>,
    {
        let config = kithara_warp::WarpConfig::builder()
            .render_quantum_frames(
                NonZeroUsize::new(quantum_frames).expect("test quantum is non-zero"),
            )
            .build();
        let warp = Warp::new((), &config);
        let renderer = warp.renderer(spec, pools.clone());
        let drain = EffectDrain::new(effects.len(), pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        WarpSource::new(source, renderer, effects, drain, spec, pools.clone())
    }

    struct RawSource {
        head: Arc<AtomicU64>,
        seek: Arc<SeekState>,
        chunks: VecDeque<AudioChunk>,
    }

    impl AudioSource for RawSource {
        type Chunk = AudioChunk;

        fn decode_epoch(&self) -> u64 {
            self.seek.epoch()
        }

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
        fn held_source_frames(&self) -> u64 {
            self.buffered
                .as_ref()
                .map_or(0, |chunk| u64::from(chunk.meta.frames))
        }

        fn reset(&mut self) {
            self.buffered = None;
        }

        delegate::delegate! {
            to self.buffered {
                #[expr($.and_then(halve_frames))]
                #[call(take)]
                fn flush(&mut self) -> Option<AudioChunk>;
                #[expr($.and_then(halve_frames))]
                #[call(replace)]
                fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk>;
            }
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

        fn finish_deferred(&mut self) {
            self.log.lock().push("source.finish");
        }

        fn prepare_deferred(&mut self) -> Option<AudioSpec> {
            self.log.lock().push("source.prepare");
            Some(self.spec)
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

        fn service_deferred(&mut self, spec: AudioSpec) {
            self.log.lock().push("effect.service");
            *self.serviced.lock() = Some(spec);
        }
    }

    struct RevisionSource {
        discontinuity: Arc<Mutex<SourceDiscontinuity>>,
        seek: Arc<SeekState>,
        chunks: VecDeque<AudioChunk>,
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
        seek: Arc<SeekState>,
        steps: Arc<AtomicU64>,
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
        seek: Arc<SeekState>,
        spec: AudioSpec,
        decode_epoch: u64,
        revision: u64,
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

    fn chunk(
        pools: &PoolRegion<TestPools>,
        spec: AudioSpec,
        frame_offset: u64,
        input: &[f32],
    ) -> AudioChunk {
        const FRAMES: usize = 128;
        chunk_with_frames(
            pools,
            spec,
            frame_offset,
            u32::try_from(FRAMES).expect("fixture frames fit u32"),
            input,
        )
    }

    fn chunk_with_frames(
        pools: &PoolRegion<TestPools>,
        spec: AudioSpec,
        frame_offset: u64,
        frames: u32,
        input: &[f32],
    ) -> AudioChunk {
        let samples = usize::try_from(frames)
            .expect("fixture frames fit usize")
            .checked_mul(usize::from(spec.channels))
            .expect("fixture sample count fits usize");
        let mut buffer = pools
            .get_with_len::<f32>(samples)
            .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
        buffer.copy_from_slice(&input[..samples]);
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

    #[kithara::test(native)]
    #[case::q16(16)]
    #[case::q32(32)]
    fn unity_source_chunks_bypass_staging_without_losing_terminal_input(
        #[case] quantum_frames: usize,
        quarter: Vec<f32>,
        negative_half: Vec<f32>,
        three_quarter: Vec<f32>,
    ) {
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
        let pools = pools();
        let input_frames = [20_usize, 20, 10];
        let chunks = [
            chunk_with_frames(&pools, spec, 0, 20, &quarter),
            chunk_with_frames(&pools, spec, 20, 20, &negative_half),
            chunk_with_frames(&pools, spec, 40, 10, &three_quarter),
        ];
        let expected_samples = chunks
            .iter()
            .flat_map(|chunk| chunk.samples.iter().copied())
            .collect::<Vec<_>>();
        let source = RawSource {
            chunks: VecDeque::from(chunks),
            head: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        };
        let mut source =
            source_stage_with_quantum(&pools, source, Vec::new(), spec, quantum_frames);
        let mut output_frames = Vec::new();
        let mut output_samples = Vec::new();

        for _ in 0..32 {
            match source.step_track() {
                TrackStep::Produced(Fetch::Data { data, .. }) => {
                    output_frames.push(data.frames());
                    output_samples.extend_from_slice(&data.samples);
                }
                TrackStep::StateChanged => {}
                TrackStep::Eof => break,
                _ => panic!("staged source must only produce, progress, or finish"),
            }
            flush_deferred(&mut source);
        }

        assert!(output_frames.iter().all(|frames| *frames <= quantum_frames));
        assert_eq!(
            output_frames.iter().sum::<usize>(),
            input_frames.iter().copied().sum::<usize>()
        );
        assert_eq!(output_samples, expected_samples);
    }

    #[kithara::test]
    #[case::unity(1.0, 1_387)]
    #[case::stretched(1.5, 925)]
    fn decoder_timestamp_gap_does_not_discard_pcm(
        #[case] rate: f32,
        #[case] minimum_frames: usize,
    ) {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let input = vec![0.25; 2_048];
        let source = RawSource {
            chunks: VecDeque::from([
                chunk_with_frames(&pools, spec, 1_024, 363, &input),
                chunk_with_frames(&pools, spec, 2_048, 1_024, &input),
            ]),
            head: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        };
        let config = kithara_warp::WarpConfig::builder()
            .stretch(StretchControls::new(rate))
            .render_quantum_frames(NonZeroUsize::new(32).expect("test quantum"))
            .build();
        let warp = Warp::new((), &config);
        let renderer = warp.renderer(spec, pools.clone());
        let drain = EffectDrain::new(0, &pools).expect("empty effect drain");
        let mut source = WarpSource::new(source, renderer, Vec::new(), drain, spec, pools);
        let mut output_frames = 0;
        let mut eof = false;

        for _ in 0..512 {
            flush_deferred(&mut source);
            match source.step_track() {
                TrackStep::Produced(Fetch::Data { data, .. }) => output_frames += data.frames(),
                TrackStep::StateChanged => {}
                TrackStep::Eof => {
                    eof = true;
                    break;
                }
                _ => panic!("timestamp gaps must not fail decoded PCM delivery"),
            }
        }

        assert!(eof, "both decoded ranges must reach EOF");
        assert!(
            output_frames >= minimum_frames,
            "both decoded ranges must be rendered: {output_frames} < {minimum_frames}"
        );
        assert_eq!(source.source.chunks.len(), 0, "both source chunks consumed");
    }

    #[kithara::test]
    fn buffered_frame_changing_effect_tracks_live_and_flush_frontiers(quarter: Vec<f32>) {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let head = Arc::new(AtomicU64::new(0));
        let source = RawSource {
            chunks: VecDeque::from([
                chunk(&pools, spec, 0, &quarter),
                chunk(&pools, spec, u64::from(128_u32), &quarter),
            ]),
            head: Arc::clone(&head),
            seek: Arc::new(SeekState::new()),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::<BufferThenHalveFrames>::default()];
        let mut source = source_stage(&pools, source, effects, spec);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            head.load(Ordering::Acquire),
            128,
            "one worker pass advances exactly one source transition"
        );
        flush_deferred(&mut source);
        let mut produced = None;
        for _ in 0..3 {
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    produced = Some(fetch);
                    break;
                }
                TrackStep::StateChanged => flush_deferred(&mut source),
                _ => panic!("the second raw chunk must release the first buffered span"),
            }
        }
        let Some(Fetch::Data {
            data, source_end, ..
        }) = produced
        else {
            panic!("the second raw chunk must release the first buffered span");
        };

        assert_eq!(head.load(Ordering::Acquire), 256);
        assert_eq!(data.meta.frame_offset, 0);
        assert_eq!(data.meta.frames, 64);
        assert_eq!(
            source_end,
            Some(SourceEnd::new(
                128,
                NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
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

        assert_eq!(data.meta.frame_offset, 128);
        assert_eq!(data.meta.frames, 64);
        assert_eq!(
            source_end,
            Some(SourceEnd::new(
                256,
                NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
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
            spec,
            log: Arc::clone(&log),
            seek: Arc::new(SeekState::new()),
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
    fn unity_warp_preserves_samples_and_meta_across_discontinuity(
        quarter: Vec<f32>,
        negative_quarter: Vec<f32>,
    ) {
        let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
        let changed = AudioSpec::new(1, NonZeroU32::new(48_000).expect("changed rate"));
        let pools = pools();
        let first = chunk(&pools, initial, 256, &quarter);
        let first_meta = first.meta;
        let first_samples = first.samples.to_vec();
        let mut second = chunk(&pools, changed, 512, &negative_quarter);
        second.meta.segment_index = Some(3);
        second.meta.variant_index = Some(2);
        second.meta.epoch = 9;
        second.meta.source_byte_offset = Some(4096);
        second.meta.source_bytes = 1024;
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
        assert_eq!(&data.samples[..], &first_samples);

        *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("post-discontinuity unity span must pass through");
        };
        assert_eq!(data.meta, second_meta);
        assert_eq!(&data.samples[..], &second_samples);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn live_warp_drain_holds_the_source_and_seek_discards_stale_unity(
        #[case] backend: StretchKind,
        half: Vec<f32>,
        quarter: Vec<f32>,
        three_quarter: Vec<f32>,
    ) {
        const ACTIVE_FRAMES: u32 = 4096;
        const UNITY_FRAMES: u32 = 4096;
        const SENTINEL_FRAMES: u32 = 4096;

        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let first_active_end = u64::from(ACTIVE_FRAMES);
        let first_unity_end = first_active_end.saturating_add(u64::from(UNITY_FRAMES));
        let sentinel_end = first_unity_end.saturating_add(u64::from(SENTINEL_FRAMES));

        let first_active = chunk_with_frames(&pools, spec, 0, ACTIVE_FRAMES, &quarter);
        let first_unity = chunk_with_frames(&pools, spec, first_active_end, UNITY_FRAMES, &half);
        let sentinel = chunk_with_frames(
            &pools,
            spec,
            first_unity_end,
            SENTINEL_FRAMES,
            &three_quarter,
        );
        let sentinel_ptr = sentinel.samples.as_ptr();
        let sentinel_samples = sentinel.samples.to_vec();

        let head = Arc::new(AtomicU64::new(0));
        let seek = Arc::new(SeekState::new());
        let raw = RawSource {
            chunks: VecDeque::from([first_active, first_unity, sentinel]),
            head: Arc::clone(&head),
            seek: Arc::clone(&seek),
        };
        let controls = StretchControls::new(0.5);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let render_quantum_frames = usize::try_from(ACTIVE_FRAMES)
            .expect("test quantum fits usize")
            .saturating_mul(2)
            .saturating_add(1);
        let config = kithara_warp::WarpConfig::builder()
            .stretch(Arc::clone(&controls))
            .render_quantum_frames(
                NonZeroUsize::new(render_quantum_frames).expect("test quantum is non-zero"),
            )
            .build();
        let mut warp = Warp::new((), &config);
        let publisher = warp.take_publisher().expect("fixture owns publisher");
        let renderer = warp.renderer(spec, pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let mut source = WarpSource::new(raw, renderer, effects, drain, spec, pools.clone());

        let initial = source.step_track();
        assert!(matches!(
            &initial,
            TrackStep::Produced(_) | TrackStep::StateChanged
        ));
        assert_eq!(head.load(Ordering::Acquire), first_active_end);
        flush_deferred(&mut source);
        if matches!(&initial, TrackStep::StateChanged) {
            let TrackStep::Produced(_) = source.step_track() else {
                panic!("the first active quantum must render");
            };
            flush_deferred(&mut source);
        }

        controls.set_speed(1.0);
        let frame = SessionFrame::new(i64::from(ACTIVE_FRAMES));
        let context = RenderContext::new(
            frame..frame,
            spec.sample_rate,
            None,
            SessionEpoch::new(0),
            None,
        )
        .expect("fixture context")
        .with_rate(SyncMode::Off, controls.rate_target());
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(first_active_end)
                .output(frame)
                .build(),
        );
        let transition = source.step_track();
        assert!(matches!(
            &transition,
            TrackStep::Produced(_) | TrackStep::StateChanged
        ));
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        flush_deferred(&mut source);
        if matches!(&transition, TrackStep::StateChanged) {
            let TrackStep::Produced(_) = source.step_track() else {
                panic!("active-to-unity transition must emit its first tail quantum");
            };
        }
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        assert!(source.warp.transition_pending());

        assert_eq!(
            seek.begin(kithara_platform::time::Duration::from_secs(1)),
            1
        );
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        assert!(!source.warp.transition_pending());

        flush_deferred(&mut source);
        let resumed = source.step_track();
        let resumed = match resumed {
            TrackStep::Produced(fetch) => fetch,
            TrackStep::StateChanged => {
                flush_deferred(&mut source);
                let TrackStep::Produced(fetch) = source.step_track() else {
                    panic!("playback must resume with the post-seek source chunk");
                };
                fetch
            }
            _ => panic!("playback must resume with the post-seek source chunk"),
        };
        let Fetch::Data { data, .. } = resumed else {
            panic!("playback must resume with post-seek audio data");
        };
        assert_eq!(head.load(Ordering::Acquire), sentinel_end);
        assert_eq!(data.samples.as_ptr(), sentinel_ptr);
        assert_eq!(&data.samples[..], &sentinel_samples);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn unavailable_warp_target_fails_before_pulling_source(
        #[case] backend: StretchKind,
        quarter: Vec<f32>,
    ) {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let source_pools = pools();
        let head = Arc::new(AtomicU64::new(0));
        let raw = RawSource {
            chunks: VecDeque::from([chunk(&source_pools, spec, 0, &quarter)]),
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
        let renderer = Warp::new((), &config).renderer(spec, target_pools.clone());
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
    fn seek_cancels_stale_tail_and_resets_effects_once(quarter: Vec<f32>) {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let seek = Arc::new(SeekState::new());
        let resets = Arc::new(AtomicU64::new(0));
        let source = SeekApplyingSource {
            spec,
            decode_epoch: 0,
            revision: 0,
            seek: Arc::clone(&seek),
        };
        let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResettingTail {
            resets: Arc::clone(&resets),
            tail: Some(chunk(&pools, spec, 128, &quarter)),
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
    fn every_effect_tail_precedes_the_single_eof(quarter: Vec<f32>) {
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
                tail: Some(chunk(&pools, spec, 128, &quarter)),
            }),
            Box::new(ResettingTail {
                resets: Arc::new(AtomicU64::new(0)),
                tail: Some(chunk(&pools, spec, 256, &quarter)),
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
