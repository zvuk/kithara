use kithara_decode::{DecodeError, DecoderChunkOutcome, ErrorClass};
use kithara_stream::{PendingReason, SourcePhase, StreamType};
use kithara_test_utils::kithara;

use crate::{
    DecoderEvent,
    audio::event::{decode_error_detail, map_decode_error_class, map_decode_error_kind},
    pipeline::{
        decode::{
            core::{ActiveDecode, DecodeAction, DecodeCtx},
            format::{FormatDecision, detect},
        },
        fetch::Fetch,
        seek::skip::apply as apply_skip,
        track::WaitingReason,
    },
};

/// Surfacing EOF while a switch is in flight but the output hold is not yet engaged would latch
/// `AtEof` and abort the pending intent, so this parks as a transition wait instead and finalizes
/// one tick later.
#[kithara::measure(label = "audio.decode.step")]
#[kithara::hang_watchdog]
pub(crate) fn tick<T: StreamType>(
    core: &mut ActiveDecode,
    mut ctx: DecodeCtx<'_, T>,
) -> DecodeAction {
    let mut decoded = false;
    loop {
        hang_tick!();
        if let Some(error) = core.take_stage_error() {
            return decode_failed(core, error, &ctx);
        }
        match core.next_output(&mut *ctx.cursor, ctx.physical_duration) {
            Ok(Some(chunk)) => return DecodeAction::Produced(Box::new(Fetch::data(chunk))),
            Ok(None) => {}
            Err(error) => return decode_failed(core, error, &ctx),
        }
        if core.transition_holds_output() && !core.outgoing_holdback_needs_pcm() {
            return transition_hold(core, &ctx);
        }
        if core.active().is_source_exhausted() {
            return finish_exhausted(core, &mut ctx);
        }
        if decoded {
            return DecodeAction::Progress;
        }
        decoded = true;
        kithara::probe_event!(decode_step);
        match core.next_chunk(ctx.stream.position()) {
            Ok(DecoderChunkOutcome::Pending(PendingReason::SessionRetired)) => {
                return DecodeAction::ReaderRetired;
            }
            Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                return variant_change(core, &ctx);
            }
            Ok(DecoderChunkOutcome::Pending(PendingReason::Retry)) => {
                return DecodeAction::Progress;
            }
            Ok(DecoderChunkOutcome::Pending(_)) => {
                let reason = match ctx.stream.phase() {
                    SourcePhase::WaitingDemand => WaitingReason::WaitingDemand,
                    SourcePhase::WaitingMetadata => WaitingReason::WaitingMetadata,
                    _ => WaitingReason::Waiting,
                };
                return DecodeAction::Pending(reason);
            }
            Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                let chunk = match apply_skip(*chunk, core.active.pending_head_skip_mut()) {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => continue,
                    Err(error) => return decode_failed(core, error, &ctx),
                };
                if chunk.samples.is_empty() {
                    continue;
                }
                hang_reset!();
                core.track(&chunk, ctx.emit);
                if let Err(error) = core.push(chunk) {
                    return decode_failed(core, error, &ctx);
                }
            }
            Ok(DecoderChunkOutcome::Eof) => {
                core.mark_source_exhausted();
            }
            Err(error) if error.classify() == ErrorClass::VariantChange => {
                return variant_change(core, &ctx);
            }
            Err(error) => return decode_failed(core, error, &ctx),
        }
    }
}

fn finish_exhausted<T: StreamType>(
    core: &mut ActiveDecode,
    ctx: &mut DecodeCtx<'_, T>,
) -> DecodeAction {
    if core.has_live_incoming() {
        return transition_hold(core, ctx);
    }
    if !core.active().is_exhaustion_observed() {
        core.observe_source_exhaustion();
        return DecodeAction::Progress;
    }
    if let Err(error) = core.finish_active() {
        return decode_failed(core, error, ctx);
    }
    match core.next_output_unheld(&mut *ctx.cursor, ctx.physical_duration) {
        Ok(Some(chunk)) => return DecodeAction::Produced(Box::new(Fetch::data(chunk))),
        Ok(None) => {}
        Err(error) => return decode_failed(core, error, ctx),
    }
    if let FormatDecision::Recreate(recreate) = detect(ctx.stream, core.active()) {
        return DecodeAction::StartRecreate(recreate);
    }
    DecodeAction::Eof
}

fn transition_hold<T: StreamType>(core: &mut ActiveDecode, ctx: &DecodeCtx<'_, T>) -> DecodeAction {
    if core.announce_transition_hold()
        && let Some(emit) = ctx.emit
    {
        emit.enqueue(DecoderEvent::TransitionHold {
            source_exhausted: core.active().is_source_exhausted(),
        });
    }
    DecodeAction::TransitionPending
}

fn decode_failed<T: StreamType>(
    core: &ActiveDecode,
    error: DecodeError,
    ctx: &DecodeCtx<'_, T>,
) -> DecodeAction {
    if let Some(emit) = ctx.emit {
        emit.enqueue(DecoderEvent::DecodeError {
            class: map_decode_error_class(error.classify()),
            kind: map_decode_error_kind(&error),
            codec: core.active().media_info().and_then(|info| info.codec),
            detail: decode_error_detail(&error),
        });
    }
    DecodeAction::Failed(error)
}

pub(crate) fn variant_change<T: StreamType>(
    core: &ActiveDecode,
    ctx: &DecodeCtx<'_, T>,
) -> DecodeAction {
    match detect(ctx.stream, core.active()) {
        FormatDecision::Recreate(recreate) => DecodeAction::StartRecreate(recreate),
        FormatDecision::None => DecodeAction::Pending(WaitingReason::Waiting),
    }
}
