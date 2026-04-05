use std::{sync::Arc, time::Duration};

use kithara_events::SeekEpoch;
use kithara_platform::{BoxFuture, time::Instant, tokio};
use kithara_stream::DownloaderIo;
use url::Url;

use crate::{
    HlsError,
    fetch::{DefaultFetchManager, SegmentMeta},
    ids::{SegmentId, VariantIndex},
};

/// Pure I/O executor for HLS segment fetching.
#[derive(Clone)]
pub(crate) struct HlsIo {
    pub(crate) fetch: Arc<DefaultFetchManager>,
}

impl HlsIo {
    pub(crate) fn new(fetch: Arc<DefaultFetchManager>) -> Self {
        Self { fetch }
    }
}

/// Plan for downloading a single HLS segment.
pub(crate) struct HlsPlan {
    pub(crate) variant: VariantIndex,
    pub(crate) segment: SegmentId,
    pub(crate) need_init: bool,
    pub(crate) seek_epoch: SeekEpoch,
}

/// Result of downloading a single HLS segment.
pub(crate) struct HlsFetch {
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<Url>,
    pub(crate) media: SegmentMeta,
    pub(crate) media_cached: bool,
    pub(crate) segment: SegmentId,
    pub(crate) variant: VariantIndex,
    pub(crate) duration: Duration,
    pub(crate) seek_epoch: SeekEpoch,
}

impl DownloaderIo for HlsIo {
    type Plan = HlsPlan;
    type Fetch = HlsFetch;
    type Error = HlsError;

    fn fetch(&self, plan: HlsPlan) -> BoxFuture<'_, Result<HlsFetch, HlsError>> {
        Box::pin(async move {
            let start = Instant::now();

            let init_fut = {
                let fetch = Arc::clone(&self.fetch);
                async move {
                    if plan.need_init {
                        match fetch.load_init_segment(plan.variant).await {
                            Ok(m) => (Some(m.url), m.len),
                            Err(e) => {
                                tracing::warn!(
                                    variant = plan.variant,
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
                .expect("HlsIo::fetch called with non-Media segment");

            let (media_result, (init_url, init_len)) = tokio::join!(
                self.fetch.load_media_segment_with_source_for_epoch(
                    plan.variant,
                    seg_idx,
                    plan.seek_epoch,
                ),
                init_fut,
            );

            let duration = start.elapsed();
            let (media, media_cached) = media_result?;

            Ok(HlsFetch {
                init_len,
                init_url,
                media,
                media_cached,
                segment: plan.segment,
                variant: plan.variant,
                duration,
                seek_epoch: plan.seek_epoch,
            })
        })
    }
}
