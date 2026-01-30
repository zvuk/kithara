//! ResamplerProcessor: wraps rubato for sample rate conversion.
//!
//! Reacts to dynamic `host_sample_rate` changes via `Arc<AtomicU32>`.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use kithara_decode::{PcmChunk, PcmSpec};
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use tracing::{debug, info, trace};

use crate::traits::AudioEffect;

/// Quality preset for the audio resampler.
///
/// Controls the sinc interpolation parameters used by rubato.
/// Higher quality uses more CPU but produces better audio fidelity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResamplerQuality {
    /// Fast resampling with acceptable quality.
    /// Suitable for previews or low-power devices.
    Fast,
    /// Balanced quality and performance.
    Normal,
    /// High quality resampling, recommended for music playback.
    #[default]
    High,
}

impl ResamplerQuality {
    fn sinc_params(self) -> SincInterpolationParameters {
        match self {
            Self::Fast => SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            },
            Self::Normal => SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            },
            Self::High => SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            },
        }
    }
}

/// Configuration parameters for the resampler effect.
///
/// Contains all values needed to construct a [`ResamplerProcessor`].
pub struct ResamplerParams {
    /// Quality preset controlling sinc interpolation parameters.
    pub quality: ResamplerQuality,
    /// Number of input frames per resampler processing block.
    pub chunk_size: usize,
    /// Shared atomic for dynamic host sample rate tracking.
    pub host_sample_rate: Arc<AtomicU32>,
    /// Initial source sample rate.
    pub source_sample_rate: u32,
    /// Number of audio channels.
    pub channels: usize,
}

impl ResamplerParams {
    /// Create resampler params with required runtime values and default settings.
    pub fn new(host_sample_rate: Arc<AtomicU32>, source_sample_rate: u32, channels: usize) -> Self {
        Self {
            quality: ResamplerQuality::default(),
            chunk_size: 4096,
            host_sample_rate,
            source_sample_rate,
            channels,
        }
    }

    /// Set resampling quality preset.
    pub fn with_quality(mut self, quality: ResamplerQuality) -> Self {
        self.quality = quality;
        self
    }

    /// Set the number of input frames per resampler processing block.
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }
}

/// Audio resampler that converts between source and host sample rates.
///
/// Monitors `host_sample_rate` (an `Arc<AtomicU32>`) for dynamic changes.
/// When `host_sample_rate == 0` or equals `source_rate`, operates in passthrough mode.
pub struct ResamplerProcessor {
    source_rate: u32,
    host_sample_rate: Arc<AtomicU32>,
    channels: usize,
    resampler: Option<SincFixedIn<f32>>,
    current_ratio: f64,
    /// Quality and chunk_size for resampler (re-)creation.
    quality: ResamplerQuality,
    chunk_size: usize,
    /// Accumulated input buffer (planar format)
    input_buffer: Vec<Vec<f32>>,
    output_spec: PcmSpec,
    // Reusable temporary buffers
    temp_input_slice: Vec<Vec<f32>>,
    temp_output_bufs: Vec<Vec<f32>>,
    temp_output_all: Vec<Vec<f32>>,
}

impl ResamplerProcessor {
    /// Create a new resampler from configuration parameters.
    pub fn new(params: ResamplerParams) -> Self {
        let source_rate = params.source_sample_rate;
        let channels = params.channels;
        let host_sr = params.host_sample_rate.load(Ordering::Relaxed);
        let target_rate = if host_sr == 0 { source_rate } else { host_sr };
        let output_spec = PcmSpec {
            sample_rate: target_rate,
            channels: channels as u16,
        };

        let mut processor = Self {
            source_rate,
            host_sample_rate: params.host_sample_rate,
            channels,
            resampler: None,
            current_ratio: 1.0,
            quality: params.quality,
            chunk_size: params.chunk_size,
            input_buffer: vec![Vec::new(); channels],
            output_spec,
            temp_input_slice: Vec::with_capacity(channels),
            temp_output_bufs: Vec::with_capacity(channels),
            temp_output_all: vec![Vec::new(); channels],
        };

        processor.update_resampler_if_needed();

        info!(
            source_rate,
            host_sr = host_sr,
            target_rate,
            channels,
            active = !processor.is_passthrough(),
            "Resampler initialized"
        );

        processor
    }

    /// Process an input chunk, returning resampled output.
    pub fn process_chunk(&mut self, chunk: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        let chunk_rate = chunk.spec.sample_rate;
        let chunk_channels = chunk.spec.channels as usize;

        // Handle source spec changes (ABR switch)
        if chunk_channels != self.channels {
            // Channel count changed — must recreate resampler.
            debug!(
                old_channels = self.channels,
                new_channels = chunk_channels,
                old_rate = self.source_rate,
                new_rate = chunk_rate,
                "Channel count changed, recreating resampler"
            );
            self.channels = chunk_channels;
            self.source_rate = chunk_rate;
            self.input_buffer = vec![Vec::new(); chunk_channels];
            self.temp_output_all = vec![Vec::new(); chunk_channels];
            self.output_spec.channels = chunk_channels as u16;
            self.resampler = None;
        } else if chunk_rate != self.source_rate {
            // Only sample rate changed — clear buffers and let
            // update_resampler_if_needed() adjust the ratio dynamically.
            debug!(
                old_rate = self.source_rate,
                new_rate = chunk_rate,
                "Source sample rate changed, updating ratio dynamically"
            );
            self.source_rate = chunk_rate;
            for buf in &mut self.input_buffer {
                buf.clear();
            }
        }

        self.update_resampler_if_needed();

        if self.is_passthrough() {
            trace!(
                source_rate = self.source_rate,
                target_rate = self.output_spec.sample_rate,
                "Passthrough mode"
            );
            return Some(PcmChunk::new(self.output_spec, chunk.pcm.clone()));
        }

        trace!(
            source_rate = self.source_rate,
            target_rate = self.output_spec.sample_rate,
            input_frames = chunk.frames(),
            "Resampling"
        );
        self.resample(chunk)
    }

    /// Flush remaining data from buffer (called at end of stream).
    pub fn flush_buffer(&mut self) -> Option<PcmChunk<f32>> {
        self.resampler.as_ref()?;

        if self.input_buffer[0].is_empty() {
            return None;
        }

        let input_frames = self.resampler.as_ref()?.input_frames_next();
        let channels = self.channels;
        let buffered = self.input_buffer[0].len();

        // Pad with zeros to reach input_frames
        let padding_needed = input_frames.saturating_sub(buffered);
        for buf in &mut self.input_buffer {
            buf.extend(std::iter::repeat_n(0.0, padding_needed));
        }

        debug!(buffered, padding_needed, "Flushing resampler buffer");

        self.ensure_temp_buffers(channels);
        for ch in 0..channels {
            self.temp_input_slice[ch].clear();
            self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
        }

        let output_frames = self.resampler.as_ref()?.output_frames_next();

        for ch in 0..channels {
            self.temp_output_bufs[ch].resize(output_frames, 0.0);
        }

        let result = {
            let input_refs: Vec<&[f32]> =
                self.temp_input_slice.iter().map(|v| v.as_slice()).collect();
            let mut output_refs: Vec<&mut [f32]> = self
                .temp_output_bufs
                .iter_mut()
                .map(|v| v.as_mut_slice())
                .collect();
            let resampler = self.resampler.as_mut()?;
            resampler.process_into_buffer(&input_refs, &mut output_refs, None)
        };

        match result {
            Ok((_, out_len)) => {
                for buf in &mut self.input_buffer {
                    buf.clear();
                }

                let actual_output_frames = ((buffered as f64) * self.current_ratio).ceil() as usize;
                let frames_to_use = actual_output_frames.min(out_len);

                if frames_to_use == 0 {
                    return None;
                }

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

    fn is_passthrough(&self) -> bool {
        self.resampler.is_none()
    }

    fn should_passthrough(source_rate: u32, target_rate: u32) -> bool {
        source_rate == target_rate || target_rate == 0
    }

    fn update_resampler_if_needed(&mut self) {
        let host_sr = self.host_sample_rate.load(Ordering::Relaxed);
        let target_rate = if host_sr == 0 {
            self.source_rate
        } else {
            host_sr
        };

        // Update output spec
        self.output_spec.sample_rate = target_rate;

        let should_pt = Self::should_passthrough(self.source_rate, target_rate);
        let currently_pt = self.is_passthrough();

        let new_ratio = if self.source_rate > 0 {
            target_rate as f64 / self.source_rate as f64
        } else {
            1.0
        };
        let ratio_changed = (new_ratio - self.current_ratio).abs() > 0.0001;

        if should_pt && !currently_pt {
            debug!(
                source_rate = self.source_rate,
                target_rate, "Resampler switching to passthrough"
            );
            self.resampler = None;
            self.current_ratio = 1.0;
            for buf in &mut self.input_buffer {
                buf.clear();
            }
        } else if !should_pt && (currently_pt || ratio_changed) {
            if !currently_pt && ratio_changed {
                // Try dynamic ratio update first
                if let Some(ref mut resampler) = self.resampler {
                    if let Err(e) = resampler.set_resample_ratio(new_ratio, false) {
                        debug!(err = %e, "Failed to update ratio dynamically, recreating");
                        self.resampler = None;
                    } else {
                        debug!(
                            new_ratio,
                            source_rate = self.source_rate,
                            target_rate,
                            "Resampler ratio updated dynamically"
                        );
                        self.current_ratio = new_ratio;
                        return;
                    }
                }
            }

            debug!(
                new_ratio,
                source_rate = self.source_rate,
                target_rate,
                "Resampler activated"
            );

            match SincFixedIn::<f32>::new(
                new_ratio,
                2.0,
                self.quality.sinc_params(),
                self.chunk_size,
                self.channels,
            ) {
                Ok(resampler) => {
                    self.resampler = Some(resampler);
                    self.current_ratio = new_ratio;
                }
                Err(e) => {
                    debug!(err = %e, "Failed to create resampler, staying in current mode");
                }
            }
        }
    }

    fn ensure_temp_buffers(&mut self, channels: usize) {
        if self.temp_input_slice.len() < channels {
            self.temp_input_slice.resize_with(channels, Vec::new);
        }
        if self.temp_output_bufs.len() < channels {
            self.temp_output_bufs.resize_with(channels, Vec::new);
        }
    }

    fn resample(&mut self, chunk: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        self.resampler.as_ref()?;

        self.append_to_buffer(&chunk.pcm);

        let input_frames = self.resampler.as_ref()?.input_frames_next();
        let channels = self.channels;

        // Ensure temp buffers are sized correctly before the loop
        self.ensure_temp_buffers(channels);

        for buf in &mut self.temp_output_all {
            buf.clear();
        }

        while self.input_buffer[0].len() >= input_frames {
            // 1. Copy input data into temp_input_slice
            for ch in 0..channels {
                self.temp_input_slice[ch].clear();
                self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
            }

            // 2. Get output_frames from resampler (brief borrow)
            let output_frames = self.resampler.as_ref()?.output_frames_next();

            // 3. Prepare output buffers
            for ch in 0..channels {
                self.temp_output_bufs[ch].resize(output_frames, 0.0);
            }

            // 4. Build refs and process — all borrows in one block
            let result = {
                let input_refs: Vec<&[f32]> =
                    self.temp_input_slice.iter().map(|v| v.as_slice()).collect();
                let mut output_refs: Vec<&mut [f32]> = self
                    .temp_output_bufs
                    .iter_mut()
                    .map(|v| v.as_mut_slice())
                    .collect();
                let resampler = self.resampler.as_mut()?;
                resampler.process_into_buffer(&input_refs, &mut output_refs, None)
            };

            match result {
                Ok((_, out_len)) => {
                    for ch in 0..channels {
                        let src = &self.temp_output_bufs[ch][..out_len];
                        self.temp_output_all[ch].extend_from_slice(src);
                    }
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
}

impl AudioEffect for ResamplerProcessor {
    fn process(&mut self, chunk: PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        self.process_chunk(&chunk)
    }

    fn flush(&mut self) -> Option<PcmChunk<f32>> {
        self.flush_buffer()
    }

    fn reset(&mut self) {
        for buf in &mut self.input_buffer {
            buf.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_host_rate(rate: u32) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(rate))
    }

    fn params(host_sr: Arc<AtomicU32>, source_rate: u32, channels: usize) -> ResamplerParams {
        ResamplerParams::new(host_sr, source_rate, channels)
    }

    #[test]
    fn test_passthrough_same_rate() {
        let processor = ResamplerProcessor::new(params(make_host_rate(44100), 44100, 2));
        assert!(processor.is_passthrough());
    }

    #[test]
    fn test_passthrough_host_zero() {
        let processor = ResamplerProcessor::new(params(make_host_rate(0), 44100, 2));
        assert!(processor.is_passthrough());
    }

    #[test]
    fn test_no_passthrough_different_rate() {
        let processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));
        assert!(!processor.is_passthrough());
    }

    #[test]
    fn test_passthrough_processing() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 44100, 2));

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.1, 0.2, 0.3, 0.4],
        );

        let result = processor.process_chunk(&chunk);
        assert!(result.is_some());
        let out = result.unwrap();
        assert_eq!(out.pcm, chunk.pcm);
        assert_eq!(out.spec.sample_rate, 44100);
    }

    #[test]
    fn test_output_spec() {
        let processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));
        assert_eq!(processor.output_spec.sample_rate, 44100);
        assert_eq!(processor.output_spec.channels, 2);
    }

    #[test]
    fn test_dynamic_host_rate_change() {
        let host_sr = make_host_rate(44100);
        let mut processor = ResamplerProcessor::new(params(host_sr.clone(), 44100, 2));
        assert!(processor.is_passthrough());

        // Change host sample rate dynamically
        host_sr.store(48000, Ordering::Relaxed);

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.1; 2048],
        );
        let _ = processor.process_chunk(&chunk);

        assert!(!processor.is_passthrough());
    }

    #[test]
    fn test_accumulates_small_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let small_chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 100],
        );

        let result = processor.process_chunk(&small_chunk);
        assert!(result.is_none());
        assert_eq!(processor.input_buffer[0].len(), 50);
    }

    #[test]
    fn test_processes_large_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let large_chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 16384],
        );

        let result = processor.process_chunk(&large_chunk);
        assert!(result.is_some());
        assert!(!result.unwrap().pcm.is_empty());
    }

    #[test]
    fn test_source_rate_change_updates_dynamically() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let chunk1 = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 4096],
        );
        let _ = processor.process_chunk(&chunk1);
        assert_eq!(processor.source_rate, 48000);

        // Source rate changed (e.g. ABR switch) — resampler should update dynamically
        let chunk2 = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.2; 4096],
        );
        let _ = processor.process_chunk(&chunk2);
        assert_eq!(processor.source_rate, 44100);
        // Resampler should now be in passthrough (44100 → 44100)
        assert!(processor.is_passthrough());
    }

    #[test]
    fn test_no_data_loss_across_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let mut total_input_frames = 0;
        let mut total_output_frames = 0;

        for _ in 0..10 {
            let chunk = PcmChunk::new(
                PcmSpec {
                    sample_rate: 48000,
                    channels: 2,
                },
                vec![0.1; 2048],
            );
            total_input_frames += 1024;

            if let Some(out) = processor.process_chunk(&chunk) {
                total_output_frames += out.frames();
            }
        }

        let expected = (total_input_frames as f64 * 44100.0 / 48000.0) as usize;
        let tolerance = 1024 * 2;
        assert!(
            total_output_frames + tolerance >= expected,
            "Output {} + tolerance {} should be >= expected {}",
            total_output_frames,
            tolerance,
            expected
        );
    }

    #[test]
    fn test_audio_effect_trait() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 44100, 2));

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.1, 0.2, 0.3, 0.4],
        );

        // Test through AudioEffect trait
        let result = AudioEffect::process(&mut processor, chunk);
        assert!(result.is_some());

        // Test reset
        AudioEffect::reset(&mut processor);
        assert!(processor.input_buffer[0].is_empty());
    }
}
