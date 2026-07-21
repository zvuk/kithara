use kithara_decode::PcmChunk;
use kithara_stream::{SourcePhase, StreamType};

use super::{
    CurrentFsm, Failed, TrackFailure, TrackStep, WaitContext, WaitState, WaitingForSource,
    WaitingReason,
    phase::{Track, TrackPhase, sealed},
    rebuild::start_recreating_decoder,
};
use crate::pipeline::{
    decode::{
        core::{DecodeAction as CoreDecodeAction, DecodeCtx},
        format::{FormatDecision, detect},
        step,
    },
    fetch::Fetch,
    source::StreamAudioSource,
};

/// Normal decoding — produce PCM chunks.
pub(crate) struct Decoding;

impl sealed::Sealed for Decoding {}

impl TrackPhase for Decoding {
    type Data = ();

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::Decoding(track)
    }
}

impl Track<Decoding> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let () = self.into_inner();
        if !src.readiness.source_is_ready(&src.shared_stream) {
            if !src.seek_obs.is_pending()
                && let FormatDecision::Recreate(recreate) = detect(
                    &src.shared_stream,
                    src.decode.session(),
                    src.seek_obs.as_ref(),
                )
            {
                start_recreating_decoder(src, recreate);
                return TrackStep::StateChanged;
            }
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(
                    Track::<WaitingForSource>::new(WaitState {
                        context: WaitContext::Playback,
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
            // Stay in Decoding — the dispatcher's sentinel is already
            // `Decoding`, so no restore is needed.
            return TrackStep::Blocked(WaitingReason::Waiting);
        }

        match decode_step(src) {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::SourceProgress | DecodeStep::Interrupted => TrackStep::StateChanged,
            // The decoder read across the current segment boundary into a
            // not-ready (withheld) byte. Park in `WaitingForSource(Playback)`
            // rather than re-running the full decode every tick: the wait
            // state re-checks the forward read-ahead window cheaply and only
            // re-enters `Decoding` once that window is ready. Staying in
            // `Decoding` here hot-spins the decode loop (`source_is_ready`
            // gates only the current segment, so it never reflects the
            // blocked forward read) — flake F5.
            DecodeStep::NotReady(reason) => {
                src.update_state(
                    Track::<WaitingForSource>::new(WaitState {
                        context: WaitContext::Playback,
                        reason,
                    })
                    .erase(),
                );
                TrackStep::Blocked(reason)
            }
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

pub(super) enum DecodeStep {
    Produced(Fetch<PcmChunk>),
    SourceProgress,
    Interrupted,
    NotReady(WaitingReason),
    Eof,
    Failed,
}

pub(super) fn decode_step<T: StreamType>(src: &mut StreamAudioSource<T>) -> DecodeStep {
    let resuming = matches!(src.state, CurrentFsm::AwaitingResume(_));
    let action = {
        let resume = match &mut src.state {
            CurrentFsm::AwaitingResume(handle) => Some(handle.data_mut()),
            _ => None,
        };
        step::tick(
            &mut src.decode,
            DecodeCtx {
                emit: src.emit.as_deref(),
                playhead: src.playhead.as_ref(),
                resume,
                cursor: &mut src.resume,
                seek: &src.seek_engine,
                seek_observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        )
    };
    match action {
        CoreDecodeAction::Produced(fetch) => {
            if resuming {
                src.update_state(Track::<Decoding>::new(()).erase());
            }
            DecodeStep::Produced(fetch)
        }
        CoreDecodeAction::SourceProgress => DecodeStep::SourceProgress,
        CoreDecodeAction::Pending(reason) => DecodeStep::NotReady(reason),
        CoreDecodeAction::StartRecreate(recreate) => {
            start_recreating_decoder(src, recreate);
            DecodeStep::Interrupted
        }
        CoreDecodeAction::SeekInterrupted => DecodeStep::Interrupted,
        CoreDecodeAction::Eof => {
            src.update_state(Track::<AtEof>::new(()).erase());
            DecodeStep::Eof
        }
        CoreDecodeAction::Failed(failure) => {
            src.update_state(Track::<Failed>::new(failure).erase());
            DecodeStep::Failed
        }
    }
}

/// End of stream reached.
pub(crate) struct AtEof;

impl sealed::Sealed for AtEof {}

impl TrackPhase for AtEof {
    type Data = ();

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::AtEof(track)
    }
}
