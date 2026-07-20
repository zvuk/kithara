use std::num::NonZeroUsize;

use kithara_bufpool::PcmPool;
use kithara_platform::sync::Arc;

use super::{
    SourceAudioError, SourceAudioReader, SourceAudioTap,
    cache::SourceAudioCache,
    model::{SourceAudioCommand, SourceAudioPacket, SourceAudioStatus, SourceAudioWindow},
};
use crate::{
    renderer::AudioWorkerHandle,
    runtime::{WakeSignal, connect},
};

pub(crate) fn connect_source_audio(
    pool: &PcmPool,
    capacity: NonZeroUsize,
    max_frames: NonZeroUsize,
    worker: AudioWorkerHandle,
) -> Result<(SourceAudioReader, SourceAudioTap), SourceAudioError> {
    let capacity = capacity.get();
    let trash_capacity = capacity
        .checked_add(2)
        .ok_or(SourceAudioError::CapacityOverflow)?;
    let buffer_count = capacity
        .checked_add(capacity)
        .and_then(|count| count.checked_add(2))
        .ok_or(SourceAudioError::CapacityOverflow)?;
    let worker_wake: Arc<dyn WakeSignal> = Arc::new(worker);
    let (command_outlet, command_inlet) =
        connect::<SourceAudioCommand>(capacity, Some(Arc::clone(&worker_wake)));
    let (data_outlet, data_inlet) = connect::<SourceAudioPacket>(capacity, None);
    let (status_outlet, status_inlet) = connect::<SourceAudioStatus>(capacity, None);
    let (trash_outlet, trash_inlet) =
        connect::<SourceAudioWindow>(trash_capacity, Some(worker_wake));

    let buffers = (0..buffer_count).map(|_| pool.get()).collect();
    let reader = SourceAudioReader::new(
        command_outlet,
        data_inlet,
        status_inlet,
        trash_outlet,
        SourceAudioCache::new(
            NonZeroUsize::new(capacity).ok_or(SourceAudioError::CapacityOverflow)?,
        ),
    );
    let tap = SourceAudioTap::new(
        command_inlet,
        data_outlet,
        status_outlet,
        trash_inlet,
        buffers,
        max_frames,
    );
    Ok((reader, tap))
}
