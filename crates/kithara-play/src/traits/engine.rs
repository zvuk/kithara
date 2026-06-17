use kithara_events::EventReceiver;
use kithara_platform::maybe_send::{MaybeSend, MaybeSync};

#[rustfmt::skip]
use crate::traits::dj::crossfade::CrossfadeConfig;
use crate::{error::PlayError, types::SlotId};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

#[kithara::mock(api = EngineMock)]
pub trait Engine: MaybeSend + MaybeSync + 'static {
    fn active_slots(&self) -> Vec<SlotId>;

    fn allocate_slot(&self) -> Result<SlotId, PlayError>;

    fn cancel_crossfade(&self) -> Result<(), PlayError>;

    fn crossfade(&self, from: SlotId, to: SlotId, config: CrossfadeConfig)
    -> Result<(), PlayError>;

    fn is_crossfading(&self) -> bool;

    fn is_running(&self) -> bool;

    fn master_channels(&self) -> u16;

    fn master_sample_rate(&self) -> u32;

    fn master_volume(&self) -> f32;

    fn max_slots(&self) -> usize;

    fn release_slot(&self, slot: SlotId) -> Result<(), PlayError>;

    fn set_master_volume(&self, volume: f32);

    fn slot_count(&self) -> usize;

    fn start(&self) -> Result<(), PlayError>;

    fn stop(&self) -> Result<(), PlayError>;

    fn subscribe(&self) -> EventReceiver;
}
