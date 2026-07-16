use std::num::NonZeroUsize;

use kithara_bufpool::PcmPool;
use kithara_platform::sync::Arc;

use super::{
    SourceAudioError, SourceAudioReader, SourceAudioTap,
    cache::SourceAudioCache,
    model::{SourceAudioCommand, SourceAudioPacket, SourceAudioStatus, SourceAudioWindow},
    reader::next_lane_id,
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
    let lane_id = next_lane_id()?;
    let wake: Arc<dyn WakeSignal> = Arc::new(SourceAudioWorkerWake(worker));
    let (command_outlet, command_inlet) =
        connect::<SourceAudioCommand>(capacity, Some(Arc::clone(&wake)));
    let (data_outlet, data_inlet) = connect::<SourceAudioPacket>(capacity, None);
    let (status_outlet, status_inlet) = connect::<SourceAudioStatus>(capacity, None);
    let (trash_outlet, trash_inlet) = connect::<SourceAudioWindow>(trash_capacity, Some(wake));

    let mut buffers = Vec::with_capacity(buffer_count);
    for _ in 0..buffer_count {
        buffers.push(pool.get());
    }
    let reader = SourceAudioReader::new(
        lane_id,
        command_outlet,
        data_inlet,
        status_inlet,
        trash_outlet,
        SourceAudioCache::new(
            NonZeroUsize::new(capacity).ok_or(SourceAudioError::CapacityOverflow)?,
        ),
    );
    let tap = SourceAudioTap::new(
        lane_id,
        command_inlet,
        data_outlet,
        status_outlet,
        trash_inlet,
        buffers,
        max_frames,
    );
    Ok((reader, tap))
}

struct SourceAudioWorkerWake(AudioWorkerHandle);

impl WakeSignal for SourceAudioWorkerWake {
    fn wake(&self) {
        self.0.wake();
    }
}
