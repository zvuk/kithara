use bon::Builder;
use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec, ResamplerQuality};
use num_traits::cast::AsPrimitive;

use super::track_analysis::TrackAnalysis;

struct Consts;

impl Consts {
    const DEFAULT_BEAT_BLOCK_FRAMES: usize = 1024;
    const DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS: u32 = 2;
    const DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS: u32 = 30;
    const DEFAULT_BEAT_RESAMPLER_QUALITY: ResamplerQuality = ResamplerQuality::High;
    const DEFAULT_BEAT_TARGET_RATE: u32 = 22_050;
}

/// Beat-analysis tunables used by [`AnalyzerBuilder`].
#[derive(Clone, Copy, Debug, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct BeatAnalysisConfig {
    /// Mono resampler input block size in frames.
    #[builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES)]
    pub block_frames: usize,
    /// Detector input sample rate in Hz.
    #[builder(default = Consts::DEFAULT_BEAT_TARGET_RATE)]
    pub target_rate: u32,
    /// Quality used by the rubato sinc beat-resampler backend.
    #[builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY)]
    pub resampler_quality: ResamplerQuality,
    /// Maximum NN detector window length in seconds.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS)]
    pub detector_window_seconds: u32,
    /// Seconds carried from the end of one detector window into the next.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS)]
    pub detector_overlap_seconds: u32,
}

impl BeatAnalysisConfig {
    #[must_use]
    pub fn cache_tag(self) -> Option<String> {
        super::nn::tag(self)
    }
}

impl Default for BeatAnalysisConfig {
    fn default() -> Self {
        Self {
            block_frames: Consts::DEFAULT_BEAT_BLOCK_FRAMES,
            target_rate: Consts::DEFAULT_BEAT_TARGET_RATE,
            resampler_quality: Consts::DEFAULT_BEAT_RESAMPLER_QUALITY,
            detector_window_seconds: Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS,
            detector_overlap_seconds: Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS,
        }
    }
}

#[derive(Default)]
pub(crate) struct TrackAnalyzers {
    beat: beat::Slot,
    waveform: waveform::Slot,
    source_frames: u64,
}

impl TrackAnalyzers {
    pub(crate) fn finish_staged<F: FnMut(TrackAnalysis)>(self, mut emit: F) {
        let source_frames = self.source_frames;
        let waveform = waveform::finish(self.waveform);

        if beat::is_empty(&self.beat) {
            emit(TrackAnalysis {
                waveform,
                source_frames,
                beat: None,
            });
            return;
        }

        emit(TrackAnalysis {
            source_frames,
            beat: None,
            waveform: waveform.clone(),
        });

        emit(TrackAnalysis {
            beat: beat::finish(self.beat),
            waveform,
            source_frames,
        });
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk) {
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self.source_frames.saturating_add(frames);

        waveform::push(&mut self.waveform, chunk);
        beat::push(&mut self.beat, chunk);
    }
}

#[derive(Default)]
pub struct AnalyzerBuilder {
    beat: beat::Config,
    beat_config: BeatAnalysisConfig,
    pcm_pool: PcmPool,
    waveform: waveform::Config,
}

impl AnalyzerBuilder {
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers {
        TrackAnalyzers {
            beat: beat::build(&self.beat, spec, &self.pcm_pool),
            waveform: waveform::build(&self.waveform, spec),
            source_frames: 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        waveform::config_is_empty(&self.waveform) && beat::config_is_empty(&self.beat)
    }

    #[must_use]
    pub fn with_beat(self) -> Self {
        let mut builder = self;
        beat::with_default(&mut builder.beat, builder.beat_config);
        builder
    }

    #[must_use]
    pub fn with_beat_config(self, config: BeatAnalysisConfig) -> Self {
        let mut builder = self;
        builder.beat_config = config;
        beat::set_resampler(&mut builder.beat, config);
        builder
    }

    #[cfg(all(test, feature = "analysis-beat"))]
    pub(crate) fn with_beat_detector(
        self,
        detector: crate::analysis::beat::SharedBeatDetector,
        params: crate::analysis::beat::GridParams,
    ) -> Self {
        let mut builder = self;
        beat::with_detector(&mut builder.beat, detector, params, builder.beat_config);
        builder
    }

    #[must_use]
    pub fn with_pcm_pool(self, pool: PcmPool) -> Self {
        Self {
            pcm_pool: pool,
            ..self
        }
    }

    #[must_use]
    #[cfg(feature = "analysis-waveform")]
    pub fn with_waveform(self, buckets: usize) -> Self {
        let mut builder = self;
        waveform::with_buckets(&mut builder.waveform, buckets);
        builder
    }
}

#[cfg(feature = "analysis-beat")]
mod beat {
    use std::sync::Arc;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmSpec};

    use super::super::{nn, track_analysis::Analyzer};
    use crate::{
        analysis::{
            analyzer::BeatAnalysisConfig,
            beat::{BeatPass, GridParams, SharedBeatDetector},
        },
        waveform::BeatGrid,
    };

    pub(super) struct BeatConfig {
        detector: SharedBeatDetector,
        params: GridParams,
        resampler: BeatAnalysisConfig,
    }

    pub(super) type Config = Option<BeatConfig>;
    pub(super) type Slot = Option<BeatPass>;

    pub(super) fn build(config: &Config, spec: PcmSpec, pcm_pool: &PcmPool) -> Slot {
        config.as_ref().map(|config| {
            BeatPass::new(
                spec.sample_rate.get(),
                config.params.clone(),
                Arc::clone(&config.detector),
                config.resampler,
                pcm_pool,
            )
        })
    }

    pub(super) fn config_is_empty(config: &Config) -> bool {
        config.is_none()
    }

    pub(super) fn finish(slot: Slot) -> Option<BeatGrid> {
        slot.and_then(Analyzer::finish)
    }

    pub(super) fn is_empty(slot: &Slot) -> bool {
        slot.is_none()
    }

    pub(super) fn push(slot: &mut Slot, chunk: &PcmChunk) {
        if let Some(a) = slot {
            a.push(chunk);
        }
    }

    pub(super) fn with_default(config: &mut Config, resampler: BeatAnalysisConfig) {
        *config = nn::detector().map(|detector| BeatConfig {
            detector,
            params: GridParams::default(),
            resampler,
        });
    }

    pub(super) fn set_resampler(config: &mut Config, resampler: BeatAnalysisConfig) {
        if let Some(config) = config {
            config.resampler = resampler;
        }
    }

    #[cfg(test)]
    pub(super) fn with_detector(
        config: &mut Config,
        detector: SharedBeatDetector,
        params: GridParams,
        resampler: BeatAnalysisConfig,
    ) {
        *config = Some(BeatConfig {
            detector,
            params,
            resampler,
        });
    }
}

#[cfg(not(feature = "analysis-beat"))]
mod beat {
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmSpec};

    use crate::waveform::BeatGrid;

    #[derive(Default)]
    pub(super) struct Config;

    #[derive(Default)]
    pub(super) struct Slot;

    pub(super) fn build(_config: &Config, _spec: PcmSpec, _pcm_pool: &PcmPool) -> Slot {
        Slot
    }

    pub(super) fn config_is_empty(_config: &Config) -> bool {
        true
    }

    pub(super) fn finish(_slot: Slot) -> Option<BeatGrid> {
        None
    }

    pub(super) fn is_empty(_slot: &Slot) -> bool {
        true
    }

    pub(super) fn push(_slot: &mut Slot, _chunk: &PcmChunk) {}

    pub(super) fn with_default(_config: &mut Config, _resampler: super::BeatAnalysisConfig) {}

    pub(super) fn set_resampler(_config: &mut Config, _resampler: super::BeatAnalysisConfig) {}
}

#[cfg(feature = "analysis-waveform")]
mod waveform {
    use kithara_decode::{PcmChunk, PcmSpec};

    use super::super::{WaveformPass, track_analysis::Analyzer};
    use crate::waveform::Waveform;

    pub(super) type Config = Option<usize>;
    pub(super) type Slot = Option<WaveformPass>;

    pub(super) fn build(config: &Config, spec: PcmSpec) -> Slot {
        config
            .as_ref()
            .map(|buckets| WaveformPass::new(spec.sample_rate.get(), *buckets))
    }

    pub(super) fn config_is_empty(config: &Config) -> bool {
        config.is_none()
    }

    pub(super) fn finish(slot: Slot) -> Option<Waveform> {
        slot.map(Analyzer::finish)
    }

    pub(super) fn push(slot: &mut Slot, chunk: &PcmChunk) {
        if let Some(analyzer) = slot {
            analyzer.push(chunk);
        }
    }

    pub(super) fn with_buckets(config: &mut Config, buckets: usize) {
        *config = Some(buckets);
    }
}

#[cfg(not(feature = "analysis-waveform"))]
mod waveform {
    use kithara_decode::{PcmChunk, PcmSpec};

    use crate::waveform::Waveform;

    #[derive(Default)]
    pub(super) struct Config;

    #[derive(Default)]
    pub(super) struct Slot;

    pub(super) fn build(_config: &Config, _spec: PcmSpec) -> Slot {
        Slot
    }

    pub(super) fn config_is_empty(_config: &Config) -> bool {
        true
    }

    pub(super) fn finish(_slot: Slot) -> Option<Waveform> {
        None
    }

    pub(super) fn push(_slot: &mut Slot, _chunk: &PcmChunk) {}
}

#[cfg(all(test, feature = "analysis-beat", feature = "analysis-waveform"))]
mod tests {
    use std::{num::NonZeroU32, sync::Arc};

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
    use kithara_platform::sync::Mutex;
    use unimock::{MockFn, Unimock, matching};

    use super::{super::track_analysis::TrackAnalysis, AnalyzerBuilder, TrackAnalyzers};
    use crate::analysis::beat::{
        BeatDetector, BeatDetectorMock, GridParams, RawBeats, SharedBeatDetector,
    };

    fn chunk(frames: usize, channels: u16) -> PcmChunk {
        let samples = vec![0.0_f32; frames * usize::from(channels)];
        PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels,
                    sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
                },
                frames: u32::try_from(frames).unwrap_or(0),
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    fn collect(analyzers: TrackAnalyzers) -> Vec<TrackAnalysis> {
        let mut out = Vec::new();
        analyzers.finish_staged(|a| out.push(a));
        out
    }

    fn beat_detector() -> SharedBeatDetector {
        let raw = RawBeats {
            beats: Vec::new(),
            downbeats: (0..9u8).map(|n| f32::from(n) * 2.0).collect(),
        };
        let mock = Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
        );
        Arc::new(Mutex::new(Box::new(mock) as Box<dyn BeatDetector>))
    }

    #[test]
    fn finish_staged_emits_once_without_a_beat_pass() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        };
        let mut analyzers = AnalyzerBuilder::default().with_waveform(8).build(spec);
        analyzers.push(&chunk(64, 2));

        let stages = collect(analyzers);
        assert_eq!(stages.len(), 1, "waveform-only emits exactly once");
        assert!(stages[0].waveform.is_some());
        assert!(stages[0].beat.is_none());
    }

    #[test]
    fn finish_staged_emits_waveform_then_waveform_plus_beat() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        };
        let mut analyzers = AnalyzerBuilder::default()
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default())
            .build(spec);
        analyzers.push(&chunk(8192, 2));

        let stages = collect(analyzers);
        assert_eq!(
            stages.len(),
            2,
            "beat pass emits a fast stage then a complete one"
        );

        let first = &stages[0];
        assert!(first.waveform.is_some(), "stage 1 carries the waveform");
        assert!(first.beat.is_none(), "stage 1 has no beat yet");

        let second = &stages[1];
        assert!(second.beat.is_some(), "stage 2 carries the beat grid");
        assert_eq!(
            first.waveform.as_ref().map(Vec::<u8>::from),
            second.waveform.as_ref().map(Vec::<u8>::from),
            "both stages share the same waveform"
        );
    }
}
