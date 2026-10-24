use kithara_decode::{
    DecodeResult, DecoderDuration, GaplessMode, GaplessOutput, GaplessProfile,
    GaplessTailCompensation, GaplessTrimmer,
};
use kithara_platform::time::Duration;
use kithara_signal::AudioChunk;
use kithara_stream::AudioCodec;

/// Iterator over one pending gapless output batch.
type GaplessOutputIter = <GaplessOutput as IntoIterator>::IntoIter;

/// Adapts gapless trimming to the worker's one-chunk-at-a-time decode loop.
///
/// `GaplessTrimmer` can return zero, one, or several chunks for one decoder
/// input, especially when it releases buffered tail data. This stage keeps
/// that burst local and lets `StreamAudioSource` pull one ready chunk per
/// worker step without putting gapless into the user effect chain.
pub(crate) struct GaplessStage {
    /// Owns the per-track leading/trailing trim contract.
    trimmer: GaplessTrimmer,
    /// Remaining chunks from the current trimmer output batch.
    pending: Option<GaplessOutputIter>,
    /// An exhausted batch can own heap storage; the deferred shell drops it.
    retired_pending: Vec<GaplessOutputIter>,
}

impl GaplessStage {
    /// Builds one per-generation trimmer from immutable decoder facts.
    #[must_use]
    pub(crate) fn build(
        profile: GaplessProfile,
        mode: GaplessMode,
        codec: Option<AudioCodec>,
    ) -> Self {
        let tail = tail_compensation(profile, codec);
        let from_info = |info| GaplessTrimmer::from(info).with_tail_compensation(tail);
        let trimmer = match mode {
            GaplessMode::MediaOnly => profile
                .gapless()
                .map_or_else(GaplessTrimmer::disabled, from_info),
            GaplessMode::CodecPriming => profile
                .gapless()
                .map_or_else(|| resolve_codec_priming(profile), from_info),
            _ => GaplessTrimmer::disabled(),
        };
        Self {
            trimmer,
            pending: None,
            retired_pending: Vec::with_capacity(1),
        }
    }

    pub(crate) fn prepare_deferred(&mut self) {
        self.retired_pending.clear();
    }

    /// Release any trimmer-held tail at decoder EOF.
    pub(crate) fn flush(&mut self) -> DecodeResult<()> {
        let output = self.trimmer.flush()?;
        self.replace_pending(output);
        Ok(())
    }

    #[must_use]
    pub(crate) fn has_output(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.len() != 0)
    }

    /// Return the next trimmed chunk from the current output batch.
    #[must_use]
    pub(crate) fn next(&mut self) -> Option<AudioChunk> {
        let pending = self.pending.as_mut()?;
        let next = pending.next();
        if pending.len() == 0
            && let Some(pending) = self.pending.take()
        {
            self.retired_pending.push(pending);
        }
        next
    }

    pub(crate) fn notify_seek(&mut self) {
        self.retired_pending.clear();
        self.pending = None;
        self.trimmer.notify_seek();
    }

    /// Feed one decoded chunk into the trimmer.
    pub(crate) fn push(&mut self, chunk: AudioChunk) -> DecodeResult<()> {
        let output = self.trimmer.push(chunk)?;
        self.replace_pending(output);
        Ok(())
    }

    /// Install a new trimmer output batch after the previous one is drained.
    fn replace_pending(&mut self, output: GaplessOutput) {
        debug_assert!(
            self.pending
                .as_ref()
                .is_none_or(|pending| pending.len() == 0)
        );
        self.pending = (!output.is_empty()).then(|| output.into_iter());
    }

    pub(crate) fn set_tail_compensation(
        &mut self,
        profile: GaplessProfile,
        codec: Option<AudioCodec>,
    ) {
        self.trimmer
            .set_tail_compensation(tail_compensation(profile, codec));
    }
}

fn tail_compensation(
    profile: GaplessProfile,
    codec: Option<AudioCodec>,
) -> Option<GaplessTailCompensation> {
    profile
        .tail_compensation()
        .filter(|_| !codec.is_some_and(AudioCodec::transform_padded))
}

fn resolve_codec_priming(profile: GaplessProfile) -> GaplessTrimmer {
    let frames = profile.default_priming_frames();
    if frames == 0 {
        GaplessTrimmer::disabled()
    } else {
        GaplessTrimmer::codec_priming(frames, profile.spec().sample_rate.get())
    }
}

/// Original visible PCM position when the configured leading trim is exact.
pub(crate) fn visible_origin(
    raw: Duration,
    profile: GaplessProfile,
    mode: GaplessMode,
) -> Option<Duration> {
    let leading = match mode {
        GaplessMode::Disabled => 0,
        GaplessMode::MediaOnly => profile.gapless().map_or(0, |info| info.leading_frames),
        GaplessMode::CodecPriming => profile.gapless().map_or_else(
            || profile.default_priming_frames(),
            |info| info.leading_frames,
        ),
        _ => return None,
    };
    if leading == 0 {
        return Some(raw);
    }
    let spec = profile.spec();
    let start = spec.frame_at(raw).ok()?;
    let end = start.checked_add(leading)?;
    let offset = spec
        .duration_for(end)
        .ok()?
        .checked_sub(spec.duration_for(start).ok()?)?;
    raw.checked_add(offset)
}

/// Returns the PCM duration downstream of exact metadata trim.
///
/// `MediaOnly` without reported counts keeps the raw duration until EOF.
#[must_use]
pub(crate) fn visible_duration(
    declared: Option<DecoderDuration>,
    profile: GaplessProfile,
    mode: GaplessMode,
    visible_origin: Option<Duration>,
    media_origin: Option<Duration>,
) -> Option<Duration> {
    let raw = match declared? {
        DecoderDuration::RawLength(raw) => raw,
        DecoderDuration::MediaEnd(end) => {
            return media_origin?
                .checked_add(trimmed_media_end(end, profile, mode)?)?
                .checked_sub(visible_origin?);
        }
    };
    if matches!(mode, GaplessMode::Disabled) {
        return Some(raw);
    }
    let trim_frames = match profile.gapless() {
        Some(info) => info.leading_frames.saturating_add(info.trailing_frames),
        None if matches!(mode, GaplessMode::CodecPriming) => profile.default_priming_frames(),
        None => return Some(raw),
    };
    if trim_frames == 0 {
        return Some(raw);
    }

    let trim = profile
        .spec()
        .duration_for(trim_frames)
        .unwrap_or(Duration::from_nanos(u64::MAX));
    Some(raw.saturating_sub(trim))
}

fn trimmed_media_end(
    end: Duration,
    profile: GaplessProfile,
    mode: GaplessMode,
) -> Option<Duration> {
    let trailing = if matches!(mode, GaplessMode::Disabled) {
        0
    } else {
        profile.gapless().map_or(0, |info| info.trailing_frames)
    };
    end.checked_sub(profile.spec().duration_for(trailing).ok()?)
}

/// Projects declared or measured source geometry into the logical media endpoint.
pub(crate) fn media_end(
    declared: Option<DecoderDuration>,
    profile: GaplessProfile,
    mode: GaplessMode,
    physical: Option<Duration>,
    physical_origin: Option<Duration>,
    media_origin: Option<Duration>,
    decoded_end: Option<Duration>,
) -> Option<Duration> {
    if let Some((end, origin)) = decoded_end.zip(media_origin) {
        return end.checked_sub(origin);
    }
    match declared {
        Some(DecoderDuration::RawLength(_)) | None => physical_origin?
            .checked_add(physical?)?
            .checked_sub(media_origin?),
        Some(DecoderDuration::MediaEnd(end)) => trimmed_media_end(end, profile, mode),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_utils::{
        bufpool::{pools, sample_buffer},
        kithara,
    };

    use super::*;

    #[kithara::test]
    #[case::native_44100(44_100, 46_439_909, 661, 61_428_571)]
    #[case::fractional_pts(1_000, 42_250_000, 2, 44_250_000)]
    fn sealed_visible_origin_keeps_the_original_frame_phase(
        #[case] rate: u32,
        #[case] raw_ns: u64,
        #[case] leading: u64,
        #[case] expected_ns: u64,
    ) {
        let spec = AudioSpec::new(2, NonZeroU32::new(rate).expect("source rate"));
        let profile = GaplessProfile::new(spec, None, None, leading);
        assert_eq!(
            visible_origin(
                Duration::from_nanos(raw_ns),
                profile,
                GaplessMode::CodecPriming
            ),
            Some(Duration::from_nanos(expected_ns)),
        );
        assert_eq!(
            media_end(
                Some(DecoderDuration::MediaEnd(Duration::from_secs(2))),
                profile,
                GaplessMode::CodecPriming,
                None,
                None,
                None,
                None,
            ),
            Some(Duration::from_secs(2)),
            "default leading priming cannot shorten a logical endpoint",
        );
        let leading_only = GaplessProfile::new(
            spec,
            Some(kithara_decode::GaplessInfo::new(leading, 0)),
            None,
            0,
        );
        assert_eq!(
            media_end(
                Some(DecoderDuration::MediaEnd(Duration::from_secs(2))),
                leading_only,
                GaplessMode::MediaOnly,
                None,
                None,
                None,
                Some(Duration::from_secs(1)),
            ),
            Some(Duration::from_secs(2)),
            "unrepresentable raw media zero does not erase a logical declaration",
        );
    }

    #[kithara::test]
    fn disabled_visible_origin_preserves_unrepresentable_source_frames() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("source rate"));
        let profile = GaplessProfile::new(spec, None, None, 661);
        assert_eq!(
            visible_origin(Duration::MAX, profile, GaplessMode::Disabled),
            Some(Duration::MAX)
        );
        assert_eq!(
            visible_origin(Duration::MAX, profile, GaplessMode::CodecPriming),
            None
        );
        assert_eq!(
            media_end(
                Some(DecoderDuration::MediaEnd(Duration::ZERO)),
                profile,
                GaplessMode::Disabled,
                None,
                None,
                None,
                None,
            ),
            Some(Duration::ZERO),
        );
        let padded =
            GaplessProfile::new(spec, Some(kithara_decode::GaplessInfo::new(0, 1)), None, 0);
        assert_eq!(
            media_end(
                Some(DecoderDuration::MediaEnd(Duration::ZERO)),
                padded,
                GaplessMode::MediaOnly,
                None,
                None,
                None,
                None,
            ),
            None,
        );
    }

    #[kithara::rtsan_forbid_blocking]
    fn pop_last(stage: &mut GaplessStage) -> Option<AudioChunk> {
        stage.next()
    }

    #[kithara::test]
    fn exhausted_heap_batch_is_reclaimed_by_deferred_prepare() {
        let pools = pools();
        let mut output = GaplessOutput::new();
        for _ in 0..3 {
            output.push(AudioChunk::new(
                AudioChunkInfo::default(),
                sample_buffer(&pools, &[0.0]),
            ));
        }
        assert!(output.spilled(), "fixture batch must own heap storage");
        let mut stage = GaplessStage {
            trimmer: GaplessTrimmer::disabled(),
            pending: Some(output.into_iter()),
            retired_pending: Vec::with_capacity(1),
        };
        drop(stage.next());
        drop(stage.next());
        let last = pop_last(&mut stage).expect("last pending chunk");
        assert!(stage.pending.is_none());
        assert_eq!(stage.retired_pending.len(), 1);
        drop(last);
        stage.prepare_deferred();
        assert!(stage.retired_pending.is_empty());
    }
}
