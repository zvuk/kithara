use kithara_decode::{PcmChunk, PcmSpec};

use crate::waveform::Waveform;
#[cfg(feature = "analysis")]
use crate::waveform::{AnalysisParams, WaveformAnalyzer};

/// Streaming per-track analyzer: fed every decoded chunk once, folds its
/// result into the shared [`TrackAnalysis`] at end of stream.
pub trait TrackAnalyzer: Send {
    /// Consume one decoded chunk (interleaved PCM with meta).
    fn push(&mut self, chunk: &PcmChunk);
    /// Fold the finished stream into the aggregate output.
    fn finish(self: Box<Self>, out: &mut TrackAnalysis);
}

/// Aggregate output of one analysis pass; each registered analyzer fills
/// its own slot.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TrackAnalysis {
    /// Colored waveform, when a waveform analyzer is registered.
    pub waveform: Option<Waveform>,
}

/// Builds one analyzer at the source spec, once the first chunk arrives.
pub type AnalyzerFactory = Box<dyn Fn(PcmSpec) -> Box<dyn TrackAnalyzer> + Send + Sync>;

/// Set of analyzers to run per track: decode once, feed all of them.
#[derive(Default)]
pub struct AnalyzerRegistry {
    factories: Vec<AnalyzerFactory>,
}

impl AnalyzerRegistry {
    pub fn register(&mut self, factory: AnalyzerFactory) {
        self.factories.push(factory);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    pub(crate) fn build(&self, spec: PcmSpec) -> Vec<Box<dyn TrackAnalyzer>> {
        self.factories.iter().map(|f| f(spec)).collect()
    }
}

/// Waveform analyzer factory for [`AnalyzerRegistry::register`]: `buckets`
/// columns at the resolution the UI paints. `None` when the crate is built
/// without the `analysis` feature — the runtime check that lets callers
/// stay free of conditional compilation.
#[must_use]
pub fn waveform_analyzer(buckets: usize) -> Option<AnalyzerFactory> {
    #[cfg(feature = "analysis")]
    {
        Some(Box::new(move |spec: PcmSpec| {
            Box::new(WaveformSlot {
                inner: WaveformAnalyzer::new(spec.sample_rate, AnalysisParams::default()),
                buckets,
            }) as Box<dyn TrackAnalyzer>
        }))
    }
    #[cfg(not(feature = "analysis"))]
    {
        let _ = buckets;
        None
    }
}

#[cfg(feature = "analysis")]
struct WaveformSlot {
    inner: WaveformAnalyzer,
    buckets: usize,
}

#[cfg(feature = "analysis")]
impl TrackAnalyzer for WaveformSlot {
    fn push(&mut self, chunk: &PcmChunk) {
        let channels = usize::from(chunk.spec().channels.max(1));
        self.inner.push_interleaved(&chunk.pcm[..], channels);
    }

    fn finish(self: Box<Self>, out: &mut TrackAnalysis) {
        out.waveform = Some(self.inner.finalize(self.buckets));
    }
}
