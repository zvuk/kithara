use std::time::Duration;

use kithara::{
    audio::{ChunkOutcome, Envelope, PeakAccumulator},
    prelude::{Resource, ResourceConfig},
};
use kithara_platform::{thread::sleep, tokio::task::spawn_blocking};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Raw bins per final bucket; oversampling stabilises the shape on long
/// tracks where the accumulator max-merges.
const RAW_OVERSAMPLE: usize = 16;

/// Backoff while the reader is buffering and has no chunk ready.
const PENDING_BACKOFF: Duration = Duration::from_millis(5);

/// Decode `config` end to end off-thread into a peak-normalised envelope
/// of `buckets` values; `None` on open/decode failure or upfront cancel.
/// `cancel` must be a child of the app master.
pub async fn analyze(
    config: ResourceConfig,
    buckets: usize,
    cancel: CancellationToken,
) -> Option<Envelope> {
    if buckets == 0 || cancel.is_cancelled() {
        return None;
    }

    let mut resource = match Resource::new(config).await {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "waveform: resource open failed");
            return None;
        }
    };
    if let Err(e) = resource.preload().await {
        warn!(?e, "waveform: preload failed");
        return None;
    }

    let cap = buckets.saturating_mul(RAW_OVERSAMPLE);
    spawn_blocking(move || decode_envelope(resource, buckets, cap, &cancel))
        .await
        .ok()
        .flatten()
}

fn decode_envelope(
    mut resource: Resource,
    buckets: usize,
    cap: usize,
    cancel: &CancellationToken,
) -> Option<Envelope> {
    let mut acc = PeakAccumulator::new(cap);
    loop {
        if cancel.is_cancelled() {
            debug!("waveform: analysis cancelled");
            return None;
        }
        match resource.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let channels = usize::from(chunk.meta.spec.channels);
                acc.push_interleaved(&chunk.pcm[..], channels);
            }
            Ok(ChunkOutcome::Pending { .. }) => sleep(PENDING_BACKOFF),
            Ok(ChunkOutcome::Eof { .. }) => return Some(acc.finalize(buckets)),
            Err(e) => {
                warn!(?e, "waveform: decode error");
                return None;
            }
        }
    }
}
