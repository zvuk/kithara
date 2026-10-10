use kithara_stream::VariantTransition;

use super::transition::{
    IncomingDecode, OutgoingFrontier, PreparedPromotion, PromotionJoin, PromotionReadiness,
    incoming_origin, promotion_readiness, shares_default_profile, trim_staged_head,
};
use crate::pipeline::decode::{core::ActiveDecode, generation::DecoderGeneration};

impl ActiveDecode {
    pub(crate) fn commit_prepared_promotion(
        &mut self,
        mut prepared: PreparedPromotion,
    ) -> DecoderGeneration {
        if shares_default_profile(
            self.active.gapless_profile(),
            prepared.generation.gapless_profile(),
        ) {
            prepared
                .generation
                .align_timeline_gap(self.active.timeline_gap());
        }
        let incoming_profile = prepared.generation.blender_profile();
        match prepared.join {
            PromotionJoin::HardCut => self.blender.replace_active(incoming_profile),
            PromotionJoin::Blend { .. } => self.blender.commit_join(),
        }
        std::mem::replace(&mut self.active, prepared.generation)
    }

    pub(crate) fn outgoing_holdback_needs_pcm(&self) -> bool {
        let Some(IncomingDecode::Priming {
            frontier,
            generation,
            ..
        }) = self.incoming.as_ref()
        else {
            return false;
        };
        let gapless_mode = self.gapless_mode();
        let outgoing_origin = self.active.timeline_origin(gapless_mode);
        let incoming_origin = incoming_origin(&self.active, generation, gapless_mode);
        matches!(
            promotion_readiness(
                &self.active,
                &self.blender,
                generation,
                *frontier,
                outgoing_origin,
                incoming_origin,
            ),
            PromotionReadiness::AwaitingOutgoingPcm
        )
    }

    /// A finished incoming may trim to an empty tail: the end-of-track hard cut proves nothing
    /// exists past the cut, and an empty tail is exactly consistent with that proof.
    pub(crate) fn prepare_promotion(&mut self) -> Option<PreparedPromotion> {
        let (transition, span) = {
            let IncomingDecode::Priming {
                transition,
                generation,
                frontier,
            } = self.incoming.as_ref()?
            else {
                return None;
            };
            let gapless_mode = self.gapless_mode();
            let outgoing_origin = self.active.timeline_origin(gapless_mode);
            let incoming_origin = incoming_origin(&self.active, generation, gapless_mode);
            let PromotionReadiness::Ready(span) = promotion_readiness(
                &self.active,
                &self.blender,
                generation,
                *frontier,
                outgoing_origin,
                incoming_origin,
            ) else {
                return None;
            };
            (*transition, span)
        };
        let IncomingDecode::Priming { mut generation, .. } = self.incoming.take()? else {
            return None;
        };
        let trimmed = match trim_staged_head(&mut generation, span.overlap) {
            Ok(trimmed) => trimmed,
            Err(error) => {
                tracing::warn!(?error, "incoming promotion mapping failed");
                self.incoming = Some(IncomingDecode::Failed {
                    transition,
                    generation,
                });
                return None;
            }
        };
        assert!(
            trimmed || generation.is_finished(),
            "BUG: a minted incoming promotion no longer covers its proven cut"
        );
        if let PromotionJoin::Blend {
            frames,
            outgoing_first,
            spec,
        } = span.join
        {
            let active = &self.active;
            let copied = self.blender.prepare_join(|outgoing| {
                active.copy_staged_frames(spec, outgoing_first, frames, outgoing)
            });
            assert!(
                copied,
                "BUG: a minted incoming promotion lost its proven outgoing PCM"
            );
        }
        Some(PreparedPromotion {
            generation,
            transition,
            join: span.join,
        })
    }

    pub(crate) fn restore_prepared_promotion(&mut self, prepared: PreparedPromotion) {
        self.incoming = Some(IncomingDecode::Priming {
            transition: prepared.transition,
            generation: prepared.generation,
            frontier: OutgoingFrontier::Awaiting,
        });
    }

    pub(crate) fn take_failed_incoming(
        &mut self,
    ) -> Option<(VariantTransition, DecoderGeneration)> {
        let incoming = self.incoming.take()?;
        match incoming {
            IncomingDecode::Failed {
                transition,
                generation,
            } => Some((transition, generation)),
            incoming => {
                self.incoming = Some(incoming);
                None
            }
        }
    }

    pub(crate) fn transition_holds_output(&self) -> bool {
        if self.active.is_finished() || !self.blender.is_steady() {
            return false;
        }
        let Some(IncomingDecode::Priming {
            frontier,
            generation,
            ..
        }) = self.incoming.as_ref()
        else {
            return false;
        };
        if *frontier == OutgoingFrontier::Awaiting {
            return false;
        }
        let gapless_mode = self.gapless_mode();
        let outgoing_origin = self.active.timeline_origin(gapless_mode);
        let incoming_origin = incoming_origin(&self.active, generation, gapless_mode);
        !matches!(
            promotion_readiness(
                &self.active,
                &self.blender,
                generation,
                *frontier,
                outgoing_origin,
                incoming_origin,
            ),
            PromotionReadiness::AwaitingOutgoingFrontier
        )
    }
}
