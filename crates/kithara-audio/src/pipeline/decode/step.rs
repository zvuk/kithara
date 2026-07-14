use kithara_decode::{DecodeError, DecoderChunkOutcome, ErrorClass, PcmChunk};
use kithara_events::{AudioEvent, DecoderEvent, SeekLifecycleStage, SegmentLocation};
use kithara_stream::{PendingReason, StreamType};
use kithara_test_utils::kithara;

use crate::{
    audio::event::{
        decode_error_detail, map_audio_codec_kind, map_decode_error_class, map_decode_error_kind,
    },
    pipeline::{
        decode::{
            core::{DecodeAction, DecodeCore, DecodeCtx},
            format::{FormatDecision, detect, handle_variant_change},
        },
        fetch::Fetch,
        gapless::visible_duration,
        seek::skip::apply as apply_skip,
        track::{TrackFailure, WaitingReason},
    },
};

struct Consts;

impl Consts {
    const VARIANT_CHANGE: &str = "variant change signal without observable format transition";
}

#[kithara::hang_watchdog]
pub(crate) fn tick<T: StreamType>(
    core: &mut DecodeCore,
    mut ctx: DecodeCtx<'_, T>,
) -> DecodeAction {
    if let Some(duration) = visible_duration(core.session().decoder.as_ref(), core.gapless_mode())
        && Some(duration) > ctx.playhead.duration()
    {
        ctx.playhead.set_duration(Some(duration));
    }
    let epoch = ctx.seek.epoch();
    loop {
        if ctx.seek_observe.is_flushing() || ctx.seek_observe.is_pending() {
            return DecodeAction::SeekInterrupted;
        }
        if let Some(chunk) = core.next_gapless() {
            return produced(chunk, epoch, &mut ctx);
        }
        match core.next_chunk(ctx.stream.position()) {
            Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                return variant_change(
                    core,
                    &ctx,
                    &DecodeError::InvalidData {
                        detail: Consts::VARIANT_CHANGE,
                    },
                );
            }
            Ok(DecoderChunkOutcome::Pending(_)) if ctx.stream.has_variant_change_pending() => {
                return variant_change(
                    core,
                    &ctx,
                    &DecodeError::InvalidData {
                        detail: Consts::VARIANT_CHANGE,
                    },
                );
            }
            Ok(DecoderChunkOutcome::Pending(_)) => {
                return DecodeAction::Pending(WaitingReason::Waiting);
            }
            Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                let Some(chunk) = apply_skip(chunk, epoch, ctx.resume.as_deref_mut()) else {
                    continue;
                };
                if chunk.samples.is_empty() {
                    continue;
                }
                hang_reset!();
                core.track(&chunk, ctx.playhead, ctx.emit);
                core.push(chunk);
            }
            Ok(DecoderChunkOutcome::Eof) => {
                core.set_tail_compensation();
                if let Some(chunk) = core.next_gapless() {
                    return produced(chunk, epoch, &mut ctx);
                }
                if let FormatDecision::Recreate(recreate) =
                    detect(ctx.stream, core.session(), ctx.seek_observe)
                {
                    return DecodeAction::StartRecreate(recreate);
                }
                if let Some(chunk) = core.next_drain() {
                    return produced(chunk, epoch, &mut ctx);
                }
                if let Some(emit) = ctx.emit {
                    emit.enqueue(AudioEvent::EndOfStream.into());
                }
                return DecodeAction::Eof;
            }
            Err(error) if error.classify() == ErrorClass::VariantChange => {
                return variant_change(core, &ctx, &error);
            }
            Err(error) if error.classify() == ErrorClass::Interrupted => {}
            Err(error) => {
                if let Some(emit) = ctx.emit {
                    emit.enqueue(
                        DecoderEvent::DecodeError {
                            class: map_decode_error_class(error.classify()),
                            kind: map_decode_error_kind(&error),
                            codec: core
                                .session()
                                .media_info
                                .as_ref()
                                .and_then(|info| info.codec)
                                .map(map_audio_codec_kind),
                            detail: decode_error_detail(&error),
                        }
                        .into(),
                    );
                }
                return DecodeAction::Failed(TrackFailure::Decode(error));
            }
        }
    }
}

pub(crate) fn produced<T: StreamType>(
    chunk: PcmChunk,
    epoch: u64,
    ctx: &mut DecodeCtx<'_, T>,
) -> DecodeAction {
    if ctx
        .resume
        .as_deref()
        .is_some_and(|resume| resume.seek.epoch == epoch)
        && let Some(emit) = ctx.emit
    {
        emit.enqueue(
            AudioEvent::SeekLifecycle {
                stage: SeekLifecycleStage::DecodeStarted,
                seek_epoch: epoch,
                location: SegmentLocation::new(
                    chunk.meta.variant_index,
                    chunk.meta.segment_index,
                    None,
                    None,
                ),
            }
            .into(),
        );
    }
    ctx.cursor.record(&chunk, epoch);
    DecodeAction::Produced(Fetch::data(chunk, epoch))
}

pub(crate) fn variant_change<T: StreamType>(
    core: &DecodeCore,
    ctx: &DecodeCtx<'_, T>,
    error: &DecodeError,
) -> DecodeAction {
    match handle_variant_change(ctx.stream, core.session(), ctx.seek_observe, error) {
        Ok(recreate) => DecodeAction::StartRecreate(recreate),
        Err(error) => {
            if error.is_interrupted() {
                DecodeAction::SeekInterrupted
            } else {
                DecodeAction::Failed(TrackFailure::Decode(error))
            }
        }
    }
}
