use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;
use kithara_resampler::ResamplerBackend;

use super::{config::BeatAnalysisConfig, session::TrackAnalyzers};
use crate::analysis::slots::{beat, waveform};

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
            source_spec: spec,
        }
    }

    pub(crate) fn take_detector(&mut self) -> Option<beat::Detector> {
        self.beat.take_detector()
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
        detector: Box<dyn crate::analysis::beat::BeatDetector>,
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
    use kithara_platform::sync::Arc;
    use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::{
            session::{AnalysisInputError, TrackAnalyzers},
            track::TrackAnalysis,
        },
        AnalyzerBuilder,
    };
    use crate::analysis::beat::{BeatDetector, BeatDetectorMock, GridParams, RawBeats};

    fn chunk(frames: usize, channels: u16) -> PcmChunk {
        chunk_at_rate(frames, channels, 44_100)
    }

    fn chunk_at_rate(frames: usize, channels: u16, sample_rate: u32) -> PcmChunk {
        let samples = vec![0.0_f32; frames * usize::from(channels)];
        PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels,
                    sample_rate: NonZeroU32::new(sample_rate)
                        .expect("test sample rate is non-zero"),
                },
                frames: u32::try_from(frames).unwrap_or(0),
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    fn collect<B: kithara_resampler::ResamplerBackend>(
        mut analyzers: TrackAnalyzers<B>,
        mut detector: Option<crate::analysis::slots::beat::Detector>,
    ) -> Vec<TrackAnalysis> {
        let source_frames = analyzers.source_frames();
        let source_sample_rate = analyzers.source_sample_rate();
        let waveform = analyzers.finish_waveform();
        let mut out = vec![TrackAnalysis::with_source_rate(
            None,
            waveform.clone(),
            source_frames,
            source_sample_rate,
        )];
        if analyzers.has_beat() {
            out.push(TrackAnalysis::with_source_rate(
                analyzers.finish_beat(detector.as_mut()),
                waveform,
                source_frames,
                source_sample_rate,
            ));
        }
        out
    }

    fn beat_detector() -> Box<dyn BeatDetector> {
        let raw = RawBeats {
            beats: Vec::new(),
            downbeats: (0..9u8).map(|n| f32::from(n) * 2.0).collect(),
        };
        let mock = Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
        );
        Box::new(mock)
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
        analyzers
            .push(&chunk(64, 2), None)
            .expect("stable input format");

        let stages = collect(analyzers, None);
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
        let mut builder = AnalyzerBuilder::<RubatoBackend>::default()
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder.build(spec);
        analyzers
            .push(&chunk(8192, 2), detector.as_mut())
            .expect("stable input format");

        let stages = collect(analyzers, detector);
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

    #[test]
    fn analysis_rejects_mid_pass_format_change() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        };
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(spec);

        let error = analyzers
            .push(&chunk_at_rate(64, 2, 48_000), None)
            .expect_err("one analysis pass has one source format");

        assert!(matches!(
            error,
            AnalysisInputError::FormatChanged { expected, actual }
                if expected == spec && actual.sample_rate.get() == 48_000
        ));
        assert_eq!(analyzers.source_frames(), 0);
    }

    #[test]
    fn analysis_rejects_source_frame_count_overflow() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        };
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(spec);
        analyzers.source_frames = u64::MAX;

        let error = analyzers
            .push(&chunk(1, 2), None)
            .expect_err("source extent must not saturate");

        assert_eq!(error, AnalysisInputError::SourceFrameCountOverflow);
        assert_eq!(analyzers.source_frames(), u64::MAX);
    }
}
