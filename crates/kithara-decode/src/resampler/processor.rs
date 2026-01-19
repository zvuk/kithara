//! ResamplerProcessor: wraps rubato for sample rate conversion and speed control.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use tracing::{debug, trace};

use crate::{PcmChunk, PcmSpec};

const SPEED_EPSILON: f32 = 0.0001;
const DEFAULT_CHUNK_SIZE: usize = 1024;

/// Internal processor that handles resampling logic.
pub(crate) struct ResamplerProcessor {
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    speed: Arc<AtomicU32>,
    resampler: Option<SincFixedIn<f32>>,
    current_speed: f32,
    // Accumulated input buffer (planar format) - keeps leftover samples between process() calls
    input_buffer: Vec<Vec<f32>>,
    output_spec: PcmSpec,
    // Reusable temporary buffers to avoid allocations on each process() call
    temp_input_slice: Vec<Vec<f32>>,
    temp_output_bufs: Vec<Vec<f32>>,
    temp_output_all: Vec<Vec<f32>>,
}

impl ResamplerProcessor {
    pub fn new(source_rate: u32, target_rate: u32, channels: usize, speed: Arc<AtomicU32>) -> Self {
        let current_speed = f32::from_bits(speed.load(Ordering::Relaxed));
        let output_spec = PcmSpec {
            sample_rate: target_rate,
            channels: channels as u16,
        };

        let mut processor = Self {
            source_rate,
            target_rate,
            channels,
            speed,
            resampler: None,
            current_speed,
            input_buffer: vec![Vec::new(); channels],
            output_spec,
            temp_input_slice: Vec::with_capacity(channels),
            temp_output_bufs: Vec::with_capacity(channels),
            temp_output_all: vec![Vec::new(); channels],
        };

        processor.update_resampler_if_needed(current_speed);
        processor
    }

    pub fn process(&mut self, chunk: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        // Check if source spec changed (ABR switch)
        let chunk_rate = chunk.spec.sample_rate;
        let chunk_channels = chunk.spec.channels as usize;

        if chunk_rate != self.source_rate || chunk_channels != self.channels {
            debug!(
                old_rate = self.source_rate,
                new_rate = chunk_rate,
                old_channels = self.channels,
                new_channels = chunk_channels,
                "Source spec changed, recreating resampler"
            );
            self.source_rate = chunk_rate;
            if chunk_channels != self.channels {
                self.channels = chunk_channels;
                self.input_buffer = vec![Vec::new(); chunk_channels];
                self.output_spec.channels = chunk_channels as u16;
            } else {
                // Clear buffers on rate change
                for buf in &mut self.input_buffer {
                    buf.clear();
                }
            }
            // Force resampler recreation
            self.resampler = None;
        }

        let speed = self.read_speed();
        self.update_resampler_if_needed(speed);

        if self.is_passthrough() {
            trace!(
                speed,
                source_rate = self.source_rate,
                target_rate = self.target_rate,
                "Passthrough mode"
            );
            return Some(PcmChunk::new(self.output_spec, chunk.pcm.clone()));
        }

        trace!(
            speed,
            source_rate = self.source_rate,
            target_rate = self.target_rate,
            input_frames = chunk.frames(),
            "Resampling"
        );
        self.resample(chunk)
    }

    fn read_speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    fn is_passthrough(&self) -> bool {
        self.resampler.is_none()
    }

    fn should_passthrough(source_rate: u32, target_rate: u32, speed: f32) -> bool {
        source_rate == target_rate && (speed - 1.0).abs() < SPEED_EPSILON
    }

    fn update_resampler_if_needed(&mut self, speed: f32) {
        let should_pt = Self::should_passthrough(self.source_rate, self.target_rate, speed);
        let currently_pt = self.is_passthrough();
        let speed_changed = (speed - self.current_speed).abs() > SPEED_EPSILON;

        if should_pt && !currently_pt {
            debug!(speed, "Switching to passthrough mode");
            self.resampler = None;
            self.current_speed = speed;
            // Clear accumulated buffer
            for buf in &mut self.input_buffer {
                buf.clear();
            }
        } else if !should_pt && (currently_pt || speed_changed) {
            let resample_ratio = self.calculate_resample_ratio(speed);
            debug!(
                speed,
                resample_ratio,
                source_rate = self.source_rate,
                target_rate = self.target_rate,
                "Creating resampler"
            );

            // Lighter parameters for real-time processing
            let params = SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };

            match SincFixedIn::<f32>::new(
                resample_ratio,
                2.0, // max relative ratio change
                params,
                DEFAULT_CHUNK_SIZE,
                self.channels,
            ) {
                Ok(resampler) => {
                    self.resampler = Some(resampler);
                    self.current_speed = speed;
                }
                Err(e) => {
                    debug!(err = %e, "Failed to create resampler, staying in current mode");
                }
            }
        } else if !should_pt && !currently_pt && speed_changed {
            let resample_ratio = self.calculate_resample_ratio(speed);
            if let Some(ref mut resampler) = self.resampler {
                if let Err(e) = resampler.set_resample_ratio(resample_ratio, false) {
                    debug!(err = %e, "Failed to update ratio");
                } else {
                    debug!(speed, resample_ratio, "Updated resample ratio");
                    self.current_speed = speed;
                }
            }
        }
    }

    fn calculate_resample_ratio(&self, speed: f32) -> f64 {
        // For SincFixedIn, resample_ratio = fs_out / fs_in
        // To achieve speed control: output_samples = input_samples / speed
        // So: resample_ratio = (target_rate / source_rate) / speed
        let base_ratio = self.target_rate as f64 / self.source_rate as f64;
        base_ratio / speed as f64
    }

    fn resample(&mut self, chunk: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        self.resampler.as_ref()?;

        // Append new data to accumulated buffer
        self.append_to_buffer(&chunk.pcm);

        let resampler = self.resampler.as_mut()?;
        let input_frames = resampler.input_frames_next();
        let channels = self.channels;

        // Clear and prepare temporary buffers for reuse
        for buf in &mut self.temp_output_all {
            buf.clear();
        }

        // Process all complete chunks from accumulated buffer
        while self.input_buffer[0].len() >= input_frames {
            // Reuse temp_input_slice - resize instead of creating new Vec
            if self.temp_input_slice.len() < channels {
                self.temp_input_slice.resize_with(channels, Vec::new);
            }
            for ch in 0..channels {
                self.temp_input_slice[ch].clear();
                self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
            }

            let input_refs: Vec<&[f32]> = self.temp_input_slice.iter().map(|v| v.as_slice()).collect();
            let output_frames = resampler.output_frames_next();

            // Reuse temp_output_bufs - resize instead of creating new Vec
            if self.temp_output_bufs.len() < channels {
                self.temp_output_bufs.resize_with(channels, Vec::new);
            }
            for ch in 0..channels {
                self.temp_output_bufs[ch].resize(output_frames, 0.0);
            }
            let mut output_refs: Vec<&mut [f32]> =
                self.temp_output_bufs.iter_mut().map(|v| v.as_mut_slice()).collect();

            match resampler.process_into_buffer(&input_refs, &mut output_refs, None) {
                Ok((_, out_len)) => {
                    for (ch, buf) in self.temp_output_bufs.iter().enumerate() {
                        self.temp_output_all[ch].extend_from_slice(&buf[..out_len]);
                    }
                    // Remove processed samples from buffer
                    for buf in &mut self.input_buffer {
                        buf.drain(..input_frames);
                    }
                }
                Err(e) => {
                    trace!(err = %e, "Resampler error");
                    return None;
                }
            }
        }

        if self.temp_output_all[0].is_empty() {
            // Not enough data yet, return None (will accumulate more)
            trace!(
                buffered = self.input_buffer[0].len(),
                needed = input_frames,
                "Accumulating data"
            );
            return None;
        }

        let interleaved = self.interleave(&self.temp_output_all);
        Some(PcmChunk::new(self.output_spec, interleaved))
    }

    fn append_to_buffer(&mut self, interleaved: &[f32]) {
        for (i, sample) in interleaved.iter().enumerate() {
            let ch = i % self.channels;
            self.input_buffer[ch].push(*sample);
        }
    }

    fn interleave(&self, planar: &[Vec<f32>]) -> Vec<f32> {
        if planar.is_empty() || planar[0].is_empty() {
            return Vec::new();
        }

        let frames = planar[0].len();
        let mut result = Vec::with_capacity(frames * self.channels);

        for frame_idx in 0..frames {
            for channel in planar.iter().take(self.channels) {
                result.push(channel[frame_idx]);
            }
        }

        result
    }

    /// Flush remaining data from buffer (called at end of stream).
    /// Pads with zeros if needed to complete the final resampler block.
    pub fn flush(&mut self) -> Option<PcmChunk<f32>> {
        let resampler = self.resampler.as_mut()?;

        if self.input_buffer[0].is_empty() {
            return None;
        }

        let input_frames = resampler.input_frames_next();
        let channels = self.channels;
        let buffered = self.input_buffer[0].len();

        // Pad with zeros to reach input_frames
        let padding_needed = input_frames.saturating_sub(buffered);
        for buf in &mut self.input_buffer {
            buf.extend(std::iter::repeat_n(0.0, padding_needed));
        }

        debug!(buffered, padding_needed, "Flushing resampler buffer");

        // Reuse temp_input_slice - resize instead of creating new Vec
        if self.temp_input_slice.len() < channels {
            self.temp_input_slice.resize_with(channels, Vec::new);
        }
        for ch in 0..channels {
            self.temp_input_slice[ch].clear();
            self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
        }

        let input_refs: Vec<&[f32]> = self.temp_input_slice.iter().map(|v| v.as_slice()).collect();
        let output_frames = resampler.output_frames_next();

        // Reuse temp_output_bufs - resize instead of creating new Vec
        if self.temp_output_bufs.len() < channels {
            self.temp_output_bufs.resize_with(channels, Vec::new);
        }
        for ch in 0..channels {
            self.temp_output_bufs[ch].resize(output_frames, 0.0);
        }
        let mut output_refs: Vec<&mut [f32]> =
            self.temp_output_bufs.iter_mut().map(|v| v.as_mut_slice()).collect();

        match resampler.process_into_buffer(&input_refs, &mut output_refs, None) {
            Ok((_, out_len)) => {
                // Clear buffer
                for buf in &mut self.input_buffer {
                    buf.clear();
                }

                // Calculate how many output frames correspond to actual input (not padding)
                let speed = self.read_speed();
                let resample_ratio = self.calculate_resample_ratio(speed);
                let actual_output_frames = ((buffered as f64) * resample_ratio).ceil() as usize;
                let frames_to_use = actual_output_frames.min(out_len);

                if frames_to_use == 0 {
                    return None;
                }

                // Reuse temp_output_all
                for buf in &mut self.temp_output_all {
                    buf.clear();
                }
                for (ch, buf) in self.temp_output_bufs.iter().enumerate() {
                    self.temp_output_all[ch].extend_from_slice(&buf[..frames_to_use]);
                }

                let interleaved = self.interleave(&self.temp_output_all);
                Some(PcmChunk::new(self.output_spec, interleaved))
            }
            Err(e) => {
                trace!(err = %e, "Resampler flush error");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_speed_atomic(speed: f32) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(speed.to_bits()))
    }

    #[test]
    fn test_passthrough_same_rate() {
        let speed = make_speed_atomic(1.0);
        let processor = ResamplerProcessor::new(44100, 44100, 2, speed);
        assert!(processor.is_passthrough());
    }

    #[test]
    fn test_no_passthrough_different_rate() {
        let speed = make_speed_atomic(1.0);
        let processor = ResamplerProcessor::new(48000, 44100, 2, speed);
        assert!(!processor.is_passthrough());
    }

    #[test]
    fn test_no_passthrough_speed_not_one() {
        let speed = make_speed_atomic(1.5);
        let processor = ResamplerProcessor::new(44100, 44100, 2, speed);
        assert!(!processor.is_passthrough());
    }

    #[test]
    fn test_passthrough_processing() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(44100, 44100, 2, speed);

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.1, 0.2, 0.3, 0.4],
        );

        let result = processor.process(&chunk);
        assert!(result.is_some());
        let out = result.unwrap();
        assert_eq!(out.pcm, chunk.pcm);
        assert_eq!(out.spec.sample_rate, 44100);
    }

    #[test]
    fn test_output_spec() {
        let speed = make_speed_atomic(1.0);
        let processor = ResamplerProcessor::new(48000, 44100, 2, speed);
        assert_eq!(processor.output_spec.sample_rate, 44100);
        assert_eq!(processor.output_spec.channels, 2);
    }

    #[test]
    fn test_dynamic_speed_change() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(44100, 44100, 2, speed.clone());
        assert!(processor.is_passthrough());

        speed.store(1.5_f32.to_bits(), Ordering::Relaxed);

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.1; 2048],
        );
        let _ = processor.process(&chunk);

        assert!(!processor.is_passthrough());
    }

    #[test]
    fn test_accumulates_small_chunks() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(48000, 44100, 2, speed);

        // Send small chunk (less than DEFAULT_CHUNK_SIZE)
        let small_chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 100], // 50 frames, less than 1024
        );

        // First call returns None (accumulating)
        let result = processor.process(&small_chunk);
        assert!(result.is_none());

        // Buffer should have data
        assert_eq!(processor.input_buffer[0].len(), 50);
    }

    #[test]
    fn test_processes_large_chunks() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(48000, 44100, 2, speed);

        // Send large chunk (more than DEFAULT_CHUNK_SIZE)
        let large_chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 4096], // 2048 frames
        );

        let result = processor.process(&large_chunk);
        assert!(result.is_some());

        let out = result.unwrap();
        // Output should have samples (approximately 2048 * 44100/48000)
        assert!(!out.pcm.is_empty());
    }

    #[test]
    fn test_source_rate_change_resets_resampler() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(48000, 44100, 2, speed);

        // First chunk at 48kHz
        let chunk1 = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 4096],
        );
        let _ = processor.process(&chunk1);
        assert_eq!(processor.source_rate, 48000);

        // Second chunk at 44.1kHz (ABR switch)
        let chunk2 = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.2; 4096],
        );
        let _ = processor.process(&chunk2);
        assert_eq!(processor.source_rate, 44100);

        // Buffer should be cleared on rate change
        // (or contain only data from chunk2)
    }

    #[test]
    fn test_no_data_loss_across_chunks() {
        let speed = make_speed_atomic(1.0);
        let mut processor = ResamplerProcessor::new(48000, 44100, 2, speed);

        let mut total_input_frames = 0;
        let mut total_output_frames = 0;

        // Send multiple chunks
        for _ in 0..10 {
            let chunk = PcmChunk::new(
                PcmSpec {
                    sample_rate: 48000,
                    channels: 2,
                },
                vec![0.1; 2048], // 1024 frames per chunk
            );
            total_input_frames += 1024;

            if let Some(out) = processor.process(&chunk) {
                total_output_frames += out.frames();
            }
        }

        // Expected output frames = input_frames * (44100/48000)
        let expected = (total_input_frames as f64 * 44100.0 / 48000.0) as usize;

        // Allow some tolerance for buffering
        let tolerance = DEFAULT_CHUNK_SIZE * 2;
        assert!(
            total_output_frames + tolerance >= expected,
            "Output {} + tolerance {} should be >= expected {}",
            total_output_frames,
            tolerance,
            expected
        );
    }

    #[test]
    fn test_speed_affects_output_count() {
        // Same source and target rate, but speed != 1.0
        let speed = make_speed_atomic(2.0); // 2x speed
        let mut processor = ResamplerProcessor::new(44100, 44100, 2, speed);

        // Should NOT be passthrough because speed != 1.0
        assert!(
            !processor.is_passthrough(),
            "Should not be passthrough when speed != 1.0"
        );

        let mut total_output_frames = 0;
        let input_frames_per_chunk = 2048;

        // Send multiple chunks
        for _ in 0..5 {
            let chunk = PcmChunk::new(
                PcmSpec {
                    sample_rate: 44100,
                    channels: 2,
                },
                vec![0.1; input_frames_per_chunk * 2], // *2 for stereo
            );

            if let Some(out) = processor.process(&chunk) {
                total_output_frames += out.frames();
            }
        }

        let total_input_frames = input_frames_per_chunk * 5;
        // At 2x speed, we expect roughly half the output frames
        let expected_output = total_input_frames / 2;

        // Allow tolerance
        let tolerance = DEFAULT_CHUNK_SIZE * 2;
        assert!(
            total_output_frames < total_input_frames,
            "At 2x speed, output {} should be less than input {}",
            total_output_frames,
            total_input_frames
        );
        assert!(
            (total_output_frames as i64 - expected_output as i64).abs() < tolerance as i64,
            "Output {} should be close to expected {} (tolerance {})",
            total_output_frames,
            expected_output,
            tolerance
        );
    }

    #[test]
    fn test_speed_half_doubles_output() {
        // Same source and target rate, speed = 0.5 (half speed = 2x duration)
        let speed = make_speed_atomic(0.5);
        let mut processor = ResamplerProcessor::new(44100, 44100, 2, speed);

        assert!(
            !processor.is_passthrough(),
            "Should not be passthrough when speed != 1.0"
        );

        let mut total_output_frames = 0;
        let input_frames_per_chunk = 2048;

        for _ in 0..5 {
            let chunk = PcmChunk::new(
                PcmSpec {
                    sample_rate: 44100,
                    channels: 2,
                },
                vec![0.1; input_frames_per_chunk * 2],
            );

            if let Some(out) = processor.process(&chunk) {
                total_output_frames += out.frames();
            }
        }

        let total_input_frames = input_frames_per_chunk * 5;
        // At 0.5x speed, we expect roughly double the output frames
        let expected_output = total_input_frames * 2;

        assert!(
            total_output_frames > total_input_frames,
            "At 0.5x speed, output {} should be greater than input {}",
            total_output_frames,
            total_input_frames
        );

        let tolerance = DEFAULT_CHUNK_SIZE * 4;
        assert!(
            (total_output_frames as i64 - expected_output as i64).abs() < tolerance as i64,
            "Output {} should be close to expected {} (tolerance {})",
            total_output_frames,
            expected_output,
            tolerance
        );
    }
}
