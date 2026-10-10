use kithara_effects::GainDb;

use super::{RtMetricsSnapshot, SlotMark, SlotState};
use crate::rt::DeckMixerConfig;

/// What a deck's mixer last published of each slot and of itself, once per block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeckSnapshot {
    /// One entry per slot, in slot order.
    pub slots: Vec<SlotSnapshot>,
    /// Current output sample rate.
    pub sample_rate: u32,
    /// Blocks the mixer rendered.
    pub blocks: u64,
    /// The mixer's real-time counters.
    pub metrics: RtMetricsSnapshot,
    pub eq: EqSnapshot,
}

impl DeckSnapshot {
    #[must_use]
    pub fn new(config: DeckMixerConfig) -> Self {
        Self {
            slots: vec![SlotSnapshot::default(); config.slots().get()],
            eq: EqSnapshot {
                bands: config.eq_bands(),
                gains: vec![GainDb::default(); config.eq_bands()],
            },
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, get_mut)]
pub struct EqSnapshot {
    bands: usize,
    #[field(get_mut, vis = "pub(crate)")]
    gains: Vec<GainDb>,
}

impl EqSnapshot {
    #[must_use]
    pub const fn bands(&self) -> usize {
        self.bands
    }

    #[must_use]
    pub fn gain(&self, band: usize) -> Option<GainDb> {
        self.gains.get(band).copied().filter(|_| band < self.bands)
    }

    pub(crate) fn set_bands(&mut self, bands: usize) {
        self.bands = bands;
    }
}

/// One slot as its mixer last published it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SlotSnapshot {
    pub state: SlotState,
    pub mark: Option<SlotMark>,
    /// Media position in seconds.
    pub position: f64,
    /// Visible media duration in seconds; `0.0` when unknown.
    pub duration: f64,
    /// Decoded-ahead frontier in seconds, never behind `position`.
    pub frontier: f64,
    /// How much of the source is on disk, in seconds.
    pub cached: f64,
    /// The slot's envelope gain on its last mixed frame.
    pub gain: f32,
}
