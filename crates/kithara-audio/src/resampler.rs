//! `ResamplerProcessor`: wraps rubato for sample rate conversion.
//!
//! Reacts to dynamic `host_sample_rate` changes via `Arc<AtomicU32>`.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use fast_interleave::{deinterleave_variable, interleave_variable};
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_decode::{PcmChunk, PcmSpec};
use rubato::{
    Async, Fft, FixedAsync, FixedSync, PolynomialDegree, Resampler as RubatoResampler,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use smallvec::SmallVec;
use tracing::{debug, info, trace};

use crate::traits::AudioEffect;

/// Quality preset for the audio resampler.
///
/// Controls the resampling algorithm and interpolation parameters.
/// Higher quality uses more CPU but produces better audio fidelity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResamplerQuality {
    /// Fastest resampling using polynomial interpolation.
    /// Suitable for previews or low-power devices.
    Fast,
    /// Balanced sinc resampling (64-tap, linear interpolation).
    Normal,
    /// Good sinc resampling (128-tap, linear interpolation).
    Good,
    /// High sinc resampling (256-tap, cubic interpolation).
    /// Recommended for music playback.
    #[default]
    High,
    /// Maximum quality using FFT-based resampling.
    /// Highest CPU usage, best for offline processing or high-end playback.
    Maximum,
}

impl ResamplerQuality {
    fn sinc_params(self) -> SincInterpolationParameters {
        match self {
            Self::Normal => SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            },
            Self::Good => SincInterpolationParameters {
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
            // Fast and Maximum don't use sinc — unreachable from normal flow
            _ => SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            },
        }
    }
}

/// Enum wrapper for rubato resamplers (trait is not object-safe).
enum ResamplerKind {
    Poly(Async<f32>),
    Sinc(Async<f32>),
    Fft(Box<Fft<f32>>),
}

impl ResamplerKind {
    fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), rubato::ResampleError> {
        use audioadapter_buffers::direct::SequentialSliceOfVecs;

        let channels = input.len();
        let input_frames = input[0].len();
        let output_frames = output[0].len();

        let input_adapter =
            SequentialSliceOfVecs::new(input, channels, input_frames).map_err(|_| {
                rubato::ResampleError::InsufficientInputBufferSize {
                    expected: input_frames,
                    actual: 0,
                }
            })?;
        let mut output_adapter = SequentialSliceOfVecs::new_mut(output, channels, output_frames)
            .map_err(|_| rubato::ResampleError::InsufficientOutputBufferSize {
                expected: output_frames,
                actual: 0,
            })?;

        match self {
            Self::Poly(r) => r.process_into_buffer(&input_adapter, &mut output_adapter, None),
            Self::Sinc(r) => r.process_into_buffer(&input_adapter, &mut output_adapter, None),
            Self::Fft(r) => r.process_into_buffer(&input_adapter, &mut output_adapter, None),
        }
    }

    fn input_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) => r.input_frames_next(),
            Self::Sinc(r) => r.input_frames_next(),
            Self::Fft(r) => r.input_frames_next(),
        }
    }

    fn output_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) => r.output_frames_next(),
            Self::Sinc(r) => r.output_frames_next(),
            Self::Fft(r) => r.output_frames_next(),
        }
    }

    fn set_resample_ratio(&mut self, ratio: f64, ramp: bool) -> Result<(), rubato::ResampleError> {
        match self {
            Self::Poly(r) => r.set_resample_ratio(ratio, ramp),
            Self::Sinc(r) => r.set_resample_ratio(ratio, ramp),
            Self::Fft(r) => r.set_resample_ratio(ratio, ramp),
        }
    }
}

/// Configuration parameters for the resampler effect.
///
/// Contains all values needed to construct a [`ResamplerProcessor`].
pub struct ResamplerParams {
    /// Quality preset controlling resampling algorithm.
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
    resampler: Option<ResamplerKind>,
    current_ratio: f64,
    quality: ResamplerQuality,
    chunk_size: usize,
    /// Accumulated input buffer (planar format).
    input_buffer: SmallVec<[Vec<f32>; 8]>,
    output_spec: PcmSpec,
    // Reusable temporary buffers
    temp_input_slice: SmallVec<[Vec<f32>; 8]>,
    temp_output_bufs: SmallVec<[Vec<f32>; 8]>,
    temp_output_all: SmallVec<[Vec<f32>; 8]>,
    temp_deinterleave: SmallVec<[Vec<f32>; 8]>,
    /// Pool for interleave output buffers.
    pool: PcmPool,
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
            channels,
            source_rate,
            output_spec,
            host_sample_rate: params.host_sample_rate,
            resampler: None,
            current_ratio: 1.0,
            quality: params.quality,
            chunk_size: params.chunk_size,
            input_buffer: smallvec_new_vecs(channels),
            temp_input_slice: smallvec_new_vecs(channels),
            temp_output_bufs: smallvec_new_vecs(channels),
            temp_output_all: smallvec_new_vecs(channels),
            temp_deinterleave: smallvec_new_vecs(channels),
            pool: pcm_pool().clone(),
        };

        processor.update_resampler_if_needed();

        info!(
            source_rate,
            host_sr = host_sr,
            target_rate,
            channels,
            active = !processor.is_passthrough(),
            quality = ?params.quality,
            "Resampler initialized"
        );

        processor
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

        let output_frames = {
            let resampler = self.resampler.as_ref()?;
            resampler.output_frames_next()
        };

        let result = {
            let mut resampler = self.resampler.take()?;
            let res = self.process_block(&mut resampler, input_frames, output_frames);
            self.resampler = Some(resampler);
            res
        };

        match result {
            Ok(out_len) => {
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

    fn create_resampler(
        quality: ResamplerQuality,
        ratio: f64,
        chunk_size: usize,
        channels: usize,
        source_rate: u32,
        target_rate: u32,
    ) -> Result<ResamplerKind, rubato::ResamplerConstructionError> {
        match quality {
            ResamplerQuality::Fast => {
                let poly = Async::new_poly(
                    ratio,
                    2.0,
                    PolynomialDegree::Cubic,
                    chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(ResamplerKind::Poly(poly))
            }
            ResamplerQuality::Normal | ResamplerQuality::Good | ResamplerQuality::High => {
                let sinc = Async::new_sinc(
                    ratio,
                    2.0,
                    &quality.sinc_params(),
                    chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(ResamplerKind::Sinc(sinc))
            }
            ResamplerQuality::Maximum => {
                let fft = Fft::new(
                    source_rate as usize,
                    target_rate as usize,
                    chunk_size,
                    2,
                    channels,
                    FixedSync::Input,
                )?;
                Ok(ResamplerKind::Fft(Box::new(fft)))
            }
        }
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

        if should_pt {
            if !currently_pt {
                debug!(
                    source_rate = self.source_rate,
                    target_rate, "Resampler switching to passthrough"
                );
                self.resampler = None;
            }
            self.current_ratio = 1.0;
            for buf in &mut self.input_buffer {
                buf.clear();
            }
            return;
        }

        // Need an active resampler with the latest ratio
        if !currently_pt
            && ratio_changed
            && let Some(ref mut resampler) = self.resampler
        {
            match resampler.set_resample_ratio(new_ratio, false) {
                Ok(()) => {
                    debug!(
                        new_ratio,
                        source_rate = self.source_rate,
                        target_rate,
                        "Resampler ratio updated dynamically"
                    );
                    self.current_ratio = new_ratio;
                    return;
                }
                Err(e) => {
                    debug!(err = %e, "Failed to update ratio dynamically, recreating");
                    self.resampler = None;
                }
            }
        }

        if currently_pt || self.resampler.is_none() || ratio_changed {
            debug!(
                new_ratio,
                source_rate = self.source_rate,
                target_rate,
                quality = ?self.quality,
                "Resampler activated"
            );

            match Self::create_resampler(
                self.quality,
                new_ratio,
                self.chunk_size,
                self.channels,
                self.source_rate,
                target_rate,
            ) {
                Ok(resampler) => {
                    self.resampler = Some(resampler);
                    self.current_ratio = new_ratio;
                    for buf in &mut self.input_buffer {
                        buf.clear();
                    }
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

    fn process_block(
        &mut self,
        resampler: &mut ResamplerKind,
        input_frames: usize,
        output_frames: usize,
    ) -> Result<usize, rubato::ResampleError> {
        let channels = self.channels;

        for ch in 0..channels {
            self.temp_input_slice[ch].clear();
            self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
        }

        for ch in 0..channels {
            self.temp_output_bufs[ch].resize(output_frames, 0.0);
        }

        let (_, out_len) = resampler.process_into_buffer(
            &self.temp_input_slice[..channels],
            &mut self.temp_output_bufs[..channels],
        )?;
        Ok(out_len)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn resample(&mut self, chunk: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        self.resampler.as_ref()?;

        self.append_to_buffer(&chunk.pcm);

        let input_frames = self.resampler.as_ref()?.input_frames_next();
        let channels = self.channels;

        if self.input_buffer[0].len() < input_frames {
            trace!(
                buffered = self.input_buffer[0].len(),
                needed = input_frames,
                "Accumulating data"
            );
            return None;
        }

        self.ensure_temp_buffers(channels);

        if self.temp_output_all.len() < channels {
            self.temp_output_all.resize_with(channels, Vec::new);
        }
        for buf in &mut self.temp_output_all {
            buf.clear();
        }

        while self.input_buffer[0].len() >= input_frames {
            let output_frames = {
                let resampler = self.resampler.as_ref()?;
                resampler.output_frames_next()
            };

            let result = {
                let mut resampler = self.resampler.take()?;
                let res = self.process_block(&mut resampler, input_frames, output_frames);
                self.resampler = Some(resampler);
                res
            };

            match result {
                Ok(out_len) => {
                    for ch in 0..channels {
                        let src = &self.temp_output_bufs[ch][..out_len];
                        self.temp_output_all[ch].extend_from_slice(src);
                    }
                    for buf in &mut self.input_buffer {
                        buf.drain(..input_frames);
                    }

                    if self.input_buffer[0].len() < input_frames {
                        break;
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

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn append_to_buffer(&mut self, interleaved: &[f32]) {
        if interleaved.is_empty() {
            return;
        }

        let frames = interleaved.len() / self.channels;

        if self.temp_deinterleave.len() < self.channels {
            self.temp_deinterleave.resize_with(self.channels, Vec::new);
        }

        // Resize each channel buffer to needed size (reuses existing capacity)
        for buf in &mut self.temp_deinterleave[..self.channels] {
            buf.resize(frames, 0.0);
        }

        // Use fast_interleave (SIMD-optimized) to deinterleave
        let num_channels =
            std::num::NonZeroUsize::new(self.channels).expect("channels must be > 0");
        deinterleave_variable(
            interleaved,
            num_channels,
            &mut self.temp_deinterleave[..self.channels],
            0..frames,
        );

        // Append deinterleaved data to existing input buffers
        for ch in 0..self.channels {
            self.input_buffer[ch].extend_from_slice(&self.temp_deinterleave[ch][..frames]);
        }
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn interleave(&self, planar: &[Vec<f32>]) -> Vec<f32> {
        if planar.is_empty() || planar[0].is_empty() {
            return Vec::new();
        }

        let frames = planar[0].len();
        let total = frames * self.channels;

        // Get buffer from pool
        let mut result = self.pool.get_with(|b| {
            b.clear();
            b.resize(total, 0.0);
        });

        // Use fast_interleave (SIMD-optimized)
        let num_channels =
            std::num::NonZeroUsize::new(self.channels).expect("channels must be > 0");
        interleave_variable(planar, 0..frames, &mut result[..], num_channels);

        result.into_inner()
    }
}

/// Create a `SmallVec` of empty Vecs for each channel.
#[cfg_attr(feature = "perf", hotpath::measure)]
fn smallvec_new_vecs(channels: usize) -> SmallVec<[Vec<f32>; 8]> {
    (0..channels).map(|_| Vec::new()).collect()
}

impl AudioEffect for ResamplerProcessor {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn process(&mut self, chunk: PcmChunk<f32>) -> Option<PcmChunk<f32>> {
        let chunk_rate = chunk.spec.sample_rate;
        let chunk_channels = chunk.spec.channels as usize;

        // Handle source spec changes (ABR switch)
        if chunk_channels != self.channels {
            debug!(
                old_channels = self.channels,
                new_channels = chunk_channels,
                old_rate = self.source_rate,
                new_rate = chunk_rate,
                "Channel count changed, recreating resampler"
            );
            self.channels = chunk_channels;
            self.source_rate = chunk_rate;
            self.input_buffer = smallvec_new_vecs(chunk_channels);
            self.temp_input_slice = smallvec_new_vecs(chunk_channels);
            self.temp_output_bufs = smallvec_new_vecs(chunk_channels);
            self.temp_output_all = smallvec_new_vecs(chunk_channels);
            self.temp_deinterleave = smallvec_new_vecs(chunk_channels);
            self.output_spec.channels = chunk_channels as u16;
            self.resampler = None;
        } else if chunk_rate != self.source_rate {
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
                chunk_samples = chunk.pcm.len(),
                "Resampler passthrough (no resampling)"
            );
            // Passthrough: transfer ownership, just update spec
            let mut out = chunk;
            out.spec = self.output_spec;
            return Some(out);
        }

        trace!(
            source_rate = self.source_rate,
            target_rate = self.output_spec.sample_rate,
            input_frames = chunk.frames(),
            "Resampling"
        );
        self.resample(&chunk)
    }

    fn flush(&mut self) -> Option<PcmChunk<f32>> {
        self.flush_buffer()
    }

    fn reset(&mut self) {
        for buf in &mut self.input_buffer {
            buf.clear();
        }
        // Drop the rubato resampler so internal filter state (taps, phase,
        // buffered samples) from the old audio doesn't bleed into the new
        // stream.  `update_resampler_if_needed()` will create a fresh
        // instance on the next `process()` call.
        self.resampler = None;
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

    fn params_with_quality(
        host_sr: Arc<AtomicU32>,
        source_rate: u32,
        channels: usize,
        quality: ResamplerQuality,
    ) -> ResamplerParams {
        ResamplerParams::new(host_sr, source_rate, channels).with_quality(quality)
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

        let result = processor.process(chunk.clone());
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
        let _ = processor.process(chunk);

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

        let result = processor.process(small_chunk);
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

        let result = processor.process(large_chunk);
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
        let _ = processor.process(chunk1);
        assert_eq!(processor.source_rate, 48000);

        // Source rate changed (e.g. ABR switch) — resampler should update dynamically
        let chunk2 = PcmChunk::new(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            vec![0.2; 4096],
        );
        let _ = processor.process(chunk2);
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

            if let Some(out) = processor.process(chunk) {
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

    // Quality-specific tests

    #[test]
    fn test_fast_quality_resamples() {
        let mut processor = ResamplerProcessor::new(params_with_quality(
            make_host_rate(44100),
            48000,
            2,
            ResamplerQuality::Fast,
        ));
        assert!(!processor.is_passthrough());

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
    }

    #[test]
    fn test_maximum_quality_resamples() {
        let mut processor = ResamplerProcessor::new(params_with_quality(
            make_host_rate(44100),
            48000,
            2,
            ResamplerQuality::Maximum,
        ));
        assert!(!processor.is_passthrough());

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
    }

    #[test]
    fn test_good_quality_resamples() {
        let mut processor = ResamplerProcessor::new(params_with_quality(
            make_host_rate(44100),
            48000,
            2,
            ResamplerQuality::Good,
        ));
        assert!(!processor.is_passthrough());

        let chunk = PcmChunk::new(
            PcmSpec {
                sample_rate: 48000,
                channels: 2,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
    }
}
