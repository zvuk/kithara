use kithara_platform::{CancelToken, thread::paced_backoff, time::Duration};
use tracing::{debug, warn};

use super::analyzer::{AnalyzerBuilder, TrackAnalysis, TrackAnalyzers};
use crate::traits::{ChunkOutcome, PcmReader};

/// Backoff while the reader is buffering and has no chunk ready. The decode
/// loop runs on the engine-visible `kithara-analysis` thread, so the wait is
/// paced by the producer (the audio worker fed by the real download), not a
/// free virtual timer — see [`paced_backoff`].
const PENDING_BACKOFF: Duration = Duration::from_millis(5);

/// Decode `reader` to EOF and feed the analyzer set: one decode, many
/// analyzers. The set is built on the first chunk so it can take the source
/// spec. `None` on cancel, decode error, or empty input.
pub fn analyze_reader(
    reader: &mut dyn PcmReader,
    builder: &AnalyzerBuilder,
    cancel: &CancelToken,
) -> Option<TrackAnalysis> {
    let mut analyzers: Option<TrackAnalyzers> = None;
    loop {
        if cancel.is_cancelled() {
            debug!("analysis cancelled");
            return None;
        }
        match reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                analyzers
                    .get_or_insert_with(|| builder.build(chunk.spec()))
                    .push(&chunk);
            }
            Ok(ChunkOutcome::Pending { .. }) => paced_backoff(PENDING_BACKOFF),
            Ok(ChunkOutcome::Eof { .. }) => {
                let analyzers = analyzers?;
                // Finalize can be expensive: honor a cancel that raced the last chunk before
                if cancel.is_cancelled() {
                    debug!("analysis cancelled before finalize");
                    return None;
                }
                return Some(analyzers.finish());
            }
            Err(e) => {
                warn!(?e, "analysis: decode error");
                return None;
            }
        }
    }
}
