use kithara_decode::DecoderSeekOutcome;
use kithara_events::{AudioEvent, DeferredBus, Event, SeekLifecycleStage, SegmentLocation};
use kithara_platform::time::Duration;
use kithara_stream::{PlayheadWrite, SeekObserve, StreamType};

use crate::pipeline::{
    decode::{DecoderSession, core::DecodeCore},
    rebuild::{RecreateNext, RecreateState},
    seek::SeekEngine,
    stream::shared::SharedStream,
    track::{CurrentFsm, WaitContext},
};

pub(crate) fn commit_outcome<T: StreamType>(
    session: &DecoderSession,
    stream: &SharedStream<T>,
    playhead: &dyn PlayheadWrite,
    outcome: &DecoderSeekOutcome,
) {
    let sample_rate = session.decoder.spec().sample_rate.get();
    let (frame_offset, end_position, landed_byte) = match *outcome {
        DecoderSeekOutcome::Landed {
            landed_frame,
            landed_at,
            landed_byte,
            ..
        } => (landed_frame, landed_at, landed_byte),
        DecoderSeekOutcome::PastEof { duration } => {
            let frame = num_traits::cast::ToPrimitive::to_u64(
                &(duration.as_secs_f64() * f64::from(sample_rate)),
            )
            .unwrap_or(u64::MAX);
            (frame, duration, None)
        }
    };
    playhead.land(&kithara_stream::ChunkPosition {
        frame_offset,
        end_position_ns: u64::try_from(end_position.as_nanos()).unwrap_or(u64::MAX),
        frames: 0,
        source_bytes: 0,
        source_byte_offset: landed_byte,
    });
    if let Some(byte) = landed_byte {
        stream.set_position(byte);
    }
}

pub(crate) fn active_epoch(state: &CurrentFsm) -> Option<u64> {
    let recreate_epoch = |recreate: &RecreateState| match &recreate.next {
        RecreateNext::Decode => None,
        RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => Some(request.seek.epoch),
    };
    match state {
        CurrentFsm::SeekRequested(handle) => Some(handle.data().seek.epoch),
        CurrentFsm::ApplyingSeek(handle) => Some(handle.data().request.seek.epoch),
        CurrentFsm::AwaitingResume(handle) => Some(handle.data().seek.epoch),
        CurrentFsm::RecreatingDecoder(handle) => recreate_epoch(handle.data()),
        CurrentFsm::RebuildingDecoder(handle) => handle
            .data()
            .superseded_seek
            .map(|request| request.seek.epoch)
            .or_else(|| recreate_epoch(&handle.data().recreate)),
        CurrentFsm::WaitingForSource(handle) => match &handle.data().context {
            WaitContext::Seek(request) => Some(request.seek.epoch),
            WaitContext::ApplySeek(applying) => Some(applying.request.seek.epoch),
            WaitContext::Recreation(recreate) => recreate_epoch(recreate),
            WaitContext::Playback | WaitContext::PostSeek(_) => None,
        },
        CurrentFsm::Decoding(_) | CurrentFsm::AtEof(_) | CurrentFsm::Failed(_) => None,
    }
}

pub(crate) fn preempt_target(
    engine: &SeekEngine,
    state: &CurrentFsm,
    observe: &dyn SeekObserve,
) -> Option<Duration> {
    if !observe.take_preempt() {
        return None;
    }
    let epoch = observe.epoch();
    if epoch <= engine.epoch() || state.is_terminal() {
        return None;
    }
    let target = observe.target()?;
    if active_epoch(state).is_some_and(|active| active >= epoch) {
        return None;
    }
    Some(target)
}

pub(crate) fn update_len<T: StreamType>(decode: &DecodeCore, stream: &SharedStream<T>) {
    if let Some(len) = stream.len()
        && len > 0
    {
        decode.update_len(len.saturating_sub(decode.session().base_offset));
    }
}

pub(crate) fn location<T: StreamType>(stream: &SharedStream<T>) -> SegmentLocation {
    SegmentLocation::new(
        stream
            .abr_handle()
            .and_then(|handle| handle.current_variant_index()),
        None,
        None,
        None,
    )
}

pub(crate) fn emit(
    bus: Option<&DeferredBus<Event>>,
    stage: SeekLifecycleStage,
    epoch: u64,
    location: SegmentLocation,
) {
    if let Some(bus) = bus {
        bus.enqueue(
            AudioEvent::SeekLifecycle {
                stage,
                seek_epoch: epoch,
                location,
            }
            .into(),
        );
    }
}

pub(crate) fn land_eof(session: &DecoderSession, playhead: &dyn PlayheadWrite, duration: Duration) {
    let sample_rate = session.decoder.spec().sample_rate.get();
    let frame_offset =
        num_traits::cast::ToPrimitive::to_u64(&(duration.as_secs_f64() * f64::from(sample_rate)))
            .unwrap_or(u64::MAX);
    playhead.land(&kithara_stream::ChunkPosition {
        end_position_ns: u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
        frame_offset,
        frames: 0,
        source_bytes: 0,
        source_byte_offset: None,
    });
}
