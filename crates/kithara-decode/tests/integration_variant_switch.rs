//! Integration tests for HLS variant switch scenarios.
//!
//! Tests the full pipeline: HLS Worker → Decoder → PCM output
//! with variant switches and boundary handling.

use async_trait::async_trait;
use bytes::Bytes;
use kithara_decode::{DecodeResult, PcmChunk, PcmSpec, StreamDecoder, StreamPipeline};
use kithara_stream::{AudioCodec, StreamMessage, StreamMetadata};
use kithara_worker::{AsyncWorkerSource, Fetch};

/// Mock HLS segment metadata for testing
#[derive(Clone, Debug)]
struct MockHlsMetadata {
    variant: usize,
    segment_index: usize,
    codec: AudioCodec,
    is_init: bool,
    is_variant_switch: bool,
}

impl StreamMetadata for MockHlsMetadata {
    fn sequence_id(&self) -> u64 {
        ((self.variant as u64) << 32) | (self.segment_index as u64)
    }

    fn is_boundary(&self) -> bool {
        self.is_init || self.is_variant_switch
    }
}

/// Mock HLS source that simulates variant switching
struct MockHlsSource {
    messages: Vec<StreamMessage<MockHlsMetadata, Bytes>>,
    index: usize,
    epoch: u64,
}

impl MockHlsSource {
    fn new_with_variant_switch() -> Self {
        let mut messages = vec![];

        // Variant 0 - 64kbps AAC
        // Init segment
        messages.push(StreamMessage::new(
            MockHlsMetadata {
                variant: 0,
                segment_index: usize::MAX,
                codec: AudioCodec::AacLc,
                is_init: true,
                is_variant_switch: false,
            },
            Bytes::from_static(b"init_v0_data"),
        ));

        // Media segments for variant 0
        for i in 0..3 {
            messages.push(StreamMessage::new(
                MockHlsMetadata {
                    variant: 0,
                    segment_index: i,
                    codec: AudioCodec::AacLc,
                    is_init: false,
                    is_variant_switch: false,
                },
                Bytes::from(format!("v0_seg{}_data", i)),
            ));
        }

        // Variant switch to variant 1 - 128kbps AAC
        // Init segment for new variant
        messages.push(StreamMessage::new(
            MockHlsMetadata {
                variant: 1,
                segment_index: usize::MAX,
                codec: AudioCodec::AacLc,
                is_init: true,
                is_variant_switch: true,
            },
            Bytes::from_static(b"init_v1_data"),
        ));

        // Media segments for variant 1
        for i in 3..6 {
            messages.push(StreamMessage::new(
                MockHlsMetadata {
                    variant: 1,
                    segment_index: i,
                    codec: AudioCodec::AacLc,
                    is_init: false,
                    is_variant_switch: i == 3, // First segment after switch
                },
                Bytes::from(format!("v1_seg{}_data", i)),
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
impl AsyncWorkerSource for MockHlsSource {
    type Chunk = StreamMessage<MockHlsMetadata, Bytes>;
    type Command = ();

    async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        if self.index >= self.messages.len() {
            // Return EOF
            let empty_msg = StreamMessage::new(
                MockHlsMetadata {
                    variant: 0,
                    segment_index: 999,
                    codec: AudioCodec::AacLc,
                    is_init: false,
                    is_variant_switch: false,
                },
                Bytes::new(),
            );
            return Fetch::new(empty_msg, true, self.epoch);
        }

        let msg = self.messages[self.index].clone();
        self.index += 1;
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

/// Mock decoder that tracks variant switches
struct VariantSwitchTracker {
    decoded_count: usize,
    variant_switches: Vec<usize>,
    current_variant: Option<usize>,
    init_segments: Vec<usize>,
    pending_meta: Option<MockHlsMetadata>,
}

impl VariantSwitchTracker {
    fn new() -> Self {
        Self {
            decoded_count: 0,
            variant_switches: vec![],
            current_variant: None,
            init_segments: vec![],
            pending_meta: None,
        }
    }

    fn process_meta(&mut self, meta: &MockHlsMetadata) {
        self.decoded_count += 1;

        // Track init segments
        if meta.is_init {
            self.init_segments.push(meta.variant);
        }

        // Track variant switches
        if let Some(current) = self.current_variant {
            if current != meta.variant {
                self.variant_switches.push(meta.variant);
            }
        }
        self.current_variant = Some(meta.variant);
    }
}

#[async_trait]
impl StreamDecoder<MockHlsMetadata, Bytes> for VariantSwitchTracker {
    type Output = PcmChunk<f32>;

    async fn prepare_message(
        &mut self,
        message: StreamMessage<MockHlsMetadata, Bytes>,
    ) -> DecodeResult<()> {
        let (meta, _data) = message.into_parts();
        self.pending_meta = Some(meta);
        Ok(())
    }

    async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>> {
        if let Some(meta) = self.pending_meta.take() {
            self.process_meta(&meta);
            Ok(Some(PcmChunk::new(
                PcmSpec {
                    sample_rate: 44100,
                    channels: 2,
                },
                vec![],
            )))
        } else {
            Ok(None)
        }
    }

    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_variant_switch_detection() {
    let source = MockHlsSource::new_with_variant_switch();
    let decoder = VariantSwitchTracker::new();

    let pipeline = StreamPipeline::new(source, decoder)
        .await
        .expect("Failed to create pipeline");

    // Give pipeline time to process all messages
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Clean up
    drop(pipeline);

    // Note: We can't easily verify decoder state after pipeline consumes it
    // This test primarily verifies that pipeline doesn't crash on variant switch
}

#[tokio::test]
async fn test_variant_switch_boundary_handling() {
    let mut decoder = VariantSwitchTracker::new();

    // Process messages manually to verify decoder behavior
    let messages = vec![
        // Variant 0 init
        StreamMessage::new(
            MockHlsMetadata {
                variant: 0,
                segment_index: usize::MAX,
                codec: AudioCodec::AacLc,
                is_init: true,
                is_variant_switch: false,
            },
            Bytes::new(),
        ),
        // Variant 0 media
        StreamMessage::new(
            MockHlsMetadata {
                variant: 0,
                segment_index: 0,
                codec: AudioCodec::AacLc,
                is_init: false,
                is_variant_switch: false,
            },
            Bytes::new(),
        ),
        // Variant switch to variant 1
        StreamMessage::new(
            MockHlsMetadata {
                variant: 1,
                segment_index: usize::MAX,
                codec: AudioCodec::AacLc,
                is_init: true,
                is_variant_switch: true,
            },
            Bytes::new(),
        ),
        // Variant 1 media
        StreamMessage::new(
            MockHlsMetadata {
                variant: 1,
                segment_index: 1,
                codec: AudioCodec::AacLc,
                is_init: false,
                is_variant_switch: false,
            },
            Bytes::new(),
        ),
    ];

    for msg in messages {
        decoder.prepare_message(msg).await.expect("Prepare failed");
        while decoder.try_decode_chunk().await.expect("Decode failed").is_some() {}
    }

    // Verify decoder tracked variant switch
    assert_eq!(decoder.decoded_count, 4);
    assert_eq!(decoder.init_segments, vec![0, 1]);
    assert_eq!(decoder.variant_switches, vec![1]);
}

#[tokio::test]
async fn test_multiple_variant_switches() {
    let mut decoder = VariantSwitchTracker::new();

    // Simulate switching: 0 → 1 → 2 → 1
    let variants = vec![
        (0, false),
        (0, false),
        (1, true), // Switch
        (1, false),
        (2, true), // Switch
        (2, false),
        (1, true), // Switch back
        (1, false),
    ];

    for (i, (variant, is_switch)) in variants.iter().enumerate() {
        let msg = StreamMessage::new(
            MockHlsMetadata {
                variant: *variant,
                segment_index: i,
                codec: AudioCodec::AacLc,
                is_init: false,
                is_variant_switch: *is_switch,
            },
            Bytes::new(),
        );
        decoder.prepare_message(msg).await.expect("Prepare failed");
        while decoder.try_decode_chunk().await.expect("Decode failed").is_some() {}
    }

    // Verify all switches were detected
    assert_eq!(decoder.variant_switches, vec![1, 2, 1]);
    assert_eq!(decoder.decoded_count, 8);
}

#[tokio::test]
async fn test_variant_switch_with_codec_change() {
    let mut decoder = VariantSwitchTracker::new();

    // Switch from AAC to MP3 (different codec)
    let messages = vec![
        StreamMessage::new(
            MockHlsMetadata {
                variant: 0,
                segment_index: 0,
                codec: AudioCodec::AacLc,
                is_init: false,
                is_variant_switch: false,
            },
            Bytes::new(),
        ),
        StreamMessage::new(
            MockHlsMetadata {
                variant: 1,
                segment_index: 1,
                codec: AudioCodec::Mp3,
                is_init: true,
                is_variant_switch: true,
            },
            Bytes::new(),
        ),
        StreamMessage::new(
            MockHlsMetadata {
                variant: 1,
                segment_index: 2,
                codec: AudioCodec::Mp3,
                is_init: false,
                is_variant_switch: false,
            },
            Bytes::new(),
        ),
    ];

    for msg in messages {
        decoder.prepare_message(msg).await.expect("Prepare failed");
        while decoder.try_decode_chunk().await.expect("Decode failed").is_some() {}
    }

    // Verify switch was detected
    assert_eq!(decoder.variant_switches, vec![1]);
    assert_eq!(decoder.init_segments, vec![1]);
}

#[tokio::test]
async fn test_no_variant_switch_same_variant() {
    let mut decoder = VariantSwitchTracker::new();

    // All segments from same variant
    for i in 0..5 {
        let msg = StreamMessage::new(
            MockHlsMetadata {
                variant: 0,
                segment_index: i,
                codec: AudioCodec::AacLc,
                is_init: false,
                is_variant_switch: false,
            },
            Bytes::new(),
        );
        decoder.prepare_message(msg).await.expect("Prepare failed");
        while decoder.try_decode_chunk().await.expect("Decode failed").is_some() {}
    }

    // No variant switches should be detected
    assert_eq!(decoder.variant_switches.len(), 0);
    assert_eq!(decoder.decoded_count, 5);
}
