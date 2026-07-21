use kithara_decode::{DecodeError, ErrorClass};
use kithara_stream::{MediaInfo, SeekObserve, StreamType};

use crate::pipeline::{
    rebuild::state::{RebuildState, RecreateOutcome, RecreateState},
    seek::{SeekContext, SeekRequest},
    stream::shared::SharedStream,
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
            events: (seek.pending_epoch() == Some(epoch)).into(),
        },
        emit_request: false,
    })
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
