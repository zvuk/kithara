use std::num::NonZeroU32;

use kithara_stream::{
    StreamType, VariantControl, VariantPromotion, VariantReaderTake, VariantTransition,
};
use tracing::{debug, trace, warn};

use super::core::{
    OwnerPhase, StreamAudioSource, initial_promotion_frontier, promotion_frontier_for,
};
use crate::{
    DecoderChangeCause, WaitingReason,
    pipeline::decode::{
        DecoderGeneration,
        event::{GenerationInstalled, enqueue_generation_installed},
        transition::{IncomingPrime, OutgoingFrontier},
    },
};

impl<T: StreamType> StreamAudioSource<T> {
    pub(super) fn prepare_incoming_transition(
        &mut self,
        control: &dyn VariantControl,
        outgoing_frontier: OutgoingFrontier,
    ) -> Option<VariantTransition> {
        let plan = match control.plan_variant_reader(self.decode.landing_for(outgoing_frontier)) {
            Ok(plan) => plan,
            Err(error) => {
                warn!(?error, "failed to plan exact incoming variant reader");
                if let Some(transition) = self.decode.incoming_transition() {
                    self.abort_local_incoming(control, transition);
                }
                return None;
            }
        };
        let Some(plan) = plan else {
            self.discard_local_incoming();
            return None;
        };
        let transition = plan.transition();
        if self.decode.incoming_transition() != Some(transition)
            && let Some(generation) = self
                .decode
                .begin_incoming(transition, initial_promotion_frontier(transition))
        {
            drop(generation);
        }
        if !self.decode.incoming_is_preparing(transition) {
            return None;
        }

        let byte_map = self.shared_stream.byte_map();
        let profile = self
            .factory
            .reader_profile(plan.media_info(), byte_map.as_deref());
        match control.prepare_variant_reader(plan, profile) {
            Ok(Some(prepared)) if prepared == transition => Some(transition),
            Ok(Some(prepared)) => {
                warn!(
                    ?transition,
                    ?prepared,
                    "source prepared a different exact variant transition"
                );
                self.abort_local_incoming(control, transition);
                None
            }
            Ok(None) => {
                self.discard_local_incoming();
                None
            }
            Err(error) => {
                warn!(
                    ?error,
                    ?transition,
                    "failed to prepare exact incoming reader"
                );
                self.abort_local_incoming(control, transition);
                None
            }
        }
    }

    /// A reader starved on the outgoing variant keeps advancing an already-requested transition;
    /// the transition itself owns whether that source remains part of the promotion proof.
    pub(super) fn progress_variant_transition(&mut self) {
        match &self.phase {
            OwnerPhase::Decoding => {}
            OwnerPhase::AtEof | OwnerPhase::Failed { .. } => {
                if let (Some(control), Some(transition)) = (
                    self.variant_control.clone(),
                    self.decode.incoming_transition(),
                ) {
                    debug!(
                        at_eof = matches!(self.phase, OwnerPhase::AtEof),
                        latched_frontier = ?self.decode.incoming_frontier(),
                        landing = ?self.resume.decode_head(),
                        ?transition,
                        "outgoing ended: aborting variant transition"
                    );
                    self.abort_local_incoming(control.as_ref(), transition);
                }
                return;
            }
        }
        let Some(control) = self.variant_control.clone() else {
            return;
        };

        let landing_frontier = match self.resume.decode_head() {
            Some((frame, rate)) => OutgoingFrontier::Exact { frame, rate },
            None => OutgoingFrontier::Awaiting,
        };
        self.retire_failed_incoming(control.as_ref());
        let observed_frontier = self
            .decode
            .incoming_transition()
            .map_or(landing_frontier, |transition| {
                promotion_frontier_for(transition, landing_frontier)
            });
        let prime = self.decode.prime_incoming(observed_frontier);
        if let Some(incoming) = self.decode.incoming_transition() {
            trace!(
                ?landing_frontier,
                ?observed_frontier,
                latched_frontier = ?self.decode.incoming_frontier(),
                ?prime,
                ?incoming,
                "variant transition pass"
            );
        }
        if prime == IncomingPrime::Advanced {
            self.wake.wake();
        }
        if !self.promote_ready_incoming(control.as_ref()) {
            return;
        }
        let Some(transition) = self.prepare_incoming_transition(control.as_ref(), landing_frontier)
        else {
            return;
        };
        self.take_prepared_incoming(control.as_ref(), transition);
    }

    pub(super) fn promote_ready_incoming(&mut self, control: &dyn VariantControl) -> bool {
        let Some(prepared) = self.decode.prepare_promotion() else {
            return true;
        };
        let transition = prepared.transition();
        match control.promote_variant(transition) {
            VariantPromotion::Promoted => {
                let outgoing = self.decode.commit_prepared_promotion(prepared);
                {
                    let emit = &self.emit;
                    enqueue_generation_installed(
                        emit,
                        &GenerationInstalled {
                            backend: self.decoder_backend,
                            cause: DecoderChangeCause::VariantSwitch,
                            generation: self.decode.active(),
                            host_sample_rate: self.host_rate.map_or(0, NonZeroU32::get),
                            playback_resampler_backend: self.playback_resampler_backend,
                            recreates_on_route: true,
                        },
                    );
                }
                drop(outgoing);
                self.wake.wake();
                true
            }
            VariantPromotion::Deferred => {
                self.decode.restore_prepared_promotion(prepared);
                false
            }
            VariantPromotion::Stale => {
                drop(DecoderGeneration::from(prepared));
                true
            }
            _ => {
                warn!(
                    ?transition,
                    "source returned an unsupported variant promotion result"
                );
                self.decode.restore_prepared_promotion(prepared);
                false
            }
        }
    }

    pub(super) fn retire_failed_incoming(&mut self, control: &dyn VariantControl) {
        if let Some((transition, generation)) = self.decode.take_failed_incoming() {
            drop(generation);
            let _ = control.abort_variant(transition);
        }
    }

    pub(super) fn take_prepared_incoming(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
    ) {
        match control.take_prepared_variant_reader(transition) {
            Ok(VariantReaderTake::Preparing) => {}
            Ok(VariantReaderTake::Ready(reader)) => {
                self.start_incoming_build(control, transition, reader);
            }
            Ok(VariantReaderTake::Taken) => {
                warn!(
                    ?transition,
                    "incoming reader was transferred without a matching decoder build"
                );
                self.abort_local_incoming(control, transition);
            }
            Ok(VariantReaderTake::Stale) => self.discard_local_incoming(),
            Err(error) => {
                warn!(?error, ?transition, "failed to take exact incoming reader");
                self.abort_local_incoming(control, transition);
            }
            Ok(_) => {
                warn!(
                    ?transition,
                    "source returned an unsupported incoming reader state"
                );
                self.abort_local_incoming(control, transition);
            }
        }
    }

    /// Hang classification for a transition-pending decode tick: a pending
    /// transition whose incoming byte is serviced by an in-flight fetch is
    /// upstream work (`WaitingDemand`, watchdog-quiet); one with nothing in
    /// flight stays `Waiting` so a wedged switch still surfaces as a hang.
    pub(crate) fn transition_wait_reason(&self) -> WaitingReason {
        let demand_backed = self
            .variant_control
            .as_deref()
            .zip(self.decode.incoming_transition())
            .is_some_and(|(control, transition)| control.transition_demand_in_flight(transition));
        if demand_backed {
            WaitingReason::WaitingDemand
        } else {
            WaitingReason::Waiting
        }
    }
}
