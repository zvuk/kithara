use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerConfig, ResamplerMode, ResamplerProcess,
    ResamplerSettings, create_resampler,
};
use kithara_signal::{
    AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, PlanarBuffer, sanitize_sample,
};
use kithara_stream::AudioCodec;
use kithara_test_utils::kithara;
use smallvec::SmallVec;

use crate::{
    BlenderProfile, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    DecoderResamplerConfig, DecoderSeekOutcome, DecoderTrackInfo, GaplessInfo,
    GaplessTailCompensation, TrackMetadata,
};

pub(crate) fn wrap<B, S>(
    decoder: Box<dyn Decoder>,
    config: Option<DecoderResamplerConfig<B>>,
    pools: &PoolRegion<S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    let Some(config) = config else {
        return Ok(decoder);
    };
    if decoder.spec().sample_rate == config.target_sample_rate {
        return Ok(decoder);
    }
    Ok(Box::new(ResampledDecoder::new(decoder, config, pools)?))
}

struct ResampledDecoder<B, S>
where
    B: ResamplerBackend,
{
    backend: B,
    decoder: Box<dyn Decoder>,
    target_sample_rate: NonZeroU32,
    last_input_meta: Option<AudioChunkInfo>,
    pending_meta: Option<AudioChunkInfo>,
    pools: PoolRegion<S>,
    source_spec: AudioSpec,
    target_spec: AudioSpec,
    resampler: B::Resampler,
    options: kithara_resampler::ResamplerOptions,
    quality: kithara_resampler::ResamplerQuality,
    input: PlanarBuffer,
    output: PlanarBuffer,
    scratch: PlanarBuffer,
    eof_flushed: bool,
    emitted_frames: u64,
    output_frame_offset: u64,
    reanchor_output_on_next_chunk: bool,
    source_frames_seen: u64,
    output_skip_frames: usize,
}

impl<B, S> ResampledDecoder<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
{
    fn new(
        decoder: Box<dyn Decoder>,
        config: DecoderResamplerConfig<B>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self> {
        let backend = config.backend;
        let source_spec = decoder.spec();
        let target_spec = AudioSpec::new(source_spec.channels, config.target_sample_rate);
        let resampler = build_resampler(
            backend.clone(),
            source_spec,
            config.target_sample_rate,
            config.quality,
            config.options,
            pools,
        )?;
        let empty = FrameCount::new(0);
        Ok(Self {
            backend,
            decoder,
            emitted_frames: 0,
            eof_flushed: false,
            input: PlanarBuffer::new(pools, source_spec, empty)?,
            last_input_meta: None,
            options: config.options,
            output: PlanarBuffer::new(pools, target_spec, empty)?,
            output_frame_offset: 0,
            output_skip_frames: resampler.output_delay(),
            pending_meta: None,
            pools: pools.clone(),
            quality: config.quality,
            reanchor_output_on_next_chunk: false,
            resampler,
            scratch: PlanarBuffer::new(pools, target_spec, empty)?,
            source_frames_seen: 0,
            source_spec,
            target_sample_rate: config.target_sample_rate,
            target_spec,
        })
    }

    #[kithara::measure]
    fn append_chunk(&mut self, chunk: &AudioChunk) -> DecodeResult<()> {
        let spec = chunk.spec();
        let source_spec_changed = spec != self.source_spec;
        if source_spec_changed {
            self.rebuild_for_source_spec(spec)?;
        }
        if source_spec_changed || self.reanchor_output_on_next_chunk {
            self.output_frame_offset = self
                .target_spec
                .frame_at(chunk.meta.timestamp)
                .unwrap_or(u64::MAX);
            self.reanchor_output_on_next_chunk = false;
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
        let base_len = self.input.frames().get();
        self.input
            .resize_frames(FrameCount::new(base_len.saturating_add(frames)))?;
        for channel in 0..channels {
            let destination = self.input.channel_mut(channel)?;
            for frame in 0..frames {
                let base = frame.saturating_mul(channels);
                destination[base_len + frame] = sanitize_sample(chunk.samples[base + channel]);
            }
        }
        Ok(())
    }

    fn channels(&self) -> usize {
        usize::from(self.source_spec.channels)
    }

    #[kithara::measure]
    fn drain_ready(&mut self) -> DecodeResult<Option<AudioChunk>> {
        let input_frames = self.resampler.input_frames_next();
        if self.input.frames().get() < input_frames {
            return self.finish_output();
        }
        let process = self.process_block(input_frames)?;
        if process.input_frames > input_frames {
            return Err(DecodeError::InvalidData {
                detail: "decoder resampler consumed more frames than supplied",
            });
        }
        self.drop_consumed(process.input_frames)?;
        self.finish_output()
    }

    fn drop_consumed(&mut self, frames: usize) -> DecodeResult<()> {
        self.input.truncate_front(FrameCount::new(frames))?;
        Ok(())
    }

    fn expected_output_frames(&self) -> u64 {
        let source_rate = self.source_spec.sample_rate.get();
        let expected = u128::from(self.source_frames_seen)
            .saturating_mul(u128::from(self.target_sample_rate.get()))
            .saturating_add(u128::from(source_rate / 2))
            / u128::from(source_rate);
        u64::try_from(expected).unwrap_or(u64::MAX)
    }

    fn finish_output(&mut self) -> DecodeResult<Option<AudioChunk>> {
        if self.output.frames().get() == 0 {
            return Ok(None);
        }
        let frames = self.output.frames();
        let samples = self.interleave(frames)?;
        let mut meta = self.pending_meta.take().unwrap_or_default();
        meta.spec = self.target_spec;
        meta.frame_offset = self.output_frame_offset;
        meta.frames = u32::try_from(frames.get()).unwrap_or(u32::MAX);
        meta.timestamp = self
            .target_spec
            .duration_for(meta.frame_offset)
            .unwrap_or(Duration::from_nanos(u64::MAX));
        self.output_frame_offset = self
            .output_frame_offset
            .saturating_add(u64::try_from(frames.get()).unwrap_or(u64::MAX));
        self.emitted_frames = self
            .emitted_frames
            .saturating_add(u64::try_from(frames.get()).unwrap_or(u64::MAX));
        meta.end_timestamp = self
            .target_spec
            .duration_for(self.output_frame_offset)
            .unwrap_or(Duration::from_nanos(u64::MAX));
        self.output.clear();
        Ok(Some(AudioChunk::new(meta, samples)))
    }

    fn flush_residual(&mut self) -> DecodeResult<Option<AudioChunk>> {
        if self.input.frames().get() == 0
            && self.ready_output_frames() >= self.expected_output_frames()
        {
            return Ok(None);
        }
        if self.pending_meta.is_none() {
            self.pending_meta = self.last_input_meta;
        }
        while self.ready_output_frames() < self.expected_output_frames() {
            let input_frames = self.resampler.input_frames_next();
            self.input.resize_frames(FrameCount::new(input_frames))?;
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
            self.drop_consumed(process.input_frames)?;
        }
        self.finish_output()
    }

    #[kithara::measure]
    fn interleave(&self, frames: FrameCount) -> DecodeResult<SampleBuffer> {
        let mut samples = self.pools.get::<f32>();
        let sample_count = self.target_spec.sample_count(frames)?.get();
        samples.ensure_len(sample_count)?;
        self.output.view().interleave_into(&mut samples)?;
        Ok(samples)
    }

    #[kithara::measure]
    fn process_block(&mut self, input_frames: usize) -> DecodeResult<ResamplerProcess> {
        let channels = self.channels();
        let output_frames = self.resampler.output_frames_next();
        self.scratch.resize_frames(FrameCount::new(output_frames))?;
        let input_view = self.input.view().range(0..input_frames)?;
        let input = (0..channels)
            .map(|channel| input_view.channel(channel))
            .collect::<Result<SmallVec<[&[f32]; 8]>, _>>()?;
        let process = {
            let stride = self.scratch.stride().get();
            let mut remaining = self.scratch.as_samples_mut();
            let mut output = SmallVec::<[&mut [f32]; 8]>::with_capacity(channels);
            for _ in 0..channels {
                let (channel, rest) = remaining.split_at_mut(stride);
                output.push(&mut channel[..output_frames]);
                remaining = rest;
            }
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
        let old_len = self.output.frames().get();
        self.output
            .resize_frames(FrameCount::new(old_len.saturating_add(usable)))?;
        for channel in 0..channels {
            let src = &self.scratch.channel(channel)?[skip..skip + usable];
            let dst = &mut self.output.channel_mut(channel)?[old_len..old_len + usable];
            dst.copy_from_slice(src);
        }
        Ok(process)
    }

    fn ready_output_frames(&self) -> u64 {
        self.emitted_frames
            .saturating_add(u64::try_from(self.output.frames().get()).unwrap_or(u64::MAX))
    }

    fn rebuild_for_source_spec(&mut self, source_spec: AudioSpec) -> DecodeResult<()> {
        let target_spec = AudioSpec::new(source_spec.channels, self.target_sample_rate);
        let resampler = build_resampler(
            self.backend.clone(),
            source_spec,
            self.target_sample_rate,
            self.quality,
            self.options,
            &self.pools,
        )?;
        let empty = FrameCount::new(0);
        let input = PlanarBuffer::new(&self.pools, source_spec, empty)?;
        let output = PlanarBuffer::new(&self.pools, target_spec, empty)?;
        let scratch = PlanarBuffer::new(&self.pools, target_spec, empty)?;
        self.source_spec = source_spec;
        self.target_spec = target_spec;
        self.input = input;
        self.output = output;
        self.output_skip_frames = resampler.output_delay();
        self.scratch = scratch;
        self.resampler = resampler;
        self.emitted_frames = 0;
        self.source_frames_seen = 0;
        Ok(())
    }

    fn reset_resampler_state(&mut self) {
        self.input.clear();
        self.output.clear();
        self.pending_meta = None;
        self.reanchor_output_on_next_chunk = false;
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

impl<B, S> Decoder for ResampledDecoder<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn blender_profile(&self) -> BlenderProfile {
        BlenderProfile::new(self.target_spec)
    }

    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        let source = self.decoder.default_priming_frames(codec);
        round_scaled_frames_lossy(
            source,
            self.source_spec.sample_rate.get(),
            self.target_sample_rate.get(),
        )
    }

    #[kithara::measure(label = "decode.resampled.next")]
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            if let Some(output) = self.drain_ready()? {
                return Ok(DecoderChunkOutcome::Chunk(output));
            }
            match self.decoder.next_chunk()? {
                DecoderChunkOutcome::Chunk(chunk) => {
                    self.append_chunk(&chunk)?;
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

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.decoder.seek(pos)?;
        self.reset_resampler_state();
        match outcome {
            DecoderSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
                ..
            } => {
                self.output_frame_offset = self.target_spec.frame_at(landed_at).unwrap_or(u64::MAX);
                self.reanchor_output_on_next_chunk = true;
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

    fn spec(&self) -> AudioSpec {
        self.target_spec
    }

    fn timeline_gap_frames(&self) -> u64 {
        round_scaled_frames_lossy(
            self.decoder.timeline_gap_frames(),
            self.source_spec.sample_rate.get(),
            self.target_sample_rate.get(),
        )
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

    delegate::delegate! {
        to self.decoder {
            fn duration(&self) -> Option<Duration>;
            fn flush_reader_signals(&mut self);
            fn metadata(&self) -> TrackMetadata;
            fn update_byte_len(&self, len: u64);
        }
    }
}

fn build_resampler<B, S>(
    backend: B,
    source_spec: AudioSpec,
    target_sample_rate: NonZeroU32,
    quality: kithara_resampler::ResamplerQuality,
    options: kithara_resampler::ResamplerOptions,
    pools: &PoolRegion<S>,
) -> DecodeResult<B::Resampler>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
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
        .pools(pools.clone())
        .build();
    let config = ResamplerConfig::builder()
        .backend(backend)
        .settings(settings)
        .build();
    create_resampler(&config).map_err(DecodeError::backend)
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
