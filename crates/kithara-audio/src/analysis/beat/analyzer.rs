use std::{iter, sync::Arc};

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use kithara_decode::PcmChunk;
use kithara_platform::sync::Mutex;
use num_traits::cast::ToPrimitive;
use rubato::{Fft, FixedSync, Resampler};
use tracing::warn;

use super::{
    detector::{BeatDetectError, BeatDetector},
    grid::{GridParams, build_grid},
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
    source_rate: u32,
    params: GridParams,
    feed: MonoFeed,
}

/// Mono accumulation path: resample, pass through at the target rate, or
/// degrade to nothing when the resampler cannot be built (degenerate rate).
enum MonoFeed {
    Pass(Vec<f32>),
    Resample(MonoResampler),
    Broken,
}

impl BeatAnalyzer {
    /// The detector's contractual input rate (`BeatDetector::detect`).
    const TARGET_RATE: u32 = 22_050;

    /// Input frames per resampler block. Off-line analysis: latency-free, so a
    /// comfortable block keeps FFT overhead low.
    const BLOCK_FRAMES: usize = 1024;

    #[must_use]
    pub(crate) fn new(source_rate: u32, params: GridParams) -> Self {
        let feed = if source_rate == Self::TARGET_RATE {
            MonoFeed::Pass(Vec::new())
        } else {
            MonoResampler::new(source_rate).map_or(MonoFeed::Broken, MonoFeed::Resample)
        };

        Self {
            source_rate,
            params,
            feed,
        }
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
}

/// Incremental mono `source_rate` → 22 050 Hz resampler over rubato's
/// synchronous FFT engine. `finish` pads with silence until the engine has
/// emitted its whole delay line, then trims the leading delay and truncates
/// to the duration-exact length — no tail loss, no position shift.
struct MonoResampler {
    fft: Box<Fft<f32>>,
    /// Output frames per input frame (`22 050 / source_rate`).
    ratio: f64,
    /// Engine delay in output frames, trimmed from the front.
    delay: usize,
    /// Mono input awaiting a full block.
    pending: Vec<f32>,
    /// Single-channel scratch in the adapter's slice-of-vecs shape.
    input_block: Vec<Vec<f32>>,
    output_block: Vec<Vec<f32>>,
    /// Accumulated raw engine output (still carries the leading delay).
    out: Vec<f32>,
    /// Real (non-padding) input frames seen.
    total_in: u64,
}

impl MonoResampler {
    fn new(source_rate: u32) -> Option<Self> {
        let fft = Fft::<f32>::new(
            source_rate.to_usize()?,
            BeatAnalyzer::TARGET_RATE.to_usize()?,
            BeatAnalyzer::BLOCK_FRAMES,
            2,
            1,
            FixedSync::Input,
        )
        .map_err(|e| {
            warn!(
                ?e,
                source_rate, "beat analysis: resampler construction failed"
            );
        })
        .ok()?;

        let delay = fft.output_delay();

        Some(Self {
            fft: Box::new(fft),
            ratio: f64::from(BeatAnalyzer::TARGET_RATE) / f64::from(source_rate),
            delay,
            pending: Vec::new(),
            input_block: vec![Vec::new()],
            output_block: vec![Vec::new()],
            out: Vec::new(),
            total_in: 0,
        })
    }

    fn push(&mut self, mono: impl Iterator<Item = f32>) {
        let before = self.pending.len();
        self.pending.extend(mono);
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.fft.input_frames_next() {
            if !self.process_block() {
                return;
            }
        }
    }

    /// Run one fixed-size input block through the engine, appending its
    /// output. `false` on an engine error (the block is dropped).
    fn process_block(&mut self) -> bool {
        let needed = self.fft.input_frames_next();
        let out_next = self.fft.output_frames_next();
        self.input_block[0].clear();
        self.input_block[0].extend_from_slice(&self.pending[..needed]);
        self.output_block[0].resize(out_next, 0.0);

        let written = SequentialSliceOfVecs::new(&self.input_block, 1, needed)
            .map_err(|e| e.to_string())
            .and_then(|input| {
                let mut output =
                    SequentialSliceOfVecs::new_mut(&mut self.output_block, 1, out_next)
                        .map_err(|e| e.to_string())?;
                self.fft
                    .process_into_buffer(&input, &mut output, None)
                    .map(|(_, written)| written)
                    .map_err(|e| e.to_string())
            });
        self.pending.drain(..needed);

        match written {
            Ok(written) => {
                self.out.extend_from_slice(&self.output_block[0][..written]);
                true
            }
            Err(e) => {
                warn!(e, "beat analysis: resample block failed; dropped");
                false
            }
        }
    }

    /// Flush: pad with silence until the engine has emitted `delay` plus the
    /// duration-exact output, then cut the delay off the front. The lesson of
    /// the waveform analyzer's lost tail, made structural.
    fn finish(mut self) -> Vec<f32> {
        let expected = self
            .total_in
            .to_f64()
            .map(|frames| frames * self.ratio)
            .and_then(|frames| frames.round().to_usize())
            .unwrap_or(0);

        while self.out.len() < self.delay + expected {
            let needed = self.fft.input_frames_next();
            let pad = needed.saturating_sub(self.pending.len());
            self.pending.extend(iter::repeat_n(0.0_f32, pad));
            if !self.process_block() {
                break;
            }
        }

        let mut out = self.out;
        out.drain(..self.delay.min(out.len()));
        out.truncate(expected);
        out
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
            analyzer: BeatAnalyzer::new(source_rate, params),
            detector,
        }
    }
}

impl Analyzer for BeatPass {
    type Output = Option<BeatGrid>;

    fn push(&mut self, chunk: &PcmChunk) {
        let channels = usize::from(chunk.spec().channels.max(1));
        self.analyzer.push_interleaved(&chunk.samples[..], channels);
    }

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
        // A 22 050 Hz source needs no resampling: the detector sees the
        // downmixed input bit-exactly.
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
