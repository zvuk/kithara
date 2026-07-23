use std::mem;

use kithara_decode::PcmChunk;
use kithara_stream::StreamType;
use kithara_test_utils::kithara;
use tracing::debug;

use super::{
    CurrentFsm, TrackStep, WaitingReason,
    phase::{Track, TrackPhase, sealed},
    recreate::{finish_rebuild, finish_recreate_outcome, wait_for_source_on_recreate},
};
use crate::pipeline::{
    decode::resume::RouteCtx,
    rebuild::{RebuildState, RecreateNext, RecreateState},
    seek::emit::active_epoch,
    source::StreamAudioSource,
};

/// Recreating the decoder (format boundary, codec change, seek recovery).
pub(crate) struct RecreatingDecoder;

impl sealed::Sealed for RecreatingDecoder {}

impl TrackPhase for RecreatingDecoder {
    type Data = RecreateState;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::RecreatingDecoder(track)
    }
}

impl Track<RecreatingDecoder> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let recreate = self.into_inner();
        if !src
            .readiness
            .source_ready_for_recreate(&src.shared_stream, &recreate)
        {
            return wait_for_source_on_recreate(src, recreate);
        }
        match src
            .rebuild
            .prepare(&src.shared_stream, recreate, src.seek_obs.epoch())
        {
            Ok(rebuild) => {
                src.update_state(Track::<RebuildingDecoder>::new(rebuild).erase());
                TrackStep::StateChanged
            }
            Err((recreate, outcome)) => finish_recreate_outcome(src, recreate, outcome),
        }
    }
}

/// Waiting for an off-core decoder rebuild to complete.
pub(crate) struct RebuildingDecoder;

impl sealed::Sealed for RebuildingDecoder {}

impl TrackPhase for RebuildingDecoder {
    type Data = RebuildState;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::RebuildingDecoder(track)
    }
}

impl Track<RebuildingDecoder> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let mut rebuild = self.into_inner();
        rebuild.record_seek_preempt(src.seek_obs.as_ref(), src.seek_engine.epoch());
        while let Some(complete) = rebuild.completion.pop() {
            if complete.ticket == rebuild.ticket {
                return finish_rebuild(src, rebuild, complete);
            }
            if let Ok(decoder) = complete.result {
                src.retired.retire(decoder);
            }
        }
        src.update_state(Self::new(rebuild).erase());
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

#[kithara::probe]
pub(crate) fn start_recreating_decoder<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    state: RecreateState,
) {
    let pending_seek_target = match &state.next {
        RecreateNext::Seek(req) | RecreateNext::ApplySeek(req) => Some(req.seek.target),
        RecreateNext::Decode => None,
    };
    debug!(
        cause = ?state.cause,
        codec = ?state.media_info.codec,
        container = ?state.media_info.container,
        target_offset = state.offset,
        next = ?mem::discriminant(&state.next),
        ?pending_seek_target,
        committed_position = ?src.playhead.position(),
        stream_pos = src.shared_stream.position(),
        "start_recreating_decoder"
    );
    src.update_state(Track::<RecreatingDecoder>::new(state).erase());
}

pub(crate) fn start_route_change_recreate_if_needed<T: StreamType>(
    src: &mut StreamAudioSource<T>,
) -> bool {
    let Some(recreate) = src.resume.route_change(&RouteCtx {
        committed: src.playhead.position(),
        seek: &src.seek_engine,
        seek_active: active_epoch(&src.state).is_some(),
        session: src.decode.session(),
        stream: &src.shared_stream,
    }) else {
        return false;
    };
    start_recreating_decoder(src, recreate);
    true
}
