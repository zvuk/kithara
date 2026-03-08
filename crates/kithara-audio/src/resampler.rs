//! `ResamplerProcessor`: wraps rubato for sample rate conversion.
//!
//! Reacts to dynamic `host_sample_rate` changes via `Arc<AtomicU32>`.

use std::{
    iter,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use derive_setters::Setters;
use fast_interleave::{deinterleave_variable, interleave_variable};
use kithara_bufpool::{PcmBuf, PcmPool, pcm_pool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use portable_atomic::AtomicF32;
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
            // Normal, Fast and Maximum share the same default sinc params.
            // Fast and Maximum don't use sinc — unreachable from normal flow.
            Self::Normal | Self::Fast | Self::Maximum => SincInterpolationParameters {
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
            Self::Poly(r) | Self::Sinc(r) => {
                r.process_into_buffer(&input_adapter, &mut output_adapter, None)
            }
            Self::Fft(r) => r.process_into_buffer(&input_adapter, &mut output_adapter, None),
        }
    }

    fn input_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.input_frames_next(),
            Self::Fft(r) => r.input_frames_next(),
        }
    }

    fn output_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.output_frames_next(),
            Self::Fft(r) => r.output_frames_next(),
        }
    }

    fn set_resample_ratio(&mut self, ratio: f64, ramp: bool) -> Result<(), rubato::ResampleError> {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.set_resample_ratio(ratio, ramp),
            Self::Fft(r) => r.set_resample_ratio(ratio, ramp),
        }
    }
}

/// Configuration parameters for the resampler effect.
///
/// Contains all values needed to construct a [`ResamplerProcessor`].
#[derive(Setters)]
#[setters(prefix = "with_")]
pub struct ResamplerParams {
    /// Number of audio channels.
    pub channels: usize,
    /// Number of input frames per resampler processing block.
    pub chunk_size: usize,
    /// Shared atomic for dynamic host sample rate tracking.
    pub host_sample_rate: Arc<AtomicU32>,
    /// Shared atomic for dynamic playback rate (1.0 = normal speed).
    ///
    /// Affects the resampling ratio: `ratio = host_rate / (source_rate × playback_rate)`.
    /// At rate=2.0, audio plays at double speed with pitch shift (vinyl effect).
    pub playback_rate: Arc<AtomicF32>,
    /// Shared PCM pool for output buffers.
    pub pool: Option<PcmPool>,
    /// Quality preset controlling resampling algorithm.
    pub quality: ResamplerQuality,
    /// Initial source sample rate.
    pub source_sample_rate: u32,
}

impl ResamplerParams {
    /// Create resampler params with required runtime values and default settings.
    pub fn new(host_sample_rate: Arc<AtomicU32>, source_sample_rate: u32, channels: usize) -> Self {
        Self {
            channels,
            chunk_size: 4096,
            host_sample_rate,
            playback_rate: Arc::new(AtomicF32::new(1.0)),
            pool: None,
            quality: ResamplerQuality::default(),
            source_sample_rate,
        }
    }
}

/// Audio resampler that converts between source and host sample rates.
///
/// Monitors `host_sample_rate` (an `Arc<AtomicU32>`) and `playback_rate`
/// (an `Arc<AtomicF32>`) for dynamic changes.
/// When `host_sample_rate == 0` or equals `source_rate` and `playback_rate == 1.0`,
/// operates in passthrough mode.
pub struct ResamplerProcessor {
    channels: usize,
    chunk_size: usize,
    current_playback_rate: f64,
    current_ratio: f64,
    host_sample_rate: Arc<AtomicU32>,
    /// Accumulated input buffer (planar format).
    input_buffer: SmallVec<[Vec<f32>; 8]>,
    output_spec: PcmSpec,
    /// Shared atomic for dynamic playback rate tracking.
    playback_rate: Arc<AtomicF32>,
    /// Pool for interleave output buffers.
    pool: PcmPool,
    quality: ResamplerQuality,
    resampler: Option<ResamplerKind>,
    source_rate: u32,
    // Reusable temporary buffers
    temp_deinterleave: SmallVec<[Vec<f32>; 8]>,
    temp_input_slice: SmallVec<[Vec<f32>; 8]>,
    temp_output_all: SmallVec<[Vec<f32>; 8]>,
    temp_output_bufs: SmallVec<[Vec<f32>; 8]>,
}

impl ResamplerProcessor {
    /// Create a new resampler from configuration parameters.
    pub fn new(params: ResamplerParams) -> Self {
        let source_rate = params.source_sample_rate;
        let channels = params.channels;
        let host_sr = params.host_sample_rate.load(Ordering::Relaxed);
        let target_rate = if host_sr == 0 { source_rate } else { host_sr };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "channel count is always small"
        )]
        let output_spec = PcmSpec {
            channels: channels as u16,
            sample_rate: target_rate,
        };

        let initial_playback_rate = f64::from(params.playback_rate.load(Ordering::Relaxed));

        let mut processor = Self {
            channels,
            chunk_size: params.chunk_size,
            current_playback_rate: initial_playback_rate,
            current_ratio: 1.0,
            host_sample_rate: params.host_sample_rate,
            input_buffer: smallvec_new_vecs(channels),
            output_spec,
            playback_rate: params.playback_rate,
            pool: params.pool.unwrap_or_else(|| pcm_pool().clone()),
            quality: params.quality,
            resampler: None,
            source_rate,
            temp_deinterleave: smallvec_new_vecs(channels),
            temp_input_slice: smallvec_new_vecs(channels),
            temp_output_all: smallvec_new_vecs(channels),
            temp_output_bufs: smallvec_new_vecs(channels),
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
    pub fn flush_buffer(&mut self) -> Option<PcmChunk> {
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
            buf.extend(iter::repeat_n(0.0, padding_needed));
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

                #[expect(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "audio buffer size calculation: always positive and within usize range"
                )]
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
                Some(PcmChunk::new(
                    PcmMeta {
                        spec: self.output_spec,
                        ..Default::default()
                    },
                    interleaved,
                ))
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

    fn should_passthrough(source_rate: u32, target_rate: u32, playback_rate: f64) -> bool {
        (source_rate == target_rate || target_rate == 0) && (playback_rate - 1.0).abs() < 0.0001
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
                    8.0,
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
                    8.0,
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
        let target_rate = self.target_rate();
        self.output_spec.sample_rate = target_rate;
        let new_playback_rate = f64::from(self.playback_rate.load(Ordering::Relaxed)).max(0.01);
        let should_pt = Self::should_passthrough(self.source_rate, target_rate, new_playback_rate);
        let currently_pt = self.is_passthrough();
        let new_ratio = self.ratio_for_target(target_rate);
        let ratio_changed = (new_ratio - self.current_ratio).abs() > 0.0001;

        if should_pt {
            self.switch_to_passthrough(target_rate, currently_pt);
            self.current_playback_rate = new_playback_rate;
            return;
        }

        if self.try_update_ratio(target_rate, new_ratio, currently_pt, ratio_changed) {
            self.current_playback_rate = new_playback_rate;
            return;
        }

        if currently_pt || self.resampler.is_none() || ratio_changed {
            self.recreate_resampler(target_rate, new_ratio);
            self.current_playback_rate = new_playback_rate;
        }
    }

    fn target_rate(&self) -> u32 {
        let host_sr = self.host_sample_rate.load(Ordering::Relaxed);
        if host_sr == 0 {
            self.source_rate
        } else {
            host_sr
        }
    }

    fn ratio_for_target(&self, target_rate: u32) -> f64 {
        let rate = f64::from(self.playback_rate.load(Ordering::Relaxed)).max(0.01);
        if self.source_rate > 0 {
            f64::from(target_rate) / (f64::from(self.source_rate) * rate)
        } else {
            1.0 / rate
        }
    }

    fn switch_to_passthrough(&mut self, target_rate: u32, currently_pt: bool) {
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
    }

    fn try_update_ratio(
        &mut self,
        target_rate: u32,
        new_ratio: f64,
        currently_pt: bool,
        ratio_changed: bool,
    ) -> bool {
        if currently_pt || !ratio_changed {
            return false;
        }
        let Some(ref mut resampler) = self.resampler else {
            return false;
        };

        // rubato 1.0.1 panics with ramp=true on both sinc (neon) and poly
        // backends. Use instant transition; chunk boundary (~93ms) provides
        // sufficient smoothness for playback rate changes.
        match resampler.set_resample_ratio(new_ratio, false) {
            Ok(()) => {
                debug!(
                    new_ratio,
                    source_rate = self.source_rate,
                    target_rate,
                    "Resampler ratio updated dynamically"
                );
                self.current_ratio = new_ratio;
                true
            }
            Err(e) => {
                debug!(err = %e, "Failed to update ratio dynamically, recreating");
                self.resampler = None;
                false
            }
        }
    }

    fn recreate_resampler(&mut self, target_rate: u32, new_ratio: f64) {
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
    fn resample(&mut self, chunk: &PcmChunk) -> Option<PcmChunk> {
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
        Some(PcmChunk::new(
            PcmMeta {
                spec: self.output_spec,
                ..Default::default()
            },
            interleaved,
        ))
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
        let num_channels = NonZeroUsize::new(self.channels).expect("channels must be > 0");
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
    fn interleave(&self, planar: &[Vec<f32>]) -> PcmBuf {
        if planar.is_empty() || planar[0].is_empty() {
            return self.pool.get();
        }

        let frames = planar[0].len();
        let total = frames * self.channels;

        // Get buffer from pool — returned as PooledOwned for auto-recycling.
        // PcmPool has unlimited budget so ensure_len never fails in practice.
        let mut result = self.pool.get();
        if let Err(_e) = result.ensure_len(total) {
            tracing::warn!("PCM pool budget exhausted during resampling");
            return self.pool.get();
        }

        // Use fast_interleave (SIMD-optimized)
        let num_channels = NonZeroUsize::new(self.channels).expect("channels must be > 0");
        interleave_variable(planar, 0..frames, &mut result[..], num_channels);

        result
    }
}

/// Create a `SmallVec` of empty Vecs for each channel.
#[cfg_attr(feature = "perf", hotpath::measure)]
fn smallvec_new_vecs(channels: usize) -> SmallVec<[Vec<f32>; 8]> {
    (0..channels).map(|_| Vec::new()).collect()
}

impl AudioEffect for ResamplerProcessor {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        let chunk_rate = chunk.spec().sample_rate;
        let chunk_channels = chunk.spec().channels as usize;

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
            #[expect(
                clippy::cast_possible_truncation,
                reason = "channel count is always small"
            )]
            let channels_u16 = chunk_channels as u16;
            self.output_spec.channels = channels_u16;
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
            out.meta.spec = self.output_spec;
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

    fn flush(&mut self) -> Option<PcmChunk> {
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
    use kithara_bufpool::pcm_pool;
    use kithara_test_utils::kithara;

    use super::*;

    fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    fn make_host_rate(rate: u32) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(rate))
    }

    fn params(host_sr: Arc<AtomicU32>, source_rate: u32, channels: usize) -> ResamplerParams {
        ResamplerParams::new(host_sr, source_rate, channels)
    }

    fn params_with_rate(
        host_sr: Arc<AtomicU32>,
        source_rate: u32,
        channels: usize,
        rate: Arc<AtomicF32>,
    ) -> ResamplerParams {
        ResamplerParams::new(host_sr, source_rate, channels).with_playback_rate(rate)
    }

    fn params_with_quality(
        host_sr: Arc<AtomicU32>,
        source_rate: u32,
        channels: usize,
        quality: ResamplerQuality,
    ) -> ResamplerParams {
        ResamplerParams::new(host_sr, source_rate, channels).with_quality(quality)
    }

    #[kithara::test]
    #[case::same_rate(44100, 44100, true)]
    #[case::host_zero(0, 44100, true)]
    #[case::different_rate(44100, 48000, false)]
    fn test_passthrough_mode(
        #[case] host_rate: u32,
        #[case] source_rate: u32,
        #[case] expected: bool,
    ) {
        let processor = ResamplerProcessor::new(params(make_host_rate(host_rate), source_rate, 2));
        assert_eq!(processor.is_passthrough(), expected);
    }

    #[kithara::test]
    fn test_passthrough_processing() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 44100, 2));

        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.1, 0.2, 0.3, 0.4],
        );

        let result = processor.process(chunk.clone());
        assert!(result.is_some());
        let out = result.unwrap();
        assert_eq!(out.pcm, chunk.pcm);
        assert_eq!(out.spec().sample_rate, 44100);
    }

    #[kithara::test]
    fn test_output_spec() {
        let processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));
        assert_eq!(processor.output_spec.sample_rate, 44100);
        assert_eq!(processor.output_spec.channels, 2);
    }

    #[kithara::test]
    fn test_dynamic_host_rate_change() {
        let host_sr = make_host_rate(44100);
        let mut processor = ResamplerProcessor::new(params(host_sr.clone(), 44100, 2));
        assert!(processor.is_passthrough());

        // Change host sample rate dynamically
        host_sr.store(48000, Ordering::Relaxed);

        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.1; 2048],
        );
        let _ = processor.process(chunk);

        assert!(!processor.is_passthrough());
    }

    #[kithara::test]
    fn test_accumulates_small_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let small_chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 48000,
            },
            vec![0.1; 100],
        );

        let result = processor.process(small_chunk);
        assert!(result.is_none());
        assert_eq!(processor.input_buffer[0].len(), 50);
    }

    #[kithara::test]
    fn test_processes_large_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let large_chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 48000,
            },
            vec![0.1; 16384],
        );

        let result = processor.process(large_chunk);
        assert!(result.is_some());
        assert!(!result.unwrap().pcm.is_empty());
    }

    #[kithara::test]
    fn test_source_rate_change_updates_dynamically() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let chunk1 = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 48000,
            },
            vec![0.1; 4096],
        );
        let _ = processor.process(chunk1);
        assert_eq!(processor.source_rate, 48000);

        // Source rate changed (e.g. ABR switch) — resampler should update dynamically
        let chunk2 = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.2; 4096],
        );
        let _ = processor.process(chunk2);
        assert_eq!(processor.source_rate, 44100);
        // Resampler should now be in passthrough (44100 → 44100)
        assert!(processor.is_passthrough());
    }

    #[kithara::test]
    fn test_no_data_loss_across_chunks() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let mut total_input_frames = 0;
        let mut total_output_frames = 0;

        for _ in 0..10 {
            let chunk = test_chunk(
                PcmSpec {
                    channels: 2,
                    sample_rate: 48000,
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

    #[kithara::test]
    fn test_audio_effect_trait() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 44100, 2));

        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
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

    // Playback rate tests

    #[kithara::test]
    fn test_playback_rate_2x_halves_output() {
        let rate = Arc::new(AtomicF32::new(2.0));
        let mut processor =
            ResamplerProcessor::new(params_with_rate(make_host_rate(44100), 44100, 2, rate));
        assert!(!processor.is_passthrough()); // rate!=1.0 → active
        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
        let output_frames = result.unwrap().frames();
        // 2x speed → ~half output frames (input: 8192 frames → ~4096 output)
        assert!(output_frames < 5000, "Expected ~4096, got {output_frames}");
    }

    #[kithara::test]
    fn test_playback_rate_half_doubles_output() {
        let rate = Arc::new(AtomicF32::new(0.5));
        let mut processor =
            ResamplerProcessor::new(params_with_rate(make_host_rate(44100), 44100, 2, rate));
        assert!(!processor.is_passthrough());
        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
        let output_frames = result.unwrap().frames();
        // 0.5x speed → ~double output frames (input: 8192 frames → ~16384 output)
        assert!(
            output_frames > 12000,
            "Expected ~16384, got {output_frames}"
        );
    }

    #[kithara::test]
    fn test_playback_rate_1x_same_rate_passthrough() {
        let rate = Arc::new(AtomicF32::new(1.0));
        let processor =
            ResamplerProcessor::new(params_with_rate(make_host_rate(44100), 44100, 2, rate));
        assert!(processor.is_passthrough()); // rate=1.0 + source==host → passthrough
    }

    #[kithara::test]
    fn test_playback_rate_dynamic_change() {
        let rate = Arc::new(AtomicF32::new(1.0));
        let mut processor = ResamplerProcessor::new(params_with_rate(
            make_host_rate(44100),
            44100,
            2,
            Arc::clone(&rate),
        ));
        assert!(processor.is_passthrough());
        rate.store(2.0, Ordering::Relaxed);
        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.1; 16384],
        );
        let _ = processor.process(chunk);
        assert!(!processor.is_passthrough()); // should activate
    }

    // Quality-specific tests

    #[kithara::test]
    #[case::fast(ResamplerQuality::Fast)]
    #[case::normal(ResamplerQuality::Normal)]
    #[case::good(ResamplerQuality::Good)]
    #[case::high(ResamplerQuality::High)]
    #[case::maximum(ResamplerQuality::Maximum)]
    fn test_quality_resamples(#[case] quality: ResamplerQuality) {
        let mut processor = ResamplerProcessor::new(params_with_quality(
            make_host_rate(44100),
            48000,
            2,
            quality,
        ));
        assert!(!processor.is_passthrough());

        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 48000,
            },
            vec![0.1; 16384],
        );
        let result = processor.process(chunk);
        assert!(result.is_some());
    }
}
