use num_traits::ToPrimitive;

use super::{
    BufferedSourceWindow, ElasticPreparation, ElasticPreparationOutcome, ElasticPrepareError,
    ElasticRenderer, PlaybackDirection, PlayerResource, READY_WINDOW_COUNT, SessionBeat,
    SourceAudioReadOutcome, SourceCopy, SourceCursor, SourceFrameRange, Tempo, TrackBinding,
    copy_source, sample_count,
};

fn expand_preparation_history(
    max_warm_frames: usize,
    source_frame_count: u64,
    preparation: &mut ElasticPreparation,
) -> Result<Option<SourceFrameRange>, ElasticPrepareError> {
    let additional = max_warm_frames
        .checked_sub(preparation.warmup.source_frames())
        .ok_or(ElasticPrepareError::FrameOverflow)?;
    let additional = u64::try_from(additional).map_err(|_| ElasticPrepareError::FrameOverflow)?;
    let range = preparation.fetch_range;
    let extension = match preparation.direction {
        PlaybackDirection::Forward => {
            let start = range.start().saturating_sub(additional);
            (start < range.start()).then(|| SourceFrameRange::new(start, range.start()))
        }
        PlaybackDirection::Reverse => {
            let end = range
                .end()
                .checked_add(additional)
                .ok_or(ElasticPrepareError::FrameOverflow)?
                .min(source_frame_count);
            (range.end() < end).then(|| SourceFrameRange::new(range.end(), end))
        }
    }
    .transpose()?;
    if let Some(extension) = extension {
        preparation.fetch_range = SourceFrameRange::new(
            range.start().min(extension.start()),
            range.end().max(extension.end()),
        )?;
    }
    Ok(extension)
}

impl ElasticRenderer {
    pub(crate) fn begin_prefetch(
        &mut self,
        resource: &mut PlayerResource,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        revision: u64,
    ) -> Result<(), ElasticPrepareError> {
        if binding.direction() == PlaybackDirection::Reverse
            && !self.capabilities.supports_reverse()
        {
            return Err(ElasticPrepareError::ReverseUnsupported);
        }
        if self.relocation.is_some() {
            return Err(ElasticPrepareError::RelocationPending);
        }
        let mut preparation =
            self.plan_preparation(binding, anchor, tempo, Self::PREFETCH_BLOCKS)?;
        let request = preparation.fetch_range;
        self.preparation_extension = expand_preparation_history(
            self.max_warm_frames,
            self.source_frame_count,
            &mut preparation,
        )?;
        self.request_preparation_range(resource, request)?;
        self.revision = revision;
        self.preparation = Some(preparation);
        Ok(())
    }

    pub(super) fn plan_preparation(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        prefetch_blocks: usize,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        let anchor_continuous = binding
            .source_frame_at(anchor)?
            .ok_or(ElasticPrepareError::AnchorOutsideMarkerDomain)?
            .get();
        let anchor_integer = anchor_continuous
            .round()
            .to_i64()
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let output_frames = self.capabilities.latency().output_frames();
        let output_frames_f64 = output_frames
            .to_f64()
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let beat_advance = output_frames_f64 * tempo.beats_per_minute()
            / (f64::from(binding.map().host_sample_rate().get()) * 60.0);
        let horizon = SessionBeat::new(anchor.get() + beat_advance)
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let horizon_source = binding
            .source_frame_at(horizon)?
            .ok_or(ElasticPrepareError::AnchorOutsideMarkerDomain)?
            .get();
        let rate = (horizon_source - anchor_continuous).abs() / output_frames_f64;
        let warmup = self.capabilities.warmup_request(rate)?;
        let before = self
            .capabilities
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
        let range = SourceFrameRange::new(start, end)?;
        Ok(ElasticPreparation {
            anchor: SourceCursor {
                continuous: anchor_continuous,
                integer: anchor_integer,
            },
            direction: binding.direction(),
            fetch_range: range,
            warmup,
        })
    }

    pub(crate) fn validate_retarget(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
    ) -> Result<(), ElasticPrepareError> {
        self.retarget_preparation(binding, anchor, tempo).map(drop)
    }

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
        let fetch_samples = sample_count(fetch_frames, self.channels)?;
        self.prime(preparation, fetch_samples)?;
        self.cursor = Some(preparation.anchor);
        self.direction = Some(preparation.direction);
        self.revision = revision;
        Ok(())
    }

    fn retarget_preparation(
        &self,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
    ) -> Result<ElasticPreparation, ElasticPrepareError> {
        if !self.primed || self.preparation.is_some() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        let mut preparation =
            self.plan_preparation(binding, anchor, tempo, Self::PREFETCH_BLOCKS)?;
        let fetch_range = self
            .source_window
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        if fetch_range.start() > preparation.fetch_range.start()
            || fetch_range.end() < preparation.fetch_range.end()
        {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        preparation.fetch_range = fetch_range;
        Ok(preparation)
    }

    pub(crate) fn poll_preparation(
        &mut self,
        resource: &mut PlayerResource,
    ) -> Result<ElasticPreparationOutcome, ElasticPrepareError> {
        if self.primed && self.preparation.is_none() {
            return Ok(ElasticPreparationOutcome::Ready);
        }
        let preparation = self
            .preparation
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        if self.primed {
            return self.poll_preparation_window(resource);
        }
        let fetch_frames = usize::try_from(preparation.fetch_range.len())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.channels)?;
        let request = self
            .preparation_request
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        let request_frames =
            usize::try_from(request.len()).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let request_offset = request
            .start()
            .checked_sub(preparation.fetch_range.start())
            .ok_or(ElasticPrepareError::FetchWindowMismatch)?;
        let request_offset =
            usize::try_from(request_offset).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let request_sample_start = sample_count(request_offset, self.channels)?;
        let request_sample_end = request_sample_start
            .checked_add(sample_count(request_frames, self.channels)?)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let demand = self.demand.ok_or(ElasticPrepareError::SourceUnavailable)?;
        match resource.read_source_audio(
            &demand,
            request,
            &mut self.fetch[request_sample_start..request_sample_end],
        )? {
            Some(SourceAudioReadOutcome::Ready { .. }) => {}
            Some(SourceAudioReadOutcome::Pending) => {
                return Ok(ElasticPreparationOutcome::Pending);
            }
            Some(SourceAudioReadOutcome::Eof) => return Err(ElasticPrepareError::SourceEnded),
            Some(_) => return Err(ElasticPrepareError::UnsupportedSourceOutcome),
            None => return Err(ElasticPrepareError::SourceUnavailable),
        }
        if let Some(extension) = self.preparation_extension.take() {
            self.request_preparation_range(resource, extension)?;
            return Ok(ElasticPreparationOutcome::Pending);
        }
        self.preparation_request = None;
        self.prime(preparation, fetch_samples)?;
        self.cursor = Some(preparation.anchor);
        self.direction = Some(preparation.direction);
        self.primed = true;
        self.source_window = Some(preparation.fetch_range);
        if preparation.direction == PlaybackDirection::Reverse
            && self.begin_preparation_window(
                resource,
                preparation.fetch_range,
                preparation.direction,
            )?
        {
            return Ok(ElasticPreparationOutcome::Pending);
        }
        self.preparation = None;
        self.demand = None;
        self.preparation_buffers.clear();
        Ok(ElasticPreparationOutcome::Ready)
    }

    fn request_preparation_range(
        &mut self,
        resource: &mut PlayerResource,
        range: SourceFrameRange,
    ) -> Result<(), ElasticPrepareError> {
        resource
            .seek_source_frame(range.start())
            .map_err(|_| ElasticPrepareError::SourceSeek)?;
        self.demand = resource.request_source_audio(range, 0)?;
        if self.demand.is_none() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        self.preparation_request = Some(range);
        Ok(())
    }

    fn poll_preparation_window(
        &mut self,
        resource: &mut PlayerResource,
    ) -> Result<ElasticPreparationOutcome, ElasticPrepareError> {
        let Some(range) = self.preparation_window else {
            self.preparation = None;
            self.demand = None;
            return Ok(ElasticPreparationOutcome::Ready);
        };
        let frames =
            usize::try_from(range.len()).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        if frames > self.max_fetch_frames {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        let samples = sample_count(frames, self.channels)?;
        let demand = self.demand.ok_or(ElasticPrepareError::SourceUnavailable)?;
        let buffer = self
            .preparation_buffers
            .last_mut()
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        match resource.read_source_audio(&demand, range, &mut buffer[..samples])? {
            Some(SourceAudioReadOutcome::Ready { .. }) => {}
            Some(SourceAudioReadOutcome::Pending) => {
                return Ok(ElasticPreparationOutcome::Pending);
            }
            Some(SourceAudioReadOutcome::Eof) => return Err(ElasticPrepareError::SourceEnded),
            Some(_) => return Err(ElasticPrepareError::UnsupportedSourceOutcome),
            None => return Err(ElasticPrepareError::SourceUnavailable),
        }
        let samples = self
            .preparation_buffers
            .pop()
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        self.ready_windows
            .push(BufferedSourceWindow { range, samples });
        self.preparation_window = None;
        self.demand = None;
        let preparation = self
            .preparation
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        if self.ready_windows.len() < READY_WINDOW_COUNT
            && self.begin_preparation_window(resource, range, preparation.direction)?
        {
            return Ok(ElasticPreparationOutcome::Pending);
        }
        self.preparation = None;
        self.preparation_buffers.clear();
        Ok(ElasticPreparationOutcome::Ready)
    }

    fn begin_preparation_window(
        &mut self,
        resource: &mut PlayerResource,
        current: SourceFrameRange,
        direction: PlaybackDirection,
    ) -> Result<bool, ElasticPrepareError> {
        let Some(range) = self.next_source_window(current, direction)? else {
            return Ok(false);
        };
        resource
            .seek_source_frame(range.start())
            .map_err(|_| ElasticPrepareError::SourceSeek)?;
        self.demand = resource.request_source_audio(range, 0)?;
        if self.demand.is_none() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        self.preparation_window = Some(range);
        Ok(true)
    }

    pub(super) fn prime(
        &mut self,
        preparation: ElasticPreparation,
        fetch_samples: usize,
    ) -> Result<(), ElasticPrepareError> {
        let history_frames = self.capabilities.latency().source_frames();
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
        copy_source(SourceCopy {
            start: history_start,
            frames: history_frames,
            direction: preparation.direction,
            fetch_range: preparation.fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.history,
            channels: self.channels,
            source_frame_count: self.source_frame_count,
        })
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
        copy_source(SourceCopy {
            start: warmup_start,
            frames: usize::try_from(warm_source_frames)
                .map_err(|_| ElasticPrepareError::FrameOverflow)?,
            direction: preparation.direction,
            fetch_range: preparation.fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.source,
            channels: self.channels,
            source_frame_count: self.source_frame_count,
        })
        .map_err(ElasticPrepareError::from)?;
        self.backend.prime(
            preparation.warmup,
            &self.history[..sample_count(history_frames, self.channels)
                .map_err(|_| ElasticPrepareError::FrameOverflow)?],
            &self.source[..sample_count(preparation.warmup.source_frames(), self.channels)?],
            &mut self.discarded[..sample_count(preparation.warmup.output_frames(), self.channels)?],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis};
    use kithara_bufpool::PcmPool;
    use kithara_platform::CancelScope;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::player::{node::StreamShape, track::elastic_source::elastic_source_test_pair};

    const SAMPLE_RATE: u32 = 44_100;

    fn source_frames() -> u64 {
        u64::from(SAMPLE_RATE) * 5
    }

    fn renderer(pool: &PcmPool) -> ElasticRenderer {
        let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
        ElasticRenderer::prepare(
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
    fn reverse_preparation_requests_one_ascending_bounded_range() {
        let pool = PcmPool::default();
        let renderer = renderer(&pool);
        let preparation = renderer
            .plan_preparation(
                &binding(PlaybackDirection::Reverse),
                SessionBeat::new(0.0).expect("finite anchor"),
                Tempo::new(120.0).expect("valid tempo"),
                ElasticRenderer::PREFETCH_BLOCKS,
            )
            .expect("reverse preparation plan");
        let anchor = u64::try_from(preparation.anchor.integer).expect("positive source anchor");
        let prefetch = u64::try_from(renderer.max_source_frames * ElasticRenderer::PREFETCH_BLOCKS)
            .expect("prefetch span fits u64");
        let history = u64::try_from(
            renderer.capabilities.latency().source_frames() + preparation.warmup.source_frames(),
        )
        .expect("history span fits u64");

        assert_eq!(anchor - preparation.fetch_range.start(), prefetch);
        assert_eq!(preparation.fetch_range.end() - anchor, history);
        assert!(preparation.fetch_range.end() <= source_frames());
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
                ElasticRenderer::PREFETCH_BLOCKS,
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
        let mut renderer = renderer(&pool);
        let scope = CancelScope::new(None);
        let (port, mut peer) = elastic_source_test_pair(scope.token());
        renderer.source_port = Some(port);
        renderer.source_window =
            Some(SourceFrameRange::new(10_000, 20_000).expect("valid current source window"));

        renderer
            .schedule_window(PlaybackDirection::Reverse)
            .expect("reverse renewal schedules");
        let request = peer.pop_request().expect("reverse renewal request");
        assert!(request.range().start() < 10_000);
        assert_eq!(
            request.range().end(),
            10_000 + u64::try_from(renderer.max_source_frames).expect("window overlap fits u64")
        );
        assert!(request.range().len() <= renderer.source_window_frames);
        assert!(request.range().start() < request.range().end());
        assert!(peer.push_ready(request.generation(), request.range(), pool.get()));

        renderer
            .poll_source_port(PlaybackDirection::Reverse)
            .expect("earlier source window is accepted");
        assert_eq!(
            renderer.source_window,
            Some(SourceFrameRange::new(10_000, 20_000).expect("valid current source window"))
        );
        assert_eq!(
            renderer.ready_windows.first().map(|window| window.range),
            Some(request.range())
        );
        renderer
            .schedule_window(PlaybackDirection::Reverse)
            .expect("second reverse successor schedules");
        let second = peer
            .pop_request()
            .expect("second reverse successor request");
        assert!(second.range().start() < request.range().start());
        assert!(peer.push_ready(second.generation(), second.range(), pool.get()));
        renderer
            .poll_source_port(PlaybackDirection::Reverse)
            .expect("second reverse successor is accepted");
        assert_eq!(renderer.ready_windows.len(), READY_WINDOW_COUNT);

        let successor_range = SourceFrameRange::new(9_000, 9_512).expect("valid successor range");
        renderer
            .ensure_window(successor_range, PlaybackDirection::Reverse)
            .expect("ready successor window is promoted at its boundary");
        assert_eq!(renderer.source_window, Some(request.range()));
        assert_eq!(
            renderer.ready_windows.first().map(|window| window.range),
            Some(second.range())
        );
        let third = peer
            .pop_request()
            .expect("FIFO promotion replenishes one successor");
        assert!(third.range().start() < second.range().start());
    }

    #[kithara::test]
    fn decoded_frontier_includes_a_ready_forward_window() {
        let pool = PcmPool::default();
        let mut renderer = renderer(&pool);
        renderer.source_window =
            Some(SourceFrameRange::new(10_000, 20_000).expect("valid active window"));
        renderer.ready_windows.push(BufferedSourceWindow {
            range: SourceFrameRange::new(19_000, 30_000).expect("valid ready window"),
            samples: pool.get(),
        });

        assert_eq!(
            renderer.decoded_frontier(),
            30_000.0 / f64::from(SAMPLE_RATE)
        );
    }

    #[kithara::test]
    fn recycle_backpressure_preserves_multiple_ready_windows() {
        let pool = PcmPool::default();
        let mut renderer = renderer(&pool);
        let scope = CancelScope::new(None);
        let (mut port, mut peer) = elastic_source_test_pair(scope.token());
        for _ in 0..4 {
            assert!(port.recycle(pool.get()).is_ok());
        }
        let range = SourceFrameRange::new(0, 1).expect("valid source range");
        assert!(peer.push_ready(1, range, pool.get()));
        assert!(peer.push_ready(2, range, pool.get()));
        renderer.source_port = Some(port);

        renderer
            .poll_source_port(PlaybackDirection::Forward)
            .expect("first reply poll");
        assert!(renderer.pending_retirement.is_some());

        let mut retired = usize::from(peer.pop_recycled().is_some());
        renderer
            .poll_source_port(PlaybackDirection::Forward)
            .expect("second reply poll");
        assert!(renderer.pending_retirement.is_some());
        while peer.pop_recycled().is_some() {
            retired += 1;
        }

        renderer
            .poll_source_port(PlaybackDirection::Forward)
            .expect("final retirement flush");
        while peer.pop_recycled().is_some() {
            retired += 1;
        }

        assert_eq!(retired, 6);
        assert!(renderer.pending_retirement.is_none());
        assert!(
            renderer
                .source_port
                .as_mut()
                .expect("test source port")
                .receive()
                .is_none()
        );
    }
}
