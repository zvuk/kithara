use std::num::NonZeroUsize;

use kithara_bufpool::PcmPool;
use kithara_platform::sync::Arc;

use super::{
    SourceAudioActivity, SourceAudioError, SourceAudioReader, SourceAudioTap,
    cache::SourceAudioCache,
    model::{SourceAudioCommand, SourceAudioPacket, SourceAudioStatus, SourceAudioWindow},
    reader::next_lane_id,
    tap::SourceAudioOutputs,
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
    let worker_wake: Arc<dyn WakeSignal> = Arc::new(SourceAudioWorkerWake(worker));
    let activity = SourceAudioActivity::new();
    let activity_wake: Arc<dyn WakeSignal> = Arc::new(SourceAudioActivityWake(activity.clone()));
    let (command_outlet, command_inlet) =
        connect::<SourceAudioCommand>(capacity, Some(Arc::clone(&worker_wake)));
    let (data_outlet, data_inlet) =
        connect::<SourceAudioPacket>(capacity, Some(Arc::clone(&activity_wake)));
    let (status_outlet, status_inlet) = connect::<SourceAudioStatus>(capacity, Some(activity_wake));
    let (trash_outlet, trash_inlet) =
        connect::<SourceAudioWindow>(trash_capacity, Some(worker_wake));

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
        activity.clone(),
    );
    let tap = SourceAudioTap::new(
        lane_id,
        command_inlet,
        SourceAudioOutputs::new(data_outlet, status_outlet, activity),
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

struct SourceAudioActivityWake(SourceAudioActivity);

impl WakeSignal for SourceAudioActivityWake {
    fn wake(&self) {
        self.0.signal();
    }
}
