#[cfg(test)]
use std::cell::Cell;
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_decode::{
    BlenderProfile, ChunkRetire, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    GaplessMode, GaplessProfile,
};
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_stream::MediaInfo;
use tracing::warn;

use crate::pipeline::{gapless::GaplessStage, seek::ResumeState};

#[path = "generation_holdback.rs"]
mod holdback;

#[derive(Clone, Copy)]
struct Holdback {
    join_frames: u64,
    slots: usize,
    spec: AudioSpec,
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
    #[field(get, vis = "pub(crate)", copy)]
    gapless_profile: GaplessProfile,
    gapless: GaplessStage,
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
    holdback: Option<Holdback>,
    /// Lower bound for the decoder's observed timeline gap after an exact
    /// variant splice. The overlap proof normalizes equal codec profiles to
    /// the larger gap; promotion transfers that normalization here so a later
    /// transition cannot reinterpret the new active generation on a smaller
    /// origin.
    timeline_gap_floor: u64,
    media_info: Option<MediaInfo>,
    pending_head_skip: Option<ResumeState>,
    staged: VecDeque<AudioChunk>,
    #[cfg(test)]
    staged_scan_count: Cell<usize>,
    #[field(get, vis = "pub(crate)")]
    base_offset: u64,
    #[field(get, vis = "pub(crate)")]
    installed_at_seek_epoch: u64,
}

impl DecoderGeneration {
    pub(crate) fn new(
        decoder: Box<dyn Decoder>,
        media_info: Option<MediaInfo>,
        base_offset: u64,
        installed_at_seek_epoch: u64,
        pending_head_skip: Option<ResumeState>,
        gapless_mode: GaplessMode,
    ) -> Self {
        let codec = media_info.as_ref().and_then(|info| info.codec);
        let gapless_profile = decoder.gapless_profile(codec);
        let gapless = GaplessStage::build(gapless_profile, gapless_mode);
        Self {
            decoder,
            media_info,
            base_offset,
            installed_at_seek_epoch,
            gapless_profile,
            gapless,
            finished: false,
            source_exhausted: false,
            exhaustion_observed: false,
            holdback: None,
            timeline_gap_floor: 0,
            pending_head_skip,
            staged: VecDeque::new(),
            #[cfg(test)]
            staged_scan_count: Cell::new(0),
        }
    }

    delegate::delegate! {
        to self.decoder {
            pub(crate) fn blender_profile(&self) -> BlenderProfile;
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

    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.holdback = None;
        // The tail compensation counts the whole source, so it resolves only
        // here; the profile snapshot this generation holds was taken at frame
        // zero, before the decoder had seen any of it.
        let codec = self.media_info.as_ref().and_then(|info| info.codec);
        self.gapless
            .set_tail_compensation(self.decoder.gapless_profile(codec));
        self.gapless.flush();
    }

    pub(crate) const fn mark_source_exhausted(&mut self) {
        self.source_exhausted = true;
    }

    pub(crate) const fn observe_exhaustion(&mut self) {
        self.exhaustion_observed = true;
    }

    pub(crate) fn finish_staging(&mut self) {
        self.finish();
        while let Some(chunk) = self.gapless.next() {
            self.staged.push_back(chunk);
        }
    }

    pub(crate) fn has_output(&self) -> bool {
        !self.staged.is_empty() || self.gapless.has_output()
    }

    pub(crate) const fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }

    pub(crate) fn next(&mut self) -> Option<AudioChunk> {
        self.holdback = None;
        self.staged.pop_front().or_else(|| self.gapless.next())
    }

    pub(crate) fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        match catch_unwind(AssertUnwindSafe(|| self.decoder.next_chunk())) {
            Ok(result) => result,
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during next_chunk");
                Err(DecodeError::InvalidData {
                    detail: "decoder panicked during next_chunk",
                })
            }
        }
    }
    pub(crate) fn notify_seek(&mut self, retire: &dyn ChunkRetire) {
        self.finished = false;
        self.source_exhausted = false;
        self.exhaustion_observed = false;
        self.holdback = None;
        self.gapless.notify_seek(retire);
        for chunk in self.staged.drain(..) {
            retire.retire(chunk);
        }
    }

    pub(crate) const fn pending_head_skip_mut(&mut self) -> Option<&mut ResumeState> {
        self.pending_head_skip.as_mut()
    }

    pub(crate) fn stage(&mut self, chunk: AudioChunk) {
        self.holdback = None;
        self.gapless.push(chunk);
        while let Some(chunk) = self.gapless.next() {
            self.staged.push_back(chunk);
        }
    }

    pub(crate) fn push(&mut self, chunk: AudioChunk) {
        self.holdback = None;
        self.gapless.push(chunk);
    }

    pub(crate) fn pop_staged(&mut self) -> Option<AudioChunk> {
        self.holdback = None;
        self.staged.pop_front()
    }

    pub(crate) fn push_staged_front(&mut self, chunk: AudioChunk) {
        self.holdback = None;
        self.staged.push_front(chunk);
    }

    pub(crate) fn staged_span(&self) -> Option<(u64, u64, AudioSpec)> {
        #[cfg(test)]
        self.record_staged_scan();
        let first = self.staged.front()?;
        let spec = first.spec();
        let (start, mut end) = chunk_range(first, spec)?;
        for chunk in self.staged.iter().skip(1) {
            let (next, next_end) = chunk_range(chunk, spec)?;
            if next != end {
                return None;
            }
            end = next_end;
        }
        Some((start, end, spec))
    }

    pub(crate) fn staged_covers(&self, spec: AudioSpec, start: u64, frames: u64) -> bool {
        let Some(end) = start.checked_add(frames) else {
            return false;
        };
        let Some((first, staged_end, staged_spec)) = self.staged_span() else {
            return false;
        };
        staged_spec == spec && first <= start && end <= staged_end
    }

    pub(crate) fn copy_staged_frames(
        &self,
        spec: AudioSpec,
        start: u64,
        frames: u64,
        output: &mut [f32],
    ) -> bool {
        let channels = usize::from(spec.channels);
        let Ok(frame_count) = usize::try_from(frames) else {
            return false;
        };
        let Some(sample_count) = frame_count.checked_mul(channels) else {
            return false;
        };
        if channels == 0 || output.len() != sample_count || !self.staged_covers(spec, start, frames)
        {
            return false;
        }
        let Some(end) = start.checked_add(frames) else {
            return false;
        };
        let mut written = 0usize;
        for chunk in &self.staged {
            let Some((chunk_start, chunk_end)) = chunk_range(chunk, spec) else {
                return false;
            };
            let copy_start = chunk_start.max(start);
            let copy_end = chunk_end.min(end);
            if copy_start >= copy_end {
                continue;
            }
            let Some(source_start) = usize::try_from(copy_start - chunk_start)
                .ok()
                .and_then(|frame| frame.checked_mul(channels))
            else {
                return false;
            };
            let Some(copy_samples) = usize::try_from(copy_end - copy_start)
                .ok()
                .and_then(|frame| frame.checked_mul(channels))
            else {
                return false;
            };
            let Some(source_end) = source_start.checked_add(copy_samples) else {
                return false;
            };
            let Some(output_end) = written.checked_add(copy_samples) else {
                return false;
            };
            let (Some(source), Some(destination)) = (
                chunk.samples.get(source_start..source_end),
                output.get_mut(written..output_end),
            ) else {
                return false;
            };
            destination.copy_from_slice(source);
            written = output_end;
        }
        written == output.len()
    }

    pub(crate) fn align_timeline_gap(&mut self, gap: u64) {
        self.timeline_gap_floor = self.timeline_gap_floor.max(gap);
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
}

fn chunk_range(chunk: &AudioChunk, spec: AudioSpec) -> Option<(u64, u64)> {
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

fn stage_failure(chunk: AudioChunk, detail: &'static str) -> StageFailure {
    StageFailure {
        chunk,
        error: DecodeError::InvalidData { detail },
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "unknown panic payload".to_string(),
            |message| (*message).to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, num::NonZeroU32};

    use kithara_bufpool::SamplePool;
    use kithara_decode::{DecoderSeekOutcome, DropChunks, GaplessInfo, GaplessTailCompensation};
    use kithara_platform::time::Duration;
    use kithara_signal::AudioChunkInfo;
    use kithara_stream::PrerollHint;
    use kithara_test_utils::kithara;

    use super::*;

    struct EofDecoder {
        gapless: Option<GaplessInfo>,
        default_priming: u64,
        spec: AudioSpec,
        /// Ideal pre-trim length, resolved only once decoding has started —
        /// the way a fused decode-and-resample codec counts its source.
        late_tail_frames: Option<u64>,
        profile_calls: Cell<u32>,
    }

    impl Decoder for EofDecoder {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(DecoderChunkOutcome::Eof)
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(DecoderSeekOutcome::Landed {
                landed_at: position,
                landed_frame: 0,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }

        fn spec(&self) -> AudioSpec {
            self.spec
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
                gapless: None,
                default_priming: 0,
                spec,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            0,
            None,
            GaplessMode::Disabled,
        )
    }

    fn media_generation(spec: AudioSpec, trailing_frames: u64) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(EofDecoder {
                gapless: Some(GaplessInfo::new(0, trailing_frames)),
                default_priming: 0,
                spec,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            0,
            None,
            GaplessMode::MediaOnly,
        )
    }

    /// A track whose container carries no gapless metadata while the codec
    /// table offers an estimate — the AAC-LC case at a quality-switch seam.
    fn priming_generation(spec: AudioSpec, mode: GaplessMode) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(EofDecoder {
                gapless: None,
                default_priming: 1_024,
                spec,
                late_tail_frames: None,
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            0,
            None,
            mode,
        )
    }

    fn chunk(spec: AudioSpec, offset: u64, frames: u32, sample_frames: usize) -> AudioChunk {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frame_offset: offset,
                frames,
                ..Default::default()
            },
            SamplePool::default().attach(vec![0.25; sample_frames * usize::from(spec.channels)]),
        )
    }

    /// A resampled track's tail compensation counts the whole source, so the
    /// decoder resolves it only as the last packet lands. Answering the flush
    /// from the profile this generation snapshotted at frame zero installs the
    /// value from before any of that was known, and the trimmer then cuts the
    /// full trailing run instead of holding the frame the resampler rounded
    /// away.
    #[kithara::test]
    fn finish_installs_the_tail_compensation_the_decoder_resolved_while_decoding() {
        let pushed_frames = 5_u64;
        let trailing_frames = 2_u64;
        let spec = spec(2, 44_100);
        let mut generation = DecoderGeneration::new(
            Box::new(EofDecoder {
                gapless: Some(GaplessInfo::new(0, trailing_frames)),
                default_priming: 0,
                spec,
                late_tail_frames: Some(pushed_frames + 1),
                profile_calls: Cell::new(0),
            }),
            None,
            0,
            0,
            None,
            GaplessMode::MediaOnly,
        );

        for frame in 0..pushed_frames {
            generation.stage(chunk(spec, frame, 1, 1));
        }
        generation.finish_staging();

        let mut frames = 0u64;
        while let Some(next) = generation.next() {
            frames = frames.saturating_add(u64::from(next.meta.frames));
        }

        assert_eq!(
            frames,
            pushed_frames - (trailing_frames - 1),
            "the one-frame deficit must come back off the trailing trim"
        );
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
    fn staged_holdback_rejects_gap_mixed_spec_and_bad_metadata() {
        let active = spec(2, 44_100);

        let mut gap = generation(active);
        assert!(matches!(
            gap.prepare_holdback(active, 4),
            StageResult::NeedMore
        ));
        assert!(matches!(
            gap.push_holdback(chunk(active, 0, 4, 4)),
            StageResult::NeedMore
        ));
        let StageResult::Invalid(gap_failure) = gap.push_holdback(chunk(active, 5, 4, 4)) else {
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
        let _ = mixed.push_holdback(chunk(active, 0, 4, 4));
        let StageResult::Invalid(mixed_failure) =
            mixed.push_holdback(chunk(spec(2, 48_000), 4, 4, 4))
        else {
            panic!("mixed PCM must return its offending chunk");
        };
        assert_eq!(mixed_failure.chunk.spec().sample_rate.get(), 48_000);
        assert_eq!(mixed.staged.len(), 1, "mixed PCM must not enter holdback");

        let mut bad_meta = generation(active);
        let _ = bad_meta.prepare_holdback(active, 4);
        let StageResult::Invalid(meta_failure) = bad_meta.push_holdback(chunk(active, 0, 3, 4))
        else {
            panic!("bad metadata must return its offending chunk");
        };
        assert_eq!(meta_failure.chunk.meta.frames, 3);
        assert!(bad_meta.staged.is_empty(), "bad metadata must be rejected");
    }

    #[kithara::test]
    fn staged_holdback_never_grows_past_prepared_capacity() {
        let spec = spec(1, 44_100);
        let mut generation = generation(spec);
        let _ = generation.prepare_holdback(spec, 3);
        let capacity = generation.staged.capacity();
        for frame in 0..capacity {
            let result = generation.push_holdback(chunk(
                spec,
                u64::try_from(frame).unwrap_or(u64::MAX),
                1,
                1,
            ));
            if frame + 1 == capacity {
                assert!(matches!(result, StageResult::Ready));
            }
        }
        let StageResult::Invalid(failure) = generation.push_holdback(chunk(
            spec,
            u64::try_from(capacity).unwrap_or(u64::MAX),
            1,
            1,
        )) else {
            panic!("full holdback must return the excess chunk");
        };

        assert_eq!(generation.staged.capacity(), capacity);
        assert_eq!(generation.staged.len(), capacity);
        assert_eq!(
            failure.chunk.meta.frame_offset,
            u64::try_from(capacity).unwrap_or(u64::MAX)
        );
    }

    #[kithara::test]
    fn prepare_holdback_reserves_from_staged_len_to_logical_limit() {
        let spec = spec(1, 44_100);
        let mut generation = generation(spec);
        let _ = generation.prepare_holdback(spec, 1);
        let _ = generation.push_holdback(chunk(spec, 0, 1, 1));
        let old_capacity = generation.staged.capacity();
        let slots = old_capacity.checked_add(1).expect("test slot limit");
        let join_frames = u64::try_from(slots - 1).expect("test join frame count");

        assert!(generation.staged.len() > 0);
        assert!(generation.staged.len() < old_capacity);
        assert!(old_capacity < slots);

        let _ = generation.prepare_holdback(spec, join_frames);
        let reserved_capacity = generation.staged.capacity();
        assert!(
            reserved_capacity >= slots,
            "prepared capacity {reserved_capacity} must cover all {slots} logical slots"
        );

        for frame in generation.staged.len()..slots {
            let result = generation.push_holdback(chunk(
                spec,
                u64::try_from(frame).expect("test frame offset"),
                1,
                1,
            ));
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
    fn holdback_pumps_surplus_gapless_pending_without_decoder_input() {
        const TRAILING_FRAMES: u64 = 10;
        const JOIN_FRAMES: u64 = 2;

        let spec = spec(1, 44_100);
        let mut generation = media_generation(spec, TRAILING_FRAMES);
        assert!(matches!(
            generation.prepare_holdback(spec, JOIN_FRAMES),
            StageResult::NeedMore
        ));
        let capacity = generation.staged.capacity();

        for frame in 0..TRAILING_FRAMES {
            assert!(matches!(
                generation.push_holdback(chunk(spec, frame, 1, 1)),
                StageResult::NeedMore
            ));
        }
        let release = generation.push_holdback(chunk(
            spec,
            TRAILING_FRAMES,
            u32::try_from(TRAILING_FRAMES).expect("test release frame count"),
            usize::try_from(TRAILING_FRAMES).expect("test release sample frames"),
        ));
        assert!(
            matches!(release, StageResult::Ready),
            "the first three valid released chunks make holdback ready; surplus stays pending"
        );
        assert_eq!(generation.staged.capacity(), capacity);

        let mut offsets = Vec::new();
        loop {
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
            assert_eq!(generation.staged.capacity(), capacity);
        }

        assert_eq!(offsets, (0..8).collect::<Vec<_>>());
        assert_eq!(generation.staged.capacity(), capacity);
    }

    #[kithara::test]
    fn checked_holdback_scans_the_preexisting_span_once() {
        const JOIN_FRAMES: u64 = 7_680;

        let spec = spec(1, 384_000);
        let mut generation = generation(spec);
        assert!(matches!(
            generation.prepare_holdback(spec, JOIN_FRAMES),
            StageResult::NeedMore
        ));
        let capacity = generation.staged.capacity();

        for frame in 0..=JOIN_FRAMES {
            let result = generation.push_holdback(chunk(spec, frame, 1, 1));
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
                generation.push_holdback(chunk(spec, JOIN_FRAMES + frame + 1, 1, 1)),
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
    fn seek_reopens_a_finished_generation() {
        let spec = spec(2, 44_100);
        let mut generation = generation(spec);

        generation.finish();
        generation.finish();
        assert!(generation.is_finished());

        generation.notify_seek(&DropChunks);

        assert!(!generation.is_finished());
    }
}
