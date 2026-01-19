//! Audio processing pipeline.
//!
//! Combines decoder + resampler + optional FX in a single blocking thread.
//! Produces decoded PCM samples that can be accessed via PcmSource.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use kanal::{Receiver, Sender};
use kithara_stream::{MediaInfo, Source};
use ringbuf::{HeapRb, traits::{Producer, Split}};
use tokio::{runtime::Handle, task::JoinHandle};
use tracing::{debug, error, info, trace};

use crate::{
    DecodeError, DecodeResult, PcmChunk, PcmSpec, SourceReader, SymphoniaDecoder,
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
pub struct Pipeline {
    /// Handle to the blocking processing task.
    task_handle: Option<JoinHandle<()>>,
    /// Command sender.
    cmd_tx: Sender<PipelineCommand>,
    /// Shared PCM buffer for decoded audio.
    buffer: Arc<PcmBuffer>,
    /// Ring buffer consumer for reading decoded samples.
    consumer: Arc<parking_lot::Mutex<ringbuf::HeapCons<f32>>>,
    /// Output specification.
    output_spec: PcmSpec,
    /// Current speed (lock-free, stored as f32 bits in u32).
    speed: Arc<AtomicU32>,
}

impl Pipeline {
    /// Create and start a unified pipeline from a byte source.
    ///
    /// # Arguments
    /// - `source`: Byte source (e.g., HTTP stream, file)
    /// - `target_sample_rate`: Target sample rate for output
    ///
    /// The pipeline will decode and resample audio to the target sample rate.
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

        // Create initial decoder in blocking context
        let source_for_decoder = source.clone();
        let (decoder, source_spec) = tokio::task::spawn_blocking(move || {
            let reader = SourceReader::new(source_for_decoder);
            create_decoder(reader, initial_media_info.as_ref(), is_streaming)
        })
        .await
        .map_err(|e| DecodeError::Io(std::io::Error::other(e)))??;

        let output_spec = PcmSpec {
            sample_rate: target_sample_rate,
            channels: source_spec.channels,
        };

        // Channels for communication
        let (cmd_tx, cmd_rx) = kanal::bounded::<PipelineCommand>(4);

        // Shared buffer for decoded PCM with ring buffer
        let (buffer, consumer) = PcmBuffer::new(output_spec);
        let buffer = Arc::new(buffer);
        let consumer = Arc::new(parking_lot::Mutex::new(consumer));

        // Speed control (lock-free)
        let speed = Arc::new(AtomicU32::new(1.0_f32.to_bits()));

        // Get runtime handle for decoder recreation
        let rt_handle = Handle::current();

        // Spawn unified processing task
        let buffer_clone = buffer.clone();
        let speed_clone = speed.clone();
        let task_handle = tokio::task::spawn_blocking(move || {
            run_unified_pipeline(
                source,
                decoder,
                source_spec,
                target_sample_rate,
                cmd_rx,
                buffer_clone,
                speed_clone,
                rt_handle,
            );
        });

        Ok(Self {
            task_handle: Some(task_handle),
            cmd_tx,
            buffer,
            consumer,
            output_spec,
            speed,
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
    pub fn set_speed(&self, speed: f32) -> DecodeResult<()> {
        let clamped = speed.clamp(0.5, 2.0);
        self.speed.store(clamped.to_bits(), Ordering::Relaxed);
        self.cmd_tx
            .send(PipelineCommand::SetSpeed(clamped))
            .map_err(|_| DecodeError::Io(std::io::Error::other("Pipeline stopped")))?;
        Ok(())
    }

    /// Get the PCM buffer for direct access.
    pub fn buffer(&self) -> &Arc<PcmBuffer> {
        &self.buffer
    }

    /// Get the ring buffer consumer for reading samples.
    pub fn consumer(&self) -> &Arc<parking_lot::Mutex<ringbuf::HeapCons<f32>>> {
        &self.consumer
    }

    /// Stop the pipeline.
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(PipelineCommand::Stop);
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

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(PipelineCommand::Stop);
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
    /// Ring buffer producer for streaming.
    producer: parking_lot::Mutex<ringbuf::HeapProd<f32>>,
    /// Total frames written.
    frames_written: AtomicU64,
    /// Whether EOF has been reached.
    eof: AtomicBool,
}

impl PcmBuffer {
    /// Create new PCM buffer with hybrid storage.
    ///
    /// Returns (PcmBuffer with producer + Vec, consumer for streaming).
    /// Ring buffer size: 10 seconds of audio at target sample rate.
    fn new(spec: PcmSpec) -> (Self, ringbuf::HeapCons<f32>) {
        // Calculate ring buffer size: 10 seconds of audio
        let buffer_seconds = 10;
        let samples_per_sec = spec.sample_rate as usize * spec.channels as usize;
        let buffer_size = samples_per_sec * buffer_seconds;

        let rb = HeapRb::<f32>::new(buffer_size);
        let (producer, consumer) = rb.split();

        let buffer = Self {
            spec,
            samples: parking_lot::RwLock::new(Vec::new()),
            producer: parking_lot::Mutex::new(producer),
            frames_written: AtomicU64::new(0),
            eof: AtomicBool::new(false),
        };

        (buffer, consumer)
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

    /// Append a PCM chunk to both Vec and ring buffer.
    ///
    /// Vec: for random access (grows unbounded - backwards compatible).
    /// Ring buffer: for streaming (limited size - memory efficient).
    fn append(&self, chunk: &PcmChunk<f32>) {
        // Append to Vec for random access
        let mut samples = self.samples.write();
        samples.extend_from_slice(&chunk.pcm);
        drop(samples);

        // Push to ring buffer for streaming
        let mut producer = self.producer.lock();
        let pushed = producer.push_slice(&chunk.pcm);

        if pushed < chunk.pcm.len() {
            trace!(
                wanted = chunk.pcm.len(),
                pushed,
                "Ring buffer full, some samples dropped (Vec still has all data)"
            );
        }
        drop(producer);

        let frames = chunk.frames() as u64;
        self.frames_written.fetch_add(frames, Ordering::Relaxed);
    }

    /// Mark EOF.
    fn set_eof(&self) {
        self.eof.store(true, Ordering::Relaxed);
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

/// Main unified pipeline loop.
#[allow(clippy::too_many_arguments)]
fn run_unified_pipeline<S>(
    source: Arc<S>,
    mut decoder: SymphoniaDecoder,
    source_spec: PcmSpec,
    target_sample_rate: u32,
    cmd_rx: Receiver<PipelineCommand>,
    buffer: Arc<PcmBuffer>,
    speed: Arc<AtomicU32>,
    rt_handle: Handle,
) where
    S: Source<Item = u8>,
{
    debug!(
        source_rate = source_spec.sample_rate,
        target_rate = target_sample_rate,
        channels = source_spec.channels,
        "Unified pipeline started"
    );

    let is_streaming = source.len().is_none();
    let mut decoder_media_info = source.media_info();

    // Create resampler processor
    let channels = source_spec.channels as usize;
    let mut resampler = ResamplerProcessor::new(
        source_spec.sample_rate,
        target_sample_rate,
        channels,
        speed.clone(),
    );

    let mut chunks_processed: u64 = 0;

    loop {
        // Check for commands (non-blocking)
        if let Ok(Some(cmd)) = cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Stop => {
                    info!(chunks_processed, "Pipeline stopped by command");
                    break;
                }
                PipelineCommand::SetSpeed(new_speed) => {
                    debug!(new_speed, "Speed command processed");
                }
            }
        }

        // Decode next chunk
        match decoder.next_chunk() {
            Ok(Some(decoded_chunk)) => {
                trace!(
                    frames = decoded_chunk.frames(),
                    "Decoded chunk"
                );

                // Resample if needed
                let output_chunk = if let Some(resampled) = resampler.process(&decoded_chunk) {
                    resampled
                } else {
                    // Not enough data yet for resampler, continue
                    continue;
                };

                // Write to buffer
                buffer.append(&output_chunk);
                chunks_processed += 1;
            }
            Ok(None) => {
                // End of stream - check if media info changed (ABR switch)
                let current_media_info = source.media_info();
                if current_media_info != decoder_media_info
                    && let Some(ref new_info) = current_media_info
                {
                    info!(
                        new_codec = ?new_info.codec,
                        chunks_processed,
                        "End of data, media info changed - recreating decoder"
                    );
                    match recreate_decoder(&source, Some(new_info), &rt_handle, is_streaming) {
                        Ok(new_decoder) => {
                            decoder = new_decoder;
                            decoder_media_info = source.media_info();
                            info!("Decoder recreated");
                            continue;
                        }
                        Err(e) => {
                            error!(err = %e, "Failed to recreate decoder");
                            break;
                        }
                    }
                }

                // Flush resampler
                if let Some(final_chunk) = resampler.flush() {
                    buffer.append(&final_chunk);
                }

                info!(chunks_processed, "Pipeline: end of stream");
                buffer.set_eof();
                break;
            }
            Err(e) => {
                // Decode error - check if media info changed
                let current_media_info = source.media_info();
                if current_media_info != decoder_media_info
                    && let Some(ref new_info) = current_media_info
                {
                    info!(
                        err = %e,
                        new_codec = ?new_info.codec,
                        "Decode error, media info changed - recreating decoder"
                    );
                    match recreate_decoder(&source, Some(new_info), &rt_handle, is_streaming) {
                        Ok(new_decoder) => {
                            decoder = new_decoder;
                            decoder_media_info = source.media_info();
                            continue;
                        }
                        Err(recreate_err) => {
                            error!(err = %recreate_err, "Failed to recreate decoder");
                            break;
                        }
                    }
                }
                error!(err = %e, chunks_processed, "Pipeline decode error");
                break;
            }
        }
    }

    info!(chunks_processed, "Unified pipeline stopped");
}

/// Recreate decoder for ABR switch.
fn recreate_decoder<S>(
    source: &Arc<S>,
    media_info: Option<&MediaInfo>,
    rt_handle: &Handle,
    is_streaming: bool,
) -> DecodeResult<SymphoniaDecoder>
where
    S: Source<Item = u8>,
{
    let _guard = rt_handle.enter();
    debug!("Creating new SourceReader for decoder recreation");
    let reader = SourceReader::new(source.clone());

    if is_streaming {
        if let Some(info) = media_info
            && info.codec.is_some()
        {
            debug!(?info, "Recreating streaming decoder from media_info");
            return SymphoniaDecoder::new_from_media_info(reader, info, true);
        }
        debug!("Recreating streaming decoder with probe");
        return SymphoniaDecoder::new_with_probe(reader, None);
    }

    if let Some(info) = media_info {
        SymphoniaDecoder::new_from_media_info(reader, info, false)
    } else {
        SymphoniaDecoder::new_with_probe(reader, None)
    }
}
