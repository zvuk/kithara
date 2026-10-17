use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerConfig, ResamplerMode, ResamplerProcess,
    ResamplerSettings, create_resampler,
};
use kithara_signal::{
    AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, PlanarBuffer,
};
use kithara_stream::{AudioCodec, PendingReason};
use kithara_test_utils::kithara;
use smallvec::SmallVec;

use crate::{
    BlenderProfile, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    DecoderResamplerConfig, DecoderSeekOutcome, DecoderTrackInfo, GaplessTailCompensation,
    InputState, PcmOrigin, PrepareWait, TrackMetadata,
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

pub(super) struct ResampledDecoder<B, S>
where
    B: ResamplerBackend,
{
    pub(super) source_spec: AudioSpec,
    target_spec: AudioSpec,
    pub(super) config: DecoderResamplerConfig<B>,
    pub(super) decoder: Box<dyn Decoder>,
    last_input_meta: Option<AudioChunkInfo>,
    source_basis: Option<AudioChunkInfo>,
    pending_meta: Option<AudioChunkInfo>,
    input: PlanarBuffer,
    output: PlanarBuffer,
    scratch: PlanarBuffer,
    pools: PoolRegion<S>,
    resampler: B::Resampler,
    eof_flushed: bool,
    reanchor_output_on_next_chunk: bool,
    emitted_frames: u64,
    output_frame_offset: u64,
    initial_pcm_origin: Option<PcmOrigin>,
    observing_initial_pcm: bool,
    source_frames_seen: u64,
    output_skip_frames: usize,
    prepared: Option<DecodeResult<DecoderChunkOutcome>>,
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
        let source_spec = decoder.spec();
        let target_spec = AudioSpec::new(source_spec.channels, config.target_sample_rate);
        let initial_pcm_origin = decoder.initial_pcm_origin();
        let resampler = build_resampler(
            config.backend.clone(),
            source_spec,
            config.target_sample_rate,
            config.quality,
            config.options,
            pools,
        )?;
        let empty = FrameCount::new(0);
        Ok(Self {
            config,
            decoder,
            emitted_frames: 0,
            eof_flushed: false,
            input: PlanarBuffer::new(pools, source_spec, empty)?,
            last_input_meta: None,
            source_basis: None,
            output: PlanarBuffer::new(pools, target_spec, empty)?,
            output_frame_offset: 0,
            initial_pcm_origin,
            observing_initial_pcm: true,
            output_skip_frames: resampler.output_delay(),
            pending_meta: None,
            prepared: None,
            pools: pools.clone(),
            reanchor_output_on_next_chunk: true,
            resampler,
            scratch: PlanarBuffer::new(pools, target_spec, empty)?,
            source_frames_seen: 0,
            source_spec,
            target_spec,
        })
    }

    fn seek_selected(
        &mut self,
        pos: Duration,
        prefix: bool,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        if !pos.is_zero() {
            self.observing_initial_pcm = false;
        }
        let outcome = if prefix {
            self.decoder.seek_prefix_with_wait(pos, wait)?
        } else {
            self.decoder.seek_with_wait(pos, wait)?
        };
        self.prepared = None;
        self.reset_resampler_state();
        self.reanchor_output_on_next_chunk = true;
        Ok(self.map_seek_outcome(outcome))
    }

    fn map_seek_outcome(&mut self, outcome: DecoderSeekOutcome) -> DecoderSeekOutcome {
        match outcome {
            DecoderSeekOutcome::Pending(reason) => DecoderSeekOutcome::Pending(reason),
            DecoderSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
                ..
            } => {
                self.output_frame_offset = self.target_spec.frame_at(landed_at).unwrap_or(u64::MAX);
                DecoderSeekOutcome::Landed {
                    landed_at,
                    landed_byte,
                    preroll,
                    landed_frame: self.output_frame_offset,
                }
            }
            DecoderSeekOutcome::PastEof { duration } => DecoderSeekOutcome::PastEof { duration },
        }
    }

    fn append_chunk(&mut self, chunk: &AudioChunk) -> DecodeResult<()> {
        let spec = chunk.spec();
        let source_spec_changed = spec != self.source_spec;
        if source_spec_changed {
            self.rebuild_for_source_spec(spec)?;
            self.reanchor_output_on_next_chunk = true;
        }
        if chunk.frames() == 0 {
            return Ok(());
        }
        if self.reanchor_output_on_next_chunk {
            self.output_frame_offset = self.target_spec.frame_at(chunk.meta.timestamp)?;
            self.reanchor_output_on_next_chunk = false;
            if self.observing_initial_pcm {
                let position = chunk
                    .meta
                    .source_geometry()
                    .and_then(|span| span.position_at(0))
                    .ok_or(DecodeError::InvalidData {
                        detail: "resampled PCM has invalid native geometry",
                    })?;
                self.initial_pcm_origin = self
                    .decoder
                    .initial_pcm_origin()
                    .map(|origin| PcmOrigin::new(position, origin.comparison_base()));
                self.observing_initial_pcm = false;
            }
        }
        self.source_basis.get_or_insert(chunk.meta);
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
        let source_frames = FrameCount::new(frames);
        let samples = spec.sample_count(source_frames)?.get();
        self.input.append_interleaved(InterleavedView::new(
            &chunk.samples[..samples],
            spec,
            source_frames,
        )?)?;
        for channel in 0..channels {
            kithara_dsp::sanitize(&mut self.input.channel_mut(channel)?[base_len..]);
        }
        Ok(())
    }

    fn channels(&self) -> usize {
        usize::from(self.source_spec.channels)
    }
}

impl<B, S> ResampledDecoder<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
{
    #[kithara::measure(label = "decode.resampled.next")]
    fn prepare_chunk(&mut self, wait: PrepareWait) -> DecodeResult<DecoderChunkOutcome> {
        if let Some(output) = self.drain_ready()? {
            return Ok(DecoderChunkOutcome::Chunk(Box::new(output)));
        }
        self.decoder.prepare_next_chunk_with_wait(wait);
        match self.decoder.next_chunk_prepared()? {
            DecoderChunkOutcome::Chunk(chunk) => {
                self.append_chunk(&chunk)?;
                Ok(self.drain_ready()?.map_or(
                    DecoderChunkOutcome::Pending(PendingReason::Retry),
                    |output| DecoderChunkOutcome::Chunk(Box::new(output)),
                ))
            }
            DecoderChunkOutcome::Pending(reason) => Ok(DecoderChunkOutcome::Pending(reason)),
            DecoderChunkOutcome::Eof => {
                if !self.eof_flushed {
                    self.eof_flushed = true;
                    if let Some(output) = self.flush_residual()? {
                        return Ok(DecoderChunkOutcome::Chunk(Box::new(output)));
                    }
                }
                Ok(DecoderChunkOutcome::Eof)
            }
        }
    }

    /// Resamples every whole block the buffered input affords and emits them
    /// as one chunk.
    ///
    /// A resampler consumes a fixed input block, so emitting one chunk per
    /// block would hand the consumer pieces a fraction of the size the
    /// decoder produced, while the bypassed path hands over whole decoder
    /// chunks. A consumer that plans its work per chunk then sees the stream
    /// break up for no reason other than the presence of a resampler, so the
    /// blocks of one input chunk are coalesced back into one output chunk.
    #[kithara::hang_watchdog]
    fn drain_ready(&mut self) -> DecodeResult<Option<AudioChunk>> {
        loop {
            hang_tick!();
            let input_frames = self.resampler.input_frames_next();
            if self.input.frames().get() < input_frames {
                return self.finish_output();
            }
            let process = self.process_block(input_frames)?;
            if process.first() > input_frames {
                return Err(DecodeError::InvalidData {
                    detail: "decoder resampler consumed more frames than supplied",
                });
            }
            self.drop_consumed(process.first())?;
        }
    }

    fn drop_consumed(&mut self, frames: usize) -> DecodeResult<()> {
        self.input.truncate_front(FrameCount::new(frames))?;
        Ok(())
    }

    fn expected_output_frames(&self) -> u64 {
        round_scaled_frames_lossy(
            self.source_frames_seen,
            self.source_spec.sample_rate.get(),
            self.config.target_sample_rate.get(),
        )
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
        let basis = self.source_basis.ok_or(DecodeError::InvalidData {
            detail: "resampled PCM has no observed native basis",
        })?;
        let input = basis.source_geometry().ok_or(DecodeError::InvalidData {
            detail: "resampled PCM has invalid native geometry",
        })?;
        let anchor = self.target_spec.frame_at(basis.timestamp)?;
        let elapsed = meta
            .frame_offset
            .checked_sub(anchor)
            .ok_or(DecodeError::InvalidData {
                detail: "resampled PCM precedes its observed native basis",
            })?;
        let output_rate = u128::from(self.target_spec.sample_rate.get());
        let native_rate = u128::from(basis.spec.sample_rate.get());
        let origin = u128::from(basis.frame_offset)
            .checked_mul(output_rate)
            .and_then(|origin| {
                u128::from(elapsed)
                    .checked_mul(native_rate)
                    .and_then(|advance| origin.checked_add(advance))
            })
            .ok_or(DecodeError::InvalidData {
                detail: "resampled native phase overflow",
            })?;
        let physical = kithara_signal::SourceSpan::try_from((
            origin,
            native_rate,
            std::num::NonZeroU128::from(self.target_spec.sample_rate),
            self.target_spec.sample_rate,
            u64::try_from(frames.get()).map_err(|_| DecodeError::InvalidData {
                detail: "resampled frame count overflow",
            })?,
        ))
        .map_err(|()| DecodeError::InvalidData {
            detail: "resampled native phase is invalid",
        })?;
        let span = input.map_output_span(basis.frame_offset, physical).ok_or(
            DecodeError::InvalidData {
                detail: "resampled input geometry is not affine",
            },
        )?;
        meta.timestamp = span.position_at(0).ok_or(DecodeError::InvalidData {
            detail: "resampled timestamp overflow",
        })?;
        meta.source_span = Some(span);
        self.output_frame_offset = self
            .output_frame_offset
            .saturating_add(u64::try_from(frames.get()).unwrap_or(u64::MAX));
        self.emitted_frames = self
            .emitted_frames
            .saturating_add(u64::try_from(frames.get()).unwrap_or(u64::MAX));
        meta.end_timestamp =
            span.position_at(span.output_frames())
                .ok_or(DecodeError::InvalidData {
                    detail: "resampled endpoint overflow",
                })?;
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
            if process.first() > input_frames {
                return Err(DecodeError::InvalidData {
                    detail: "decoder resampler consumed more frames than supplied",
                });
            }
            if process.first() == 0 && self.ready_output_frames() == ready_before {
                break;
            }
            self.drop_consumed(process.first())?;
        }
        self.finish_output()
    }

    fn interleave(&self, frames: FrameCount) -> DecodeResult<SampleBuffer> {
        let mut samples = self.pools.get::<f32>();
        let sample_count = self.target_spec.sample_count(frames)?.get();
        samples.ensure_len(sample_count)?;
        self.output.view().interleave_into(&mut samples)?;
        Ok(samples)
    }

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
        if process.second() > output_frames {
            return Err(DecodeError::InvalidData {
                detail: "decoder resampler produced more frames than requested",
            });
        }
        let skip = self.output_skip_frames.min(process.second());
        self.output_skip_frames -= skip;
        let available = process.second().saturating_sub(skip);
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
        let target_spec = AudioSpec::new(source_spec.channels, self.config.target_sample_rate);
        let resampler = build_resampler(
            self.config.backend.clone(),
            source_spec,
            self.config.target_sample_rate,
            self.config.quality,
            self.config.options,
            &self.pools,
        )?;
        let empty = FrameCount::new(0);
        let input = PlanarBuffer::new(&self.pools, source_spec, empty)?;
        let output = PlanarBuffer::new(&self.pools, target_spec, empty)?;
        let scratch = PlanarBuffer::new(&self.pools, target_spec, empty)?;
        self.source_spec = source_spec;
        self.target_spec = target_spec;
        self.input = input;
        self.source_basis = None;
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
        self.source_basis = None;
        self.emitted_frames = 0;
        self.eof_flushed = false;
        self.output_skip_frames = self.resampler.output_delay();
        self.source_frames_seen = 0;
        self.resampler.reset();
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

    fn duration(&self) -> Option<crate::DecoderDuration> {
        let declared = self.decoder.duration()?;
        let native_frames = self.native_duration_frames(declared)?;
        let output_frames = round_scaled_frames(
            native_frames,
            self.source_spec.sample_rate.get(),
            self.target_spec.sample_rate.get(),
        )
        .ok()?;
        Some(match declared {
            crate::DecoderDuration::RawLength(_) => crate::DecoderDuration::RawLength(
                self.target_spec.duration_for(output_frames).ok()?,
            ),
            crate::DecoderDuration::MediaEnd(_) => {
                let origin = self.initial_pcm_origin?.position();
                let end = origin.checked_add(self.target_spec.duration_for(output_frames).ok()?)?;
                crate::DecoderDuration::MediaEnd(end.checked_sub(self.decoder.media_origin()?)?)
            }
        })
    }

    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        let source = self.decoder.default_priming_frames(codec);
        round_scaled_frames_lossy(
            source,
            self.source_spec.sample_rate.get(),
            self.config.target_sample_rate.get(),
        )
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.prepare_next_chunk();
        self.next_chunk_prepared()
    }

    fn next_chunk_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.prepared.take().ok_or(DecodeError::InvalidData {
            detail: "resampled decoder output was not prepared",
        })?
    }

    fn has_prepared_result(&self) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|result| !matches!(result, Ok(DecoderChunkOutcome::Pending(_))))
    }

    fn prepare_next_chunk(&mut self) {
        self.prepare_next_chunk_with_wait(PrepareWait::Wait);
    }

    fn prepare_next_chunk_with_wait(&mut self, wait: PrepareWait) {
        if !self.has_prepared_result() {
            self.prepared = Some(self.prepare_chunk(wait));
        }
    }

    delegate::delegate! {
        to self.decoder {
            fn flush_reader_signals(&mut self);
            fn metadata(&self) -> TrackMetadata;
            fn update_byte_len(&self, len: u64);
            fn media_origin(&self) -> Option<Duration>;
            fn release_input(&mut self) -> InputState;
        }
    }

    fn initial_pcm_origin(&self) -> Option<PcmOrigin> {
        self.initial_pcm_origin
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, false, PrepareWait::Wait)
    }

    fn seek_with_wait(
        &mut self,
        pos: Duration,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, false, wait)
    }

    fn seek_prefix(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, true, PrepareWait::Wait)
    }

    fn seek_prefix_with_wait(
        &mut self,
        pos: Duration,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, true, wait)
    }

    fn prepare_seek(&mut self) -> DecodeResult<Option<DecoderSeekOutcome>> {
        self.prepare_seek_with_wait(PrepareWait::Wait)
    }

    fn prepare_seek_with_wait(
        &mut self,
        wait: PrepareWait,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        Ok(self
            .decoder
            .prepare_seek_with_wait(wait)?
            .map(|outcome| self.map_seek_outcome(outcome)))
    }

    fn spec(&self) -> AudioSpec {
        self.target_spec
    }

    fn timeline_gap_frames(&self) -> u64 {
        round_scaled_frames_lossy(
            self.decoder.timeline_gap_frames(),
            self.source_spec.sample_rate.get(),
            self.config.target_sample_rate.get(),
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
                    self.config.target_sample_rate.get(),
                )
            }),
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

pub(super) fn round_scaled_frames(
    count: u64,
    source_rate: u32,
    target_rate: u32,
) -> DecodeResult<u64> {
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

#[cfg(test)]
mod origin_tests {
    #[cfg(feature = "resample-rubato")]
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_resampler::{ResamplerBuildError, ResamplerCapabilities, ResamplerError};
    use kithara_stream::PrerollHint;
    use kithara_test_fixtures::mock_fixtures::zero_packet;

    use super::*;
    use crate::test_pools::{Pools, pools, sample_buffer};

    #[derive(Clone)]
    struct RepeatBackend;

    struct RepeatResampler {
        mode: ResamplerMode,
        factor: usize,
    }

    impl ResamplerBackend for RepeatBackend {
        type Resampler = RepeatResampler;

        fn build<S>(
            &self,
            settings: &ResamplerSettings<S>,
        ) -> Result<Self::Resampler, ResamplerBuildError>
        where
            S: HasPool<f32>,
        {
            assert_eq!(settings.channels, NonZeroUsize::MIN);
            let ResamplerMode::FixedRatio {
                source_sample_rate,
                target_sample_rate,
            } = settings.mode
            else {
                panic!("fixed-ratio test backend");
            };
            assert!(
                [(1_000, 2_000), (8_000, 48_000)]
                    .contains(&(source_sample_rate.get(), target_sample_rate.get()))
            );
            Ok(RepeatResampler {
                mode: settings.mode,
                factor: usize::try_from(target_sample_rate.get() / source_sample_rate.get())
                    .expect("small integer rate conversion"),
            })
        }
        fn capabilities(&self) -> ResamplerCapabilities {
            ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
        }
        fn name(&self) -> &'static str {
            "repeat-fixture"
        }
    }

    impl Resampler for RepeatResampler {
        fn capabilities(&self) -> ResamplerCapabilities {
            ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
        }
        fn channels(&self) -> NonZeroUsize {
            NonZeroUsize::MIN
        }
        fn input_frames_max(&self) -> usize {
            4
        }
        fn input_frames_next(&self) -> usize {
            4
        }
        fn mode(&self) -> ResamplerMode {
            self.mode
        }
        fn output_frames_for_input(&self, frames: usize) -> usize {
            frames * self.factor
        }
        fn output_frames_max(&self) -> usize {
            4 * self.factor
        }
        fn output_frames_next(&self) -> usize {
            4 * self.factor
        }
        fn process_into_buffer(
            &mut self,
            input: &[&[f32]],
            output: &mut [&mut [f32]],
        ) -> Result<ResamplerProcess, ResamplerError> {
            for (value, pair) in input[0].iter().zip(output[0].chunks_exact_mut(self.factor)) {
                pair.fill(*value);
            }
            Ok(ResamplerProcess::new(4, 4 * self.factor))
        }
        fn reset(&mut self) {}
    }

    struct OffsetDecoder {
        rate: NonZeroU32,
        length: u64,
        declared: crate::DecoderDuration,
        media_origin: Option<Duration>,
        origin: u64,
        phase: Duration,
        next_frame: u64,
        original: Option<PcmOrigin>,
        observing_original: bool,
        pools: Pools,
    }

    impl OffsetDecoder {
        fn position(&self, frame: u64) -> DecodeResult<Duration> {
            Ok(self.spec().duration_for(frame)? + self.phase)
        }

        fn seek_selected(
            &mut self,
            position: Duration,
            prefix: bool,
        ) -> DecodeResult<DecoderSeekOutcome> {
            if !position.is_zero() {
                self.observing_original = false;
            }
            let native = position.saturating_sub(self.phase);
            let frame = if prefix {
                u64::try_from(native.as_nanos() * u128::from(self.rate.get()) / 1_000_000_000)
                    .expect("bounded native prefix")
            } else {
                self.spec().frame_at(native)?
            }
            .max(self.origin);
            assert!((self.origin..self.origin + self.length).contains(&frame));
            self.next_frame = frame;
            Ok(DecoderSeekOutcome::Landed {
                landed_at: self.position(frame)?,
                landed_frame: frame,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }
    }

    impl Decoder for OffsetDecoder {
        fn initial_pcm_origin(&self) -> Option<PcmOrigin> {
            self.original
        }
        fn duration(&self) -> Option<crate::DecoderDuration> {
            Some(self.declared)
        }
        fn media_origin(&self) -> Option<Duration> {
            self.media_origin
        }
        fn spec(&self) -> AudioSpec {
            AudioSpec::new(1, self.rate)
        }
        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            let end = self.origin + self.length;
            if self.next_frame == end {
                return Ok(DecoderChunkOutcome::Eof);
            }
            let start = self.next_frame;
            if self.observing_original {
                let position = self.position(start)?;
                self.original = Some(PcmOrigin::new(position, position));
                self.observing_original = false;
            }
            let frames = (end - start).min(64);
            self.next_frame += frames;
            let samples: Vec<_> = (start..self.next_frame)
                .map(|at| {
                    f32::from(u16::try_from(at - self.origin).expect("fixture frame fits u16"))
                        / 2_000.0
                })
                .collect();
            Ok(DecoderChunkOutcome::Chunk(Box::new(AudioChunk::new(
                AudioChunkInfo {
                    spec: self.spec(),
                    frames: u32::try_from(frames).expect("bounded physical PCM"),
                    frame_offset: start,
                    timestamp: self.position(start)?,
                    end_timestamp: self.position(self.next_frame)?,
                    ..AudioChunkInfo::default()
                },
                sample_buffer(&self.pools, &samples),
            ))))
        }
        fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            self.seek_selected(position, false)
        }
        fn seek_prefix(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            self.seek_selected(position, true)
        }
        fn update_byte_len(&self, _len: u64) {}
    }

    fn assert_physical_output(chunk: &AudioChunk, origin: u64) {
        assert_eq!(chunk.frames(), chunk.meta.frames as usize);
        assert_eq!(chunk.samples.len(), chunk.frames());
        assert_eq!(chunk.spec().sample_rate.get(), 2_000);
        assert_eq!(
            chunk.meta.timestamp,
            chunk
                .spec()
                .duration_for(chunk.meta.frame_offset)
                .expect("output timestamp")
        );
        assert_eq!(
            chunk.meta.end_timestamp,
            chunk
                .spec()
                .duration_for(chunk.meta.frame_range().end)
                .expect("output endpoint")
        );
        let span = chunk
            .meta
            .source_span
            .expect("exact resampled source geometry");
        assert_eq!(span.sample_rate(), chunk.spec().sample_rate);
        assert_eq!(span.position_at(0), Some(chunk.meta.timestamp));
        assert_eq!(
            span.position_at(span.output_frames()),
            Some(chunk.meta.end_timestamp)
        );
        assert_eq!(
            span.source_ratio_at(0),
            Some((
                u128::from(chunk.meta.frame_offset),
                std::num::NonZeroU128::MIN
            ))
        );

        for (offset, value) in chunk.samples.iter().enumerate() {
            let source_frame = (chunk.meta.frame_offset + offset as u64) / 2;
            assert_eq!(
                *value,
                f32::from(u16::try_from(source_frame - origin).expect("fixture frame fits u16"))
                    / 2_000.0
            );
        }
    }

    #[kithara::hang_watchdog]
    fn first_pcm(decoder: &mut dyn Decoder) -> Box<AudioChunk> {
        loop {
            hang_tick!();
            match decoder.next_chunk().expect("resampled PCM") {
                DecoderChunkOutcome::Chunk(chunk) => return chunk,
                DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
                other => panic!("expected the first resampled PCM, got {other:?}"),
            }
        }
    }

    #[kithara::test]
    fn first_resampled_pcm_keeps_the_head_basis_after_read_ahead_crosses_a_pts_gap(
        zero_packet: &'static [u8],
    ) {
        let pools = pools();
        let expected = [0.25, 0.25, -0.5, -0.5, 0.25, 0.25, -0.5, -0.5];
        let source = crate::composed::origin_tests::head_gap_fixture(vec![0.25, -0.5], zero_packet);
        let config = DecoderResamplerConfig::builder()
            .target_sample_rate(NonZeroU32::new(2_000).expect("target rate"))
            .backend(RepeatBackend)
            .build();
        let mut decoder = ResampledDecoder::new(source, config, &pools).expect("resampled decoder");
        assert_eq!(decoder.initial_pcm_origin(), None);
        let output = first_pcm(&mut decoder);
        let origin = decoder
            .initial_pcm_origin()
            .expect("paired original geometry");
        assert_eq!(output.meta.timestamp, Duration::from_millis(59));
        assert_eq!(output.meta.frame_offset, 118);
        assert_eq!(output.meta.frames, 8);
        assert_eq!(output.samples.as_ref(), expected.as_slice());
        assert_eq!(decoder.timeline_gap_frames(), 62);
        assert_eq!(origin.position(), Duration::from_millis(59));
        assert_eq!(origin.comparison_base(), Duration::from_millis(42));
        for (native_rate, output_rate, physical_origin, requested_ns) in [
            (1_000, 2_000, 42, 43_300_000),
            (8_000, 48_000, 338, 42_350_000),
        ] {
            let native_rate = NonZeroU32::new(native_rate).expect("native rate");
            let output_rate = NonZeroU32::new(output_rate).expect("output rate");
            let raw_origin = Duration::from_nanos(42_250_000);
            let native_spec = AudioSpec::new(1, native_rate);
            let source = OffsetDecoder {
                rate: native_rate,
                length: 128,
                declared: crate::DecoderDuration::RawLength(Duration::from_secs(1)),
                media_origin: Some(raw_origin),
                origin: physical_origin,
                phase: raw_origin
                    - native_spec
                        .duration_for(physical_origin)
                        .expect("physical native origin"),
                next_frame: physical_origin,
                original: Some(PcmOrigin::new(raw_origin, raw_origin)),
                observing_original: false,
                pools: pools.clone(),
            };
            let config = DecoderResamplerConfig::builder()
                .target_sample_rate(output_rate)
                .backend(RepeatBackend)
                .build();
            let mut decoder = ResampledDecoder::new(Box::new(source), config, &pools)
                .expect("actual SRC wrapper");
            let first = first_pcm(&mut decoder);
            let factor =
                usize::try_from(output_rate.get() / native_rate.get()).expect("integer SRC ratio");
            let anchor = decoder
                .spec()
                .frame_at(raw_origin)
                .expect("integral physical output anchor");
            assert_eq!(first.meta.frame_offset, anchor);
            assert_eq!(first.meta.timestamp, raw_origin);
            assert_eq!(first.frames(), 64 * factor);
            let expected = (0..64_u16)
                .flat_map(|index| std::iter::repeat_n(f32::from(index) / 2_000.0, factor))
                .collect::<Vec<_>>();
            assert_eq!(first.samples.as_ref(), expected.as_slice());
            let span = first.meta.source_span.expect("exact native SRC basis");
            assert_eq!(span.position_at(0), Some(raw_origin));
            assert_eq!(span.sample_rate(), output_rate);
            assert_eq!(
                span.output_ratio_at_source(
                    anchor,
                    (
                        raw_origin.as_nanos() * u128::from(output_rate.get()),
                        std::num::NonZeroU128::new(1_000_000_000).expect("raw nanoseconds")
                    )
                ),
                Some((u128::from(anchor), std::num::NonZeroU128::MIN))
            );
            if native_rate.get() == 1_000 {
                assert_eq!(anchor, 85);
                assert_eq!(
                    span.source_ratio_at(0),
                    Some((
                        169,
                        std::num::NonZeroU128::new(2).expect("subsample raw origin")
                    ))
                );
                assert_ne!(
                    first.meta.timestamp,
                    decoder.spec().duration_for(anchor).expect("physical label")
                );
            }
            let next = first_pcm(&mut decoder);
            assert_eq!(
                next.meta.frame_offset,
                anchor + u64::try_from(first.frames()).expect("output extent")
            );
            assert_eq!(
                next.meta.timestamp,
                raw_origin + native_spec.duration_for(64).expect("first native extent")
            );
            assert_eq!(next.samples[0], 64.0 / 2_000.0);
            assert_eq!(
                next.meta
                    .source_span
                    .expect("same immutable SRC basis")
                    .source_ratio_at(0),
                span.source_ratio_at(span.output_frames())
            );
            let mut trimmer = crate::GaplessTrimmer::from(crate::GaplessInfo {
                leading_frames: 2,
                trailing_frames: 2,
            });
            let output_frames = u64::try_from(first.frames()).expect("actual SRC extent");
            let expected_span = span
                .for_output_range(2..output_frames - 2)
                .expect("mapped PCM trim");
            assert!(trimmer.push(*first).expect("valid gapless PCM").is_empty());
            let mut trimmed = trimmer.flush().expect("valid gapless PCM");
            assert_eq!(trimmed.len(), 1);
            let trimmed = trimmed.remove(0);
            assert_eq!(trimmed.meta.frame_offset, anchor + 2);
            assert_eq!(trimmed.samples.as_ref(), &expected[2..expected.len() - 2]);
            assert_eq!(trimmed.meta.source_span, Some(expected_span));
            assert_eq!(
                u64::from(trimmed.meta.frames),
                expected_span.output_frames()
            );
            assert_eq!(
                trimmed.meta.timestamp,
                expected_span.position_at(0).expect("trimmed raw start")
            );
            assert_eq!(
                trimmed.meta.end_timestamp,
                expected_span
                    .position_at(expected_span.output_frames())
                    .expect("trimmed raw endpoint")
            );
            let requested = Duration::from_nanos(requested_ns);
            let prefix_bound = requested.saturating_sub(Duration::from_nanos(
                1_000_000_000u64.div_ceil(u64::from(output_rate.get())),
            ));
            decoder.seek(prefix_bound).expect("ordinary nearest seek");
            let nearest = first_pcm(&mut decoder);
            assert_eq!(nearest.samples[0], 1.0 / 2_000.0);
            assert_eq!(
                nearest.meta.timestamp,
                raw_origin + native_spec.duration_for(1).expect("next native sample")
            );
            decoder
                .seek_prefix(requested)
                .expect("owned native prefix seek");
            let prefix = first_pcm(&mut decoder);
            assert_eq!(
                prefix.samples[0],
                if native_rate.get() == 1_000 {
                    1.0 / 2_000.0
                } else {
                    0.0
                }
            );
            assert!(prefix.meta.timestamp <= requested);
            assert_eq!(
                prefix
                    .meta
                    .source_span
                    .expect("postseek actual basis")
                    .position_at(0),
                Some(prefix.meta.timestamp)
            );
            assert_eq!(
                decoder.initial_pcm_origin().map(PcmOrigin::position),
                Some(raw_origin)
            );
        }
        assert_eq!(
            decoder
                .spec()
                .frame_at(origin.comparison_base())
                .expect("basis")
                + decoder.timeline_gap_frames(),
            146
        );
    }

    #[kithara::test]
    #[kithara::hang_watchdog]
    #[case::observed_nonzero(700, false, true)]
    #[case::known_nonzero_before_output(700, true, false)]
    #[case::known_zero_before_output(0, true, false)]
    #[case::unknown_sought_suffix(700, false, false)]
    fn original_resampled_clock_survives_a_nonzero_seek(
        #[case] origin: u64,
        #[case] reports_origin: bool,
        #[case] observe_original: bool,
    ) {
        let pools = pools();
        let source = OffsetDecoder {
            rate: NonZeroU32::new(1_000).expect("source rate"),
            length: 1_000,
            declared: crate::DecoderDuration::RawLength(Duration::from_secs(60)),
            media_origin: None,
            phase: Duration::ZERO,
            origin,
            next_frame: origin,
            original: reports_origin.then(|| {
                let position = Duration::from_millis(origin);
                PcmOrigin::new(position, position)
            }),
            observing_original: !reports_origin,
            pools: pools.clone(),
        };
        let config = DecoderResamplerConfig::builder()
            .target_sample_rate(NonZeroU32::new(2_000).expect("target rate"))
            .backend(RepeatBackend)
            .build();
        let mut decoder =
            ResampledDecoder::new(Box::new(source), config, &pools).expect("production wrapper");
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            reports_origin.then(|| Duration::from_millis(origin))
        );
        if observe_original {
            let DecoderChunkOutcome::Chunk(first) =
                decoder.next_chunk().expect("original resampled PCM")
            else {
                panic!("original PCM chunk");
            };
            assert_physical_output(&first, origin);
            assert_eq!(first.frames(), 128);
            assert_eq!(first.meta.frame_offset, origin * 2);
            assert_eq!(first.meta.timestamp, Duration::from_millis(origin));
            assert_eq!(&first.samples[..2], &[0.0; 2]);
        }
        let original = (reports_origin || observe_original).then(|| Duration::from_millis(origin));
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            original
        );
        let target = Duration::from_millis(origin + 800);
        let outcome = decoder.seek(target).expect("resampled physical seek");
        assert!(
            matches!(outcome, DecoderSeekOutcome::Landed { landed_at, landed_frame, .. }
            if landed_at == target && landed_frame == (origin + 800) * 2)
        );
        let mut frames = 0usize;
        let mut endpoint = target;
        loop {
            hang_tick!();
            match decoder.next_chunk().expect("resampled suffix") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    assert_physical_output(&chunk, origin);
                    assert_eq!(chunk.meta.timestamp, endpoint);
                    frames += chunk.frames();
                    endpoint = chunk.meta.end_timestamp;
                    hang_reset!();
                }
                DecoderChunkOutcome::Eof => break,
                DecoderChunkOutcome::Pending(reason) => {
                    panic!("bounded fixture cannot pend: {reason:?}")
                }
            }
        }
        assert_eq!(frames, 400);
        assert_eq!(endpoint, Duration::from_millis(origin + 1_000));
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            original
        );
        if let Some(original) = original {
            assert_eq!(endpoint - original, Duration::from_secs(1));
        }
    }
    #[cfg(feature = "resample-rubato")]
    #[kithara::test]
    #[kithara::hang_watchdog]
    fn original_resampled_basis_retains_exact_time_under_output_clock_rounding() {
        let pools = pools();
        let original = PcmOrigin::new(Duration::from_millis(701), Duration::from_millis(701));
        let source = OffsetDecoder {
            rate: NonZeroU32::new(1_000).expect("source rate"),
            length: 1_000,
            declared: crate::DecoderDuration::RawLength(Duration::from_secs(60)),
            media_origin: None,
            phase: Duration::ZERO,
            origin: 701,
            next_frame: 701,
            original: Some(original),
            observing_original: false,
            pools: pools.clone(),
        };
        let config = DecoderResamplerConfig::builder()
            .target_sample_rate(NonZeroU32::new(1_500).expect("target rate"))
            .backend(RubatoBackend::new())
            .build();
        let mut decoder =
            ResampledDecoder::new(Box::new(source), config, &pools).expect("production wrapper");
        let before = decoder
            .initial_pcm_origin()
            .expect("known original geometry");
        assert_eq!(before.position(), Duration::from_millis(701));
        assert_eq!(before.comparison_base(), Duration::from_millis(701));
        let head = first_pcm(&mut decoder);
        assert_eq!(head.meta.frame_offset, 1_052);
        assert_eq!(head.meta.timestamp, Duration::from_millis(701));
        assert_eq!(
            head.meta
                .source_span
                .expect("unsnapped raw head")
                .source_ratio_at(0),
            Some((
                2_103,
                std::num::NonZeroU128::new(2).expect("half output frame")
            ))
        );
        decoder
            .seek(Duration::from_millis(900))
            .expect("nonzero seek");
        let chunk = first_pcm(&mut decoder);
        assert!(chunk.meta.timestamp >= Duration::from_millis(900));
        assert_eq!(decoder.initial_pcm_origin(), Some(before));
        let native_rate = NonZeroU32::new(44_100).expect("native rate");
        let native = AudioSpec::new(1, native_rate);
        let origin = native.duration_for(1).expect("nonzero native origin");
        let length = native.duration_for(44_105).expect("native length");
        for declared in [
            crate::DecoderDuration::RawLength(length),
            crate::DecoderDuration::MediaEnd(length),
        ] {
            let source = OffsetDecoder {
                rate: native_rate,
                length: 44_105,
                declared,
                media_origin: Some(origin),
                phase: Duration::ZERO,
                origin: 1,
                next_frame: 1,
                original: Some(PcmOrigin::new(origin, origin)),
                observing_original: false,
                pools: pools.clone(),
            };
            let config = DecoderResamplerConfig::builder()
                .target_sample_rate(NonZeroU32::new(48_000).expect("actual output rate"))
                .backend(RubatoBackend::new())
                .build();
            let mut decoder =
                ResampledDecoder::new(Box::new(source), config, &pools).expect("actual resampler");
            let first = first_pcm(&mut decoder);
            assert_eq!(first.meta.frame_offset, 1);
            assert!(
                !decoder.eof_flushed,
                "the declaration must be exact before natural EOF"
            );
            assert_eq!(decoder.media_origin(), Some(origin));
            let physical_origin = decoder
                .initial_pcm_origin()
                .expect("actual output origin")
                .position();
            let declared_end = match decoder.duration().expect("mapped output declaration") {
                crate::DecoderDuration::RawLength(length) => physical_origin + length,
                crate::DecoderDuration::MediaEnd(end) => origin + end,
            };
            assert_eq!(
                decoder
                    .spec()
                    .frame_at(declared_end)
                    .expect("declared physical endpoint"),
                48_006
            );
            let mut output_frames = u64::from(first.meta.frames);
            let mut endpoint = first.meta.frame_range().end;
            loop {
                hang_tick!();
                match decoder.next_chunk().expect("actual resampled output") {
                    DecoderChunkOutcome::Chunk(chunk) => {
                        output_frames += u64::from(chunk.meta.frames);
                        endpoint = chunk.meta.frame_range().end;
                        hang_reset!();
                    }
                    DecoderChunkOutcome::Pending(_) => {}
                    DecoderChunkOutcome::Eof => break,
                }
            }
            assert_eq!(output_frames, 48_005);
            assert_eq!(endpoint, 48_006);
            assert_eq!(
                decoder
                    .spec()
                    .frame_at(declared_end)
                    .expect("same early declaration"),
                endpoint
            );
        }
    }

    #[cfg(feature = "resample-rubato")]
    #[kithara::test]
    #[case::replay_original_head(0, 1_052)]
    #[case::sought_suffix(900, 1_350)]
    fn resampled_head_and_seek_landings_share_the_actual_output_frame_clock(
        #[case] requested_millis: u64,
        #[case] expected_frame: u64,
    ) {
        let pools = pools();
        let source = OffsetDecoder {
            rate: NonZeroU32::new(1_000).expect("source rate"),
            length: 1_000,
            declared: crate::DecoderDuration::RawLength(Duration::from_secs(60)),
            media_origin: None,
            phase: Duration::ZERO,
            origin: 701,
            next_frame: 701,
            original: Some(PcmOrigin::new(
                Duration::from_millis(701),
                Duration::from_millis(701),
            )),
            observing_original: false,
            pools: pools.clone(),
        };
        let config = DecoderResamplerConfig::builder()
            .target_sample_rate(NonZeroU32::new(1_500).expect("target rate"))
            .backend(RubatoBackend::new())
            .build();
        let mut decoder =
            ResampledDecoder::new(Box::new(source), config, &pools).expect("production wrapper");
        let spec = decoder.spec();
        let original = decoder
            .initial_pcm_origin()
            .map(PcmOrigin::position)
            .expect("known original head");
        assert_eq!(original, Duration::from_millis(701));
        assert_eq!(spec.frame_at(original).expect("original frame"), 1_052);
        let DecoderSeekOutcome::Landed {
            landed_at,
            landed_frame,
            ..
        } = decoder
            .seek(Duration::from_millis(requested_millis))
            .expect("actual source landing")
        else {
            panic!("seek landing");
        };
        assert_eq!(landed_frame, expected_frame);
        assert_eq!(
            spec.frame_at(landed_at).expect("landing frame"),
            landed_frame
        );
        if requested_millis == 0 {
            assert_eq!(landed_at, Duration::from_millis(701));
            assert_eq!(landed_at, original);
            assert_eq!(
                landed_frame,
                spec.frame_at(original).expect("original frame")
            );
        } else {
            assert_eq!(landed_at, Duration::from_millis(900));
            assert!(landed_frame > spec.frame_at(original).expect("original frame"));
        }
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(original)
        );
        let output = first_pcm(&mut decoder);
        assert_eq!(output.meta.frame_offset, expected_frame);
        assert_eq!(output.meta.timestamp, landed_at);
        let span = output.meta.source_span.expect("actual native source basis");
        assert_eq!(span.position_at(0), Some(landed_at));
        if requested_millis == 0 {
            assert_eq!(
                span.source_ratio_at(0),
                Some((
                    2_103,
                    std::num::NonZeroU128::new(2).expect("half output frame")
                ))
            );
        }
    }
}

#[cfg(all(test, feature = "resample-rubato"))]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use kithara_platform::sync::Arc;
    use kithara_resampler::{ResamplerOptions, ResamplerQuality, rubato::RubatoBackend};
    use kithara_stream::{
        BoxedEventSink, PrerollHint, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal,
    };

    use super::*;
    use crate::test_pools::{Pools, TestPools, pools, sample_buffer};

    #[derive(Default)]
    struct Calls {
        reads: usize,
        prepares: usize,
        signals: Vec<&'static str>,
        flushes: usize,
        seek_script: VecDeque<DecoderSeekOutcome>,
    }

    struct Hooks(Arc<Mutex<Calls>>);

    impl ReaderEventSink for Hooks {
        fn on_chunk(&mut self, signal: ReaderChunkSignal) {
            let tag = match signal {
                ReaderChunkSignal::Chunk => "chunk",
                ReaderChunkSignal::Pending(_) => "pending",
                ReaderChunkSignal::Eof => "eof",
                _ => panic!("unexpected reader signal"),
            };
            self.0.lock().expect("calls").signals.push(tag);
        }

        fn on_seek(&mut self, _signal: ReaderSeekSignal) {
            self.0.lock().expect("calls").signals.push("seek");
        }

        fn flush(&mut self) {
            self.0.lock().expect("calls").flushes += 1;
        }
    }

    struct ScriptedDecoder {
        outcomes: VecDeque<DecodeResult<DecoderChunkOutcome>>,
        after_seek: Option<Vec<DecodeResult<DecoderChunkOutcome>>>,
        calls: Arc<Mutex<Calls>>,
        hooks: BoxedEventSink,
        seeking: Option<PendingReason>,
    }

    impl ScriptedDecoder {
        fn seek_outcome(&mut self, outcome: DecoderSeekOutcome) -> DecoderSeekOutcome {
            let signal = match outcome {
                DecoderSeekOutcome::Pending(reason) => {
                    self.seeking = Some(reason);
                    return outcome;
                }
                DecoderSeekOutcome::Landed {
                    landed_byte,
                    preroll,
                    ..
                } => ReaderSeekSignal::Landed {
                    landed_byte,
                    preroll,
                },
                DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
            };
            self.seeking = None;
            self.hooks.on_seek(signal);
            outcome
        }
    }

    impl Decoder for ScriptedDecoder {
        fn duration(&self) -> Option<crate::DecoderDuration> {
            Some(crate::DecoderDuration::RawLength(Duration::from_secs(2)))
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            if let Some(reason) = self.seeking {
                return Ok(DecoderChunkOutcome::Pending(reason));
            }
            self.calls.lock().expect("calls").reads += 1;
            let result = self.outcomes.pop_front().expect("bounded source script");
            if let Ok(outcome) = &result {
                self.hooks.on_chunk(match outcome {
                    DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
                    DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
                    DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
                });
            }
            result
        }

        fn prepare_next_chunk(&mut self) {
            self.calls.lock().expect("calls").prepares += 1;
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            self.outcomes = self.after_seek.take().expect("seek script").into();
            let scripted = self.calls.lock().expect("calls").seek_script.pop_front();
            let outcome = scripted.unwrap_or_else(|| DecoderSeekOutcome::Landed {
                landed_at: pos,
                landed_frame: source_spec().frame_at(pos).expect("source frame"),
                landed_byte: Some(0),
                preroll: PrerollHint::NotNeeded,
            });
            Ok(self.seek_outcome(outcome))
        }

        fn prepare_seek(&mut self) -> DecodeResult<Option<DecoderSeekOutcome>> {
            if self.seeking.is_none() {
                return Ok(None);
            }
            let outcome = self
                .calls
                .lock()
                .expect("calls")
                .seek_script
                .pop_front()
                .expect("bounded seek continuation script");
            Ok(Some(self.seek_outcome(outcome)))
        }

        fn spec(&self) -> AudioSpec {
            source_spec()
        }

        fn flush_reader_signals(&mut self) {
            self.hooks.flush();
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    fn source_spec() -> AudioSpec {
        AudioSpec::new(1, NonZeroU32::new(44_100).expect("source rate"))
    }

    fn chunk(
        pools: &Pools,
        frames: usize,
        offset: u64,
        source_revision: u64,
    ) -> DecoderChunkOutcome {
        let spec = source_spec();
        let samples: Vec<f32> = (0..frames)
            .map(|frame| {
                let frame = offset + u64::try_from(frame).expect("fixture frame");
                let marker = u8::try_from(frame % 31).expect("bounded marker");
                f32::from(marker) / 31.0
            })
            .collect();
        let frames_u64 = u64::try_from(frames).expect("fixture length");
        DecoderChunkOutcome::Chunk(Box::new(AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: u32::try_from(frames).expect("fixture frames"),
                timestamp: spec.duration_for(offset).expect("fixture timestamp"),
                end_timestamp: spec.duration_for(offset + frames_u64).expect("fixture end"),
                frame_offset: offset,
                source_revision,
                ..AudioChunkInfo::default()
            },
            sample_buffer(pools, &samples),
        )))
    }

    fn scripted_resampled(
        pools: &Pools,
        outcomes: Vec<DecodeResult<DecoderChunkOutcome>>,
        after_seek: Option<Vec<DecodeResult<DecoderChunkOutcome>>>,
    ) -> (
        ResampledDecoder<RubatoBackend, TestPools>,
        Arc<Mutex<Calls>>,
    ) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let inner = ScriptedDecoder {
            outcomes: outcomes.into(),
            after_seek,
            seeking: None,
            calls: Arc::clone(&calls),
            hooks: Box::new(Hooks(Arc::clone(&calls))),
        };
        let config = DecoderResamplerConfig::builder()
            .backend(RubatoBackend::new())
            .target_sample_rate(NonZeroU32::new(48_000).expect("target rate"))
            .options(ResamplerOptions::builder().chunk_size(256).build())
            .quality(ResamplerQuality::Fast)
            .build();
        (
            ResampledDecoder::new(Box::new(inner), config, pools).expect("resampled decoder"),
            calls,
        )
    }

    #[kithara::rtsan_forbid_blocking]
    fn consume_prepared(decoder: &mut dyn Decoder) -> DecodeResult<DecoderChunkOutcome> {
        decoder.next_chunk_prepared()
    }

    #[kithara::test]
    fn a_short_inner_chunk_takes_one_preparation_step() {
        let pools = pools();
        let (mut decoder, calls) = scripted_resampled(
            &pools,
            vec![Ok(chunk(&pools, 128, 0, 1)), Ok(chunk(&pools, 128, 128, 1))],
            None,
        );
        assert!(decoder.resampler.input_frames_next() > 128);
        decoder.prepare_next_chunk();
        assert_eq!(calls.lock().expect("calls").reads, 1);
        assert_eq!(calls.lock().expect("calls").prepares, 1);
        assert!(!decoder.has_prepared_result());
        assert!(matches!(
            consume_prepared(&mut decoder).expect("provisional result"),
            DecoderChunkOutcome::Pending(PendingReason::Retry)
        ));
        assert_eq!(calls.lock().expect("calls").reads, 1);
        decoder.prepare_next_chunk();
        assert_eq!(calls.lock().expect("calls").reads, 2);
        assert_eq!(calls.lock().expect("calls").prepares, 2);
        assert!(decoder.has_prepared_result());
        let DecoderChunkOutcome::Chunk(output) =
            consume_prepared(&mut decoder).expect("completed block")
        else {
            panic!("two short inputs must complete a resampler block");
        };
        assert!(output.frames() > 0);
        assert_eq!(output.meta.frame_offset, 0);
        assert_eq!(decoder.source_frames_seen, 256);
        assert_eq!(calls.lock().expect("calls").signals, ["chunk", "chunk"]);
    }

    #[derive(Clone, Copy, Debug)]
    enum Completion {
        Chunk,
        Error,
        Eof,
    }

    fn completed(pools: &Pools, completion: Completion) -> DecodeResult<DecoderChunkOutcome> {
        match completion {
            Completion::Chunk => Ok(chunk(pools, 512, 0, 1)),
            Completion::Error => Err(DecodeError::InvalidData {
                detail: "scripted decode failure",
            }),
            Completion::Eof => Ok(DecoderChunkOutcome::Eof),
        }
    }

    #[kithara::test]
    #[case::chunk(Completion::Chunk)]
    #[case::error(Completion::Error)]
    #[case::eof(Completion::Eof)]
    fn completed_preparation_is_retained_and_delivered_once(#[case] completion: Completion) {
        let pools = pools();
        let (mut decoder, calls) =
            scripted_resampled(&pools, vec![completed(&pools, completion)], None);
        decoder.prepare_next_chunk();
        assert!(decoder.has_prepared_result());
        let allocated = pools.stats().allocated_bytes;
        let source_frames = decoder.source_frames_seen;
        let emitted_frames = decoder.emitted_frames;
        let signals = calls.lock().expect("calls").signals.len();
        for _ in 0..2 {
            decoder.prepare_next_chunk();
            assert!(decoder.has_prepared_result());
            assert_eq!(calls.lock().expect("calls").reads, 1);
            assert_eq!(calls.lock().expect("calls").prepares, 1);
            assert_eq!(decoder.source_frames_seen, source_frames);
            assert_eq!(pools.stats().allocated_bytes, allocated);
        }
        let result = consume_prepared(&mut decoder);
        assert_eq!(calls.lock().expect("calls").reads, 1);
        assert_eq!(calls.lock().expect("calls").prepares, 1);
        assert_eq!(calls.lock().expect("calls").signals.len(), signals);
        assert_eq!(pools.stats().allocated_bytes, allocated);
        assert_eq!(decoder.source_frames_seen, source_frames);
        assert_eq!(decoder.emitted_frames, emitted_frames);
        match (completion, result) {
            (Completion::Chunk, Ok(DecoderChunkOutcome::Chunk(output))) => {
                assert!(output.frames() > 0);
                assert_eq!(output.meta.source_revision, 1);
            }
            (Completion::Error, Err(DecodeError::InvalidData { detail })) => {
                assert_eq!(detail, "scripted decode failure");
            }
            (Completion::Eof, Ok(DecoderChunkOutcome::Eof)) => {}
            other => panic!("completed result was replaced: {other:?}"),
        }
        assert!(!decoder.has_prepared_result());
        assert!(matches!(
            consume_prepared(&mut decoder),
            Err(DecodeError::InvalidData { .. })
        ));
        assert_eq!(calls.lock().expect("calls").reads, 1);
        assert_eq!(calls.lock().expect("calls").prepares, 1);
        assert_eq!(
            calls.lock().expect("calls").signals.len(),
            usize::from(!matches!(completion, Completion::Error))
        );
        decoder.flush_reader_signals();
        assert_eq!(calls.lock().expect("calls").flushes, 1);
    }

    #[kithara::test]
    #[case::chunk(Completion::Chunk, false)]
    #[case::error(Completion::Error, false)]
    #[case::eof(Completion::Eof, false)]
    #[case::pending_chunk(Completion::Chunk, true)]
    #[case::pending_error(Completion::Error, true)]
    #[case::pending_eof(Completion::Eof, true)]
    fn seek_discards_a_completed_preparation(
        #[case] completion: Completion,
        #[case] pending: bool,
    ) {
        let pools = pools();
        let (mut decoder, calls) = scripted_resampled(
            &pools,
            vec![completed(&pools, completion)],
            Some(vec![Ok(chunk(&pools, 512, 44_100, 2))]),
        );
        decoder.prepare_next_chunk();
        assert!(decoder.has_prepared_result());
        let target = Duration::from_secs(1);
        if pending {
            calls.lock().expect("calls").seek_script = vec![
                DecoderSeekOutcome::Pending(PendingReason::Retry),
                DecoderSeekOutcome::Pending(PendingReason::Retry),
                DecoderSeekOutcome::Landed {
                    landed_at: target,
                    landed_frame: 44_100,
                    landed_byte: Some(1_234),
                    preroll: PrerollHint::Required(1_000),
                },
            ]
            .into();
        }
        let outcome = decoder.seek(target).expect("seek");
        if pending {
            assert_eq!(outcome, DecoderSeekOutcome::Pending(PendingReason::Retry));
            assert!(decoder.reanchor_output_on_next_chunk);
            assert_eq!(
                calls
                    .lock()
                    .expect("calls")
                    .signals
                    .iter()
                    .filter(|s| **s == "seek")
                    .count(),
                0
            );
            decoder.prepare_next_chunk();
            assert!(matches!(
                consume_prepared(&mut decoder).expect("unresolved seek blocks output"),
                DecoderChunkOutcome::Pending(PendingReason::Retry)
            ));
            assert_eq!(calls.lock().expect("calls").reads, 1);
            assert_eq!(decoder.emitted_frames, 0);
            assert_eq!(decoder.source_frames_seen, 0);
            assert_eq!(
                decoder.prepare_seek().expect("pending continuation"),
                Some(outcome)
            );
            assert!(decoder.reanchor_output_on_next_chunk);
            assert_eq!(
                decoder.prepare_seek().expect("terminal continuation"),
                Some(DecoderSeekOutcome::Landed {
                    landed_at: target,
                    landed_frame: 48_000,
                    landed_byte: Some(1_234),
                    preroll: PrerollHint::Required(1_000),
                })
            );
            assert!(decoder.reanchor_output_on_next_chunk);
            assert_eq!(decoder.prepare_seek().expect("settled continuation"), None);
            assert_eq!(decoder.emitted_frames, 0);
            assert_eq!(decoder.source_frames_seen, 0);
        }
        assert!(!decoder.has_prepared_result());
        assert!(matches!(
            consume_prepared(&mut decoder),
            Err(DecodeError::InvalidData { .. })
        ));
        decoder.prepare_next_chunk();
        let DecoderChunkOutcome::Chunk(output) =
            consume_prepared(&mut decoder).expect("after seek")
        else {
            panic!("seek must deliver the new input");
        };
        assert_eq!(output.meta.source_revision, 2);
        assert_eq!(output.meta.timestamp, Duration::from_secs(1));
        assert_eq!(output.meta.frame_offset, 48_000);
        assert_eq!(calls.lock().expect("calls").reads, 2);
        assert_eq!(
            calls
                .lock()
                .expect("calls")
                .signals
                .iter()
                .filter(|signal| **signal == "seek")
                .count(),
            1
        );
    }

    fn pcm_to_eof(decoder: &mut dyn Decoder, prepared: bool) -> Vec<f32> {
        let mut pcm = Vec::new();
        let mut frame_offset = 0;
        for _ in 0..16 {
            let result = if prepared {
                decoder.prepare_next_chunk();
                consume_prepared(decoder)
            } else {
                decoder.next_chunk()
            };
            match result.expect("decode") {
                DecoderChunkOutcome::Chunk(output) => {
                    assert_eq!(output.meta.frame_offset, frame_offset);
                    frame_offset += u64::try_from(output.frames()).expect("output frames");
                    assert_eq!(output.spec(), decoder.spec());
                    pcm.extend_from_slice(&output.samples);
                }
                DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
                DecoderChunkOutcome::Eof => return pcm,
                other @ DecoderChunkOutcome::Pending(_) => {
                    panic!("unexpected scripted outcome: {other:?}");
                }
            }
        }
        panic!("EOF within the script budget");
    }

    #[kithara::test]
    fn prepared_pcm_eof_residual_accounting_and_hooks_match_regular_decode() {
        let pools = pools();
        let script = || {
            vec![
                Ok(chunk(&pools, 128, 0, 1)),
                Ok(chunk(&pools, 384, 128, 1)),
                Ok(chunk(&pools, 17, 512, 1)),
                Ok(DecoderChunkOutcome::Eof),
                Ok(DecoderChunkOutcome::Eof),
            ]
        };
        let (mut prepared, prepared_calls) = scripted_resampled(&pools, script(), None);
        let (mut regular, regular_calls) = scripted_resampled(&pools, script(), None);
        let actual = pcm_to_eof(&mut prepared, true);
        let expected = pcm_to_eof(&mut regular, false);
        assert_eq!(actual, expected);
        let output_frames = u64::try_from(actual.len()).expect("output frame count");
        assert_eq!(
            output_frames,
            round_scaled_frames(529, 44_100, 48_000).expect("scaled frames")
        );
        assert_eq!(prepared.source_frames_seen, 529);
        assert_eq!(prepared.emitted_frames, output_frames);
        assert_eq!(
            prepared_calls.lock().expect("calls").signals,
            ["chunk", "chunk", "chunk", "eof", "eof"]
        );
        assert_eq!(
            prepared_calls.lock().expect("calls").signals,
            regular_calls.lock().expect("calls").signals
        );
        assert!(!prepared.has_prepared_result());
    }
}
