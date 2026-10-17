use std::num::NonZeroU32;

use kithara_decode::{DecodeError, DecodeResult, DecoderSeekOutcome};
use kithara_platform::{sync::mpsc, time::Duration};
use kithara_signal::{AudioSpec, FrameCount, rescale_frame_ceil};
use kithara_stream::{PendingReason, VariantTransition};
use kithara_test_utils::kithara;
use tracing::{debug, trace};

use super::{
    generation::DecoderGeneration,
    transition_promotion::preparation::{
        IncomingPreparation, IncomingPreparationOwner, IncomingPreparationRequest, PreparedIncoming,
    },
};
use crate::pipeline::{blend::GaplessBlender, seek::skip::apply_frames};

pub(crate) enum IncomingDecode {
    Preparing {
        transition: VariantTransition,
        frontier: OutgoingFrontier,
    },
    Building {
        transition: VariantTransition,
        frontier: OutgoingFrontier,
        preparation: IncomingPreparationOwner,
    },
    Priming {
        transition: VariantTransition,
        generation: DecoderGeneration,
        frontier: OutgoingFrontier,
        preparation: IncomingPreparationOwner,
    },
    Failed {
        transition: VariantTransition,
        generation: DecoderGeneration,
    },
}

impl IncomingDecode {
    pub(crate) const fn transition(&self) -> VariantTransition {
        match self {
            Self::Preparing { transition, .. }
            | Self::Building { transition, .. }
            | Self::Priming { transition, .. }
            | Self::Failed { transition, .. } => *transition,
        }
    }
}

impl From<IncomingDecode> for Option<DecoderGeneration> {
    fn from(incoming: IncomingDecode) -> Self {
        match incoming {
            IncomingDecode::Priming { generation, .. }
            | IncomingDecode::Failed { generation, .. } => Some(generation),
            IncomingDecode::Preparing { .. } | IncomingDecode::Building { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutgoingFrontier {
    Awaiting,
    Exact { frame: u64, rate: u32 },
    Unavailable,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct PreparedPromotion {
    pub(super) generation: DecoderGeneration,
    pub(super) join: PromotionJoin,
    pub(super) preparation: IncomingPreparationOwner,
    #[field(get, vis = "pub(crate)", copy)]
    pub(super) resume_head: crate::SourceEnd,
    #[field(get, vis = "pub(crate)", copy)]
    pub(super) transition: VariantTransition,
}

#[derive(Clone, Copy)]
pub(super) struct PromotionSpan {
    pub(super) overlap: OverlapSpan,
    pub(super) join: PromotionJoin,
}

#[derive(Clone, Copy)]
pub(super) enum PromotionReadiness {
    Ready(PromotionSpan),
    NeedIncoming,
    AwaitingOutgoingFrontier,
    AwaitingOutgoingPcm { first: u64 },
}

impl PromotionReadiness {
    pub(super) fn after_incoming_eof(
        self,
        generation: &DecoderGeneration,
        latched_frontier: OutgoingFrontier,
        outgoing_rate: u32,
        outgoing_origin: u64,
        incoming_origin: u64,
    ) -> IncomingPrime {
        match self {
            Self::Ready(_) => IncomingPrime::Ready,
            Self::AwaitingOutgoingFrontier | Self::AwaitingOutgoingPcm { .. } => {
                IncomingPrime::Pending
            }
            Self::NeedIncoming => {
                if staged_ahead(
                    generation,
                    latched_frontier,
                    outgoing_rate,
                    outgoing_origin,
                    incoming_origin,
                ) {
                    IncomingPrime::Pending
                } else {
                    debug!(
                        outgoing_frontier = ?latched_frontier,
                        outgoing_origin,
                        "incoming generation reached EOF before the outgoing frontier"
                    );
                    IncomingPrime::Failed
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum PromotionJoin {
    HardCut,
    Blend {
        frames: u64,
        outgoing_first: u64,
        spec: AudioSpec,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct OverlapSpan {
    incoming_first: u64,
    incoming_next: u64,
}

impl From<PreparedPromotion> for DecoderGeneration {
    fn from(prepared: PreparedPromotion) -> Self {
        prepared.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncomingPrime {
    Idle,
    Advanced,
    Pending,
    Ready,
    Failed,
}

impl super::core::ActiveDecode {
    /// One `DecoderEvent::TransitionHold` per transition: the first held
    /// pass announces it, later passes stay silent.
    ///
    /// That once-per-transition edge is also where the reason the pipeline
    /// stopped producing is recorded. A hang dump otherwise shows only that the
    /// consumer starved: the readiness verdict itself is `TRACE` and never
    /// reaches the flight recorder.
    pub(crate) fn announce_transition_hold(&mut self) -> bool {
        let Some(transition) = self.incoming.as_ref().map(IncomingDecode::transition) else {
            return false;
        };
        if self.announced_hold == Some(transition) {
            return false;
        }
        self.announced_hold = Some(transition);
        let (stage, staged, incoming_finished) = match self.incoming.as_ref() {
            Some(IncomingDecode::Preparing { .. }) => ("preparing", None, false),
            Some(IncomingDecode::Building { .. }) => ("building", None, false),
            Some(IncomingDecode::Priming { generation, .. }) => (
                "priming",
                generation.staged_span().map(|(first, end, _)| (first, end)),
                generation.is_finished(),
            ),
            Some(IncomingDecode::Failed { .. }) | None => ("failed", None, false),
        };
        debug!(
            ?transition,
            stage,
            ?staged,
            incoming_finished,
            frontier = ?self.incoming_frontier(),
            source_exhausted = self.active.is_source_exhausted(),
            blender_steady = self.blender.is_steady(),
            "decode parked on a pending variant transition"
        );
        true
    }

    pub(crate) fn begin_incoming(
        &mut self,
        transition: VariantTransition,
        frontier: OutgoingFrontier,
    ) -> Option<DecoderGeneration> {
        if self.incoming_transition() == Some(transition) {
            return None;
        }
        self.incoming
            .replace(IncomingDecode::Preparing {
                transition,
                frontier,
            })
            .and_then(Into::into)
    }

    pub(crate) fn flush_incoming_reader_signals(&mut self) {
        match self.incoming.as_mut() {
            Some(IncomingDecode::Priming { generation, .. })
            | Some(IncomingDecode::Failed { generation, .. }) => {
                generation.decoder_mut().flush_reader_signals();
            }
            Some(IncomingDecode::Preparing { .. } | IncomingDecode::Building { .. }) | None => {}
        }
    }

    pub(crate) const fn has_live_incoming(&self) -> bool {
        matches!(
            self.incoming,
            Some(
                IncomingDecode::Preparing { .. }
                    | IncomingDecode::Building { .. }
                    | IncomingDecode::Priming { .. }
            )
        )
    }

    pub(crate) fn incoming_frontier(&self) -> Option<OutgoingFrontier> {
        match self.incoming.as_ref()? {
            IncomingDecode::Preparing { frontier, .. }
            | IncomingDecode::Building { frontier, .. }
            | IncomingDecode::Priming { frontier, .. } => Some(*frontier),
            IncomingDecode::Failed { .. } => None,
        }
    }

    pub(crate) fn incoming_is_preparing(&self, transition: VariantTransition) -> bool {
        matches!(
            self.incoming,
            Some(IncomingDecode::Preparing {
                transition: current,
                ..
            }) if current == transition
        )
    }

    #[cfg(test)]
    pub(crate) fn incoming_is_priming(&self, transition: VariantTransition) -> bool {
        matches!(
            self.incoming,
            Some(IncomingDecode::Priming {
                transition: current,
                ..
            }) if current == transition
        )
    }

    #[cfg(test)]
    pub(crate) fn incoming_staged_span(&self) -> Option<(u64, u64, AudioSpec)> {
        let IncomingDecode::Priming { generation, .. } = self.incoming.as_ref()? else {
            return None;
        };
        generation.staged_span()
    }

    pub(crate) fn begin_incoming_build(
        &mut self,
        transition: VariantTransition,
        preparation: IncomingPreparationOwner,
    ) -> bool {
        let Some(IncomingDecode::Preparing {
            transition: current,
            frontier,
        }) = self.incoming.as_ref()
        else {
            return false;
        };
        if *current != transition {
            return false;
        }
        self.incoming = Some(IncomingDecode::Building {
            transition,
            frontier: *frontier,
            preparation,
        });
        true
    }

    pub(crate) fn poll_incoming_build(
        &self,
    ) -> Option<(VariantTransition, DecodeResult<PreparedIncoming>)> {
        let IncomingDecode::Building {
            transition,
            preparation,
            ..
        } = self.incoming.as_ref()?
        else {
            return None;
        };
        let result = match preparation.completion.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(_) => Err(DecodeError::InvalidData {
                detail: "incoming decoder preparation channel closed",
            }),
        };
        Some((*transition, result))
    }

    pub(crate) fn install_incoming(
        &mut self,
        transition: VariantTransition,
        generation: DecoderGeneration,
    ) -> Option<DecoderGeneration> {
        let Some(IncomingDecode::Building {
            transition: current,
            ..
        }) = self.incoming.as_ref()
        else {
            return Some(generation);
        };
        if *current != transition {
            return Some(generation);
        }
        let Some(IncomingDecode::Building {
            frontier,
            preparation,
            ..
        }) = self.incoming.take()
        else {
            return Some(generation);
        };
        self.prepare_incoming_profile(generation.blender_profile());
        self.incoming = Some(IncomingDecode::Priming {
            transition,
            generation,
            frontier,
            preparation,
        });
        None
    }

    /// Land incoming content at the exact outgoing decode frontier, never the lagging playhead.
    /// A landing behind it cannot catch up; one ahead assumes progress a paused consumer may refuse.
    /// Non-exact frontiers return `None`, preserving the source's seek-derived target.
    pub(crate) fn landing_for(&self, outgoing_frontier: OutgoingFrontier) -> Option<Duration> {
        let OutgoingFrontier::Exact { frame, rate } = outgoing_frontier else {
            return None;
        };
        let origin = self.active.comparison_origin(self.gapless_mode())?;
        let active_rate = self.active.blender_profile().spec().sample_rate.get();
        content_time(rate, frame, active_rate, origin)
    }

    pub(crate) fn observe_source_exhaustion(&mut self) {
        if self.active.is_source_exhausted() {
            self.active.observe_exhaustion();
        }
    }

    pub(crate) fn incoming_preparation(
        &self,
        frontier: OutgoingFrontier,
    ) -> Option<IncomingPreparation> {
        let gapless_mode = self.gapless_mode();
        Some(IncomingPreparation {
            frontier,
            outgoing_spec: self.active.blender_profile().spec(),
            outgoing_profile: self.active.gapless_profile(),
            outgoing_gap: self.active.timeline_gap(),
            outgoing_origin: self.active.comparison_origin(gapless_mode)?,
            outgoing_exhausted: self.active.is_source_exhausted(),
            join_frames: self.blender.join_frame_count(),
            gapless_mode,
        })
    }

    pub(crate) fn finish_incoming_preparation(&mut self, outcome: DecodeResult<IncomingPrime>) {
        match outcome {
            Ok(IncomingPrime::Failed) => self.fail_incoming(),
            Err(error) => {
                self.stage_error = Some(error);
                self.fail_incoming();
            }
            Ok(_) => {}
        }
    }

    pub(crate) fn prime_incoming(&mut self, outgoing_frontier: OutgoingFrontier) -> IncomingPrime {
        if !matches!(self.incoming, Some(IncomingDecode::Priming { .. })) {
            return IncomingPrime::Idle;
        }
        let Some(mut target) = self.incoming_preparation(outgoing_frontier) else {
            self.fail_incoming();
            return IncomingPrime::Failed;
        };
        let (active, blender) = (&self.active, &self.blender);
        let Some(IncomingDecode::Priming {
            frontier,
            generation,
            ..
        }) = self.incoming.as_mut()
        else {
            return IncomingPrime::Idle;
        };
        let Some(incoming_origin) = incoming_origin_from(
            target.outgoing_profile,
            target.outgoing_gap,
            generation,
            target.gapless_mode,
        ) else {
            self.fail_incoming();
            return IncomingPrime::Failed;
        };
        let mut candidate = if *frontier == OutgoingFrontier::Awaiting {
            outgoing_frontier
        } else {
            *frontier
        };
        let mut readiness = promotion_readiness(
            active,
            blender,
            generation,
            candidate,
            target.outgoing_origin,
            incoming_origin,
            target.gapless_mode,
        );
        if matches!(readiness, PromotionReadiness::AwaitingOutgoingFrontier)
            && observed_frontier_is_later(
                candidate,
                outgoing_frontier,
                target.outgoing_spec.sample_rate.get(),
                target.outgoing_origin,
            )
        {
            candidate = outgoing_frontier;
            readiness = promotion_readiness(
                active,
                blender,
                generation,
                candidate,
                target.outgoing_origin,
                incoming_origin,
                target.gapless_mode,
            );
        }
        if matches!(
            readiness,
            PromotionReadiness::Ready(_) | PromotionReadiness::AwaitingOutgoingPcm { .. }
        ) {
            *frontier = candidate;
        }
        match readiness {
            PromotionReadiness::Ready(_) => return IncomingPrime::Ready,
            PromotionReadiness::AwaitingOutgoingFrontier
            | PromotionReadiness::AwaitingOutgoingPcm { .. } => return IncomingPrime::Pending,
            PromotionReadiness::NeedIncoming => {}
        }
        if generation.is_finished() {
            let outcome = readiness.after_incoming_eof(
                generation,
                candidate,
                target.outgoing_spec.sample_rate.get(),
                target.outgoing_origin,
                incoming_origin,
            );
            if outcome == IncomingPrime::Failed {
                self.fail_incoming();
            }
            return outcome;
        }
        target.frontier = candidate;
        let Some(IncomingDecode::Priming {
            transition,
            generation,
            frontier,
            preparation,
        }) = self.incoming.take()
        else {
            return IncomingPrime::Idle;
        };
        match preparation
            .requests
            .send(IncomingPreparationRequest { generation, target })
        {
            Ok(()) => {
                self.incoming = Some(IncomingDecode::Building {
                    transition,
                    frontier,
                    preparation,
                });
                IncomingPrime::Advanced
            }
            Err(error) => {
                self.incoming = Some(IncomingDecode::Failed {
                    transition,
                    generation: error.0.generation,
                });
                IncomingPrime::Failed
            }
        }
    }

    fn fail_incoming(&mut self) {
        if let Some(IncomingDecode::Priming {
            transition,
            generation,
            ..
        }) = self.incoming.take()
        {
            self.incoming = Some(IncomingDecode::Failed {
                transition,
                generation,
            });
        }
    }

    delegate::delegate! {
        to self.incoming {
            #[expr($.and_then(Into::into))]
            #[call(take)]
            pub(crate) fn discard_incoming(&mut self) -> Option<DecoderGeneration>;
            #[expr($.map(IncomingDecode::transition))]
            #[call(as_ref)]
            pub(crate) fn incoming_transition(&self) -> Option<VariantTransition>;
        }
    }
}

pub(super) fn classify_incoming_preparation(
    outcome: &DecodeResult<Option<DecoderSeekOutcome>>,
) -> Option<IncomingPrime> {
    match outcome {
        Ok(Some(DecoderSeekOutcome::Pending(
            PendingReason::VariantChange | PendingReason::SessionRetired,
        )))
        | Err(_) => Some(IncomingPrime::Failed),
        Ok(Some(
            DecoderSeekOutcome::Pending(PendingReason::Retry) | DecoderSeekOutcome::Landed { .. },
        )) => Some(IncomingPrime::Advanced),
        Ok(Some(DecoderSeekOutcome::Pending(_))) => Some(IncomingPrime::Pending),
        Ok(Some(DecoderSeekOutcome::PastEof { .. })) | Ok(None) => None,
    }
}

pub(super) fn promotion_readiness(
    active: &DecoderGeneration,
    blender: &GaplessBlender,
    generation: &DecoderGeneration,
    outgoing_frontier: OutgoingFrontier,
    outgoing_origin: u64,
    incoming_origin: u64,
    gapless_mode: kithara_decode::GaplessMode,
) -> PromotionReadiness {
    if !blender.is_steady() {
        return PromotionReadiness::NeedIncoming;
    }
    let target = IncomingPreparation {
        frontier: outgoing_frontier,
        outgoing_spec: active.blender_profile().spec(),
        outgoing_profile: active.gapless_profile(),
        outgoing_gap: active.timeline_gap(),
        outgoing_origin,
        outgoing_exhausted: active.is_source_exhausted(),
        join_frames: blender.join_frame_count(),
        gapless_mode,
    };
    let readiness = incoming_readiness(generation, target, incoming_origin);
    let PromotionReadiness::Ready(mut span) = readiness else {
        return readiness;
    };
    let PromotionJoin::Blend {
        frames,
        outgoing_first,
        spec,
    } = span.join
    else {
        return readiness;
    };
    if active.staged_covers(spec, outgoing_first, frames) {
        return readiness;
    }
    if active.is_source_exhausted() {
        span.join = PromotionJoin::HardCut;
        return PromotionReadiness::Ready(span);
    }
    if active.is_finished() {
        PromotionReadiness::NeedIncoming
    } else {
        PromotionReadiness::AwaitingOutgoingPcm {
            first: outgoing_first,
        }
    }
}

pub(super) fn incoming_readiness(
    generation: &DecoderGeneration,
    target: IncomingPreparation,
    incoming_origin: u64,
) -> PromotionReadiness {
    let Some((incoming_first, incoming_end, incoming_spec)) = generation.staged_span() else {
        return PromotionReadiness::NeedIncoming;
    };
    let incoming_rate = incoming_spec.sample_rate.get();
    let (outgoing_frame, outgoing_rate) =
        match resolve_frontier(target.outgoing_exhausted, target.frontier, incoming_first) {
            Ok(pair) => pair,
            Err(verdict) => return verdict,
        };
    let active_spec = target.outgoing_spec;
    let active_rate = active_spec.sample_rate.get();
    let Some(incoming_first_time) = content_time(
        incoming_rate,
        incoming_first.max(incoming_origin),
        incoming_rate,
        incoming_origin,
    ) else {
        return PromotionReadiness::NeedIncoming;
    };
    let Some(incoming_end_time) =
        content_time(incoming_rate, incoming_end, incoming_rate, incoming_origin)
    else {
        return PromotionReadiness::NeedIncoming;
    };
    let Some(outgoing_next) = content_time(
        outgoing_rate,
        outgoing_frame,
        active_rate,
        target.outgoing_origin,
    ) else {
        return PromotionReadiness::NeedIncoming;
    };
    if incoming_first_time > outgoing_next {
        if target.outgoing_exhausted {
            debug!(
                ?incoming_first_time,
                ?outgoing_next,
                "outgoing exhausted behind the incoming landing: hard cut"
            );
            return hard_cut_at(incoming_first, incoming_first);
        }
        trace!(
            ?incoming_first_time,
            ?incoming_end_time,
            ?outgoing_next,
            incoming_first,
            incoming_origin,
            incoming_rate,
            outgoing_frame,
            outgoing_origin = target.outgoing_origin,
            outgoing_rate,
            "incoming generation is waiting for the outgoing frontier"
        );
        return PromotionReadiness::AwaitingOutgoingFrontier;
    }
    if outgoing_next >= incoming_end_time {
        if target.outgoing_exhausted && generation.is_finished() {
            debug!(
                incoming_first,
                incoming_end,
                ?outgoing_next,
                "exhausted frontier meets a finished incoming: committing at the end"
            );
            return hard_cut_at(incoming_first, incoming_end);
        }
        return PromotionReadiness::NeedIncoming;
    }
    let (incoming_next, join) = if incoming_spec == active_spec {
        let Some(outgoing_first) = NonZeroU32::new(outgoing_rate)
            .and_then(|rate| rescale_frame_ceil(outgoing_frame, rate, active_spec.sample_rate))
        else {
            return PromotionReadiness::NeedIncoming;
        };
        let Some(incoming_next) = outgoing_first
            .checked_sub(target.outgoing_origin)
            .and_then(|frame| frame.checked_add(incoming_origin))
        else {
            return PromotionReadiness::NeedIncoming;
        };
        let join = match same_spec_join(
            generation,
            active_spec,
            incoming_next,
            outgoing_first,
            outgoing_next,
            target.join_frames,
        ) {
            Ok(join) => join,
            Err(verdict) => return verdict,
        };
        (incoming_next, join)
    } else {
        let Some(incoming_next) = frame_at_or_after(incoming_rate, incoming_origin, outgoing_next)
        else {
            return PromotionReadiness::NeedIncoming;
        };
        (incoming_next, PromotionJoin::HardCut)
    };
    if incoming_first > incoming_next || incoming_next >= incoming_end {
        return PromotionReadiness::NeedIncoming;
    }
    let span = PromotionSpan {
        join,
        overlap: OverlapSpan {
            incoming_first,
            incoming_next,
        },
    };
    debug!(
        incoming_first,
        incoming_next,
        incoming_end,
        incoming_origin,
        incoming_rate,
        outgoing_frame,
        outgoing_origin = target.outgoing_origin,
        outgoing_rate,
        ?outgoing_next,
        blended = matches!(span.join, PromotionJoin::Blend { .. }),
        "incoming generation covers the outgoing frontier"
    );
    PromotionReadiness::Ready(span)
}

fn hard_cut_at(incoming_first: u64, incoming_next: u64) -> PromotionReadiness {
    PromotionReadiness::Ready(PromotionSpan {
        join: PromotionJoin::HardCut,
        overlap: OverlapSpan {
            incoming_first,
            incoming_next,
        },
    })
}

/// An exhausted outgoing can never establish or advance a frontier, so every case waiting on it
/// below is a dead end; the switch degrades to a hard cut instead of wedging forever.
fn resolve_frontier(
    outgoing_exhausted: bool,
    frontier: OutgoingFrontier,
    incoming_first: u64,
) -> Result<(u64, u32), PromotionReadiness> {
    match frontier {
        OutgoingFrontier::Awaiting if !outgoing_exhausted => {
            Err(PromotionReadiness::AwaitingOutgoingFrontier)
        }
        OutgoingFrontier::Awaiting | OutgoingFrontier::Unavailable => {
            Err(hard_cut_at(incoming_first, incoming_first))
        }
        OutgoingFrontier::Exact { frame, rate } => Ok((frame, rate)),
    }
}

/// A finished incoming without enough join PCM can only splice at the proven cut.
fn same_spec_join(
    generation: &DecoderGeneration,
    spec: AudioSpec,
    incoming_next: u64,
    outgoing_first: u64,
    outgoing_next: Duration,
    frames: u64,
) -> Result<PromotionJoin, PromotionReadiness> {
    if generation.staged_covers(spec, incoming_next, frames) {
        return Ok(PromotionJoin::Blend {
            frames,
            outgoing_first,
            spec,
        });
    }
    if generation.is_finished() {
        debug!(
            frames,
            incoming_next,
            outgoing_first,
            "incoming finished without join PCM: hard cut at the frontier"
        );
        return Ok(PromotionJoin::HardCut);
    }
    trace!(
        frames,
        incoming_next,
        outgoing_first,
        ?outgoing_next,
        "same-spec promotion is waiting for incoming join PCM"
    );
    Err(PromotionReadiness::NeedIncoming)
}

fn staged_ahead(
    generation: &DecoderGeneration,
    outgoing_frontier: OutgoingFrontier,
    outgoing_origin_rate: u32,
    outgoing_origin: u64,
    incoming_origin: u64,
) -> bool {
    let Some((incoming_first, _, incoming_spec)) = generation.staged_span() else {
        return false;
    };
    let incoming_rate = incoming_spec.sample_rate.get();
    match outgoing_frontier {
        OutgoingFrontier::Awaiting => true,
        OutgoingFrontier::Unavailable => false,
        OutgoingFrontier::Exact { frame, rate } => match (
            content_time(
                incoming_rate,
                incoming_first,
                incoming_rate,
                incoming_origin,
            ),
            content_time(rate, frame, outgoing_origin_rate, outgoing_origin),
        ) {
            (Some(incoming), Some(outgoing)) => incoming > outgoing,
            _ => false,
        },
    }
}

fn content_time(rate: u32, frame: u64, origin_rate: u32, origin: u64) -> Option<Duration> {
    if rate == origin_rate {
        let spec = timeline_spec(rate)?;
        return frame.checked_sub(origin).map(|frames| {
            spec.duration_for(frames)
                .unwrap_or(Duration::from_nanos(u64::MAX))
        });
    }
    let frame_time = timeline_spec(rate)?
        .duration_for(frame)
        .unwrap_or(Duration::from_nanos(u64::MAX));
    let origin_time = timeline_spec(origin_rate)?
        .duration_for(origin)
        .unwrap_or(Duration::from_nanos(u64::MAX));
    frame_time.checked_sub(origin_time)
}

fn observed_frontier_is_later(
    current: OutgoingFrontier,
    observed: OutgoingFrontier,
    origin_rate: u32,
    origin: u64,
) -> bool {
    let (
        OutgoingFrontier::Exact {
            frame: current_frame,
            rate: current_rate,
        },
        OutgoingFrontier::Exact {
            frame: observed_frame,
            rate: observed_rate,
        },
    ) = (current, observed)
    else {
        return false;
    };
    match (
        content_time(current_rate, current_frame, origin_rate, origin),
        content_time(observed_rate, observed_frame, origin_rate, origin),
    ) {
        (Some(current), Some(observed)) => observed > current,
        _ => false,
    }
}

fn frame_at_or_after(rate: u32, origin: u64, time: Duration) -> Option<u64> {
    let spec = timeline_spec(rate)?;
    let frames = spec.frames_for(time).unwrap_or(FrameCount::new(usize::MAX));
    let mut frame = u64::try_from(frames.get()).ok()?;
    if spec
        .duration_for(frame)
        .unwrap_or(Duration::from_nanos(u64::MAX))
        < time
    {
        frame = frame.checked_add(1)?;
    }
    origin.checked_add(frame)
}

fn timeline_spec(sample_rate: u32) -> Option<AudioSpec> {
    NonZeroU32::new(sample_rate).map(|sample_rate| AudioSpec::new(1, sample_rate))
}

#[kithara::hang_watchdog]
pub(super) fn trim_staged_head(
    generation: &mut DecoderGeneration,
    overlap: OverlapSpan,
) -> DecodeResult<bool> {
    let mut remaining = overlap.incoming_next.saturating_sub(overlap.incoming_first);
    if remaining == 0 {
        return Ok(generation.has_output());
    }
    while remaining != 0 {
        hang_tick!();
        let previous = remaining;
        let Some(chunk) = generation.pop_staged() else {
            return Ok(false);
        };
        if let Some(chunk) = apply_frames(chunk, &mut remaining)? {
            generation.push_staged_front(chunk);
        }
        if remaining < previous {
            hang_reset!();
        }
    }
    Ok(generation.has_output())
}

pub(super) fn incoming_origin(
    active: &DecoderGeneration,
    incoming: &DecoderGeneration,
    mode: kithara_decode::GaplessMode,
) -> Option<u64> {
    incoming_origin_from(
        active.gapless_profile(),
        active.timeline_gap(),
        incoming,
        mode,
    )
}

/// The seam moves by exactly the AAC-LC default priming when the incoming track's gapless metadata
/// is missing, since `origin` alone cannot say which case it is.
pub(super) fn incoming_origin_from(
    active_profile: kithara_decode::GaplessProfile,
    active_gap: u64,
    incoming: &DecoderGeneration,
    mode: kithara_decode::GaplessMode,
) -> Option<u64> {
    let incoming_profile = incoming.gapless_profile();
    let gap = if shares_default_profile(active_profile, incoming_profile) {
        active_gap.max(incoming.timeline_gap())
    } else {
        incoming.timeline_gap()
    };
    let origin = incoming.comparison_origin_with_gap(mode, gap)?;
    debug!(
        origin,
        gap,
        gapless_known = incoming_profile.gapless().is_some(),
        default_priming = incoming_profile.default_priming_frames(),
        visible_origin = ?incoming.visible_origin(),
        comparison_base = ?incoming.comparison_base(),
        ?mode,
        "incoming timeline origin resolved"
    );
    Some(origin)
}

pub(super) fn shares_default_profile(
    active: kithara_decode::GaplessProfile,
    incoming: kithara_decode::GaplessProfile,
) -> bool {
    active.gapless().is_none()
        && incoming.gapless().is_none()
        && active.default_priming_frames() == incoming.default_priming_frames()
}
#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{content_time, frame_at_or_after};

    #[kithara::test(native, flash(false))]
    fn same_rate_cut_preserves_exact_nonzero_origin() {
        const RATE: u32 = 44_100;
        const ORIGIN: u64 = 1_024;
        const FRONTIER: u64 = 2_048;

        let time = content_time(RATE, FRONTIER, RATE, ORIGIN).expect("frontier follows origin");
        let cut = frame_at_or_after(RATE, ORIGIN, time);

        assert_eq!(cut, Some(FRONTIER));
    }
}
