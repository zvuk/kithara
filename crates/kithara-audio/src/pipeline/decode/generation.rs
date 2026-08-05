use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_decode::{
    BlenderProfile, ChunkSink, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    GaplessMode, GaplessProfile, PcmChunk,
};
use kithara_stream::MediaInfo;
use tracing::warn;

use crate::pipeline::{gapless::GaplessStage, seek::ResumeState};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct DecoderGeneration {
    decoder: Box<dyn Decoder>,
    #[field(get, vis = "pub(crate)", copy)]
    gapless_profile: GaplessProfile,
    gapless: GaplessStage,
    media_info: Option<MediaInfo>,
    pending_head_skip: Option<ResumeState>,
    staged: VecDeque<PcmChunk>,
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
            pending_head_skip,
            staged: VecDeque::new(),
        }
    }

    delegate::delegate! {
        to self.decoder {
            pub(crate) fn blender_profile(&self) -> BlenderProfile;
            #[call(as_ref)]
            pub(crate) fn decoder(&self) -> &dyn Decoder;
            #[call(as_mut)]
            pub(crate) fn decoder_mut(&mut self) -> &mut dyn Decoder;
            #[call(timeline_gap_frames)]
            pub(crate) fn timeline_gap(&self) -> u64;
        }
        to self.staged {
            #[call(pop_front)]
            pub(crate) fn pop_staged(&mut self) -> Option<PcmChunk>;
            #[call(push_front)]
            pub(crate) fn push_staged_front(&mut self, chunk: PcmChunk);
        }
    }

    pub(crate) fn finish(&mut self) {
        self.gapless.set_tail_compensation(self.gapless_profile());
        self.gapless.flush();
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

    pub(crate) fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }

    pub(crate) fn next(&mut self) -> Option<PcmChunk> {
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

    pub(crate) fn notify_seek(&mut self, retire: &dyn ChunkSink) {
        self.gapless.notify_seek(retire);
        for chunk in self.staged.drain(..) {
            retire.retire(chunk);
        }
    }

    pub(crate) fn pending_head_skip_mut(&mut self) -> Option<&mut ResumeState> {
        self.pending_head_skip.as_mut()
    }

    pub(crate) fn push(&mut self, chunk: PcmChunk) {
        self.gapless.push(chunk);
    }

    pub(crate) fn stage(&mut self, chunk: PcmChunk) {
        self.gapless.push(chunk);
        while let Some(chunk) = self.gapless.next() {
            self.staged.push_back(chunk);
        }
    }

    pub(crate) fn staged_span(&self) -> Option<(u64, u64, u32)> {
        let first = self.staged.front()?;
        let last = self.staged.back()?;
        Some((
            first.meta.frame_offset,
            last.meta
                .frame_offset
                .saturating_add(u64::from(last.meta.frames)),
            first.meta.spec.sample_rate.get(),
        ))
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

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "unknown panic payload".to_string(),
            |message| (*message).to_string(),
        ),
    }
}
