use kithara_platform::{CancelToken, thread::paced_backoff, time::Duration};
use kithara_resampler::ResamplerBackend;
use tracing::{debug, warn};

use super::analyzer::{AnalyzerBuilder, TrackAnalysis, TrackAnalyzers};
use crate::traits::{ChunkOutcome, PcmReader};

/// Backoff while the reader is buffering and has no chunk ready. The decode
/// loop runs on the engine-visible `kithara-analysis` thread, so the wait is
/// paced by the producer (the audio worker fed by the real download), not a
/// free virtual timer; see [`paced_backoff`].
const PENDING_BACKOFF: Duration = Duration::from_millis(5);

/// Decode `reader` to EOF feeding the analyzer set, then finalize in stages
/// to `emit` (waveform first, then waveform+beat). Nothing is emitted on
/// cancel, decode error, or empty input.
pub fn analyze_reader<B, F: FnMut(TrackAnalysis)>(
    reader: &mut dyn PcmReader,
    builder: &AnalyzerBuilder<B>,
    cancel: &CancelToken,
    emit: F,
) where
    B: ResamplerBackend,
{
    let mut analyzers: Option<TrackAnalyzers<B>> = None;
    loop {
        if cancel.is_cancelled() {
            debug!("analysis cancelled");
            return;
        }
        match reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                analyzers
                    .get_or_insert_with(|| builder.build(chunk.spec()))
                    .push(&chunk);
            }
            Ok(ChunkOutcome::Pending { .. }) => paced_backoff(PENDING_BACKOFF),
            Ok(ChunkOutcome::Eof { .. }) => {
                let Some(analyzers) = analyzers else {
                    return;
                };
                // Finalize can be expensive: honor a cancel that raced the last chunk.
                if cancel.is_cancelled() {
                    debug!("analysis cancelled before finalize");
                    return;
                }
                analyzers.finish_staged(emit);
                return;
            }
            Err(e) => {
                warn!(?e, "analysis: decode error");
                return;
            }
        }
    }
}
