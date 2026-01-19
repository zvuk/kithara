//! ResamplerPipeline: async-friendly resampler between decoder and audio output.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use kanal::{Receiver, Sender};
use tokio::task::JoinHandle;
use tracing::{debug, info, trace};

use super::processor::ResamplerProcessor;
use crate::{PcmChunk, PcmSpec};

/// Commands sent to the resampler pipeline.
#[derive(Debug)]
pub enum ResamplerCommand {
    Stop,
}

/// Resampler pipeline between decoder and audio output.
///
/// Runs resampling in a blocking task, supporting:
/// - Sample rate conversion (e.g., 48kHz -> 44.1kHz)
/// - Playback speed control (0.5x - 2.0x)
/// - Passthrough when no resampling needed
pub struct ResamplerPipeline {
    task_handle: Option<JoinHandle<()>>,
    cmd_tx: Sender<ResamplerCommand>,
    speed: Arc<AtomicU32>,
    audio_rx: Option<Receiver<PcmChunk<f32>>>,
    output_spec: PcmSpec,
}

impl ResamplerPipeline {
    /// Create a new resampler pipeline.
    ///
    /// # Arguments
    /// - `audio_rx`: Receiver for decoded audio from AudioPipeline
    /// - `source_spec`: Audio spec from decoder (source sample rate)
    /// - `target_sample_rate`: Target sample rate for output device
    pub fn new(
        audio_rx: Receiver<PcmChunk<f32>>,
        source_spec: PcmSpec,
        target_sample_rate: u32,
    ) -> Self {
        let speed = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let (cmd_tx, cmd_rx) = kanal::bounded::<ResamplerCommand>(4);
        // Larger buffer to handle speed changes (0.5x produces 2x chunks)
        let (output_tx, output_rx) = kanal::bounded::<PcmChunk<f32>>(32);

        let output_spec = PcmSpec {
            sample_rate: target_sample_rate,
            channels: source_spec.channels,
        };

        let speed_clone = speed.clone();
        let source_rate = source_spec.sample_rate;
        let channels = source_spec.channels as usize;

        let task_handle = tokio::task::spawn_blocking(move || {
            run_resampler(
                audio_rx,
                output_tx,
                cmd_rx,
                source_rate,
                target_sample_rate,
                channels,
                speed_clone,
            );
        });

        Self {
            task_handle: Some(task_handle),
            cmd_tx,
            speed,
            audio_rx: Some(output_rx),
            output_spec,
        }
    }

    /// Take ownership of the audio receiver.
    pub fn take_audio_receiver(&mut self) -> Option<Receiver<PcmChunk<f32>>> {
        self.audio_rx.take()
    }

    /// Get receiver reference for decoded audio chunks.
    pub fn audio_receiver(&self) -> Option<&Receiver<PcmChunk<f32>>> {
        self.audio_rx.as_ref()
    }

    /// Set playback speed factor (0.5 - 2.0).
    ///
    /// This is lock-free and can be called from any thread.
    pub fn set_speed(&self, factor: f32) {
        let clamped = factor.clamp(0.5, 2.0);
        self.speed.store(clamped.to_bits(), Ordering::Relaxed);
        debug!(speed = clamped, "Speed updated");
    }

    /// Get current playback speed.
    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    /// Get output audio specification.
    pub fn output_spec(&self) -> PcmSpec {
        self.output_spec
    }

    /// Stop the resampler pipeline.
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(ResamplerCommand::Stop);
    }

    /// Wait for pipeline to finish.
    pub async fn wait(mut self) -> crate::DecodeResult<()> {
        if let Some(handle) = self.task_handle.take() {
            handle
                .await
                .map_err(|e| crate::DecodeError::Io(std::io::Error::other(e)))?;
        }
        Ok(())
    }
}

impl Drop for ResamplerPipeline {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ResamplerCommand::Stop);
    }
}

fn run_resampler(
    audio_rx: Receiver<PcmChunk<f32>>,
    output_tx: Sender<PcmChunk<f32>>,
    cmd_rx: Receiver<ResamplerCommand>,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    speed: Arc<AtomicU32>,
) {
    debug!(
        source_rate,
        target_rate, channels, "Resampler pipeline started"
    );

    let mut processor = ResamplerProcessor::new(source_rate, target_rate, channels, speed);
    let mut chunks_processed: u64 = 0;

    loop {
        if let Ok(Some(cmd)) = cmd_rx.try_recv() {
            match cmd {
                ResamplerCommand::Stop => break,
            }
        }

        match audio_rx.recv() {
            Ok(chunk) => {
                trace!(
                    samples = chunk.pcm.len(),
                    frames = chunk.frames(),
                    "Resampler received chunk"
                );

                if let Some(output) = processor.process(&chunk) {
                    chunks_processed += 1;
                    if output_tx.send(output).is_err() {
                        info!(chunks_processed, "Resampler: output channel closed");
                        break;
                    }
                }
            }
            Err(_) => {
                // Input channel closed - flush remaining data
                debug!(
                    chunks_processed,
                    "Resampler: input channel closed, flushing"
                );
                if let Some(output) = processor.flush() {
                    chunks_processed += 1;
                    let _ = output_tx.send(output);
                }
                break;
            }
        }
    }

    info!(chunks_processed, "Resampler pipeline stopped");
}
