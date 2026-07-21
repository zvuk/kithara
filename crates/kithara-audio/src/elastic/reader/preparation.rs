use kithara_events::PlaybackDirection;

use super::{
    super::{ElasticAnchor, ElasticReaderError},
    Active, BufferedSourceWindow, ElasticPreparation, ElasticPreparationPoll, ElasticReader,
    PreparationPhase, PreparedRuntime, Preparing, Ready, RenderRuntime, Unprepared,
    rendering::SourceCopy,
    sample_count,
};
use crate::{PcmReader, SourceRange, SourceRangeReadOutcome, SourceRangeRequest};

type ElasticPrepareError = ElasticReaderError;

fn expand_preparation_history(
    max_warm_frames: usize,
    source_frame_count: u64,
    preparation: &mut ElasticPreparation,
) -> Result<Option<SourceRange>, ElasticPrepareError> {
    let additional = max_warm_frames
        .checked_sub(preparation.warmup.source_frames())
        .ok_or(ElasticPrepareError::FrameOverflow)?;
    let additional = u64::try_from(additional).map_err(|_| ElasticPrepareError::FrameOverflow)?;
    let range = preparation.fetch_range;
    let extension = match preparation.direction {
        PlaybackDirection::Forward => {
            let start = range.start().get().saturating_sub(additional);
            (start < range.start().get()).then(|| SourceRange::try_from(start..range.start().get()))
        }
        PlaybackDirection::Reverse => {
            let end = range
                .end()
                .get()
                .checked_add(additional)
                .ok_or(ElasticPrepareError::FrameOverflow)?
                .min(source_frame_count);
            (range.end().get() < end).then(|| SourceRange::try_from(range.end().get()..end))
        }
    }
    .transpose()?;
    if let Some(extension) = extension {
        preparation.fetch_range = SourceRange::try_from(
            range.start().get().min(extension.start().get())
                ..range.end().get().max(extension.end().get()),
        )?;
    }
    Ok(extension)
}

impl ElasticReader<Unprepared> {
    /// Starts bounded source preparation for a validated source anchor.
    /// # Errors
    /// Returns a typed source, range, direction, or preparation error.
    pub fn begin<R>(
        self,
        source: &mut R,
        anchor: ElasticAnchor,
    ) -> Result<ElasticReader<Preparing>, ElasticPrepareError>
    where
        R: PcmReader + ?Sized,
    {
        if anchor.direction() == PlaybackDirection::Reverse
            && !self.backend.capabilities().supports_reverse()
        {
            return Err(ElasticPrepareError::ReverseUnsupported);
        }
        let mut preparation = self.plan_preparation(anchor, self.config.prefetch_blocks())?;
        let range = preparation.fetch_range;
        let extension = expand_preparation_history(
            self.max_warm_frames,
            self.source_frame_count,
            &mut preparation,
        )?;
        let request = source.request_source_range(range)?;
        Ok(self.map_state(|Unprepared| Preparing {
            preparation,
            phase: PreparationPhase::Priming {
                request,
                range,
                extension,
            },
        }))
    }
}

impl ElasticReader<Preparing> {
    fn begin_preparation_window<R>(
        &mut self,
        source: &mut R,
        current: SourceRange,
        runtime: PreparedRuntime,
    ) -> Result<bool, ElasticPrepareError>
    where
        R: PcmReader + ?Sized,
    {
        let Some(range) = self.next_source_window(current, runtime.direction)? else {
            return Ok(false);
        };
        let request = source.request_source_range(range)?;
        self.state.phase = PreparationPhase::Window {
            request,
            range,
            runtime,
        };
        Ok(true)
    }
}

impl<State> ElasticReader<State> {
    pub(super) fn plan_preparation(
        &self,
        anchor: ElasticAnchor,
        prefetch_blocks: usize,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        let capabilities = self.backend.capabilities();
        let source_cursor = anchor.cursor();
        let anchor_integer = source_cursor.integer();
        let warmup = capabilities.warmup_request(anchor.source_frames_per_output())?;
        let before = capabilities
            .latency()
            .source_frames()
            .checked_add(warmup.source_frames())
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let before = i64::try_from(before).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let after = self
            .max_source_frames
            .checked_mul(prefetch_blocks)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let after = i64::try_from(after).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let source_end = i64::try_from(self.source_frame_count)
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let (start, end) = match anchor.direction() {
            PlaybackDirection::Forward => (
                anchor_integer.saturating_sub(before).max(0),
                anchor_integer
                    .checked_add(after)
                    .ok_or(ElasticPrepareError::FrameOverflow)?
                    .min(source_end),
            ),
            PlaybackDirection::Reverse => (
                anchor_integer.saturating_sub(after).max(0),
                anchor_integer
                    .checked_add(before)
                    .ok_or(ElasticPrepareError::FrameOverflow)?
                    .min(source_end),
            ),
        };
        if start >= end {
            return Err(ElasticPrepareError::SourceEnded);
        }
        let start = u64::try_from(start).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let end = u64::try_from(end).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let range = SourceRange::try_from(start..end)?;
        Ok(ElasticPreparation {
            warmup,
            anchor: source_cursor,
            direction: anchor.direction(),
            fetch_range: range,
        })
    }
}

impl ElasticReader<Preparing> {
    /// Polls bounded source preparation without blocking.
    /// # Errors
    /// Returns a typed source, range, or DSP preparation error.
    pub fn poll<R>(self, source: &mut R) -> Result<ElasticPreparationPoll, ElasticPrepareError>
    where
        R: PcmReader + ?Sized,
    {
        match self.state.phase {
            PreparationPhase::Priming {
                request,
                range,
                extension,
            } => self.poll_priming(source, request, range, extension),
            PreparationPhase::Window {
                request,
                range,
                runtime,
            } => self.poll_preparation_window(source, request, range, runtime),
        }
    }

    fn poll_preparation_window<R>(
        mut self,
        source: &mut R,
        request: SourceRangeRequest,
        range: SourceRange,
        runtime: PreparedRuntime,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError>
    where
        R: PcmReader + ?Sized,
    {
        let frames =
            usize::try_from(range.len()).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        if frames > self.max_fetch_frames {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        let samples = sample_count(frames, self.backend.capabilities().channels())?;
        let buffer = self
            .window_buffers
            .last_mut()
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        match source.read_source_range(request, &mut buffer[..samples])? {
            SourceRangeReadOutcome::Ready { .. } => {}
            SourceRangeReadOutcome::Pending => {
                return Ok(ElasticPreparationPoll::Pending(self));
            }
            SourceRangeReadOutcome::Eof => return Err(ElasticPrepareError::SourceEnded),
        }
        let samples = self
            .window_buffers
            .pop()
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        self.ready_windows
            .push_back(BufferedSourceWindow { samples, range });
        if self.ready_windows.len() < self.config.ready_window_count()
            && self.begin_preparation_window(source, range, runtime)?
        {
            return Ok(ElasticPreparationPoll::Pending(self));
        }
        Ok(self.ready(runtime))
    }

    fn poll_priming<R>(
        mut self,
        source: &mut R,
        request: SourceRangeRequest,
        range: SourceRange,
        extension: Option<SourceRange>,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError>
    where
        R: PcmReader + ?Sized,
    {
        let preparation = self.state.preparation;
        let fetch_frames = usize::try_from(preparation.fetch_range.len())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        let channels = self.backend.capabilities().channels();
        let fetch_samples = sample_count(fetch_frames, channels)?;
        let request_frames =
            usize::try_from(range.len()).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let request_offset = range
            .start()
            .get()
            .checked_sub(preparation.fetch_range.start().get())
            .ok_or(ElasticPrepareError::FetchWindowMismatch)?;
        let request_offset =
            usize::try_from(request_offset).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let request_sample_start = sample_count(request_offset, channels)?;
        let request_sample_end = request_sample_start
            .checked_add(sample_count(request_frames, channels)?)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        match source.read_source_range(
            request,
            &mut self.fetch[request_sample_start..request_sample_end],
        )? {
            SourceRangeReadOutcome::Ready { .. } => {}
            SourceRangeReadOutcome::Pending => {
                return Ok(ElasticPreparationPoll::Pending(self));
            }
            SourceRangeReadOutcome::Eof => return Err(ElasticPrepareError::SourceEnded),
        }
        if let Some(extension) = extension {
            let request = source.request_source_range(extension)?;
            self.state.phase = PreparationPhase::Priming {
                request,
                range: extension,
                extension: None,
            };
            return Ok(ElasticPreparationPoll::Pending(self));
        }
        self.prime(preparation, fetch_samples)?;
        let runtime = PreparedRuntime {
            direction: preparation.direction,
            cursor: preparation.anchor,
            source_window: preparation.fetch_range,
        };
        if preparation.direction == PlaybackDirection::Reverse
            && self.begin_preparation_window(source, preparation.fetch_range, runtime)?
        {
            return Ok(ElasticPreparationPoll::Pending(self));
        }
        Ok(self.ready(runtime))
    }

    fn ready(self, prepared: PreparedRuntime) -> ElasticPreparationPoll {
        ElasticPreparationPoll::Ready(self.map_state(|_| Ready {
            runtime: RenderRuntime {
                prepared,
                pending_source_read: None,
                relocation: None,
            },
        }))
    }
}

impl<State> ElasticReader<State> {
    pub(super) fn prime(
        &mut self,
        preparation: ElasticPreparation,
        fetch_samples: usize,
    ) -> Result<(), ElasticPrepareError> {
        let capabilities = self.backend.capabilities();
        let channels = capabilities.channels();
        let history_frames = capabilities.latency().source_frames();
        let warm_source_frames = preparation.warmup.source_frames();
        let warm_source_frames =
            i64::try_from(warm_source_frames).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let history_frames_i64 =
            i64::try_from(history_frames).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let history_start = match preparation.direction {
            PlaybackDirection::Forward => preparation
                .anchor
                .integer()
                .checked_sub(warm_source_frames)
                .and_then(|start| start.checked_sub(history_frames_i64)),
            PlaybackDirection::Reverse => preparation
                .anchor
                .integer()
                .checked_add(warm_source_frames)
                .and_then(|start| start.checked_add(history_frames_i64)),
        }
        .ok_or(ElasticPrepareError::FrameOverflow)?;
        SourceCopy {
            channels,
            start: history_start,
            frames: history_frames,
            direction: preparation.direction,
            fetch_range: preparation.fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.history,
            source_frame_count: self.source_frame_count,
        }
        .copy()
        .map_err(ElasticPrepareError::from)?;
        let warmup_start = match preparation.direction {
            PlaybackDirection::Forward => {
                preparation.anchor.integer().checked_sub(warm_source_frames)
            }
            PlaybackDirection::Reverse => {
                preparation.anchor.integer().checked_add(warm_source_frames)
            }
        }
        .ok_or(ElasticPrepareError::FrameOverflow)?;
        SourceCopy {
            channels,
            start: warmup_start,
            frames: usize::try_from(warm_source_frames)
                .map_err(|_| ElasticPrepareError::FrameOverflow)?,
            direction: preparation.direction,
            fetch_range: preparation.fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.source,
            source_frame_count: self.source_frame_count,
        }
        .copy()
        .map_err(ElasticPrepareError::from)?;
        self.backend.prime(
            preparation.warmup,
            &self.history[..sample_count(history_frames, channels)
                .map_err(|_| ElasticPrepareError::FrameOverflow)?],
            &self.source[..sample_count(preparation.warmup.source_frames(), channels)?],
            &mut self.discarded[..sample_count(preparation.warmup.output_frames(), channels)?],
        )?;
        Ok(())
    }
}

impl ElasticReader<Ready> {
    #[must_use]
    pub fn activate(self) -> ElasticReader<Active> {
        self.map_state(|Ready { runtime }| Active { runtime })
    }

    /// Re-primes the ready reader at an anchor inside its retained PCM window.
    /// # Errors
    /// Returns a typed range or DSP preparation error.
    pub fn retarget(&mut self, anchor: ElasticAnchor) -> Result<(), ElasticPrepareError> {
        let preparation = self.retarget_preparation(anchor)?;
        let fetch_frames = usize::try_from(preparation.fetch_range.len())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let fetch_samples = sample_count(fetch_frames, self.backend.capabilities().channels())?;
        self.prime(preparation, fetch_samples)?;
        self.state.runtime.prepared.cursor = preparation.anchor;
        self.state.runtime.prepared.direction = preparation.direction;
        Ok(())
    }

    fn retarget_preparation(
        &self,
        anchor: ElasticAnchor,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        let mut preparation = self.plan_preparation(anchor, self.config.prefetch_blocks())?;
        let fetch_range = self.state.runtime.prepared.source_window;
        if fetch_range.start() > preparation.fetch_range.start()
            || fetch_range.end() < preparation.fetch_range.end()
        {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        preparation.fetch_range = fetch_range;
        Ok(preparation)
    }

    /// Checks whether the retained PCM window can support an anchor.
    /// # Errors
    /// Returns a typed range or preparation error without mutating the reader.
    pub fn validate_retarget(&self, anchor: ElasticAnchor) -> Result<(), ElasticPrepareError> {
        self.retarget_preparation(anchor).map(drop)
    }
}

impl From<ElasticReader<Active>> for ElasticReader<Ready> {
    fn from(reader: ElasticReader<Active>) -> Self {
        reader.map_state(|Active { runtime }| Ready { runtime })
    }
}

#[cfg(test)]
mod tests {
    use std::{mem::replace, num::NonZeroU32};

    use kithara_bufpool::PcmPool;
    use kithara_decode::PcmSpec;
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{super::ElasticReaderConfig, *};

    struct TestConfig {
        sample_rate: u32,
        pcm_max_buffers: usize,
        pcm_trim_capacity: usize,
    }

    const CONFIG: TestConfig = TestConfig {
        sample_rate: 44_100,
        pcm_max_buffers: 32,
        pcm_trim_capacity: 0,
    };

    fn test_pool() -> PcmPool {
        PcmPool::new(CONFIG.pcm_max_buffers, CONFIG.pcm_trim_capacity)
    }

    fn source_frames() -> u64 {
        u64::from(CONFIG.sample_rate) * 5
    }

    fn reader(pool: &PcmPool) -> ElasticReader<Unprepared> {
        let sample_rate =
            NonZeroU32::new(CONFIG.sample_rate).expect("invariant: static sample rate");
        ElasticReader::<Unprepared>::allocate(
            PcmSpec::new(1, sample_rate),
            source_frames(),
            sample_rate,
            NonZeroU32::new(512).expect("invariant: static block size"),
            2,
            ElasticReaderConfig::default(),
            pool,
        )
        .expect("invariant: elastic reader allocation")
    }

    fn active_reader(
        pool: &PcmPool,
        source_window: SourceRange,
        direction: PlaybackDirection,
    ) -> ElasticReader<Active> {
        let integer =
            i64::try_from(source_window.start().get()).expect("invariant: test range fits i64");
        let cursor = ElasticAnchor::try_from((
            integer
                .to_f64()
                .expect("invariant: test range converts to f64"),
            1.0,
            direction,
        ))
        .expect("invariant: test anchor is representable")
        .cursor();
        reader(pool).map_state(|Unprepared| Active {
            runtime: RenderRuntime {
                pending_source_read: None,
                relocation: None,
                prepared: PreparedRuntime {
                    cursor,
                    direction,
                    source_window,
                },
            },
        })
    }

    fn anchor(direction: PlaybackDirection, rate: f64) -> ElasticAnchor {
        ElasticAnchor::try_from((f64::from(CONFIG.sample_rate) * 2.5, rate, direction))
            .expect("invariant: valid numeric elastic anchor")
    }

    #[kithara::test]
    fn direction_and_rate_stress_stays_inside_configured_preparation_budgets() {
        let pool = test_pool();
        let reader = reader(&pool);
        let max_fetch_frames =
            u64::try_from(reader.max_fetch_frames).expect("invariant: fetch budget fits u64");
        let envelope = reader.capabilities().rate_envelope();
        let rates = [
            envelope.min_source_frames_per_output(),
            0.8,
            1.0,
            1.2,
            envelope.max_source_frames_per_output(),
        ];
        for direction in [PlaybackDirection::Forward, PlaybackDirection::Reverse] {
            for rate in rates {
                let preparation = reader
                    .plan_preparation(anchor(direction, rate), reader.config.prefetch_blocks())
                    .expect("invariant: supported rate prepares inside configured buffers");

                assert!(preparation.fetch_range.start() < preparation.fetch_range.end());
                assert!(preparation.fetch_range.len() <= max_fetch_frames);
                assert!(preparation.warmup.source_frames() <= reader.max_warm_frames);
            }
        }
    }

    fn assert_window_stress(direction: PlaybackDirection, initial: SourceRange) {
        const PROMOTIONS: usize = 24;

        let pool = test_pool();
        let mut reader = active_reader(&pool, initial, direction);
        let ready_window_count = reader.config.ready_window_count();
        let ready_capacity = reader.ready_windows.capacity();
        let buffer_capacity = reader.window_buffers.capacity();
        while reader.ready_windows.len() < ready_window_count {
            let frontier = reader
                .ready_windows
                .back()
                .map_or(initial, |window| window.range);
            let range = reader
                .next_source_window(frontier, direction)
                .expect("invariant: successor range calculation")
                .expect("invariant: source has another successor window");
            let samples = reader
                .window_buffers
                .pop()
                .expect("invariant: fixed window buffer is available");
            reader
                .stage_ready_window(range, samples, direction)
                .expect("invariant: successor advances the directional frontier");
        }
        assert_eq!(fixed_buffer_count(&reader), ready_window_count + 1);

        for _ in 0..PROMOTIONS {
            let window = reader
                .ready_windows
                .pop_front()
                .expect("invariant: a ready window is available");
            let next = window.range;
            let old = replace(&mut reader.fetch, window.samples);
            reader.window_buffers.push(old);
            reader.state.runtime.prepared.source_window = next;
            assert_eq!(reader.state.runtime.prepared.source_window, next);
            assert_eq!(reader.ready_windows.len(), ready_window_count - 1);

            let frontier = reader
                .ready_windows
                .back()
                .map_or(next, |ready| ready.range);
            let range = reader
                .next_source_window(frontier, direction)
                .expect("invariant: successor range calculation")
                .expect("invariant: stress range stays away from source boundary");
            let samples = reader
                .window_buffers
                .pop()
                .expect("invariant: promoted buffer replenishes the fixed bank");
            reader
                .stage_ready_window(range, samples, direction)
                .expect("invariant: scheduled successor restores the full pipeline");
            assert_eq!(reader.ready_windows.len(), ready_window_count);
            assert_eq!(fixed_buffer_count(&reader), ready_window_count + 1);
            assert_eq!(reader.ready_windows.capacity(), ready_capacity);
            assert_eq!(reader.window_buffers.capacity(), buffer_capacity);
        }
    }

    fn fixed_buffer_count(reader: &ElasticReader<Active>) -> usize {
        let primary_pending = usize::from(reader.state.runtime.pending_source_read.is_some());
        1 + reader.window_buffers.len() + reader.ready_windows.len() + primary_pending
    }

    #[kithara::test]
    fn forward_and_reverse_window_stress_never_exceeds_the_ready_bound() {
        assert_window_stress(
            PlaybackDirection::Forward,
            SourceRange::try_from(10_000..20_000).expect("invariant: valid forward window"),
        );
        assert_window_stress(
            PlaybackDirection::Reverse,
            SourceRange::try_from(200_000..210_000).expect("invariant: valid reverse window"),
        );
    }

    #[kithara::test]
    fn reverse_preparation_requests_one_ascending_bounded_range() {
        let pool = test_pool();
        let reader = reader(&pool);
        let preparation = reader
            .plan_preparation(
                anchor(PlaybackDirection::Reverse, 1.0),
                reader.config.prefetch_blocks(),
            )
            .expect("invariant: reverse preparation plan");
        let anchor =
            u64::try_from(preparation.anchor.integer()).expect("invariant: positive source anchor");
        let prefetch = u64::try_from(reader.max_source_frames * reader.config.prefetch_blocks())
            .expect("invariant: prefetch span fits u64");
        let history = u64::try_from(
            reader.backend.capabilities().latency().source_frames()
                + preparation.warmup.source_frames(),
        )
        .expect("invariant: history span fits u64");

        assert_eq!(anchor - preparation.fetch_range.start().get(), prefetch);
        assert_eq!(preparation.fetch_range.end().get() - anchor, history);
        assert!(preparation.fetch_range.end().get() <= source_frames());
    }

    #[kithara::test]
    fn reverse_preparation_fetches_extra_tempo_history_as_a_separate_range() {
        let pool = test_pool();
        let reader = reader(&pool);
        let mut preparation = reader
            .plan_preparation(
                anchor(PlaybackDirection::Reverse, 1.0),
                reader.config.prefetch_blocks(),
            )
            .expect("invariant: reverse preparation plan");
        let request = preparation.fetch_range;

        let extension = expand_preparation_history(
            reader.max_warm_frames,
            reader.source_frame_count,
            &mut preparation,
        )
        .expect("invariant: expanded preparation history")
        .expect("invariant: maximum rate needs additional history");

        assert_eq!(extension.start(), request.end());
        assert_eq!(preparation.fetch_range.start(), request.start());
        assert_eq!(preparation.fetch_range.end(), extension.end());
        assert_eq!(
            extension.len(),
            u64::try_from(reader.max_warm_frames - preparation.warmup.source_frames())
                .expect("invariant: history extension fits u64")
        );
    }

    #[kithara::test]
    fn reverse_window_renewal_keeps_the_active_window_until_successor_use() {
        let pool = test_pool();
        let current =
            SourceRange::try_from(10_000..20_000).expect("invariant: valid current source window");
        let mut reader = active_reader(&pool, current, PlaybackDirection::Reverse);
        let ready_window_count = reader.config.ready_window_count();

        let request = reader
            .next_source_window(current, PlaybackDirection::Reverse)
            .expect("invariant: reverse range calculation")
            .expect("invariant: reverse renewal range");
        assert!(request.start().get() < 10_000);
        assert_eq!(
            request.end().get(),
            10_000
                + u64::try_from(reader.max_source_frames)
                    .expect("invariant: window overlap fits u64")
        );
        assert!(request.len() <= reader.source_window_frames);
        let samples = reader
            .window_buffers
            .pop()
            .expect("invariant: first fixed window buffer");
        reader
            .stage_ready_window(request, samples, PlaybackDirection::Reverse)
            .expect("invariant: earlier source window is accepted");
        assert_eq!(reader.state.runtime.prepared.source_window, current);
        assert_eq!(
            reader.ready_windows.front().map(|window| window.range),
            Some(request)
        );

        let second = reader
            .next_source_window(request, PlaybackDirection::Reverse)
            .expect("invariant: second reverse range calculation")
            .expect("invariant: second reverse successor range");
        assert!(second.start() < request.start());
        let samples = reader
            .window_buffers
            .pop()
            .expect("invariant: second fixed window buffer");
        reader
            .stage_ready_window(second, samples, PlaybackDirection::Reverse)
            .expect("invariant: second reverse successor is accepted");
        assert_eq!(reader.ready_windows.len(), ready_window_count);

        let successor_range =
            SourceRange::try_from(9_000..9_512).expect("invariant: valid successor range");
        assert!(request.start() <= successor_range.start());
        assert!(successor_range.end() <= request.end());
        let window = reader
            .ready_windows
            .pop_front()
            .expect("invariant: a ready window is available");
        let old = replace(&mut reader.fetch, window.samples);
        reader.window_buffers.push(old);
        reader.state.runtime.prepared.source_window = window.range;
        assert_eq!(reader.state.runtime.prepared.source_window, request);
        assert_eq!(
            reader.ready_windows.front().map(|window| window.range),
            Some(second)
        );
        assert_eq!(fixed_buffer_count(&reader), ready_window_count + 1);
    }

    #[kithara::test]
    fn decoded_frontier_uses_the_directional_window_extent() {
        let pool = test_pool();
        for (direction, active, ready) in [
            (PlaybackDirection::Forward, 10_000..20_000, 19_000..30_000),
            (PlaybackDirection::Reverse, 20_000..30_000, 10_000..21_000),
        ] {
            let active = SourceRange::try_from(active).expect("invariant: valid active window");
            let mut reader = active_reader(&pool, active, direction);
            let samples = reader
                .window_buffers
                .pop()
                .expect("invariant: fixed window buffer");
            reader.ready_windows.push_back(BufferedSourceWindow {
                samples,
                range: SourceRange::try_from(ready).expect("invariant: valid ready window"),
            });

            assert_eq!(
                reader.decoded_frontier(),
                30_000.0 / f64::from(CONFIG.sample_rate)
            );
        }
    }
}
