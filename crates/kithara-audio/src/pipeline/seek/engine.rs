use std::sync::atomic::{AtomicU64, Ordering};

use kithara_decode::DecodeError;
use kithara_events::{DeferredBus, Event, SeekLifecycleStage};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{PlayheadWrite, SeekControl, SeekObserve, SourceSeekAnchor, StreamType};
use tracing::{trace, warn};

use crate::pipeline::{
    decode::{
        core::DecodeCore,
        format::{FormatDecision, detect},
        gate::ReadinessGate,
    },
    rebuild::{RecreateCause, RecreateNext, RecreateState},
    seek::{
        anchor,
        emit::{emit, land_eof, location, update_len},
        recover::SeekRecovery,
        skip::estimate_target_byte,
        state::{ApplySeekState, ResumeState, SeekMode, SeekRequest},
    },
    stream::shared::SharedStream,
    track::{WaitContext, WaitingReason},
};

pub(crate) struct SeekEngine {
    epoch: Arc<AtomicU64>,
    resume_target: Option<(u64, Duration)>,
}

pub(crate) struct SeekApplyCtx<'a, T: StreamType> {
    pub(crate) decode: &'a mut DecodeCore,
    pub(crate) emit: Option<&'a DeferredBus<Event>>,
    pub(crate) playhead: &'a dyn PlayheadWrite,
    pub(crate) readiness: &'a ReadinessGate,
    pub(crate) seek: &'a dyn SeekControl,
    pub(crate) observe: &'a dyn SeekObserve,
    pub(crate) stream: &'a SharedStream<T>,
}

pub(crate) enum SeekTransition {
    Ack {
        epoch: u64,
    },
    Apply(ApplySeekState),
    Applied {
        epoch: u64,
        resume: ResumeState,
    },
    AtEof {
        epoch: u64,
    },
    Recreate(RecreateState),
    Resolve(SeekRequest),
    Reject {
        request: SeekRequest,
        error: DecodeError,
        context: &'static str,
    },
    Failed {
        request: SeekRequest,
        error: DecodeError,
        context: &'static str,
    },
    Wait {
        context: WaitContext,
        reason: WaitingReason,
    },
}

struct Consts;

impl Consts {
    const ANCHOR_RESOLUTION: &str = "seek anchor resolution failed";
}

impl SeekEngine {
    pub(crate) fn new(epoch: Arc<AtomicU64>) -> Self {
        Self {
            epoch,
            resume_target: None,
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn commit_decode_epoch(&self, epoch: u64, reason: &'static str) {
        trace!(epoch, reason, "committing producer decode epoch");
        self.epoch.store(epoch, Ordering::Release);
    }

    pub(crate) fn record_resume_target(&mut self, epoch: u64, target: Duration) {
        self.resume_target = Some((epoch, target));
    }

    pub(crate) fn resume_target(&self) -> Option<(u64, Duration)> {
        self.resume_target
    }

    pub(crate) fn apply_from_timeline<T: StreamType>(
        &mut self,
        mut request: SeekRequest,
        ctx: &SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        if ctx.observe.target().is_none() || epoch <= self.epoch() {
            ctx.seek.complete(epoch);
            return SeekTransition::Ack { epoch };
        }
        if let Some(duration) = ctx.playhead.duration()
            && position >= duration
        {
            land_eof(ctx.decode.session(), ctx.playhead, duration);
            ctx.seek.complete(epoch);
            return SeekTransition::AtEof { epoch };
        }
        if request.emit_request {
            emit(
                ctx.emit,
                SeekLifecycleStage::SeekRequest,
                epoch,
                location(ctx.stream),
            );
            request.emit_request = false;
        }
        let anchor = ctx.stream.seek_time_anchor(position);
        ctx.stream.clear_variant_fence();
        ctx.seek.complete(epoch);
        ctx.readiness.arm_peer_wake();
        let mode = match anchor {
            Ok(Some(anchor)) => SeekMode::Anchor(anchor),
            Ok(None) => SeekMode::Direct {
                target_byte: estimate_target_byte(ctx.decode.session(), ctx.stream, position),
            },
            Err(error) => {
                warn!(?error, "seek anchor resolution failed");
                return SeekTransition::Failed {
                    request,
                    error: DecodeError::SeekFailed {
                        detail: Consts::ANCHOR_RESOLUTION,
                    },
                    context: "seek anchor resolution failed",
                };
            }
        };
        SeekTransition::Apply(ApplySeekState { mode, request })
    }

    pub(crate) fn apply<T: StreamType>(
        &mut self,
        applying: ApplySeekState,
        mut ctx: SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        let request = applying.request;
        if anchor::stale(
            match applying.mode {
                SeekMode::Anchor(value) => value.variant_index,
                SeekMode::Direct { .. } => None,
            },
            ctx.stream
                .abr_handle()
                .and_then(|handle| handle.current_variant_index()),
        ) {
            return SeekTransition::Resolve(request);
        }
        match applying.mode {
            SeekMode::Anchor(anchor) => self.apply_anchor(request, anchor, &mut ctx),
            SeekMode::Direct { .. } => self.apply_direct(request, &mut ctx),
        }
    }

    fn apply_anchor<T: StreamType>(
        &mut self,
        request: SeekRequest,
        anchor_value: SourceSeekAnchor,
        ctx: &mut SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        match anchor::resolve(ctx.stream, ctx.decode.session(), request, anchor_value) {
            anchor::AnchorPlan::Failed { context, error } => {
                return SeekTransition::Failed {
                    request,
                    error,
                    context,
                };
            }
            anchor::AnchorPlan::Recreate(recreate) => {
                return SeekTransition::Recreate(recreate);
            }
            anchor::AnchorPlan::Seek => {}
        }
        ctx.stream.clear_variant_fence();
        update_len(ctx.decode, ctx.stream);
        match ctx
            .decode
            .seek(ctx.stream, ctx.playhead, request.seek.target)
        {
            Ok(_) => self.applied(request, Some(anchor_value), ctx),
            Err(error) => SeekRecovery::new(
                request,
                request.seek.target,
                anchor_value.byte_offset,
                SeekMode::Anchor(anchor_value),
            )
            .resolve(error, ctx),
        }
    }

    fn apply_direct<T: StreamType>(
        &mut self,
        request: SeekRequest,
        ctx: &mut SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        if let FormatDecision::Recreate(recreate) =
            detect(ctx.stream, ctx.decode.session(), ctx.observe)
        {
            return SeekTransition::Recreate(RecreateState {
                cause: RecreateCause::VariantSwitch,
                media_info: recreate.media_info,
                next: RecreateNext::ApplySeek(request),
                offset: recreate.offset,
            });
        }
        update_len(ctx.decode, ctx.stream);
        match ctx
            .decode
            .seek(ctx.stream, ctx.playhead, request.seek.target)
        {
            Ok(_) => self.applied(request, None, ctx),
            Err(error) => {
                let offset = ctx.decode.session().base_offset;
                let target =
                    estimate_target_byte(ctx.decode.session(), ctx.stream, request.seek.target);
                SeekRecovery::new(
                    request,
                    request.seek.target,
                    offset,
                    SeekMode::Direct {
                        target_byte: target,
                    },
                )
                .resolve(error, ctx)
            }
        }
    }

    fn applied<T: StreamType>(
        &mut self,
        request: SeekRequest,
        anchor: Option<SourceSeekAnchor>,
        ctx: &mut SeekApplyCtx<'_, T>,
    ) -> SeekTransition {
        ctx.decode.reset();
        self.record_resume_target(request.seek.epoch, request.seek.target);
        emit(
            ctx.emit,
            SeekLifecycleStage::SeekApplied,
            request.seek.epoch,
            location(ctx.stream),
        );
        SeekTransition::Applied {
            epoch: request.seek.epoch,
            resume: ResumeState {
                anchor_offset: anchor.map(|value| value.byte_offset),
                anchor_variant_index: anchor.and_then(|value| value.variant_index),
                seek: request.seek,
                ..Default::default()
            },
        }
    }
}
