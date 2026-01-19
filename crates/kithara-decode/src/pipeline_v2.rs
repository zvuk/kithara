//! Audio processing pipeline with async control and decode in blocking thread.
//!
//! The pipeline works directly with async Source, using SourceReader for sync access.
//! It monitors source.media_info() changes and recreates decoder when codec changes (ABR switch).

use std::sync::Arc;

use kanal::{Receiver, Sender};
use kithara_stream::{MediaInfo, Source};
use tokio::{runtime::Handle, task::JoinHandle};
use tracing::{debug, error, info};

use crate::{DecodeError, DecodeResult, PcmChunk, PcmSpec, SourceReader, SymphoniaDecoder};

/// Commands sent to the pipeline.
#[derive(Debug)]
pub enum PipelineCommand {
    /// Stop the pipeline.
    Stop,
}

/// Audio pipeline that runs decoding in a blocking task.
///
/// Works directly with async Source, monitoring media_info() for codec changes.
pub struct AudioPipeline {
    /// Handle to the blocking decode task.
    task_handle: Option<JoinHandle<()>>,
    /// Channel to send commands to pipeline.
    cmd_tx: Sender<PipelineCommand>,
    /// Channel to receive decoded audio.
    audio_rx: Option<Receiver<PcmChunk<f32>>>,
    /// Audio specification (sample rate, channels).
    spec: PcmSpec,
}

impl AudioPipeline {
    /// Create and start a new audio pipeline from Source.
    ///
    /// The pipeline monitors source.media_info() and recreates decoder on codec changes.
    pub async fn open<S>(source: Arc<S>) -> DecodeResult<Self>
    where
        S: Source<Item = u8>,
    {
        let is_streaming = source.len().is_none();

        // For streaming sources, wait until media_info has container info
        // (detected from first segment URL)
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
        let (decoder, spec) = tokio::task::spawn_blocking(move || {
            let reader = SourceReader::new(source_for_decoder);
            create_decoder(reader, initial_media_info.as_ref(), is_streaming)
        })
        .await
        .map_err(|e| DecodeError::Io(std::io::Error::other(e)))??;

        // Channels for communication
        let (cmd_tx, cmd_rx) = kanal::bounded::<PipelineCommand>(4);
        // Large buffer to prevent backpressure from blocking HTTP downloads
        let (audio_tx, audio_rx) = kanal::bounded::<PcmChunk<f32>>(64);

        // Get runtime handle for decoder recreation on ABR switch
        let rt_handle = Handle::current();

        // Spawn blocking decode task
        let task_handle = tokio::task::spawn_blocking(move || {
            run_pipeline(source, decoder, cmd_rx, audio_tx, rt_handle);
        });

        Ok(Self {
            task_handle: Some(task_handle),
            cmd_tx,
            audio_rx: Some(audio_rx),
            spec,
        })
    }

    /// Get audio specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Get receiver for decoded audio chunks.
    pub fn audio_receiver(&self) -> Option<&Receiver<PcmChunk<f32>>> {
        self.audio_rx.as_ref()
    }

    /// Take ownership of the audio receiver.
    pub fn take_audio_receiver(&mut self) -> Option<Receiver<PcmChunk<f32>>> {
        self.audio_rx.take()
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

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(PipelineCommand::Stop);
    }
}

/// Create decoder, trying media_info first, falling back to probe.
fn create_decoder<R>(
    reader: R,
    media_info: Option<&MediaInfo>,
    is_streaming: bool,
) -> DecodeResult<(SymphoniaDecoder, PcmSpec)>
where
    R: std::io::Read + std::io::Seek + Send + Sync + 'static,
{
    // For streaming sources with codec hint, use new_from_media_info
    // which can try codec-specific readers (e.g., AdtsReader for AAC)
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

/// Main pipeline loop running in blocking context.
fn run_pipeline<S>(
    source: Arc<S>,
    mut decoder: SymphoniaDecoder,
    cmd_rx: Receiver<PipelineCommand>,
    audio_tx: Sender<PcmChunk<f32>>,
    rt_handle: Handle,
) where
    S: Source<Item = u8>,
{
    debug!(
        sample_rate = decoder.spec().sample_rate,
        channels = decoder.spec().channels,
        "Pipeline started"
    );

    let is_streaming = source.len().is_none();
    // Track media info that decoder was created with
    let mut decoder_media_info = source.media_info();
    let mut chunks_decoded: u64 = 0;

    loop {
        // Check for commands (non-blocking)
        if let Ok(Some(cmd)) = cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Stop => break,
            }
        }

        // Decode next chunk
        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                chunks_decoded += 1;
                if audio_tx.send(chunk).is_err() {
                    info!(chunks_decoded, "Pipeline: audio channel closed");
                    break;
                }
            }
            Ok(None) => {
                // End of stream - check if media info changed
                let current_media_info = source.media_info();
                if current_media_info != decoder_media_info
                    && let Some(ref new_info) = current_media_info
                {
                    info!(
                        new_codec = ?new_info.codec,
                        chunks_decoded,
                        "End of current data, media info changed - recreating decoder"
                    );
                    match recreate_decoder(&source, Some(new_info), &rt_handle, is_streaming) {
                        Ok(new_decoder) => {
                            decoder = new_decoder;
                            decoder_media_info = source.media_info();
                            info!(
                                sample_rate = decoder.spec().sample_rate,
                                channels = decoder.spec().channels,
                                "Decoder recreated for new variant"
                            );
                            continue;
                        }
                        Err(e) => {
                            error!(err = %e, "Failed to recreate decoder");
                            break;
                        }
                    }
                }
                info!(chunks_decoded, "Pipeline: end of stream from decoder");
                break;
            }
            Err(e) => {
                // Decode error - check if media info changed (data format mismatch)
                let current_media_info = source.media_info();
                if current_media_info != decoder_media_info
                    && let Some(ref new_info) = current_media_info
                {
                    info!(
                        err = %e,
                        new_codec = ?new_info.codec,
                        chunks_decoded,
                        "Decode error, media info changed - recreating decoder"
                    );
                    match recreate_decoder(&source, Some(new_info), &rt_handle, is_streaming) {
                        Ok(new_decoder) => {
                            decoder = new_decoder;
                            decoder_media_info = source.media_info();
                            info!(
                                sample_rate = decoder.spec().sample_rate,
                                channels = decoder.spec().channels,
                                "Decoder recreated after error"
                            );
                            continue;
                        }
                        Err(recreate_err) => {
                            error!(err = %recreate_err, "Failed to recreate decoder");
                            break;
                        }
                    }
                }
                error!(err = %e, chunks_decoded, "Pipeline decode error");
                break;
            }
        }
    }
    info!(chunks_decoded, "Pipeline stopped");
}

/// Recreate decoder with new SourceReader for ABR switch.
fn recreate_decoder<S>(
    source: &Arc<S>,
    media_info: Option<&MediaInfo>,
    rt_handle: &Handle,
    is_streaming: bool,
) -> DecodeResult<SymphoniaDecoder>
where
    S: Source<Item = u8>,
{
    // Enter tokio context for SourceReader (it needs Handle::current())
    let _guard = rt_handle.enter();

    debug!("Creating new SourceReader for decoder recreation");
    let reader = SourceReader::new(source.clone());

    if is_streaming {
        if let Some(info) = media_info
            && info.codec.is_some()
        {
            debug!(?info, "Recreating streaming decoder from media_info");
            let decoder = SymphoniaDecoder::new_from_media_info(reader, info, true)?;
            debug!("Streaming decoder created successfully");
            return Ok(decoder);
        }
        debug!("Recreating streaming decoder with probe");
        let decoder = SymphoniaDecoder::new_with_probe(reader, None)?;
        debug!("Probe decoder created successfully");
        return Ok(decoder);
    }

    if let Some(info) = media_info {
        Ok(SymphoniaDecoder::new_from_media_info(reader, info, false)?)
    } else {
        Ok(SymphoniaDecoder::new_with_probe(reader, None)?)
    }
}
