use num_traits::ToPrimitive;

use super::{
    Active, BufferedSourceWindow, ElasticPreparation, ElasticPreparationPoll, ElasticPrepareError,
    ElasticRenderer, PcmPool, PlaybackDirection, PreparationPhase, PreparedRuntime, Preparing,
    READY_WINDOW_COUNT, Ready, RenderRuntime, SessionBeat, SourceCopy, SourceCursor, SourceRange,
    SourceRangeReadOutcome, SourceRangeRequest, StreamShape, Tempo, TrackBinding, sample_count,
};
use crate::resource::Resource;

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

impl ElasticRenderer<Preparing> {
    pub(crate) fn begin(
        resource: &mut Resource,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        revision: u64,
        shape: StreamShape,
        pool: &PcmPool,
    ) -> Result<Self, ElasticPrepareError> {
        let spec = resource.spec();
        let renderer = ElasticRenderer::<()>::allocate(
            spec.sample_rate,
            usize::from(spec.channels),
            binding.map().source_frame_count(),
            shape,
            pool,
        )?;
        if binding.direction() == PlaybackDirection::Reverse
            && !renderer.backend.capabilities().supports_reverse()
        {
            return Err(ElasticPrepareError::ReverseUnsupported);
        }
        let mut preparation = renderer.plan_preparation(
            binding,
            anchor,
            tempo,
            ElasticRenderer::<()>::PREFETCH_BLOCKS,
        )?;
        let range = preparation.fetch_range;
        let extension = expand_preparation_history(
            renderer.max_warm_frames,
            renderer.source_frame_count,
            &mut preparation,
        )?;
        let request = resource.request_source_range(range)?;
        Ok(renderer.map_state(|()| Preparing {
            preparation,
            revision,
            phase: PreparationPhase::Priming {
                request,
                range,
                extension,
            },
        }))
    }

    fn begin_preparation_window(
        &mut self,
        resource: &mut Resource,
        current: SourceRange,
        runtime: PreparedRuntime,
    ) -> Result<bool, ElasticPrepareError> {
        let Some(range) = self.next_source_window(current, runtime.direction)? else {
            return Ok(false);
        };
        let request = resource.request_source_range(range)?;
        self.state.phase = PreparationPhase::Window {
            request,
            range,
            runtime,
        };
        Ok(true)
    }
}

impl<State> ElasticRenderer<State> {
    pub(super) fn plan_preparation(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        prefetch_blocks: usize,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        let capabilities = self.backend.capabilities();
        let anchor_continuous = binding
            .source_frame_at(anchor)?
            .ok_or(ElasticPrepareError::AnchorOutsideMarkerDomain)?
            .get();
        let anchor_integer = anchor_continuous
            .round()
            .to_i64()
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let output_frames = capabilities.latency().output_frames();
        let output_frames_f64 = output_frames
            .to_f64()
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let beat_advance = output_frames_f64 * tempo.beats_per_second()
            / f64::from(binding.map().host_sample_rate().get());
        let horizon = SessionBeat::new(anchor.get() + beat_advance)
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let horizon_source = binding
            .source_frame_at(horizon)?
            .ok_or(ElasticPrepareError::AnchorOutsideMarkerDomain)?
            .get();
        let rate = (horizon_source - anchor_continuous).abs() / output_frames_f64;
        let warmup = capabilities.warmup_request(rate)?;
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
        let (start, end) = match binding.direction() {
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
            anchor: SourceCursor {
                continuous: anchor_continuous,
                integer: anchor_integer,
            },
            direction: binding.direction(),
            fetch_range: range,
        })
    }
}

impl ElasticRenderer<Preparing> {
    pub(crate) fn poll(
        self,
        resource: &mut Resource,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError> {
        match self.state.phase {
            PreparationPhase::Priming {
                request,
                range,
                extension,
            } => self.poll_priming(resource, request, range, extension),
            PreparationPhase::Window {
                request,
                range,
                runtime,
            } => self.poll_preparation_window(resource, request, range, runtime),
        }
    }

    fn poll_priming(
        mut self,
        resource: &mut Resource,
        request: SourceRangeRequest,
        range: SourceRange,
        extension: Option<SourceRange>,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError> {
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
        match resource.read_source_range(
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
            let request = resource.request_source_range(extension)?;
            self.state.phase = PreparationPhase::Priming {
                request,
                range: extension,
                extension: None,
            };
            return Ok(ElasticPreparationPoll::Pending(self));
        }
        self.prime(preparation, fetch_samples)?;
        let runtime = PreparedRuntime {
            cursor: preparation.anchor,
            direction: preparation.direction,
            source_window: preparation.fetch_range,
            request_id: 1,
            revision: self.state.revision,
        };
        if preparation.direction == PlaybackDirection::Reverse
            && self.begin_preparation_window(resource, preparation.fetch_range, runtime)?
        {
            return Ok(ElasticPreparationPoll::Pending(self));
        }
        Ok(self.ready(runtime))
    }

    fn poll_preparation_window(
        mut self,
        resource: &mut Resource,
        request: SourceRangeRequest,
        range: SourceRange,
        runtime: PreparedRuntime,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError> {
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
        match resource.read_source_range(request, &mut buffer[..samples])? {
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
            .push(BufferedSourceWindow { samples, range });
        if self.ready_windows.len() < READY_WINDOW_COUNT
            && self.begin_preparation_window(resource, range, runtime)?
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
            },
        }))
    }
}

impl<State> ElasticRenderer<State> {
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
                .integer
                .checked_sub(warm_source_frames)
                .and_then(|start| start.checked_sub(history_frames_i64)),
            PlaybackDirection::Reverse => preparation
                .anchor
                .integer
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
                preparation.anchor.integer.checked_sub(warm_source_frames)
            }
            PlaybackDirection::Reverse => {
                preparation.anchor.integer.checked_add(warm_source_frames)
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

impl ElasticRenderer<Ready> {
    pub(crate) fn retarget(
        &mut self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        revision: u64,
    ) -> Result<(), ElasticPrepareError> {
        let preparation = self.retarget_preparation(binding, anchor, tempo)?;
        let fetch_frames = usize::try_from(preparation.fetch_range.len())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let fetch_samples = sample_count(fetch_frames, self.backend.capabilities().channels())?;
        self.prime(preparation, fetch_samples)?;
        self.state.runtime.prepared.cursor = preparation.anchor;
        self.state.runtime.prepared.direction = preparation.direction;
        self.state.runtime.prepared.revision = revision;
        Ok(())
    }

    fn retarget_preparation(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        let mut preparation =
            self.plan_preparation(binding, anchor, tempo, Self::PREFETCH_BLOCKS)?;
        let fetch_range = self.state.runtime.prepared.source_window;
        if fetch_range.start() > preparation.fetch_range.start()
            || fetch_range.end() < preparation.fetch_range.end()
        {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        preparation.fetch_range = fetch_range;
        Ok(preparation)
    }

    pub(crate) fn validate_retarget(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
    ) -> Result<(), ElasticPrepareError> {
        self.retarget_preparation(binding, anchor, tempo).map(drop)
    }

    pub(crate) fn activate(self) -> ElasticRenderer<Active> {
        self.map_state(|Ready { runtime }| Active { runtime })
    }
}

impl From<ElasticRenderer<Active>> for ElasticRenderer<Ready> {
    fn from(renderer: ElasticRenderer<Active>) -> Self {
        renderer.map_state(|Active { runtime }| Ready { runtime })
    }
}

#[cfg(test)]
mod tests {
    use std::{mem::replace, num::NonZeroU32};

    use kithara_audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis};
    use kithara_bufpool::PcmPool;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::player::node::StreamShape;

    const SAMPLE_RATE: u32 = 44_100;

    fn source_frames() -> u64 {
        u64::from(SAMPLE_RATE) * 5
    }

    fn renderer(pool: &PcmPool) -> ElasticRenderer<()> {
        let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
        ElasticRenderer::<()>::allocate(
            sample_rate,
            1,
            source_frames(),
            StreamShape {
                sample_rate,
                max_block_frames: NonZeroU32::new(512).expect("static block size"),
            },
            pool,
        )
        .expect("elastic renderer preparation")
    }

    fn active_renderer(
        pool: &PcmPool,
        source_window: SourceRange,
        direction: PlaybackDirection,
    ) -> ElasticRenderer<Active> {
        let integer = i64::try_from(source_window.start().get()).expect("test range fits i64");
        renderer(pool).map_state(|()| Active {
            runtime: RenderRuntime {
                prepared: PreparedRuntime {
                    cursor: SourceCursor {
                        continuous: integer.to_f64().expect("test range converts to f64"),
                        integer,
                    },
                    direction,
                    source_window,
                    request_id: 1,
                    revision: 0,
                },
                pending_source_read: None,
            },
        })
    }

    fn binding(direction: PlaybackDirection) -> TrackBinding {
        let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
        let frames_per_beat = u64::from(SAMPLE_RATE) / 2;
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                120.0,
                (0..=10).map(|beat| beat * frames_per_beat).collect(),
                vec![0],
                Vec::new(),
            )),
            None,
            source_frames(),
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("finite session beat"),
            TrackBeat::new(4.0).expect("finite track beat"),
            direction,
        )
        .expect("valid track binding")
    }

    #[kithara::test]
    fn direction_and_tempo_stress_stays_inside_fixed_preparation_budgets() {
        let pool = PcmPool::default();
        let renderer = renderer(&pool);
        let max_fetch_frames =
            u64::try_from(renderer.max_fetch_frames).expect("fetch budget fits u64");
        for direction in [PlaybackDirection::Forward, PlaybackDirection::Reverse] {
            let binding = binding(direction);
            for beats_per_minute in [80.0, 100.0, 120.0, 140.0, 160.0] {
                let preparation = renderer
                    .plan_preparation(
                        &binding,
                        SessionBeat::new(0.0).expect("finite anchor"),
                        Tempo::new(beats_per_minute).expect("valid tempo"),
                        ElasticRenderer::<()>::PREFETCH_BLOCKS,
                    )
                    .expect("supported direction and tempo prepare inside fixed buffers");

                assert!(preparation.fetch_range.start() < preparation.fetch_range.end());
                assert!(preparation.fetch_range.len() <= max_fetch_frames);
                assert!(preparation.warmup.source_frames() <= renderer.max_warm_frames);
            }
        }
    }

    fn assert_window_stress(direction: PlaybackDirection, initial: SourceRange) {
        const PROMOTIONS: usize = 24;

        let pool = PcmPool::default();
        let mut renderer = active_renderer(&pool, initial, direction);
        while renderer.ready_windows.len() < READY_WINDOW_COUNT {
            let frontier = renderer
                .ready_windows
                .last()
                .map_or(initial, |window| window.range);
            let range = renderer
                .next_source_window(frontier, direction)
                .expect("successor range calculation")
                .expect("source has another successor window");
            let samples = renderer
                .window_buffers
                .pop()
                .expect("fixed window buffer is available");
            renderer
                .stage_ready_window(range, samples, direction)
                .expect("successor advances the directional frontier");
        }
        assert_eq!(fixed_buffer_count(&renderer), READY_WINDOW_COUNT + 1);

        for _ in 0..PROMOTIONS {
            let window = renderer.ready_windows.remove(0);
            let next = window.range;
            let old = replace(&mut renderer.fetch, window.samples);
            renderer.window_buffers.push(old);
            renderer.state.runtime.prepared.source_window = next;
            assert_eq!(renderer.state.runtime.prepared.source_window, next);
            assert_eq!(renderer.ready_windows.len(), READY_WINDOW_COUNT - 1);

            let frontier = renderer
                .ready_windows
                .last()
                .map_or(next, |ready| ready.range);
            let range = renderer
                .next_source_window(frontier, direction)
                .expect("successor range calculation")
                .expect("stress range stays away from source boundary");
            let samples = renderer
                .window_buffers
                .pop()
                .expect("promoted buffer replenishes the fixed bank");
            renderer
                .stage_ready_window(range, samples, direction)
                .expect("scheduled successor restores the full pipeline");
            assert_eq!(renderer.ready_windows.len(), READY_WINDOW_COUNT);
            assert_eq!(fixed_buffer_count(&renderer), READY_WINDOW_COUNT + 1);
            assert!(!renderer.ready_windows.spilled());
            assert!(!renderer.window_buffers.spilled());
        }
    }

    fn fixed_buffer_count(renderer: &ElasticRenderer<Active>) -> usize {
        let primary_pending = usize::from(renderer.state.runtime.pending_source_read.is_some());
        1 + renderer.window_buffers.len() + renderer.ready_windows.len() + primary_pending
    }

    #[kithara::test]
    fn forward_and_reverse_window_stress_never_exceeds_the_ready_bound() {
        assert_window_stress(
            PlaybackDirection::Forward,
            SourceRange::try_from(10_000..20_000).expect("valid forward window"),
        );
        assert_window_stress(
            PlaybackDirection::Reverse,
            SourceRange::try_from(200_000..210_000).expect("valid reverse window"),
        );
    }

    #[kithara::test]
    fn reverse_preparation_requests_one_ascending_bounded_range() {
        let pool = PcmPool::default();
        let renderer = renderer(&pool);
        let preparation = renderer
            .plan_preparation(
                &binding(PlaybackDirection::Reverse),
                SessionBeat::new(0.0).expect("finite anchor"),
                Tempo::new(120.0).expect("valid tempo"),
                ElasticRenderer::<()>::PREFETCH_BLOCKS,
            )
            .expect("reverse preparation plan");
        let anchor = u64::try_from(preparation.anchor.integer).expect("positive source anchor");
        let prefetch =
            u64::try_from(renderer.max_source_frames * ElasticRenderer::<()>::PREFETCH_BLOCKS)
                .expect("prefetch span fits u64");
        let history = u64::try_from(
            renderer.backend.capabilities().latency().source_frames()
                + preparation.warmup.source_frames(),
        )
        .expect("history span fits u64");

        assert_eq!(anchor - preparation.fetch_range.start().get(), prefetch);
        assert_eq!(preparation.fetch_range.end().get() - anchor, history);
        assert!(preparation.fetch_range.end().get() <= source_frames());
    }

    #[kithara::test]
    fn reverse_preparation_fetches_extra_tempo_history_as_a_separate_range() {
        let pool = PcmPool::default();
        let renderer = renderer(&pool);
        let mut preparation = renderer
            .plan_preparation(
                &binding(PlaybackDirection::Reverse),
                SessionBeat::new(0.0).expect("finite anchor"),
                Tempo::new(120.0).expect("valid tempo"),
                ElasticRenderer::<()>::PREFETCH_BLOCKS,
            )
            .expect("reverse preparation plan");
        let request = preparation.fetch_range;

        let extension = expand_preparation_history(
            renderer.max_warm_frames,
            renderer.source_frame_count,
            &mut preparation,
        )
        .expect("expanded preparation history")
        .expect("maximum rate needs additional history");

        assert_eq!(extension.start(), request.end());
        assert_eq!(preparation.fetch_range.start(), request.start());
        assert_eq!(preparation.fetch_range.end(), extension.end());
        assert_eq!(
            extension.len(),
            u64::try_from(renderer.max_warm_frames - preparation.warmup.source_frames())
                .expect("history extension fits u64")
        );
    }

    #[kithara::test]
    fn reverse_window_renewal_keeps_the_active_window_until_successor_use() {
        let pool = PcmPool::default();
        let current = SourceRange::try_from(10_000..20_000).expect("valid current source window");
        let mut renderer = active_renderer(&pool, current, PlaybackDirection::Reverse);

        let request = renderer
            .next_source_window(current, PlaybackDirection::Reverse)
            .expect("reverse range calculation")
            .expect("reverse renewal range");
        assert!(request.start().get() < 10_000);
        assert_eq!(
            request.end().get(),
            10_000 + u64::try_from(renderer.max_source_frames).expect("window overlap fits u64")
        );
        assert!(request.len() <= renderer.source_window_frames);
        let samples = renderer
            .window_buffers
            .pop()
            .expect("first fixed window buffer");
        renderer
            .stage_ready_window(request, samples, PlaybackDirection::Reverse)
            .expect("earlier source window is accepted");
        assert_eq!(renderer.state.runtime.prepared.source_window, current);
        assert_eq!(
            renderer.ready_windows.first().map(|window| window.range),
            Some(request)
        );

        let second = renderer
            .next_source_window(request, PlaybackDirection::Reverse)
            .expect("second reverse range calculation")
            .expect("second reverse successor range");
        assert!(second.start() < request.start());
        let samples = renderer
            .window_buffers
            .pop()
            .expect("second fixed window buffer");
        renderer
            .stage_ready_window(second, samples, PlaybackDirection::Reverse)
            .expect("second reverse successor is accepted");
        assert_eq!(renderer.ready_windows.len(), READY_WINDOW_COUNT);

        let successor_range = SourceRange::try_from(9_000..9_512).expect("valid successor range");
        assert!(request.start() <= successor_range.start());
        assert!(successor_range.end() <= request.end());
        let window = renderer.ready_windows.remove(0);
        let old = replace(&mut renderer.fetch, window.samples);
        renderer.window_buffers.push(old);
        renderer.state.runtime.prepared.source_window = window.range;
        assert_eq!(renderer.state.runtime.prepared.source_window, request);
        assert_eq!(
            renderer.ready_windows.first().map(|window| window.range),
            Some(second)
        );
        assert_eq!(fixed_buffer_count(&renderer), READY_WINDOW_COUNT + 1);
    }

    #[kithara::test]
    fn decoded_frontier_includes_a_ready_forward_window() {
        let pool = PcmPool::default();
        let active = SourceRange::try_from(10_000..20_000).expect("valid active window");
        let mut renderer = active_renderer(&pool, active, PlaybackDirection::Forward);
        let samples = renderer.window_buffers.pop().expect("fixed window buffer");
        renderer.ready_windows.push(BufferedSourceWindow {
            samples,
            range: SourceRange::try_from(19_000..30_000).expect("valid ready window"),
        });

        assert_eq!(
            renderer.decoded_frontier(),
            30_000.0 / f64::from(SAMPLE_RATE)
        );
    }
}
