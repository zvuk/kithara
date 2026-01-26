//! Streaming decoder using MediaStream.
//!
//! Reads bytes on-demand from MediaStream, decoding as data arrives.
//! Supports both HLS (segmented) and progressive files.

use std::io::{self, Read, Seek, SeekFrom};

use tracing::{debug, trace};

use crate::{
    decoder::Decoder,
    media_source::{AudioCodec, ContainerFormat, MediaInfo, MediaStream},
    symphonia_mod::SymphoniaDecoder,
    types::{DecodeResult, PcmChunk, PcmSpec},
};

/// Streaming decoder that reads from MediaStream.
///
/// # Architecture
///
/// The decoder pulls data on-demand:
/// 1. Check `take_boundary()` - reinit decoder if needed
/// 2. Read bytes via `read()` (may block waiting for data)
/// 3. Decode and return PCM chunks
///
/// # Streaming
///
/// Unlike segment-based approach, bytes are read incrementally.
/// This reduces latency - decoding starts before full segment downloads.
///
/// # Example
///
/// ```ignore
/// let source = HlsMediaSource::new(/* ... */);
/// let stream = source.open()?;
/// let mut decoder = StreamDecoder::new(stream)?;
///
/// while let Some(chunk) = decoder.decode_next()? {
///     play_audio(chunk);
/// }
/// ```
pub struct StreamDecoder {
    /// Media stream (owned)
    stream: Box<dyn MediaStream>,

    /// Current Symphonia decoder
    decoder: Option<Box<dyn Decoder + Send + Sync>>,

    /// Reader wrapper for Symphonia
    reader: StreamReader,

    /// Current PCM spec
    current_spec: Option<PcmSpec>,

    /// Boundaries encountered
    boundaries_count: usize,

    /// Chunks decoded
    chunks_decoded: usize,
}

impl StreamDecoder {
    /// Create a new streaming decoder.
    ///
    /// Initializes decoder from first boundary info.
    pub fn new(stream: Box<dyn MediaStream>) -> DecodeResult<Self> {
        let reader = StreamReader::new();

        let mut decoder = Self {
            stream,
            decoder: None,
            reader,
            current_spec: None,
            boundaries_count: 0,
            chunks_decoded: 0,
        };

        // Check for initial boundary
        decoder.check_boundary()?;

        Ok(decoder)
    }

    /// Get current PCM specification.
    pub fn spec(&self) -> Option<PcmSpec> {
        self.current_spec
    }

    /// Get number of boundaries encountered.
    pub fn boundaries_count(&self) -> usize {
        self.boundaries_count
    }

    /// Get number of chunks decoded.
    pub fn chunks_decoded(&self) -> usize {
        self.chunks_decoded
    }

    /// Decode next PCM chunk.
    ///
    /// May block waiting for data from network.
    /// Returns `None` when stream is exhausted.
    pub fn decode_next(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // Check for boundary (codec/format change)
        self.check_boundary()?;

        // Check EOF
        if self.stream.is_eof() && self.reader.available() == 0 {
            debug!(
                boundaries = self.boundaries_count,
                chunks = self.chunks_decoded,
                "stream exhausted"
            );
            return Ok(None);
        }

        // Fill reader buffer from stream
        self.fill_reader()?;

        // Check for boundary again - it may have been set during fill_reader
        // (e.g., when stream loaded first segment with format info)
        self.check_boundary()?;

        // Try to decode
        let Some(decoder) = self.decoder.as_mut() else {
            // No decoder yet - try to create one
            self.try_create_decoder()?;
            return self.decode_next();
        };

        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                self.current_spec = Some(chunk.spec);
                self.chunks_decoded += 1;
                Ok(Some(chunk))
            }
            Ok(None) => {
                // Need more data
                if self.stream.is_eof() {
                    Ok(None)
                } else {
                    // Try to get more data
                    self.fill_reader()?;
                    self.decode_next()
                }
            }
            Err(e) => {
                trace!(?e, "decode error, trying to continue");
                // Try to continue
                if self.stream.is_eof() {
                    Ok(None)
                } else {
                    self.fill_reader()?;
                    self.decode_next()
                }
            }
        }
    }

    /// Check for boundary and reinit decoder if needed.
    fn check_boundary(&mut self) -> DecodeResult<()> {
        if let Some(info) = self.stream.take_boundary() {
            self.boundaries_count += 1;
            debug!(
                boundaries = self.boundaries_count,
                codec = ?info.codec,
                container = ?info.container,
                "boundary detected - reinitializing decoder"
            );

            // Only clear reader if we have an existing decoder (mid-stream switch).
            // For first boundary, keep the data that was just loaded.
            if self.decoder.is_some() {
                self.reader.clear();
            }
            self.decoder = None;

            // Create new decoder
            self.create_decoder(&info)?;
        }
        Ok(())
    }

    /// Try to create decoder if we have enough data.
    fn try_create_decoder(&mut self) -> DecodeResult<()> {
        if self.reader.available() > 0 {
            let info = MediaInfo::default();
            self.create_decoder(&info)?;
        }
        Ok(())
    }

    /// Create decoder from media info.
    fn create_decoder(&mut self, info: &MediaInfo) -> DecodeResult<()> {
        // Fill reader with some initial data
        self.fill_reader()?;

        if self.reader.available() == 0 {
            return Ok(());
        }

        let stream_info = convert_to_stream_info(info);

        debug!(?stream_info, "creating decoder");

        let reader = self.reader.clone();
        let decoder = SymphoniaDecoder::new_from_media_info(reader, &stream_info, true)?;

        self.current_spec = Some(decoder.spec());
        self.decoder = Some(Box::new(decoder));

        debug!(spec = ?self.current_spec, "decoder created");
        Ok(())
    }

    /// Fill reader buffer from stream.
    fn fill_reader(&mut self) -> io::Result<()> {
        const CHUNK_SIZE: usize = 32 * 1024; // 32KB chunks
        let mut buf = vec![0u8; CHUNK_SIZE];

        match self.stream.read(&mut buf) {
            Ok(0) => Ok(()), // EOF or no data available
            Ok(n) => {
                buf.truncate(n);
                self.reader.append(&buf);
                trace!(bytes = n, "filled reader buffer");
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Internal reader that wraps bytes for Symphonia.
///
/// Accumulates bytes from stream and provides Read + Seek.
#[derive(Clone)]
struct StreamReader {
    /// Accumulated data
    buffer: std::sync::Arc<std::sync::RwLock<Vec<u8>>>,
    /// Current read position
    position: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StreamReader {
    fn new() -> Self {
        Self {
            buffer: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
            position: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn append(&self, data: &[u8]) {
        let mut buf = self.buffer.write().unwrap();
        buf.extend_from_slice(data);
    }

    fn clear(&self) {
        let mut buf = self.buffer.write().unwrap();
        buf.clear();
        self.position
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    fn available(&self) -> usize {
        let buf = self.buffer.read().unwrap();
        let pos = self.position.load(std::sync::atomic::Ordering::Relaxed);
        buf.len().saturating_sub(pos)
    }
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.buffer.read().unwrap();
        let pos = self.position.load(std::sync::atomic::Ordering::Relaxed);

        if pos >= data.len() {
            return Ok(0);
        }

        let available = &data[pos..];
        let to_read = buf.len().min(available.len());
        buf[..to_read].copy_from_slice(&available[..to_read]);

        self.position
            .fetch_add(to_read, std::sync::atomic::Ordering::Relaxed);
        Ok(to_read)
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let data = self.buffer.read().unwrap();
        let len = data.len() as i64;
        let current = self.position.load(std::sync::atomic::Ordering::Relaxed) as i64;

        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(p) => len + p,
            SeekFrom::Current(p) => current + p,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }

        let new_pos = new_pos as usize;
        self.position
            .store(new_pos, std::sync::atomic::Ordering::Relaxed);
        Ok(new_pos as u64)
    }
}

/// Convert MediaInfo to kithara_stream::MediaInfo for Symphonia.
fn convert_to_stream_info(info: &MediaInfo) -> kithara_stream::MediaInfo {
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
    use super::*;

    #[test]
    fn test_stream_reader_basic() {
        let reader = StreamReader::new();
        reader.append(b"hello world");
        assert_eq!(reader.available(), 11);
    }

    #[test]
    fn test_stream_reader_read() {
        let mut reader = StreamReader::new();
        reader.append(b"hello");

        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"hel");
        assert_eq!(reader.available(), 2);
    }

    #[test]
    fn test_stream_reader_seek() {
        let mut reader = StreamReader::new();
        reader.append(b"hello world");

        reader.seek(SeekFrom::Start(6)).unwrap();
        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn test_stream_reader_clear() {
        let mut reader = StreamReader::new();
        reader.append(b"hello");

        let mut buf = [0u8; 2];
        reader.read(&mut buf).unwrap();

        reader.clear();
        assert_eq!(reader.available(), 0);

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }
}
