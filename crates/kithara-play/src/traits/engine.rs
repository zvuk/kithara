use kithara_events::EventReceiver;
use kithara_platform::{MaybeSend, MaybeSync};

#[rustfmt::skip]
use crate::traits::dj::crossfade::CrossfadeConfig;
use crate::{error::PlayError, types::SlotId};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

#[kithara::mock(api = EngineMock)]
pub trait Engine: MaybeSend + MaybeSync + 'static {
    fn start(&self) -> Result<(), PlayError>;

    fn stop(&self) -> Result<(), PlayError>;

    fn is_running(&self) -> bool;

    fn allocate_slot(&self) -> Result<SlotId, PlayError>;

    fn release_slot(&self, slot: SlotId) -> Result<(), PlayError>;

    fn active_slots(&self) -> Vec<SlotId>;

    fn slot_count(&self) -> usize;

    fn max_slots(&self) -> usize;

    fn master_volume(&self) -> f32;

    fn set_master_volume(&self, volume: f32);

    fn master_sample_rate(&self) -> u32;

    fn master_channels(&self) -> u16;

    fn crossfade(&self, from: SlotId, to: SlotId, config: CrossfadeConfig)
    -> Result<(), PlayError>;

    fn cancel_crossfade(&self) -> Result<(), PlayError>;

    fn is_crossfading(&self) -> bool;

    fn subscribe(&self) -> EventReceiver;
}
