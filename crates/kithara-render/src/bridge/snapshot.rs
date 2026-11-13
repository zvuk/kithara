use kithara_command::Seq;
use kithara_effects::GainDb;
use kithara_signal::AudioChunkInfo;

use super::{
    DeckParameter, FilterSnapshot, LayoutRevision, RtMetricsSnapshot, SlotBinding, SlotMark,
    SlotOutputSnapshot, SlotState,
};
use crate::rt::DeckMixerConfig;

/// What a deck's mixer last published of each slot and of itself, once per block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeckSnapshot {
    /// One entry per slot, in slot order.
    pub slots: Vec<SlotSnapshot>,
    /// The mixer's next unrendered session frame.
    pub at: kithara_signal::SessionFrame,
    /// Current output sample rate.
    pub sample_rate: u32,
    /// Blocks the mixer rendered.
    pub blocks: u64,
    /// The mixer's real-time counters.
    pub metrics: RtMetricsSnapshot,
    pub eq: EqSnapshot,
    pub filter: FilterSnapshot,
    pub epoch: Option<kithara_signal::SessionEpoch>,
    pub channels: [ParameterSnapshot; 3],
    pub fx_layouts: Vec<Option<LayoutRevision>>,
}

impl DeckSnapshot {
    #[must_use]
    pub fn new(config: DeckMixerConfig) -> Self {
        Self {
            slots: vec![SlotSnapshot::default(); config.slots().get()],
            eq: EqSnapshot {
                bands: config.eq_bands(),
                gains: vec![GainDb::default(); config.eq_bands()],
                layout: None,
                programs: vec![None; config.eq_bands()],
                authorities: vec![None; config.eq_bands()],
            },
            fx_layouts: vec![None; config.fx_units()],
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParameterSnapshot {
    value: f64,
    program: Option<Seq>,
    authority: Option<Seq>,
}

impl ParameterSnapshot {
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }

    #[must_use]
    pub fn program(&self) -> Option<Seq> {
        self.program
    }

    #[must_use]
    pub fn authority(&self) -> Option<Seq> {
        self.authority
    }

    pub(crate) const fn new(value: f64, program: Option<Seq>, authority: Option<Seq>) -> Self {
        Self {
            value,
            program,
            authority,
        }
    }
}

impl DeckSnapshot {
    #[must_use]
    pub fn parameter(&self, parameter: DeckParameter) -> Option<ParameterSnapshot> {
        match parameter {
            DeckParameter::Filter => Some(ParameterSnapshot::new(
                self.filter.knob(),
                self.filter.program(),
                self.filter.authority(),
            )),
            DeckParameter::EqBand { band, layout } if self.eq.layout() == Some(layout) => {
                self.eq.gain(band).map(|gain| {
                    ParameterSnapshot::new(
                        f64::from(f32::from(gain)),
                        self.eq.program(band),
                        self.eq.authority(band),
                    )
                })
            }
            DeckParameter::EqBand { .. } => None,
            parameter => self.channels.get(parameter.index()).copied(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, get_mut)]
pub struct EqSnapshot {
    bands: usize,
    #[field(get_mut, vis = "pub(crate)")]
    gains: Vec<GainDb>,
    layout: Option<LayoutRevision>,
    programs: Vec<Option<Seq>>,
    authorities: Vec<Option<Seq>>,
}

impl EqSnapshot {
    #[must_use]
    pub fn layout(&self) -> Option<LayoutRevision> {
        self.layout
    }

    #[must_use]
    pub const fn bands(&self) -> usize {
        self.bands
    }

    #[must_use]
    pub fn gain(&self, band: usize) -> Option<GainDb> {
        self.gains.get(band).copied().filter(|_| band < self.bands)
    }

    #[must_use]
    pub fn program(&self, band: usize) -> Option<Seq> {
        self.programs
            .get(band)
            .copied()
            .flatten()
            .filter(|_| band < self.bands)
    }

    #[must_use]
    pub fn authority(&self, band: usize) -> Option<Seq> {
        self.authorities
            .get(band)
            .copied()
            .flatten()
            .filter(|_| band < self.bands)
    }

    pub(crate) fn publish_program(
        &mut self,
        band: usize,
        program: Option<Seq>,
        authority: Option<Seq>,
    ) {
        if let Some(value) = self.programs.get_mut(band) {
            *value = program;
        }
        if let Some(value) = self.authorities.get_mut(band) {
            *value = authority;
        }
    }

    pub(crate) fn set_layout(&mut self, layout: Option<LayoutRevision>) {
        self.layout = layout;
    }

    pub(crate) fn set_bands(&mut self, bands: usize) {
        self.bands = bands;
    }
}

/// One slot as its mixer last published it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SlotSnapshot {
    pub binding: Option<SlotBinding>,
    pub output: SlotOutputSnapshot,
    pub state: SlotState,
    pub mark: Option<SlotMark>,
    /// Raw decoded-source position in seconds.
    pub position: f64,
    /// Immutable source facts from the current segment's observed PCM or terminal packet.
    pub source: Option<AudioChunkInfo>,
    /// Decoded-ahead frontier in seconds, never behind `position`.
    pub frontier: f64,
    /// How much of the source is on disk, in seconds.
    pub cached: f64,
    /// The slot's envelope gain on its last mixed frame.
    pub gain: f32,
}
