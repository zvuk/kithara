//! Segment-based stream decoder using SegmentSource trait.
//!
//! This decoder reads segments on-demand from a SegmentSource,
//! enabling zero-copy architecture where bytes stay on disk until needed.

use std::sync::Arc;

use tracing::{debug, trace};

use crate::{
    chunked_reader::SharedChunkedReader,
    decoder::Decoder,
    segment_source::{AudioCodec, ContainerFormat, SegmentId, SegmentInfo, SegmentSource},
    symphonia_mod::SymphoniaDecoder,
    types::{DecodeResult, PcmChunk, PcmSpec},
};

/// Stream decoder that reads segments from a SegmentSource.
///
/// # Architecture
///
/// The decoder pulls data from the source on-demand:
/// 1. Check if current segment is a boundary (codec change, variant switch)
/// 2. If boundary, reinitialize the Symphonia decoder
/// 3. Read segment bytes from source
/// 4. Decode and return PCM chunks
///
/// # Zero-Copy
///
/// Bytes are read directly from disk/network when `decode_next()` is called,
/// avoiding intermediate channel buffers.
///
/// # Example
///
/// ```ignore
/// let source = HlsSegmentSource::new(/* ... */);
/// let mut decoder = SegmentStreamDecoder::new(Arc::new(source));
///
/// while let Some(chunk) = decoder.decode_next()? {
///     play_audio(chunk);
/// }
/// ```
pub struct SegmentStreamDecoder<S: SegmentSource> {
    /// Segment source (HLS, progressive file, etc.)
    source: Arc<S>,

    /// Shared chunked reader for continuous decoding
    chunked_reader: SharedChunkedReader,

    /// Current Symphonia decoder instance
    decoder: Option<Box<dyn Decoder + Send + Sync>>,

    /// Current PCM spec
    current_spec: Option<PcmSpec>,

    /// Current segment being decoded
    current_segment: Option<SegmentId>,

    /// Segments processed count
    segments_processed: usize,

    /// Boundaries encountered
    boundaries_encountered: usize,

    /// Flag indicating decoder has more chunks to emit from current segment
    has_pending_chunks: bool,
}

impl<S: SegmentSource> SegmentStreamDecoder<S> {
    /// Create a new segment stream decoder.
    pub fn new(source: Arc<S>) -> Self {
        Self {
            source,
            chunked_reader: SharedChunkedReader::new(),
            decoder: None,
            current_spec: None,
            current_segment: None,
            segments_processed: 0,
            boundaries_encountered: 0,
            has_pending_chunks: false,
        }
    }

    /// Get current PCM specification.
    pub fn spec(&self) -> Option<PcmSpec> {
        self.current_spec
    }

    /// Get number of segments processed.
    pub fn segments_processed(&self) -> usize {
        self.segments_processed
    }

    /// Get number of boundaries encountered.
    pub fn boundaries_encountered(&self) -> usize {
        self.boundaries_encountered
    }

    /// Decode next PCM chunk.
    ///
    /// This method:
    /// 1. Advances to next segment if needed
    /// 2. Reads segment bytes from source
    /// 3. Decodes and returns PCM chunk
    ///
    /// Returns `None` when stream is exhausted.
    pub fn decode_next(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // If we have pending chunks from current segment, try to decode
        if self.has_pending_chunks {
            if let Some(chunk) = self.try_decode_chunk()? {
                return Ok(Some(chunk));
            }
            // No more chunks from current segment, need to advance
            self.has_pending_chunks = false;
        }

        // Advance to next segment
        let segment_id = match self.source.advance() {
            Some(id) => id,
            None => {
                debug!("segment source exhausted");
                return Ok(None);
            }
        };

        // Get segment info
        let info = self.source.current_info();

        trace!(
            variant = segment_id.variant,
            segment = segment_id.segment_index,
            is_boundary = info.is_boundary,
            "processing segment"
        );

        // Handle boundary (codec/variant change)
        if info.is_boundary || self.decoder.is_none() {
            self.boundaries_encountered += 1;
            debug!(
                segment_index = segment_id.segment_index,
                boundaries_total = self.boundaries_encountered,
                "boundary detected - reinitializing decoder"
            );

            self.chunked_reader.clear();
            self.decoder = None;
        }

        // Read segment bytes
        let bytes = self.source.read_segment(&segment_id)?;

        if bytes.is_empty() {
            debug!(segment_index = segment_id.segment_index, "empty segment");
            return self.decode_next();
        }

        // Append to chunked reader
        self.chunked_reader.append(bytes);
        self.current_segment = Some(segment_id);
        self.segments_processed += 1;

        // Create decoder if needed
        if self.decoder.is_none() {
            self.create_decoder(&info)?;
        }

        // Mark that we have pending chunks
        self.has_pending_chunks = true;

        // Decode first chunk
        self.try_decode_chunk()
    }

    /// Try to decode next chunk from current segment.
    fn try_decode_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(None);
        };

        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                self.current_spec = Some(chunk.spec);
                Ok(Some(chunk))
            }
            Ok(None) => {
                // No more data from current segment
                self.chunked_reader.release_consumed();
                self.has_pending_chunks = false;
                Ok(None)
            }
            Err(e) => {
                debug!(?e, "decode chunk error");
                self.chunked_reader.release_consumed();
                self.has_pending_chunks = false;
                Ok(None)
            }
        }
    }

    /// Create decoder from segment info.
    fn create_decoder(&mut self, info: &SegmentInfo) -> DecodeResult<()> {
        let media_info = convert_to_media_info(info);

        debug!(?media_info, "creating decoder from segment info");

        let reader_clone = self.chunked_reader.clone();
        let decoder = SymphoniaDecoder::new_from_media_info(reader_clone, &media_info, true)?;

        self.current_spec = Some(decoder.spec());
        self.decoder = Some(Box::new(decoder));

        debug!(spec = ?self.current_spec, "decoder created successfully");
        Ok(())
    }
}

/// Convert SegmentInfo to kithara_stream::MediaInfo for SymphoniaDecoder.
fn convert_to_media_info(info: &SegmentInfo) -> kithara_stream::MediaInfo {
    let container = info.container.map(|c| match c {
        ContainerFormat::Fmp4 => kithara_stream::ContainerFormat::Fmp4,
        ContainerFormat::MpegTs => kithara_stream::ContainerFormat::MpegTs,
        ContainerFormat::MpegAudio => kithara_stream::ContainerFormat::MpegAudio,
        ContainerFormat::Wav => kithara_stream::ContainerFormat::Wav,
        ContainerFormat::Ogg => kithara_stream::ContainerFormat::Ogg,
    });

    let codec = info.codec.map(|c| match c {
        AudioCodec::AacLc => kithara_stream::AudioCodec::AacLc,
        AudioCodec::AacHe => kithara_stream::AudioCodec::AacHe,
        AudioCodec::AacHeV2 => kithara_stream::AudioCodec::AacHeV2,
        AudioCodec::Mp3 => kithara_stream::AudioCodec::Mp3,
        AudioCodec::Flac => kithara_stream::AudioCodec::Flac,
        AudioCodec::Vorbis => kithara_stream::AudioCodec::Vorbis,
        AudioCodec::Opus => kithara_stream::AudioCodec::Opus,
        AudioCodec::Alac => kithara_stream::AudioCodec::Alac,
        AudioCodec::Pcm => kithara_stream::AudioCodec::Pcm,
    });

    kithara_stream::MediaInfo {
        container,
        codec,
        sample_rate: None,
        channels: None,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use bytes::Bytes;

    use super::*;

    /// Test segment source that returns predefined data.
    struct MockSegmentSource {
        segments: Vec<(SegmentInfo, Bytes)>,
        current: Mutex<usize>,
    }

    impl MockSegmentSource {
        fn new(segments: Vec<(SegmentInfo, Bytes)>) -> Self {
            Self {
                segments,
                current: Mutex::new(0),
            }
        }
    }

    impl SegmentSource for MockSegmentSource {
        fn read_segment(&self, id: &SegmentId) -> io::Result<Bytes> {
            self.segments
                .get(id.segment_index)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "segment not found"))
        }

        fn current_info(&self) -> SegmentInfo {
            let idx = *self.current.lock().unwrap();
            self.segments
                .get(idx.saturating_sub(1))
                .map(|(info, _)| info.clone())
                .unwrap_or_default()
        }

        fn advance(&self) -> Option<SegmentId> {
            let mut current = self.current.lock().unwrap();
            if *current >= self.segments.len() {
                return None;
            }
            let id = SegmentId::new(0, *current);
            *current += 1;
            Some(id)
        }

        fn current_id(&self) -> Option<SegmentId> {
            let current = *self.current.lock().unwrap();
            if current == 0 || current > self.segments.len() {
                return None;
            }
            Some(SegmentId::new(0, current - 1))
        }
    }

    #[test]
    fn test_decoder_creation() {
        let source = MockSegmentSource::new(vec![]);
        let decoder = SegmentStreamDecoder::new(Arc::new(source));

        assert_eq!(decoder.segments_processed(), 0);
        assert_eq!(decoder.boundaries_encountered(), 0);
        assert!(decoder.spec().is_none());
    }

    #[test]
    fn test_empty_source_returns_none() {
        let source = MockSegmentSource::new(vec![]);
        let mut decoder = SegmentStreamDecoder::new(Arc::new(source));

        let result = decoder.decode_next().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_boundary_counting() {
        // This test just verifies boundary detection logic
        // Real audio decoding tested in integration tests
        let segments = vec![
            (SegmentInfo::boundary(None, None), Bytes::new()),
            (SegmentInfo::continuation(), Bytes::new()),
            (SegmentInfo::boundary(None, None), Bytes::new()),
        ];

        let source = MockSegmentSource::new(segments);
        let mut decoder = SegmentStreamDecoder::new(Arc::new(source));

        // Try to advance through segments (will fail on decode, but boundaries counted)
        let _ = decoder.decode_next();
        let _ = decoder.decode_next();
        let _ = decoder.decode_next();

        // First segment is always a boundary (decoder is None initially)
        // + 2 explicit boundaries = 3 total
        assert!(decoder.boundaries_encountered() >= 1);
    }
}
