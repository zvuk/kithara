use std::num::NonZeroUsize;

use kithara_command::Seq;
use kithara_effects::GainDb;
use kithara_signal::AudioChunkInfo;

use super::{
    DeckParameter, FilterSnapshot, FxTarget, LayoutRevision, RtMetricsSnapshot, SlotBinding,
    SlotMark, SlotOutputSnapshot, SlotState,
};

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
    pub channels: [ParameterSnapshot; DeckParameter::CHANNELS],
    /// One entry per FX unit, in unit order.
    pub fx: Vec<FxSnapshot>,
}

/// One FX unit as its mixer last published it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FxSnapshot {
    /// The revision of the effect the unit holds, which addresses its programs.
    pub layout: Option<LayoutRevision>,
    /// The dry/wet the unit applied.
    pub mix: ParameterSnapshot,
}

impl DeckSnapshot {
    /// Allocates an empty snapshot for the deck's fixed slot, EQ and effect capacities.
    #[must_use]
    pub fn new(slots: NonZeroUsize, eq_bands: usize, fx_units: usize) -> Self {
        Self {
            slots: vec![SlotSnapshot::default(); slots.get()],
            eq: EqSnapshot {
                active: eq_bands,
                bands: vec![EqBandSnapshot::default(); eq_bands],
                layout: None,
            },
            fx: vec![FxSnapshot::default(); fx_units],
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
            DeckParameter::Fx {
                unit,
                layout,
                target,
            } => {
                let unit = self.fx.get(unit).filter(|fx| fx.layout == Some(layout))?;
                match target {
                    FxTarget::Mix => Some(unit.mix),
                    FxTarget::Param(_) => None,
                }
            }
            parameter => parameter
                .fixed()
                .and_then(|index| self.channels.get(index))
                .copied(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get_mut)]
pub struct EqSnapshot {
    /// Bands of the layout that receives commands; at most `bands.len()`.
    active: usize,
    /// Every band the snapshot has room for, active or not.
    #[field(get_mut = bands_mut, vis = "pub(crate)")]
    bands: Vec<EqBandSnapshot>,
    layout: Option<LayoutRevision>,
}

/// One EQ band as its mixer last published it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct EqBandSnapshot {
    pub(crate) gain: GainDb,
    pub(crate) killed: bool,
    program: Option<Seq>,
    authority: Option<Seq>,
}

impl EqSnapshot {
    #[must_use]
    pub fn layout(&self) -> Option<LayoutRevision> {
        self.layout
    }

    #[must_use]
    pub const fn bands(&self) -> usize {
        self.active
    }

    #[must_use]
    pub fn gain(&self, band: usize) -> Option<GainDb> {
        let record = self.band(band)?;
        Some(record.gain)
    }

    /// Whether kill is engaged for a band in the observed layout.
    #[must_use]
    pub fn killed(&self, band: usize) -> Option<bool> {
        let record = self.band(band)?;
        Some(record.killed)
    }

    #[must_use]
    pub fn program(&self, band: usize) -> Option<Seq> {
        let record = self.band(band)?;
        record.program
    }

    #[must_use]
    pub fn authority(&self, band: usize) -> Option<Seq> {
        let record = self.band(band)?;
        record.authority
    }

    fn band(&self, band: usize) -> Option<&EqBandSnapshot> {
        self.bands.get(band).filter(|_| band < self.active)
    }

    pub(crate) fn publish_program(
        &mut self,
        band: usize,
        program: Option<Seq>,
        authority: Option<Seq>,
    ) {
        if let Some(record) = self.bands.get_mut(band) {
            record.program = program;
            record.authority = authority;
        }
    }

    pub(crate) fn set_layout(&mut self, layout: Option<LayoutRevision>) {
        self.layout = layout;
    }

    pub(crate) fn set_active(&mut self, active: usize) {
        self.active = active;
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
    /// The slot's envelope gain on its last mixed frame.
    pub gain: f32,
}
