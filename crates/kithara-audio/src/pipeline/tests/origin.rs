use std::{collections::VecDeque, num::NonZeroU32};

use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
use kithara_decode::{
    DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo, GaplessMode,
    GaplessProfile, PcmOrigin,
};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{AudioCodec, PrerollHint, VariantTransition, VariantTransitionId};
use kithara_test_utils::kithara;

use super::{
    DecoderGeneration,
    core::ActiveDecode,
    transition::{PreparedPromotion, PromotionJoin, incoming_origin},
};
use crate::test_pools::{Pools, pools, sample_buffer};

struct OriginDecoder {
    spec: AudioSpec,
    initial: Option<PcmOrigin>,
    chunks: VecDeque<AudioChunk>,
    observing_original: bool,
    gap: u64,
    gapless: Option<GaplessInfo>,
    priming: u64,
}

impl OriginDecoder {
    fn new(initial: Option<Duration>, gap: u64) -> Self {
        Self {
            spec: AudioSpec::new(2, NonZeroU32::new(1_000).expect("source rate")),
            initial: initial.map(|position| {
                PcmOrigin::new(
                    position,
                    position.saturating_sub(Duration::from_millis(gap)),
                )
            }),
            chunks: VecDeque::new(),
            observing_original: initial.is_none(),
            gap,
            gapless: None,
            priming: 19,
        }
    }
}

impl Decoder for OriginDecoder {
    fn duration(&self) -> Option<kithara_decode::DecoderDuration> {
        None
    }

    delegate::delegate! {
        to self {
            #[field(initial)]
            fn initial_pcm_origin(&self) -> Option<PcmOrigin>;
            #[field(gap)]
            fn timeline_gap_frames(&self) -> u64;
            #[field]
            fn spec(&self) -> AudioSpec;
        }
    }

    fn gapless_profile(&self, _codec: Option<AudioCodec>) -> GaplessProfile {
        GaplessProfile::new(self.spec, self.gapless, None, self.priming)
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let Some(chunk) = self.chunks.pop_front() else {
            return Ok(DecoderChunkOutcome::Eof);
        };
        if self.observing_original {
            let position = chunk.meta.timestamp;
            self.initial = Some(PcmOrigin::new(
                position,
                position.saturating_sub(self.spec.duration_for(self.gap)?),
            ));
            self.observing_original = false;
        }
        Ok(DecoderChunkOutcome::Chunk(Box::new(chunk)))
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        if !position.is_zero() {
            self.observing_original = false;
        }
        Ok(DecoderSeekOutcome::Landed {
            landed_at: position,
            landed_frame: self.spec.frame_at(position)?,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
            clock: kithara_decode::LandingClock::Kept,
        })
    }

    fn update_byte_len(&self, _len: u64) {}
}

fn generation(decoder: OriginDecoder, mode: GaplessMode) -> DecoderGeneration {
    DecoderGeneration::new(Box::new(decoder), None, 0, None, None, mode)
}

fn chunk(pools: &Pools, spec: AudioSpec, start: u64) -> AudioChunk {
    let frames = 64_u16;
    let samples = (0..frames)
        .flat_map(|frame| [0.25 + f32::from(frame) / 1_000.0; 2])
        .collect::<Vec<_>>();
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset: start,
            frames: u32::from(frames),
            timestamp: spec.duration_for(start).expect("original start"),
            end_timestamp: spec
                .duration_for(start + u64::from(frames))
                .expect("original end"),
            ..Default::default()
        },
        sample_buffer(pools, &samples),
    )
}

#[kithara::test]
#[case::media_only(GaplessMode::MediaOnly, None, 0, 19)]
#[case::codec_priming(GaplessMode::CodecPriming, None, 19, 19)]
#[case::disabled(GaplessMode::Disabled, None, 0, 0)]
#[case::media_metadata(GaplessMode::MediaOnly, Some(GaplessInfo::new(7, 0)), 7, 7)]
fn visible_pcm_origin_and_content_comparison_apply_each_leading_contract_once(
    #[case] mode: GaplessMode,
    #[case] gapless: Option<GaplessInfo>,
    #[case] trim: u64,
    #[case] content_leading: u64,
) {
    let pools = pools();
    let raw = Duration::from_millis(700);
    let mut decoder = OriginDecoder::new(Some(raw), 0);
    decoder.gapless = gapless;
    let spec = decoder.spec;
    let mut generation = generation(decoder, mode);
    let input = chunk(&pools, spec, 700);
    let tail = input.samples[input.samples.len() - 32..].to_vec();
    generation.stage(input).expect("valid gapless PCM");
    let output = generation.pop_staged().expect("original PCM survives trim");

    assert_eq!(output.meta.frame_offset, 700 + trim);
    assert_eq!(u64::from(output.meta.frames), 64 - trim);
    assert_eq!(output.meta.frame_range().end, 764);
    assert_eq!(
        &output.samples[output.samples.len() - 32..],
        tail.as_slice()
    );
    assert_eq!(
        generation.visible_origin(),
        Some(Duration::from_millis(700 + trim))
    );
    assert_eq!(
        generation.comparison_origin(mode),
        Some(700 + content_leading)
    );
}

#[kithara::test]
fn live_decoder_gap_extends_comparison_without_recounting_the_original_pcm_head() {
    let mut generation = generation(
        OriginDecoder::new(Some(Duration::from_millis(59)), 17),
        GaplessMode::MediaOnly,
    );

    assert_eq!(generation.visible_origin(), Some(Duration::from_millis(59)));
    assert_eq!(
        generation.comparison_origin(GaplessMode::MediaOnly),
        Some(78)
    );
    generation.align_timeline_gap(31);
    assert_eq!(
        generation.comparison_origin(GaplessMode::MediaOnly),
        Some(92)
    );
    assert_eq!(generation.visible_origin(), Some(Duration::from_millis(59)));
}

#[kithara::test]
fn first_original_pcm_keeps_its_comparison_basis_past_a_sought_suffix() {
    let pools = pools();
    let mut decoder = OriginDecoder::new(None, 0);
    let spec = decoder.spec;
    decoder.chunks.push_back(chunk(&pools, spec, 42));
    let mut generation = generation(decoder, GaplessMode::MediaOnly);
    let DecoderChunkOutcome::Chunk(input) = generation.next_chunk().expect("original decode")
    else {
        panic!("original decoder PCM");
    };
    generation.stage(*input).expect("valid gapless PCM");
    let original = generation.pop_staged().expect("original PCM");

    assert_eq!(original.meta.timestamp, Duration::from_millis(42));
    assert_eq!(generation.visible_origin(), Some(Duration::from_millis(42)));
    assert_eq!(
        generation.comparison_origin(GaplessMode::MediaOnly),
        Some(61)
    );
    generation.notify_seek();
    generation
        .seek(Duration::from_millis(500), GaplessMode::MediaOnly)
        .expect("sought suffix");
    generation
        .stage(chunk(&pools, spec, 500))
        .expect("valid gapless PCM");
    let suffix = generation.pop_staged().expect("sought PCM");
    assert_eq!(suffix.meta.frame_offset, 500);
    assert_eq!(suffix.meta.frames, 64);
    assert_eq!(generation.visible_origin(), Some(Duration::from_millis(42)));
    assert_eq!(
        generation.comparison_origin(GaplessMode::MediaOnly),
        Some(61)
    );
}

#[kithara::test]
fn observed_incoming_origin_keeps_the_shared_gap_after_promotion() {
    let pools = pools();
    let mut active = generation(
        OriginDecoder::new(Some(Duration::ZERO), 0),
        GaplessMode::MediaOnly,
    );
    active.align_timeline_gap(17);
    let mut incoming = generation(
        OriginDecoder::new(Some(Duration::ZERO), 0),
        GaplessMode::MediaOnly,
    );
    incoming.align_timeline_gap(3);
    assert_eq!(active.comparison_origin(GaplessMode::MediaOnly), Some(36));
    assert_eq!(incoming.comparison_origin(GaplessMode::MediaOnly), Some(22));
    assert_eq!(
        incoming_origin(&active, &incoming, GaplessMode::MediaOnly),
        Some(36)
    );

    let rate = active.spec().sample_rate;
    let mut decode = ActiveDecode::new(active, GaplessMode::MediaOnly, None, &pools)
        .expect("generation owner scratch");
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming generation claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket()),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    drop(decode.commit_prepared_promotion(PreparedPromotion {
        preparation:
            crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(
            ),
        generation: incoming,
        transition,
        join: PromotionJoin::HardCut,
        resume_head: crate::SourceEnd::new(64, rate),
    }));
    assert_eq!(decode.active().timeline_gap(), 17);
    assert_eq!(decode.active().visible_origin(), Some(Duration::ZERO));
    assert_eq!(
        decode.active().comparison_origin(GaplessMode::MediaOnly),
        Some(36)
    );
}
