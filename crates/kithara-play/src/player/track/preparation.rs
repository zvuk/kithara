use std::mem;

use num_traits::ToPrimitive;

use super::{
    ElasticPreparation, ElasticPreparationOutcome, ElasticPrepareError, ElasticRenderError,
    ElasticRenderer, ElasticSourcePort, ElasticSourceReply, ElasticSourceRequest,
    PlaybackDirection, PlayerResource, SessionBeat, SourceAudioReadOutcome, SourceCopy,
    SourceCursor, SourceFrameRange, Tempo, TrackBinding, copy_source, sample_count,
};

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
        let preparation = self.plan_preparation(binding, anchor, tempo, Self::PREFETCH_BLOCKS)?;
        let start = preparation.fetch_range.start();
        resource
            .seek_source_frame(start)
            .map_err(|_| ElasticPrepareError::SourceSeek)?;
        self.demand = resource.request_source_audio(preparation.fetch_range, 0)?;
        if self.demand.is_none() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
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

    pub(crate) fn poll_preparation(
        &mut self,
        resource: &mut PlayerResource,
    ) -> Result<ElasticPreparationOutcome, ElasticPrepareError> {
        if self.primed {
            return Ok(ElasticPreparationOutcome::Ready);
        }
        let preparation = self
            .preparation
            .ok_or(ElasticPrepareError::SourceUnavailable)?;
        let fetch_frames = usize::try_from(preparation.fetch_range.len())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticPrepareError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.channels)?;
        let demand = self.demand.ok_or(ElasticPrepareError::SourceUnavailable)?;
        match resource.read_source_audio(
            &demand,
            preparation.fetch_range,
            &mut self.fetch[..fetch_samples],
        )? {
            Some(SourceAudioReadOutcome::Ready { .. }) => {}
            Some(SourceAudioReadOutcome::Pending) => {
                return Ok(ElasticPreparationOutcome::Pending);
            }
            Some(SourceAudioReadOutcome::Eof) => return Err(ElasticPrepareError::SourceEnded),
            Some(_) => return Err(ElasticPrepareError::UnsupportedSourceOutcome),
            None => return Err(ElasticPrepareError::SourceUnavailable),
        }
        self.prime(preparation, fetch_samples)?;
        self.cursor = Some(preparation.anchor);
        self.direction = Some(preparation.direction);
        self.preparation = None;
        self.primed = true;
        self.source_window = Some(preparation.fetch_range);
        Ok(ElasticPreparationOutcome::Ready)
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

    pub(crate) fn attach_source_ports(
        &mut self,
        port: ElasticSourcePort,
        relocation_port: ElasticSourcePort,
    ) {
        self.source_port = Some(port);
        self.relocation_port = Some(relocation_port);
    }

    pub(super) fn ensure_window(
        &mut self,
        range: SourceFrameRange,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        self.poll_source_port(direction)?;
        if self
            .source_window
            .is_some_and(|window| window.start() <= range.start() && range.end() <= window.end())
        {
            self.schedule_window(range, direction)?;
            return Ok(());
        }
        self.schedule_window(range, direction)?;
        Err(ElasticRenderError::SourceWindowDeadlineMissed)
    }

    fn schedule_window(
        &mut self,
        range: SourceFrameRange,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        let renewal = self
            .max_source_frames
            .checked_mul(Self::RENEWAL_BLOCKS)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let renewal = u64::try_from(renewal).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let needs_window = self.source_window.is_none_or(|window| match direction {
            PlaybackDirection::Forward => {
                window.end() < self.source_frame_count
                    && window.end().saturating_sub(range.end()) <= renewal
            }
            PlaybackDirection::Reverse => {
                window.start() > 0 && range.start().saturating_sub(window.start()) <= renewal
            }
        });
        if !needs_window {
            return Ok(());
        }
        if self.pending_request.is_some() {
            return Ok(());
        }
        let window_frames = self
            .max_source_frames
            .checked_mul(Self::PREFETCH_BLOCKS)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let window_frames =
            u64::try_from(window_frames).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let request_range = match direction {
            PlaybackDirection::Forward => SourceFrameRange::new(
                range.start(),
                range
                    .start()
                    .checked_add(window_frames)
                    .ok_or(ElasticRenderError::FrameOverflow)?
                    .min(self.source_frame_count),
            )?,
            PlaybackDirection::Reverse => {
                SourceFrameRange::new(range.end().saturating_sub(window_frames), range.end())?
            }
        };
        let generation = self
            .source_generation
            .checked_add(1)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let request = ElasticSourceRequest::new(generation, request_range);
        let port = self
            .source_port
            .as_mut()
            .ok_or(ElasticRenderError::SourceWorkerUnavailable)?;
        if port.request(request).is_ok() {
            self.source_generation = generation;
            self.pending_request = Some(request);
        }
        Ok(())
    }

    fn poll_source_port(&mut self, direction: PlaybackDirection) -> Result<(), ElasticRenderError> {
        if self.source_port.is_none() {
            return Err(ElasticRenderError::SourceWorkerUnavailable);
        }
        if let Some(samples) = self.pending_retirement.take()
            && let Err(samples) = self
                .source_port
                .as_mut()
                .ok_or(ElasticRenderError::SourceWorkerUnavailable)?
                .recycle(samples)
        {
            self.pending_retirement = Some(samples);
            return Ok(());
        }
        while let Some(reply) = self
            .source_port
            .as_mut()
            .ok_or(ElasticRenderError::SourceWorkerUnavailable)?
            .receive()
        {
            let pending = self
                .pending_request
                .filter(|pending| pending.generation() == reply.generation());
            let expected = pending.is_some();
            if !expected {
                if let ElasticSourceReply::Ready(window) = reply {
                    self.recycle_window(window);
                    if self.pending_retirement.is_some() {
                        return Ok(());
                    }
                }
                continue;
            }
            match reply {
                ElasticSourceReply::Ready(window) => {
                    let range = window.range();
                    let pending = pending.ok_or(ElasticRenderError::SourceWorkerFailed)?;
                    if range != pending.range() {
                        self.recycle_window(window);
                        self.pending_request = None;
                        return Err(ElasticRenderError::SourceWorkerFailed);
                    }
                    let samples = window.release_samples();
                    let advances = self.source_window.is_none_or(|current| match direction {
                        PlaybackDirection::Forward => range.end() > current.end(),
                        PlaybackDirection::Reverse => range.start() < current.start(),
                    });
                    if !advances {
                        self.recycle_samples(samples);
                        self.pending_request = None;
                        return Err(ElasticRenderError::SourceWorkerFailed);
                    }
                    let old = mem::replace(&mut self.fetch, samples);
                    self.recycle_samples(old);
                    self.source_window = Some(range);
                    self.pending_request = None;
                    if self.pending_retirement.is_some() {
                        return Ok(());
                    }
                }
                ElasticSourceReply::Eof { .. } | ElasticSourceReply::Failed { .. } => {
                    self.pending_request = None;
                    return Err(ElasticRenderError::SourceWorkerFailed);
                }
            }
        }
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
    fn reverse_window_renewal_requests_and_accepts_an_earlier_ascending_range() {
        let pool = PcmPool::default();
        let mut renderer = renderer(&pool);
        let scope = CancelScope::new(None);
        let (port, mut peer) = elastic_source_test_pair(scope.token());
        renderer.source_port = Some(port);
        renderer.source_window =
            Some(SourceFrameRange::new(10_000, 20_000).expect("valid current source window"));
        let rendered = SourceFrameRange::new(10_000, 10_512).expect("valid rendered range");

        renderer
            .schedule_window(rendered, PlaybackDirection::Reverse)
            .expect("reverse renewal schedules");
        let request = peer.pop_request().expect("reverse renewal request");
        assert!(request.range().start() < 10_000);
        assert_eq!(request.range().end(), rendered.end());
        assert!(request.range().start() < request.range().end());
        assert!(peer.push_ready(request.generation(), request.range(), pool.get()));

        renderer
            .poll_source_port(PlaybackDirection::Reverse)
            .expect("earlier source window is accepted");
        assert_eq!(renderer.source_window, Some(request.range()));
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
