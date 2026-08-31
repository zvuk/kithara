use std::collections::{BTreeMap, BTreeSet};

use bon::Builder;
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_resampler::ResamplerBackend;
use num_traits::cast::ToPrimitive;

use super::{
    detector::{BeatDetectError, BeatDetector, BeatMark, RawBeats},
    grid::{GridBuffers, GridParams, build_grid_with, fold_octave},
    runs::{Intake, Runs},
};
use crate::{
    BeatArtifact, BlobError,
    analyzer::BeatAnalysisConfig,
    blob::Writer,
    coverage::{Coverage, FrameRange},
    progress::{BeatMarkResume, BeatResume, RawBeatsResume},
};

/// Detector windows of backlog the pass holds before it turns audio down.
const BUDGET_WINDOWS: usize = 4;

#[derive(Clone, Copy)]
struct WindowMeta {
    full: bool,
    index: usize,
    keep_seconds: f32,
    offset_seconds: f32,
}

pub(crate) struct DetectRequest {
    input: SampleBuffer,
    window: WindowMeta,
}

pub(crate) struct DetectOutput {
    result: Result<RawBeats, BeatDetectError>,
    window: WindowMeta,
}

impl DetectRequest {
    pub(crate) fn detect(self, detector: &dyn BeatDetector) -> DetectOutput {
        DetectOutput {
            result: detector.detect(&self.input),
            window: self.window,
        }
    }
}

#[derive(Builder)]
pub(crate) struct BeatPassConfig<B, S>
where
    B: ResamplerBackend,
{
    resampler: BeatAnalysisConfig<B>,
    #[builder(default)]
    params: GridParams,
    pools: PoolRegion<S>,
    source_rate: u32,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct BeatAnalyzer<B>
where
    B: ResamplerBackend,
{
    params: GridParams,
    failure: Option<BeatDetectError>,
    downmix: SampleBuffer,
    grid: GridBuffers,
    runs: Runs<B>,
    windows: BTreeMap<usize, RawBeats>,
    short: BTreeSet<usize>,
    hop_frames: usize,
    min_frames: usize,
    ready_frames: usize,
    window_frames: usize,
    #[field(get, copy, vis = "pub(crate)")]
    source_rate: u32,
}

impl<B> BeatAnalyzer<B>
where
    B: ResamplerBackend,
{
    #[must_use]
    pub(crate) fn new<S>(config: BeatPassConfig<B, S>) -> Self
    where
        S: HasPool<f32>,
    {
        let BeatPassConfig {
            source_rate,
            params,
            resampler: config,
            pools,
        } = config;

        let detector_rate = config.target_rate().max(1);
        let window_frames =
            frames_for_seconds(detector_rate, config.detector_window_seconds().max(1));
        let overlap_seconds = config
            .detector_overlap_seconds()
            .min(config.detector_window_seconds().saturating_sub(1));
        let overlap_frames = frames_for_seconds(detector_rate, overlap_seconds);
        let ready_frames = window_frames.saturating_add(overlap_frames);
        let hop_frames = window_frames.saturating_sub(overlap_frames).max(1);
        // Four detector windows of backlog, in at most four runs. Held audio
        // past the budget then always includes a run the detector can read,
        // which is what frees room for the audio the pass waits to take.
        let budget = ready_frames.saturating_mul(BUDGET_WINDOWS);

        Self {
            params,
            hop_frames,
            min_frames: frames_for_seconds(detector_rate, config.detector_min_window_seconds())
                .min(window_frames)
                .max(1),
            ready_frames,
            window_frames,
            runs: Runs::new(config, source_rate, budget, BUDGET_WINDOWS),
            windows: BTreeMap::new(),
            short: BTreeSet::new(),
            failure: None,
            downmix: pools.get::<f32>(),
            grid: GridBuffers::new(&pools),
            source_rate,
        }
    }

    /// What the pass never took, which is only ever the tail a source stopped
    /// short of delivering.
    pub(crate) fn unanalysed(&self, extent: Option<u64>) -> Vec<FrameRange> {
        extent.map_or_else(Vec::new, |extent| self.runs.taken().gaps(extent))
    }

    delegate::delegate! {
        to self.runs {
            pub(crate) fn intake(&self) -> Intake;
            #[call(taken)]
            pub(crate) fn coverage(&self) -> &Coverage;
            #[cfg(test)]
            #[call(held)]
            pub(crate) fn held_frames(&self) -> usize;
        }
    }

    pub(crate) fn snapshot<S>(
        &mut self,
        pools: &PoolRegion<S>,
        detector: &dyn BeatDetector,
        ending: bool,
    ) -> Result<BeatArtifact, BeatDetectError>
    where
        S: HasPool<f32>,
    {
        if ending {
            self.runs.flush()?;
        }
        self.detect(pools, detector, ending)?;

        self.build_artifact()
    }

    pub(crate) fn snapshot_deferred(
        &mut self,
        ending: bool,
    ) -> Result<BeatArtifact, BeatDetectError> {
        if ending {
            self.runs.flush()?;
        }
        self.build_artifact()
    }

    fn build_artifact(&mut self) -> Result<BeatArtifact, BeatDetectError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }

        let mut raw = RawBeats {
            beats: Vec::new(),
            downbeats: Vec::new(),
        };
        for window in self.windows.values() {
            raw.beats.extend_from_slice(&window.beats);
            raw.downbeats.extend_from_slice(&window.downbeats);
        }
        normalize_marks(&mut raw.beats);
        normalize_marks(&mut raw.downbeats);

        build_grid_with(&raw, self.source_rate, &self.params, &mut self.grid)
            .map(fold_octave)
            .map_err(Into::into)
    }

    pub(crate) fn push_interleaved<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: bool,
        detector: &dyn BeatDetector,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        let took = self.push_interleaved_deferred(pools, pcm, channels, at, opens);
        self.failure = self.detect(pools, detector, false).err();
        took
    }

    pub(crate) fn push_interleaved_deferred<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
        opens: bool,
    ) -> bool
    where
        S: HasPool<f32>,
    {
        if channels == 0 || self.failure.is_some() {
            return false;
        }
        let frames = pcm.len() / channels;
        if frames == 0 {
            return false;
        }

        let inv = 1.0 / channels.to_f32().unwrap_or(1.0);
        if let Err(error) = self.downmix.ensure_len(frames) {
            self.failure = Some(error.into());
            return false;
        }
        self.downmix.truncate(frames);
        for (dst, frame) in self.downmix.iter_mut().zip(pcm.chunks_exact(channels)) {
            *dst = frame.iter().sum::<f32>() * inv;
        }
        match self.runs.push(pools, &self.downmix, at, opens) {
            Ok(took) => took,
            Err(error) => {
                self.failure = Some(error);
                false
            }
        }
    }

    pub(crate) fn prepare_detection<S>(
        &mut self,
        pools: &PoolRegion<S>,
        trailing: bool,
    ) -> Option<DetectRequest>
    where
        S: HasPool<f32>,
    {
        if self.failure.is_some() {
            return None;
        }
        if trailing && let Err(error) = self.runs.flush() {
            self.failure = Some(error);
            return None;
        }
        let rate = self.runs.target_rate().to_f32().unwrap_or(1.0);

        for (start, mono) in self.runs.spans() {
            let base = self.runs.offset_in_run(0, start);
            let mut index = base.div_ceil(self.hop_frames);
            loop {
                let span = index.saturating_mul(self.hop_frames);
                let Some(offset) = span.checked_sub(base) else {
                    break;
                };
                let Some(available) = mono.len().checked_sub(offset).filter(|left| *left > 0)
                else {
                    break;
                };

                let full = available >= self.ready_frames;
                if !full && !trailing && available < self.min_frames {
                    break;
                }
                let known = self.windows.contains_key(&index);
                if !known || (full && self.short.contains(&index)) {
                    let end = if full {
                        offset.saturating_add(self.window_frames)
                    } else {
                        mono.len()
                    };
                    let input = mono.get(offset..end)?;
                    let mut owned = match pools.get_with_len::<f32>(input.len()) {
                        Ok(owned) => owned,
                        Err(error) => {
                            self.failure = Some(error.into());
                            return None;
                        }
                    };
                    owned.copy_from_slice(input);
                    let keep = if full { self.hop_frames } else { available };
                    return Some(DetectRequest {
                        input: owned,
                        window: WindowMeta {
                            full,
                            index,
                            keep_seconds: keep.to_f32().unwrap_or(f32::MAX) / rate,
                            offset_seconds: span.to_f32().unwrap_or(f32::MAX) / rate,
                        },
                    });
                }
                if !full {
                    break;
                }
                index = index.saturating_add(1);
            }
        }
        None
    }

    pub(crate) fn apply_detection(&mut self, output: DetectOutput) {
        match output.result {
            Ok(raw) => self.apply_raw(output.window, raw),
            Err(error) => self.failure = Some(error),
        }
    }

    pub(crate) fn write_resume(&mut self, out: &mut Vec<u8>) {
        let mut writer = Writer::new(out);
        self.runs.write_resume(&mut writer);
        writer.write_len(self.windows.len());
        for (index, raw) in &self.windows {
            writer.write_u64(u64::try_from(*index).unwrap_or(u64::MAX));
            write_marks(&mut writer, &raw.beats);
            write_marks(&mut writer, &raw.downbeats);
        }
        writer.write_len(self.short.len());
        for index in &self.short {
            writer.write_u64(u64::try_from(*index).unwrap_or(u64::MAX));
        }
    }

    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: BeatResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        let BeatResume {
            runs,
            taken,
            windows,
            short,
        } = resume;
        self.runs.restore(pools, runs, taken)?;
        self.windows = windows
            .into_iter()
            .map(|(index, raw)| (index, raw_beats(raw)))
            .collect();
        self.short = short;
        if self
            .short
            .iter()
            .any(|index| !self.windows.contains_key(index))
        {
            return Err(BlobError::Corrupt);
        }
        self.failure = None;
        Ok(())
    }

    fn apply_raw(&mut self, window: WindowMeta, raw: RawBeats) {
        self.windows.insert(
            window.index,
            RawBeats {
                beats: window_marks(raw.beats, window.offset_seconds, window.keep_seconds),
                downbeats: window_marks(raw.downbeats, window.offset_seconds, window.keep_seconds),
            },
        );
        if window.full {
            self.short.remove(&window.index);
        } else {
            self.short.insert(window.index);
        }
        self.release_detected();
    }

    /// A run releases everything ahead of the window it still waits on: earlier
    /// audio fed a full window, and a window is read again only when it was cut
    /// short. A run that has fed none keeps what it holds, since the audio in
    /// front of its first window belongs to a window starting before it.
    fn release_detected(&mut self) {
        let (windows, short, hop) = (&self.windows, &self.short, self.hop_frames);
        self.runs.release(|base| {
            let first = base.div_ceil(hop);
            let mut index = first;
            while windows.contains_key(&index) && !short.contains(&index) {
                index = index.saturating_add(1);
            }
            if index == first {
                base
            } else {
                index.saturating_mul(hop)
            }
        });
    }

    fn detect<S>(
        &mut self,
        pools: &PoolRegion<S>,
        detector: &dyn BeatDetector,
        trailing: bool,
    ) -> Result<(), BeatDetectError>
    where
        S: HasPool<f32>,
    {
        while let Some(request) = self.prepare_detection(pools, trailing) {
            let DetectOutput { result, window } = request.detect(detector);
            let raw = result?;
            self.apply_raw(window, raw);
        }
        Ok(())
    }
}

fn window_marks(marks: Vec<BeatMark>, offset: f32, keep_until: f32) -> Vec<BeatMark> {
    marks
        .into_iter()
        .filter(|mark| mark.at.is_finite() && mark.at >= 0.0 && mark.at < keep_until)
        .map(|mark| BeatMark {
            at: offset + mark.at,
            ..mark
        })
        .collect()
}

fn write_marks(writer: &mut Writer<'_>, marks: &[BeatMark]) {
    writer.write_len(marks.len());
    for mark in marks {
        writer.write_f32(mark.at);
        writer.write_f32(mark.confidence);
    }
}

fn raw_beats(raw: RawBeatsResume) -> RawBeats {
    RawBeats {
        beats: raw.beats.into_iter().map(beat_mark).collect(),
        downbeats: raw.downbeats.into_iter().map(beat_mark).collect(),
    }
}

const fn beat_mark(mark: BeatMarkResume) -> BeatMark {
    BeatMark {
        at: mark.at,
        confidence: mark.confidence,
    }
}

fn frames_for_seconds(sample_rate: u32, seconds: u32) -> usize {
    usize::try_from(u64::from(sample_rate) * u64::from(seconds)).unwrap_or(usize::MAX)
}

fn normalize_marks(marks: &mut Vec<BeatMark>) {
    marks.retain(|mark| mark.at.is_finite() && mark.at >= 0.0);
    marks.sort_by(|a, b| a.at.total_cmp(&b.at));
    marks.dedup_by(|dropped, kept| {
        if dropped.at != kept.at {
            return false;
        }
        kept.confidence = kept.confidence.max(dropped.confidence);
        true
    });
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::{Arc, Mutex};
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::detector::{BeatDetectError, BeatDetector, BeatDetectorMock, BeatMark, RawBeats},
        BeatAnalyzer, normalize_marks, window_marks,
    };
    use crate::{
        BeatAnalysisConfig,
        beat::BeatPassConfig,
        test_pools::{TestPools, pools},
    };

    struct Consts;

    impl Consts {
        const SRC: u32 = 44_100;
        const TARGET: usize = 22_050;
    }

    #[kithara::test(native, flash(false))]
    fn a_block_boundary_moves_a_mark_without_touching_its_confidence() {
        let marks = vec![
            BeatMark {
                at: 0.25,
                confidence: 0.9,
            },
            BeatMark {
                at: 0.75,
                confidence: 0.1,
            },
        ];

        let moved = window_marks(marks, 10.0, 1.0);

        assert_eq!(
            moved.iter().map(|mark| mark.at).collect::<Vec<_>>(),
            vec![10.25, 10.75],
            "positions move onto the track timeline"
        );
        assert_eq!(
            moved.iter().map(|mark| mark.confidence).collect::<Vec<_>>(),
            vec![0.9, 0.1],
            "confidences ride along untouched"
        );
    }

    #[kithara::test(native, flash(false))]
    fn two_windows_reporting_one_beat_keep_the_surer_answer() {
        let mut marks = vec![
            BeatMark {
                at: 1.0,
                confidence: 0.4,
            },
            BeatMark {
                at: 1.0,
                confidence: 0.8,
            },
            BeatMark {
                at: 2.0,
                confidence: 0.6,
            },
        ];

        normalize_marks(&mut marks);

        assert_eq!(marks.len(), 2, "the doubled beat is one mark");
        assert_eq!(marks[0].confidence, 0.8, "the surer window wins");
        assert_eq!(marks[1].confidence, 0.6);
    }

    fn empty_raw() -> RawBeats {
        RawBeats {
            beats: Vec::new(),
            downbeats: Vec::new(),
        }
    }

    struct Pass {
        analyzer: BeatAnalyzer<RubatoBackend>,
        pools: kithara_bufpool::PoolRegion<TestPools>,
    }

    impl Pass {
        fn push_interleaved(
            &mut self,
            pcm: &[f32],
            channels: usize,
            at: u64,
            opens: bool,
            detector: &dyn BeatDetector,
        ) -> bool {
            self.analyzer
                .push_interleaved(&self.pools, pcm, channels, at, opens, detector)
        }

        fn snapshot(
            &mut self,
            detector: &dyn BeatDetector,
            ending: bool,
        ) -> Result<crate::BeatArtifact, BeatDetectError> {
            self.analyzer.snapshot(&self.pools, detector, ending)
        }

        fn write_resume(&mut self, out: &mut Vec<u8>) {
            self.analyzer.write_resume(out);
        }

        fn push_interleaved_deferred(
            &mut self,
            pcm: &[f32],
            channels: usize,
            at: u64,
            opens: bool,
        ) -> bool {
            self.analyzer
                .push_interleaved_deferred(&self.pools, pcm, channels, at, opens)
        }

        fn prepare_detection(&mut self, trailing: bool) -> Option<super::DetectRequest> {
            self.analyzer.prepare_detection(&self.pools, trailing)
        }

        fn apply_detection(&mut self, output: super::DetectOutput) {
            self.analyzer.apply_detection(output);
        }

        fn held_frames(&self) -> usize {
            self.analyzer.held_frames()
        }

        fn unanalysed(&self, extent: Option<u64>) -> Vec<crate::coverage::FrameRange> {
            self.analyzer.unanalysed(extent)
        }
    }

    fn analyzer(source_rate: u32, config: BeatAnalysisConfig<RubatoBackend>) -> Pass {
        let pools = pools();
        let analyzer = BeatAnalyzer::new(
            BeatPassConfig::builder()
                .source_rate(source_rate)
                .resampler(config)
                .pools(pools.clone())
                .build(),
        );
        Pass { analyzer, pools }
    }

    fn detector(check: impl Fn(&[f32]) -> RawBeats + Send + Sync + 'static) -> Unimock {
        Unimock::new(
            BeatDetectorMock
                .each_call(matching!(_))
                .answers_arc(Arc::new(move |_, mono| Ok(check(mono)))),
        )
    }

    fn stereo(frames: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = f(n);
            out.push(s);
            out.push(s);
        }
        out
    }

    fn push_chunked(
        analyzer: &mut Pass,
        pcm: &[f32],
        frames_per_chunk: usize,
        detector: &dyn BeatDetector,
    ) {
        let mut at = 0;
        for chunk in pcm.chunks(frames_per_chunk * 2) {
            analyzer.push_interleaved(chunk, 2, at, true, detector);
            at += u64::try_from(chunk.len() / 2).unwrap_or(0);
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
    fn resume_between_blocks_leaves_no_step_in_the_audio() {
        // A 440 Hz sine at 22 050 Hz moves at most 0.063 between neighbouring
        // samples. Anything larger is a seam, and an onset detector reads a
        // seam as a beat.
        let step = std::f32::consts::TAU * 440.0 / 44_100.0;
        let pcm = stereo(2 * 44_100, |n| {
            let t: f32 = n.as_();
            0.5 * (step * t).sin()
        });
        let mut analyzer = analyzer(Consts::SRC, BeatAnalysisConfig::<RubatoBackend>::default());
        let detector = detector(|mono| {
            let worst = mono
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0f32, f32::max);
            assert!(
                worst < 0.1,
                "neighbouring samples jump by {worst} in a 440 Hz sine"
            );
            empty_raw()
        });

        let mut resume = Vec::new();
        let mut at = 0;
        for chunk in pcm.chunks(1000 * 2) {
            analyzer.push_interleaved(chunk, 2, at, true, &detector);
            at += u64::try_from(chunk.len() / 2).unwrap_or(0);
            resume.clear();
            analyzer.write_resume(&mut resume);
        }
        analyzer.snapshot(&detector, true).expect("mock detects");
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
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
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
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
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
        analyzer.push_interleaved(&pcm, 2, 0, true, &mut detector);
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
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
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
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
        analyzer.push_interleaved(&pcm, 2, 0, true, &mut detector);
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
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
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_detector = Arc::clone(&seen);
        let mut detector = detector(move |mono| {
            seen_for_detector.lock().push(mono.len());
            assert!(mono.len() <= usize::try_from(Consts::SRC).unwrap_or(0));
            empty_raw()
        });
        let mut analyzer = analyzer(Consts::SRC, config);

        push_chunked(&mut analyzer, &pcm, 2048, &mut detector);
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");

        let seen = seen.lock().clone();
        assert_eq!(seen.as_slice(), &[44_100, 44_100, 44_100]);
    }

    #[kithara::test]
    fn a_run_at_the_minimum_is_detected_before_the_flush() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(2)
            .detector_overlap_seconds(1)
            .build();
        let pcm = stereo(2 * usize::try_from(Consts::SRC).unwrap_or(0), |_| 0.25);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_detector = Arc::clone(&seen);
        let mut detector = detector(move |mono| {
            seen_for_detector.lock().push(mono.len());
            empty_raw()
        });
        let mut analyzer = analyzer(Consts::SRC, config);

        analyzer.push_interleaved(&pcm, 2, 0, true, &mut detector);
        assert_eq!(
            seen.lock().as_slice(),
            &[2 * 44_100],
            "the run is usable as soon as it reaches the minimum"
        );
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");

        let seen = seen.lock().clone();
        assert_eq!(
            seen.as_slice(),
            &[2 * 44_100],
            "the flush must not re-run a window that cannot grow"
        );
    }

    #[kithara::test]
    fn a_short_run_yields_a_grid_and_is_refined_when_it_fills() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(8)
            .detector_overlap_seconds(1)
            .detector_min_window_seconds(2)
            .build();
        let second = usize::try_from(Consts::SRC).unwrap_or(1);
        let pcm = stereo(12 * second, |_| 0.25);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_detector = Arc::clone(&seen);
        let mut detector = detector(move |mono| {
            seen_for_detector.lock().push(mono.len());
            RawBeats {
                beats: vec![BeatMark::at(0.5)],
                downbeats: vec![BeatMark::at(0.5)],
            }
        });
        let mut analyzer = analyzer(Consts::SRC, config);

        // Three seconds: past the minimum, far short of a window.
        analyzer.push_interleaved(&pcm[..3 * second * 2], 2, 0, true, &mut detector);
        let early = analyzer
            .snapshot(&mut detector, false)
            .expect("a short run still builds a grid");
        assert!(
            !early.beats().is_empty(),
            "one covered piece must already carry markers"
        );
        assert_eq!(
            seen.lock().len(),
            1,
            "the short run is detected once, not once per push"
        );

        // The rest arrives: the window fills and the estimate is replaced.
        analyzer.push_interleaved(
            &pcm[3 * second * 2..],
            2,
            u64::try_from(3 * second).unwrap_or(0),
            true,
            &mut detector,
        );
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
        let seen = seen.lock().clone();
        assert!(
            seen.contains(&(8 * second)),
            "a filled window must be re-detected at its full length, saw {seen:?}"
        );
    }

    #[kithara::test]
    fn finalize_builds_grid_in_source_frames() {
        // 9 downbeats every 2.0 s -> 120 bpm, positions converted at the
        // SOURCE rate (48 kHz here), not the detector's 22 050 Hz.
        let raw = RawBeats {
            beats: (0..33)
                .map(|n| {
                    let t: f32 = n.as_();
                    BeatMark::at(t * 0.5)
                })
                .collect(),
            downbeats: (0..9)
                .map(|n| {
                    let t: f32 = n.as_();
                    BeatMark::at(t * 2.0)
                })
                .collect(),
        };
        let mut analyzer = analyzer(48_000, BeatAnalysisConfig::<RubatoBackend>::default());
        let mut detector = detector(move |_| raw.clone());
        analyzer.push_interleaved(&stereo(17 * 48_000, |_| 0.1), 2, 0, true, &mut detector);
        let grid = analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");

        assert!(
            (grid.bpm() - 120.0).abs() < 1e-6,
            "2 s bars are 120 bpm, got {}",
            grid.bpm()
        );
        assert_eq!(grid.downbeats().len(), 9);
        assert_eq!(grid.downbeats()[1], 96_000, "downbeats are source frames");
        assert_eq!(grid.beats()[1], 24_000, "beats are source frames");
        assert!(
            grid.regions().is_empty(),
            "9 downbeats are below the stable window: tempo only"
        );
    }

    #[kithara::test]
    fn a_run_holds_what_waits_rather_than_the_track() {
        // Window 2 s, overlap 1 s: a 3 s window, a 12 s budget. Detection
        // keeps pace, so what waits is one window however long the track is.
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(2)
            .detector_overlap_seconds(1)
            .build();
        let second = usize::try_from(Consts::SRC).unwrap_or(1);
        let pcm = stereo(60 * second, |_| 0.25);
        let mut analyzer = analyzer(Consts::SRC, config);
        let mut detector = detector(|_| empty_raw());

        let mut worst = 0;
        for (index, chunk) in pcm.chunks(second * 2).enumerate() {
            let at = u64::try_from(index * second).unwrap_or(0);
            analyzer.push_interleaved_deferred(chunk, 2, at, true);
            while let Some(request) = analyzer.prepare_detection(false) {
                analyzer.apply_detection(request.detect(&mut detector));
            }
            worst = worst.max(analyzer.held_frames());
        }

        assert!(
            worst <= 6 * second,
            "one window is 3 s, yet the run held {worst} frames of {}",
            60 * second
        );
    }

    #[kithara::test]
    fn a_run_that_fed_no_window_keeps_what_it_holds() {
        // Window 2 s, no overlap. The first run opens half a second in and is
        // too short for any window; the second is a whole one. Releasing on
        // the second must leave the first alone: what it holds sits in front
        // of a window that starts before it, and the audio arriving there
        // later is the only thing that can complete it.
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(2)
            .detector_overlap_seconds(0)
            .build();
        let second = usize::try_from(Consts::SRC).unwrap_or(1);
        let mut analyzer = analyzer(Consts::SRC, config);
        let mut detector = detector(|_| empty_raw());

        let at = u64::try_from(second / 2).unwrap_or(0);
        analyzer.push_interleaved_deferred(&stereo(second, |_| 0.25), 2, at, true);
        let held = analyzer.held_frames();
        assert!(held > 0, "the short run holds what it was given");

        // One whole window on the grid, so releasing on it leaves nothing of
        // its own behind and the hold that remains is the short run's.
        let far = u64::try_from(10 * second).unwrap_or(0);
        analyzer.push_interleaved_deferred(&stereo(2 * second, |_| 0.25), 2, far, true);
        while let Some(request) = analyzer.prepare_detection(false) {
            analyzer.apply_detection(request.detect(&mut detector));
        }

        assert!(
            analyzer.held_frames() >= held,
            "the run that fed no window gave up {} of its {held} frames",
            held.saturating_sub(analyzer.held_frames())
        );
    }

    #[kithara::test]
    fn a_track_past_the_budget_is_taken_whole_by_waiting() {
        // A detector too slow to keep pace fills the hold. Offered the same
        // second again once room appears, the pass takes the whole track.
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(Consts::SRC)
            .detector_window_seconds(2)
            .detector_overlap_seconds(1)
            .build();
        let second = usize::try_from(Consts::SRC).unwrap_or(1);
        let seconds = 60;
        let pcm = stereo(seconds * second, |_| 0.25);
        let mut analyzer = analyzer(Consts::SRC, config);
        let mut detector = detector(|_| empty_raw());

        let mut taken = 0;
        let mut offers = 0;
        while taken < seconds {
            offers += 1;
            assert!(
                offers <= 4 * seconds,
                "the pass stalled after {taken} seconds"
            );
            let from = taken * second * 2;
            if let Some(block) = pcm.get(from..from + second * 2)
                && analyzer.push_interleaved_deferred(
                    block,
                    2,
                    u64::try_from(taken * second).unwrap_or(0),
                    true,
                )
            {
                taken += 1;
            }
            // One window per offer: a detector that lags is what fills the hold.
            if let Some(request) = analyzer.prepare_detection(false) {
                analyzer.apply_detection(request.detect(&mut detector));
            }
        }
        analyzer
            .snapshot(&mut detector, true)
            .expect("mock detects");
        let track = u64::try_from(seconds * second).unwrap_or(0);
        let lost: u64 = analyzer
            .unanalysed(Some(track))
            .iter()
            .map(|range| range.frames())
            .sum();
        assert_eq!(lost, 0, "{lost} frames were never taken");
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
        analyzer.push_interleaved(&stereo(4096, |_| 0.1), 2, 0, true, &mut detector);
        assert!(analyzer.snapshot(&mut detector, true).is_err());
    }

    #[kithara::test]
    fn shuffled_blocks_place_markers_where_ascending_does() {
        // One detector window per second, so a 6 s source yields several
        // windows and the shuffle actually reorders detected spans.
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(22_050)
            .detector_window_seconds(1)
            .detector_overlap_seconds(0)
            .build();
        // Short enough that the mono budget never reclaims a span before its
        // window completes: marker equality across arrival orders holds below
        // the budget, and the budget's own behaviour is asserted separately.
        let seconds = 3;
        let frames = seconds * usize::try_from(Consts::SRC).unwrap_or(1);
        let step = std::f32::consts::TAU * 220.0 / 44_100.0;
        let pcm = stereo(frames, |n| {
            let t: f32 = n.as_();
            0.5 * (step * t).sin()
        });

        // Each window reports one beat a quarter of the way in, so the marker
        // positions are a pure function of where the window sits.
        let beats = |_: &[f32]| RawBeats {
            beats: vec![BeatMark::at(0.25)],
            downbeats: vec![BeatMark::at(0.25)],
        };

        let block = usize::try_from(Consts::SRC).unwrap_or(1) * 2;
        let blocks: Vec<(u64, &[f32])> = pcm
            .chunks(block)
            .enumerate()
            .map(|(i, part)| (u64::try_from(i).unwrap_or(0) * 44_100, part))
            .collect();

        let run = |order: &[usize]| {
            let mut analyzer = analyzer(Consts::SRC, config.clone());
            let mut detector = detector(beats);
            for index in order {
                let Some((at, part)) = blocks.get(*index) else {
                    continue;
                };
                analyzer.push_interleaved(part, 2, *at, true, &mut detector);
            }
            analyzer
                .snapshot(&mut detector, true)
                .expect("mock detects")
                .downbeats()
                .to_vec()
        };

        let ascending = run(&[0, 1, 2]);
        let shuffled = run(&[2, 0, 1]);
        assert!(
            !ascending.is_empty(),
            "the ascending pass must find markers"
        );
        assert_eq!(
            ascending.len(),
            shuffled.len(),
            "shuffled ingestion must find the same markers"
        );
        for (want, got) in ascending.iter().zip(shuffled.iter()) {
            assert!(
                want.abs_diff(*got) <= 64,
                "marker must keep its absolute source frame: want {want}, got {got}"
            );
        }
    }
}
