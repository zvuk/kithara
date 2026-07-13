use std::num::NonZeroU32;

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_resampler::{MonoStream, MonoStreamConfig, ResamplerBackend, ResamplerOptions};
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{
    detector::{BeatDetectError, BeatDetector, RawBeats},
    grid::{GridParams, build_grid},
};
use crate::{analysis::analyzer::BeatAnalysisConfig, waveform::BeatGrid};

pub(crate) struct BeatPassConfig<B>
where
    B: ResamplerBackend,
{
    source_rate: u32,
    params: GridParams,
    resampler: BeatAnalysisConfig<B>,
    pcm_pool: PcmPool,
}

impl<B> BeatPassConfig<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(
        source_rate: u32,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
        pcm_pool: PcmPool,
    ) -> Self {
        Self {
            source_rate,
            params,
            resampler,
            pcm_pool,
        }
    }
}

/// Streaming front-end for beat detection: downmixes interleaved PCM to
/// mono and incrementally resamples it to the detector's 22 050 Hz input
/// rate. `finalize` flushes the resampler tail, runs the detector, and
/// builds the cleaned [`BeatGrid`] in source frames.
pub(crate) struct BeatAnalyzer<B>
where
    B: ResamplerBackend,
{
    failure: Option<BeatDetectError>,
    params: GridParams,
    feed: MonoFeed,
    resampler: Option<MonoStream<B>>,
    windows: WindowedBeats,
    source_rate: u32,
}

enum MonoFeed {
    Pass,
    Resample,
    Broken,
}

impl<B> BeatAnalyzer<B>
where
    B: ResamplerBackend,
{
    #[must_use]
    pub(crate) fn new(config: BeatPassConfig<B>) -> Self {
        let BeatPassConfig {
            source_rate,
            params,
            resampler: config,
            pcm_pool,
        } = config;
        let (feed, resampler) = if source_rate == config.target_rate() {
            (MonoFeed::Pass, None)
        } else {
            build_mono_stream(source_rate, &config, &pcm_pool).map_or_else(
                || (MonoFeed::Broken, None),
                |r| (MonoFeed::Resample, Some(r)),
            )
        };

        Self {
            failure: None,
            params,
            feed,
            resampler,
            windows: WindowedBeats::new(&config, &pcm_pool),
            source_rate,
        }
    }

    /// # Errors
    /// Propagates the detector failure.
    pub(crate) fn finalize(
        mut self,
        detector: &mut dyn BeatDetector,
    ) -> Result<BeatGrid, BeatDetectError> {
        if let Some(e) = self.failure.take() {
            return Err(e);
        }

        self.resampler.take().map_or(Ok(()), |resampler| {
            let windows = &mut self.windows;
            let mut failure = None;
            let flushed = resampler.finish(|samples| {
                capture_failure(&mut failure, || windows.push_slice(samples, detector));
            });
            if let Err(e) = flushed {
                warn!(?e, "beat analysis: resampler flush failed");
                return Err(BeatDetectError::Buffer);
            }
            failure.map_or(Ok(()), Err)
        })?;

        let raw = self.windows.finish(detector)?;
        Ok(build_grid(&raw, self.source_rate, &self.params))
    }

    pub(crate) fn push_interleaved(
        &mut self,
        pcm: &[f32],
        channels: usize,
        detector: &mut dyn BeatDetector,
    ) {
        if channels == 0 || self.failure.is_some() {
            return;
        }

        let inv = 1.0 / channels.to_f32().unwrap_or(1.0);
        let mono = pcm
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * inv);

        match &mut self.feed {
            MonoFeed::Pass => {
                if let Err(e) = self.windows.push(mono, detector) {
                    self.failure = Some(e);
                }
            }
            MonoFeed::Resample => {
                if let Some(resampler) = &mut self.resampler {
                    let windows = &mut self.windows;
                    let mut failure = None;
                    let pushed = resampler.push(mono, |samples| {
                        capture_failure(&mut failure, || windows.push_slice(samples, detector));
                    });
                    self.failure = match pushed {
                        Ok(()) => failure,
                        Err(e) => {
                            warn!(?e, "beat analysis: resample block failed");
                            Some(BeatDetectError::Buffer)
                        }
                    };
                }
            }
            MonoFeed::Broken => {}
        }
    }
}

fn build_mono_stream<B>(
    source_rate: u32,
    config: &BeatAnalysisConfig<B>,
    pcm_pool: &PcmPool,
) -> Option<MonoStream<B>>
where
    B: ResamplerBackend,
{
    let backend = config.resampler_backend().clone();
    let source_sample_rate = NonZeroU32::new(source_rate)?;
    let target_sample_rate = NonZeroU32::new(config.target_rate())?;
    let stream_config = MonoStreamConfig::builder()
        .backend(backend)
        .source_sample_rate(source_sample_rate)
        .target_sample_rate(target_sample_rate)
        .quality(config.resampler_quality())
        .options(
            ResamplerOptions::builder()
                .chunk_size(config.block_frames())
                .build(),
        )
        .pcm_pool(pcm_pool.clone())
        .build();
    MonoStream::new(stream_config)
        .map_err(|e| {
            warn!(
                ?e,
                source_rate, "beat analysis: resampler construction failed"
            );
        })
        .ok()
}

fn capture_failure<F>(failure: &mut Option<BeatDetectError>, detect: F)
where
    F: FnOnce() -> Result<(), BeatDetectError>,
{
    if failure.is_none() {
        *failure = detect().err();
    }
}

struct WindowedBeats {
    buffer: PcmBuf,
    hop_frames: usize,
    offset_frames: u64,
    ready_frames: usize,
    raw: RawBeats,
    sample_rate: u32,
    window_frames: usize,
}

impl WindowedBeats {
    fn new<B>(config: &BeatAnalysisConfig<B>, pcm_pool: &PcmPool) -> Self
    where
        B: ResamplerBackend,
    {
        let sample_rate = config.target_rate().max(1);
        let window_frames =
            frames_for_seconds(sample_rate, config.detector_window_seconds().max(1));
        let overlap_seconds = config
            .detector_overlap_seconds()
            .min(config.detector_window_seconds().saturating_sub(1));
        let overlap_frames = frames_for_seconds(sample_rate, overlap_seconds);
        let ready_frames = window_frames.saturating_add(overlap_frames);
        let mut buffer = pcm_pool.get();
        if buffer.ensure_len(ready_frames).is_ok() {
            buffer.clear();
        }
        Self {
            buffer,
            hop_frames: window_frames.saturating_sub(overlap_frames).max(1),
            offset_frames: 0,
            ready_frames,
            raw: RawBeats {
                beats: Vec::new(),
                downbeats: Vec::new(),
            },
            sample_rate,
            window_frames,
        }
    }

    fn detect_ready_windows(
        &mut self,
        detector: &mut dyn BeatDetector,
    ) -> Result<(), BeatDetectError> {
        while self.buffer.len() >= self.ready_frames {
            self.detect_window(detector, false)?;
        }
        Ok(())
    }

    fn detect_window(
        &mut self,
        detector: &mut dyn BeatDetector,
        final_window: bool,
    ) -> Result<(), BeatDetectError> {
        let window_len = if final_window {
            self.buffer.len()
        } else {
            self.window_frames
        };
        let keep_frames = if final_window {
            window_len
        } else {
            self.hop_frames.min(window_len)
        };

        let raw = detector.detect(&self.buffer[..window_len])?;
        let sample_rate = self.sample_rate.to_f32().unwrap_or(1.0);
        let offset_seconds = self.offset_frames.to_f32().unwrap_or(f32::MAX) / sample_rate;
        let keep_seconds = keep_frames.to_f32().unwrap_or(f32::MAX) / sample_rate;
        append_window_times(&mut self.raw.beats, raw.beats, offset_seconds, keep_seconds);
        append_window_times(
            &mut self.raw.downbeats,
            raw.downbeats,
            offset_seconds,
            keep_seconds,
        );

        if final_window {
            self.buffer.clear();
        } else {
            self.buffer.drain(..self.hop_frames);
            self.offset_frames = self.offset_frames.saturating_add(self.hop_frames as u64);
        }
        Ok(())
    }

    fn finish(mut self, detector: &mut dyn BeatDetector) -> Result<RawBeats, BeatDetectError> {
        if !self.buffer.is_empty() {
            self.detect_window(detector, true)?;
        }
        normalize_times(&mut self.raw.beats);
        normalize_times(&mut self.raw.downbeats);
        Ok(self.raw)
    }

    fn push(
        &mut self,
        samples: impl Iterator<Item = f32>,
        detector: &mut dyn BeatDetector,
    ) -> Result<(), BeatDetectError> {
        append_iter(&mut self.buffer, samples)?;
        self.detect_ready_windows(detector)
    }

    fn push_slice(
        &mut self,
        samples: &[f32],
        detector: &mut dyn BeatDetector,
    ) -> Result<(), BeatDetectError> {
        append_slice(&mut self.buffer, samples)?;
        self.detect_ready_windows(detector)
    }
}

fn append_window_times(target: &mut Vec<f32>, values: Vec<f32>, offset: f32, keep_until: f32) {
    target.extend(
        values
            .into_iter()
            .filter(|t| t.is_finite() && *t >= 0.0 && *t < keep_until)
            .map(|t| offset + t),
    );
}

fn frames_for_seconds(sample_rate: u32, seconds: u32) -> usize {
    usize::try_from(u64::from(sample_rate) * u64::from(seconds)).unwrap_or(usize::MAX)
}

fn append_iter(
    dst: &mut PcmBuf,
    samples: impl Iterator<Item = f32>,
) -> Result<(), BeatDetectError> {
    for sample in samples {
        let old_len = dst.len();
        dst.ensure_len(old_len.saturating_add(1))
            .map_err(|_| BeatDetectError::Buffer)?;
        dst[old_len] = sample;
    }
    Ok(())
}

fn append_slice(dst: &mut PcmBuf, src: &[f32]) -> Result<(), BeatDetectError> {
    let old_len = dst.len();
    dst.ensure_len(old_len.saturating_add(src.len()))
        .map_err(|_| BeatDetectError::Buffer)?;
    dst[old_len..old_len + src.len()].copy_from_slice(src);
    Ok(())
}

fn normalize_times(values: &mut Vec<f32>) {
    values.retain(|t| t.is_finite() && *t >= 0.0);
    values.sort_by(f32::total_cmp);
    values.dedup_by(|a, b| *a == *b);
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;
    use kithara_platform::sync::Arc;
    use kithara_resampler::{ResamplerBackend, rubato::RubatoBackend};
    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::detector::{BeatDetectError, BeatDetector, BeatDetectorMock, RawBeats},
        BeatAnalyzer,
    };
    use crate::analysis::{
        BeatAnalysisConfig,
        beat::{BeatPassConfig, GridParams},
    };

    struct Consts;

    impl Consts {
        const SRC: u32 = 44_100;
        const TARGET: usize = 22_050;
    }

    fn empty_raw() -> RawBeats {
        RawBeats {
            beats: Vec::new(),
            downbeats: Vec::new(),
        }
    }

    fn pcm_pool() -> PcmPool {
        PcmPool::default()
    }

    fn analyzer(
        source_rate: u32,
        config: BeatAnalysisConfig<RubatoBackend>,
    ) -> BeatAnalyzer<RubatoBackend> {
        BeatAnalyzer::new(BeatPassConfig::new(
            source_rate,
            GridParams::default(),
            config,
            pcm_pool(),
        ))
    }

    /// Mocked detector, called exactly once: `check` asserts on the mono
    /// input it received and scripts the raw outcome.
    fn detector(check: impl Fn(&[f32]) -> RawBeats + Send + Sync + 'static) -> Unimock {
        Unimock::new(
            BeatDetectorMock
                .each_call(matching!(_))
                .answers_arc(Arc::new(move |_, mono| Ok(check(mono)))),
        )
    }

    /// Interleaved stereo, both channels equal to `f(frame_idx)`.
    fn stereo(frames: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = f(n);
            out.push(s);
            out.push(s);
        }
        out
    }

    fn push_chunked<B>(
        analyzer: &mut BeatAnalyzer<B>,
        pcm: &[f32],
        frames_per_chunk: usize,
        detector: &mut dyn BeatDetector,
    ) where
        B: ResamplerBackend,
    {
        for chunk in pcm.chunks(frames_per_chunk * 2) {
            analyzer.push_interleaved(chunk, 2, detector);
        }
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let n: f32 = samples.len().as_();
        (samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt()
    }

    #[kithara::test]
    fn resamples_all_input_without_tail_loss() {
        // 2.0 s of 440 Hz at 44.1 kHz must reach the detector as exactly
        // 2.0 s at 22 050 Hz, with real signal all the way to the end —
        // the resampler tail must be flushed, not dropped.
        let step = std::f32::consts::TAU * 440.0 / 44_100.0;
        let pcm = stereo(2 * 44_100, |n| {
            let t: f32 = n.as_();
            0.5 * (step * t).sin()
        });
        let mut analyzer = analyzer(Consts::SRC, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(|mono| {
            assert_eq!(
                mono.len(),
                2 * Consts::TARGET,
                "every input frame must reach the detector at 22 050 Hz"
            );
            let whole = rms(mono);
            assert!(
                (whole - 0.354).abs() < 0.05,
                "sine RMS must survive resampling, got {whole}"
            );
            let tail = rms(&mono[mono.len() - 256..]);
            assert!(
                tail > 0.2,
                "the final 256 samples must carry signal (tail flushed), rms {tail}"
            );
            empty_raw()
        });
        push_chunked(&mut analyzer, &pcm, 1000, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn resampler_delay_is_trimmed_so_positions_stay_aligned() {
        // 1 s silence then 1 s of DC 0.5: the step must sit at output
        // sample ~22050. An untrimmed resampler delay shifts it late.
        let pcm = stereo(2 * 44_100, |n| if n < 44_100 { 0.0 } else { 0.5 });
        let mut analyzer = analyzer(Consts::SRC, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(|mono| {
            assert_eq!(mono.len(), 2 * Consts::TARGET);
            let crossing = mono
                .iter()
                .position(|s| s.abs() > 0.25)
                .expect("the step must appear in the output");
            let expected = Consts::TARGET;
            assert!(
                crossing.abs_diff(expected) <= 64,
                "step must stay at its source position: got {crossing}, want ~{expected}"
            );
            empty_raw()
        });
        push_chunked(&mut analyzer, &pcm, 4096, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn downmix_is_channel_mean() {
        // L = +0.8, R = -0.8 cancels to mono silence.
        let mut pcm = Vec::with_capacity(44_100 * 2);
        for _ in 0..44_100 {
            pcm.push(0.8);
            pcm.push(-0.8);
        }
        let mut analyzer = analyzer(Consts::SRC, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(|mono| {
            assert_eq!(mono.len(), Consts::TARGET);
            let peak = mono.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
            assert!(peak < 0.05, "cancelling stereo must downmix to ~0: {peak}");
            empty_raw()
        });
        analyzer.push_interleaved(&pcm, 2, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn passthrough_at_detector_rate() {
        // A 22 050 Hz source needs no resampling: the detector sees the input.
        let pcm = stereo(10_000, |_| 0.25);
        let mut analyzer = analyzer(22_050, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(|mono| {
            assert_eq!(mono, vec![0.25_f32; 10_000].as_slice());
            empty_raw()
        });
        push_chunked(&mut analyzer, &pcm, 999, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn custom_detector_rate_controls_passthrough_domain() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .build();
        let pcm = stereo(4096, |_| 0.25);
        let mut analyzer = analyzer(Consts::SRC, config);
        let mut detector = detector(|mono| {
            assert_eq!(mono, vec![0.25_f32; 4096].as_slice());
            empty_raw()
        });
        analyzer.push_interleaved(&pcm, 2, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn detector_input_is_bounded_by_configured_window() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(1)
            .detector_overlap_seconds(0)
            .build();
        let pcm = stereo(3 * usize::try_from(Consts::SRC).unwrap_or(0), |_| 0.25);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_for_detector = Arc::clone(&seen);
        let mut detector = detector(move |mono| {
            seen_for_detector.lock().unwrap().push(mono.len());
            assert!(mono.len() <= usize::try_from(Consts::SRC).unwrap_or(0));
            empty_raw()
        });
        let mut analyzer = analyzer(Consts::SRC, config);

        push_chunked(&mut analyzer, &pcm, 2048, &mut detector);
        analyzer.finalize(&mut detector).expect("mock detects");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.as_slice(), &[44_100, 44_100, 44_100]);
    }

    #[kithara::test]
    fn exact_window_waits_for_final_flush_when_overlap_is_configured() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(2)
            .detector_overlap_seconds(1)
            .build();
        let pcm = stereo(2 * usize::try_from(Consts::SRC).unwrap_or(0), |_| 0.25);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_for_detector = Arc::clone(&seen);
        let mut detector = detector(move |mono| {
            seen_for_detector.lock().unwrap().push(mono.len());
            empty_raw()
        });
        let mut analyzer = analyzer(Consts::SRC, config);

        analyzer.push_interleaved(&pcm, 2, &mut detector);
        assert!(seen.lock().unwrap().is_empty());
        analyzer.finalize(&mut detector).expect("mock detects");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.as_slice(), &[2 * 44_100]);
    }

    #[kithara::test]
    fn finalize_builds_grid_in_source_frames() {
        // 9 downbeats every 2.0 s -> 120 bpm, positions converted at the
        // SOURCE rate (48 kHz here), not the detector's 22 050 Hz.
        let raw = RawBeats {
            beats: (0..33)
                .map(|n| {
                    let t: f32 = n.as_();
                    t * 0.5
                })
                .collect(),
            downbeats: (0..9)
                .map(|n| {
                    let t: f32 = n.as_();
                    t * 2.0
                })
                .collect(),
        };
        let mut analyzer = analyzer(48_000, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(move |_| raw.clone());
        analyzer.push_interleaved(&stereo(17 * 48_000, |_| 0.1), 2, &mut detector);
        let grid = analyzer.finalize(&mut detector).expect("mock detects");

        assert!(
            (grid.bpm() - 120.0).abs() < 1e-6,
            "2 s bars are 120 bpm, got {}",
            grid.bpm()
        );
        assert_eq!(grid.downbeats().len(), 9);
        assert_eq!(grid.downbeats()[1], 96_000, "downbeats are source frames");
        assert_eq!(grid.beats()[1], 24_000, "beats are source frames");
        assert!(
            grid.segments().is_empty(),
            "9 downbeats are below the stable window: tempo only"
        );
    }

    #[kithara::test]
    fn detector_failure_propagates() {
        let mut analyzer = analyzer(Consts::SRC, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector =
            Unimock::new(BeatDetectorMock.next_call(matching!(_)).answers(&|_, _| {
                Err(BeatDetectError::Detect {
                    reason: "scripted".to_string(),
                })
            }));
        analyzer.push_interleaved(&stereo(4096, |_| 0.1), 2, &mut detector);
        assert!(analyzer.finalize(&mut detector).is_err());
    }
}
