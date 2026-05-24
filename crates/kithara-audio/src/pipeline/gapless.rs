use kithara_decode::{
    Decoder, GaplessMode, GaplessOutput, GaplessTrimmer, PcmChunk, codec_priming_frames,
    duration_for_frames,
};
use kithara_platform::time::Duration;
use kithara_stream::MediaInfo;

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
}

impl GaplessStage {
    /// Builds the trimmer for this track before any PCM flows through [`Self::push`].
    ///
    /// The implementation maps `GaplessMode` and the decoder `DecoderTrackInfo` into a `GaplessTrimmer`:
    /// - **`MediaOnly`** — apply gapless from track metadata when `track_info.gapless` is set; otherwise no trim.
    /// - **`CodecPriming`** — prefer metadata when present; if absent, approximate a codec priming strip using `media_info.codec`
    ///   plus the decoder PCM spec (see module helper `resolve_codec_priming`), or disable when codec/length is unknown.
    /// - **`SilenceTrim`** — prefer metadata when present; otherwise use the silence-trim parameters from the mode.
    /// - **Disabled** — passthrough, no trim.
    #[must_use]
    pub(crate) fn build(
        decoder: &dyn Decoder,
        mode: GaplessMode,
        media_info: Option<&MediaInfo>,
    ) -> Self {
        let trimmer = match mode {
            GaplessMode::MediaOnly => decoder
                .track_info()
                .gapless
                .map_or_else(GaplessTrimmer::disabled, GaplessTrimmer::from),
            GaplessMode::CodecPriming => decoder.track_info().gapless.map_or_else(
                || resolve_codec_priming(decoder, media_info),
                GaplessTrimmer::from,
            ),
            GaplessMode::SilenceTrim(params) => decoder.track_info().gapless.map_or_else(
                || GaplessTrimmer::silence_trim(params),
                GaplessTrimmer::from,
            ),
            _ => GaplessTrimmer::disabled(),
        };
        Self {
            trimmer,
            pending: None,
        }
    }

    /// Release any trimmer-held tail at decoder EOF.
    pub(crate) fn flush(&mut self) {
        let output = self.trimmer.flush();
        self.replace_pending(output);
    }

    /// Return the next trimmed chunk from the current output batch.
    #[must_use]
    pub(crate) fn next(&mut self) -> Option<PcmChunk> {
        let pending = self.pending.as_mut()?;
        let next = pending.next();
        if pending.len() == 0 {
            self.pending = None;
        }
        next
    }

    /// Drop pending output and reset seek-sensitive trimming state.
    pub(crate) fn notify_seek(&mut self) {
        self.trimmer.notify_seek();
        self.pending = None;
    }

    /// Feed one decoded chunk into the trimmer.
    pub(crate) fn push(&mut self, chunk: PcmChunk) {
        let output = self.trimmer.push(chunk);
        self.replace_pending(output);
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
}

fn resolve_codec_priming(decoder: &dyn Decoder, media_info: Option<&MediaInfo>) -> GaplessTrimmer {
    let Some(codec) = media_info.and_then(|info| info.codec) else {
        return GaplessTrimmer::disabled();
    };
    let frames = codec_priming_frames(codec);
    if frames == 0 {
        GaplessTrimmer::disabled()
    } else {
        GaplessTrimmer::codec_priming(frames, decoder.spec().sample_rate)
    }
}

/// Duration of the PCM stream as observed downstream of `GaplessStage`.
///
/// The decoder reports the raw container duration (e.g. AAC reports a value
/// that includes encoder priming and padding frames). For metadata-driven
/// trim we know the exact frame counts and can subtract them from the
/// reported duration.
///
/// For [`GaplessMode::CodecPriming`] and [`GaplessMode::SilenceTrim`], when decoder
/// metadata is absent, we deliberately return the raw duration — the
/// trim amount isn't known *a priori*, and `PlayerTrack` reconciles the
/// duration once the decoder hits EOF (`frames_until_eof` in
/// `player_track.rs`). The crossfade trigger may fire a few ms late,
/// which is preferable to mispredicting early.
///
/// [`GaplessMode::Disabled`] ignores trim metadata entirely; timeline duration stays raw.
#[must_use]
pub(crate) fn visible_duration(decoder: &dyn Decoder, mode: GaplessMode) -> Option<Duration> {
    let raw = decoder.duration()?;
    if matches!(mode, GaplessMode::Disabled) {
        return Some(raw);
    }
    let Some(info) = decoder.track_info().gapless else {
        return Some(raw);
    };
    let trim_frames = info.leading_frames.saturating_add(info.trailing_frames);
    if trim_frames == 0 {
        return Some(raw);
    }

    if decoder.spec().sample_rate == 0 {
        return Some(raw);
    }

    let trim = duration_for_frames(decoder.spec().sample_rate, trim_frames);
    Some(raw.saturating_sub(trim))
}
