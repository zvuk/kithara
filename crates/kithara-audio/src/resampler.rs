use std::{
    iter,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use bon::Builder;
use fast_interleave::{deinterleave_variable, interleave_variable};
use kithara_bufpool::{PcmBuf, PcmPool};
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

impl From<ResamplerQuality> for SincInterpolationParameters {
    fn from(quality: ResamplerQuality) -> Self {
        /// Oversampling factor for good/high quality sinc filters.
        const OVERSAMPLING_HIGH: usize = 256;
        /// Oversampling factor for normal quality sinc filter.
        const OVERSAMPLING_NORMAL: usize = 128;
        /// Cutoff frequency ratio for sinc filters.
        const CUTOFF: f32 = 0.95;
        /// Sinc filter length for good quality (128-tap).
        const LEN_GOOD: usize = 128;
        /// Sinc filter length for high quality (256-tap).
        const LEN_HIGH: usize = 256;
        /// Sinc filter length for normal quality (64-tap).
        const LEN_NORMAL: usize = 64;

        match quality {
            ResamplerQuality::Good => Self {
                sinc_len: LEN_GOOD,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: OVERSAMPLING_HIGH,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplerQuality::High => Self {
                sinc_len: LEN_HIGH,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: OVERSAMPLING_HIGH,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplerQuality::Normal | ResamplerQuality::Fast | ResamplerQuality::Maximum => Self {
                sinc_len: LEN_NORMAL,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: OVERSAMPLING_NORMAL,
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
    // ast-grep-ignore: idioms.match-self-conversion
    fn input_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.input_frames_next(),
            Self::Fft(r) => r.input_frames_next(),
        }
    }
    // ast-grep-ignore: idioms.match-self-conversion
    fn output_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.output_frames_next(),
            Self::Fft(r) => r.output_frames_next(),
        }
    }

    /// Worst-case input block across the whole ratio-adjustment window
    /// (`MAX_RATIO_ADJUSTMENT`). Pre-sizing input scratch to this means a
    /// live ratio change (DJ rate sweep) never reallocates on the
    /// produce-core.
    // ast-grep-ignore: idioms.match-self-conversion
    fn input_frames_max(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.input_frames_max(),
            Self::Fft(r) => r.input_frames_max(),
        }
    }

    /// Worst-case output block across the whole ratio-adjustment window.
    // ast-grep-ignore: idioms.match-self-conversion
    fn output_frames_max(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.output_frames_max(),
            Self::Fft(r) => r.output_frames_max(),
        }
    }

    fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), rubato::ResampleError> {
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
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerParams {
    /// Shared atomic for dynamic host sample rate tracking.
    pub host_sample_rate: Arc<AtomicU32>,
    /// Shared atomic for dynamic playback rate (1.0 = normal speed).
    ///
    /// Affects the resampling ratio: `ratio = host_rate / (source_rate × playback_rate)`.
    /// At rate=2.0, audio plays at double speed with pitch shift (vinyl effect).
    #[builder(default = Arc::new(AtomicF32::new(1.0)))]
    pub playback_rate: Arc<AtomicF32>,
    /// Shared PCM pool for output buffers.
    pub pool: Option<PcmPool>,
    /// Quality preset controlling resampling algorithm.
    #[builder(default)]
    pub quality: ResamplerQuality,
    /// Initial source sample rate.
    pub source_sample_rate: u32,
    /// Number of audio channels.
    pub channels: usize,
    /// Number of input frames per resampler processing block.
    #[builder(default = ResamplerProcessor::DEFAULT_CHUNK_SIZE)]
    pub chunk_size: usize,
}

impl ResamplerParams {
    /// Create resampler params with required runtime values and default settings.
    pub fn new(host_sample_rate: Arc<AtomicU32>, source_sample_rate: u32, channels: usize) -> Self {
        Self::builder()
            .host_sample_rate(host_sample_rate)
            .source_sample_rate(source_sample_rate)
            .channels(channels)
            .build()
    }
}

/// Audio resampler that converts between source and host sample rates.
///
/// Monitors `host_sample_rate` (an `Arc<AtomicU32>`) and `playback_rate`
/// (an `Arc<AtomicF32>`) for dynamic changes.
/// When `host_sample_rate == 0` or equals `source_rate` and `playback_rate == 1.0`,
/// operates in passthrough mode.
pub struct ResamplerProcessor {
    host_sample_rate: Arc<AtomicU32>,
    /// Shared atomic for dynamic playback rate tracking.
    playback_rate: Arc<AtomicF32>,
    /// Most recently observed input `PcmMeta`. Carried over to each
    /// resampled output chunk so the timeline still gets the decoder's
    /// authoritative `timestamp` / `end_timestamp` after rate
    /// conversion (rubato changes frame counts but not wall-clock
    /// duration). `None` until the first input chunk arrives.
    last_input_meta: Option<PcmMeta>,
    resampler: Option<ResamplerKind>,
    /// Pool for interleave output buffers.
    pool: PcmPool,
    output_spec: PcmSpec,
    quality: ResamplerQuality,
    /// Accumulated input buffer (planar format).
    input_buffer: SmallVec<[Vec<f32>; 8]>,
    temp_deinterleave: SmallVec<[Vec<f32>; 8]>,
    temp_input_slice: SmallVec<[Vec<f32>; 8]>,
    temp_output_all: SmallVec<[Vec<f32>; 8]>,
    temp_output_bufs: SmallVec<[Vec<f32>; 8]>,
    current_playback_rate: f64,
    current_ratio: f64,
    source_rate: u32,
    channels: usize,
    chunk_size: usize,
}

impl ResamplerProcessor {
    /// Default resampler chunk size in frames.
    const DEFAULT_CHUNK_SIZE: usize = 4096;

    /// Sub-chunk count for FFT resampler.
    const FFT_SUB_CHUNKS: usize = 2;

    /// Maximum ratio adjustment factor for async resamplers.
    const MAX_RATIO_ADJUSTMENT: f64 = 8.0;

    /// Minimum playback rate to avoid division by zero or extreme ratios.
    const MIN_PLAYBACK_RATE: f64 = 0.01;

    /// Passthrough detection tolerance for playback rate.
    const PASSTHROUGH_TOLERANCE: f64 = 0.0001;

    /// Create a new resampler from configuration parameters.
    pub fn new(params: ResamplerParams) -> Self {
        let source_rate = params.source_sample_rate;
        let channels = params.channels;
        let host_sr = params.host_sample_rate.load(Ordering::Relaxed);
        let target_rate = if host_sr == 0 { source_rate } else { host_sr };
        let output_spec = PcmSpec {
            channels: u16::try_from(channels).unwrap_or(u16::MAX),
            sample_rate: target_rate,
        };

        let initial_playback_rate = f64::from(params.playback_rate.load(Ordering::Relaxed));

        let mut processor = Self {
            channels,
            output_spec,
            source_rate,
            chunk_size: params.chunk_size,
            current_playback_rate: initial_playback_rate,
            current_ratio: 1.0,
            host_sample_rate: params.host_sample_rate,
            input_buffer: smallvec_new_vecs(channels),
            playback_rate: params.playback_rate,
            pool: params.pool.unwrap_or_else(|| PcmPool::default().clone()),
            quality: params.quality,
            resampler: None,
            temp_deinterleave: smallvec_new_vecs(channels),
            temp_input_slice: smallvec_new_vecs(channels),
            temp_output_all: smallvec_new_vecs(channels),
            temp_output_bufs: smallvec_new_vecs(channels),
            last_input_meta: None,
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

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn append_to_buffer(&mut self, interleaved: &[f32]) {
        if interleaved.is_empty() {
            return;
        }

        let frames = interleaved.len() / self.channels;

        if self.temp_deinterleave.len() < self.channels {
            self.temp_deinterleave.resize_with(self.channels, Vec::new);
        }

        for buf in &mut self.temp_deinterleave[..self.channels] {
            buf.resize(frames, 0.0);
        }

        let num_channels = NonZeroUsize::new(self.channels).expect("channels must be > 0");
        deinterleave_variable(
            interleaved,
            num_channels,
            &mut self.temp_deinterleave[..self.channels],
            0..frames,
        );

        for ch in 0..self.channels {
            self.input_buffer[ch].extend_from_slice(&self.temp_deinterleave[ch][..frames]);
        }
    }

    /// Assemble the final `PcmChunk` from a successful flush `process_block`.
    fn build_flush_output(&mut self, buffered: usize, out_len: usize) -> Option<PcmChunk> {
        for buf in &mut self.input_buffer {
            buf.clear();
        }

        let frames_f64 =
            (num_traits::cast::AsPrimitive::<f64>::as_(buffered) * self.current_ratio).ceil();
        let actual_output_frames =
            num_traits::cast::ToPrimitive::to_usize(&frames_f64).unwrap_or(usize::MAX);
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
        let mut meta = self.last_input_meta.unwrap_or_default();
        meta.spec = self.output_spec;
        let frame_count = interleaved
            .len()
            .checked_div(self.channels.max(1))
            .unwrap_or(0);
        let out_frames = u32::try_from(frame_count).unwrap_or(u32::MAX);
        meta.frames = out_frames;
        Some(PcmChunk::new(meta, interleaved))
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
                    Self::MAX_RATIO_ADJUSTMENT,
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
                    Self::MAX_RATIO_ADJUSTMENT,
                    &SincInterpolationParameters::from(quality),
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
                    Self::FFT_SUB_CHUNKS,
                    channels,
                    FixedSync::Input,
                )?;
                Ok(ResamplerKind::Fft(Box::new(fft)))
            }
        }
    }

    /// Drain accumulated input into `temp_output_all` one block at a time.
    /// Returns `false` if the underlying resampler errored.
    fn drive_resample_loop(&mut self, channels: usize, input_frames: usize) -> bool {
        while self.input_buffer[0].len() >= input_frames {
            let output_frames = {
                let Some(resampler) = self.resampler.as_ref() else {
                    return false;
                };
                resampler.output_frames_next()
            };

            let result = {
                let Some(mut resampler) = self.resampler.take() else {
                    return false;
                };
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
                    return false;
                }
            }
        }
        true
    }

    fn ensure_temp_buffers(&mut self, channels: usize) {
        if self.temp_input_slice.len() < channels {
            self.temp_input_slice.resize_with(channels, Vec::new);
        }
        if self.temp_output_bufs.len() < channels {
            self.temp_output_bufs.resize_with(channels, Vec::new);
        }
    }

    /// Turn the accumulated planar output into an interleaved `PcmChunk`.
    fn finalize_resample_chunk(&self, _input_frames: usize) -> Option<PcmChunk> {
        if self.temp_output_all[0].is_empty() {
            return None;
        }

        let interleaved = self.interleave(&self.temp_output_all);
        let mut meta = self.last_input_meta.unwrap_or_default();
        meta.spec = self.output_spec;
        let frame_count = interleaved
            .len()
            .checked_div(self.channels.max(1))
            .unwrap_or(0);
        let out_frames = u32::try_from(frame_count).unwrap_or(u32::MAX);
        meta.frames = out_frames;
        Some(PcmChunk::new(meta, interleaved))
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

        self.pad_input_for_flush(input_frames, buffered);

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
            Ok(out_len) => self.build_flush_output(buffered, out_len),
            Err(e) => {
                trace!(err = %e, "Resampler flush error");
                None
            }
        }
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn interleave(&self, planar: &[Vec<f32>]) -> PcmBuf {
        if planar.is_empty() || planar[0].is_empty() {
            return self.pool.get();
        }

        let frames = planar[0].len();
        let total = frames * self.channels;

        let mut result = self.pool.get();
        if let Err(_e) = result.ensure_len(total) {
            tracing::warn!("PCM pool budget exhausted during resampling");
            return self.pool.get();
        }

        let num_channels = NonZeroUsize::new(self.channels).expect("channels must be > 0");
        interleave_variable(planar, 0..frames, &mut result[..], num_channels);

        result
    }

    fn is_passthrough(&self) -> bool {
        self.resampler.is_none()
    }

    /// Pad input buffer with zeros so a final block can be processed.
    fn pad_input_for_flush(&mut self, input_frames: usize, buffered: usize) {
        let padding_needed = input_frames.saturating_sub(buffered);
        self.input_buffer
            .iter_mut()
            .for_each(|buf| buf.extend(iter::repeat_n(0.0, padding_needed)));
        debug!(buffered, padding_needed, "Flushing resampler buffer");
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

    fn ratio_for_target(&self, target_rate: u32) -> f64 {
        let rate =
            f64::from(self.playback_rate.load(Ordering::Relaxed)).max(Self::MIN_PLAYBACK_RATE);
        if self.source_rate > 0 {
            f64::from(target_rate) / (f64::from(self.source_rate) * rate)
        } else {
            1.0 / rate
        }
    }

    fn recreate_resampler(&mut self, target_rate: u32, new_ratio: f64) {
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
                self.presize_scratch();
            }
            Err(e) => {
                debug!(err = %e, "Failed to create resampler, staying in current mode");
            }
        }
    }

    /// Reserve steady-state capacity for every per-chunk scratch buffer up
    /// front (construction / resampler-recreate), so the produce-core never
    /// reallocates them mid-playback. Sizes come from the resampler's
    /// worst-case input/output blocks (`*_frames_max`, which already fold in
    /// the `MAX_RATIO_ADJUSTMENT` window), so even a live DJ rate sweep stays
    /// allocation-free. No-op in passthrough — there is no resampler and the
    /// scratch is unused. Pure capacity reservation: output is bit-exact.
    fn presize_scratch(&mut self) {
        let Some((in_max, out_max, in_next)) = self.resampler.as_ref().map(|r| {
            (
                r.input_frames_max(),
                r.output_frames_max(),
                r.input_frames_next().max(1),
            )
        }) else {
            return;
        };
        let channels = self.channels;
        let chunk = self.chunk_size;
        // `input_buffer` carries a sub-block residual plus one freshly
        // appended decoder chunk before the next drain.
        let in_buf_cap = in_max + chunk;
        // `temp_output_all` gathers every block emitted from one `process`
        // call before interleaving.
        let blocks = chunk.div_ceil(in_next).saturating_add(1);
        let out_all_cap = out_max.saturating_mul(blocks);

        reserve_channel_scratch(&mut self.input_buffer, channels, in_buf_cap);
        reserve_channel_scratch(&mut self.temp_deinterleave, channels, chunk.max(in_max));
        reserve_channel_scratch(&mut self.temp_input_slice, channels, in_max);
        reserve_channel_scratch(&mut self.temp_output_bufs, channels, out_max);
        reserve_channel_scratch(&mut self.temp_output_all, channels, out_all_cap);
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn resample(&mut self, chunk: &PcmChunk) -> Option<PcmChunk> {
        self.resampler.as_ref()?;

        self.last_input_meta = Some(chunk.meta);
        self.append_to_buffer(&chunk.pcm);

        let input_frames = self.resampler.as_ref()?.input_frames_next();
        let channels = self.channels;

        if self.input_buffer[0].len() < input_frames {
            return None;
        }

        self.ensure_temp_buffers(channels);
        self.reset_output_accumulator(channels);

        let loop_ok = self.drive_resample_loop(channels, input_frames);
        if !loop_ok {
            return None;
        }

        self.finalize_resample_chunk(input_frames)
    }

    /// Clear and resize the per-channel output accumulator used by `resample`.
    fn reset_output_accumulator(&mut self, channels: usize) {
        if self.temp_output_all.len() < channels {
            self.temp_output_all.resize_with(channels, Vec::new);
        }
        for buf in &mut self.temp_output_all {
            buf.clear();
        }
    }

    fn should_passthrough(source_rate: u32, target_rate: u32, playback_rate: f64) -> bool {
        (source_rate == target_rate || target_rate == 0)
            && (playback_rate - 1.0).abs() < Self::PASSTHROUGH_TOLERANCE
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

    fn target_rate(&self) -> u32 {
        let host_sr = self.host_sample_rate.load(Ordering::Relaxed);
        if host_sr == 0 {
            self.source_rate
        } else {
            host_sr
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

    fn update_resampler_if_needed(&mut self) {
        let target_rate = self.target_rate();
        self.output_spec.sample_rate = target_rate;
        let new_playback_rate =
            f64::from(self.playback_rate.load(Ordering::Relaxed)).max(Self::MIN_PLAYBACK_RATE);
        let should_pt = Self::should_passthrough(self.source_rate, target_rate, new_playback_rate);
        let currently_pt = self.is_passthrough();
        let new_ratio = self.ratio_for_target(target_rate);
        let ratio_changed = (new_ratio - self.current_ratio).abs() > Self::PASSTHROUGH_TOLERANCE;

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
}

/// Create a `SmallVec` of empty Vecs for each channel.
#[cfg_attr(feature = "perf", hotpath::measure)]
fn smallvec_new_vecs(channels: usize) -> SmallVec<[Vec<f32>; 8]> {
    (0..channels).map(|_| Vec::new()).collect()
}

/// Reserve `cap` floats of capacity on each per-channel scratch Vec,
/// growing the outer `SmallVec` to `channels` first. Pure capacity — the
/// lengths and contents are untouched, so resampler output stays bit-exact.
fn reserve_channel_scratch(bufs: &mut SmallVec<[Vec<f32>; 8]>, channels: usize, cap: usize) {
    if bufs.len() < channels {
        bufs.resize_with(channels, Vec::new);
    }
    for buf in bufs.iter_mut().take(channels) {
        if buf.capacity() < cap {
            buf.reserve(cap.saturating_sub(buf.len()));
        }
    }
}

impl ResamplerProcessor {
    /// Apply incoming chunk spec changes in-place (channels first, then rate).
    fn apply_source_spec_changes(&mut self, chunk_channels: usize, chunk_rate: u32) {
        if chunk_channels != self.channels {
            self.handle_channel_change(chunk_channels, chunk_rate);
        } else if chunk_rate != self.source_rate {
            self.handle_source_rate_change(chunk_rate);
        }
    }

    /// React to a changed channel count in incoming chunks (ABR switch).
    fn handle_channel_change(&mut self, chunk_channels: usize, chunk_rate: u32) {
        self.channels = chunk_channels;
        self.source_rate = chunk_rate;
        self.input_buffer = smallvec_new_vecs(chunk_channels);
        self.temp_input_slice = smallvec_new_vecs(chunk_channels);
        self.temp_output_bufs = smallvec_new_vecs(chunk_channels);
        self.temp_output_all = smallvec_new_vecs(chunk_channels);
        self.temp_deinterleave = smallvec_new_vecs(chunk_channels);
        let channels_u16 = u16::try_from(chunk_channels).unwrap_or(u16::MAX);
        self.output_spec.channels = channels_u16;
        self.resampler = None;
    }

    /// React to a changed source sample rate while channel count is stable.
    fn handle_source_rate_change(&mut self, chunk_rate: u32) {
        self.source_rate = chunk_rate;
        for buf in &mut self.input_buffer {
            buf.clear();
        }
    }

    /// Passthrough path: take the chunk, stamp the current output spec, return it.
    fn passthrough_chunk(&self, mut chunk: PcmChunk) -> PcmChunk {
        chunk.meta.spec = self.output_spec;
        chunk
    }
}

impl AudioEffect for ResamplerProcessor {
    fn flush(&mut self) -> Option<PcmChunk> {
        self.flush_buffer()
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        let chunk_rate = chunk.spec().sample_rate;
        let chunk_channels = chunk.spec().channels as usize;

        self.apply_source_spec_changes(chunk_channels, chunk_rate);
        self.update_resampler_if_needed();
        if self.is_passthrough() {
            return Some(self.passthrough_chunk(chunk));
        }
        self.resample(&chunk)
    }

    fn reset(&mut self) {
        for buf in &mut self.input_buffer {
            buf.clear();
        }
        self.resampler = None;
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;
    use kithara_test_utils::kithara;

    use super::*;

    fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            PcmPool::default().attach(pcm),
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
        ResamplerParams::builder()
            .host_sample_rate(host_sr)
            .source_sample_rate(source_rate)
            .channels(channels)
            .playback_rate(rate)
            .build()
    }

    fn params_with_quality(
        host_sr: Arc<AtomicU32>,
        source_rate: u32,
        channels: usize,
        quality: ResamplerQuality,
    ) -> ResamplerParams {
        ResamplerParams::builder()
            .host_sample_rate(host_sr)
            .source_sample_rate(source_rate)
            .channels(channels)
            .quality(quality)
            .build()
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
    #[case::small(100, false)]
    #[case::large(16384, true)]
    fn test_chunk_size_threshold(#[case] interleaved_len: usize, #[case] produces_output: bool) {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let chunk = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 48000,
            },
            vec![0.1; interleaved_len],
        );

        let result = processor.process(chunk);
        if produces_output {
            assert!(result.is_some());
            assert!(!result.unwrap().pcm.is_empty());
        } else {
            assert!(result.is_none());
            assert_eq!(processor.input_buffer[0].len(), interleaved_len / 2);
        }
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

        let chunk2 = test_chunk(
            PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            vec![0.2; 4096],
        );
        let _ = processor.process(chunk2);
        assert_eq!(processor.source_rate, 44100);
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

        let expected = usize::try_from(i64::from(total_input_frames) * 44100 / 48000)
            .expect("test bounded result fits usize");
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

        let result = AudioEffect::process(&mut processor, chunk);
        assert!(result.is_some());

        AudioEffect::reset(&mut processor);
        assert!(processor.input_buffer[0].is_empty());
    }

    #[kithara::test]
    #[case::rate_2x_halves(2.0, true, 5000)]
    #[case::rate_half_doubles(0.5, false, 12000)]
    fn test_playback_rate_scales_output(
        #[case] rate_value: f32,
        #[case] expect_less_than: bool,
        #[case] threshold: usize,
    ) {
        let rate = Arc::new(AtomicF32::new(rate_value));
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
        if expect_less_than {
            assert!(
                output_frames < threshold,
                "Expected < {threshold}, got {output_frames}"
            );
        } else {
            assert!(
                output_frames > threshold,
                "Expected > {threshold}, got {output_frames}"
            );
        }
    }

    #[kithara::test]
    fn test_playback_rate_1x_same_rate_passthrough() {
        let rate = Arc::new(AtomicF32::new(1.0));
        let processor =
            ResamplerProcessor::new(params_with_rate(make_host_rate(44100), 44100, 2, rate));
        assert!(processor.is_passthrough());
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
        assert!(!processor.is_passthrough());
    }

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
