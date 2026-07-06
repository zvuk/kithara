use std::sync::Arc;

use kithara_decode::PcmChunk;
use kithara_platform::sync::Mutex;
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{
    TARGET_RATE,
    detector::{BeatDetectError, BeatDetector},
    grid::{GridParams, build_grid},
    resampler::MonoResampler,
};
use crate::{analysis::analyzer::Analyzer, waveform::BeatGrid};

/// Detector shared across sequential per-track analyzers; model load is
/// paid once at registration, the worker runs one analysis at a time.
pub(crate) type SharedBeatDetector = Arc<Mutex<Box<dyn BeatDetector>>>;

/// Streaming front-end for beat detection: downmixes interleaved PCM to
/// mono and incrementally resamples it to the detector's 22 050 Hz input
/// rate. `finalize` flushes the resampler tail, runs the detector, and
/// builds the cleaned [`BeatGrid`] in source frames.
pub(crate) struct BeatAnalyzer {
    params: GridParams,
    feed: MonoFeed,
    source_rate: u32,
}

/// Mono accumulation path: resample, pass through at the target rate, or
/// degrade to nothing when the resampler cannot be built (degenerate rate).
enum MonoFeed {
    Pass(Vec<f32>),
    Resample(Box<MonoResampler>),
    Broken,
}

impl BeatAnalyzer {
    #[must_use]
    pub(crate) fn new(source_rate: u32, params: GridParams) -> Self {
        let feed = if source_rate == TARGET_RATE {
            MonoFeed::Pass(Vec::new())
        } else {
            MonoResampler::new(source_rate)
                .map(Box::new)
                .map_or(MonoFeed::Broken, MonoFeed::Resample)
        };

        Self {
            params,
            feed,
            source_rate,
        }
    }

    /// Flush the resampler tail, detect beats on the whole mono track, and
    /// build the grid. Positions are source frames at the analyzer's rate.
    ///
    /// # Errors
    /// Propagates the detector failure.
    pub(crate) fn finalize(
        self,
        detector: &mut dyn BeatDetector,
    ) -> Result<BeatGrid, BeatDetectError> {
        let mono = match self.feed {
            MonoFeed::Pass(out) => out,
            MonoFeed::Resample(resampler) => resampler.finish(),
            MonoFeed::Broken => Vec::new(),
        };

        let raw = detector.detect(&mono)?;
        Ok(build_grid(&raw, self.source_rate, &self.params))
    }

    /// Fold one interleaved chunk: downmix to mono (channel mean) and feed
    /// the incremental resampler.
    pub(crate) fn push_interleaved(&mut self, pcm: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }

        let inv = 1.0 / channels.to_f32().unwrap_or(1.0);
        let mono = pcm
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * inv);

        match &mut self.feed {
            MonoFeed::Pass(out) => out.extend(mono),
            MonoFeed::Resample(resampler) => resampler.push(mono),
            MonoFeed::Broken => {}
        }
    }
}

/// [`Analyzer`] adapter over [`BeatAnalyzer`] and the shared detector. Its
/// artifact is `Option<BeatGrid>`: `None` when detection fails (logged).
pub(crate) struct BeatPass {
    analyzer: BeatAnalyzer,
    detector: SharedBeatDetector,
}

impl BeatPass {
    pub(crate) fn new(source_rate: u32, params: GridParams, detector: SharedBeatDetector) -> Self {
        Self {
            detector,
            analyzer: BeatAnalyzer::new(source_rate, params),
        }
    }
}

impl Analyzer for BeatPass {
    type Output = Option<BeatGrid>;

    fn finish(self) -> Option<BeatGrid> {
        let mut detector = self.detector.lock();
        match self.analyzer.finalize(detector.as_mut()) {
            Ok(grid) => Some(grid),
            Err(e) => {
                warn!(?e, "beat analysis failed; leaving the beat slot empty");
                None
            }
        }
    }

    fn push(&mut self, chunk: &PcmChunk) {
        let channels = usize::from(chunk.spec().channels.max(1));
        self.analyzer.push_interleaved(&chunk.samples[..], channels);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::detector::{BeatDetectError, BeatDetectorMock, RawBeats},
        BeatAnalyzer,
    };
    use crate::analysis::beat::GridParams;

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

    /// Mocked detector, called exactly once: `check` asserts on the mono
    /// input it received and scripts the raw outcome.
    fn detector(check: impl Fn(&[f32]) -> RawBeats + Send + Sync + 'static) -> Unimock {
        Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
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

    fn push_chunked(analyzer: &mut BeatAnalyzer, pcm: &[f32], frames_per_chunk: usize) {
        for chunk in pcm.chunks(frames_per_chunk * 2) {
            analyzer.push_interleaved(chunk, 2);
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
        let mut analyzer = BeatAnalyzer::new(Consts::SRC, GridParams::default());
        push_chunked(&mut analyzer, &pcm, 1000);

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
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn resampler_delay_is_trimmed_so_positions_stay_aligned() {
        // 1 s silence then 1 s of DC 0.5: the step must sit at output
        // sample ~22050. An untrimmed resampler delay shifts it late.
        let pcm = stereo(2 * 44_100, |n| if n < 44_100 { 0.0 } else { 0.5 });
        let mut analyzer = BeatAnalyzer::new(Consts::SRC, GridParams::default());
        push_chunked(&mut analyzer, &pcm, 4096);

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
        let mut analyzer = BeatAnalyzer::new(Consts::SRC, GridParams::default());
        analyzer.push_interleaved(&pcm, 2);

        let mut detector = detector(|mono| {
            assert_eq!(mono.len(), Consts::TARGET);
            let peak = mono.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
            assert!(peak < 0.05, "cancelling stereo must downmix to ~0: {peak}");
            empty_raw()
        });
        analyzer.finalize(&mut detector).expect("mock detects");
    }

    #[kithara::test]
    fn passthrough_at_detector_rate() {
        // A 22 050 Hz source needs no resampling: the detector sees the input.
        let pcm = stereo(10_000, |_| 0.25);
        let mut analyzer = BeatAnalyzer::new(22_050, GridParams::default());
        push_chunked(&mut analyzer, &pcm, 999);

        let mut detector = detector(|mono| {
            assert_eq!(mono, vec![0.25_f32; 10_000].as_slice());
            empty_raw()
        });
        analyzer.finalize(&mut detector).expect("mock detects");
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
        let mut analyzer = BeatAnalyzer::new(48_000, GridParams::default());
        analyzer.push_interleaved(&stereo(48_000, |_| 0.1), 2);

        let mut detector = detector(move |_| raw.clone());
        let grid = analyzer.finalize(&mut detector).expect("mock detects");

        assert!(
            (grid.bpm - 120.0).abs() < 1e-6,
            "2 s bars are 120 bpm, got {}",
            grid.bpm
        );
        assert_eq!(grid.downbeats.len(), 9);
        assert_eq!(grid.downbeats[1], 96_000, "downbeats are source frames");
        assert_eq!(grid.beats[1], 24_000, "beats are source frames");
        assert!(
            grid.segments.is_empty(),
            "9 downbeats are below the stable window: tempo only"
        );
    }

    #[kithara::test]
    fn detector_failure_propagates() {
        let mut analyzer = BeatAnalyzer::new(Consts::SRC, GridParams::default());
        analyzer.push_interleaved(&stereo(4096, |_| 0.1), 2);
        let mut detector =
            Unimock::new(BeatDetectorMock.next_call(matching!(_)).answers(&|_, _| {
                Err(BeatDetectError::Detect {
                    reason: "scripted".to_string(),
                })
            }));
        assert!(analyzer.finalize(&mut detector).is_err());
    }
}
