pub(super) use std::num::NonZeroU32;

pub(super) use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
pub(super) use kithara_decode::{
    DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo, GaplessMode,
};
pub(super) use kithara_platform::{sync::Arc, time::Duration};
pub(super) use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
pub(super) use kithara_stream::{
    AudioCodec, PrerollHint, VariantPromotion, VariantReaderPlan, VariantTransition,
    VariantTransitionId,
};
pub(super) use kithara_test_utils::kithara;

pub(super) use super::super::{
    AudioSource, DecoderFactory, DecoderGeneration, RecreateCause, RecreateState, TrackStep,
    rebuild::{TestControl, TestStream, media_info, produced_data, test_source},
};
pub(super) use crate::{
    pipeline::{
        decode::core::ActiveDecode,
        gapless::visible_duration,
        source::{SourceDecoderConfig, StreamAudioSource},
    },
    test_pools::{Pools, sample_buffer},
};

#[derive(Clone, Copy)]
pub(super) struct PcmExtent {
    pub(super) origin: u64,
    pub(super) frames: u64,
    pub(super) declared: Option<kithara_decode::DecoderDuration>,
    pub(super) media_origin: Option<Duration>,
    pub(super) gapless: Option<GaplessInfo>,
    /// Encoder priming assumed without gapless metadata, in milliseconds;
    /// `None` keeps the codec's own default.
    pub(super) priming: Option<u64>,
    pub(super) reports_origin: bool,
}

pub(super) struct PhysicalDecoder {
    extent: PcmExtent,
    next_frame: u64,
    spec: AudioSpec,
    pools: Pools,
    original: Option<kithara_decode::PcmOrigin>,
    observing_original: bool,
}

impl PhysicalDecoder {
    pub(super) fn new(extent: PcmExtent, rate: NonZeroU32, pools: Pools) -> Self {
        Self {
            next_frame: extent.origin * u64::from(rate.get()) / 1_000,
            extent,
            spec: AudioSpec::new(2, rate),
            pools,
            original: extent.reports_origin.then(|| {
                let position = Duration::from_millis(extent.origin);
                kithara_decode::PcmOrigin::new(position, position)
            }),
            observing_original: !extent.reports_origin,
        }
    }
    fn end_frame(&self) -> u64 {
        (self.extent.origin + self.extent.frames) * u64::from(self.spec.sample_rate.get()) / 1_000
    }
}

impl Decoder for PhysicalDecoder {
    fn initial_pcm_origin(&self) -> Option<kithara_decode::PcmOrigin> {
        self.original
    }

    delegate::delegate! {
        to self.extent {
            #[field(declared)]
            fn duration(&self) -> Option<kithara_decode::DecoderDuration>;
            #[field]
            fn media_origin(&self) -> Option<Duration>;
        }
    }

    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        self.extent.priming.map_or_else(
            || AudioCodec::encoder_priming_frames(codec),
            |priming| priming * u64::from(self.spec.sample_rate.get()) / 1_000,
        )
    }
    fn track_info(&self) -> kithara_decode::DecoderTrackInfo {
        let mut info = kithara_decode::DecoderTrackInfo::default();
        info.gapless = self.extent.gapless.map(|info| {
            let scale = u64::from(self.spec.sample_rate.get());
            GaplessInfo::new(
                info.leading_frames * scale / 1_000,
                info.trailing_frames * scale / 1_000,
            )
        });
        info
    }
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if self.next_frame >= self.end_frame() {
            return Ok(DecoderChunkOutcome::Eof);
        }
        let start = self.next_frame;
        if self.observing_original {
            let position = self.spec.duration_for(start)?;
            self.original = Some(kithara_decode::PcmOrigin::new(position, position));
            self.observing_original = false;
        }
        let frames = (self.end_frame() - start).min(256);
        self.next_frame += frames;
        let origin = self.extent.origin * u64::from(self.spec.sample_rate.get()) / 1_000;
        let mut samples = Vec::new();
        for frame in start..self.next_frame {
            let value = f32::from(
                u16::try_from((frame - origin) * 1_000 / u64::from(self.spec.sample_rate.get()))
                    .expect("fixture marker fits u16"),
            ) / 10_000.0;
            samples.extend([value; 2]);
        }
        Ok(DecoderChunkOutcome::Chunk(Box::new(AudioChunk::new(
            AudioChunkInfo {
                spec: self.spec,
                frames: u32::try_from(frames).expect("bounded physical fixture"),
                frame_offset: start,
                timestamp: self.spec.duration_for(start)?,
                end_timestamp: self.spec.duration_for(self.next_frame)?,
                ..AudioChunkInfo::default()
            },
            sample_buffer(&self.pools, &samples),
        ))))
    }
    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        if !position.is_zero() {
            self.observing_original = false;
        }
        let origin = self.extent.origin * u64::from(self.spec.sample_rate.get()) / 1_000;
        let frame = self.spec.frame_at(position)?.max(origin);
        self.next_frame = frame.min(self.end_frame());
        if frame >= self.end_frame() {
            return Ok(DecoderSeekOutcome::PastEof {
                duration: self.spec.duration_for(self.end_frame())?,
            });
        }
        Ok(DecoderSeekOutcome::Landed {
            landed_at: self.spec.duration_for(frame)?,
            landed_frame: frame,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
            clock: kithara_decode::LandingClock::Kept,
        })
    }
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn update_byte_len(&self, _len: u64) {}
}

pub(super) async fn physical_source(extent: PcmExtent) -> StreamAudioSource<TestStream> {
    let mode = if extent.gapless.is_some() {
        GaplessMode::MediaOnly
    } else {
        GaplessMode::Disabled
    };
    physical_source_with_mode(extent, mode).await
}

pub(super) async fn physical_source_with_mode(
    extent: PcmExtent,
    mode: GaplessMode,
) -> StreamAudioSource<TestStream> {
    physical_fixture(extent, mode).await.0
}

pub(super) async fn physical_fixture(
    extent: PcmExtent,
    mode: GaplessMode,
) -> (StreamAudioSource<TestStream>, Arc<TestControl>) {
    let fixture = test_source(0).await;
    let rate = NonZeroU32::new(1_000).expect("fixture sample rate");
    let decoder = PhysicalDecoder::new(extent, rate, fixture.pools.clone());
    let profile = decoder.gapless_profile(None);
    let origin = decoder.initial_pcm_origin().and_then(|origin| {
        crate::pipeline::gapless::visible_origin(origin.position(), profile, mode)
    });
    let duration = visible_duration(
        decoder.duration(),
        profile,
        mode,
        origin,
        decoder.media_origin(),
    );
    let decode = ActiveDecode::new(
        DecoderGeneration::new(Box::new(decoder), Some(media_info(0)), 0, None, None, mode),
        mode,
        None,
        &fixture.pools,
    )
    .expect("physical fixture decode scratch");
    let pools = fixture.pools.clone();
    let factory = DecoderFactory::new(
        move |_, _, host_rate| {
            Ok(Box::new(PhysicalDecoder::new(
                extent,
                host_rate.unwrap_or(rate),
                pools.clone(),
            )))
        },
        None,
    );
    let source = StreamAudioSource::new(
        fixture.source.shared_stream.clone(),
        decode,
        SourceDecoderConfig {
            runtime: kithara_platform::tokio::runtime::Handle::try_current()
                .expect("source fixture runtime"),
            factory,
            host_rate: Some(rate),
            backend: kithara_decode::DecoderBackend::default(),
            playback_resampler_backend: "none",
        },
        fixture.source.emit.clone(),
        fixture.source.wake.clone(),
    );
    source.playhead.set_duration(duration);
    (source, fixture.control)
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(super) fn drain(
    source: &mut StreamAudioSource<TestStream>,
    extent: PcmExtent,
) -> Vec<AudioChunkInfo> {
    let mut metadata = Vec::new();
    loop {
        hang_tick!();
        let _ = source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                let meta = chunk.meta;
                assert_eq!(chunk.frames(), meta.frames as usize);
                assert_eq!(chunk.samples.len(), chunk.frames() * 2);
                let origin = extent.origin * u64::from(meta.spec.sample_rate.get()) / 1_000;
                for (index, &frame) in chunk.samples.as_chunks::<2>().0.iter().enumerate() {
                    let at = meta.frame_offset + index as u64;
                    let expected = f32::from(
                        u16::try_from(
                            (at - origin) * 1_000 / u64::from(meta.spec.sample_rate.get()),
                        )
                        .expect("fixture marker fits u16"),
                    ) / 10_000.0;
                    assert_eq!(frame, [expected; 2]);
                }
                if let Some(span) = meta.source_span {
                    assert_eq!(span.output_frames(), u64::from(meta.frames));
                    assert_eq!(span.position_at(0), Some(meta.timestamp));
                    assert_eq!(
                        span.position_at(span.output_frames()),
                        Some(meta.end_timestamp)
                    );
                } else {
                    assert_eq!(
                        meta.timestamp,
                        meta.spec
                            .duration_for(meta.frame_offset)
                            .expect("raw timestamp")
                    );
                }
                metadata.push(meta);
                hang_reset!();
            }
            TrackStep::Eof => break,
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Failed(failure) => panic!("physical source failed: {failure:?}"),
        }
        source.finish_deferred();
    }
    metadata
}

pub(super) fn extent(declared: Option<Duration>, origin: u64, reports_origin: bool) -> PcmExtent {
    PcmExtent {
        origin,
        frames: 1_000,
        declared: declared.map(kithara_decode::DecoderDuration::RawLength),
        media_origin: None,
        gapless: Some(GaplessInfo::new(17, 31)),
        priming: None,
        reports_origin,
    }
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(super) fn first_chunk(source: &mut StreamAudioSource<TestStream>) -> AudioChunk {
    loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                hang_reset!();
                return produced_data(fetch);
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            _ => panic!("original PCM precedes EOF"),
        }
    }
}

pub(super) fn start_physical_promotion(
    source: &mut StreamAudioSource<TestStream>,
    control: &TestControl,
    incoming: PcmExtent,
    landing: Duration,
) {
    let rate = NonZeroU32::new(1_000).expect("millisecond source rate");
    let pools = crate::test_pools::pools();
    source.factory = DecoderFactory::new(
        move |_, _, host_rate| {
            Ok(Box::new(PhysicalDecoder::new(
                incoming,
                host_rate.unwrap_or(rate),
                pools.clone(),
            )))
        },
        None,
    );
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("exact incoming claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket()),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    control.set_exact_plan(VariantReaderPlan::new(transition, media_info(1), landing));
    control.set_promotion(VariantPromotion::Promoted);
    control.set_exact_reader_ready();
}
