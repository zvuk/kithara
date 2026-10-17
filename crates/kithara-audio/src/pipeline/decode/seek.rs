use std::panic::{AssertUnwindSafe, catch_unwind};

use kithara_decode::{DecodeError, DecodeResult, DecoderSeekOutcome, GaplessMode, PrepareWait};
use kithara_platform::time::Duration;
use tracing::warn;

use super::DecoderGeneration;
use crate::pipeline::{
    decode::core::panic_message,
    gapless::GaplessStage,
    seek::{ResumeState, ResumeTarget},
};

impl DecoderGeneration {
    pub(crate) fn notify_seek(&mut self) {
        self.finished = false;
        self.source_exhausted = false;
        self.exhaustion_observed = false;
        self.holdback = None;
        self.gapless.notify_seek();
        self.pending_head_skip = None;
        self.staged.clear();
    }

    pub(crate) const fn pending_head_skip_mut(&mut self) -> Option<&mut ResumeState> {
        self.pending_head_skip.as_mut()
    }

    pub(crate) fn seek(
        &mut self,
        position: Duration,
        gapless_mode: GaplessMode,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(position, gapless_mode, false)
    }

    pub(crate) fn seek_for_render(
        &mut self,
        position: Duration,
        gapless_mode: GaplessMode,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(position, gapless_mode, true)
    }

    fn seek_selected(
        &mut self,
        position: Duration,
        gapless_mode: GaplessMode,
        for_render: bool,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_with_wait(position, gapless_mode, for_render, PrepareWait::Poll)
    }

    pub(crate) fn seek_with_wait(
        &mut self,
        position: Duration,
        gapless_mode: GaplessMode,
        for_render: bool,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        if !position.is_zero() {
            self.seal_visible_origin(gapless_mode);
        }
        if let Some(gate) = &self.construction_gate {
            gate.arm();
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            if for_render {
                self.decoder.seek_prefix_with_wait(position, wait)
            } else {
                self.decoder.seek_with_wait(position, wait)
            }
        }));
        if let Some(gate) = &self.construction_gate {
            gate.disarm();
        }
        match result {
            Ok(result) => {
                if let Ok(outcome) = &result {
                    self.reset_at_original_head(position, gapless_mode, outcome);
                }
                result
            }
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during seek");
                Err(DecodeError::InvalidData {
                    detail: "decoder panicked during seek",
                })
            }
        }
    }

    fn reset_at_original_head(
        &mut self,
        position: Duration,
        gapless_mode: GaplessMode,
        outcome: &DecoderSeekOutcome,
    ) {
        let DecoderSeekOutcome::Landed { landed_frame, .. } = *outcome else {
            return;
        };
        let at_known_head = self.decoder.initial_pcm_origin().is_some_and(|origin| {
            let spec = self.decoder.spec();
            let (Ok(origin_frame), Ok(requested_frame)) =
                (spec.frame_at(origin.position()), spec.frame_at(position))
            else {
                return false;
            };
            requested_frame <= origin_frame && landed_frame == origin_frame
        });
        if (position.is_zero() && landed_frame == 0) || at_known_head {
            let codec = self.media_info.as_ref().and_then(|info| info.codec);
            self.gapless = GaplessStage::build(self.gapless_profile, gapless_mode, codec);
        }
    }

    pub(crate) fn trim_to(&mut self, target: ResumeTarget) {
        self.pending_head_skip = Some(ResumeState {
            target,
            trim_head: true,
        });
    }

    pub(crate) fn resume_target(&self) -> Option<ResumeTarget> {
        self.pending_head_skip
            .as_ref()
            .filter(|resume| resume.trim_head)
            .map(|resume| resume.target)
    }

    pub(crate) fn prepare_deferred(
        &mut self,
        prepare_input: bool,
        gapless_mode: GaplessMode,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        self.prepare_deferred_with_wait(prepare_input, gapless_mode, PrepareWait::Poll)
    }

    pub(crate) fn prepare_deferred_with_wait(
        &mut self,
        prepare_input: bool,
        gapless_mode: GaplessMode,
        wait: PrepareWait,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        self.gapless.prepare_deferred();
        if !prepare_input || self.finished {
            return Ok(None);
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            self.decoder.prepare_seek_with_wait(wait)
        }))
        .map_err(|payload| {
            warn!(panic = %panic_message(payload), "decoder panicked during seek preparation");
            DecodeError::InvalidData {
                detail: "decoder panicked during seek preparation",
            }
        })??;
        match outcome {
            Some(DecoderSeekOutcome::Pending(_)) => return Ok(outcome),
            Some(terminal) => {
                let target = self.resume_target().ok_or(DecodeError::InvalidData {
                    detail: "seek continuation has no retained target",
                })?;
                self.reset_at_original_head(DecodeResult::from(target)?, gapless_mode, &terminal);
                if matches!(terminal, DecoderSeekOutcome::PastEof { .. }) {
                    self.finish()?;
                }
                return Ok(outcome);
            }
            None => {}
        }
        self.decoder.prepare_next_chunk_with_wait(wait);
        Ok(outcome)
    }
}
