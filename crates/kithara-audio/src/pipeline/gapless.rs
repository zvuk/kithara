use kithara_decode::{
    ChunkRetire, GaplessMode, GaplessOutput, GaplessProfile, GaplessTailCompensation,
    GaplessTrimmer,
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
            GaplessMode::SilenceTrim(params) => profile
                .gapless()
                .map_or_else(|| GaplessTrimmer::silence_trim(params), from_info),
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
        if pending.len() == 0 {
            self.pending = None;
        }
        next
    }

    pub(crate) fn notify_seek(&mut self, retire: &dyn ChunkRetire) {
        if let Some(pending) = self.pending.take() {
            for chunk in pending {
                retire.retire(chunk);
            }
        }
        self.trimmer.notify_seek(retire);
    }

    /// Feed one decoded chunk into the trimmer.
    pub(crate) fn push(&mut self, chunk: AudioChunk) {
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

    pub(crate) fn set_tail_compensation(
        &mut self,
        profile: GaplessProfile,
        codec: Option<AudioCodec>,
    ) {
        self.trimmer
            .set_tail_compensation(tail_compensation(profile, codec));
    }
}

/// Tail compensation buys the frame the resampler rounded away back off the
/// trailing trim. Where padding meets audio inside a transform window, the
/// frame that buys is the tapered one, so an exact length would cost a step
/// the listener hears at the end of every track.
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

/// Returns the PCM duration downstream of exact metadata trim.
///
/// Heuristic trim keeps the raw duration until EOF reconciles the timeline.
#[must_use]
pub(crate) fn visible_duration(
    raw: Option<Duration>,
    profile: GaplessProfile,
    mode: GaplessMode,
) -> Option<Duration> {
    let raw = raw?;
    if matches!(mode, GaplessMode::Disabled) {
        return Some(raw);
    }
    let Some(info) = profile.gapless() else {
        return Some(raw);
    };
    let trim_frames = info.leading_frames.saturating_add(info.trailing_frames);
    if trim_frames == 0 {
        return Some(raw);
    }

    let trim = profile
        .spec()
        .duration_for(trim_frames)
        .unwrap_or(Duration::from_nanos(u64::MAX));
    Some(raw.saturating_sub(trim))
}
