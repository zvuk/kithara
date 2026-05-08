//! HLS plan → fetch orchestration.
//!
//! Extracted from `scheduler/worker.rs` so the fetch pipeline lives in
//! one place independent of how it is driven. Future work that needs
//! to build `FetchCmd`s from `HlsPlan`s from outside the worker loop
//! (e.g. a `Stream<Item = FetchCmd>` adapter) can reuse the same
//! [`fetch_plan`] helper without pulling in worker/hang-detector state.

use std::{sync::Arc, time::Duration};

use kithara_platform::{time::Instant, tokio};

use super::{HlsFetch, HlsPlan};
use crate::{HlsError, loading::SegmentLoader};

/// Fetch a single HLS plan — replaces the deleted `HlsIo::fetch`.
///
/// Kicks off the init-segment load (if requested) in parallel with the
/// media-segment load via `tokio::join!`, measures download duration,
/// and packages the result into an [`HlsFetch`].
pub(super) async fn fetch_plan(
    loader: &Arc<SegmentLoader>,
    plan: HlsPlan,
) -> Result<HlsFetch, HlsError> {
    let start = Instant::now();

    let init_fut = {
        let loader = Arc::clone(loader);
        let variant = plan.variant;
        let need_init = plan.need_init;
        async move {
            if need_init {
                match loader.load_init_segment(variant).await {
                    Ok(m) => (Some(m.url), m.len),
                    Err(e) => {
                        tracing::warn!(
                            variant,
                            error = %e,
                            "init segment load failed"
                        );
                        (None, 0)
                    }
                }
            } else {
                (None, 0)
            }
        }
    };

    let seg_idx = plan
        .segment
        .media_index()
        .expect("fetch_plan called with non-Media segment");

    let (media_result, (init_url, init_len)) = tokio::join!(
        loader.load_media_segment_with_source_for_epoch(plan.variant, seg_idx, plan.seek_epoch),
        init_fut,
    );

    let duration: Duration = start.elapsed();
    let (media, media_cached) = media_result?;

    Ok(HlsFetch {
        init_len,
        init_url,
        media,
        media_cached,
        duration,
        segment: plan.segment,
        variant: plan.variant,
        seek_epoch: plan.seek_epoch,
    })
}
