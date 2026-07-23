use kithara_decode::DecodeError;
use kithara_platform::time::Duration;
use kithara_stream::StreamType;
use tracing::warn;

use crate::pipeline::{
    rebuild::{RecreateCause, RecreateNext, RecreateState},
    seek::{
        anchor,
        engine::{SeekApplyCtx, SeekTransition},
        state::{ApplySeekState, SeekMode, SeekRequest},
    },
    track::{WaitContext, WaitingReason},
};

#[derive(Clone, Copy)]
pub(crate) struct SeekRecovery {
    pub(crate) position: Duration,
    pub(crate) mode: SeekMode,
    pub(crate) request: SeekRequest,
    pub(crate) offset: u64,
}

impl SeekRecovery {
    pub(crate) fn new(
        request: SeekRequest,
        position: Duration,
        offset: u64,
        mode: SeekMode,
    ) -> Self {
        Self {
            position,
            mode,
            request,
            offset,
        }
    }

    pub(crate) fn resolve<T: StreamType>(
        self,
        error: DecodeError,
        ctx: &SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        warn!(
            ?error,
            epoch = self.request.seek.epoch,
            position = ?self.position,
            offset = self.offset,
            "decoder seek failed; applying one-shot recovery"
        );
        let fail_context = match self.mode {
            SeekMode::Direct { .. } => "seek: decoder.seek failed",
            SeekMode::Anchor(_) => "seek anchor path: exact decoder seek failed",
        };
        if matches!(error, DecodeError::SeekOutOfRange { .. }) {
            return SeekTransition::Reject {
                request: self.request,
                error,
                context: fail_context,
            };
        }
        if error.is_interrupted() {
            let applying = ApplySeekState {
                request: self.request,
                mode: self.mode,
            };
            let wait = WaitContext::ApplySeek(applying);
            let phase =
                crate::pipeline::decode::gate::source_phase_for_wait_context(ctx.stream, &wait);
            let reason = ctx
                .readiness
                .source_park(ctx.stream, phase)
                .unwrap_or(WaitingReason::Waiting);
            return SeekTransition::Wait {
                context: wait,
                reason,
            };
        }
        let Some(media_info) = ctx
            .stream
            .media_info()
            .or_else(|| ctx.decode.session().media_info.clone())
        else {
            return SeekTransition::Failed {
                request: self.request,
                error,
                context: fail_context,
            };
        };
        let Some(offset) =
            anchor::recreate_offset(ctx.stream, media_info.container, false, self.offset)
        else {
            return SeekTransition::Failed {
                request: self.request,
                error: DecodeError::InvalidData {
                    detail: "seek recovery: decoder recreate requires init segment range",
                },
                context: fail_context,
            };
        };
        SeekTransition::Recreate(RecreateState {
            cause: RecreateCause::VariantSwitch,
            media_info,
            next: RecreateNext::ApplySeek(self.request),
            offset,
        })
    }
}
