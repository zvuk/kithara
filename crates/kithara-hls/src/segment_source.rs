//! HLS segment source implementing kithara_decode::SegmentSource trait.
//!
//! Provides zero-copy access to HLS segments for decoding.

use std::{
    collections::HashSet,
    io,
    sync::{Arc, Mutex},
};

use bytes::{Bytes, BytesMut};
use kithara_abr::{AbrController, AbrOptions, ThroughputEstimator, Variant};
use kithara_assets::{Assets, AssetStore, ResourceKey};
use kithara_decode::{AudioCodec, ContainerFormat, SegmentId, SegmentInfo, SegmentSource};
use kithara_storage::Resource;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::{
    cache::{Loader, SegmentMeta},
    events::HlsEvent,
    parsing::ContainerFormat as HlsContainerFormat,
    worker::VariantMetadata,
};

/// HLS segment source for zero-copy decoding.
///
/// Implements `SegmentSource` trait from kithara-decode, allowing the decoder
/// to read segment bytes directly from disk without intermediate buffers.
///
/// # Architecture
///
/// The source maintains:
/// - Current segment index and variant
/// - ABR controller for adaptive switching
/// - Init segment tracking (for fMP4)
/// - Events channel for monitoring
///
/// # Thread Safety
///
/// Uses internal `Mutex` for state to allow concurrent access from decoder thread.
pub struct HlsSegmentSource {
    /// Inner mutable state
    inner: Mutex<HlsSegmentSourceInner>,

    /// Segment loader (fetches segment metadata)
    loader: Arc<dyn Loader>,

    /// Asset store (reads bytes from disk)
    assets: AssetStore,

    /// Variant metadata (codec, container, bitrate)
    variant_metadata: Vec<VariantMetadata>,

    /// Events channel (optional)
    events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,

    /// Cancellation token
    cancel: CancellationToken,
}

/// Inner mutable state for HlsSegmentSource.
struct HlsSegmentSourceInner {
    /// Current variant index
    current_variant: usize,

    /// Current segment index within variant
    current_segment_index: usize,

    /// Init segments already sent for each variant
    sent_init_for_variant: HashSet<usize>,

    /// Whether current segment is a variant switch
    is_variant_switch: bool,

    /// Last loaded segment metadata
    last_segment_meta: Option<SegmentMeta>,

    /// Last init segment metadata
    last_init_meta: Option<SegmentMeta>,

    /// ABR controller (TODO: implement ABR in advance())
    #[allow(dead_code)]
    abr_controller: Option<AbrController<ThroughputEstimator>>,

    /// ABR variants list (TODO: implement ABR in advance())
    #[allow(dead_code)]
    abr_variants: Vec<Variant>,

    /// Total segments count (cached)
    total_segments: Option<usize>,
}

impl HlsSegmentSource {
    /// Create a new HLS segment source.
    pub fn new(
        loader: Arc<dyn Loader>,
        assets: AssetStore,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_options: Option<AbrOptions>,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        let abr_variants: Vec<Variant> = variant_metadata
            .iter()
            .map(|v| Variant {
                variant_index: v.index,
                bandwidth_bps: v.bitrate.unwrap_or(0),
            })
            .collect();

        let abr_controller = abr_options
            .as_ref()
            .filter(|opts| opts.is_auto())
            .map(|opts| AbrController::new(opts.clone()));

        Self {
            inner: Mutex::new(HlsSegmentSourceInner {
                current_variant: initial_variant,
                current_segment_index: 0,
                sent_init_for_variant: HashSet::new(),
                is_variant_switch: true, // First segment is always a switch
                last_segment_meta: None,
                last_init_meta: None,
                abr_controller,
                abr_variants,
                total_segments: None,
            }),
            loader,
            assets,
            variant_metadata,
            events_tx,
            cancel,
        }
    }

    /// Emit an event.
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Get tokio runtime handle for blocking operations.
    fn runtime_handle() -> io::Result<tokio::runtime::Handle> {
        tokio::runtime::Handle::try_current()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}

impl SegmentSource for HlsSegmentSource {
    fn read_segment(&self, id: &SegmentId) -> io::Result<Bytes> {
        let handle = Self::runtime_handle()?;

        handle.block_on(async {
            let mut inner = self.inner.lock().unwrap();

            // Get cached metadata
            let meta = inner
                .last_segment_meta
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no segment metadata"))?
                .clone();

            let init_meta = inner.last_init_meta.clone();
            let is_variant_switch = inner.is_variant_switch;

            // Clear variant switch flag after reading
            inner.is_variant_switch = false;
            drop(inner);

            let mut segment_bytes = BytesMut::new();

            // Read init segment if variant switch
            if is_variant_switch {
                if let Some(ref init) = init_meta {
                    let init_key = ResourceKey::from_url(&init.url);
                    match self.assets.open_streaming_resource(&init_key).await {
                        Ok(resource) => match resource.read().await {
                            Ok(bytes) => {
                                trace!(
                                    variant = id.variant,
                                    len = bytes.len(),
                                    "read init segment"
                                );
                                segment_bytes.extend_from_slice(&bytes);
                            }
                            Err(e) => {
                                warn!(variant = id.variant, error = ?e, "failed to read init segment");
                            }
                        },
                        Err(e) => {
                            warn!(variant = id.variant, error = ?e, "failed to open init resource");
                        }
                    }
                }
            }

            // Read media segment
            let media_key = ResourceKey::from_url(&meta.url);
            match self.assets.open_streaming_resource(&media_key).await {
                Ok(resource) => match resource.read().await {
                    Ok(bytes) => {
                        trace!(
                            variant = id.variant,
                            segment = id.segment_index,
                            len = bytes.len(),
                            "read media segment"
                        );
                        segment_bytes.extend_from_slice(&bytes);
                    }
                    Err(e) => {
                        warn!(
                            variant = id.variant,
                            segment = id.segment_index,
                            error = ?e,
                            "failed to read media segment"
                        );
                        return Err(io::Error::new(io::ErrorKind::Other, e.to_string()));
                    }
                },
                Err(e) => {
                    warn!(
                        variant = id.variant,
                        segment = id.segment_index,
                        error = ?e,
                        "failed to open media resource"
                    );
                    return Err(io::Error::new(io::ErrorKind::Other, e.to_string()));
                }
            }

            Ok(segment_bytes.freeze())
        })
    }

    fn current_info(&self) -> SegmentInfo {
        let inner = self.inner.lock().unwrap();

        let is_boundary = inner.is_variant_switch;

        // Get codec and container from variant metadata
        let variant_meta = self.variant_metadata.get(inner.current_variant);

        let codec = variant_meta.and_then(|v| convert_codec(v.codec));
        let container = inner
            .last_segment_meta
            .as_ref()
            .and_then(|m| m.container)
            .and_then(convert_container);

        if is_boundary {
            SegmentInfo::boundary(codec, container)
        } else {
            SegmentInfo::continuation()
        }
    }

    fn advance(&self) -> Option<SegmentId> {
        if self.cancel.is_cancelled() {
            debug!("segment source cancelled");
            return None;
        }

        let handle = Self::runtime_handle().ok()?;

        handle.block_on(async {
            let mut inner = self.inner.lock().unwrap();
            let current_variant = inner.current_variant;

            // Get total segments count (cache it)
            let total_segments = match inner.total_segments {
                Some(n) => n,
                None => {
                    match self.loader.num_segments(current_variant).await {
                        Ok(n) => {
                            inner.total_segments = Some(n);
                            n
                        }
                        Err(e) => {
                            debug!(error = ?e, "failed to get segment count");
                            return None;
                        }
                    }
                }
            };

            if inner.current_segment_index >= total_segments {
                debug!("reached end of playlist");
                self.emit_event(HlsEvent::EndOfStream);
                return None;
            }

            // Check if this is a variant switch
            inner.is_variant_switch = !inner.sent_init_for_variant.contains(&current_variant);

            // Load segment metadata
            let meta = match self
                .loader
                .load_segment(current_variant, inner.current_segment_index)
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    debug!(
                        variant = current_variant,
                        segment_index = inner.current_segment_index,
                        error = ?e,
                        "segment metadata load failed"
                    );
                    return None;
                }
            };

            inner.last_segment_meta = Some(meta);

            // Load init segment metadata if variant switch
            if inner.is_variant_switch {
                match self.loader.load_segment(current_variant, usize::MAX).await {
                    Ok(init_meta) => {
                        debug!(
                            variant = current_variant,
                            init_url = %init_meta.url,
                            "init segment loaded for variant switch"
                        );
                        inner.sent_init_for_variant.insert(current_variant);
                        inner.last_init_meta = Some(init_meta);
                    }
                    Err(e) => {
                        debug!(variant = current_variant, error = ?e, "init segment load failed");
                        inner.last_init_meta = None;
                    }
                }
            }

            let segment_id = SegmentId::new(current_variant, inner.current_segment_index);
            inner.current_segment_index += 1;

            trace!(
                variant = segment_id.variant,
                segment = segment_id.segment_index,
                is_variant_switch = inner.is_variant_switch,
                "advancing to segment"
            );

            // TODO: ABR decision (for now, stay on current variant)

            Some(segment_id)
        })
    }

    fn current_id(&self) -> Option<SegmentId> {
        let inner = self.inner.lock().unwrap();
        if inner.current_segment_index == 0 {
            return None;
        }
        Some(SegmentId::new(
            inner.current_variant,
            inner.current_segment_index - 1,
        ))
    }
}

/// Convert HLS AudioCodec to decode AudioCodec.
fn convert_codec(codec: Option<kithara_stream::AudioCodec>) -> Option<AudioCodec> {
    codec.map(|c| match c {
        kithara_stream::AudioCodec::AacLc => AudioCodec::AacLc,
        kithara_stream::AudioCodec::AacHe => AudioCodec::AacHe,
        kithara_stream::AudioCodec::AacHeV2 => AudioCodec::AacHeV2,
        kithara_stream::AudioCodec::Mp3 => AudioCodec::Mp3,
        kithara_stream::AudioCodec::Flac => AudioCodec::Flac,
        kithara_stream::AudioCodec::Vorbis => AudioCodec::Vorbis,
        kithara_stream::AudioCodec::Opus => AudioCodec::Opus,
        kithara_stream::AudioCodec::Alac => AudioCodec::Alac,
        kithara_stream::AudioCodec::Pcm => AudioCodec::Pcm,
        kithara_stream::AudioCodec::Adpcm => AudioCodec::Pcm, // Map ADPCM to PCM
    })
}

/// Convert HLS ContainerFormat to decode ContainerFormat.
fn convert_container(container: HlsContainerFormat) -> Option<ContainerFormat> {
    match container {
        HlsContainerFormat::Fmp4 => Some(ContainerFormat::Fmp4),
        HlsContainerFormat::Ts => Some(ContainerFormat::MpegTs),
        HlsContainerFormat::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_codec() {
        assert_eq!(
            convert_codec(Some(kithara_stream::AudioCodec::AacLc)),
            Some(AudioCodec::AacLc)
        );
        assert_eq!(
            convert_codec(Some(kithara_stream::AudioCodec::Mp3)),
            Some(AudioCodec::Mp3)
        );
        assert_eq!(convert_codec(None), None);
    }

    #[test]
    fn test_convert_container() {
        assert_eq!(
            convert_container(HlsContainerFormat::Fmp4),
            Some(ContainerFormat::Fmp4)
        );
        assert_eq!(
            convert_container(HlsContainerFormat::Ts),
            Some(ContainerFormat::MpegTs)
        );
        assert_eq!(convert_container(HlsContainerFormat::Other), None);
    }
}
