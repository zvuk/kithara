use kithara_decode::PcmChunk;
use kithara_events::{AudioEvent, DecoderChangeCause, DecoderEvent, FrameDomain};
use kithara_stream::{SourcePhase, StreamType};
use tracing::{debug, warn};

use super::{
    AwaitingResume, Decoding, Failed, RecreatingDecoder, SeekRequested, Track, TrackFailure,
    TrackStep, WaitContext, WaitState, WaitingForSource, WaitingReason, fsm::apply_seek_transition,
    rebuild::start_recreating_decoder,
};
use crate::{
    audio::event::{
        DecoderChangedEventData, decoder_changed_event, decoder_gapless_event,
        map_playback_resampler_kind, map_resampler_kind,
    },
    pipeline::{
        decode::{
            format::{FormatDecision, detect},
            gate::recreate_phase,
            resume::seek_position,
        },
        rebuild::{
            DecoderRebuildComplete, RebuildState, RecreateCause, RecreateNext, RecreateOutcome,
            RecreateState,
            policy::{classify, observed_seek, superseded},
        },
        seek::{ResumeState, SeekRequest, engine::SeekTransition},
        source::StreamAudioSource,
    },
};

fn decoder_change_cause(cause: RecreateCause) -> DecoderChangeCause {
    match cause {
        RecreateCause::FormatBoundary => DecoderChangeCause::FormatBoundary,
        RecreateCause::RouteChange => DecoderChangeCause::HostRateChange,
        RecreateCause::VariantSwitch => DecoderChangeCause::VariantSwitch,
    }
}

fn decoder_resampler_event<T: StreamType>(
    src: &StreamAudioSource<T>,
    media_info: &kithara_stream::MediaInfo,
    spec: kithara_decode::PcmSpec,
) -> Option<DecoderEvent> {
    if !src.resume.recreates_on_route() {
        return None;
    }
    let output_rate = src.resume.host_rate();
    if output_rate == 0 {
        return None;
    }
    let input_rate = media_info
        .sample_rate
        .or_else(|| (spec.sample_rate.get() == output_rate).then_some(spec.sample_rate.get()))?;
    Some(DecoderEvent::ResamplerConfigured {
        backend: map_resampler_kind(src.playback_resampler_backend),
        input_rate,
        output_rate,
        channels: spec.channels,
        bypassed: input_rate == output_rate,
    })
}

fn playback_resampler_event<T: StreamType>(
    src: &StreamAudioSource<T>,
    media_info: &kithara_stream::MediaInfo,
    spec: kithara_decode::PcmSpec,
) -> Option<AudioEvent> {
    let host_sample_rate = src.resume.host_rate();
    if host_sample_rate == 0 {
        return None;
    }
    let source_sample_rate = media_info.sample_rate.or_else(|| {
        (spec.sample_rate.get() == host_sample_rate).then_some(spec.sample_rate.get())
    })?;
    Some(AudioEvent::PlaybackResamplerConfigured {
        backend: map_playback_resampler_kind(src.playback_resampler_backend),
        host_sample_rate,
        source_sample_rate,
        active: host_sample_rate != source_sample_rate && src.playback_resampler_backend != "none",
    })
}

fn finish_route_change_after_recreate<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    recreate: &RecreateState,
    request: SeekRequest,
) -> TrackStep<PcmChunk> {
    let target = request.seek.target;
    match src
        .decode
        .seek(&src.shared_stream, src.playhead.as_ref(), target)
    {
        Ok(outcome) => {
            src.decode.reset();
            src.seek_engine
                .record_resume_target(request.seek.epoch, target);
            let landed_at = seek_position(outcome);
            let remaining = target.saturating_sub(landed_at);
            let skip = (!remaining.is_zero()).then_some(remaining);
            src.update_state(
                Track::<AwaitingResume>::new(ResumeState {
                    anchor_offset: Some(recreate.offset),
                    anchor_variant_index: recreate
                        .media_info
                        .variant_index
                        .and_then(|variant| usize::try_from(variant).ok()),
                    skip,
                    seek: request.seek,
                })
                .erase(),
            );
            TrackStep::StateChanged
        }
        Err(err) => {
            warn!(
                ?err,
                ?target,
                "route-change recreate: recreated decoder seek failed"
            );
            src.update_state(
                Track::<Failed>::new(TrackFailure::RecreateFailed {
                    offset: src.decode.session().base_offset,
                })
                .erase(),
            );
            TrackStep::StateChanged
        }
    }
}

/// Apply the `RecreateNext` action after a successful recreation.
fn apply_recreate_next<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    recreate: &RecreateState,
) -> TrackStep<PcmChunk> {
    match &recreate.next {
        RecreateNext::Decode => {
            src.decode.reset();
            src.update_state(Track::<Decoding>::new(()).erase());
            TrackStep::StateChanged
        }
        RecreateNext::Seek(request) => {
            src.update_state(Track::<SeekRequested>::new(*request).erase());
            TrackStep::StateChanged
        }
        RecreateNext::ApplySeek(request) if recreate.cause == RecreateCause::RouteChange => {
            finish_route_change_after_recreate(src, recreate, *request)
        }
        RecreateNext::ApplySeek(request) => finish_apply_seek_after_recreate(src, *request),
    }
}

fn finish_format_boundary_rebuild<T: StreamType>(
    src: &mut StreamAudioSource<T>,
) -> RecreateOutcome {
    // Continue the new decoder from the producer's decode head, not the
    // consumer's lagging `committed`: chunks in [committed..decode_head]
    // are already queued in the outlet ring (a FormatBoundary recreate
    // neither flushes it nor bumps the seek epoch), so resuming at
    // `committed` re-emits them — duplicated content, a backward phase
    // jump. The decode head is an exact frame; the demuxer quantizes the
    // seek landing to a sample, and `frame_offset_for` rounds that back
    // to the nearest frame (consistent with `frames_to_trim`), so the
    // rebuilt decoder relabels its first chunk at exactly `decode_head`.
    let committed = src.playhead.position();
    let epoch_now = src.seek_engine.epoch();
    // `resume_target` wins only while the target has NOT yet
    // materialized in produced chunks (`target > decode_head`);
    // comparing against the consumer's lagging `committed` mislabels
    // the warmed-up case and re-emits `[target..decode_head)`.
    let target_time =
        src.resume
            .resume_position(epoch_now, committed, src.seek_engine.resume_target());
    debug!(
        ?target_time,
        stream_pos = src.shared_stream.position(),
        stream_len = ?src.shared_stream.len(),
        "execute_recreation: after apply_format_change, about to decoder_seek_safe"
    );
    if !target_time.is_zero()
        && let Err(e) = src
            .decode
            .seek(&src.shared_stream, src.playhead.as_ref(), target_time)
    {
        warn!(
            ?e,
            ?target_time,
            "Failed to seek decoder to timeline position after cross-codec recreate"
        );
        return classify(&e);
    }
    debug!(
        ?target_time,
        stream_pos_final = src.shared_stream.position(),
        "execute_recreation: FormatBoundary+Decode branch exit"
    );
    RecreateOutcome::Done
}

pub(super) fn finish_recreate_outcome<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    recreate: RecreateState,
    outcome: RecreateOutcome,
) -> TrackStep<PcmChunk> {
    match outcome {
        RecreateOutcome::Done => apply_recreate_next(src, &recreate),
        RecreateOutcome::SoftFailed => {
            src.update_state(
                Track::<Failed>::new(TrackFailure::RecreateFailed {
                    offset: recreate.offset,
                })
                .erase(),
            );
            TrackStep::Failed
        }
        RecreateOutcome::NeedsSourceWait => wait_for_source_on_recreate(src, recreate),
    }
}

pub(super) fn finish_rebuild<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    rebuild: RebuildState,
    complete: DecoderRebuildComplete,
) -> TrackStep<PcmChunk> {
    if superseded(&src.shared_stream, src.seek_obs.as_ref(), &rebuild) {
        if let Ok(decoder) = complete.result {
            src.retired.retire(decoder);
        }
        return transition_after_rebuild_superseded(src, &rebuild);
    }
    let recreate = rebuild.recreate;
    let decoder = match complete.result {
        Ok(decoder) => decoder,
        Err(outcome) => return finish_recreate_outcome(src, recreate, outcome),
    };
    let duration = decoder.duration();
    let spec = decoder.spec();
    let track_info = decoder.track_info();
    let old = src.decode.install(
        decoder,
        recreate.media_info.clone(),
        recreate.offset,
        src.seek_obs.epoch(),
    );
    src.retired.retire(old);
    debug!(
        ?duration,
        offset = recreate.offset,
        "Decoder recreated successfully"
    );
    if let Some(ref emit) = src.emit {
        emit.enqueue(
            decoder_changed_event(DecoderChangedEventData {
                backend: src.decoder_backend,
                media_info: Some(&recreate.media_info),
                spec,
                track_info: &track_info,
                epoch: src.seek_obs.epoch(),
                cause: decoder_change_cause(recreate.cause),
                base_offset: recreate.offset,
                duration,
            })
            .into(),
        );
        if let Some(event) = decoder_gapless_event(
            Some(&recreate.media_info),
            spec,
            &track_info,
            FrameDomain::Output,
        ) {
            emit.enqueue(kithara_events::Event::from(event));
        }
        if let Some(event) = decoder_resampler_event(src, &recreate.media_info, spec) {
            emit.enqueue(kithara_events::Event::from(event));
        }
        if let Some(event) = playback_resampler_event(src, &recreate.media_info, spec) {
            emit.enqueue(kithara_events::Event::from(event));
        }
    }
    let outcome = if recreate_resumes_decode_head(&recreate) {
        finish_format_boundary_rebuild(src)
    } else {
        RecreateOutcome::Done
    };
    finish_recreate_outcome(src, recreate, outcome)
}

fn transition_to_seek_request<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    request: SeekRequest,
) -> TrackStep<PcmChunk> {
    src.update_state(Track::<SeekRequested>::new(request).erase());
    src.decode.reset();
    src.decode.notify_seek();
    TrackStep::StateChanged
}

fn transition_after_rebuild_superseded<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    rebuild: &RebuildState,
) -> TrackStep<PcmChunk> {
    let carried_seek = match &rebuild.recreate.next {
        RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => Some(*request),
        RecreateNext::Decode => None,
    };
    if let Some(request) = rebuild
        .superseded_seek
        .or_else(|| observed_seek(src.seek_obs.as_ref(), rebuild.started_seek_epoch))
        .or(carried_seek)
    {
        return transition_to_seek_request(src, request);
    }
    if let FormatDecision::Recreate(recreate) = detect(
        &src.shared_stream,
        src.decode.session(),
        src.seek_obs.as_ref(),
    ) {
        start_recreating_decoder(src, recreate);
    } else {
        src.update_state(Track::<Decoding>::new(()).erase());
    }
    TrackStep::StateChanged
}

fn finish_apply_seek_after_recreate<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    request: SeekRequest,
) -> TrackStep<PcmChunk> {
    debug!(
        target = ?request.seek.target,
        epoch = request.seek.epoch,
        emit_request = request.emit_request,
        committed_position = ?src.playhead.position(),
        stream_pos = src.shared_stream.position(),
        "finish_apply_seek_after_recreate: enter"
    );
    match src.decode.seek(
        &src.shared_stream,
        src.playhead.as_ref(),
        request.seek.target,
    ) {
        Ok(_outcome) => {
            src.decode.reset();
            src.seek_engine
                .record_resume_target(request.seek.epoch, request.seek.target);
            if request.seek.events.should_publish()
                && let Some(ref emit) = src.emit
            {
                emit.enqueue(
                    AudioEvent::SeekLifecycle {
                        stage: kithara_events::SeekLifecycleStage::SeekApplied,
                        seek_epoch: request.seek.epoch,
                        location: kithara_events::SegmentLocation::new(
                            src.shared_stream
                                .abr_handle()
                                .and_then(|handle| handle.current_variant_index()),
                            None,
                            None,
                            None,
                        ),
                    }
                    .into(),
                );
            }
            apply_seek_transition(
                src,
                SeekTransition::Applied {
                    epoch: request.seek.epoch,
                    resume: ResumeState {
                        seek: request.seek,
                        ..Default::default()
                    },
                },
            );
            TrackStep::StateChanged
        }
        Err(err) => {
            apply_seek_transition(
                src,
                SeekTransition::Reject {
                    request,
                    error: err,
                    context: "step_recreating_decoder: recreated decoder seek failed",
                },
            );
            TrackStep::StateChanged
        }
    }
}

/// Handle the "source not ready for boundary" branch of
/// recreate. Transitions to `WaitingForSource` or terminates the
/// track, depending on the source phase. Owns the `RecreateState`
/// (moved out of the FSM by the caller) — no `src.state` re-read.
pub(super) fn wait_for_source_on_recreate<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    recreate: RecreateState,
) -> TrackStep<PcmChunk> {
    let phase = recreate_phase(&src.shared_stream, &recreate);
    if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
        src.update_state(
            Track::<WaitingForSource>::new(WaitState {
                context: WaitContext::Recreation(recreate),
                reason,
            })
            .erase(),
        );
        return TrackStep::Blocked(reason);
    }
    if phase == SourcePhase::Cancelled {
        src.update_state(Track::<Failed>::new(TrackFailure::SourceCancelled).erase());
        return TrackStep::Failed;
    }
    src.update_state(Track::<RecreatingDecoder>::new(recreate).erase());
    TrackStep::Blocked(WaitingReason::Waiting)
}

fn recreate_resumes_decode_head(recreate: &RecreateState) -> bool {
    recreate.cause == RecreateCause::FormatBoundary && matches!(recreate.next, RecreateNext::Decode)
}
