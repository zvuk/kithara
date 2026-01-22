//! HLS-specific stream decoder implementation.
//!
//! Implements `StreamDecoder` trait for HLS segments, handling:
//! - Variant switches (decoder reinitialization)
//! - Init segments (codec configuration)
//! - Boundary detection (`is_boundary()`)

use std::io::Cursor;

use async_trait::async_trait;
use bytes::Bytes;
use kithara_stream::{AudioCodec, MediaInfo, StreamMessage, StreamMetadata};
use tracing::debug;

use crate::{
    stream_decoder::StreamDecoder,
    symphonia_mod::SymphoniaDecoder,
    types::{DecodeError, DecodeResult, PcmChunk, PcmSpec},
};

/// Convert HLS ContainerFormat to stream ContainerFormat.
fn convert_container(
    container: Option<kithara_hls::ContainerFormat>,
) -> Option<kithara_stream::ContainerFormat> {
    container.map(|c| match c {
        kithara_hls::ContainerFormat::Fmp4 => kithara_stream::ContainerFormat::Fmp4,
        kithara_hls::ContainerFormat::Ts => kithara_stream::ContainerFormat::MpegTs,
        kithara_hls::ContainerFormat::Other => kithara_stream::ContainerFormat::Fmp4,
    })
}

/// HLS segment metadata (re-export for convenience).
///
/// This is defined in kithara-hls but re-exported here for API clarity.
/// Use `kithara_hls::worker::HlsSegmentMetadata` directly if needed.
pub use kithara_hls::worker::HlsSegmentMetadata;

/// HLS stream decoder.
///
/// Decodes HLS segment messages into PCM chunks, handling variant switches
/// and decoder reinitialization automatically.
///
/// # Type Parameters
/// - Generic over inner decoder implementation (for testability)
/// - Default implementation uses Symphonia via `SymphoniaHlsDecoder`
///
/// # Boundary Handling
///
/// Checks `meta.is_boundary()` to detect:
/// - Init segments (codec/format configuration)
/// - Variant switches (different codec/bitrate)
///
/// On boundary, decoder reinitializes internal state.
///
/// # Examples
///
/// ```ignore
/// use kithara_decode::HlsStreamDecoder;
/// use kithara_stream::StreamMessage;
///
/// let decoder = HlsStreamDecoder::new();
///
/// for message in hls_messages {
///     let pcm = decoder.decode_message(message).await?;
///     play_audio(pcm);
/// }
/// ```
pub struct HlsStreamDecoder {
    /// Current Symphonia decoder (recreated on boundaries)
    decoder: Option<SymphoniaDecoder>,

    /// Current decoder state
    current_codec: Option<AudioCodec>,
    current_container: Option<kithara_hls::ContainerFormat>,
    current_spec: Option<PcmSpec>,

    /// Init segment data (stored for decoder creation)
    init_segment: Option<Bytes>,

    /// Total segments processed (for debugging)
    segments_processed: usize,

    /// Total boundaries encountered (init + variant switch)
    boundaries_encountered: usize,
}

impl HlsStreamDecoder {
    /// Create a new HLS stream decoder.
    pub fn new() -> Self {
        Self {
            decoder: None,
            current_codec: None,
            current_container: None,
            current_spec: None,
            init_segment: None,
            segments_processed: 0,
            boundaries_encountered: 0,
        }
    }

    /// Reinitialize decoder for new codec/format.
    ///
    /// Called when `meta.is_boundary()` is true.
    fn reinitialize(&mut self, meta: &HlsSegmentMetadata, data: &Bytes) -> DecodeResult<()> {
        self.boundaries_encountered += 1;

        debug!(
            variant = meta.variant,
            segment_index = meta.segment_index,
            codec = ?meta.codec,
            is_init = meta.is_init_segment,
            is_switch = meta.is_variant_switch,
            "Reinitializing decoder at boundary"
        );

        // Update codec/container info
        self.current_codec = meta.codec;
        self.current_container = meta.container;

        // If this is an init segment, store it
        if meta.is_init_segment && !data.is_empty() {
            debug!(size = data.len(), "Storing init segment");
            self.init_segment = Some(data.clone());
        }

        // Discard old decoder on variant switch
        if meta.is_variant_switch {
            debug!("Variant switch - discarding old decoder");
            self.decoder = None;
        }

        Ok(())
    }

    /// Decode segment bytes to PCM.
    ///
    /// Each media segment is decoded independently with init segment.
    fn decode_segment(&mut self, bytes: &Bytes) -> DecodeResult<PcmChunk<f32>> {
        // Create decoder for this segment (init + media)
        let decoder = self.create_decoder_for_segment(bytes)?;

        // Decode all available chunks from this segment
        let mut all_samples = Vec::new();
        let mut spec = decoder.spec();
        let mut chunks_decoded = 0;

        let mut decoder = decoder;
        loop {
            match decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    spec = chunk.spec;
                    all_samples.extend_from_slice(&chunk.pcm);
                    chunks_decoded += 1;
                }
                Ok(None) => {
                    // End of stream
                    debug!(chunks_decoded, "Decoder EOF reached");
                    break;
                }
                Err(e) => {
                    // Expected for end of fMP4 fragment
                    debug!(?e, chunks_decoded, "Decode ended");
                    break;
                }
            }
        }

        self.current_spec = Some(spec);

        debug!(
            chunks_decoded,
            samples = all_samples.len(),
            "Segment decoded"
        );

        Ok(PcmChunk::new(spec, all_samples))
    }

    /// Create Symphonia decoder for a single media segment.
    ///
    /// Combines init segment with media segment to create complete fMP4 fragment.
    fn create_decoder_for_segment(&mut self, media_bytes: &Bytes) -> DecodeResult<SymphoniaDecoder> {
        // Combine init segment with this media segment
        let full_data = if let Some(init) = &self.init_segment {
            let mut combined = Vec::with_capacity(init.len() + media_bytes.len());
            combined.extend_from_slice(init);
            combined.extend_from_slice(media_bytes);
            Bytes::from(combined)
        } else {
            media_bytes.clone()
        };

        let total_bytes = full_data.len();
        let cursor = Cursor::new(full_data);

        // Create decoder based on available metadata
        let decoder = if let Some(codec) = self.current_codec {
            let container = convert_container(self.current_container);

            let media_info = MediaInfo {
                container,
                codec: Some(codec),
                sample_rate: None,
                channels: None,
            };

            debug!(?codec, ?container, total_bytes, "Creating decoder for segment");
            SymphoniaDecoder::new_from_media_info(cursor, &media_info, true)?
        } else {
            // Fallback to probe
            debug!(total_bytes, "Creating decoder for segment with probe");
            SymphoniaDecoder::new_with_probe(cursor, None)?
        };

        self.current_spec = Some(decoder.spec());

        debug!(
            spec = ?self.current_spec,
            "Decoder created successfully"
        );

        Ok(decoder)
    }
}

impl Default for HlsStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamDecoder<HlsSegmentMetadata, Bytes> for HlsStreamDecoder {
    type Output = PcmChunk<f32>;

    async fn decode_message(
        &mut self,
        message: StreamMessage<HlsSegmentMetadata, Bytes>,
    ) -> DecodeResult<Self::Output> {
        let (meta, data) = message.into_parts();

        self.segments_processed += 1;

        // Check if reinitialization needed
        if meta.is_boundary() {
            self.reinitialize(&meta, &data)?;

            // Init segments don't contain media data to decode
            if meta.is_init_segment {
                return Ok(PcmChunk::new(
                    self.current_spec.unwrap_or(PcmSpec {
                        sample_rate: 44100,
                        channels: 2,
                    }),
                    vec![],
                ));
            }
        } else {
            // Update codec even for non-boundary segments
            if let Some(codec) = meta.codec {
                self.current_codec = Some(codec);
            }
        }

        // Skip empty messages
        if data.is_empty() {
            return Ok(PcmChunk::new(
                self.current_spec.unwrap_or(PcmSpec {
                    sample_rate: 44100,
                    channels: 2,
                }),
                vec![],
            ));
        }

        // Decode bytes
        let pcm = self.decode_segment(&data)?;

        // Update spec from decoded output
        self.current_spec = Some(pcm.spec);

        Ok(pcm)
    }

    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
        debug!(
            segments_processed = self.segments_processed,
            boundaries = self.boundaries_encountered,
            "Flushing decoder"
        );

        // Clear decoder and state
        self.decoder = None;
        self.init_segment = None;

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kithara_stream::StreamMetadata;
    use url::Url;

    fn create_test_metadata(
        variant: usize,
        segment_index: usize,
        is_init: bool,
        is_switch: bool,
    ) -> HlsSegmentMetadata {
        HlsSegmentMetadata {
            byte_offset: 0,
            variant,
            segment_index,
            segment_url: Url::parse("http://test.com/seg.ts").unwrap(),
            segment_duration: None,
            codec: Some(AudioCodec::AacLc),
            container: None,
            bitrate: Some(128000),
            encryption: None,
            is_init_segment: is_init,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: is_switch,
        }
    }

    #[tokio::test]
    async fn test_decoder_creation() {
        let decoder = HlsStreamDecoder::new();
        assert_eq!(decoder.segments_processed, 0);
        assert_eq!(decoder.boundaries_encountered, 0);
    }

    #[tokio::test]
    async fn test_boundary_detection() {
        let mut decoder = HlsStreamDecoder::new();

        // Regular segment - no boundary
        let meta1 = create_test_metadata(0, 0, false, false);
        assert!(!meta1.is_boundary());

        let msg1 = StreamMessage::new(meta1, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg1).await.unwrap();
        assert_eq!(decoder.boundaries_encountered, 0);

        // Init segment - boundary
        let meta2 = create_test_metadata(0, usize::MAX, true, false);
        assert!(meta2.is_boundary());

        let msg2 = StreamMessage::new(meta2, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg2).await.unwrap();
        assert_eq!(decoder.boundaries_encountered, 1);

        // Variant switch - boundary
        let meta3 = create_test_metadata(1, 0, false, true);
        assert!(meta3.is_boundary());

        let msg3 = StreamMessage::new(meta3, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg3).await.unwrap();
        assert_eq!(decoder.boundaries_encountered, 2);
    }

    #[tokio::test]
    async fn test_segments_counter() {
        let mut decoder = HlsStreamDecoder::new();

        for i in 0..5 {
            let meta = create_test_metadata(0, i, false, false);
            let msg = StreamMessage::new(meta, Bytes::new());
            let _pcm: PcmChunk<f32> = decoder.decode_message(msg).await.unwrap();
        }

        assert_eq!(decoder.segments_processed, 5);
    }

    #[tokio::test]
    async fn test_flush() {
        let mut decoder = HlsStreamDecoder::new();

        // Process some segments
        let meta = create_test_metadata(0, 0, false, false);
        let msg = StreamMessage::new(meta, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg).await.unwrap();

        // Flush
        let remaining: Vec<PcmChunk<f32>> = decoder.flush().await.unwrap();
        assert_eq!(remaining.len(), 0);

        // Decoder should be cleared
        assert!(decoder.decoder.is_none());
        assert!(decoder.init_segment.is_none());
    }

    #[tokio::test]
    async fn test_codec_change_on_boundary() {
        let mut decoder = HlsStreamDecoder::new();

        // First segment with AAC
        let mut meta1 = create_test_metadata(0, 0, false, false);
        meta1.codec = Some(AudioCodec::AacLc);
        let msg1 = StreamMessage::new(meta1, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg1).await.unwrap();
        assert_eq!(decoder.current_codec, Some(AudioCodec::AacLc));

        // Variant switch with MP3
        let mut meta2 = create_test_metadata(1, 0, false, true);
        meta2.codec = Some(AudioCodec::Mp3);
        let msg2 = StreamMessage::new(meta2, Bytes::new());
        let _pcm: PcmChunk<f32> = decoder.decode_message(msg2).await.unwrap();

        // Codec should have changed
        assert_eq!(decoder.current_codec, Some(AudioCodec::Mp3));
        assert_eq!(decoder.boundaries_encountered, 1);
    }
}
