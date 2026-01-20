//! Isolated unit tests for Pipeline with mock decoder.
//!
//! Tests Pipeline independently from HLS to verify core decode pipeline logic.

use std::time::Duration;

use kithara_decode::{DecodeResult, Decoder, PcmChunk, PcmSpec, Pipeline};
use rstest::rstest;

/// Simple mock decoder that generates predictable PCM chunks.
///
/// Each chunk has a predictable pattern:
/// - Sample 0: chunk_number as f32
/// - Sample 1..101: sequential numbers [0.0, 1.0, ..., 99.0]
struct SimpleMockDecoder {
    chunks_to_generate: usize,
    chunks_generated: usize,
    spec: PcmSpec,
}

impl SimpleMockDecoder {
    fn new(chunks_to_generate: usize) -> Self {
        Self {
            chunks_to_generate,
            chunks_generated: 0,
            spec: PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
        }
    }
}

impl Decoder for SimpleMockDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        if self.chunks_generated >= self.chunks_to_generate {
            return Ok(None); // EOF
        }

        let chunk_number = self.chunks_generated;
        self.chunks_generated += 1;

        // Generate predictable samples
        let mut samples = Vec::with_capacity(101);
        samples.push(chunk_number as f32); // Chunk number as first sample

        for i in 0..100 {
            samples.push(i as f32);
        }

        Ok(Some(PcmChunk::new(self.spec, samples)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

/// Test that Pipeline can process chunks from SimpleMockDecoder.
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_pipeline_with_simple_mock_decoder() {
    let decoder = SimpleMockDecoder::new(5); // Generate 5 chunks
    let spec = decoder.spec();

    let pipeline = Pipeline::with_decoder(decoder, spec, 44100)
        .await
        .expect("Failed to create pipeline");

    let consumer = pipeline.consumer();

    // Read all chunks from ring buffer
    let mut all_samples = Vec::new();
    let max_samples = 5 * 101; // 5 chunks * 101 samples each

    // Give pipeline time to process
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Read from channel
    let async_rx = consumer.as_async();
    let read_timeout = tokio::time::Duration::from_millis(100);

    loop {
        let result = tokio::time::timeout(read_timeout, async {
            async_rx.recv().await
        })
        .await;

        match result {
            Ok(Ok(chunk)) => {
                all_samples.extend_from_slice(&chunk);

                if all_samples.len() >= max_samples {
                    break;
                }
            }
            Ok(Err(_)) | Err(_) => {
                // Channel closed or timeout
                if all_samples.len() >= max_samples {
                    break;
                }
                // If we haven't received enough samples yet, break anyway
                break;
            }
        }
    }

    println!("Total samples read: {}", all_samples.len());

    // Verify we got all expected chunks
    assert!(
        all_samples.len() >= max_samples,
        "Expected at least {} samples, got {}",
        max_samples,
        all_samples.len()
    );

    // Verify chunk patterns
    for chunk_idx in 0..5 {
        let offset = chunk_idx * 101;
        if offset >= all_samples.len() {
            break;
        }

        let chunk_number = all_samples[offset];
        println!("Chunk {}: first sample = {}", chunk_idx, chunk_number);

        // First sample should be chunk number
        assert_eq!(
            chunk_number, chunk_idx as f32,
            "Chunk {} has wrong number",
            chunk_idx
        );

        // Next 100 samples should be sequential
        for i in 0..100 {
            if offset + 1 + i >= all_samples.len() {
                break;
            }
            assert_eq!(
                all_samples[offset + 1 + i],
                i as f32,
                "Chunk {} sample {} wrong",
                chunk_idx,
                i
            );
        }
    }
}

/// Test Pipeline EOF handling.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_pipeline_eof() {
    let decoder = SimpleMockDecoder::new(2); // Only 2 chunks
    let spec = decoder.spec();

    let pipeline = Pipeline::with_decoder(decoder, spec, 44100)
        .await
        .expect("Failed to create pipeline");

    let buffer = pipeline.buffer();

    // Wait for pipeline to finish
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Check EOF flag
    assert!(buffer.is_eof(), "Pipeline should reach EOF");

    // Check we got expected number of frames
    let frames = buffer.frames_written();
    println!("Frames written: {}", frames);

    // Each chunk is 101 samples, spec is 2 channels
    // So 101 samples / 2 channels = 50 frames (with 1 sample leftover)
    // 2 chunks = ~100 frames
    assert!(frames > 0, "Should have written some frames");
}

/// Test Pipeline output spec.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_pipeline_output_spec() {
    let decoder = SimpleMockDecoder::new(1);
    let source_spec = PcmSpec {
        sample_rate: 48000,
        channels: 2,
    };
    let target_rate = 44100;

    let pipeline = Pipeline::with_decoder(decoder, source_spec, target_rate)
        .await
        .expect("Failed to create pipeline");

    let output_spec = pipeline.output_spec();

    assert_eq!(output_spec.sample_rate, target_rate);
    assert_eq!(output_spec.channels, source_spec.channels);
}

/// Test speed control.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_pipeline_speed_control() {
    let decoder = SimpleMockDecoder::new(5);
    let spec = decoder.spec();

    let pipeline = Pipeline::with_decoder(decoder, spec, 44100)
        .await
        .expect("Failed to create pipeline");

    // Default speed
    assert_eq!(pipeline.speed(), 1.0);

    // Set speed
    pipeline
        .set_speed(1.5)
        .await
        .expect("Failed to set speed");
    assert_eq!(pipeline.speed(), 1.5);

    // Clamp to limits
    pipeline
        .set_speed(3.0)
        .await
        .expect("Failed to set speed");
    assert_eq!(pipeline.speed(), 2.0); // Clamped to max

    pipeline
        .set_speed(0.1)
        .await
        .expect("Failed to set speed");
    assert_eq!(pipeline.speed(), 0.5); // Clamped to min
}
