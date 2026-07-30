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
        },
        emit_request: false,
    })
}

/// The switch stays outstanding for the whole rebuild — it is acked at the
/// install site — so "a fence is up" no longer distinguishes the switch this
/// rebuild serves from a newer one. Compare targets instead: only a fence
/// demanding some *other* variant supersedes the decoder just built.
fn variant_superseded<T: StreamType>(stream: &SharedStream<T>, recreate: &RecreateState) -> bool {
    if let Some(target) = stream.variant_change_target()
        && recreate
            .media_info
            .variant_index
            .and_then(|index| usize::try_from(index).ok())
            != Some(target)
    {
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
