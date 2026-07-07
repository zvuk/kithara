use std::{
    iter,
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use bon::Builder;
use fast_interleave::{deinterleave_variable, interleave_variable};
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{
    PcmChunk, PcmMeta, PcmSpec, Resampler, ResamplerBackend, ResamplerError, ResamplerOptions,
    ResamplerQuality, create_resampler,
};
use smallvec::SmallVec;
use tracing::{debug, info, trace};

use crate::traits::AudioEffect;

/// Configuration parameters for the resampler effect.
///
/// Contains all values needed to construct a [`ResamplerProcessor`].
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerParams {
    /// Shared atomic for dynamic host sample rate tracking.
    pub host_sample_rate: Arc<AtomicU32>,
    /// Shared PCM pool for output buffers.
    pub pool: Option<PcmPool>,
    /// Quality preset controlling resampling algorithm.
    #[builder(default)]
    pub quality: ResamplerQuality,
    /// Initial source sample rate.
    pub source_sample_rate: u32,
    /// Number of audio channels.
    pub channels: usize,
    /// Resampler implementation tunables.
    #[builder(default)]
    pub options: ResamplerOptions,
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
/// Monitors `host_sample_rate` (an `Arc<AtomicU32>`) for dynamic changes.
/// When `host_sample_rate == 0` or equals `source_rate`, operates in
/// passthrough mode.
pub struct ResamplerProcessor {
    host_sample_rate: Arc<AtomicU32>,
    /// Last input metadata; resampled output keeps decoder timestamps.
    last_input_meta: Option<PcmMeta>,
    resampler: Option<Box<dyn Resampler>>,
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
    current_ratio: f64,
    options: ResamplerOptions,
    source_rate: u32,
    channels: usize,
    missing_backend_logged: bool,
}

impl ResamplerProcessor {
    /// Create a new resampler from configuration parameters.
    pub fn new(params: ResamplerParams) -> Self {
        let source_rate = params.source_sample_rate;
        let channels = params.channels;
        let options = params.options;
        let host_sr = params.host_sample_rate.load(Ordering::Relaxed);
        let target_rate = if host_sr == 0 { source_rate } else { host_sr };
        // target_rate is always non-zero: either source_rate (non-zero by upstream contract)
        // or host_sr (only used when != 0). The MIN fallback can never fire.
        let nz_target = NonZeroU32::new(target_rate).unwrap_or(NonZeroU32::MIN);
        let output_spec = PcmSpec::new(u16::try_from(channels).unwrap_or(u16::MAX), nz_target);
        let mut processor = Self {
            channels,
            output_spec,
            source_rate,
            current_ratio: 1.0,
            options,
            host_sample_rate: params.host_sample_rate,
            input_buffer: smallvec_new_vecs(channels),
            pool: params.pool.unwrap_or_else(|| PcmPool::default().clone()),
            quality: params.quality,
            resampler: None,
            temp_deinterleave: smallvec_new_vecs(channels),
            temp_input_slice: smallvec_new_vecs(channels),
            temp_output_all: smallvec_new_vecs(channels),
            temp_output_bufs: smallvec_new_vecs(channels),
            last_input_meta: None,
            missing_backend_logged: false,
        };

        processor.update_resampler_if_needed();

        info!(
            source_rate,
            host_sr = host_sr,
            target_rate,
            channels,
            active = !processor.is_passthrough(),
            quality = ?params.quality,
            chunk_size = options.chunk_size,
            passthrough_tolerance = options.passthrough_tolerance,
            max_ratio_adjustment = options.max_ratio_adjustment,
            "Resampler initialized"
        );

        processor
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn append_to_buffer(&mut self, interleaved: &[f32]) {
        if interleaved.is_empty() {
            return;
        }

        let Some(num_channels) = NonZeroUsize::new(self.channels) else {
            return;
        };
        let frames = interleaved.len() / num_channels.get();

        if self.temp_deinterleave.len() < self.channels {
            self.temp_deinterleave.resize_with(self.channels, Vec::new);
        }

        for buf in &mut self.temp_deinterleave[..self.channels] {
            buf.resize(frames, 0.0);
        }

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
                let res = self.process_block(resampler.as_mut(), input_frames, output_frames);
                self.resampler = Some(resampler);
                res
            };

            match result {
                Ok((consumed, out_len)) => {
                    if consumed > input_frames {
                        trace!(
                            consumed,
                            input_frames, "Resampler consumed more frames than provided"
                        );
                        return false;
                    }
                    for ch in 0..channels {
                        let src = &self.temp_output_bufs[ch][..out_len];
                        self.temp_output_all[ch].extend_from_slice(src);
                    }

                    if consumed > 0 {
                        for buf in &mut self.input_buffer {
                            buf.drain(..consumed);
                        }
                    }

                    if consumed == 0 || self.input_buffer[0].len() < input_frames {
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
            let res = self.process_block(resampler.as_mut(), input_frames, output_frames);
            self.resampler = Some(resampler);
            res
        };

        match result {
            Ok((_, out_len)) => self.build_flush_output(buffered, out_len),
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

        let Some(num_channels) = NonZeroUsize::new(self.channels) else {
            return self.pool.get();
        };
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

    /// Reserve steady-state capacity for every per-chunk scratch buffer up
    /// front (construction / resampler-recreate), so the produce-core never
    /// reallocates them mid-playback. Sizes come from the resampler's
    /// worst-case input/output blocks (`*_frames_max`, which already fold in
    /// the configured max-ratio-adjustment window), so even a live host/source-rate
    /// ratio move stays allocation-free. No-op in passthrough - there is no
    /// resampler and the scratch is unused. Pure capacity reservation: output
    /// is bit-exact.
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
        let chunk = self.options.chunk_size;
        // `input_buffer` carries a sub-block residual plus one freshly
        // appended decoder chunk before the next drain.
        let in_buf_cap = in_max + chunk;
        // `temp_output_all` gathers every block emitted from one `process`
        let blocks = chunk.div_ceil(in_next).saturating_add(1);
        let out_all_cap = out_max.saturating_mul(blocks);

        reserve_channel_scratch(&mut self.input_buffer, channels, in_buf_cap);
        reserve_channel_scratch(&mut self.temp_deinterleave, channels, chunk.max(in_max));
        reserve_channel_scratch(&mut self.temp_input_slice, channels, in_max);
        reserve_channel_scratch(&mut self.temp_output_bufs, channels, out_max);
        reserve_channel_scratch(&mut self.temp_output_all, channels, out_all_cap);
    }

    fn process_block(
        &mut self,
        resampler: &mut dyn Resampler,
        input_frames: usize,
        output_frames: usize,
    ) -> Result<(usize, usize), ResamplerError> {
        let channels = self.channels;

        for ch in 0..channels {
            self.temp_input_slice[ch].clear();
            self.temp_input_slice[ch].extend_from_slice(&self.input_buffer[ch][..input_frames]);
        }

        for ch in 0..channels {
            self.temp_output_bufs[ch].resize(output_frames, 0.0);
        }

        let (consumed, out_len) = resampler.process_into_buffer(
            &self.temp_input_slice[..channels],
            &mut self.temp_output_bufs[..channels],
        )?;
        Ok((consumed, out_len))
    }

    fn ratio_for_target(&self, target_rate: u32) -> f64 {
        if self.source_rate > 0 {
            f64::from(target_rate) / f64::from(self.source_rate)
        } else {
            1.0
        }
    }

    fn recreate_resampler(&mut self, backend: ResamplerBackend, target_rate: u32, new_ratio: f64) {
        match create_resampler(
            backend,
            self.quality,
            self.source_rate,
            target_rate,
            self.channels,
            self.options,
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

    fn switch_to_source_passthrough(&mut self, target_rate: u32, currently_pt: bool) {
        if !currently_pt {
            self.resampler = None;
        }
        if !self.missing_backend_logged {
            debug!(
                source_rate = self.source_rate,
                target_rate, "No resampler backend compiled; leaving PCM in source-rate domain"
            );
            self.missing_backend_logged = true;
        }
        self.current_ratio = 1.0;
        self.output_spec.sample_rate =
            NonZeroU32::new(self.source_rate).unwrap_or(self.output_spec.sample_rate);
        for buf in &mut self.input_buffer {
            buf.clear();
        }
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn resample(&mut self, chunk: &PcmChunk) -> Option<PcmChunk> {
        self.resampler.as_ref()?;

        self.last_input_meta = Some(chunk.meta);
        self.append_to_buffer(&chunk.samples);

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

    fn update_resampler_if_needed(&mut self) {
        let target_rate = self.target_rate();
        // target_rate() returns source_rate when host==0 (non-zero) or host_sr (non-zero by contract).
        self.output_spec.sample_rate =
            NonZeroU32::new(target_rate).unwrap_or(self.output_spec.sample_rate);
        let should_pt = self.source_rate == target_rate || target_rate == 0;
        let currently_pt = self.is_passthrough();
        let new_ratio = self.ratio_for_target(target_rate);
        let ratio_changed =
            (new_ratio - self.current_ratio).abs() > self.options.passthrough_tolerance;

        if should_pt {
            self.switch_to_passthrough(target_rate, currently_pt);
            return;
        }

        let Some(backend) = ResamplerBackend::preferred() else {
            self.switch_to_source_passthrough(target_rate, currently_pt);
            return;
        };

        if currently_pt || self.resampler.is_none() || ratio_changed {
            self.recreate_resampler(backend, target_rate, new_ratio);
        }
    }
}

/// Create a `SmallVec` of empty Vecs for each channel.
#[cfg_attr(feature = "perf", hotpath::measure)]
fn smallvec_new_vecs(channels: usize) -> SmallVec<[Vec<f32>; 8]> {
    (0..channels).map(|_| Vec::new()).collect()
}

/// Reserve `cap` floats of capacity on each per-channel scratch Vec,
/// growing the outer `SmallVec` to `channels` first. Pure capacity - the
/// lengths and contents are untouched, so resampler output stays bit-exact.
fn reserve_channel_scratch(bufs: &mut SmallVec<[Vec<f32>; 8]>, channels: usize, cap: usize) {
    if bufs.len() < channels {
        bufs.resize_with(channels, Vec::new);
    }
    bufs.iter_mut()
        .take(channels)
        .filter(|buf| buf.capacity() < cap)
        .for_each(|buf| buf.reserve(cap.saturating_sub(buf.len())));
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
        let chunk_rate = chunk.spec().sample_rate.get();
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

    fn pcm_spec(channels: u16, hz: u32) -> PcmSpec {
        PcmSpec::new(channels, NonZeroU32::new(hz).expect("test rate"))
    }

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

        let chunk = test_chunk(pcm_spec(2, 44100), vec![0.1, 0.2, 0.3, 0.4]);

        let result = processor.process(chunk.clone());
        assert!(result.is_some());
        let out = result.unwrap();
        assert_eq!(out.samples, chunk.samples);
        assert_eq!(out.spec().sample_rate.get(), 44100);
    }

    #[kithara::test]
    fn test_output_spec() {
        let processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));
        assert_eq!(processor.output_spec.sample_rate.get(), 44100);
        assert_eq!(processor.output_spec.channels, 2);
    }

    #[kithara::test]
    fn test_dynamic_host_rate_change() {
        let host_sr = make_host_rate(44100);
        let mut processor = ResamplerProcessor::new(params(host_sr.clone(), 44100, 2));
        assert!(processor.is_passthrough());

        host_sr.store(48000, Ordering::Relaxed);

        let chunk = test_chunk(pcm_spec(2, 44100), vec![0.1; 2048]);
        let _ = processor.process(chunk);

        assert!(!processor.is_passthrough());
    }

    #[kithara::test]
    #[case::small(100, false)]
    #[case::large(16384, true)]
    fn test_chunk_size_threshold(#[case] interleaved_len: usize, #[case] produces_output: bool) {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let chunk = test_chunk(pcm_spec(2, 48000), vec![0.1; interleaved_len]);

        let result = processor.process(chunk);
        if produces_output {
            assert!(result.is_some());
            assert!(!result.unwrap().samples.is_empty());
        } else {
            assert!(result.is_none());
            assert_eq!(processor.input_buffer[0].len(), interleaved_len / 2);
        }
    }

    #[kithara::test]
    fn test_source_rate_change_updates_dynamically() {
        let mut processor = ResamplerProcessor::new(params(make_host_rate(44100), 48000, 2));

        let chunk1 = test_chunk(pcm_spec(2, 48000), vec![0.1; 4096]);
        let _ = processor.process(chunk1);
        assert_eq!(processor.source_rate, 48000);

        let chunk2 = test_chunk(pcm_spec(2, 44100), vec![0.2; 4096]);
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
            let chunk = test_chunk(pcm_spec(2, 48000), vec![0.1; 2048]);
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

        let chunk = test_chunk(pcm_spec(2, 44100), vec![0.1, 0.2, 0.3, 0.4]);

        let result = AudioEffect::process(&mut processor, chunk);
        assert!(result.is_some());

        AudioEffect::reset(&mut processor);
        assert!(processor.input_buffer[0].is_empty());
    }

    #[kithara::test]
    #[case::fast(ResamplerQuality::Fast)]
    #[case::normal(ResamplerQuality::Normal)]
    #[case::good(ResamplerQuality::Good)]
    #[case::high(ResamplerQuality::High)]
    fn test_quality_resamples(#[case] quality: ResamplerQuality) {
        let mut processor = ResamplerProcessor::new(params_with_quality(
            make_host_rate(44100),
            48000,
            2,
            quality,
        ));
        assert!(!processor.is_passthrough());

        let chunk = test_chunk(pcm_spec(2, 48000), vec![0.1; 16384]);
        let result = processor.process(chunk);
        assert!(result.is_some());
    }
}
