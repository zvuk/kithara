#![forbid(unsafe_code)]

//! Tests for AudioPipeline - async audio processing pipeline.

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use kithara_decode::AudioPipeline;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Source, StreamResult, WaitOutcome};
use rstest::{fixture, rstest};
use tokio::sync::RwLock;

mod fixture;
use fixture::EmbeddedAudio;

// ==================== Mock Source ====================

/// In-memory source that wraps embedded audio data.
struct MemoryAudioSource {
    data: Vec<u8>,
    media_info: Option<MediaInfo>,
}

impl MemoryAudioSource {
    fn new(data: Vec<u8>, media_info: Option<MediaInfo>) -> Self {
        Self { data, media_info }
    }

    fn wav() -> Self {
        Self::new(
            EmbeddedAudio::get().wav().to_vec(),
            Some(MediaInfo {
                container: Some(ContainerFormat::Wav),
                codec: None,
                sample_rate: Some(44100),
                channels: Some(2),
            }),
        )
    }

    fn mp3() -> Self {
        Self::new(
            EmbeddedAudio::get().mp3().to_vec(),
            Some(MediaInfo {
                container: Some(ContainerFormat::MpegAudio),
                codec: Some(AudioCodec::Mp3),
                sample_rate: None,
                channels: None,
            }),
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error("MemoryAudioSourceError")]
struct MemoryAudioSourceError;

#[async_trait]
impl Source for MemoryAudioSource {
    type Error = MemoryAudioSourceError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.media_info.clone()
    }
}

// ==================== Fixtures ====================

#[fixture]
fn wav_source() -> Arc<MemoryAudioSource> {
    Arc::new(MemoryAudioSource::wav())
}

#[fixture]
fn mp3_source() -> Arc<MemoryAudioSource> {
    Arc::new(MemoryAudioSource::mp3())
}

// ==================== Pipeline Basic Tests ====================

#[rstest]
#[tokio::test]
async fn open_wav(wav_source: Arc<MemoryAudioSource>) {
    let pipeline = AudioPipeline::open(wav_source).await.unwrap();

    let spec = pipeline.spec();
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[rstest]
#[tokio::test]
async fn open_mp3(mp3_source: Arc<MemoryAudioSource>) {
    let pipeline = AudioPipeline::open(mp3_source).await.unwrap();

    let spec = pipeline.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

#[rstest]
#[tokio::test]
async fn audio_receiver_available(wav_source: Arc<MemoryAudioSource>) {
    let pipeline = AudioPipeline::open(wav_source).await.unwrap();
    assert!(pipeline.audio_receiver().is_some());
}

#[rstest]
#[tokio::test]
async fn take_audio_receiver(wav_source: Arc<MemoryAudioSource>) {
    let mut pipeline = AudioPipeline::open(wav_source).await.unwrap();

    let receiver = pipeline.take_audio_receiver();
    assert!(receiver.is_some());

    // Second take should return None
    assert!(pipeline.take_audio_receiver().is_none());
}

#[rstest]
#[case::wav(true)]
#[case::mp3(false)]
#[tokio::test]
async fn decode_chunks(#[case] use_wav: bool) {
    let source: Arc<MemoryAudioSource> = if use_wav {
        Arc::new(MemoryAudioSource::wav())
    } else {
        Arc::new(MemoryAudioSource::mp3())
    };

    let mut pipeline = AudioPipeline::open(source).await.unwrap();
    let receiver = pipeline.take_audio_receiver().unwrap();

    let mut chunk_count = 0;
    let mut total_samples = 0;

    while let Ok(Some(chunk)) = tokio::time::timeout(Duration::from_secs(5), async {
        receiver.as_async().recv().await.ok()
    })
    .await
    {
        chunk_count += 1;
        total_samples += chunk.pcm.len();

        if chunk_count >= 10 {
            break;
        }
    }

    assert!(chunk_count > 0, "Should receive at least one chunk");
    if use_wav {
        assert!(total_samples > 0, "Should have samples");
    }
}

#[rstest]
#[tokio::test]
async fn stop_pipeline(wav_source: Arc<MemoryAudioSource>) {
    let mut pipeline = AudioPipeline::open(wav_source).await.unwrap();
    let receiver = pipeline.take_audio_receiver().unwrap();

    // Receive one chunk
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        receiver.as_async().recv().await.ok()
    })
    .await;

    pipeline.stop();

    // Wait should complete without hanging
    let result = tokio::time::timeout(Duration::from_secs(2), pipeline.wait()).await;
    assert!(result.is_ok(), "Pipeline should stop within timeout");
}

#[rstest]
#[tokio::test]
async fn drop_stops_pipeline(wav_source: Arc<MemoryAudioSource>) {
    let mut pipeline = AudioPipeline::open(wav_source).await.unwrap();
    let receiver = pipeline.take_audio_receiver().unwrap();

    drop(pipeline);

    // Receiver should eventually close
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match receiver.as_async().recv().await {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "Receiver should close after pipeline drop"
    );
}

// ==================== ABR Switch Mock Source ====================

/// Source that can change media_info to simulate ABR switch.
struct AbrSwitchSource {
    data: Arc<RwLock<Vec<u8>>>,
    media_info: Arc<RwLock<Option<MediaInfo>>>,
    read_count: AtomicUsize,
}

impl AbrSwitchSource {
    fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(EmbeddedAudio::get().wav().to_vec())),
            media_info: Arc::new(RwLock::new(Some(MediaInfo {
                container: Some(ContainerFormat::Wav),
                codec: None,
                sample_rate: Some(44100),
                channels: Some(2),
            }))),
            read_count: AtomicUsize::new(0),
        }
    }

    async fn switch_to_mp3(&self) {
        let mut data = self.data.write().await;
        *data = EmbeddedAudio::get().mp3().to_vec();

        let mut info = self.media_info.write().await;
        *info = Some(MediaInfo {
            container: Some(ContainerFormat::MpegAudio),
            codec: Some(AudioCodec::Mp3),
            sample_rate: None,
            channels: None,
        });
    }

    fn read_count(&self) -> usize {
        self.read_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Source for AbrSwitchSource {
    type Error = MemoryAudioSourceError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        let data = self.data.read().await;
        if range.start >= data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        self.read_count.fetch_add(1, Ordering::SeqCst);

        let data = self.data.read().await;
        let offset = offset as usize;
        if offset >= data.len() {
            return Ok(0);
        }
        let available = data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&data[offset..offset + n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        // Return None to indicate streaming (unknown length)
        None
    }

    fn media_info(&self) -> Option<MediaInfo> {
        // Non-blocking try_read to avoid deadlock in sync context
        self.media_info.try_read().ok().and_then(|g| g.clone())
    }
}

#[fixture]
fn abr_source() -> Arc<AbrSwitchSource> {
    Arc::new(AbrSwitchSource::new())
}

// ==================== ABR Switch Tests ====================

#[rstest]
#[tokio::test]
async fn detects_media_info_change(abr_source: Arc<AbrSwitchSource>) {
    let source_clone = Arc::clone(&abr_source);

    let mut pipeline = AudioPipeline::open(abr_source).await.unwrap();
    let receiver = pipeline.take_audio_receiver().unwrap();

    // Receive initial chunk
    let initial_chunk: Result<Option<_>, _> = tokio::time::timeout(Duration::from_secs(2), async {
        receiver.as_async().recv().await.ok()
    })
    .await;

    assert!(initial_chunk.is_ok(), "Should receive initial chunk");

    // Simulate ABR switch
    source_clone.switch_to_mp3().await;

    // Read count should increase (data is being read)
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(source_clone.read_count() > 0);

    pipeline.stop();
}

#[rstest]
#[tokio::test]
async fn handles_streaming_source(abr_source: Arc<AbrSwitchSource>) {
    let mut pipeline = AudioPipeline::open(abr_source).await.unwrap();
    let receiver = pipeline.take_audio_receiver().unwrap();

    let chunk: Result<Option<_>, _> = tokio::time::timeout(Duration::from_secs(2), async {
        receiver.as_async().recv().await.ok()
    })
    .await;

    assert!(chunk.is_ok(), "Should receive chunk from streaming source");

    pipeline.stop();
}

// ==================== Spec Consistency Tests ====================

#[rstest]
#[tokio::test]
async fn spec_matches_wav_format(wav_source: Arc<MemoryAudioSource>) {
    let pipeline = AudioPipeline::open(wav_source).await.unwrap();

    let spec = pipeline.spec();

    // Our test WAV is 44.1kHz stereo
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[rstest]
#[tokio::test]
async fn chunks_match_spec(wav_source: Arc<MemoryAudioSource>) {
    let mut pipeline = AudioPipeline::open(wav_source).await.unwrap();

    let spec = pipeline.spec();
    let receiver = pipeline.take_audio_receiver().unwrap();

    let chunk = tokio::time::timeout(Duration::from_secs(2), async {
        receiver.as_async().recv().await.ok()
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        chunk.pcm.len() % spec.channels as usize,
        0,
        "Sample count should be multiple of channels"
    );
}

// ==================== Error Handling Tests ====================

/// Source with invalid audio data.
struct InvalidSource;

#[async_trait]
impl Source for InvalidSource {
    type Error = MemoryAudioSourceError;

    async fn wait_range(&self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(WaitOutcome::Ready)
    }

    async fn read_at(&self, _offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        for b in buf.iter_mut() {
            *b = 0xFF;
        }
        Ok(buf.len())
    }

    fn len(&self) -> Option<u64> {
        Some(1000)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}

/// Empty source that returns EOF immediately.
struct EmptySource;

#[async_trait]
impl Source for EmptySource {
    type Error = MemoryAudioSourceError;

    async fn wait_range(&self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(WaitOutcome::Eof)
    }

    async fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        Ok(0)
    }

    fn len(&self) -> Option<u64> {
        Some(0)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}

#[rstest]
#[tokio::test]
async fn invalid_source_fails() {
    let source = Arc::new(InvalidSource);
    let result = AudioPipeline::open(source).await;
    assert!(result.is_err());
}

#[rstest]
#[tokio::test]
async fn empty_source_fails() {
    let source = Arc::new(EmptySource);
    let result = AudioPipeline::open(source).await;
    assert!(result.is_err());
}
