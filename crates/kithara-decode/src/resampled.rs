use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerConfig, ResamplerMode, ResamplerProcess,
    ResamplerSettings, create_resampler,
};
use kithara_stream::AudioCodec;
use smallvec::SmallVec;

use crate::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderResamplerConfig,
    DecoderSeekOutcome, DecoderTrackInfo, GaplessInfo, GaplessTailCompensation, PcmChunk, PcmMeta,
    PcmSpec, TrackMetadata, duration_for_frames, frames_for_duration,
};

#[cfg(test)]
mod tests;

pub(crate) fn wrap<B>(
    decoder: Box<dyn Decoder>,
    config: Option<DecoderResamplerConfig<B>>,
    pool: &PcmPool,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
{
    let Some(config) = config else {
        return Ok(decoder);
    };
    if decoder.spec().sample_rate == config.target_sample_rate {
        return Ok(decoder);
    }
    Ok(Box::new(ResampledDecoder::new(decoder, config, pool)?))
}

struct ResampledDecoder<B>
where
    B: ResamplerBackend,
{
    backend: B,
    decoder: Box<dyn Decoder>,
    emitted_frames: u64,
    eof_flushed: bool,
    input: SmallVec<[PcmBuf; 8]>,
    last_input_meta: Option<PcmMeta>,
    options: kithara_resampler::ResamplerOptions,
    output: SmallVec<[PcmBuf; 8]>,
    output_frame_offset: u64,
    output_skip_frames: usize,
    pending_meta: Option<PcmMeta>,
    pool: PcmPool,
    quality: kithara_resampler::ResamplerQuality,
    resampler: B::Resampler,
    scratch: SmallVec<[PcmBuf; 8]>,
    source_frames_seen: u64,
    source_spec: PcmSpec,
    target_sample_rate: NonZeroU32,
    target_spec: PcmSpec,
}

impl<B> ResampledDecoder<B>
where
    B: ResamplerBackend,
{
    fn new(
        decoder: Box<dyn Decoder>,
        config: DecoderResamplerConfig<B>,
        pool: &PcmPool,
    ) -> DecodeResult<Self> {
        let backend = config.backend;
        let source_spec = decoder.spec();
        let target_spec = PcmSpec::new(source_spec.channels, config.target_sample_rate);
        let resampler = build_resampler(
            backend.clone(),
            source_spec,
            config.target_sample_rate,
            config.quality,
            config.options,
            pool.clone(),
        )?;
        Ok(Self {
            backend,
            decoder,
            emitted_frames: 0,
            eof_flushed: false,
            input: channel_buffers(pool, source_spec.channels),
            last_input_meta: None,
            options: config.options,
            output: channel_buffers(pool, source_spec.channels),
            output_frame_offset: 0,
            output_skip_frames: resampler.output_delay(),
            pending_meta: None,
            pool: pool.clone(),
            quality: config.quality,
            resampler,
            scratch: channel_buffers(pool, source_spec.channels),
            source_frames_seen: 0,
            source_spec,
            target_sample_rate: config.target_sample_rate,
            target_spec,
        })
    }

    fn append_chunk(&mut self, chunk: &PcmChunk) -> DecodeResult<()> {
        let spec = chunk.spec();
        if spec != self.source_spec {
            self.rebuild_for_source_spec(spec)?;
            self.output_frame_offset = u64::try_from(frames_for_duration(
                self.target_sample_rate.get(),
                chunk.meta.timestamp,
            ))
            .unwrap_or(u64::MAX);
        }
        if self.pending_meta.is_none() {
            self.pending_meta = Some(chunk.meta);
        }
        self.last_input_meta = Some(chunk.meta);
        let channels = self.channels();
        let frames = chunk.frames();
        self.source_frames_seen = self
            .source_frames_seen
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        let base_len = self.input[0].len();
        for channel in self.input.iter_mut().take(channels) {
            let old_len = channel.len();
            channel.ensure_len(old_len.saturating_add(frames))?;
        }
        for frame in 0..frames {
            let base = frame.saturating_mul(channels);
            for channel in 0..channels {
                let dst = base_len + frame;
                self.input[channel][dst] = chunk.samples[base + channel];
            }
        }
        Ok(())
    }

    fn channels(&self) -> usize {
        usize::from(self.source_spec.channels)
    }

    fn clear_planar(buffers: &mut SmallVec<[PcmBuf; 8]>, channels: usize) {
        for buffer in buffers.iter_mut().take(channels) {
            buffer.clear();
        }
    }

    fn drain_ready(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            let input_frames = self.resampler.input_frames_next();
            if self.input[0].len() < input_frames {
                break;
            }
            let process = self.process_block(input_frames)?;
            if process.input_frames > input_frames {
                return Err(DecodeError::InvalidData {
                    detail: "decoder resampler consumed more frames than supplied",
                });
            }
            if process.input_frames == 0 {
                break;
            }
            self.drop_consumed(process.input_frames);
        }
        self.finish_output()
    }

    fn drop_consumed(&mut self, frames: usize) {
        let channels = self.channels();
        for buffer in self.input.iter_mut().take(channels) {
            buffer.drain(..frames);
        }
    }

    fn finish_output(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.output[0].is_empty() {
            return Ok(None);
        }
        let frames = self.output[0].len();
        let samples = self.interleave(frames)?;
        let mut meta = self.pending_meta.take().unwrap_or_default();
        meta.spec = self.target_spec;
        meta.frame_offset = self.output_frame_offset;
        meta.frames = u32::try_from(frames).unwrap_or(u32::MAX);
        meta.timestamp = duration_for_frames(self.target_sample_rate.get(), meta.frame_offset);
        self.output_frame_offset = self
            .output_frame_offset
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        self.emitted_frames = self
            .emitted_frames
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        meta.end_timestamp =
            duration_for_frames(self.target_sample_rate.get(), self.output_frame_offset);
        let channels = self.channels();
        Self::clear_planar(&mut self.output, channels);
        Ok(Some(PcmChunk::new(meta, samples)))
    }

    fn flush_residual(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.input[0].is_empty() && self.ready_output_frames() >= self.expected_output_frames() {
            return Ok(None);
        }
        if self.pending_meta.is_none() {
            self.pending_meta = self.last_input_meta;
        }
        let channels = self.channels();
        while self.ready_output_frames() < self.expected_output_frames() {
            let input_frames = self.resampler.input_frames_next();
            let buffered = self.input[0].len();
            for buffer in self.input.iter_mut().take(channels) {
                buffer.ensure_len(input_frames)?;
                buffer[buffered..input_frames].fill(0.0);
            }
            let ready_before = self.ready_output_frames();
            let process = self.process_block(input_frames)?;
            if process.input_frames > input_frames {
                return Err(DecodeError::InvalidData {
                    detail: "decoder resampler consumed more frames than supplied",
                });
            }
            if process.input_frames == 0 && self.ready_output_frames() == ready_before {
                break;
            }
            self.drop_consumed(process.input_frames);
        }
        self.finish_output()
    }

    fn interleave(&self, frames: usize) -> DecodeResult<PcmBuf> {
        let channels = self.channels();
        let mut samples = self.pool.get();
        samples.ensure_len(frames.saturating_mul(channels))?;
        for frame in 0..frames {
            let base = frame.saturating_mul(channels);
            for channel in 0..channels {
                samples[base + channel] = self.output[channel][frame];
            }
        }
        Ok(samples)
    }

    fn expected_output_frames(&self) -> u64 {
        let source_rate = self.source_spec.sample_rate.get();
        let expected = u128::from(self.source_frames_seen)
            .saturating_mul(u128::from(self.target_sample_rate.get()))
            .saturating_add(u128::from(source_rate / 2))
            / u128::from(source_rate);
        u64::try_from(expected).unwrap_or(u64::MAX)
    }

    fn ready_output_frames(&self) -> u64 {
        self.emitted_frames
            .saturating_add(u64::try_from(self.output[0].len()).unwrap_or(u64::MAX))
    }

    fn process_block(&mut self, input_frames: usize) -> DecodeResult<ResamplerProcess> {
        let channels = self.channels();
        let output_frames = self.resampler.output_frames_next();
        for buffer in self.scratch.iter_mut().take(channels) {
            buffer.ensure_len(output_frames)?;
        }
        let input = self.input[..channels]
            .iter()
            .map(|buffer| &buffer[..input_frames])
            .collect::<SmallVec<[&[f32]; 8]>>();
        let process = {
            let mut output = self.scratch[..channels]
                .iter_mut()
                .map(|buffer| &mut buffer[..output_frames])
                .collect::<SmallVec<[&mut [f32]; 8]>>();
            self.resampler
                .process_into_buffer(&input, &mut output)
                .map_err(DecodeError::backend)?
        };
        if process.output_frames > output_frames {
            return Err(DecodeError::InvalidData {
                detail: "decoder resampler produced more frames than requested",
            });
        }
        let skip = self.output_skip_frames.min(process.output_frames);
        self.output_skip_frames -= skip;
        let available = process.output_frames.saturating_sub(skip);
        let remaining = self
            .expected_output_frames()
            .saturating_sub(self.ready_output_frames());
        let usable = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(available);
        for channel in 0..channels {
            let old_len = self.output[channel].len();
            self.output[channel].ensure_len(old_len.saturating_add(usable))?;
            let dst = &mut self.output[channel][old_len..old_len + usable];
            let src = &self.scratch[channel][skip..skip + usable];
            dst.copy_from_slice(src);
        }
        Ok(process)
    }

    fn rebuild_for_source_spec(&mut self, source_spec: PcmSpec) -> DecodeResult<()> {
        let target_spec = PcmSpec::new(source_spec.channels, self.target_sample_rate);
        let resampler = build_resampler(
            self.backend.clone(),
            source_spec,
            self.target_sample_rate,
            self.quality,
            self.options,
            self.pool.clone(),
        )?;
        self.source_spec = source_spec;
        self.target_spec = target_spec;
        self.input = channel_buffers(&self.pool, source_spec.channels);
        self.output = channel_buffers(&self.pool, source_spec.channels);
        self.output_skip_frames = resampler.output_delay();
        self.scratch = channel_buffers(&self.pool, source_spec.channels);
        self.resampler = resampler;
        self.emitted_frames = 0;
        self.source_frames_seen = 0;
        Ok(())
    }

    fn reset_resampler_state(&mut self) {
        let channels = self.channels();
        Self::clear_planar(&mut self.input, channels);
        Self::clear_planar(&mut self.output, channels);
        self.pending_meta = None;
        self.last_input_meta = None;
        self.emitted_frames = 0;
        self.eof_flushed = false;
        self.output_skip_frames = self.resampler.output_delay();
        self.source_frames_seen = 0;
        self.resampler.reset();
    }

    fn scaled_gapless(&self, info: GaplessInfo) -> DecodeResult<GaplessInfo> {
        let source_rate = self.source_spec.sample_rate.get();
        let target_rate = self.target_sample_rate.get();
        Ok(GaplessInfo {
            leading_frames: round_scaled_frames(info.leading_frames, source_rate, target_rate)?,
            trailing_frames: round_scaled_frames(info.trailing_frames, source_rate, target_rate)?,
        })
    }
}

impl<B> Decoder for ResampledDecoder<B>
where
    B: ResamplerBackend,
{
    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        let source = self.decoder.default_priming_frames(codec);
        round_scaled_frames_lossy(
            source,
            self.source_spec.sample_rate.get(),
            self.target_sample_rate.get(),
        )
    }

    fn duration(&self) -> Option<kithara_platform::time::Duration> {
        self.decoder.duration()
    }

    fn flush_reader_signals(&mut self) {
        self.decoder.flush_reader_signals();
    }

    fn metadata(&self) -> TrackMetadata {
        self.decoder.metadata()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            match self.decoder.next_chunk()? {
                DecoderChunkOutcome::Chunk(chunk) => {
                    self.append_chunk(&chunk)?;
                    if let Some(output) = self.drain_ready()? {
                        return Ok(DecoderChunkOutcome::Chunk(output));
                    }
                }
                DecoderChunkOutcome::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                DecoderChunkOutcome::Eof => {
                    if !self.eof_flushed {
                        self.eof_flushed = true;
                        if let Some(output) = self.flush_residual()? {
                            return Ok(DecoderChunkOutcome::Chunk(output));
                        }
                    }
                    return Ok(DecoderChunkOutcome::Eof);
                }
            }
        }
    }

    fn seek(&mut self, pos: kithara_platform::time::Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.decoder.seek(pos)?;
        self.reset_resampler_state();
        match outcome {
            DecoderSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
                ..
            } => {
                self.output_frame_offset = u64::try_from(frames_for_duration(
                    self.target_sample_rate.get(),
                    landed_at,
                ))
                .unwrap_or(u64::MAX);
                Ok(DecoderSeekOutcome::Landed {
                    landed_at,
                    landed_byte,
                    preroll,
                    landed_frame: self.output_frame_offset,
                })
            }
            DecoderSeekOutcome::PastEof { duration } => {
                Ok(DecoderSeekOutcome::PastEof { duration })
            }
        }
    }

    fn spec(&self) -> PcmSpec {
        self.target_spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        let info = self.decoder.track_info();
        DecoderTrackInfo {
            gapless: info
                .gapless
                .map(|gapless| self.scaled_gapless(gapless))
                .transpose()
                .unwrap_or(None),
            gapless_tail: info.gapless_tail.and_then(|tail| {
                GaplessTailCompensation::for_source_frames(
                    tail.ideal_pre_trim_frames(),
                    self.source_spec.sample_rate.get(),
                    self.target_sample_rate.get(),
                )
            }),
        }
    }

    fn update_byte_len(&self, len: u64) {
        self.decoder.update_byte_len(len);
    }
}

fn build_resampler<B>(
    backend: B,
    source_spec: PcmSpec,
    target_sample_rate: NonZeroU32,
    quality: kithara_resampler::ResamplerQuality,
    options: kithara_resampler::ResamplerOptions,
    pool: PcmPool,
) -> DecodeResult<B::Resampler>
where
    B: ResamplerBackend,
{
    let channels =
        NonZeroUsize::new(usize::from(source_spec.channels)).ok_or(DecodeError::InvalidData {
            detail: "decoder resampler requires at least one channel",
        })?;
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            target_sample_rate,
            source_sample_rate: source_spec.sample_rate,
        })
        .options(options)
        .quality(quality)
        .pcm_pool(pool)
        .build();
    let config = ResamplerConfig::builder()
        .backend(backend)
        .settings(settings)
        .build();
    create_resampler(&config).map_err(DecodeError::backend)
}

fn channel_buffers(pool: &PcmPool, channels: u16) -> SmallVec<[PcmBuf; 8]> {
    (0..usize::from(channels)).map(|_| pool.get()).collect()
}

fn round_scaled_frames(count: u64, source_rate: u32, target_rate: u32) -> DecodeResult<u64> {
    if source_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "decoder.resampler.source",
        });
    }
    if target_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "decoder.resampler.target",
        });
    }
    let numerator = u128::from(count)
        .saturating_mul(u128::from(target_rate))
        .saturating_add(u128::from(source_rate / 2));
    let scaled = numerator / u128::from(source_rate);
    u64::try_from(scaled).map_err(|_| DecodeError::InvalidData {
        detail: "decoder resampler frame count overflow",
    })
}

fn round_scaled_frames_lossy(count: u64, source_rate: u32, target_rate: u32) -> u64 {
    round_scaled_frames(count, source_rate, target_rate).unwrap_or(u64::MAX)
}
