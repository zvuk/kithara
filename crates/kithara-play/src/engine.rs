use tokio::sync::broadcast;

use crate::dj::crossfade::CrossfadeConfig;
use crate::error::PlayError;
use crate::events::EngineEvent;
use crate::types::SlotId;

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = EngineMock)
)]
pub trait Engine: Send + Sync + 'static {
    // -- lifecycle --

    fn start(&self) -> Result<(), PlayError>;

    fn stop(&self) -> Result<(), PlayError>;

    fn is_running(&self) -> bool;

    // -- arena slot management --

    fn allocate_slot(&self) -> Result<SlotId, PlayError>;

    fn release_slot(&self, slot: SlotId) -> Result<(), PlayError>;

    fn active_slots(&self) -> Vec<SlotId>;

    fn slot_count(&self) -> usize;

    fn max_slots(&self) -> usize;

    // -- master output --

    fn master_volume(&self) -> f32;

    fn set_master_volume(&self, volume: f32);

    fn master_sample_rate(&self) -> u32;

    fn master_channels(&self) -> u16;

    // -- crossfade (convenience delegation) --

    fn crossfade(&self, from: SlotId, to: SlotId, config: CrossfadeConfig)
    -> Result<(), PlayError>;

    fn cancel_crossfade(&self) -> Result<(), PlayError>;

    fn is_crossfading(&self) -> bool;

    // -- events --

    fn subscribe(&self) -> broadcast::Receiver<EngineEvent>;
}
