mod build;
mod core;
mod cursor;
pub(crate) mod event;
mod park;
mod position;
mod ring;
mod seek;

pub use core::{Audio, PreparedAudio};

pub(crate) use position::chunk_position;
#[cfg(test)]
pub(crate) use seek::SeekHandleParts;
pub use seek::{ScheduledSeekActivator, SeekHandle};

pub(crate) use crate::{
    AudioConfig, AudioControl, AudioDecoderConfig, AudioLaneEvent, AudioRead, AudioSession,
    ChunkOutcome, ConsumerWakeMode, DecodeError, Fetch, PendingReason, PreloadGate,
    PreparedAudioLane, ReadOutcome, SeekOutcome,
    pipeline::{
        consumer::{ConsumerPhase, FailureSource},
        fetch::EpochValidator,
        parts::SourceParts,
        rebuild::port::RebuildRuntime,
        source::{
            DecodeInit, DecoderFactory as StreamDecoderFactory, SharedStream, StreamAudioSource,
        },
    },
    producer::ProducerPort,
    runtime::{Inlet, Outlet, WakeSignal, connect, wake::ThreadWake},
};
