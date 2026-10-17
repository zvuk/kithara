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
        if self.active.blender_profile().spec() != incoming_profile.spec()
            || !self.active.has_same_source_basis(&prepared.generation)
            || self.active.media_origin() != prepared.generation.media_origin()
            || self.active.comparison_origin(self.gapless_mode())
                != incoming_origin(&self.active, &prepared.generation, self.gapless_mode())
        {
            self.discontinuity_revision = self.discontinuity_revision.wrapping_add(1);
        }
        match prepared.join {
            PromotionJoin::HardCut => self.blender.replace_active(incoming_profile),
            PromotionJoin::Blend { .. } => self.blender.commit_join(),
        }
        std::mem::replace(&mut self.active, prepared.generation)
    }

    pub(crate) fn outgoing_holdback_needs_pcm(&self) -> bool {
        self.outgoing_holdback_cut().is_some()
    }

    pub(super) fn outgoing_holdback_cut(&self) -> Option<u64> {
        let Some(IncomingDecode::Priming {
            frontier,
            generation,
            ..
        }) = self.incoming.as_ref()
        else {
            return None;
        };
        let gapless_mode = self.gapless_mode();
        let (outgoing_origin, incoming_origin) = self
            .active
            .comparison_origin(gapless_mode)
            .zip(incoming_origin(&self.active, generation, gapless_mode))?;
        match promotion_readiness(
            &self.active,
            &self.blender,
            generation,
            *frontier,
            outgoing_origin,
            incoming_origin,
            gapless_mode,
        ) {
            PromotionReadiness::AwaitingOutgoingPcm { first } => Some(first),
            _ => None,
        }
    }

    /// A finished incoming may trim to an empty tail: the end-of-track hard cut proves nothing
    /// exists past the cut, and an empty tail is exactly consistent with that proof.
    pub(crate) fn prepare_promotion(&mut self) -> Option<PreparedPromotion> {
        let (transition, span, incoming_end) = {
            let IncomingDecode::Priming {
                transition,
                generation,
                frontier,
                ..
            } = self.incoming.as_ref()?
            else {
                return None;
            };
            let gapless_mode = self.gapless_mode();
            let outgoing_origin = self.active.comparison_origin(gapless_mode)?;
            let incoming_origin = incoming_origin(&self.active, generation, gapless_mode)?;
            let PromotionReadiness::Ready(span) = promotion_readiness(
                &self.active,
                &self.blender,
                generation,
                *frontier,
                outgoing_origin,
                incoming_origin,
                gapless_mode,
            ) else {
                return None;
            };
            let last = generation.staged.back()?.meta;
            let incoming_end = last.source_span.map_or_else(
                || crate::SourceEnd::new(last.frame_range().end, last.spec.sample_rate),
                |span| {
                    crate::SourceEnd::new(span.end(), span.sample_rate())
                        .with_mapping_revision(span.mapping_revision())
                },
            );
            (*transition, span, incoming_end)
        };
        let IncomingDecode::Priming {
            mut generation,
            preparation,
            ..
        } = self.incoming.take()?
        else {
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
        let resume_head = generation.staged.front().map_or(incoming_end, |first| {
            first.meta.source_span.map_or_else(
                || crate::SourceEnd::new(first.meta.frame_offset, first.meta.spec.sample_rate),
                |span| {
                    crate::SourceEnd::new(span.start(), span.sample_rate())
                        .with_mapping_revision(span.mapping_revision())
                },
            )
        });
        Some(PreparedPromotion {
            generation,
            resume_head,
            transition,
            join: span.join,
            preparation,
        })
    }

    pub(crate) fn restore_prepared_promotion(&mut self, prepared: PreparedPromotion) {
        self.incoming = Some(IncomingDecode::Priming {
            transition: prepared.transition,
            generation: prepared.generation,
            frontier: OutgoingFrontier::Awaiting,
            preparation: prepared.preparation,
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
        let Some((outgoing_origin, incoming_origin)) = self
            .active
            .comparison_origin(gapless_mode)
            .zip(incoming_origin(&self.active, generation, gapless_mode))
        else {
            return false;
        };
        !matches!(
            promotion_readiness(
                &self.active,
                &self.blender,
                generation,
                *frontier,
                outgoing_origin,
                incoming_origin,
                gapless_mode,
            ),
            PromotionReadiness::AwaitingOutgoingFrontier
        )
    }
}

pub(crate) mod preparation {
    use kithara_decode::{
        DecodeResult, DecoderChunkOutcome, GaplessMode, GaplessProfile, PrepareWait,
    };
    use kithara_platform::sync::mpsc;
    use kithara_signal::AudioSpec;
    use kithara_stream::PendingReason;

    use super::super::transition::{
        IncomingPrime, OutgoingFrontier, PromotionReadiness, classify_incoming_preparation,
        incoming_origin_from, incoming_readiness,
    };
    use crate::{
        consts,
        pipeline::{decode::DecoderGeneration, seek::skip::apply as apply_skip},
    };

    pub(crate) struct IncomingPreparationOwner {
        pub(crate) requests: mpsc::Sender<IncomingPreparationRequest>,
        pub(crate) completion: mpsc::Receiver<DecodeResult<PreparedIncoming>>,
    }

    pub(crate) struct IncomingPreparationRequest {
        pub(crate) generation: DecoderGeneration,
        pub(crate) target: IncomingPreparation,
    }

    pub(crate) struct PreparedIncoming {
        pub(crate) generation: DecoderGeneration,
        pub(crate) outcome: DecodeResult<IncomingPrime>,
    }

    impl IncomingPreparationRequest {
        pub(crate) fn prepare(mut self) -> PreparedIncoming {
            let outcome = self.generation.prepare_incoming(self.target);
            PreparedIncoming {
                generation: self.generation,
                outcome,
            }
        }
    }

    #[derive(Clone, Copy)]
    pub(crate) struct IncomingPreparation {
        pub(in crate::pipeline::decode) frontier: OutgoingFrontier,
        pub(in crate::pipeline::decode) outgoing_spec: AudioSpec,
        pub(in crate::pipeline::decode) outgoing_profile: GaplessProfile,
        pub(in crate::pipeline::decode) outgoing_gap: u64,
        pub(in crate::pipeline::decode) outgoing_origin: u64,
        pub(in crate::pipeline::decode) outgoing_exhausted: bool,
        pub(in crate::pipeline::decode) join_frames: u64,
        pub(in crate::pipeline::decode) gapless_mode: GaplessMode,
    }

    impl IncomingPreparation {
        fn readiness(&self, generation: &DecoderGeneration) -> Option<PromotionReadiness> {
            let origin = incoming_origin_from(
                self.outgoing_profile,
                self.outgoing_gap,
                generation,
                self.gapless_mode,
            )?;
            Some(incoming_readiness(generation, *self, origin))
        }

        fn stage_chunk(
            generation: &mut DecoderGeneration,
            chunk: kithara_signal::AudioChunk,
        ) -> DecodeResult<IncomingPrime> {
            match apply_skip(chunk, generation.pending_head_skip_mut()) {
                Ok(Some(chunk)) if !chunk.samples.is_empty() => generation.stage(chunk)?,
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    tracing::warn!(?error, "incoming source mapping failed");
                    return Ok(IncomingPrime::Failed);
                }
            }
            Ok(IncomingPrime::Advanced)
        }

        pub(in crate::pipeline::decode) fn prepare(
            &self,
            generation: &mut DecoderGeneration,
        ) -> DecodeResult<IncomingPrime> {
            for _ in 0..consts::PRIME_STEPS_PER_PASS {
                let Some(readiness) = self.readiness(generation) else {
                    return Ok(IncomingPrime::Failed);
                };
                match readiness {
                    PromotionReadiness::Ready(_) => return Ok(IncomingPrime::Ready),
                    PromotionReadiness::AwaitingOutgoingFrontier
                    | PromotionReadiness::AwaitingOutgoingPcm { .. } => {
                        return Ok(IncomingPrime::Pending);
                    }
                    PromotionReadiness::NeedIncoming => {}
                }
                let preparation = generation.prepare_deferred_with_wait(
                    true,
                    self.gapless_mode,
                    PrepareWait::Wait,
                );
                if let Some(outcome) = classify_incoming_preparation(&preparation) {
                    if outcome == IncomingPrime::Advanced {
                        continue;
                    }
                    return Ok(outcome);
                }
                match generation.next_chunk() {
                    Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                        if Self::stage_chunk(generation, *chunk)? == IncomingPrime::Failed {
                            return Ok(IncomingPrime::Failed);
                        }
                    }
                    Ok(DecoderChunkOutcome::Pending(
                        PendingReason::VariantChange | PendingReason::SessionRetired,
                    ))
                    | Err(_) => return Ok(IncomingPrime::Failed),
                    Ok(DecoderChunkOutcome::Pending(PendingReason::Retry)) => continue,
                    Ok(DecoderChunkOutcome::Pending(_)) => return Ok(IncomingPrime::Pending),
                    Ok(DecoderChunkOutcome::Eof) => {
                        generation.finish_staging()?;
                        return Ok(IncomingPrime::Pending);
                    }
                }
            }
            Ok(IncomingPrime::Advanced)
        }
    }

    #[cfg(test)]
    pub(crate) mod fixtures {
        use kithara_platform::sync::mpsc;
        use kithara_stream::VariantTransition;

        use super::{IncomingPreparationOwner, IncomingPreparationRequest};
        use crate::pipeline::decode::{DecoderGeneration, core::ActiveDecode};

        pub(crate) fn preparation_owner() -> IncomingPreparationOwner {
            let (requests, requested) = mpsc::channel::<IncomingPreparationRequest>();
            let (completed, completion) = mpsc::channel();
            drop(kithara_platform::thread::spawn(move || {
                while let Ok(request) = requested.recv() {
                    if completed.send(Ok(request.prepare())).is_err() {
                        return;
                    }
                }
            }));
            IncomingPreparationOwner {
                requests,
                completion,
            }
        }

        pub(crate) fn install(
            decode: &mut ActiveDecode,
            transition: VariantTransition,
            generation: DecoderGeneration,
        ) -> Option<DecoderGeneration> {
            if decode.incoming_is_preparing(transition) {
                assert!(decode.begin_incoming_build(transition, preparation_owner()));
            }
            decode.install_incoming(transition, generation)
        }
    }
}
