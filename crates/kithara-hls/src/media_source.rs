//! HLS media source implementing kithara_decode::MediaSource.
//!
//! Provides streaming access to HLS segments for decoding.
//! Bytes are read incrementally, allowing decode to start before full download.

use std::{
    collections::HashSet,
    io::{self, Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara_abr::{AbrController, AbrOptions, ThroughputEstimator, Variant};
use kithara_assets::{Assets, AssetStore, ResourceKey};
use kithara_decode::{AudioCodec, ContainerFormat, MediaInfo, MediaSource, MediaStream};
use kithara_storage::Resource;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::{
    cache::Loader,
    events::HlsEvent,
    parsing::ContainerFormat as HlsContainerFormat,
    worker::VariantMetadata,
};

/// HLS media source for streaming decode.
///
/// Implements `MediaSource` trait from kithara-decode.
/// Opens `HlsMediaStream` which reads segments incrementally.
pub struct HlsMediaSource {
    /// Segment loader
    loader: Arc<dyn Loader>,

    /// Asset store
    assets: AssetStore,

    /// Variant metadata
    variant_metadata: Vec<VariantMetadata>,

    /// Initial variant index
    initial_variant: usize,

    /// ABR options
    abr_options: Option<AbrOptions>,

    /// Events channel
    events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,

    /// Cancellation token
    cancel: CancellationToken,
}

impl HlsMediaSource {
    /// Create a new HLS media source.
    pub fn new(
        loader: Arc<dyn Loader>,
        assets: AssetStore,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_options: Option<AbrOptions>,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            loader,
            assets,
            variant_metadata,
            initial_variant,
            abr_options,
            events_tx,
            cancel,
        }
    }
}

impl MediaSource for HlsMediaSource {
    fn open(&self) -> io::Result<Box<dyn MediaStream>> {
        let abr_variants: Vec<Variant> = self
            .variant_metadata
            .iter()
            .map(|v| Variant {
                variant_index: v.index,
                bandwidth_bps: v.bitrate.unwrap_or(0),
            })
            .collect();

        let abr_controller = self
            .abr_options
            .as_ref()
            .filter(|opts| opts.is_auto())
            .map(|opts| AbrController::new(opts.clone()));

        let stream = HlsMediaStream::new(
            Arc::clone(&self.loader),
            self.assets.clone(),
            self.variant_metadata.clone(),
            self.initial_variant,
            abr_controller,
            abr_variants,
            self.events_tx.clone(),
            self.cancel.clone(),
        );

        Ok(Box::new(stream))
    }
}

/// HLS media stream for incremental reading.
///
/// Reads segment bytes as they become available.
/// Handles variant switching and boundary detection.
pub struct HlsMediaStream {
    /// Segment loader
    loader: Arc<dyn Loader>,

    /// Asset store
    assets: AssetStore,

    /// Variant metadata
    variant_metadata: Vec<VariantMetadata>,

    /// Current variant index
    current_variant: usize,

    /// Current segment index
    current_segment_index: usize,

    /// Total segments (cached)
    total_segments: Option<usize>,

    /// Init segments sent per variant
    sent_init_for_variant: HashSet<usize>,

    /// Current segment buffer
    segment_buffer: Vec<u8>,

    /// Read position in current segment
    read_position: usize,

    /// Pending boundary info
    pending_boundary: Option<MediaInfo>,

    /// Whether stream is at EOF
    eof: bool,

    /// ABR controller
    #[allow(dead_code)]
    abr_controller: Option<AbrController<ThroughputEstimator>>,

    /// ABR variants
    #[allow(dead_code)]
    abr_variants: Vec<Variant>,

    /// Events channel
    events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,

    /// Cancellation token
    cancel: CancellationToken,
}

impl HlsMediaStream {
    fn new(
        loader: Arc<dyn Loader>,
        assets: AssetStore,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_controller: Option<AbrController<ThroughputEstimator>>,
        abr_variants: Vec<Variant>,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        // Don't set pending_boundary here - we don't have container info yet.
        // It will be set when first segment is loaded.

        Self {
            loader,
            assets,
            variant_metadata,
            current_variant: initial_variant,
            current_segment_index: 0,
            total_segments: None,
            sent_init_for_variant: HashSet::new(),
            segment_buffer: Vec::new(),
            read_position: 0,
            pending_boundary: None,
            eof: false,
            abr_controller,
            abr_variants,
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

    /// Load next segment into buffer.
    fn load_next_segment(&mut self) -> io::Result<bool> {
        if self.cancel.is_cancelled() {
            debug!("stream cancelled");
            self.eof = true;
            return Ok(false);
        }

        // Use block_in_place to safely call async code from sync context
        // This works correctly with spawn_blocking
        tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(self.load_next_segment_async())
        })
    }

    /// Async implementation of segment loading.
    async fn load_next_segment_async(&mut self) -> io::Result<bool> {
            // Get total segments count
            let total = match self.total_segments {
                Some(n) => n,
                None => {
                    match self.loader.num_segments(self.current_variant).await {
                        Ok(n) => {
                            self.total_segments = Some(n);
                            n
                        }
                        Err(e) => {
                            debug!(error = ?e, "failed to get segment count");
                            self.eof = true;
                            return Ok(false);
                        }
                    }
                }
            };

            if self.current_segment_index >= total {
                debug!("reached end of playlist");
                self.emit_event(HlsEvent::EndOfStream);
                self.eof = true;
                return Ok(false);
            }

            // Check for variant switch
            let is_variant_switch = !self.sent_init_for_variant.contains(&self.current_variant);

            // Load segment metadata
            let meta = match self
                .loader
                .load_segment(self.current_variant, self.current_segment_index)
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    debug!(
                        variant = self.current_variant,
                        segment = self.current_segment_index,
                        error = ?e,
                        "segment load failed"
                    );
                    self.eof = true;
                    return Ok(false);
                }
            };

            // Clear buffer for new segment
            self.segment_buffer.clear();
            self.read_position = 0;

            // Set boundary if variant switch
            if is_variant_switch {
                let codec = self
                    .variant_metadata
                    .get(self.current_variant)
                    .and_then(|v| convert_codec(v.codec));
                let container = meta.container.and_then(convert_container);

                self.pending_boundary = Some(MediaInfo::new(codec, container));

                // Load init segment
                if let Ok(init_meta) = self.loader.load_segment(self.current_variant, usize::MAX).await {
                    debug!(
                        variant = self.current_variant,
                        init_url = %init_meta.url,
                        "loading init segment"
                    );

                    if let Ok(resource) = self.assets.open_streaming_resource(&ResourceKey::from_url(&init_meta.url)).await {
                        if let Ok(bytes) = resource.read().await {
                            trace!(len = bytes.len(), "read init segment");
                            self.segment_buffer.extend_from_slice(&bytes);
                        }
                    }
                }

                self.sent_init_for_variant.insert(self.current_variant);
            }

            // Load media segment
            let media_key = ResourceKey::from_url(&meta.url);
            match self.assets.open_streaming_resource(&media_key).await {
                Ok(resource) => {
                    match resource.read().await {
                        Ok(bytes) => {
                            trace!(
                                variant = self.current_variant,
                                segment = self.current_segment_index,
                                len = bytes.len(),
                                "read media segment"
                            );
                            self.segment_buffer.extend_from_slice(&bytes);
                        }
                        Err(e) => {
                            warn!(error = ?e, "failed to read media segment");
                            self.eof = true;
                            return Ok(false);
                        }
                    }
                }
                Err(e) => {
                    warn!(error = ?e, "failed to open media resource");
                    self.eof = true;
                    return Ok(false);
                }
            }

            self.current_segment_index += 1;
            Ok(true)
    }
}

impl Read for HlsMediaStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Check if we have data in buffer
        if self.read_position >= self.segment_buffer.len() {
            // Need to load next segment
            if self.eof {
                return Ok(0);
            }

            if !self.load_next_segment()? {
                return Ok(0);
            }
        }

        // Read from buffer
        let available = &self.segment_buffer[self.read_position..];
        let to_read = buf.len().min(available.len());
        buf[..to_read].copy_from_slice(&available[..to_read]);
        self.read_position += to_read;

        Ok(to_read)
    }
}

impl Seek for HlsMediaStream {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        // HLS streams don't support arbitrary seek
        // This is called by Symphonia but we can't really implement it
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "HLS streams don't support seek",
        ))
    }
}

impl MediaStream for HlsMediaStream {
    fn take_boundary(&mut self) -> Option<MediaInfo> {
        self.pending_boundary.take()
    }

    fn is_eof(&self) -> bool {
        self.eof
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
        kithara_stream::AudioCodec::Adpcm => AudioCodec::Pcm,
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
        assert_eq!(convert_codec(None), None);
    }

    #[test]
    fn test_convert_container() {
        assert_eq!(
            convert_container(HlsContainerFormat::Fmp4),
            Some(ContainerFormat::Fmp4)
        );
        assert_eq!(convert_container(HlsContainerFormat::Other), None);
    }
}
