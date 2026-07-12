use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;
use num_traits::cast::AsPrimitive;

use super::{config::BeatAnalysisConfig, track_analysis::TrackAnalysis};

#[derive(Default)]
pub(crate) struct TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    beat: beat::Slot<B>,
    waveform: waveform::Slot,
    source_frames: u64,
}

impl<B> TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
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
pub struct AnalyzerBuilder<B>
where
    B: ResamplerBackend,
{
    beat: beat::Config<B>,
    beat_config: Option<BeatAnalysisConfig<B>>,
    pcm_pool: PcmPool,
    waveform: waveform::Config,
}

impl<B> AnalyzerBuilder<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers<B> {
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
    pub fn with_beat(self) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        beat::with_default(&mut builder.beat, beat_config.clone());
        builder.beat_config = Some(beat_config);
        builder
    }

    #[must_use]
    pub fn with_beat_config(self, config: BeatAnalysisConfig<B>) -> Self {
        let mut builder = self;
        builder.beat_config = Some(config.clone());
        beat::set_resampler(&mut builder.beat, config);
        builder
    }

    #[cfg(all(test, feature = "analysis-beat"))]
    pub(crate) fn with_beat_detector(
        self,
        detector: crate::analysis::beat::SharedBeatDetector,
        params: crate::analysis::beat::GridParams,
    ) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        beat::with_detector(&mut builder.beat, detector, params, beat_config.clone());
        builder.beat_config = Some(beat_config);
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
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmSpec};
    use kithara_platform::sync::Arc;
    use kithara_resampler::ResamplerBackend;

    use super::super::{nn, track_analysis::Analyzer};
    use crate::{
        analysis::{
            analyzer::BeatAnalysisConfig,
            beat::{BeatPass, GridParams, SharedBeatDetector},
        },
        waveform::BeatGrid,
    };

    pub(super) struct BeatConfig<B>
    where
        B: ResamplerBackend,
    {
        detector: SharedBeatDetector,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
    }

    pub(super) type Config<B> = Option<BeatConfig<B>>;
    pub(super) type Slot<B> = Option<BeatPass<B>>;

    pub(super) fn build<B>(config: &Config<B>, spec: PcmSpec, pcm_pool: &PcmPool) -> Slot<B>
    where
        B: ResamplerBackend,
    {
        config.as_ref().map(|config| {
            BeatPass::new(
                spec.sample_rate.get(),
                config.params.clone(),
                Arc::clone(&config.detector),
                &config.resampler,
                pcm_pool,
            )
        })
    }

    pub(super) fn config_is_empty<B>(config: &Config<B>) -> bool
    where
        B: ResamplerBackend,
    {
        config.is_none()
    }

    pub(super) fn finish<B>(slot: Slot<B>) -> Option<BeatGrid>
    where
        B: ResamplerBackend,
    {
        slot.and_then(Analyzer::finish)
    }

    pub(super) fn is_empty<B>(slot: &Slot<B>) -> bool
    where
        B: ResamplerBackend,
    {
        slot.is_none()
    }

    pub(super) fn push<B>(slot: &mut Slot<B>, chunk: &PcmChunk)
    where
        B: ResamplerBackend,
    {
        if let Some(a) = slot {
            a.push(chunk);
        }
    }

    pub(super) fn with_default<B>(config: &mut Config<B>, resampler: BeatAnalysisConfig<B>)
    where
        B: ResamplerBackend,
    {
        *config = nn::detector().map(|detector| BeatConfig {
            detector,
            params: GridParams::default(),
            resampler,
        });
    }

    pub(super) fn set_resampler<B>(config: &mut Config<B>, resampler: BeatAnalysisConfig<B>)
    where
        B: ResamplerBackend,
    {
        if let Some(config) = config {
            config.resampler = resampler;
        }
    }

    #[cfg(test)]
    pub(super) fn with_detector<B>(
        config: &mut Config<B>,
        detector: SharedBeatDetector,
        params: GridParams,
        resampler: BeatAnalysisConfig<B>,
    ) where
        B: ResamplerBackend,
    {
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
    use kithara_resampler::ResamplerBackend;

    use crate::waveform::BeatGrid;

    #[derive(Default)]
    pub(super) struct Config<B>(std::marker::PhantomData<B>);

    #[derive(Default)]
    pub(super) struct Slot<B>(std::marker::PhantomData<B>);

    pub(super) fn build<B>(_config: &Config<B>, _spec: PcmSpec, _pcm_pool: &PcmPool) -> Slot<B>
    where
        B: ResamplerBackend,
    {
        Slot(std::marker::PhantomData)
    }

    pub(super) fn config_is_empty<B>(_config: &Config<B>) -> bool
    where
        B: ResamplerBackend,
    {
        true
    }

    pub(super) fn finish<B>(_slot: Slot<B>) -> Option<BeatGrid>
    where
        B: ResamplerBackend,
    {
        None
    }

    pub(super) fn is_empty<B>(_slot: &Slot<B>) -> bool
    where
        B: ResamplerBackend,
    {
        true
    }

    pub(super) fn push<B>(_slot: &mut Slot<B>, _chunk: &PcmChunk)
    where
        B: ResamplerBackend,
    {
    }

    pub(super) fn with_default<B>(_config: &mut Config<B>, _resampler: super::BeatAnalysisConfig<B>)
    where
        B: ResamplerBackend,
    {
    }

    pub(super) fn set_resampler<B>(
        _config: &mut Config<B>,
        _resampler: super::BeatAnalysisConfig<B>,
    ) where
        B: ResamplerBackend,
    {
    }
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
    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
    use kithara_platform::sync::{Arc, Mutex};
    use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
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

    fn collect<B: kithara_resampler::ResamplerBackend>(
        analyzers: TrackAnalyzers<B>,
    ) -> Vec<TrackAnalysis> {
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
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(spec);
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
        let mut analyzers = AnalyzerBuilder::<RubatoBackend>::default()
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
