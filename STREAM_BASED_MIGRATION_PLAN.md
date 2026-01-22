# Stream-Based Architecture Migration Plan

**Цель:** Перейти от random-access Source trait к stream-based архитектуре с generic metadata messages.

**Требования:**
1. ✅ Stream = Messages с metadata (не просто байты)
2. ✅ Generic first (mockall для тестов)
3. ✅ Переиспользование kithara-worker
4. ✅ ABR must have (сразу)
5. ✅ Metadata: segment_index, variant, offset, codec, container, size, etc.

---

## 1. Generic Message Type

### Core Types (kithara-worker или новый kithara-stream)

```rust
/// Generic stream message with metadata and data
#[derive(Debug, Clone)]
pub struct StreamMessage<M, D> {
    /// Message metadata
    pub meta: M,
    /// Message data (bytes, PCM samples, etc)
    pub data: D,
}

/// Marker traits for metadata
pub trait StreamMetadata: Send + Sync + Clone + 'static {
    /// Unique identifier for this message (for ordering/validation)
    fn sequence_id(&self) -> u64;

    /// Message boundaries (for decoder reinitialization)
    fn is_boundary(&self) -> bool;
}

/// Marker trait for stream data
pub trait StreamData: Send + Sync + 'static {
    type Item;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
}
```

---

## 2. HLS-Specific Types

### HLS Message Metadata

```rust
/// HLS segment metadata (generic over different HLS formats)
#[derive(Debug, Clone)]
pub struct HlsSegmentMetadata {
    /// Variant index
    pub variant: usize,

    /// Segment index (usize::MAX for init segment)
    pub segment_index: usize,

    /// Global byte offset in stream
    pub byte_offset: u64,

    /// Segment URL
    pub url: Url,

    /// Segment duration (for ABR buffer tracking)
    pub duration: Option<Duration>,

    /// Audio codec
    pub codec: Option<AudioCodec>,

    /// Container format (fMP4, MPEG-TS)
    pub container: Option<ContainerFormat>,

    /// Bitrate (from variant)
    pub bitrate: Option<u64>,

    /// Encryption info
    pub encryption: Option<EncryptionInfo>,

    /// Segment boundaries
    pub is_init_segment: bool,
    pub is_segment_start: bool,
    pub is_segment_end: bool,

    /// Variant switch marker (decoder должен reinitialize)
    pub is_variant_switch: bool,
}

impl StreamMetadata for HlsSegmentMetadata {
    fn sequence_id(&self) -> u64 {
        // Combine variant + segment for unique ID
        ((self.variant as u64) << 32) | (self.segment_index as u64)
    }

    fn is_boundary(&self) -> bool {
        self.is_init_segment || self.is_variant_switch
    }
}

/// HLS segment data
impl StreamData for Bytes {
    type Item = u8;
    fn len(&self) -> usize { self.len() }
    fn is_empty(&self) -> bool { self.is_empty() }
}

/// Type alias for HLS stream messages
pub type HlsMessage = StreamMessage<HlsSegmentMetadata, Bytes>;
```

---

## 3. Generic Worker Pattern (kithara-worker enhancement)

### Extend AsyncWorkerSource

```rust
// В kithara-worker/src/traits.rs

/// Source that produces stream messages with metadata
#[async_trait]
pub trait StreamWorkerSource: Send + 'static {
    /// Message type (StreamMessage<M, D>)
    type Message: Send + 'static;

    /// Command type
    type Command: Send + 'static;

    /// Fetch next message
    async fn fetch_next(&mut self) -> Fetch<Self::Message>;

    /// Handle command, return new epoch
    fn handle_command(&mut self, cmd: Self::Command) -> u64;

    /// Current epoch
    fn epoch(&self) -> u64;
}

/// AsyncWorker уже generic, просто расширяем:
/// AsyncWorker<S: StreamWorkerSource> вместо AsyncWorkerSource
```

**Преимущество:** Fetch<T> уже есть в kithara-worker, используем как есть!

---

## 4. HLS Worker (уже реализован!)

### HlsWorkerSource остается почти без изменений

```rust
// В kithara-hls/src/worker/source.rs

impl StreamWorkerSource for HlsWorkerSource {
    type Message = HlsMessage;  // StreamMessage<HlsSegmentMetadata, Bytes>
    type Command = HlsCommand;

    async fn fetch_next(&mut self) -> Fetch<HlsMessage> {
        // Все уже есть! HlsChunk → HlsMessage
        // 1. Load segment
        // 2. ABR decision
        // 3. Return HlsMessage with full metadata

        let meta = HlsSegmentMetadata {
            variant: self.current_variant,
            segment_index: self.current_segment_index,
            byte_offset: self.byte_offset,
            url: segment_url,
            duration: segment_duration,
            codec: variant_meta.codec,
            container: segment_meta.container,
            bitrate: variant_meta.bitrate,
            encryption: segment_meta.encryption,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: variant_switched,
        };

        Fetch::new(
            StreamMessage { meta, data: bytes },
            is_eof,
            self.epoch
        )
    }
}
```

**ABR уже интегрирован!** Просто переименовываем HlsChunk → HlsMessage.

---

## 5. Generic Stream Decoder

### Decoder Trait (mockall-ready)

```rust
// В kithara-decode/src/stream_decoder.rs

/// Generic decoder that processes stream messages
#[cfg_attr(test, automock)]
#[async_trait]
pub trait StreamDecoder<M, D>: Send + Sync
where
    M: StreamMetadata,
    D: StreamData,
{
    /// Output PCM chunk type
    type Output: Send;

    /// Process a stream message
    ///
    /// Decoder checks `meta.is_boundary()` to decide if reinitialization needed
    async fn decode_message(
        &mut self,
        message: StreamMessage<M, D>
    ) -> Result<Self::Output, DecodeError>;

    /// Flush pending data (at EOF)
    async fn flush(&mut self) -> Result<Vec<Self::Output>, DecodeError>;
}

/// HLS-specific decoder
pub struct HlsStreamDecoder<D: AudioDecoder> {
    /// Inner decoder (Symphonia, mock, etc)
    decoder: D,

    /// Resampler (if needed)
    resampler: Option<ResamplerProcessor>,

    /// Current decoder state
    current_codec: Option<AudioCodec>,
    current_sample_rate: u32,
}

#[async_trait]
impl<D: AudioDecoder> StreamDecoder<HlsSegmentMetadata, Bytes> for HlsStreamDecoder<D> {
    type Output = PcmChunk<f32>;

    async fn decode_message(
        &mut self,
        message: HlsMessage
    ) -> Result<PcmChunk<f32>, DecodeError> {
        let meta = &message.meta;

        // Check if reinitialization needed
        if meta.is_boundary() {
            self.reinitialize(meta)?;
        }

        // Decode bytes
        let pcm = self.decoder.decode(&message.data)?;

        // Resample if needed
        if let Some(ref mut resampler) = self.resampler {
            resampler.process(pcm)
        } else {
            Ok(pcm)
        }
    }

    async fn flush(&mut self) -> Result<Vec<PcmChunk<f32>>, DecodeError> {
        // Flush resampler
        if let Some(ref mut resampler) = self.resampler {
            Ok(resampler.flush())
        } else {
            Ok(vec![])
        }
    }
}

impl<D: AudioDecoder> HlsStreamDecoder<D> {
    fn reinitialize(&mut self, meta: &HlsSegmentMetadata) -> Result<(), DecodeError> {
        debug!(
            variant = meta.variant,
            codec = ?meta.codec,
            "Reinitializing decoder for variant switch"
        );

        // Recreate decoder with new codec
        if let Some(codec) = meta.codec {
            self.decoder = D::new_with_codec(codec)?;
            self.current_codec = Some(codec);
        }

        // Update resampler if sample rate changed
        // ...

        Ok(())
    }
}
```

---

## 6. Pipeline Architecture

### Новая архитектура (3 threads, 3 loops)

```rust
// В kithara-decode/src/stream_pipeline.rs

pub struct StreamPipeline<S, D>
where
    S: StreamWorkerSource,
    D: StreamDecoder<S::Message::Meta, S::Message::Data>,
{
    // Worker handle
    worker_task: JoinHandle<()>,

    // Command channel
    cmd_tx: kanal::AsyncSender<S::Command>,

    // PCM buffer
    pcm_buffer: Arc<PcmBuffer>,

    // Consumer handle
    consumer_task: JoinHandle<()>,
}

impl<S, D> StreamPipeline<S, D>
where
    S: StreamWorkerSource,
    D: StreamDecoder</* ... */>,
{
    pub async fn new(
        source: S,
        decoder: D,
    ) -> Result<Self, DecodeError> {
        // 1. Create channels
        let (cmd_tx, cmd_rx) = kanal::bounded_async(16);
        let (msg_tx, msg_rx) = kanal::bounded_async(32);

        // 2. Create worker (HLS fetcher)
        let worker = AsyncWorker::new(source, cmd_rx, msg_tx);
        let worker_task = tokio::spawn(worker.run());

        // 3. Create decoder worker (blocking thread)
        let pcm_buffer = Arc::new(PcmBuffer::new());
        let buffer_clone = pcm_buffer.clone();

        let consumer_task = tokio::task::spawn_blocking(move || {
            Self::decode_loop(msg_rx, decoder, buffer_clone)
        });

        Ok(Self {
            worker_task,
            cmd_tx,
            pcm_buffer,
            consumer_task,
        })
    }

    /// Decode loop (blocking thread)
    fn decode_loop(
        msg_rx: kanal::AsyncReceiver<Fetch<S::Message>>,
        mut decoder: D,
        buffer: Arc<PcmBuffer>,
    ) {
        // Use Handle::block_on for async in blocking context
        let handle = tokio::runtime::Handle::current();

        loop {
            // Receive message
            let fetch = match handle.block_on(msg_rx.recv()) {
                Ok(f) => f,
                Err(_) => break, // Channel closed
            };

            if fetch.is_eof {
                // Flush decoder
                if let Ok(chunks) = handle.block_on(decoder.flush()) {
                    for chunk in chunks {
                        buffer.append(chunk);
                    }
                }
                break;
            }

            // Decode message
            match handle.block_on(decoder.decode_message(fetch.data)) {
                Ok(pcm_chunk) => {
                    buffer.append(pcm_chunk);
                }
                Err(e) => {
                    error!(?e, "Decode error");
                    break;
                }
            }
        }
    }
}
```

---

## 7. Убираем лишние слои

### До (5 циклов):

```
HlsWorkerSource → Fetch<HlsChunk>
    ↓ channel
HlsSourceAdapter → buffered_chunks (Source trait)
    ↓ Source trait
BytePrefetchSource → Fetch<ByteChunk>
    ↓ channel
SyncReader → Read + Seek
    ↓ block_on
DecodeSource → Symphonia
    ↓ channel
Consumer → PcmBuffer
```

### После (3 цикла):

```
HlsWorkerSource → Fetch<HlsMessage>
    ↓ channel (direct!)
HlsStreamDecoder → PcmChunk
    ↓ append
PcmBuffer
    ↓ channel
rodio AudioSyncReader
```

**Убрали:**
- ❌ HlsSourceAdapter (не нужен Source trait)
- ❌ BytePrefetchSource (decoder читает messages напрямую)
- ❌ SyncReader (не нужен Read + Seek)
- ❌ Consumer loop (decoder пишет напрямую)

---

## 8. Testing Strategy (mockall everywhere!)

### Layer 1: HLS Worker (уже есть тесты!)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mockall::mock;

    mock! {
        Loader {}

        #[async_trait]
        impl Loader for Loader {
            async fn load_segment(&self, variant: usize, index: usize)
                -> Result<SegmentMeta, HlsError>;
        }
    }

    #[tokio::test]
    async fn test_abr_variant_switch() {
        let mut mock_loader = MockLoader::new();

        // Setup expectations
        mock_loader.expect_load_segment()
            .returning(|v, i| Ok(SegmentMeta { /* ... */ }));

        let worker = HlsWorkerSource::new(
            Arc::new(mock_loader),
            // ...
        );

        // Test variant switch
        let fetch1 = worker.fetch_next().await;
        assert_eq!(fetch1.data.meta.variant, 0);

        // Trigger ABR switch
        // ...

        let fetch2 = worker.fetch_next().await;
        assert_eq!(fetch2.data.meta.variant, 3);
        assert!(fetch2.data.meta.is_variant_switch);
    }
}
```

### Layer 2: Stream Decoder

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mockall::mock;

    mock! {
        AudioDecoder {}

        impl AudioDecoder for AudioDecoder {
            fn decode(&mut self, bytes: &[u8]) -> Result<PcmChunk<f32>>;
            fn new_with_codec(codec: AudioCodec) -> Result<Self>;
        }
    }

    #[tokio::test]
    async fn test_decoder_reinit_on_variant_switch() {
        let mut mock_decoder = MockAudioDecoder::new();

        // Expect reinitialization on variant switch
        mock_decoder.expect_new_with_codec()
            .with(eq(AudioCodec::AAC))
            .times(1)
            .returning(|_| Ok(MockAudioDecoder::new()));

        let mut stream_decoder = HlsStreamDecoder {
            decoder: mock_decoder,
            resampler: None,
            current_codec: None,
            current_sample_rate: 44100,
        };

        // First message (variant 0, AAC)
        let msg1 = HlsMessage {
            meta: HlsSegmentMetadata {
                variant: 0,
                codec: Some(AudioCodec::AAC),
                is_variant_switch: false,
                // ...
            },
            data: Bytes::from(vec![/* ... */]),
        };

        stream_decoder.decode_message(msg1).await.unwrap();

        // Variant switch message (variant 3, AAC)
        let msg2 = HlsMessage {
            meta: HlsSegmentMetadata {
                variant: 3,
                codec: Some(AudioCodec::AAC),
                is_variant_switch: true,  // BOUNDARY!
                is_init_segment: true,
                // ...
            },
            data: Bytes::from(vec![/* init segment */]),
        };

        // Should trigger reinit
        stream_decoder.decode_message(msg2).await.unwrap();

        // Verify decoder was recreated
        // mockall will verify expect_new_with_codec was called
    }
}
```

### Layer 3: Full Pipeline Integration

```rust
#[tokio::test]
async fn test_pipeline_abr_variant_switch() {
    // Use real TestContext with fixture server
    let ctx = TestContext::new().await;

    // Create real HlsWorkerSource
    let source = create_test_hls_source(&ctx).await;

    // Use MOCK decoder to verify reinit calls
    let mut mock_decoder = MockStreamDecoder::new();
    mock_decoder.expect_decode_message()
        .returning(|msg| {
            // Verify variant switch triggers reinit
            if msg.meta.is_variant_switch {
                assert!(msg.meta.is_init_segment);
            }
            Ok(PcmChunk::empty())
        });

    let pipeline = StreamPipeline::new(source, mock_decoder).await.unwrap();

    // Wait for ABR switch
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Verify decoder saw variant switch message
    // mockall verifies all expectations
}
```

---

## 9. Migration Steps

### Phase 1: Prepare kithara-worker (if needed)

```bash
# Extend StreamWorkerSource trait (alias to AsyncWorkerSource)
# No breaking changes - just add generic type bounds
```

### Phase 2: Rename HlsChunk → HlsMessage

```bash
# In kithara-hls/src/worker/chunk.rs
pub type HlsMessage = StreamMessage<HlsSegmentMetadata, Bytes>;

# Alias for backward compat during migration
#[deprecated = "Use HlsMessage instead"]
pub type HlsChunk = HlsMessage;
```

### Phase 3: Create StreamDecoder trait + HlsStreamDecoder

```bash
# New file: kithara-decode/src/stream_decoder.rs
# Implement trait
# Add mockall support
# Unit tests
```

### Phase 4: Create StreamPipeline

```bash
# New file: kithara-decode/src/stream_pipeline.rs
# 3-loop architecture
# Direct message → decoder
# Integration tests
```

### Phase 5: Deprecate old layers

```bash
# Mark as deprecated:
# - HlsSourceAdapter
# - BytePrefetchSource
# - SyncReader (for HLS, keep for files)
# - Old Pipeline

# Add #[deprecated] attributes
# Update examples to use StreamPipeline
```

### Phase 6: Remove deprecated code

```bash
# After 1-2 releases, remove old code
```

---

## 10. ABR Integration (уже есть!)

### ABR работает на уровне HlsWorkerSource

```rust
// В fetch_next():
let abr_decision = self.abr_controller.decide(
    &self.abr_variants,
    buffer_level,
    Instant::now()
);

if abr_decision.changed {
    let new_variant = abr_decision.target_variant_index;
    self.current_variant = new_variant;

    // Mark next message as variant switch
    self.sent_init_for_variant.remove(&new_variant);

    emit_event(HlsEvent::VariantApplied { ... });
}

// Next fetch_next() will send init segment with is_variant_switch=true
```

**Decoder получает explicit signal** через `meta.is_variant_switch`.

---

## 11. Преимущества новой архитектуры

### ✅ Codec Safety
```rust
if message.meta.is_boundary() {
    decoder.reinitialize(message.meta)?;
}
```
Decoder **знает** когда codec меняется.

### ✅ Generic + Testable
```rust
StreamDecoder<M: StreamMetadata, D: StreamData>
```
Mock любой слой независимо.

### ✅ Переиспользование kithara-worker
```rust
AsyncWorker<HlsWorkerSource>  // Уже работает!
```

### ✅ ABR встроен
```rust
HlsWorkerSource::fetch_next() {
    // ABR decision
    // Mark variant switch in metadata
}
```

### ✅ Меньше копирований
```
HlsMessage (одна аллокация) → Decoder
Вместо: HlsChunk → bytes → ByteChunk → bytes → Decoder
```

### ✅ Простой data flow
```
Worker → channel → Decoder → Buffer
```

---

## 12. Backward Compatibility

### Для простых файлов (MP3, etc)

```rust
// Source trait остается для file-based sources
impl Source for FileSource { /* ... */ }

// SyncReader остается для file playback
let reader = SyncReader::new(file_source);
rodio::Decoder::new(reader)
```

### Для HLS

```rust
// Новый API
let pipeline = StreamPipeline::<HlsWorkerSource, HlsStreamDecoder>::new(
    hls_source,
    decoder,
).await?;
```

---

## 13. Next Steps

1. ✅ Спроектировать generic types → **DONE (этот документ)**
2. ⏭️ Implement StreamDecoder trait в kithara-decode
3. ⏭️ Rename HlsChunk → HlsMessage
4. ⏭️ Implement HlsStreamDecoder
5. ⏭️ Create StreamPipeline
6. ⏭️ Integration tests
7. ⏭️ Update hls_decode example
8. ⏭️ Deprecate old layers

---

## Summary

**Generic Types:**
```rust
StreamMessage<M: StreamMetadata, D: StreamData>
StreamDecoder<M, D>
StreamPipeline<S: StreamWorkerSource, D: StreamDecoder>
```

**HLS Types:**
```rust
HlsMessage = StreamMessage<HlsSegmentMetadata, Bytes>
HlsStreamDecoder<D: AudioDecoder>
```

**Architecture:**
```
3 threads, 3 loops:
1. HlsWorkerSource (tokio task) - fetch + ABR
2. HlsStreamDecoder (blocking) - decode + reinit on boundary
3. rodio (OS thread) - audio output
```

**Result:**
- ✅ ABR variant switch работает
- ✅ Generic + mockall everywhere
- ✅ Переиспользование kithara-worker
- ✅ 40% меньше параллелизма
- ✅ 50% меньше копирований

**Ready to implement?** 🚀
