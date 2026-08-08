use kithara_decode::{DecoderChunkOutcome, ErrorClass, PcmChunk};
use kithara_events::{AudioEvent, DecoderEvent, SeekLifecycleStage, SegmentLocation};
use kithara_stream::{PendingReason, StreamType};
use kithara_test_utils::kithara;

use crate::{
    audio::event::{
        decode_error_detail, map_audio_codec_kind, map_decode_error_class, map_decode_error_kind,
    },
    pipeline::{
        decode::{
            core::{ActiveDecode, DecodeAction, DecodeCtx},
            format::{FormatDecision, detect},
        },
        fetch::Fetch,
        gapless::visible_duration,
        seek::skip::apply as apply_skip,
        track::{TrackFailure, WaitingReason},
        window::SourceEnd,
    },
};

#[kithara::hang_watchdog]
pub(crate) fn tick<T: StreamType>(
    core: &mut ActiveDecode,
    mut ctx: DecodeCtx<'_, T>,
) -> DecodeAction {
    let active = core.active();
    if let Some(duration) = visible_duration(
        active.decoder().duration(),
        active.gapless_profile(),
        core.gapless_mode(),
    ) && Some(duration) > ctx.playhead.duration()
    {
        ctx.playhead.set_duration(Some(duration));
    }
    let epoch = ctx.seek.epoch();
    loop {
        if ctx.seek_observe.is_flushing() || ctx.seek_observe.is_pending() {
            return DecodeAction::SeekInterrupted;
        }
        if let Some(chunk) = core.next_output() {
            return produced(chunk, core.source_end(), epoch, &mut ctx);
        }
        match core.next_chunk(ctx.stream.position()) {
            Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                return variant_change(core, &ctx);
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
                if let Some(chunk) = core.next_output() {
                    return produced(chunk, core.source_end(), epoch, &mut ctx);
                }
                if let FormatDecision::Recreate(recreate) =
                    detect(ctx.stream, core.active(), ctx.seek_observe)
                {
                    return DecodeAction::StartRecreate(recreate);
                }
                if let Some(chunk) = core.next_drain() {
                    return produced(chunk, core.source_end(), epoch, &mut ctx);
                }
                if let Some(emit) = ctx.emit {
                    emit.enqueue(AudioEvent::EndOfStream.into());
                }
                return DecodeAction::Eof;
            }
            Err(error) if error.classify() == ErrorClass::VariantChange => {
                return variant_change(core, &ctx);
            }
            Err(error) if error.classify() == ErrorClass::Interrupted => {}
            Err(error) => {
                if let Some(emit) = ctx.emit {
                    emit.enqueue(
                        DecoderEvent::DecodeError {
                            class: map_decode_error_class(error.classify()),
                            kind: map_decode_error_kind(&error),
                            codec: core
                                .active()
                                .media_info()
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
    source_end: Option<SourceEnd>,
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
    ctx.cursor.record(source_end, epoch);
    DecodeAction::Produced(Fetch::data(chunk, epoch))
}

pub(crate) fn variant_change<T: StreamType>(
    core: &ActiveDecode,
    ctx: &DecodeCtx<'_, T>,
) -> DecodeAction {
    match detect(ctx.stream, core.active(), ctx.seek_observe) {
        FormatDecision::Recreate(recreate) => DecodeAction::StartRecreate(recreate),
        FormatDecision::None => DecodeAction::SeekInterrupted,
    }
}
