use std::time::Duration;

use derivative::Derivative;
use kithara_platform::{MaybeSend, MaybeSync};

use crate::{error::PlayError, types::SlotId};

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct BpmInfo {
    pub bpm: f64,
    pub confidence: f32,
    pub first_beat_offset: Duration,
}

#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
#[non_exhaustive]
pub struct BeatGrid {
    #[derivative(Default(value = "120.0"))]
    pub bpm: f64,
    pub offset: Duration,
    #[derivative(Default(value = "4"))]
    pub beats_per_bar: u8,
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = BpmAnalyzerMock)
)]
pub trait BpmAnalyzer: MaybeSend + MaybeSync + 'static {
    fn analyze(&self, slot: SlotId) -> Result<BpmInfo, PlayError>;

    fn bpm(&self, slot: SlotId) -> Option<f64>;

    fn set_manual_bpm(&self, slot: SlotId, bpm: f64) -> Result<(), PlayError>;

    fn beat_grid(&self, slot: SlotId) -> Option<BeatGrid>;

    fn set_beat_grid(&self, slot: SlotId, grid: BeatGrid) -> Result<(), PlayError>;

    fn current_beat(&self, slot: SlotId) -> Option<u64>;

    fn beats_until_end(&self, slot: SlotId) -> Option<u64>;

    fn tap_tempo(&self, slot: SlotId) -> Result<f64, PlayError>;
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = BpmSyncMock)
)]
pub trait BpmSync: MaybeSend + MaybeSync + 'static {
    fn sync(&self, follower: SlotId, leader: SlotId) -> Result<(), PlayError>;

    fn unsync(&self, slot: SlotId) -> Result<(), PlayError>;

    fn is_synced(&self, slot: SlotId) -> bool;

    fn leader(&self, slot: SlotId) -> Option<SlotId>;

    fn phase_offset(&self, slot: SlotId) -> f64;

    fn set_phase_offset(&self, slot: SlotId, beats: f64) -> Result<(), PlayError>;

    fn nudge_forward(&self, slot: SlotId, beats: f64) -> Result<(), PlayError>;

    fn nudge_backward(&self, slot: SlotId, beats: f64) -> Result<(), PlayError>;

    fn quantize_enabled(&self) -> bool;

    fn set_quantize_enabled(&self, enabled: bool);
}
