use kithara_decode::{GaplessTrimmer, InnerDecoder, PcmChunk};
use smallvec::{IntoIter, SmallVec};

/// Inline output batch returned by `GaplessTrimmer`.
type GaplessOutput = SmallVec<[PcmChunk; 2]>;
/// Iterator over one pending gapless output batch.
type GaplessOutputIter = IntoIter<[PcmChunk; 2]>;

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
    /// Build a stage from decoder-owned playback metadata.
    #[must_use]
    pub(crate) fn from_decoder(decoder: &dyn InnerDecoder) -> Self {
        let trimmer = decoder
            .track_info()
            .gapless
            .map_or_else(GaplessTrimmer::disabled, GaplessTrimmer::from_info);
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_bufpool::pcm_pool;
    use kithara_decode::{DecodeResult, DecoderTrackInfo, GaplessInfo, PcmMeta, PcmSpec};
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

    fn decoder(info: GaplessInfo) -> TrackInfoDecoder {
        let mut track_info = DecoderTrackInfo::default();
        track_info.gapless = Some(info);
        TrackInfoDecoder {
            spec: mono_spec(),
            track_info,
        }
    }

    fn passthrough_decoder() -> TrackInfoDecoder {
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

    #[kithara::test]
    fn disabled_gapless_is_passthrough() {
        let mut stage = GaplessStage::from_decoder(&passthrough_decoder());

        stage.push(chunk(0, 4));

        let out = stage.next().expect("passthrough output");
        assert_eq!(out.frames(), 4);
        assert_eq!(out.samples(), &[0.0, 1.0, 2.0, 3.0]);
        assert!(stage.next().is_none());
    }

    #[kithara::test]
    fn applies_gapless_contract_as_one_stage() {
        let mut stage = GaplessStage::from_decoder(&decoder(gapless_info(1, 1)));

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
        let mut stage = GaplessStage::from_decoder(&decoder(gapless_info(0, 100)));

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
        let mut stage = GaplessStage::from_decoder(&decoder(gapless_info(0, 100)));

        for start in (0..100).step_by(10) {
            stage.push(chunk(start, 10));
        }
        stage.push(chunk(100, 1_000));
        stage.notify_seek();

        assert!(stage.next().is_none());
    }
}
