use std::mem;

use num_traits::ToPrimitive;

use super::{
    ElasticPreparation, ElasticPreparationOutcome, ElasticPrepareError, ElasticRenderError,
    ElasticRenderer, ElasticSourcePort, ElasticSourceReply, ElasticSourceRequest,
    ElasticSourceWindow, PcmBuf, PlaybackDirection, PlayerResource, SessionBeat,
    SourceAudioReadOutcome, SourceCopy, SourceCursor, SourceFrameRange, Tempo, TrackBinding,
    copy_source, sample_count,
};

impl ElasticRenderer {
    pub(crate) fn begin_prefetch(
        &mut self,
        resource: &mut PlayerResource,
        binding: &TrackBinding,
        tempo: Tempo,
        revision: u64,
    ) -> Result<(), ElasticPrepareError> {
        if binding.direction() != PlaybackDirection::Forward {
            return Err(ElasticPrepareError::ReversePreparationRequired);
        }
        let anchor_continuous = binding
            .source_frame_at(binding.session_anchor())?
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
        let horizon = SessionBeat::new(binding.session_anchor().get() + beat_advance)
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
            .checked_mul(Self::PREFETCH_BLOCKS)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let after = i64::try_from(after).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let source_end = i64::try_from(self.source_frame_count)
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let start = anchor_integer.saturating_sub(before).max(0);
        let end = anchor_integer
            .checked_add(after)
            .ok_or(ElasticPrepareError::FrameOverflow)?
            .min(source_end);
        if start >= end {
            return Err(ElasticPrepareError::SourceEnded);
        }
        let start = u64::try_from(start).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let end = u64::try_from(end).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        resource
            .seek_source_frame(start)
            .map_err(|_| ElasticPrepareError::SourceSeek)?;
        let range = SourceFrameRange::new(start, end)?;
        self.demand = resource.request_source_audio(range, 0)?;
        if self.demand.is_none() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        self.revision = revision;
        self.preparation = Some(ElasticPreparation {
            anchor: SourceCursor {
                continuous: anchor_continuous,
                integer: anchor_integer,
            },
            fetch_range: range,
            warmup,
        });
        Ok(())
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
        self.preparation = None;
        self.primed = true;
        self.source_window = Some(preparation.fetch_range);
        Ok(ElasticPreparationOutcome::Ready)
    }

    fn prime(
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
        let history_start = preparation
            .anchor
            .integer
            .checked_sub(warm_source_frames)
            .and_then(|start| start.checked_sub(history_frames_i64))
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        copy_source(SourceCopy {
            start: history_start,
            frames: history_frames,
            fetch_range: preparation.fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.history,
            channels: self.channels,
            source_frame_count: self.source_frame_count,
        })
        .map_err(ElasticPrepareError::from)?;
        let warmup_start = preparation
            .anchor
            .integer
            .checked_sub(warm_source_frames)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        copy_source(SourceCopy {
            start: warmup_start,
            frames: usize::try_from(warm_source_frames)
                .map_err(|_| ElasticPrepareError::FrameOverflow)?,
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

    pub(crate) fn attach_source_port(&mut self, port: ElasticSourcePort) {
        self.source_port = Some(port);
    }

    pub(super) fn ensure_window(
        &mut self,
        range: SourceFrameRange,
    ) -> Result<(), ElasticRenderError> {
        self.poll_source_port()?;
        if self
            .source_window
            .is_some_and(|window| window.start() <= range.start() && range.end() <= window.end())
        {
            self.schedule_window(range)?;
            return Ok(());
        }
        self.schedule_window(range)?;
        Err(ElasticRenderError::SourceWindowDeadlineMissed)
    }

    pub(super) fn schedule_window(
        &mut self,
        range: SourceFrameRange,
    ) -> Result<(), ElasticRenderError> {
        if self.pending_request.is_some() {
            return Ok(());
        }
        let renewal = self
            .max_source_frames
            .checked_mul(Self::RENEWAL_BLOCKS)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let renewal = u64::try_from(renewal).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let range = self.pinned_range(range)?;
        let needs_window = self.source_window.is_none_or(|window| {
            window.start() > range.start()
                || range.end() > window.end()
                || (window.end() < self.source_frame_count
                    && window.end().saturating_sub(range.end()) <= renewal)
        });
        if !needs_window {
            return Ok(());
        }
        let window_frames = self
            .max_source_frames
            .checked_mul(Self::PREFETCH_BLOCKS)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let window_frames =
            u64::try_from(window_frames).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let request_end = range
            .start()
            .checked_add(window_frames)
            .ok_or(ElasticRenderError::FrameOverflow)?
            .min(self.source_frame_count);
        if range.end() > request_end {
            return Err(ElasticRenderError::FetchWindowMismatch);
        }
        let request_range = SourceFrameRange::new(range.start(), request_end)?;
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

    pub(super) fn pinned_range(
        &self,
        range: SourceFrameRange,
    ) -> Result<SourceFrameRange, ElasticRenderError> {
        let Some(pin) = self.tempo_pin else {
            return Ok(range);
        };
        SourceFrameRange::new(
            range.start().min(pin.range.start()),
            range.end().max(pin.range.end()),
        )
        .map_err(ElasticRenderError::Source)
    }

    pub(super) fn poll_source_port(&mut self) -> Result<(), ElasticRenderError> {
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
                .filter(|request| request.generation() == reply.generation());
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
                    let request = pending.ok_or(ElasticRenderError::SourceWorkerFailed)?;
                    if self
                        .tempo_pin
                        .is_some_and(|pin| !range_contains(range, pin.range))
                    {
                        self.recycle_window(window);
                        self.pending_request = None;
                        if self.pending_retirement.is_some() {
                            return Ok(());
                        }
                        continue;
                    }
                    let advances = self
                        .source_window
                        .is_none_or(|current| range.end() > current.end());
                    if range != request.range() || !advances {
                        self.recycle_window(window);
                        self.pending_request = None;
                        return Err(ElasticRenderError::SourceWorkerFailed);
                    }
                    let samples = window.release_samples();
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

    fn recycle_window(&mut self, window: ElasticSourceWindow) {
        self.recycle_samples(window.release_samples());
    }

    fn recycle_samples(&mut self, samples: PcmBuf) {
        let Some(port) = self.source_port.as_mut() else {
            self.pending_retirement = Some(samples);
            return;
        };
        if let Err(samples) = port.recycle(samples) {
            self.pending_retirement = Some(samples);
        }
    }
}

pub(super) const fn range_contains(
    container: SourceFrameRange,
    required: SourceFrameRange,
) -> bool {
    container.start() <= required.start() && required.end() <= container.end()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis};
    use kithara_bufpool::PcmPool;
    use kithara_platform::CancelScope;
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{
        super::{ElasticTempoPreparationOutcome, TempoSourcePin},
        *,
    };
    use crate::{
        api::{SessionBeat, Tempo, TrackBinding},
        rt::{
            RenderFrame, StreamShape,
            context::{RenderContext, SessionTransportCommit},
            track::{
                elastic::ElasticPlanError,
                elastic_source::{ElasticSourceRequest, elastic_source_test_pair},
            },
        },
    };

    fn tempo_test_binding(source_tempo: f64, sample_rate: NonZeroU32) -> TrackBinding {
        let frames_per_beat = (60.0 * f64::from(sample_rate.get()) / source_tempo)
            .to_u64()
            .expect("source tempo resolves to an integer frame grid");
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                source_tempo,
                (0..=6).map(|beat| beat * frames_per_beat).collect(),
                vec![0],
                Vec::new(),
            )),
            None,
            frames_per_beat * 6,
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("valid session anchor"),
            TrackBeat::new(0.0).expect("valid track anchor"),
            PlaybackDirection::Forward,
        )
        .expect("valid tempo test binding")
    }

    fn playing_context(
        frames: std::ops::Range<i64>,
        beats: std::ops::Range<f64>,
        tempo: f64,
        revision: u64,
        sample_rate: NonZeroU32,
    ) -> RenderContext {
        RenderContext::new(
            RenderFrame::new(frames.start)..RenderFrame::new(frames.end),
            sample_rate,
            Some(
                SessionBeat::new(beats.start).expect("valid beat start")
                    ..SessionBeat::new(beats.end).expect("valid beat end"),
            ),
            Some(SessionTransportCommit::new(
                Tempo::new(tempo).expect("valid tempo"),
                true,
                revision,
            )),
        )
        .expect("valid playing context")
    }

    #[kithara::test]
    fn tempo_preparation_rejects_an_unsupported_candidate_rate() {
        let pool = PcmPool::default();
        let sample_rate = NonZeroU32::new(48_000).expect("static sample rate");
        let shape = StreamShape {
            sample_rate,
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        let mut renderer = ElasticRenderer::prepare(sample_rate, 1, 172_800, shape, &pool)
            .expect("elastic renderer preparation");
        renderer.primed = true;
        let current_end = 512.0 * 120.0 / (48_000.0 * 60.0);
        let boundary_beat = 2_560.0 * 120.0 / (48_000.0 * 60.0);
        let candidate_end = boundary_beat + 1_024.0 * 60.0 / (48_000.0 * 60.0);
        let current = playing_context(0..512, 0.0..current_end, 120.0, 1, sample_rate);
        let candidate = playing_context(
            2_560..3_584,
            boundary_beat..candidate_end,
            60.0,
            2,
            sample_rate,
        );

        let error = renderer
            .prepare_tempo(
                &tempo_test_binding(100.0, sample_rate),
                &current,
                &candidate,
                2,
            )
            .expect_err("source rate 0.6 is below the supported envelope");
        assert!(matches!(
            error,
            ElasticRenderError::Plan(ElasticPlanError::UnsupportedRate { rate, .. })
                if (rate - 0.6).abs() < 1.0e-9
        ));
        assert!(renderer.tempo_pin.is_none());
    }

    #[kithara::test]
    fn recycle_backpressure_preserves_multiple_ready_windows() {
        let pool = PcmPool::default();
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let mut renderer = ElasticRenderer::prepare(
            sample_rate,
            1,
            44_100,
            StreamShape {
                sample_rate,
                max_block_frames: NonZeroU32::new(512).expect("static block size"),
            },
            &pool,
        )
        .expect("elastic renderer preparation");
        let scope = CancelScope::new(None);
        let (mut port, mut peer) = elastic_source_test_pair(scope.token());
        for _ in 0..4 {
            assert!(port.recycle(pool.get()).is_ok());
        }
        let range = SourceFrameRange::new(0, 1).expect("valid source range");
        assert!(peer.push_ready(1, range, pool.get()));
        assert!(peer.push_ready(2, range, pool.get()));
        renderer.source_port = Some(port);

        renderer.poll_source_port().expect("first reply poll");
        assert!(renderer.pending_retirement.is_some());

        let mut retired = usize::from(peer.pop_recycled().is_some());
        renderer.poll_source_port().expect("second reply poll");
        assert!(renderer.pending_retirement.is_some());
        while peer.pop_recycled().is_some() {
            retired += 1;
        }

        renderer.poll_source_port().expect("final retirement flush");
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

    #[kithara::test]
    fn tempo_window_stays_pinned_until_its_revision_finishes() {
        let pool = PcmPool::default();
        let sample_rate = NonZeroU32::new(48_000).expect("static sample rate");
        let mut renderer = ElasticRenderer::prepare(
            sample_rate,
            1,
            48_000,
            StreamShape {
                sample_rate,
                max_block_frames: NonZeroU32::new(512).expect("static block size"),
            },
            &pool,
        )
        .expect("elastic renderer preparation");
        renderer.tempo_pin = Some(TempoSourcePin {
            range: SourceFrameRange::new(1_000, 2_000).expect("valid pinned range"),
            revision: 7,
        });

        let required = renderer
            .pinned_range(SourceFrameRange::new(1_500, 2_500).expect("valid render range"))
            .expect("combined source range");
        assert_eq!(required.start(), 1_000);
        assert_eq!(required.end(), 2_500);

        renderer.abort_tempo(6);
        assert!(renderer.tempo_pin.is_some());
        renderer.apply_tempo(7);
        assert!(renderer.tempo_pin.is_none());
    }

    #[kithara::test]
    fn tempo_window_waits_for_and_discards_an_incompatible_pending_reply() {
        let pool = PcmPool::default();
        let sample_rate = NonZeroU32::new(48_000).expect("static sample rate");
        let mut renderer = ElasticRenderer::prepare(
            sample_rate,
            1,
            48_000,
            StreamShape {
                sample_rate,
                max_block_frames: NonZeroU32::new(512).expect("static block size"),
            },
            &pool,
        )
        .expect("elastic renderer preparation");
        let scope = CancelScope::new(None);
        let (port, mut peer) = elastic_source_test_pair(scope.token());
        renderer.source_port = Some(port);
        renderer.source_window =
            Some(SourceFrameRange::new(500, 2_500).expect("resident window covers candidate"));
        let incompatible = SourceFrameRange::new(3_000, 4_000).expect("valid incompatible range");
        renderer.pending_request = Some(ElasticSourceRequest::new(1, incompatible));
        let required = SourceFrameRange::new(1_000, 2_000).expect("valid tempo range");

        assert_eq!(
            renderer
                .prepare_tempo_window(7, required)
                .expect("incompatible request remains pending"),
            ElasticTempoPreparationOutcome::Pending
        );
        assert!(peer.push_ready(1, incompatible, pool.get()));
        assert!(matches!(
            renderer
                .prepare_tempo_window(7, required)
                .expect("incompatible reply is discarded"),
            ElasticTempoPreparationOutcome::Ready { .. }
        ));
        assert_eq!(
            renderer.source_window.expect("resident window retained"),
            SourceFrameRange::new(500, 2_500).expect("valid resident range")
        );
        assert!(
            renderer
                .pending_request
                .is_none_or(|request| range_contains(request.range(), required))
        );
    }
}
