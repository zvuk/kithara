use std::sync::Arc;

use kithara_decode::{PcmChunk, PcmSpec};
use num_traits::cast::AsPrimitive;

use super::{
    track_analysis::{Analyzer, TrackAnalysis},
    waveform_pass::WaveformPass,
};
use crate::analysis::beat::{BeatPass, GridParams, SharedBeatDetector};

/// Cache fingerprint of the active beat-analysis configuration (detector
/// kind, model, grid tuning); folding it into the analysis cache key makes a
/// config change a cache miss. `None` when no beat detector is compiled in.
#[must_use]
pub fn beat_cache_tag() -> Option<String> {
    super::nn::tag()
}

#[derive(Default)]
pub(crate) struct TrackAnalyzers {
    beat: Option<BeatPass>,
    waveform: Option<WaveformPass>,
    source_frames: u64,
}

impl TrackAnalyzers {
    pub(crate) fn push(&mut self, chunk: &PcmChunk) {
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self.source_frames.saturating_add(frames);

        if let Some(a) = &mut self.waveform {
            a.push(chunk);
        }

        if let Some(a) = &mut self.beat {
            a.push(chunk);
        }
    }

    /// Emit the fast waveform first, then waveform+beat.
    pub(crate) fn finish_staged<F: FnMut(TrackAnalysis)>(mut self, mut emit: F) {
        let source_frames = self.source_frames;
        let waveform = self.waveform.take().map(Analyzer::finish);

        if self.beat.is_none() {
            emit(TrackAnalysis {
                beat: None,
                source_frames,
                waveform,
            });
            return;
        }

        emit(TrackAnalysis {
            beat: None,
            source_frames,
            waveform: waveform.clone(),
        });

        let beat = self.beat.and_then(Analyzer::finish);
        emit(TrackAnalysis {
            beat,
            source_frames,
            waveform,
        });
    }
}

/// Configures which analyzers run, then mints a fresh [`TrackAnalyzers`]
/// per track at the source spec.
#[derive(Default)]
pub struct AnalyzerBuilder {
    beat: Option<(SharedBeatDetector, GridParams)>,
    waveform_buckets: Option<usize>,
}

impl AnalyzerBuilder {
    pub(crate) fn build(&self, spec: PcmSpec) -> TrackAnalyzers {
        TrackAnalyzers {
            beat: self.beat.as_ref().map(|(detector, params)| {
                BeatPass::new(spec.sample_rate.get(), params.clone(), Arc::clone(detector))
            }),
            waveform: self
                .waveform_buckets
                .map(|buckets| WaveformPass::new(spec.sample_rate.get(), buckets)),
            source_frames: 0,
        }
    }

    /// `true` when no analyzer would run — the runtime signal to skip the
    /// decode pass entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waveform_buckets.is_none() && self.beat.is_none()
    }

    /// Run the NN beat analyzer. No-op when no detector is compiled in or
    /// its model fails to load (`beat-nn`).
    #[must_use]
    pub fn with_beat(self) -> Self {
        #[cfg(feature = "beat-nn")]
        {
            Self {
                beat: super::nn::detector().map(|d| (d, GridParams::default())),
                ..self
            }
        }
        #[cfg(not(feature = "beat-nn"))]
        {
            self
        }
    }

    /// Test seam: install a mock detector without loading the NN model.
    #[cfg(test)]
    pub(crate) fn with_beat_detector(
        mut self,
        detector: SharedBeatDetector,
        params: GridParams,
    ) -> Self {
        self.beat = Some((detector, params));
        self
    }

    /// Run a waveform analyzer at `buckets` resolution.
    #[must_use]
    pub fn with_waveform(self, buckets: usize) -> Self {
        Self {
            waveform_buckets: Some(buckets),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::Arc};

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
    use kithara_platform::sync::Mutex;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::track_analysis::TrackAnalysis, AnalyzerBuilder, GridParams, TrackAnalyzers,
    };
    use crate::analysis::beat::{BeatDetector, BeatDetectorMock, RawBeats, SharedBeatDetector};

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
