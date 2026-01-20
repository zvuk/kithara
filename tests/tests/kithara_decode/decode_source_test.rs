//! Isolated unit tests for DecodeSource with MockDecoder.
//!
//! Tests that DecodeSource correctly reads all chunks from the inner decoder
//! without resampling interference.

use std::time::Duration;

use kithara_decode::{DecodeResult, Decoder, PcmChunk, PcmSpec, Pipeline};

/// Simple mock decoder that generates N chunks with predictable data.
///
/// Each chunk contains:
/// - Sample 0: chunk_index as f32 (0.0, 1.0, 2.0, ...)
/// - Sample 1..10: sequential data [0.0, 1.0, 2.0, ..., 8.0]
struct SimpleMockDecoder {
    spec: PcmSpec,
    total_chunks: usize,
    current_chunk: usize,
}

impl SimpleMockDecoder {
    fn new(spec: PcmSpec, total_chunks: usize) -> Self {
        Self {
            spec,
            total_chunks,
            current_chunk: 0,
        }
    }

    fn generate_chunk(&self, chunk_index: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(10);

        // Metadata: chunk index
        samples.push(chunk_index as f32);

        // Data: sequential pattern
        for i in 0..9 {
            samples.push(i as f32);
        }

        samples
    }
}

impl Decoder for SimpleMockDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        if self.current_chunk >= self.total_chunks {
            return Ok(None); // EOF
        }

        let pcm = self.generate_chunk(self.current_chunk);
        self.current_chunk += 1;

        Ok(Some(PcmChunk::new(self.spec, pcm)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_mock_decoder_generates_correct_chunks() {
        let spec = PcmSpec {
            sample_rate: 44100,
            channels: 2,
        };
        let mut decoder = SimpleMockDecoder::new(spec, 5);

        // Chunk 0
        let chunk = decoder.next_chunk().unwrap().unwrap();
        assert_eq!(chunk.pcm[0], 0.0); // chunk_index = 0
        assert_eq!(chunk.pcm[1], 0.0); // data[0]
        assert_eq!(chunk.pcm[9], 8.0); // data[8]

        // Chunk 1
        let chunk = decoder.next_chunk().unwrap().unwrap();
        assert_eq!(chunk.pcm[0], 1.0); // chunk_index = 1

        // Skip to EOF
        decoder.next_chunk().unwrap(); // 2
        decoder.next_chunk().unwrap(); // 3
        decoder.next_chunk().unwrap(); // 4

        // EOF
        assert!(decoder.next_chunk().unwrap().is_none());
        assert!(decoder.next_chunk().unwrap().is_none()); // Repeated calls return None
    }

    /// Test that Pipeline with SimpleMockDecoder reads ALL chunks from decoder.
    ///
    /// This verifies that DecodeSource correctly processes all chunks without:
    /// - Losing chunks
    /// - Stopping early
    /// - Duplicating chunks
    #[tokio::test]
    async fn test_pipeline_reads_all_chunks_from_decoder() {
        const TOTAL_CHUNKS: usize = 100;

        let spec = PcmSpec {
            sample_rate: 44100,
            channels: 2,
        };

        // Create SimpleMockDecoder with 100 chunks
        let decoder = SimpleMockDecoder::new(spec, TOTAL_CHUNKS);

        // Create Pipeline with decoder (no resampling - source_rate == target_rate)
        let pipeline = Pipeline::with_decoder(decoder, spec, 44100).await.unwrap();

        // Give pipeline time to process all chunks
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Read all samples from channel
        let mut all_samples = Vec::new();
        let sample_rx = pipeline.consumer();
        let read_timeout = Duration::from_millis(100);

        // Read until timeout (all chunks processed)
        loop {
            // Try to receive next chunk with timeout
            let async_rx = sample_rx.as_async();

            let result = tokio::time::timeout(read_timeout, async { async_rx.recv().await }).await;

            match result {
                Ok(Ok(chunk)) => {
                    all_samples.extend_from_slice(&chunk);
                }
                Ok(Err(_)) | Err(_) => {
                    break; // Channel closed or timeout
                }
            }
        }

        println!("Total samples read: {}", all_samples.len());

        // Verify total chunks (each chunk = 10 samples)
        let total_chunks_read = all_samples.len() / 10;
        assert_eq!(
            total_chunks_read, TOTAL_CHUNKS,
            "Should read exactly {} chunks, got {}",
            TOTAL_CHUNKS, total_chunks_read
        );

        // Verify chunks are sequential and correct
        for chunk_idx in 0..TOTAL_CHUNKS {
            let offset = chunk_idx * 10;
            let chunk_samples = &all_samples[offset..offset + 10];

            // Check metadata (chunk index)
            assert_eq!(
                chunk_samples[0], chunk_idx as f32,
                "Chunk {} has wrong index",
                chunk_idx
            );

            // Check data pattern [0.0, 1.0, 2.0, ..., 8.0]
            for i in 0..9 {
                assert_eq!(
                    chunk_samples[i + 1],
                    i as f32,
                    "Chunk {} data mismatch at index {}",
                    chunk_idx,
                    i
                );
            }
        }

        println!("✅ All {} chunks read correctly!", TOTAL_CHUNKS);
    }
}
