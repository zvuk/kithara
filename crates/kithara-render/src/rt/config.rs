use std::num::{NonZeroU32, NonZeroUsize};

use kithara_config::Config;
use kithara_derive::Patch;
use kithara_dsp::param::SmootherConfig;
use kithara_signal::FrameCount;
use num_traits::ToPrimitive;

use crate::{
    bridge::{DeckMixSettings, DeckMixSettingsPatch, DeckMixSettingsPatchError},
    consts::{DEFAULT_DECK_SLOTS, DEFAULT_DECLICK, DEFAULT_EVICT_FADE, DEFAULT_SAMPLE_RATE},
};

/// What a [`DeckMixer`](super::DeckMixer) is built with, fixed for its life.
#[derive(Clone, Copy, Debug, PartialEq, Config, Patch)]
#[config(default, fields(value, get(copy)), patch(fallible))]
pub struct DeckMixerConfig {
    /// Source axis used to prepare the deck's initial equalizer off-RT.
    #[config(builder(default = DEFAULT_SAMPLE_RATE))]
    sample_rate: NonZeroU32,
    /// Initial log-spaced bands and capacity of later layouts. Default: 10.
    #[config(builder(default = 10))]
    eq_bands: usize,
    /// How many tracks the deck holds at once; its owner assigns them. Default: 4.
    #[config(builder(default = DEFAULT_DECK_SLOTS))]
    slots: NonZeroUsize,
    /// The ramp a slot starts and stops with. Default: 5 ms.
    #[config(builder(default = DEFAULT_DECLICK))]
    declick: SmootherConfig,
    /// Frames of a replaced consumer that play out of its slot's tail, ramped down to silence.
    /// Default: 512.
    #[config(builder(default = DEFAULT_EVICT_FADE), patch(wire = usize, from = FrameCount::new))]
    evict_fade: FrameCount,
    /// Maximum obsolete packets recycled by each slot in one block.
    #[config(builder(default = crate::consts::CAPACITY))]
    recycle_per_block: NonZeroUsize,
    /// How loud the deck sounds before its owner changes it.
    #[config(builder(default), patch(nested, fallible))]
    mix: DeckMixSettings,
}

impl DeckMixerConfig {
    /// Output frames in the configured declick, rounded to the nearest frame.
    #[must_use]
    pub fn declick_frames(self, sample_rate: NonZeroU32) -> FrameCount {
        declick_frame_count(self.declick, sample_rate)
    }
}

pub(crate) fn declick_frame_count(declick: SmootherConfig, sample_rate: NonZeroU32) -> FrameCount {
    let rate = sample_rate.get().to_f32().unwrap_or(f32::MAX);
    FrameCount::new(
        (declick.smooth_seconds.max(0.0) * rate)
            .round()
            .to_usize()
            .unwrap_or(usize::MAX),
    )
}
