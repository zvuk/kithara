#[path = "transition_promotion.rs"]
mod promotion;

use kithara_decode::{DecoderChunkOutcome, PcmSpec, duration_for_frames, frames_for_duration};
use kithara_platform::time::Duration;
use kithara_stream::{PendingReason, VariantTransition};
use tracing::{debug, trace};

use super::generation::DecoderGeneration;
use crate::pipeline::{
    blend::PcmBlender,
    rebuild::state::BuildId,
    seek::skip::{apply as apply_skip, apply_frames},
};

struct Consts;

impl Consts {
    const PRIME_STEPS_PER_PASS: usize = 8;
}

pub(crate) enum IncomingDecode {
    Preparing {
        transition: VariantTransition,
        frontier: OutgoingFrontier,
    },
    Building {
        transition: VariantTransition,
        build: BuildId,
        frontier: OutgoingFrontier,
    },
    Priming {
        transition: VariantTransition,
        generation: DecoderGeneration,
        frontier: OutgoingFrontier,
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
    generation: DecoderGeneration,
    join: PromotionJoin,
    #[field(get, vis = "pub(crate)", copy)]
    transition: VariantTransition,
}

#[derive(Clone, Copy)]
struct PromotionSpan {
    join: PromotionJoin,
    overlap: OverlapSpan,
}

#[derive(Clone, Copy)]
enum PromotionReadiness {
    Ready(PromotionSpan),
    NeedIncoming,
    AwaitingOutgoingFrontier,
    AwaitingOutgoingPcm,
}

#[derive(Clone, Copy)]
enum PromotionJoin {
    HardCut,
    Blend {
        frames: u64,
        outgoing_first: u64,
        spec: PcmSpec,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct OverlapSpan {
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

    /// One `DecoderEvent::TransitionHold` per transition: the first held
    /// pass announces it, later passes stay silent.
    pub(crate) fn announce_transition_hold(&mut self) -> bool {
        let Some(transition) = self.incoming.as_ref().map(IncomingDecode::transition) else {
            return false;
        };
        if self.announced_hold == Some(transition) {
            return false;
        }
        self.announced_hold = Some(transition);
        true
    }

    pub(crate) fn observe_source_exhaustion(&mut self) {
        if self.active.is_source_exhausted() {
            self.active.observe_exhaustion();
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

    pub(crate) fn flush_incoming_reader_signals(&mut self) {
        match self.incoming.as_mut() {
            Some(IncomingDecode::Priming { generation, .. })
            | Some(IncomingDecode::Failed { generation, .. }) => {
                generation.decoder_mut().flush_reader_signals();
            }
            Some(IncomingDecode::Preparing { .. } | IncomingDecode::Building { .. }) | None => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn incoming_is_building(&self, transition: VariantTransition) -> bool {
        matches!(
            self.incoming,
            Some(IncomingDecode::Building {
                transition: current,
                ..
            }) if current == transition
        )
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
    pub(crate) fn incoming_staged_span(&self) -> Option<(u64, u64, PcmSpec)> {
        let IncomingDecode::Priming { generation, .. } = self.incoming.as_ref()? else {
            return None;
        };
        generation.staged_span()
    }

    pub(crate) fn incoming_frontier(&self) -> Option<OutgoingFrontier> {
        match self.incoming.as_ref()? {
            IncomingDecode::Preparing { frontier, .. }
            | IncomingDecode::Building { frontier, .. }
            | IncomingDecode::Priming { frontier, .. } => Some(*frontier),
            IncomingDecode::Failed { .. } => None,
        }
    }

    pub(crate) fn install_incoming(
        &mut self,
        transition: VariantTransition,
        build: BuildId,
        generation: DecoderGeneration,
    ) -> Option<DecoderGeneration> {
        let Some(IncomingDecode::Building {
            transition: current,
            build: current_build,
            frontier,
        }) = self.incoming.as_ref()
        else {
            return Some(generation);
        };
        if *current != transition || *current_build != build {
            return Some(generation);
        }
        let frontier = *frontier;
        self.prepare_incoming_profile(generation.blender_profile());
        self.incoming = Some(IncomingDecode::Priming {
            transition,
            generation,
            frontier,
        });
        None
    }

    /// Content time an incoming variant must be landed at to be spliceable.
    ///
    /// The audible playhead is not that time: it trails the decode frontier by
    /// the whole PCM ring, and a reader that pulls faster than real time can
    /// open the gap without bound. An incoming landed behind the frontier has
    /// to decode the entire gap before it can cover it, and it decodes no
    /// faster than the outgoing does — so the gap never closes and the switch
    /// never commits. Only [`OutgoingFrontier::Exact`] names a frame; the other
    /// two states carry no frontier to land against and the source keeps its
    /// own seek-derived target.
    ///
    /// The frontier itself, and never ahead of it. A promotion proof is minted
    /// when the outgoing walks *into* the staged span, and the outgoing stops
    /// decoding the moment its PCM ring is full — so a landing placed ahead of
    /// the frontier is a bet on motion the outgoing owes nobody, and a consumer
    /// that stops pulling wedges the switch for good. Landing on the frontier
    /// removes the bet: priming extends the staged *end*, and the end only has
    /// to overtake a frontier that moves no faster than the incoming decodes.
    pub(crate) fn landing_for(&self, outgoing_frontier: OutgoingFrontier) -> Option<Duration> {
        let OutgoingFrontier::Exact { frame, rate } = outgoing_frontier else {
            return None;
        };
        let origin = self.active.timeline_origin(self.gapless_mode());
        let active_rate = self.active.blender_profile().spec().sample_rate.get();
        content_time(rate, frame, active_rate, origin)
    }

    pub(crate) fn mark_incoming_building(
        &mut self,
        transition: VariantTransition,
        build: BuildId,
    ) -> bool {
        if !self.incoming_is_preparing(transition) {
            return false;
        }
        let Some(IncomingDecode::Preparing { frontier, .. }) = self.incoming.as_ref() else {
            return false;
        };
        let frontier = *frontier;
        self.incoming = Some(IncomingDecode::Building {
            transition,
            build,
            frontier,
        });
        true
    }

    pub(crate) fn prime_incoming(&mut self, outgoing_frontier: OutgoingFrontier) -> IncomingPrime {
        let gapless_mode = self.gapless_mode();
        let outgoing_origin = self.active.timeline_origin(gapless_mode);
        let outgoing_rate = self.active.blender_profile().spec().sample_rate.get();
        let active_profile = self.active.gapless_profile();
        let active_gap = self.active.timeline_gap();
        let active = &self.active;
        let blender = &self.blender;
        let Some(IncomingDecode::Priming {
            frontier,
            generation,
            ..
        }) = self.incoming.as_mut()
        else {
            return IncomingPrime::Idle;
        };
        if *frontier == OutgoingFrontier::Awaiting
            && outgoing_frontier != OutgoingFrontier::Awaiting
        {
            *frontier = outgoing_frontier;
        }
        let mut latched_frontier = *frontier;
        let incoming_origin =
            incoming_origin_from(active_profile, active_gap, generation, gapless_mode);
        let mut readiness = promotion_readiness(
            active,
            blender,
            generation,
            latched_frontier,
            outgoing_origin,
            incoming_origin,
        );
        if matches!(readiness, PromotionReadiness::AwaitingOutgoingFrontier)
            && observed_frontier_is_later(
                latched_frontier,
                outgoing_frontier,
                outgoing_rate,
                outgoing_origin,
            )
        {
            *frontier = outgoing_frontier;
            latched_frontier = outgoing_frontier;
            readiness = promotion_readiness(
                active,
                blender,
                generation,
                latched_frontier,
                outgoing_origin,
                incoming_origin,
            );
        }
        match readiness {
            PromotionReadiness::Ready(_) => return IncomingPrime::Ready,
            PromotionReadiness::AwaitingOutgoingFrontier
            | PromotionReadiness::AwaitingOutgoingPcm => {
                return IncomingPrime::Pending;
            }
            PromotionReadiness::NeedIncoming => {}
        }
        let mut outcome = IncomingPrime::Pending;
        for _ in 0..Consts::PRIME_STEPS_PER_PASS {
            outcome = match generation.next_chunk() {
                Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                    let epoch = generation.installed_at_seek_epoch();
                    let Some(chunk) = apply_skip(chunk, epoch, generation.pending_head_skip_mut())
                    else {
                        continue;
                    };
                    if !chunk.samples.is_empty() {
                        generation.stage(chunk);
                    }
                    match promotion_readiness(
                        active,
                        blender,
                        generation,
                        latched_frontier,
                        outgoing_origin,
                        incoming_origin,
                    ) {
                        PromotionReadiness::Ready(_) => IncomingPrime::Ready,
                        PromotionReadiness::AwaitingOutgoingFrontier
                        | PromotionReadiness::AwaitingOutgoingPcm => IncomingPrime::Pending,
                        PromotionReadiness::NeedIncoming => {
                            outcome = IncomingPrime::Advanced;
                            continue;
                        }
                    }
                }
                Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                    IncomingPrime::Failed
                }
                Ok(DecoderChunkOutcome::Pending(_)) => IncomingPrime::Pending,
                Ok(DecoderChunkOutcome::Eof) => {
                    generation.finish_staging();
                    match promotion_readiness(
                        active,
                        blender,
                        generation,
                        latched_frontier,
                        outgoing_origin,
                        incoming_origin,
                    ) {
                        PromotionReadiness::Ready(_) => IncomingPrime::Ready,
                        PromotionReadiness::AwaitingOutgoingFrontier
                        | PromotionReadiness::AwaitingOutgoingPcm => IncomingPrime::Pending,
                        PromotionReadiness::NeedIncoming => {
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
                Err(_) => IncomingPrime::Failed,
            };
            break;
        }
        if outcome == IncomingPrime::Failed
            && let Some(IncomingDecode::Priming {
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
        outcome
    }
}

fn promotion_readiness(
    active: &DecoderGeneration,
    blender: &PcmBlender,
    generation: &DecoderGeneration,
    outgoing_frontier: OutgoingFrontier,
    outgoing_origin: u64,
    incoming_origin: u64,
) -> PromotionReadiness {
    if !blender.is_steady() {
        return PromotionReadiness::NeedIncoming;
    }
    let Some((incoming_first, incoming_end, incoming_spec)) = generation.staged_span() else {
        return PromotionReadiness::NeedIncoming;
    };
    let incoming_rate = incoming_spec.sample_rate.get();
    let (outgoing_frame, outgoing_rate) =
        match resolve_frontier(active, outgoing_frontier, incoming_first) {
            Ok(pair) => pair,
            Err(verdict) => return verdict,
        };
    let active_spec = active.blender_profile().spec();
    let active_rate = active_spec.sample_rate.get();
    let Some(incoming_first_time) = content_time(
        incoming_rate,
        incoming_first,
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
    let Some(outgoing_next) =
        content_time(outgoing_rate, outgoing_frame, active_rate, outgoing_origin)
    else {
        return PromotionReadiness::NeedIncoming;
    };
    if incoming_first_time > outgoing_next {
        if active.is_source_exhausted() {
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
            outgoing_origin,
            outgoing_rate,
            "incoming generation is waiting for the outgoing frontier"
        );
        return PromotionReadiness::AwaitingOutgoingFrontier;
    }
    if outgoing_next >= incoming_end_time {
        if active.is_source_exhausted() && generation.is_finished() {
            // Nothing exists past the cut on either side: the outgoing ran
            // out of source at the frontier and the incoming ran out at or
            // before it. Commit the switch at the end with an empty incoming
            // tail instead of failing it as unlandable — the intent resolves
            // and the track then ends on the promoted variant.
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
    let Some(incoming_next) = frame_at_or_after(incoming_rate, incoming_origin, outgoing_next)
    else {
        return PromotionReadiness::NeedIncoming;
    };
    if incoming_first > incoming_next || incoming_next >= incoming_end {
        return PromotionReadiness::NeedIncoming;
    }
    let join = if incoming_spec == active_spec {
        let Some(outgoing_first) = frame_at_or_after(active_rate, outgoing_origin, outgoing_next)
        else {
            return PromotionReadiness::NeedIncoming;
        };
        match same_spec_join(
            active,
            blender,
            generation,
            active_spec,
            incoming_next,
            outgoing_first,
            outgoing_next,
        ) {
            Ok(join) => join,
            Err(verdict) => return verdict,
        }
    } else {
        PromotionJoin::HardCut
    };
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
        outgoing_origin,
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

fn resolve_frontier(
    active: &DecoderGeneration,
    frontier: OutgoingFrontier,
    incoming_first: u64,
) -> Result<(u64, u32), PromotionReadiness> {
    match frontier {
        // An exhausted outgoing can never establish (or advance) a frontier,
        // so every "wait for the outgoing" answer below is a dead end; the
        // switch degrades to a hard cut instead of wedging forever.
        OutgoingFrontier::Awaiting if !active.is_source_exhausted() => {
            Err(PromotionReadiness::NeedIncoming)
        }
        OutgoingFrontier::Awaiting | OutgoingFrontier::Unavailable => {
            Err(hard_cut_at(incoming_first, incoming_first))
        }
        OutgoingFrontier::Exact { frame, rate } => Ok((frame, rate)),
    }
}

fn same_spec_join(
    active: &DecoderGeneration,
    blender: &PcmBlender,
    generation: &DecoderGeneration,
    spec: PcmSpec,
    incoming_next: u64,
    outgoing_first: u64,
    outgoing_next: Duration,
) -> Result<PromotionJoin, PromotionReadiness> {
    let frames = blender.join_frame_count();
    if !generation.staged_covers(spec, incoming_next, frames) {
        trace!(
            frames,
            incoming_next,
            outgoing_first,
            ?outgoing_next,
            "same-spec promotion is waiting for incoming join PCM"
        );
        return Err(PromotionReadiness::NeedIncoming);
    }
    if active.staged_covers(spec, outgoing_first, frames) {
        return Ok(PromotionJoin::Blend {
            frames,
            outgoing_first,
            spec,
        });
    }
    if active.is_source_exhausted() {
        // The join PCM would have to come from past the final decode
        // head — it does not exist. Cut at the frontier instead of
        // demanding PCM the outgoing can never produce.
        debug!(
            frames,
            incoming_next,
            outgoing_first,
            "outgoing exhausted without join PCM: hard cut at the final frontier"
        );
        return Ok(PromotionJoin::HardCut);
    }
    trace!(
        frames,
        incoming_next,
        outgoing_first,
        ?outgoing_next,
        "same-spec promotion is waiting for outgoing join PCM"
    );
    if active.is_finished() {
        Err(PromotionReadiness::NeedIncoming)
    } else {
        Err(PromotionReadiness::AwaitingOutgoingPcm)
    }
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
        return frame
            .checked_sub(origin)
            .map(|frames| duration_for_frames(rate, frames));
    }
    duration_for_frames(rate, frame).checked_sub(duration_for_frames(origin_rate, origin))
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
    let mut frame = u64::try_from(frames_for_duration(rate, time)).ok()?;
    if duration_for_frames(rate, frame) < time {
        frame = frame.checked_add(1)?;
    }
    origin.checked_add(frame)
}

fn trim_staged_head(generation: &mut DecoderGeneration, overlap: OverlapSpan) -> bool {
    let mut remaining = overlap.incoming_next.saturating_sub(overlap.incoming_first);
    if remaining == 0 {
        return generation.has_output();
    }
    while remaining != 0 {
        let Some(chunk) = generation.pop_staged() else {
            return false;
        };
        if let Some(chunk) = apply_frames(chunk, &mut remaining) {
            generation.push_staged_front(chunk);
        }
    }
    generation.has_output()
}

fn incoming_origin(
    active: &DecoderGeneration,
    incoming: &DecoderGeneration,
    mode: kithara_decode::GaplessMode,
) -> u64 {
    incoming_origin_from(
        active.gapless_profile(),
        active.timeline_gap(),
        incoming,
        mode,
    )
}

fn incoming_origin_from(
    active_profile: kithara_decode::GaplessProfile,
    active_gap: u64,
    incoming: &DecoderGeneration,
    mode: kithara_decode::GaplessMode,
) -> u64 {
    let incoming_profile = incoming.gapless_profile();
    let gap = if shares_default_profile(active_profile, incoming_profile) {
        active_gap.max(incoming.timeline_gap())
    } else {
        incoming.timeline_gap()
    };
    let origin = incoming.timeline_origin_with_gap(mode, gap);
    // The seam moves by exactly the AAC-LC default priming when the incoming
    // track's own gapless metadata is missing: `origin` alone cannot say which
    // of the two it is, so record the profile that produced it.
    debug!(
        origin,
        gap,
        gapless_known = incoming_profile.gapless().is_some(),
        default_priming = incoming_profile.default_priming_frames(),
        ?mode,
        "incoming timeline origin resolved"
    );
    origin
}

fn shares_default_profile(
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
