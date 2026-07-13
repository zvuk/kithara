use kithara_decode::{DecodeError, ErrorClass};
use kithara_stream::{MediaInfo, SeekObserve, StreamType};

use crate::pipeline::{
    stream::shared::SharedStream,
    track_fsm::{RebuildState, RecreateOutcome, RecreateState, SeekContext, SeekRequest},
};

pub(crate) fn classify(error: &DecodeError) -> RecreateOutcome {
    if error.classify() == ErrorClass::Interrupted {
        RecreateOutcome::NeedsSourceWait
    } else {
        RecreateOutcome::SoftFailed
    }
}

pub(crate) fn superseded<T: StreamType>(
    stream: &SharedStream<T>,
    seek: &dyn SeekObserve,
    rebuild: &RebuildState,
) -> bool {
    rebuild.superseded_seek.is_some()
        || seek.epoch() != rebuild.started_seek_epoch
        || variant_superseded(stream, &rebuild.recreate)
}

pub(crate) fn observed_seek(seek: &dyn SeekObserve, min_epoch: u64) -> Option<SeekRequest> {
    let epoch = seek.epoch();
    if epoch <= min_epoch {
        return None;
    }
    Some(SeekRequest {
        seek: SeekContext {
            target: seek.target()?,
            epoch,
        },
        emit_request: false,
    })
}

pub(crate) fn record_seek_preempt(
    rebuild: &mut RebuildState,
    seek: &dyn SeekObserve,
    decode_epoch: u64,
) {
    if !seek.take_preempt() {
        return;
    }
    let epoch = seek.epoch();
    let min_epoch = rebuild
        .superseded_seek
        .map_or(rebuild.started_seek_epoch, |request| request.seek.epoch);
    if epoch <= min_epoch || epoch <= decode_epoch {
        return;
    }
    let Some(target) = seek.target() else {
        return;
    };
    rebuild.superseded_seek = Some(SeekRequest {
        seek: SeekContext { target, epoch },
        emit_request: false,
    });
}

fn variant_superseded<T: StreamType>(stream: &SharedStream<T>, recreate: &RecreateState) -> bool {
    if stream.has_variant_change_pending() {
        return true;
    }
    let Some(current) = stream.media_info() else {
        return false;
    };
    media_differs(&current, &recreate.media_info)
}

fn media_differs(current: &MediaInfo, rebuilding: &MediaInfo) -> bool {
    if let (Some(current), Some(rebuilding)) = (current.variant_index, rebuilding.variant_index)
        && current != rebuilding
    {
        return true;
    }
    matches!(
        (current.codec, rebuilding.codec),
        (Some(current), Some(rebuilding)) if current != rebuilding
    )
}
