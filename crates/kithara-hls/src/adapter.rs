//! HlsSource: Source adapter for random-access over HLS stream.
//!
//! NEW ARCHITECTURE: Built on top of CachedLoader instead of SegmentStream.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use kithara_stream::{
    ContainerFormat as StreamContainerFormat, MediaInfo, Source,
    StreamResult as KitharaIoResult,
};
use kithara_storage::WaitOutcome;
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::{
    HlsError,
    abr::{AbrController, ThroughputEstimator},
    cache::{CachedLoader, FetchLoader},
    events::HlsEvent,
    parsing::{CodecInfo, ContainerFormat},
};

/// HLS source for random-access reading.
///
/// NEW ARCHITECTURE: Wraps CachedLoader<FetchLoader> instead of SegmentStream.
pub struct HlsSource {
    loader: Arc<CachedLoader<FetchLoader>>,
    abr: Arc<Mutex<AbrController<ThroughputEstimator>>>,
    events_tx: broadcast::Sender<HlsEvent>,
    /// Codec info for each variant (indexed by variant number).
    variant_codecs: Arc<Vec<Option<CodecInfo>>>,
    /// Last read position for seek detection and progress tracking.
    last_read_pos: std::sync::atomic::AtomicU64,
}

impl HlsSource {
    /// Create HLS source from CachedLoader (NEW architecture).
    pub(crate) fn new(
        loader: CachedLoader<FetchLoader>,
        abr: AbrController<ThroughputEstimator>,
        events_tx: broadcast::Sender<HlsEvent>,
        variant_codecs: Vec<Option<CodecInfo>>,
    ) -> Self {
        Self {
            loader: Arc::new(loader),
            abr: Arc::new(Mutex::new(abr)),
            events_tx,
            variant_codecs: Arc::new(variant_codecs),
            last_read_pos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events_tx.subscribe()
    }

    /// Get the events sender (for StreamSource implementation).
    pub(crate) fn events_tx(&self) -> &broadcast::Sender<HlsEvent> {
        &self.events_tx
    }

    /// Get current variant.
    pub fn current_variant(&self) -> usize {
        self.loader.current_variant()
    }

    /// Set current variant (manual ABR control).
    pub fn set_current_variant(&self, variant: usize) {
        let old_variant = self.loader.current_variant();
        if old_variant != variant {
            self.loader.set_current_variant(variant);
            let _ = self.events_tx.send(HlsEvent::VariantApplied {
                from_variant: old_variant,
                to_variant: variant,
                reason: crate::abr::AbrReason::ManualOverride,
            });
        }
    }

    /// Get number of loaded segments for current variant.
    pub fn loaded_segment_count(&self) -> usize {
        let variant = self.loader.current_variant();
        self.loader.loaded_segment_count(variant)
    }

    /// Get all loaded segment indices for current variant.
    pub fn loaded_segment_indices(&self) -> Vec<usize> {
        let variant = self.loader.current_variant();
        self.loader.loaded_segment_indices(variant)
    }

    /// Check if segment is loaded for current variant.
    pub fn has_segment(&self, segment_index: usize) -> bool {
        let variant = self.loader.current_variant();
        self.loader.has_segment(variant, segment_index)
    }
}

#[async_trait]
impl Source for HlsSource {
    type Item = u8;
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        // Delegate to CachedLoader
        self.loader.wait_range(range).await
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> KitharaIoResult<usize, HlsError> {
        use std::sync::atomic::Ordering;

        // Read from CachedLoader
        let bytes_read = self.loader.read_at(offset, buf).await?;

        // Update last read position and emit progress event
        let new_pos = offset.saturating_add(bytes_read as u64);
        self.last_read_pos.store(new_pos, Ordering::Relaxed);

        // Emit playback progress event
        if let Some(total_len) = self.loader.len() {
            let percent = if total_len > 0 {
                Some(((new_pos as f64 / total_len as f64) * 100.0).min(100.0) as f32)
            } else {
                None
            };
            let _ = self.events_tx.send(HlsEvent::PlaybackProgress {
                position: new_pos,
                percent,
            });
        }

        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        self.loader.len()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let variant = self.loader.current_variant();
        let codec_info = self.variant_codecs.get(variant)?.as_ref()?;

        // Convert local ContainerFormat to kithara_stream's ContainerFormat
        let container = match codec_info.container {
            Some(ContainerFormat::Fmp4) => Some(StreamContainerFormat::Fmp4),
            Some(ContainerFormat::Ts) => Some(StreamContainerFormat::MpegTs),
            Some(ContainerFormat::Other) => None,
            None => None,
        };

        Some(MediaInfo {
            container,
            codec: codec_info.audio_codec,
            sample_rate: None,
            channels: None,
        })
    }
}
