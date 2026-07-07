use kithara_decode::{PcmChunk, PcmSpec};
use num_traits::cast::AsPrimitive;

use super::{
    WaveformPass,
    track_analysis::{Analyzer, TrackAnalysis},
};

#[must_use]
pub fn beat_cache_tag() -> Option<String> {
    super::nn::tag()
}

#[derive(Default)]
pub(crate) struct TrackAnalyzers {
    beat: beat::Slot,
    waveform: Option<WaveformPass>,
    source_frames: u64,
}

impl TrackAnalyzers {
    pub(crate) fn finish_staged<F: FnMut(TrackAnalysis)>(mut self, mut emit: F) {
        let source_frames = self.source_frames;
        let waveform = self.waveform.take().map(Analyzer::finish);

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

        if let Some(a) = &mut self.waveform {
            a.push(chunk);
        }

        beat::push(&mut self.beat, chunk);
    }
}

#[derive(Default)]
pub struct AnalyzerBuilder {
    beat: beat::Config,
    waveform_buckets: Option<usize>,
}

impl AnalyzerBuilder {
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers {
        TrackAnalyzers {
            beat: beat::build(&self.beat, spec),
            waveform: self
                .waveform_buckets
                .map(|buckets| WaveformPass::new(spec.sample_rate.get(), buckets)),
            source_frames: 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waveform_buckets.is_none() && beat::config_is_empty(&self.beat)
    }

    #[must_use]
    pub fn with_beat(self) -> Self {
        let mut builder = self;
        beat::with_default(&mut builder.beat);
        builder
    }

    #[cfg(all(test, feature = "analysis-beat"))]
    pub(crate) fn with_beat_detector(
        self,
        detector: crate::analysis::beat::SharedBeatDetector,
        params: crate::analysis::beat::GridParams,
    ) -> Self {
        let mut builder = self;
        beat::with_detector(&mut builder.beat, detector, params);
        builder
    }

    #[must_use]
    #[cfg(feature = "analysis-waveform")]
    pub fn with_waveform(self, buckets: usize) -> Self {
        Self {
            waveform_buckets: Some(buckets),
            ..self
        }
    }

    #[must_use]
    #[cfg(not(feature = "analysis-waveform"))]
    pub fn with_waveform(self, _buckets: usize) -> Self {
        self
    }
}

#[cfg(feature = "analysis-beat")]
mod beat {
    use std::sync::Arc;

    use kithara_decode::{PcmChunk, PcmSpec};

    use super::{super::nn, Analyzer};
    use crate::{
        analysis::beat::{BeatPass, GridParams, SharedBeatDetector},
        waveform::BeatGrid,
    };

    pub(super) type Config = Option<(SharedBeatDetector, GridParams)>;
    pub(super) type Slot = Option<BeatPass>;

    pub(super) fn build(config: &Config, spec: PcmSpec) -> Slot {
        config.as_ref().map(|(detector, params)| {
            BeatPass::new(spec.sample_rate.get(), params.clone(), Arc::clone(detector))
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

    pub(super) fn with_default(config: &mut Config) {
        *config = nn::detector().map(|d| (d, GridParams::default()));
    }

    #[cfg(test)]
    pub(super) fn with_detector(
        config: &mut Config,
        detector: SharedBeatDetector,
        params: GridParams,
    ) {
        *config = Some((detector, params));
    }
}

#[cfg(not(feature = "analysis-beat"))]
mod beat {
    use kithara_decode::{PcmChunk, PcmSpec};

    use crate::waveform::BeatGrid;

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

    pub(super) fn finish(_slot: Slot) -> Option<BeatGrid> {
        None
    }

    pub(super) fn is_empty(_slot: &Slot) -> bool {
        true
    }

    pub(super) fn push(_slot: &mut Slot, _chunk: &PcmChunk) {}

    pub(super) fn with_default(_config: &mut Config) {}
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
