use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;
use num_traits::cast::AsPrimitive;

use super::{config::BeatAnalysisConfig, track::TrackAnalysis};
use crate::analysis::slots::{beat, waveform};

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

        if self.beat.is_empty() {
            emit(TrackAnalysis::new(None, waveform, source_frames));
            return;
        }

        emit(TrackAnalysis::new(None, waveform.clone(), source_frames));

        emit(TrackAnalysis::new(
            self.beat.finish(),
            waveform,
            source_frames,
        ));
    }

    pub(crate) fn push(&mut self, chunk: &PcmChunk) {
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self.source_frames.saturating_add(frames);

        waveform::push(&mut self.waveform, chunk);
        self.beat.push(chunk);
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
            beat: self.beat.build(spec, &self.pcm_pool),
            waveform: waveform::build(&self.waveform, spec),
            source_frames: 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        waveform::config_is_empty(&self.waveform) && self.beat.is_empty()
    }

    #[must_use]
    pub fn with_beat(self) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        builder.beat.with_default(beat_config.clone());
        builder.beat_config = Some(beat_config);
        builder
    }

    #[must_use]
    pub fn with_beat_config(self, config: BeatAnalysisConfig<B>) -> Self {
        let mut builder = self;
        builder.beat_config = Some(config.clone());
        builder.beat.set_resampler(config);
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
        builder
            .beat
            .with_detector(detector, params, beat_config.clone());
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

#[cfg(all(test, feature = "analysis-beat", feature = "analysis-waveform"))]
mod tests {
    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
    use kithara_platform::sync::{Arc, Mutex};
    use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
    use unimock::{MockFn, Unimock, matching};

    use super::{super::track::TrackAnalysis, AnalyzerBuilder, TrackAnalyzers};
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
        assert!(stages[0].waveform().is_some());
        assert!(stages[0].beat().is_none());
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
        assert!(first.waveform().is_some(), "stage 1 carries the waveform");
        assert!(first.beat().is_none(), "stage 1 has no beat yet");

        let second = &stages[1];
        assert!(second.beat().is_some(), "stage 2 carries the beat grid");
        assert_eq!(
            first.waveform().map(Vec::<u8>::from),
            second.waveform().map(Vec::<u8>::from),
            "both stages share the same waveform"
        );
    }
}
