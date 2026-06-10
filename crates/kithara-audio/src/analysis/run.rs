use std::time::Duration;

use kithara_platform::{CancellationToken, thread::sleep};
use tracing::{debug, warn};

use super::analyzer::{AnalyzerRegistry, TrackAnalysis, TrackAnalyzer};
use crate::traits::{ChunkOutcome, PcmReader};

/// Backoff while the reader is buffering and has no chunk ready.
const PENDING_BACKOFF: Duration = Duration::from_millis(5);

/// Decode `reader` to EOF and feed every registered analyzer: one decode,
/// many analyzers. Analyzers are built on the first chunk so they can take
/// the source spec. `None` on cancel, decode error, or empty input.
pub fn analyze_reader(
    reader: &mut dyn PcmReader,
    registry: &AnalyzerRegistry,
    cancel: &CancellationToken,
) -> Option<TrackAnalysis> {
    let mut analyzers: Option<Vec<Box<dyn TrackAnalyzer>>> = None;
    loop {
        if cancel.is_cancelled() {
            debug!("analysis cancelled");
            return None;
        }
        match reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let set = analyzers.get_or_insert_with(|| registry.build(chunk.spec()));
                for analyzer in set.iter_mut() {
                    analyzer.push(&chunk);
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => sleep(PENDING_BACKOFF),
            Ok(ChunkOutcome::Eof { .. }) => {
                let set = analyzers?;
                let mut out = TrackAnalysis::default();
                for analyzer in set {
                    analyzer.finish(&mut out);
                }
                return Some(out);
            }
            Err(e) => {
                warn!(?e, "analysis: decode error");
                return None;
            }
        }
    }
}

#[cfg(all(test, feature = "analysis"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::super::analyzer::waveform_analyzer;
    use super::super::fake::{FakeReader, SR, sine};
    use super::*;
    use crate::waveform::{AnalysisParams, WaveformAnalyzer};

    const BUCKETS: usize = 64;

    fn registry() -> AnalyzerRegistry {
        let mut registry = AnalyzerRegistry::default();
        registry.register(waveform_analyzer(BUCKETS).expect("analysis feature is on in tests"));
        registry
    }

    #[kithara::test]
    fn matches_direct_waveform_analyzer_over_chunked_stream() {
        let samples = sine(usize::try_from(SR).unwrap());
        let mut direct = WaveformAnalyzer::new(SR, AnalysisParams::default());
        direct.push_interleaved(&samples, 2);
        let want = direct.finalize(BUCKETS);

        let mut reader = FakeReader::chunked(&samples, 4);
        let out = analyze_reader(&mut reader, &registry(), &CancellationToken::default())
            .expect("stream with data analyses");
        let got = out.waveform.expect("waveform analyzer fills its slot");
        assert_eq!(
            got.to_bytes(),
            want.to_bytes(),
            "worker path must reproduce the direct analyzer output"
        );
    }

    #[kithara::test]
    fn cancelled_token_yields_none() {
        let cancel = CancellationToken::default();
        cancel.cancel();
        let mut reader = FakeReader::chunked(&sine(4096), 2);
        assert!(analyze_reader(&mut reader, &registry(), &cancel).is_none());
    }

    #[kithara::test]
    fn decode_error_yields_none() {
        let mut reader = FakeReader::failing();
        let out = analyze_reader(&mut reader, &registry(), &CancellationToken::default());
        assert!(out.is_none());
    }

    #[kithara::test]
    fn empty_stream_yields_none() {
        let mut reader = FakeReader::empty();
        let out = analyze_reader(&mut reader, &registry(), &CancellationToken::default());
        assert!(out.is_none(), "EOF with no chunks is not an analysis");
    }

    #[kithara::test]
    fn pending_is_tolerated_mid_stream() {
        let samples = sine(8192);
        let mut reader = FakeReader::chunked_with_pending(&samples, 2);
        let out = analyze_reader(&mut reader, &registry(), &CancellationToken::default());
        assert!(out.is_some_and(|a| a.waveform.is_some()));
    }
}
