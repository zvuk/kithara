use kithara_decode::DecoderSeekOutcome;
use kithara_stream::{PlayheadWrite, StreamType};
use num_traits::cast::ToPrimitive;

use crate::pipeline::{decode::DecoderGeneration, stream::shared::SharedStream};

pub(crate) fn commit_outcome<T: StreamType>(
    active: &DecoderGeneration,
    stream: &SharedStream<T>,
    playhead: &dyn PlayheadWrite,
    outcome: &DecoderSeekOutcome,
) {
    let sample_rate = active.decoder().spec().sample_rate.get();
    let (frame_offset, end_position, landed_byte) = match *outcome {
        DecoderSeekOutcome::Landed {
            landed_frame,
            landed_at,
            landed_byte,
            ..
        } => (landed_frame, landed_at, landed_byte),
        DecoderSeekOutcome::PastEof { duration } => {
            let frame = ToPrimitive::to_u64(&(duration.as_secs_f64() * f64::from(sample_rate)))
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
