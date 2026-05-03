use std::time::Duration;

use kithara_decode::{
    GaplessMode, GaplessOutput, GaplessTrimmer, InnerDecoder, PcmChunk, codec_priming_frames,
    duration_for_frames,
};
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
    pub(crate) fn from_decoder(
        decoder: &dyn InnerDecoder,
        mode: GaplessMode,
        media_info: Option<&MediaInfo>,
    ) -> Self {
        let trimmer = match mode {
            GaplessMode::MediaOnly => decoder
                .track_info()
                .gapless
                .map_or_else(GaplessTrimmer::disabled, GaplessTrimmer::from_info),
            GaplessMode::CodecPriming => decoder.track_info().gapless.map_or_else(
                || resolve_codec_priming(decoder, media_info),
                GaplessTrimmer::from_info,
            ),
            GaplessMode::SilenceTrim(params) => decoder.track_info().gapless.map_or_else(
                || GaplessTrimmer::silence_trim(params),
                GaplessTrimmer::from_info,
            ),
            // Disabled and forward-compatible variants — no trim.
            _ => GaplessTrimmer::disabled(),
        };
        Self {
            trimmer,
            pending: None,
        }
    }

    /// Feed one decoded chunk into the trimmer.
    pub(crate) fn push(&mut self, chunk: PcmChunk) {
        let output = self.trimmer.push(chunk);
        self.replace_pending(output);
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

fn resolve_codec_priming(
    decoder: &dyn InnerDecoder,
    media_info: Option<&MediaInfo>,
) -> GaplessTrimmer {
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
pub(crate) fn visible_duration(decoder: &dyn InnerDecoder, mode: GaplessMode) -> Option<Duration> {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_bufpool::pcm_pool;
    use kithara_decode::{
        DecodeResult, DecoderTrackInfo, GaplessInfo, GaplessMode, PcmMeta, PcmSpec,
        SilenceTrimParams,
    };
    use kithara_stream::{AudioCodec, MediaInfo};
    use kithara_test_utils::kithara;

    use super::*;

    struct TrackInfoDecoder {
        spec: PcmSpec,
        track_info: DecoderTrackInfo,
    }

    impl InnerDecoder for TrackInfoDecoder {
        fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
            Ok(None)
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
            Ok(())
        }

        fn update_byte_len(&self, _len: u64) {}

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn track_info(&self) -> DecoderTrackInfo {
            self.track_info.clone()
        }
    }

    fn mono_spec() -> PcmSpec {
        PcmSpec {
            channels: 1,
            sample_rate: 48_000,
        }
    }

    fn decoder_with_metadata(info: GaplessInfo) -> TrackInfoDecoder {
        let mut track_info = DecoderTrackInfo::default();
        track_info.gapless = Some(info);
        TrackInfoDecoder {
            spec: mono_spec(),
            track_info,
        }
    }

    fn metadata_free_decoder() -> TrackInfoDecoder {
        TrackInfoDecoder {
            spec: mono_spec(),
            track_info: DecoderTrackInfo::default(),
        }
    }

    fn gapless_info(leading_frames: u64, trailing_frames: u64) -> GaplessInfo {
        let mut info = GaplessInfo::default();
        info.leading_frames = leading_frames;
        info.trailing_frames = trailing_frames;
        info
    }

    fn aac_media_info() -> MediaInfo {
        let mut info = MediaInfo::default();
        info.codec = Some(AudioCodec::AacLc);
        info
    }

    fn flac_media_info() -> MediaInfo {
        let mut info = MediaInfo::default();
        info.codec = Some(AudioCodec::Flac);
        info
    }

    fn chunk(first_sample: usize, frames: usize) -> PcmChunk {
        let spec = mono_spec();
        let pcm = (first_sample..first_sample.saturating_add(frames))
            .map(|sample| f32::from(u16::try_from(sample).expect("test sample fits in u16")))
            .collect::<Vec<_>>();
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset: u64::try_from(first_sample).expect("test offset fits in u64"),
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    fn prefixed_chunk(frame_offset: u64, silence_frames: usize, content_frames: usize) -> PcmChunk {
        let spec = mono_spec();
        let mut pcm = vec![0.0; silence_frames];
        pcm.extend(
            (frame_offset as usize + silence_frames
                ..frame_offset as usize + silence_frames + content_frames)
                .map(|sample| f32::from(u16::try_from(sample).expect("test sample fits in u16"))),
        );
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    #[kithara::test]
    fn disabled_gapless_is_passthrough() {
        let mut stage =
            GaplessStage::from_decoder(&metadata_free_decoder(), GaplessMode::MediaOnly, None);

        stage.push(chunk(0, 4));

        let out = stage.next().expect("passthrough output");
        assert_eq!(out.frames(), 4);
        assert_eq!(out.samples(), &[0.0, 1.0, 2.0, 3.0]);
        assert!(stage.next().is_none());
    }

    #[kithara::test]
    fn applies_gapless_contract_as_one_stage() {
        let mut stage = GaplessStage::from_decoder(
            &decoder_with_metadata(gapless_info(1, 1)),
            GaplessMode::MediaOnly,
            None,
        );

        stage.push(chunk(0, 4));
        assert!(stage.next().is_none());

        stage.flush();
        let out = stage.next().expect("trimmed output");
        assert_eq!(out.frames(), 2);
        assert_eq!(out.meta.frame_offset, 1);
        assert_eq!(out.samples(), &[1.0, 2.0]);
        assert!(stage.next().is_none());
    }

    #[kithara::test]
    fn stores_burst_outputs_inside_stage() {
        let mut stage = GaplessStage::from_decoder(
            &decoder_with_metadata(gapless_info(0, 100)),
            GaplessMode::MediaOnly,
            None,
        );

        for start in (0..100).step_by(10) {
            stage.push(chunk(start, 10));
            assert!(stage.next().is_none());
        }

        stage.push(chunk(100, 1_000));
        for start in (0..100).step_by(10) {
            let out = stage.next().expect("ready chunk");
            assert_eq!(out.frames(), 10);
            assert_eq!(
                out.samples()[0],
                f32::from(u16::try_from(start).expect("test sample fits in u16"))
            );
        }
        assert!(stage.next().is_none());
    }

    #[kithara::test]
    fn notify_seek_drops_ready_output() {
        let mut stage = GaplessStage::from_decoder(
            &decoder_with_metadata(gapless_info(0, 100)),
            GaplessMode::MediaOnly,
            None,
        );

        for start in (0..100).step_by(10) {
            stage.push(chunk(start, 10));
        }
        stage.push(chunk(100, 1_000));
        stage.notify_seek();

        assert!(stage.next().is_none());
    }

    #[kithara::test]
    fn metadata_gapless_takes_precedence_over_codec_priming() {
        let mut stage = GaplessStage::from_decoder(
            &decoder_with_metadata(gapless_info(0, 0)),
            GaplessMode::CodecPriming,
            Some(&aac_media_info()),
        );
        stage.push(prefixed_chunk(0, 32, 32));
        let out = stage.next().expect("metadata output");
        assert_eq!(out.frames(), 64);
        assert_eq!(out.samples()[0], 0.0);
    }

    #[kithara::test]
    fn codec_priming_used_when_metadata_absent() {
        let mut stage = GaplessStage::from_decoder(
            &metadata_free_decoder(),
            GaplessMode::CodecPriming,
            Some(&aac_media_info()),
        );

        // Big enough chunk to swallow the AAC LC priming (2112) plus
        // the 144-frame fade-in plus a few "real" samples after.
        let frames = 2300_usize;
        let pcm: Vec<f32> = (0..frames).map(|_| 1.0).collect();
        let chunk = PcmChunk::new(
            PcmMeta {
                spec: mono_spec(),
                frame_offset: 0,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        );
        stage.push(chunk);

        // GaplessStage requires the caller to drain the pending batch
        // emitted by `push` before the next `push` or `flush` writes
        // a new batch. Drain → flush → drain mirrors the worker loop.
        let mut out_pcm = Vec::new();
        while let Some(chunk) = stage.next() {
            out_pcm.extend(chunk.samples().iter().copied());
        }
        stage.flush();
        while let Some(chunk) = stage.next() {
            out_pcm.extend(chunk.samples().iter().copied());
        }
        assert_eq!(out_pcm.len(), frames - 2112);
        // First sample is faded → close to zero.
        assert!(out_pcm[0].abs() < 0.05);
    }

    #[kithara::test]
    fn codec_priming_with_unknown_codec_no_trim() {
        let mut stage = GaplessStage::from_decoder(
            &metadata_free_decoder(),
            GaplessMode::CodecPriming,
            None, // no MediaInfo at all
        );
        stage.push(chunk(0, 4));
        let out = stage.next().expect("passthrough output");
        assert_eq!(out.samples(), &[0.0, 1.0, 2.0, 3.0]);
    }

    #[kithara::test]
    fn codec_priming_for_lossless_codec_no_trim() {
        let mut stage = GaplessStage::from_decoder(
            &metadata_free_decoder(),
            GaplessMode::CodecPriming,
            Some(&flac_media_info()),
        );
        stage.push(chunk(0, 4));
        let out = stage.next().expect("passthrough output");
        assert_eq!(out.samples(), &[0.0, 1.0, 2.0, 3.0]);
    }

    #[kithara::test]
    fn silence_trim_used_when_metadata_absent() {
        let mut stage = GaplessStage::from_decoder(
            &metadata_free_decoder(),
            GaplessMode::SilenceTrim(SilenceTrimParams::default()),
            None,
        );

        stage.push(prefixed_chunk(0, 300, 100));
        stage.flush();
        let out = stage.next().expect("heuristic output");
        // Frame offset is preserved past the trim point.
        assert_eq!(out.meta.frame_offset, 300);
    }
}
