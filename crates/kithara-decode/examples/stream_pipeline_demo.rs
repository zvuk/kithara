//! Example: Demonstrate stream-based decoding architecture.
//!
//! This shows the new StreamPipeline architecture with:
//! - Generic StreamDecoder trait
//! - Stream messages with metadata (codec, variant, boundaries)
//! - 3-loop architecture: Worker → Decoder → PCM output
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example stream_pipeline_demo
//! ```

use std::error::Error;

use async_trait::async_trait;
use bytes::Bytes;
use kithara_decode::{DecodeResult, PcmChunk, PcmSpec, StreamDecoder, StreamPipeline};
use kithara_stream::{StreamMessage, StreamMetadata};
use kithara_worker::{AsyncWorkerSource, Fetch};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;

/// Example metadata type (simulates HLS segment metadata).
#[derive(Clone, Debug)]
struct ExampleMetadata {
    segment_index: usize,
    variant: usize,
    is_boundary: bool,
}

impl StreamMetadata for ExampleMetadata {
    fn sequence_id(&self) -> u64 {
        ((self.variant as u64) << 32) | (self.segment_index as u64)
    }

    fn is_boundary(&self) -> bool {
        self.is_boundary
    }
}

/// Example worker source that generates test messages.
struct ExampleSource {
    messages: Vec<StreamMessage<ExampleMetadata, Bytes>>,
    index: usize,
    epoch: u64,
}

impl ExampleSource {
    fn new() -> Self {
        let mut messages = vec![];

        // Simulate 3 segments from variant 0
        for i in 0..3 {
            messages.push(StreamMessage::new(
                ExampleMetadata {
                    segment_index: i,
                    variant: 0,
                    is_boundary: i == 0,
                },
                Bytes::from(format!("data_v0_seg{}", i)),
            ));
        }

        // Simulate variant switch to variant 1
        for i in 3..6 {
            messages.push(StreamMessage::new(
                ExampleMetadata {
                    segment_index: i,
                    variant: 1,
                    is_boundary: i == 3, // First segment after switch
                },
                Bytes::from(format!("data_v1_seg{}", i)),
            ));
        }

        Self {
            messages,
            index: 0,
            epoch: 0,
        }
    }
}

#[async_trait]
impl AsyncWorkerSource for ExampleSource {
    type Chunk = StreamMessage<ExampleMetadata, Bytes>;
    type Command = ();

    async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        if self.index >= self.messages.len() {
            let empty_msg = StreamMessage::new(
                ExampleMetadata {
                    segment_index: 999,
                    variant: 0,
                    is_boundary: false,
                },
                Bytes::new(),
            );
            return Fetch::new(empty_msg, true, self.epoch);
        }

        let msg = self.messages[self.index].clone();
        self.index += 1;

        // Simulate processing delay
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Fetch::new(msg, false, self.epoch)
    }

    fn handle_command(&mut self, _cmd: Self::Command) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Example decoder that tracks variant switches and generates PCM.
struct ExampleDecoder {
    decoded_count: usize,
    current_variant: Option<usize>,
    pending: Option<(ExampleMetadata, usize)>,
}

impl ExampleDecoder {
    fn new() -> Self {
        Self {
            decoded_count: 0,
            current_variant: None,
            pending: None,
        }
    }
}

#[async_trait]
impl StreamDecoder<ExampleMetadata, Bytes> for ExampleDecoder {
    type Output = PcmChunk<f32>;

    async fn prepare_message(
        &mut self,
        message: StreamMessage<ExampleMetadata, Bytes>,
    ) -> DecodeResult<()> {
        let (meta, data) = message.into_parts();
        self.pending = Some((meta, data.len()));
        Ok(())
    }

    async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>> {
        let Some((meta, data_len)) = self.pending.take() else {
            return Ok(None);
        };

        self.decoded_count += 1;

        // Detect variant switch
        if let Some(current) = self.current_variant {
            if current != meta.variant {
                info!(
                    from_variant = current,
                    to_variant = meta.variant,
                    "Variant switch detected"
                );
            }
        }
        self.current_variant = Some(meta.variant);

        // Check boundary
        if meta.is_boundary() {
            info!(
                segment = meta.segment_index,
                variant = meta.variant,
                "Boundary detected - reinitializing decoder"
            );
        }

        info!(
            segment = meta.segment_index,
            variant = meta.variant,
            bytes = data_len,
            total_decoded = self.decoded_count,
            "Decoded message"
        );

        // Generate synthetic PCM (in real implementation: decode bytes with Symphonia)
        let spec = PcmSpec {
            sample_rate: 44100,
            channels: 2,
        };

        // Generate 100 samples (50 frames for stereo)
        let pcm = vec![0.1f32 * meta.segment_index as f32; 100];

        Ok(Some(PcmChunk::new(spec, pcm)))
    }

    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
        info!(total_decoded = self.decoded_count, "Flushing decoder");
        Ok(vec![])
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    info!("Starting StreamPipeline demo");

    // Create source and decoder
    let source = ExampleSource::new();
    let decoder = ExampleDecoder::new();

    // Create pipeline (connects worker → decoder → PCM output)
    let pipeline = StreamPipeline::new(source, decoder).await?;

    info!("Pipeline created, processing messages...");

    // Consume PCM chunks from pipeline
    let pcm_rx = pipeline.pcm_rx();
    let mut chunks_received = 0;

    loop {
        match pcm_rx.recv() {
            Ok(chunk) => {
                chunks_received += 1;
                info!(
                    chunk = chunks_received,
                    frames = chunk.frames(),
                    sample_rate = chunk.spec.sample_rate,
                    channels = chunk.spec.channels,
                    "Received PCM chunk"
                );

                // In real application: send to audio output
            }
            Err(_) => {
                info!("PCM channel closed");
                break;
            }
        }
    }

    info!(
        total_chunks = chunks_received,
        "Demo complete, waiting for pipeline"
    );

    // Wait for pipeline to finish
    pipeline.wait().await?;

    info!("Pipeline finished");

    Ok(())
}
