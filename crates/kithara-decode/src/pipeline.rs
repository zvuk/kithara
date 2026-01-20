//! Audio processing pipeline.
//!
//! Combines decoder + resampler + optional FX in a single blocking thread.
//! Produces decoded PCM samples that can be accessed via PcmSource.

use std::{
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

use kithara_stream::{MediaInfo, Source, SyncReader, SyncReaderParams};
use kithara_worker::{SimpleItem, SyncWorker, SyncWorkerSource};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace};

use crate::{
    Decoder, DecodeError, DecodeResult, PcmChunk, PcmSpec, SymphoniaDecoder,
    resampler::ResamplerProcessor,
};

/// Commands for controlling the unified pipeline.
#[derive(Debug, Clone)]
pub enum PipelineCommand {
    /// Stop the pipeline.
    Stop,
    /// Set playback speed (0.5 - 2.0).
    SetSpeed(f32),
}

/// Decode source that processes entire pipeline: decoder → resampler → effects.
///
/// This implements `SyncWorkerSource` and is run by `SyncWorker`.
/// Uses SyncReader which internally has AsyncWorker for byte prefetch.
///
/// Generic over `D: Decoder` to support both production (SymphoniaDecoder)
/// and test (MockDecoder) implementations.
struct DecodeSource<D: Decoder> {
    /// Audio decoder (generic).
    decoder: D,
    /// Resampler processor (optional - None means no resampling).
    resampler: Option<ResamplerProcessor>,
    /// Output specification (target format after resampling).
    output_spec: PcmSpec,
}

impl<D: Decoder> DecodeSource<D> {
    fn new(
        decoder: D,
        source_spec: PcmSpec,
        target_sample_rate: u32,
        speed: Arc<AtomicU32>,
        pool: kithara_bufpool::SharedPool<32, Vec<f32>>,
    ) -> Self {
        // Only create resampler if sample rates differ
        let resampler = if source_spec.sample_rate != target_sample_rate {
            let channels = source_spec.channels as usize;
            Some(ResamplerProcessor::new(
                source_spec.sample_rate,
                target_sample_rate,
                channels,
                speed,
                pool,
            ))
        } else {
            None
        };

        let output_spec = PcmSpec {
            sample_rate: target_sample_rate,
            channels: source_spec.channels,
        };

        Self {
            decoder,
            resampler,
            output_spec,
        }
    }
}

impl<D: Decoder> SyncWorkerSource for DecodeSource<D> {
    type Chunk = PcmChunk<f32>;
    type Command = PipelineCommand;

    fn fetch_next(&mut self) -> Option<Self::Chunk> {
        loop {
            // Decode next chunk (blocking operation, SyncReader is safe in blocking context)
            match self.decoder.next_chunk() {
                Ok(Some(decoded_chunk)) => {
                    trace!(frames = decoded_chunk.frames(), "Decoded chunk");

                    // Resample if resampler exists
                    match &mut self.resampler {
                        Some(resampler) => {
                            if let Some(resampled) = resampler.process(&decoded_chunk) {
                                return Some(resampled);
                            }
                            // Not enough data yet for resampler, continue
                            continue;
                        }
                        None => {
                            // No resampling - return decoded chunk as-is
                            return Some(decoded_chunk);
                        }
                    }
                }
                Ok(None) => {
                    // End of stream - flush resampler if exists
                    if let Some(resampler) = &mut self.resampler {
                        if let Some(final_chunk) = resampler.flush() {
                            info!("Pipeline: flushing resampler");
                            return Some(final_chunk);
                        }
                    }

                    info!("Pipeline: end of stream");
                    return None;
                }
                Err(e) => {
                    error!(err = %e, "Pipeline decode error");
                    return None;
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Self::Command) {
        match cmd {
            PipelineCommand::Stop => {
                debug!("DecodeSource: received Stop command");
            }
            PipelineCommand::SetSpeed(new_speed) => {
                debug!(new_speed, "DecodeSource: speed command processed");
            }
        }
    }

    fn eof_chunk(&self) -> Self::Chunk {
        PcmChunk::new(self.output_spec, Vec::new())
    }
}

/// Unified audio processing pipeline.
///
/// Runs decoder + resampler in a single blocking thread, producing PCM samples
/// that are buffered in memory for random access via PcmSource.
///
/// ## Architecture
///
/// ```text
/// [Byte Source] → [SourceReader] → [Decoder] → [Resampler] → [Buffer]
///                                                                 ↓
///                                                          [PcmSource]
/// ```
///
/// ## Usage
///
/// ```ignore
/// let pipeline = Pipeline::open(source, 44100).await?;
/// let pcm_source = pipeline.pcm_source();
///
/// // Control speed
/// pipeline.set_speed(1.5)?;
///
/// // Read samples
/// let mut buf = vec![0.0f32; 1024];
/// pcm_source.read_at(sample_offset, &mut buf).await?;
/// ```
pub struct Pipeline<D: Decoder = SymphoniaDecoder> {
    /// Handle to the blocking processing task.
    task_handle: Option<JoinHandle<()>>,
    /// Command sender.
    cmd_tx: tokio::sync::mpsc::Sender<PipelineCommand>,
    /// Shared PCM buffer for decoded audio.
    buffer: Arc<PcmBuffer>,
    /// Channel receiver for reading decoded samples (with backpressure).
    sample_rx: kanal::Receiver<Vec<f32>>,
    /// Output specification.
    output_spec: PcmSpec,
    /// Current speed (lock-free, stored as f32 bits in u32).
    speed: Arc<AtomicU32>,
    /// Phantom data to track decoder type.
    _decoder: PhantomData<D>,
}

impl Pipeline<SymphoniaDecoder> {
    /// Create and start a unified pipeline from a byte source using Symphonia decoder.
    ///
    /// # Arguments
    /// - `source`: Byte source (e.g., HTTP stream, file)
    /// - `target_sample_rate`: Target sample rate for output
    ///
    /// The pipeline will decode and resample audio to the target sample rate.
    ///
    /// For custom decoders (e.g., MockDecoder in tests), use `Pipeline::with_decoder()`.
    pub async fn open<S>(
        source: Arc<S>,
        target_sample_rate: u32,
    ) -> DecodeResult<Self>
    where
        S: Source<Item = u8>,
    {
        let is_streaming = source.len().is_none();

        // For streaming sources, wait until media_info is available
        let initial_media_info = if is_streaming {
            let mut attempts = 0;
            loop {
                let info = source.media_info();
                if info.as_ref().is_some_and(|i| i.container.is_some()) {
                    break info;
                }
                attempts += 1;
                if attempts > 100 {
                    break info;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        } else {
            source.media_info()
        };

        // Create SyncReader (must be in tokio runtime context)
        let reader = SyncReader::new(source.clone(), SyncReaderParams::default());

        // Create initial decoder in blocking context
        let (decoder, source_spec) = tokio::task::spawn_blocking(move || {
            create_decoder(reader, initial_media_info.as_ref(), is_streaming)
        })
        .await
        .map_err(|e| DecodeError::Io(std::io::Error::other(e)))??;

        let output_spec = PcmSpec {
            sample_rate: target_sample_rate,
            channels: source_spec.channels,
        };

        // Channels for communication
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<PipelineCommand>(4);
        let (data_tx, data_rx) = kanal::bounded::<SimpleItem<PcmChunk<f32>>>(8);

        // Shared buffer for decoded PCM with sample channel
        let (buffer, sample_rx) = PcmBuffer::new(output_spec);
        let buffer = Arc::new(buffer);

        // Speed control (lock-free)
        let speed = Arc::new(AtomicU32::new(1.0_f32.to_bits()));

        // Create shared buffer pool for resampler
        let pool = kithara_bufpool::SharedPool::<32, Vec<f32>>::new(1024, 32 * 1024);

        // Create decode source
        let decode_source = DecodeSource::new(
            decoder,
            source_spec,
            target_sample_rate,
            speed.clone(),
            pool,
        );

        // Create worker
        let worker = SyncWorker::new(decode_source, cmd_rx, data_tx);

        // Spawn worker in blocking task (log decoder source info too)
        let worker_handle = tokio::task::spawn_blocking(move || {
            debug!("SyncWorker starting");
            worker.run();
            debug!("SyncWorker finished");
        });

        // Spawn consumer in async task
        let buffer_clone = buffer.clone();
        let consumer_handle = tokio::spawn(async move {
            run_consumer_async(data_rx, buffer_clone).await;
        });

        // Combine both handles
        let task_handle = tokio::spawn(async move {
            let _ = tokio::join!(worker_handle, consumer_handle);
        });

        Ok(Self {
            task_handle: Some(task_handle),
            cmd_tx,
            buffer,
            sample_rx,
            output_spec,
            speed,
            _decoder: PhantomData,
        })
    }
}

// Generic impl block for all decoder types
impl<D: Decoder> Pipeline<D> {
    /// Create pipeline with a custom decoder.
    ///
    /// This is used for testing with MockDecoder. Production code should use
    /// `Pipeline::open()` which creates a SymphoniaDecoder automatically.
    ///
    /// # Arguments
    /// - `decoder`: Custom decoder implementation
    /// - `source_spec`: Input PCM specification from decoder
    /// - `target_sample_rate`: Target sample rate for output
    pub async fn with_decoder(
        decoder: D,
        source_spec: PcmSpec,
        target_sample_rate: u32,
    ) -> DecodeResult<Self> {
        let output_spec = PcmSpec {
            sample_rate: target_sample_rate,
            channels: source_spec.channels,
        };

        // Channels for communication
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<PipelineCommand>(4);
        let (data_tx, data_rx) = kanal::bounded::<SimpleItem<PcmChunk<f32>>>(8);

        // Shared buffer for decoded PCM with sample channel
        let (buffer, sample_rx) = PcmBuffer::new(output_spec);
        let buffer = Arc::new(buffer);

        // Speed control (lock-free)
        let speed = Arc::new(AtomicU32::new(1.0_f32.to_bits()));

        // Create shared buffer pool for resampler
        let pool = kithara_bufpool::SharedPool::<32, Vec<f32>>::new(1024, 32 * 1024);

        // Create decode source
        let decode_source = DecodeSource::new(
            decoder,
            source_spec,
            target_sample_rate,
            speed.clone(),
            pool,
        );

        // Create worker
        let worker = SyncWorker::new(decode_source, cmd_rx, data_tx);

        // Spawn worker in blocking task
        let worker_handle = tokio::task::spawn_blocking(move || {
            debug!("SyncWorker starting");
            worker.run();
            debug!("SyncWorker finished");
        });

        // Spawn consumer in async task
        let buffer_clone = buffer.clone();
        let consumer_handle = tokio::spawn(async move {
            run_consumer_async(data_rx, buffer_clone).await;
        });

        // Combine both handles
        let task_handle = tokio::spawn(async move {
            let _ = tokio::join!(worker_handle, consumer_handle);
        });

        Ok(Self {
            task_handle: Some(task_handle),
            cmd_tx,
            buffer,
            sample_rx,
            output_spec,
            speed,
            _decoder: PhantomData,
        })
    }

    /// Get output PCM specification.
    pub fn output_spec(&self) -> PcmSpec {
        self.output_spec
    }

    /// Get current playback speed.
    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    /// Set playback speed (0.5 - 2.0).
    pub async fn set_speed(&self, speed: f32) -> DecodeResult<()> {
        let clamped = speed.clamp(0.5, 2.0);
        self.speed.store(clamped.to_bits(), Ordering::Relaxed);
        self.cmd_tx
            .send(PipelineCommand::SetSpeed(clamped))
            .await
            .map_err(|_| DecodeError::Io(std::io::Error::other("Pipeline stopped")))?;
        Ok(())
    }

    /// Get the PCM buffer for direct access.
    pub fn buffer(&self) -> &Arc<PcmBuffer> {
        &self.buffer
    }

    /// Get the channel receiver for reading decoded sample chunks.
    ///
    /// Returns a clone of the receiver (kanal receivers are clonable).
    /// Each chunk is a Vec<f32> of PCM samples.
    pub fn consumer(&self) -> kanal::Receiver<Vec<f32>> {
        self.sample_rx.clone()
    }

    /// Stop the pipeline.
    pub async fn stop(&self) {
        let _ = self.cmd_tx.send(PipelineCommand::Stop).await;
    }

    /// Wait for pipeline to finish.
    pub async fn wait(mut self) -> DecodeResult<()> {
        if let Some(handle) = self.task_handle.take() {
            handle
                .await
                .map_err(|e| DecodeError::Io(std::io::Error::other(e)))?;
        }
        Ok(())
    }
}

impl<D: Decoder> Drop for Pipeline<D> {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(PipelineCommand::Stop);
    }
}

/// Shared PCM buffer for decoded audio samples.
///
/// Hybrid approach:
/// - Ring buffer for streaming (AudioSyncReader) - limited memory
/// - Vec for random access (PcmSource) - backwards compatible
pub struct PcmBuffer {
    /// Output specification.
    spec: PcmSpec,
    /// All decoded samples for random access (backwards compatible).
    samples: parking_lot::RwLock<Vec<f32>>,
    /// Channel sender for streaming chunks (with backpressure).
    sample_tx: kanal::Sender<Vec<f32>>,
    /// Total frames written.
    frames_written: AtomicU64,
    /// Whether EOF has been reached.
    eof: AtomicBool,
}

impl PcmBuffer {
    /// Create new PCM buffer with hybrid storage.
    ///
    /// Returns (PcmBuffer, receiver for streaming).
    /// Channel capacity: enough for ~10 seconds of audio chunks.
    fn new(spec: PcmSpec) -> (Self, kanal::Receiver<Vec<f32>>) {
        // Channel capacity: ~100 chunks (assuming ~100ms per chunk = 10 seconds total)
        // This provides backpressure when consumer is slow
        let channel_capacity = 100;

        let (sample_tx, sample_rx) = kanal::bounded(channel_capacity);

        let buffer = Self {
            spec,
            samples: parking_lot::RwLock::new(Vec::new()),
            sample_tx,
            frames_written: AtomicU64::new(0),
            eof: AtomicBool::new(false),
        };

        (buffer, sample_rx)
    }

    /// Get PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Get total frames written.
    pub fn frames_written(&self) -> u64 {
        self.frames_written.load(Ordering::Relaxed)
    }

    /// Check if EOF has been reached.
    pub fn is_eof(&self) -> bool {
        self.eof.load(Ordering::Relaxed)
    }

    /// Read samples at given frame offset (for random access).
    ///
    /// Returns number of samples read (not frames).
    pub fn read_samples(&self, frame_offset: u64, buf: &mut [f32]) -> usize {
        let samples = self.samples.read();
        let channels = self.spec.channels as usize;

        let sample_offset = (frame_offset * channels as u64) as usize;
        if sample_offset >= samples.len() {
            return 0;
        }

        let available = samples.len() - sample_offset;
        let to_read = available.min(buf.len());
        buf[..to_read].copy_from_slice(&samples[sample_offset..sample_offset + to_read]);
        to_read
    }

    /// Append a PCM chunk to both Vec and channel.
    ///
    /// Vec: for random access (grows unbounded - backwards compatible).
    /// Channel: for streaming (bounded - provides backpressure).
    ///
    /// IMPORTANT: Uses blocking_send() which blocks if channel is full.
    /// This provides automatic backpressure to the decoder/HLS pipeline.
    fn append(&self, chunk: &PcmChunk<f32>) {
        // Append to Vec for random access
        let mut samples = self.samples.write();
        samples.extend_from_slice(&chunk.pcm);
        drop(samples);

        // Send chunk to channel for streaming (with backpressure)
        // Clone PCM data for channel (Vec is already storing it)
        let chunk_data = chunk.pcm.clone();

        // blocking_send() will block if channel is full -> backpressure!
        if let Err(e) = self.sample_tx.send(chunk_data) {
            trace!(err = %e, "Channel closed, cannot send chunk (receiver dropped)");
        }

        let frames = chunk.frames() as u64;
        self.frames_written.fetch_add(frames, Ordering::Relaxed);
    }

    /// Mark EOF.
    fn set_eof(&self) {
        self.eof.store(true, Ordering::Relaxed);
    }
}

impl crate::traits::PcmBufferTrait for PcmBuffer {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn frames_written(&self) -> u64 {
        self.frames_written.load(Ordering::Relaxed)
    }

    fn is_eof(&self) -> bool {
        self.eof.load(Ordering::Relaxed)
    }

    fn read_samples(&self, frame_offset: u64, buf: &mut [f32]) -> usize {
        self.read_samples(frame_offset, buf)
    }
}

/// Create decoder from source reader and media info.
fn create_decoder<R>(
    reader: R,
    media_info: Option<&MediaInfo>,
    is_streaming: bool,
) -> DecodeResult<(SymphoniaDecoder, PcmSpec)>
where
    R: std::io::Read + std::io::Seek + Send + Sync + 'static,
{
    if is_streaming {
        if let Some(info) = media_info
            && info.codec.is_some()
        {
            debug!(?info, "Creating streaming decoder from media_info");
            let decoder = SymphoniaDecoder::new_from_media_info(reader, info, true)?;
            let spec = decoder.spec();
            return Ok((decoder, spec));
        }
        debug!("Creating streaming decoder with probe");
        let decoder = SymphoniaDecoder::new_with_probe(reader, None)?;
        let spec = decoder.spec();
        return Ok((decoder, spec));
    }

    let decoder = if let Some(info) = media_info {
        SymphoniaDecoder::new_from_media_info(reader, info, is_streaming)?
    } else {
        SymphoniaDecoder::new_with_probe(reader, None)?
    };

    let spec = decoder.spec();
    Ok((decoder, spec))
}

/// Consumer that receives PCM chunks from worker and writes to buffer.
async fn run_consumer_async(
    data_rx: kanal::Receiver<SimpleItem<PcmChunk<f32>>>,
    buffer: Arc<PcmBuffer>,
) {
    debug!("Consumer started");

    let async_rx = data_rx.as_async();

    // Consume all data from channel
    let mut chunks_received = 0;
    loop {
        match async_rx.recv().await {
            Ok(item) => {
                if item.is_eof {
                    debug!("Consumer: received EOF marker");
                    buffer.set_eof();
                    break;
                }

                buffer.append(&item.data);
                chunks_received += 1;
            }
            Err(_) => {
                debug!("Consumer: channel closed");
                break;
            }
        }
    }

    info!(chunks_received, "Consumer stopped");

    // Mark EOF if not already marked
    if !buffer.is_eof() {
        buffer.set_eof();
    }
}

