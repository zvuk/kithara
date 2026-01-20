//! Mock decoder for testing decode pipeline with HLS.
//!
//! Parses binary segments from AbrTestServer and generates synthetic PCM samples.
//!
//! Binary format: [VARIANT:1byte][SEGMENT:4bytes][DATA_LEN:4bytes][DATA:DATA_LEN bytes]

use std::io::Read;

use kithara_decode::{DecodeError, DecodeResult, Decoder, PcmChunk, PcmSpec};

/// Mock decoder that parses binary segments and generates synthetic PCM samples.
///
/// Binary format: [VARIANT:1byte][SEGMENT:4bytes][DATA_LEN:4bytes][DATA:DATA_LEN bytes]
///
/// Generated PCM pattern per segment:
/// - Sample 0: variant as f32 (e.g., 0.0, 1.0, 2.0)
/// - Sample 1: segment as f32 (e.g., 3.0, 4.0, 5.0)
/// - Sample 2..102: sequential data [0.0, 1.0, 2.0, ..., 99.0]
///
/// This allows tests to verify:
/// - ABR switches (variant changes)
/// - Sequential segments (no gaps or duplicates)
/// - Proper decode pipeline ordering
pub struct MockDecoder<R> {
    reader: R,
    spec: PcmSpec,
    eof: bool,
}

impl<R: Read> MockDecoder<R> {
    /// Create new mock decoder.
    ///
    /// Default spec: 44100 Hz, 2 channels (stereo).
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            spec: PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            eof: false,
        }
    }

    /// Create with custom spec.
    pub fn with_spec(reader: R, spec: PcmSpec) -> Self {
        Self {
            reader,
            spec,
            eof: false,
        }
    }

    /// Read next segment header and data from reader.
    ///
    /// Binary format: [VARIANT:1byte][SEGMENT:4bytes][DATA_LEN:4bytes][DATA:DATA_LEN bytes]
    ///
    /// Returns (variant, segment) and skips DATA.
    ///
    /// Handles non-blocking readers (SyncReader) by retrying on WouldBlock/empty reads.
    fn read_segment(&mut self) -> DecodeResult<Option<(usize, usize)>> {
        if self.eof {
            return Ok(None);
        }

        // Read header: 1 + 4 + 4 = 9 bytes (with retry for non-blocking readers)
        let mut header = [0u8; 9];
        if let Err(e) = self.read_exact_with_retry(&mut header) {
            // Check if it's EOF
            if let DecodeError::Io(ref io_err) = e {
                if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                    // True EOF
                    return Ok(None);
                }
            }
            return Err(e);
        }

        // Parse header
        let variant = header[0] as usize;
        let segment = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let data_len = u32::from_be_bytes([header[5], header[6], header[7], header[8]]) as usize;

        println!(
            "[MockDecoder] Read segment: variant={}, segment={}, data_len={}",
            variant, segment, data_len
        );

        // Skip DATA (we don't need it, just segment metadata)
        let mut skip_buf = vec![0u8; data_len.min(64 * 1024)]; // Skip in chunks
        let mut remaining = data_len;

        while remaining > 0 {
            let to_read = remaining.min(skip_buf.len());
            self.read_exact_with_retry(&mut skip_buf[..to_read])?;
            remaining -= to_read;
        }

        Ok(Some((variant, segment)))
    }

    /// Read exact amount of bytes with retry for non-blocking readers.
    ///
    /// Unlike std::io::Read::read_exact(), this handles:
    /// - WouldBlock errors (retry)
    /// - Zero-byte reads from non-blocking readers (retry with sleep)
    /// - True EOF (returns UnexpectedEof error)
    fn read_exact_with_retry(&mut self, mut buf: &mut [u8]) -> DecodeResult<()> {
        const MAX_RETRIES: usize = 50_000; // 5 seconds total (50k * 100μs)
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_micros(100);

        let mut retries = 0;

        while !buf.is_empty() {
            match self.reader.read(buf) {
                Ok(0) => {
                    // Zero-byte read - could be:
                    // 1. True EOF (no more data will ever arrive)
                    // 2. Non-blocking reader with no data yet (need to retry)

                    retries += 1;
                    if retries >= MAX_RETRIES {
                        // Timeout - assume EOF
                        self.eof = true;
                        println!("[MockDecoder] EOF (timeout after {} retries)", retries);
                        println!(
                            "[MockDecoder] DEBUG: At EOF, reader position unknown (would need Seek trait)"
                        );
                        return Err(DecodeError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "EOF during read",
                        )));
                    }

                    // Sleep and retry (for non-blocking readers)
                    std::thread::sleep(RETRY_DELAY);
                }
                Ok(n) => {
                    // Progress made - reset retry counter
                    retries = 0;
                    buf = &mut buf[n..];
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Non-blocking reader - retry
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        return Err(DecodeError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "Read timeout (WouldBlock)",
                        )));
                    }
                    std::thread::sleep(RETRY_DELAY);
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    println!("[MockDecoder] EOF (UnexpectedEof)");
                    return Err(DecodeError::Io(e));
                }
                Err(e) => return Err(DecodeError::Io(e)),
            }
        }

        Ok(())
    }

    /// Generate synthetic PCM samples for a segment.
    ///
    /// Pattern:
    /// - Sample 0: variant as f32
    /// - Sample 1: segment as f32
    /// - Sample 2..102: [0.0, 1.0, 2.0, ..., 99.0]
    ///
    /// Total: 102 samples per segment.
    fn generate_samples(&self, variant: usize, segment: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(102);

        // Metadata samples
        samples.push(variant as f32);
        samples.push(segment as f32);

        // Data samples
        for i in 0..100 {
            samples.push(i as f32);
        }

        samples
    }
}

impl<R: Read + Send + 'static> Decoder for MockDecoder<R> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // Read next segment header
        match self.read_segment()? {
            Some((variant, segment)) => {
                // Generate synthetic PCM samples
                let pcm = self.generate_samples(variant, segment);
                Ok(Some(PcmChunk::new(self.spec, pcm)))
            }
            None => Ok(None), // EOF
        }
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rstest::rstest;

    use super::*;

    /// Helper: create binary segment data
    fn make_segment(variant: u8, segment: u32, data_len: usize) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(variant);
        data.extend(&segment.to_be_bytes());
        data.extend(&(data_len as u32).to_be_bytes());
        data.extend(vec![b'A'; data_len]);
        data
    }

    /// Parametrized test for decoding segments.
    ///
    /// Each case defines: (segments to create, expected metadata per chunk)
    #[rstest]
    #[case(
        vec![(0, 5, 100)],
        vec![(0.0, 5.0)],
        "single segment"
    )]
    #[case(
        vec![(0, 0, 1000), (0, 1, 1000)],
        vec![(0.0, 0.0), (0.0, 1.0)],
        "two segments same variant"
    )]
    #[case(
        vec![(0, 0, 1000), (0, 1, 1000), (2, 2, 1000)],
        vec![(0.0, 0.0), (0.0, 1.0), (2.0, 2.0)],
        "three segments with variant switch"
    )]
    fn test_decode_segments(
        #[case] segments: Vec<(u8, u32, usize)>,
        #[case] expected: Vec<(f32, f32)>,
        #[case] desc: &str,
    ) {
        // Create binary data
        let mut data = Vec::new();
        for (variant, segment, data_len) in segments {
            data.extend(make_segment(variant, segment, data_len));
        }

        let reader = Cursor::new(data);
        let mut decoder = MockDecoder::new(reader);

        // Verify each chunk
        for (idx, (expected_variant, expected_segment)) in expected.iter().enumerate() {
            let chunk = decoder
                .next_chunk()
                .unwrap()
                .unwrap_or_else(|| panic!("{}: chunk {} missing", desc, idx));

            assert_eq!(chunk.spec, decoder.spec(), "{}: spec mismatch", desc);
            assert_eq!(chunk.pcm.len(), 102, "{}: pcm length mismatch", desc);
            assert_eq!(
                chunk.pcm[0], *expected_variant,
                "{}: variant mismatch",
                desc
            );
            assert_eq!(
                chunk.pcm[1], *expected_segment,
                "{}: segment mismatch",
                desc
            );

            // Verify data pattern (first few samples)
            assert_eq!(chunk.pcm[2], 0.0, "{}: data[0] should be 0.0", desc);
            assert_eq!(chunk.pcm[3], 1.0, "{}: data[1] should be 1.0", desc);
            assert_eq!(chunk.pcm[101], 99.0, "{}: data[99] should be 99.0", desc);
        }

        // Verify EOF
        assert!(
            decoder.next_chunk().unwrap().is_none(),
            "{}: expected EOF after all segments",
            desc
        );
    }

    #[test]
    fn test_decode_eof() {
        let data = make_segment(1, 3, 50);
        let reader = Cursor::new(data);
        let mut decoder = MockDecoder::new(reader);

        // First chunk
        assert!(decoder.next_chunk().unwrap().is_some());

        // EOF
        assert!(decoder.next_chunk().unwrap().is_none());
        assert!(decoder.next_chunk().unwrap().is_none()); // Repeated calls return None
    }

    #[rstest]
    #[case(None, 44100, 2, "default spec")]
    #[case(Some(PcmSpec { sample_rate: 48000, channels: 1 }), 48000, 1, "mono 48kHz")]
    #[case(Some(PcmSpec { sample_rate: 96000, channels: 6 }), 96000, 6, "5.1 surround 96kHz")]
    fn test_decoder_spec(
        #[case] custom_spec: Option<PcmSpec>,
        #[case] expected_rate: u32,
        #[case] expected_channels: u16,
        #[case] desc: &str,
    ) {
        let reader = Cursor::new(Vec::new());
        let decoder = match custom_spec {
            Some(spec) => MockDecoder::with_spec(reader, spec),
            None => MockDecoder::new(reader),
        };

        assert_eq!(
            decoder.spec().sample_rate,
            expected_rate,
            "{}: sample rate",
            desc
        );
        assert_eq!(
            decoder.spec().channels,
            expected_channels,
            "{}: channels",
            desc
        );
    }
}
