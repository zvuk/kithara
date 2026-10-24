#[cfg(test)]
use std::cell::Cell;
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, GaplessMode, GaplessProfile, PcmOrigin,
};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{ConstructionGate, MediaInfo};
use kithara_test_utils::kithara;
use tracing::warn;

use crate::pipeline::{
    decode::core::panic_message,
    gapless::GaplessStage,
    seek::{ResumeState, ResumeTarget},
};

#[path = "seek.rs"]
mod seek;

#[derive(Clone, Copy)]
pub(super) struct Holdback {
    pub(super) spec: AudioSpec,
    pub(super) join_frames: u64,
    pub(super) slots: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum StageProgress {
    Ready,
    NeedMore,
}

pub(crate) struct StageFailure {
    pub(crate) chunk: AudioChunk,
    pub(crate) error: DecodeError,
}

pub(crate) enum StageResult {
    Ready,
    NeedMore,
    Invalid(Box<StageFailure>),
}

pub(crate) enum StageOutput {
    Output(Option<AudioChunk>),
    Invalid(StageFailure),
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct DecoderGeneration {
    decoder: Box<dyn Decoder>,
    construction_gate: Option<ConstructionGate>,
    #[field(get, vis = "pub(crate)", copy)]
    gapless_profile: GaplessProfile,
    pub(super) gapless: GaplessStage,
    #[field(get = is_finished, vis = "pub(crate)", copy)]
    finished: bool,
    /// The decoder consumed its source to the end while a variant transition
    /// was in flight. Unlike `finished`, this keeps holdback and the staged
    /// tail intact so the promotion proof stays reachable; no further PCM can
    /// ever come from this generation.
    #[field(get = is_source_exhausted, vis = "pub(crate)", copy)]
    source_exhausted: bool,
    /// The transition driver ran a pass after `source_exhausted` was set, so
    /// a pending switch intent had its chance to plant an incoming slot
    /// before EOF finalizes.
    #[field(get = is_exhaustion_observed, vis = "pub(crate)", copy)]
    exhaustion_observed: bool,
    pub(super) holdback: Option<Holdback>,
    /// Lower bound for the decoder's observed timeline gap after an exact
    /// variant splice. The overlap proof normalizes equal codec profiles to
    /// the larger gap; promotion transfers that normalization here so a later
    /// transition cannot reinterpret the new active generation on a smaller
    /// origin.
    timeline_gap_floor: u64,
    #[field(get, vis = "pub(crate)", copy)]
    native_pcm_basis: Option<AudioChunkInfo>,
    media_origin: Option<Duration>,
    visible_origin: Option<Duration>,
    comparison_base: Option<Duration>,
    observing_visible_origin: bool,
    media_info: Option<MediaInfo>,
    pending_head_skip: Option<ResumeState>,
    pub(super) staged: VecDeque<AudioChunk>,
    #[cfg(test)]
    staged_scan_count: Cell<usize>,
    #[field(get, vis = "pub(crate)")]
    base_offset: u64,
}

impl DecoderGeneration {
    pub(crate) fn observe_native_pcm(&mut self, info: AudioChunkInfo) -> AudioChunkInfo {
        let basis = self.native_pcm_basis.get_or_insert(info);
        if basis.spec != info.spec || basis.source_revision != info.source_revision {
            *basis = info;
        }
        *basis
    }

    pub(crate) fn visible_duration(&self, mode: GaplessMode) -> Option<Duration> {
        crate::pipeline::gapless::visible_duration(
            self.decoder.duration(),
            self.gapless_profile,
            mode,
            self.visible_origin,
            self.media_origin,
        )
    }

    pub(crate) fn media_end(
        &self,
        mode: GaplessMode,
        physical: Option<Duration>,
        decoded_end: Option<Duration>,
    ) -> Option<Duration> {
        let declared = self.decoder.duration();
        let content_shift = self.undeclared_priming(mode)?;
        crate::pipeline::gapless::media_end(
            declared,
            self.gapless_profile,
            mode,
            physical,
            self.visible_origin,
            self.media_origin,
            decoded_end,
        )
        .map(|end| end.saturating_sub(content_shift))
    }

    pub(crate) fn new(
        decoder: Box<dyn Decoder>,
        media_info: Option<MediaInfo>,
        base_offset: u64,
        construction_gate: Option<ConstructionGate>,
        pending_head_skip: Option<ResumeState>,
        gapless_mode: GaplessMode,
    ) -> Self {
        let codec = media_info.as_ref().and_then(|info| info.codec);
        let gapless_profile = decoder.gapless_profile(codec);
        let gapless = GaplessStage::build(gapless_profile, gapless_mode, codec);
        let media_origin = decoder.media_origin();
        let initial_origin = decoder.initial_pcm_origin();
        let initial_position = initial_origin.map(PcmOrigin::position);
        let comparison_base = initial_origin.map(PcmOrigin::comparison_base);
        let visible_origin = initial_position.and_then(|position| {
            crate::pipeline::gapless::visible_origin(position, gapless_profile, gapless_mode)
        });
        tracing::debug!(
            declared_duration = ?decoder.duration(),
            ?gapless_profile,
            ?gapless_mode,
            ?media_origin,
            ?initial_origin,
            ?visible_origin,
            "decoder generation source geometry",
        );
        Self {
            decoder,
            media_info,
            base_offset,
            construction_gate,
            gapless_profile,
            gapless,
            pending_head_skip,
            finished: false,
            source_exhausted: false,
            exhaustion_observed: false,
            holdback: None,
            timeline_gap_floor: 0,
            native_pcm_basis: None,
            media_origin,
            visible_origin,
            comparison_base,
            observing_visible_origin: true,
            staged: VecDeque::new(),
            #[cfg(test)]
            staged_scan_count: Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(super) fn record_staged_scan(&self) {
        self.staged_scan_count
            .set(self.staged_scan_count.get().saturating_add(1));
    }

    pub(crate) fn align_timeline_gap(&mut self, gap: u64) {
        self.timeline_gap_floor = self.timeline_gap_floor.max(gap);
    }

    pub(super) fn observe_comparison_base(&mut self) {
        if self.observing_visible_origin && self.comparison_base.is_none() {
            self.comparison_base = self
                .decoder
                .initial_pcm_origin()
                .map(PcmOrigin::comparison_base);
        }
    }

    pub(crate) fn observe_visible_origin(&mut self, chunk: &AudioChunk) {
        if self.observing_visible_origin && chunk.meta.frames > 0 {
            if self.visible_origin.is_none() {
                self.visible_origin = chunk
                    .meta
                    .source_span
                    .map_or(Some(chunk.meta.timestamp), |span| span.position_at(0));
            }
            self.observing_visible_origin = false;
        }
    }

    pub(crate) fn seal_visible_origin(&mut self, mode: GaplessMode) {
        if self.observing_visible_origin {
            let initial_origin = self.decoder.initial_pcm_origin();
            let initial_position = initial_origin.map(PcmOrigin::position);
            if self.comparison_base.is_none() {
                self.comparison_base = initial_origin.map(PcmOrigin::comparison_base);
            }
            self.visible_origin = initial_position.and_then(|position| {
                crate::pipeline::gapless::visible_origin(position, self.gapless_profile, mode)
            });
            self.observing_visible_origin = false;
        }
    }

    pub(crate) fn retain_visible_origin(
        &mut self,
        origin: Option<Duration>,
        comparison_base: Option<Duration>,
    ) {
        self.visible_origin = origin;
        self.comparison_base = comparison_base;
        self.observing_visible_origin = false;
    }

    pub(crate) fn retain_media_origin(&mut self, origin: Option<Duration>) {
        self.media_origin = origin;
    }

    pub(crate) fn comparison_origin(&self, mode: GaplessMode) -> Option<u64> {
        self.timeline_origin(mode)
            .checked_add(self.comparison_base_frames()?)
    }

    /// Raw PCM coordinate of content zero: media zero moved past the priming
    /// it leaves undeclared, so every variant of one stream reports one
    /// content time.
    pub(crate) fn content_origin(&self, mode: GaplessMode) -> Option<Duration> {
        self.media_origin?
            .checked_add(self.undeclared_priming(mode)?)
    }

    /// Leading frames past media zero that only the timeline origin a variant
    /// switch compares accounts for. A container that declares a nonzero
    /// media zero covers its leading frames there; one that declares none
    /// leaves the timeline origin to cover its priming.
    fn undeclared_priming(&self, mode: GaplessMode) -> Option<Duration> {
        match self.media_origin {
            Some(media) if !media.is_zero() => Some(Duration::ZERO),
            _ => self.timeline_origin_duration(mode),
        }
    }

    fn timeline_origin_duration(&self, mode: GaplessMode) -> Option<Duration> {
        self.decoder
            .spec()
            .duration_for(self.timeline_origin(mode))
            .ok()
    }

    pub(crate) fn comparison_origin_with_gap(&self, mode: GaplessMode, gap: u64) -> Option<u64> {
        self.timeline_origin_with_gap(mode, gap)
            .checked_add(self.comparison_base_frames()?)
    }

    fn comparison_base_frames(&self) -> Option<u64> {
        let spec = self.spec();
        if self
            .visible_origin
            .is_some_and(|origin| spec.frame_at(origin).is_err())
        {
            return None;
        }
        self.comparison_base
            .map_or(Some(0), |position| spec.frame_at(position).ok())
    }

    pub(crate) fn has_same_source_basis(&self, other: &Self) -> bool {
        let retained_source = |generation: &Self| {
            generation
                .pending_head_skip
                .as_ref()
                .and_then(|resume| match resume.target {
                    ResumeTarget::Source(end) => Some((end.sample_rate(), end.mapping_revision())),
                    ResumeTarget::Position(_) | ResumeTarget::Render(_) => None,
                })
        };
        self.spec().sample_rate == other.spec().sample_rate
            && retained_source(self) == retained_source(other)
    }

    pub(crate) fn finish(&mut self) -> DecodeResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.holdback = None;
        let codec = self.media_info.as_ref().and_then(|info| info.codec);
        self.gapless
            .set_tail_compensation(self.decoder.gapless_profile(codec), codec);
        self.gapless.flush()
    }

    pub(crate) const fn mark_source_exhausted(&mut self) {
        self.source_exhausted = true;
    }

    pub(crate) const fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }

    pub(crate) fn next(&mut self) -> Option<AudioChunk> {
        self.holdback = None;
        self.staged.pop_front().or_else(|| self.gapless.next())
    }

    #[kithara::measure(label = "audio.decoder.next")]
    pub(crate) fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if self.finished {
            return Ok(DecoderChunkOutcome::Eof);
        }
        match catch_unwind(AssertUnwindSafe(|| self.decoder.next_chunk_prepared())) {
            Ok(result) => result,
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during next_chunk");
                Err(DecodeError::InvalidData {
                    detail: "decoder panicked during next_chunk",
                })
            }
        }
    }

    pub(crate) fn prepare_incoming(
        &mut self,
        target: super::transition_promotion::preparation::IncomingPreparation,
    ) -> DecodeResult<super::transition::IncomingPrime> {
        if let Some(gate) = &self.construction_gate {
            gate.arm();
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| target.prepare(self)));
        if let Some(gate) = &self.construction_gate {
            gate.disarm();
        }
        match outcome {
            Ok(outcome) => outcome,
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during incoming preparation");
                Err(DecodeError::InvalidData {
                    detail: "decoder panicked during incoming preparation",
                })
            }
        }
    }

    pub(crate) const fn observe_exhaustion(&mut self) {
        self.exhaustion_observed = true;
    }

    pub(crate) fn push(&mut self, chunk: AudioChunk) -> DecodeResult<()> {
        self.holdback = None;
        self.observe_comparison_base();
        self.gapless.push(chunk)
    }

    pub(crate) fn timeline_gap(&self) -> u64 {
        self.decoder
            .timeline_gap_frames()
            .max(self.timeline_gap_floor)
    }

    pub(crate) fn timeline_origin(&self, mode: GaplessMode) -> u64 {
        self.timeline_origin_with_gap(mode, self.timeline_gap())
    }

    pub(crate) fn timeline_origin_with_gap(&self, mode: GaplessMode, gap: u64) -> u64 {
        let leading = if matches!(mode, GaplessMode::Disabled) {
            0
        } else {
            self.gapless_profile.gapless().map_or_else(
                || self.gapless_profile.default_priming_frames(),
                |info| info.leading_frames,
            )
        };
        leading.saturating_add(gap)
    }

    delegate::delegate! {
        to self {
            #[expr(self.media_origin)]
            pub(crate) const fn media_origin(&self) -> Option<Duration>;
            #[expr(self.visible_origin)]
            pub(crate) const fn visible_origin(&self) -> Option<Duration>;
            #[expr(self.comparison_base)]
            pub(crate) const fn comparison_base(&self) -> Option<Duration>;
        }
        to self.decoder {
            pub(crate) fn spec(&self) -> AudioSpec;
            #[call(as_ref)]
            pub(crate) fn decoder(&self) -> &dyn Decoder;
            #[call(as_mut)]
            pub(crate) fn decoder_mut(&mut self) -> &mut dyn Decoder;
        }
        to self.staged {
            #[cfg(test)]
            #[call(capacity)]
            pub(crate) fn staged_capacity(&self) -> usize;
        }
    }
}

pub(super) fn chunk_range(chunk: &AudioChunk, spec: AudioSpec) -> Option<(u64, u64)> {
    let channels = usize::from(spec.channels);
    if channels == 0 || chunk.spec() != spec || !chunk.samples.len().is_multiple_of(channels) {
        return None;
    }
    let frames = chunk.samples.len() / channels;
    if frames == 0 || u32::try_from(frames).ok()? != chunk.meta.frames {
        return None;
    }
    Some((
        chunk.meta.frame_offset,
        chunk
            .meta
            .frame_offset
            .checked_add(u64::from(chunk.meta.frames))?,
    ))
}

pub(super) fn stage_failure(chunk: AudioChunk, detail: &'static str) -> StageFailure {
    StageFailure {
        chunk,
        error: DecodeError::InvalidData { detail },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        num::NonZeroU32,
        sync::atomic::{AtomicU64, Ordering},
    };

    use kithara_decode::{
        DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo,
        GaplessMode, GaplessProfile, GaplessTailCompensation, PcmOrigin,
    };
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
    use kithara_stream::{MediaInfo, PrerollHint};
    use kithara_test_fixtures::unit_fixtures::decode_quarter;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{Pools, pools, sample_buffer};

    struct EofDecoder {
        spec: AudioSpec,
        profile_calls: Cell<u32>,
        gapless: Option<GaplessInfo>,
        late_tail_frames: Option<u64>,
        default_priming: u64,
    }

    impl Decoder for EofDecoder {
        fn duration(&self) -> Option<kithara_decode::DecoderDuration> {
            None
        }

        fn gapless_profile(&self, _codec: Option<kithara_stream::AudioCodec>) -> GaplessProfile {
            let call = self.profile_calls.get();
            self.profile_calls.set(call.saturating_add(1));
            let tail = (call > 0)
                .then_some(self.late_tail_frames)
                .flatten()
                .map(GaplessTailCompensation::new);
            GaplessProfile::new(self.spec, self.gapless, tail, self.default_priming)
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(DecoderChunkOutcome::Eof)
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(DecoderSeekOutcome::Landed {
                landed_at: position,
                landed_frame: self.spec.frame_at(position).expect("valid seek frame"),
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

    fn spec(channels: u16, rate: u32) -> AudioSpec {
        AudioSpec::new(
            channels,
            NonZeroU32::new(rate).expect("test rate must be non-zero"),
        )
    }

    fn generation(spec: AudioSpec) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(EofDecoder {
                spec,
                gapless: None,
                default_priming: 0,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            None,
            None,
            GaplessMode::Disabled,
        )
    }

    fn media_generation(spec: AudioSpec, trailing_frames: u64) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(EofDecoder {
                spec,
                gapless: Some(GaplessInfo::new(0, trailing_frames)),
                default_priming: 0,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            None,
            None,
            GaplessMode::MediaOnly,
        )
    }

    /// A track whose container carries no gapless metadata while the codec
    /// table offers an estimate — the AAC-LC case at a quality-switch seam.
    fn priming_generation(spec: AudioSpec, mode: GaplessMode) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(EofDecoder {
                spec,
                gapless: None,
                default_priming: 1_024,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            None,
            None,
            mode,
        )
    }

    fn chunk(
        pools: &Pools,
        pcm: &[f32],
        spec: AudioSpec,
        offset: u64,
        frames: u32,
        sample_frames: usize,
    ) -> AudioChunk {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frame_offset: offset,
                frames,
                ..Default::default()
            },
            sample_buffer(pools, &pcm[..sample_frames * usize::from(spec.channels)]),
        )
    }

    #[kithara::test]
    #[case(None, 4, "an unnamed codec keeps the compensated trim")]
    #[case(
        Some(kithara_stream::AudioCodec::Flac),
        4,
        "FLAC carries the frame exactly, so the compensation applies"
    )]
    #[case(
        Some(kithara_stream::AudioCodec::AacLc),
        3,
        "AAC would buy back its tapered frame, so the trim stays whole"
    )]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn finish_installs_the_tail_compensation_the_decoder_resolved_while_decoding(
        #[case] codec: Option<kithara_stream::AudioCodec>,
        #[case] expected_frames: u64,
        #[case] label: &str,
        decode_quarter: Vec<f32>,
    ) {
        let pools = pools();
        let pushed_frames = 5_u64;
        let trailing_frames = 2_u64;
        let spec = spec(2, 44_100);
        let media_info = codec.map(|codec| MediaInfo::builder().codec(codec).build());
        let mut generation = DecoderGeneration::new(
            Box::new(EofDecoder {
                spec,
                gapless: Some(GaplessInfo::new(0, trailing_frames)),
                default_priming: 0,
                late_tail_frames: Some(pushed_frames + 1),
                profile_calls: Cell::new(0),
            }),
            media_info,
            0,
            None,
            None,
            GaplessMode::MediaOnly,
        );

        for frame in 0..pushed_frames {
            generation
                .stage(chunk(&pools, &decode_quarter, spec, frame, 1, 1))
                .expect("valid gapless PCM");
        }
        generation.finish_staging().expect("valid gapless PCM");

        let mut frames = 0u64;
        while let Some(next) = generation.next() {
            hang_tick!();
            frames = frames.saturating_add(u64::from(next.meta.frames));
            hang_reset!();
        }

        assert_eq!(frames, expected_frames, "{label}");
    }

    /// An unlabelled AAC track keeps its encoder priming in the PCM (the
    /// `MediaOnly` trimmer removes nothing), yet those frames are not content:
    /// the seam origin counts them, and the four `quality_switch_continuity`
    /// integration tests fail by exactly this many frames if it stops.
    #[kithara::test]
    fn origin_counts_codec_priming_the_media_only_trimmer_leaves_in_place() {
        let generation = priming_generation(spec(2, 44_100), GaplessMode::MediaOnly);

        assert_eq!(generation.timeline_origin(GaplessMode::MediaOnly), 1_024);
    }

    #[kithara::test]
    fn seek_to_start_preserves_codec_priming_trim() {
        let pools = pools();
        let spec = spec(2, 44_100);
        let mut generation = priming_generation(spec, GaplessMode::CodecPriming);
        generation.notify_seek();
        let _ = generation
            .seek(Duration::ZERO, GaplessMode::CodecPriming)
            .expect("seek to start");
        generation
            .push(chunk(&pools, &vec![0.25; 4_096], spec, 0, 2_048, 2_048))
            .expect("valid gapless PCM");
        let output = generation.next().expect("output after priming trim");
        assert_eq!(output.meta.frame_offset, 1_024);
        assert_eq!(output.frames(), 1_024);
    }

    #[kithara::test]
    fn seek_into_content_does_not_reapply_codec_priming_trim() {
        let pools = pools();
        let spec = spec(2, 44_100);
        let mut generation = priming_generation(spec, GaplessMode::CodecPriming);
        generation.notify_seek();
        let _ = generation
            .seek(Duration::from_secs(1), GaplessMode::CodecPriming)
            .expect("seek into content");
        generation
            .push(chunk(
                &pools,
                &vec![0.25; 4_096],
                spec,
                44_100,
                2_048,
                2_048,
            ))
            .expect("valid gapless PCM");
        let output = generation.next().expect("output without priming trim");
        assert_eq!(output.meta.frame_offset, 44_100);
        assert_eq!(output.frames(), 2_048);
    }

    #[kithara::test]
    fn staged_holdback_rejects_gap_mixed_spec_and_bad_metadata(decode_quarter: Vec<f32>) {
        let pools = pools();
        let active = spec(2, 44_100);

        let mut gap = generation(active);
        assert!(matches!(
            gap.prepare_holdback(active, 4),
            StageResult::NeedMore
        ));
        assert!(matches!(
            gap.push_holdback(chunk(&pools, &decode_quarter, active, 0, 4, 4), None)
                .expect("valid gapless PCM"),
            StageResult::NeedMore
        ));
        let StageResult::Invalid(gap_failure) = gap
            .push_holdback(chunk(&pools, &decode_quarter, active, 5, 4, 4), None)
            .expect("valid gapless PCM")
        else {
            panic!("a discontinuity must return its offending chunk");
        };
        assert_eq!(gap_failure.chunk.meta.frame_offset, 5);
        assert!(matches!(gap_failure.error, DecodeError::InvalidData { .. }));
        assert_eq!(
            gap.staged.len(),
            1,
            "a discontinuity must not enter holdback"
        );

        let mut mixed = generation(active);
        let _ = mixed.prepare_holdback(active, 4);
        let _ = mixed
            .push_holdback(chunk(&pools, &decode_quarter, active, 0, 4, 4), None)
            .expect("valid gapless PCM");
        let StageResult::Invalid(mixed_failure) = mixed
            .push_holdback(
                chunk(&pools, &decode_quarter, spec(2, 48_000), 4, 4, 4),
                None,
            )
            .expect("valid gapless PCM")
        else {
            panic!("mixed PCM must return its offending chunk");
        };
        assert_eq!(mixed_failure.chunk.spec().sample_rate.get(), 48_000);
        assert_eq!(mixed.staged.len(), 1, "mixed PCM must not enter holdback");

        let mut bad_meta = generation(active);
        let _ = bad_meta.prepare_holdback(active, 4);
        let StageResult::Invalid(meta_failure) = bad_meta
            .push_holdback(chunk(&pools, &decode_quarter, active, 0, 3, 4), None)
            .expect("valid gapless PCM")
        else {
            panic!("bad metadata must return its offending chunk");
        };
        assert_eq!(meta_failure.chunk.meta.frames, 3);
        assert!(bad_meta.staged.is_empty(), "bad metadata must be rejected");
    }

    #[kithara::test]
    fn staged_holdback_never_grows_past_prepared_capacity(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = spec(1, 44_100);
        let mut generation = generation(spec);
        let _ = generation.prepare_holdback(spec, 3);
        let capacity = generation.staged.capacity();
        for frame in 0..capacity {
            let result = generation
                .push_holdback(
                    chunk(
                        &pools,
                        &decode_quarter,
                        spec,
                        u64::try_from(frame).unwrap_or(u64::MAX),
                        1,
                        1,
                    ),
                    None,
                )
                .expect("valid gapless PCM");
            if frame + 1 == capacity {
                assert!(matches!(result, StageResult::Ready));
            }
        }
        let StageResult::Invalid(failure) = generation
            .push_holdback(
                chunk(
                    &pools,
                    &decode_quarter,
                    spec,
                    u64::try_from(capacity).unwrap_or(u64::MAX),
                    1,
                    1,
                ),
                None,
            )
            .expect("valid gapless PCM")
        else {
            panic!("full holdback must return the excess chunk");
        };

        assert_eq!(generation.staged.capacity(), capacity);
        assert_eq!(generation.staged.len(), capacity);
        assert_eq!(
            failure.chunk.meta.frame_offset,
            u64::try_from(capacity).unwrap_or(u64::MAX)
        );

        const CUT: u64 = 1_024;
        const JOIN: u64 = 3;
        let mut held = self::generation(spec);
        let _ = held.prepare_holdback(spec, JOIN);
        let capacity = held.staged.capacity();
        let marker =
            |frame| f32::from(u16::try_from(frame).expect("fixture marker frame")) / 2_048.0;
        for frame in 0..CUT + JOIN {
            let result = held
                .push_holdback(
                    chunk(&pools, &[marker(frame)], spec, frame, 1, 1),
                    Some(CUT),
                )
                .expect("valid PCM advances the exact cut window");
            assert_eq!(held.staged.capacity(), capacity);
            assert!(held.staged.len() <= usize::try_from(JOIN + 1).expect("window frame count"));
            if frame + 1 == CUT + JOIN {
                assert!(matches!(result, StageResult::Ready));
            } else {
                assert!(matches!(result, StageResult::NeedMore));
            }
        }
        assert!(held.staged_covers(spec, CUT, JOIN));
        for frame in CUT - 1..CUT + JOIN {
            let retained = held
                .next()
                .expect("cancelled holdback keeps its predecessor and join");
            assert_eq!(retained.meta.frame_offset, frame);
            assert_eq!(retained.samples.as_ref(), &[marker(frame)]);
        }
        assert!(held.next().is_none());
    }

    #[kithara::test]
    fn prepare_holdback_reserves_from_staged_len_to_logical_limit(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = spec(1, 44_100);
        let mut generation = generation(spec);
        let _ = generation.prepare_holdback(spec, 1);
        let _ = generation
            .push_holdback(chunk(&pools, &decode_quarter, spec, 0, 1, 1), None)
            .expect("valid gapless PCM");
        let old_capacity = generation.staged.capacity();
        let slots = old_capacity.checked_add(1).expect("test slot limit");
        let join_frames = u64::try_from(slots - 1).expect("test join frame count");

        assert!(!generation.staged.is_empty());
        assert!(generation.staged.len() < old_capacity);
        assert!(old_capacity < slots);

        let _ = generation.prepare_holdback(spec, join_frames);
        let reserved_capacity = generation.staged.capacity();
        assert!(
            reserved_capacity >= slots,
            "prepared capacity {reserved_capacity} must cover all {slots} logical slots"
        );

        for frame in generation.staged.len()..slots {
            let result = generation
                .push_holdback(
                    chunk(
                        &pools,
                        &decode_quarter,
                        spec,
                        u64::try_from(frame).expect("test frame offset"),
                        1,
                        1,
                    ),
                    None,
                )
                .expect("valid gapless PCM");
            if frame + 1 == slots {
                assert!(matches!(result, StageResult::Ready));
            }
            assert_eq!(
                generation.staged.capacity(),
                reserved_capacity,
                "filling the prepared logical limit must not grow on the checked core"
            );
        }
    }

    #[kithara::test]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn holdback_pumps_surplus_gapless_pending_without_decoder_input(decode_quarter: Vec<f32>) {
        const TRAILING_FRAMES: u64 = 10;
        const JOIN_FRAMES: u64 = 2;

        let pools = pools();
        let spec = spec(1, 44_100);
        let mut generation = media_generation(spec, TRAILING_FRAMES);
        assert!(matches!(
            generation.prepare_holdback(spec, JOIN_FRAMES),
            StageResult::NeedMore
        ));
        let capacity = generation.staged.capacity();

        for frame in 0..TRAILING_FRAMES {
            assert!(matches!(
                generation
                    .push_holdback(chunk(&pools, &decode_quarter, spec, frame, 1, 1), None)
                    .expect("valid gapless PCM"),
                StageResult::NeedMore
            ));
        }
        let release = generation
            .push_holdback(
                chunk(
                    &pools,
                    &decode_quarter,
                    spec,
                    TRAILING_FRAMES,
                    u32::try_from(TRAILING_FRAMES).expect("test release frame count"),
                    usize::try_from(TRAILING_FRAMES).expect("test release sample frames"),
                ),
                None,
            )
            .expect("valid gapless PCM");
        assert!(
            matches!(release, StageResult::Ready),
            "the first three valid released chunks make holdback ready; surplus stays pending"
        );
        assert_eq!(generation.staged.capacity(), capacity);

        let mut offsets = Vec::new();
        loop {
            hang_tick!();
            let next = match generation.next_with_holdback() {
                StageOutput::Output(next) => next,
                StageOutput::Invalid(failure) => {
                    panic!("valid pending PCM failed: {}", failure.error);
                }
            };
            let Some(chunk) = next else {
                break;
            };
            offsets.push(chunk.meta.frame_offset);
            hang_reset!();
            assert_eq!(generation.staged.capacity(), capacity);
        }

        assert_eq!(offsets, (0..8).collect::<Vec<_>>());
        assert_eq!(generation.staged.capacity(), capacity);
    }

    #[kithara::test]
    fn checked_holdback_scans_the_preexisting_span_once(decode_quarter: Vec<f32>) {
        const JOIN_FRAMES: u64 = 7_680;

        let pools = pools();
        let spec = spec(1, 384_000);
        let mut generation = generation(spec);
        assert!(matches!(
            generation.prepare_holdback(spec, JOIN_FRAMES),
            StageResult::NeedMore
        ));
        let capacity = generation.staged.capacity();

        for frame in 0..=JOIN_FRAMES {
            let result = generation
                .push_holdback(chunk(&pools, &decode_quarter, spec, frame, 1, 1), None)
                .expect("valid gapless PCM");
            if frame == JOIN_FRAMES {
                assert!(matches!(result, StageResult::Ready));
            } else {
                assert!(matches!(result, StageResult::NeedMore));
            }
            assert_eq!(generation.staged.capacity(), capacity);
        }
        assert_eq!(
            generation.staged_scan_count.get(),
            1,
            "the shell validates retained PCM once; checked pushes stay O(1)"
        );

        for frame in 0..4 {
            let next = match generation.next_with_holdback() {
                StageOutput::Output(Some(chunk)) => chunk,
                StageOutput::Output(None) => panic!("ready holdback must release its front chunk"),
                StageOutput::Invalid(failure) => {
                    panic!("valid holdback failed: {}", failure.error)
                }
            };
            assert_eq!(next.meta.frame_offset, frame);
            assert!(matches!(
                generation
                    .push_holdback(
                        chunk(&pools, &decode_quarter, spec, JOIN_FRAMES + frame + 1, 1, 1,),
                        None
                    )
                    .expect("valid gapless PCM"),
                StageResult::Ready
            ));
            assert_eq!(generation.staged.capacity(), capacity);
        }
        assert_eq!(
            generation.staged_scan_count.get(),
            1,
            "ready pops and contiguous pushes must use the validated frontier cache"
        );
    }

    #[kithara::test]
    fn deferred_seek_only_completes_the_current_epoch() {
        let mut generation = generation(spec(2, 44_100));
        let first = Duration::from_secs(1);
        let second = Duration::from_secs(2);
        generation.notify_seek();
        assert!(matches!(
            generation.seek(first, GaplessMode::MediaOnly),
            Ok(DecoderSeekOutcome::Landed { landed_at, .. }) if landed_at == first
        ));
        assert!(!generation.has_output());
        let pools = pools();
        generation
            .stage(AudioChunk::new(
                AudioChunkInfo::default(),
                sample_buffer(&pools, &[0.25, 0.25]),
            ))
            .expect("valid gapless PCM");
        assert!(generation.has_output());
        generation.notify_seek();
        assert!(matches!(
            generation.seek(second, GaplessMode::MediaOnly),
            Ok(DecoderSeekOutcome::Landed { landed_at, .. }) if landed_at == second
        ));
        assert!(!generation.has_output());
        generation
            .prepare_deferred(false, GaplessMode::MediaOnly)
            .expect("gapless cleanup does not continue input");
        assert!(!generation.has_output());
        assert!(!generation.is_finished());
        assert!(matches!(
            generation.next_chunk(),
            Ok(DecoderChunkOutcome::Eof)
        ));
        assert!(!generation.has_output());
    }

    #[kithara::test]
    fn seek_reopens_a_finished_generation() {
        let spec = spec(2, 44_100);
        let mut generation = generation(spec);

        generation.finish().expect("valid gapless PCM");
        generation.finish().expect("valid gapless PCM");
        assert!(generation.is_finished());

        generation.notify_seek();

        assert!(!generation.is_finished());
    }
    struct HeadSeekDecoder {
        spec: AudioSpec,
        origin: Option<Duration>,
        landed_at: Duration,
        landed_frame: u64,
        defer_seek: bool,
        pending_seek: bool,
        seek_preparations: Arc<AtomicU64>,
        pcm_preparations: Arc<AtomicU64>,
    }

    impl Decoder for HeadSeekDecoder {
        fn initial_pcm_origin(&self) -> Option<PcmOrigin> {
            self.origin
                .map(|position| PcmOrigin::new(position, position))
        }

        fn duration(&self) -> Option<kithara_decode::DecoderDuration> {
            None
        }

        fn gapless_profile(&self, _codec: Option<kithara_stream::AudioCodec>) -> GaplessProfile {
            GaplessProfile::new(self.spec, None, None, 1_024)
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            assert!(self.pcm_preparations.load(Ordering::Relaxed) > 0);
            Ok(DecoderChunkOutcome::Eof)
        }

        fn seek(&mut self, _position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            if self.defer_seek {
                self.pending_seek = true;
                return Ok(DecoderSeekOutcome::Pending(
                    kithara_stream::PendingReason::NotReady(
                        kithara_stream::NotReadyCause::SourcePending,
                    ),
                ));
            }
            Ok(DecoderSeekOutcome::Landed {
                landed_at: self.landed_at,
                landed_frame: self.landed_frame,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
                clock: kithara_decode::LandingClock::Kept,
            })
        }

        fn prepare_seek(&mut self) -> DecodeResult<Option<DecoderSeekOutcome>> {
            if !std::mem::take(&mut self.pending_seek) {
                return Ok(None);
            }
            self.seek_preparations.fetch_add(1, Ordering::Relaxed);
            Ok(Some(DecoderSeekOutcome::Landed {
                landed_at: self.landed_at,
                landed_frame: self.landed_frame,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
                clock: kithara_decode::LandingClock::Kept,
            }))
        }

        fn prepare_next_chunk(&mut self) {
            self.pcm_preparations.fetch_add(1, Ordering::Relaxed);
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    #[kithara::test]
    fn deferred_incoming_seek_prepares_output_before_consuming_a_chunk() {
        use crate::pipeline::decode::{
            transition::{IncomingPrime, OutgoingFrontier},
            transition_promotion::preparation::IncomingPreparation,
        };

        let spec = spec(2, 1_500);
        let seek_preparations = Arc::new(AtomicU64::new(0));
        let pcm_preparations = Arc::new(AtomicU64::new(0));
        let mut generation = DecoderGeneration::new(
            Box::new(HeadSeekDecoder {
                spec,
                origin: Some(Duration::ZERO),
                landed_at: Duration::ZERO,
                landed_frame: 0,
                defer_seek: true,
                pending_seek: false,
                seek_preparations: seek_preparations.clone(),
                pcm_preparations: pcm_preparations.clone(),
            }),
            None,
            0,
            None,
            None,
            GaplessMode::Disabled,
        );
        assert!(matches!(
            generation.seek(Duration::ZERO, GaplessMode::Disabled),
            Ok(DecoderSeekOutcome::Pending(_))
        ));
        generation.trim_to(ResumeTarget::Position(Duration::ZERO));
        let preparation = IncomingPreparation {
            frontier: OutgoingFrontier::Exact {
                frame: 0,
                rate: spec.sample_rate.get(),
            },
            outgoing_spec: spec,
            outgoing_profile: generation.gapless_profile(),
            outgoing_gap: 0,
            outgoing_origin: 0,
            outgoing_exhausted: true,
            outgoing_disposition: kithara_stream::OutgoingDisposition::Retained,
            join_frames: 0,
            gapless_mode: GaplessMode::Disabled,
        };

        assert_eq!(
            generation
                .prepare_incoming(preparation)
                .expect("incoming preparation"),
            IncomingPrime::Pending
        );
        assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
        assert_eq!(pcm_preparations.load(Ordering::Relaxed), 1);
        assert!(generation.is_finished());
    }

    #[kithara::test]
    #[case::unknown_raw_zero(None, 0, 0, 0, 0, 1_024)]
    #[case::observed_shifted_pcm(Some(10_000_000), 0, 0, 0, 15, 1_024)]
    #[case::known_nonzero(Some(700_000_000), 700_000_000, 1_050, 0, 1_050, 1_024)]
    #[case::rounded_host_clock(Some(701_333_333), 701_000_000, 1_052, 0, 1_052, 1_024)]
    #[case::unknown_sought_suffix(None, 0, 0, 100_000_000, 150, 0)]
    #[case::known_sought_suffix(Some(10_000_000), 0, 0, 100_000_000, 150, 0)]
    fn seeking_an_original_head_trims_priming_but_a_coarse_sought_suffix_does_not(
        #[case] origin_nanos: Option<u64>,
        #[case] landing_nanos: u64,
        #[case] landing_frame: u64,
        #[case] requested_nanos: u64,
        #[case] output_start: u64,
        #[case] trimmed: u64,
    ) {
        for defer_seek in [false, true] {
            let spec = spec(2, 1_500);
            let pools = pools();
            let seek_preparations = Arc::new(AtomicU64::new(0));
            let pcm_preparations = Arc::new(AtomicU64::new(0));
            let mut generation = DecoderGeneration::new(
                Box::new(HeadSeekDecoder {
                    spec,
                    origin: origin_nanos.map(Duration::from_nanos),
                    landed_at: Duration::from_nanos(landing_nanos),
                    landed_frame: landing_frame,
                    defer_seek,
                    pending_seek: false,
                    seek_preparations: seek_preparations.clone(),
                    pcm_preparations: pcm_preparations.clone(),
                }),
                None,
                0,
                None,
                None,
                GaplessMode::CodecPriming,
            );
            generation.notify_seek();
            let target = Duration::from_nanos(requested_nanos);
            let outcome = generation
                .seek(target, GaplessMode::CodecPriming)
                .expect("physical seek");
            if defer_seek {
                assert!(matches!(outcome, DecoderSeekOutcome::Pending(_)));
                generation.trim_to(ResumeTarget::Position(target));
                assert!(
                    generation
                        .prepare_deferred(false, GaplessMode::CodecPriming)
                        .expect("cleanup preserves unresolved seek")
                        .is_none()
                );
                assert_eq!(seek_preparations.load(Ordering::Relaxed), 0);
                assert_eq!(pcm_preparations.load(Ordering::Relaxed), 0);
                assert!(!generation.has_output());
                assert!(
                    matches!(generation.prepare_deferred(true, GaplessMode::CodecPriming)
                    .expect("deferred physical seek"),
                    Some(DecoderSeekOutcome::Landed { landed_at, landed_frame: actual_frame, .. })
                        if landed_at == Duration::from_nanos(landing_nanos) && actual_frame == landing_frame)
                );
                assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
                assert_eq!(pcm_preparations.load(Ordering::Relaxed), 0);
                assert!(
                    generation
                        .prepare_deferred(true, GaplessMode::CodecPriming)
                        .expect("completed seek remains settled")
                        .is_none()
                );
                assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
                assert_eq!(pcm_preparations.load(Ordering::Relaxed), 1);
            } else {
                assert!(matches!(outcome, DecoderSeekOutcome::Landed { .. }));
                assert_eq!(seek_preparations.load(Ordering::Relaxed), 0);
                assert_eq!(pcm_preparations.load(Ordering::Relaxed), 0);
            }
            let mut input = chunk(&pools, &vec![0.25; 4_096], spec, output_start, 2_048, 2_048);
            input.meta.timestamp = spec
                .duration_for(output_start)
                .expect("actual output start");
            input.meta.end_timestamp = spec
                .duration_for(output_start + 2_048)
                .expect("actual output end");
            generation.push(input).expect("valid gapless PCM");
            let output = generation.next().expect("seek output");
            assert_eq!(output.meta.frame_offset, output_start + trimmed);
            assert_eq!(output.frames() as u64, 2_048 - trimmed);
            assert_eq!(output.samples.len() as u64, 2 * (2_048 - trimmed));
            if trimmed == 0 {
                assert!(output.samples.iter().all(|value| *value == 0.25));
            } else {
                const FADE_FRAMES: usize = 4;
                assert_eq!(&output.samples[..2], &[0.0; 2]);
                let mut previous = 0.0;
                for frame in output.samples[..FADE_FRAMES * 2].as_chunks::<2>().0 {
                    assert_eq!(frame[0], frame[1]);
                    assert!(frame[0] >= previous && frame[0] < 0.25);
                    previous = frame[0];
                }
                assert!(
                    output.samples[FADE_FRAMES * 2..]
                        .iter()
                        .all(|value| *value == 0.25)
                );
            }
        }
    }
}
