use kithara::effects::GainDb;

use crate::deck::TempoPercent;

pub(crate) struct DeckSettings {
    pub(crate) tempo: TempoPercent,
    pub(crate) eq_bands: Vec<GainDb>,
}

impl DeckSettings {
    pub(crate) fn new(bands: usize) -> Self {
        Self {
            eq_bands: vec![GainDb::default(); bands],
            tempo: TempoPercent::DEFAULT,
        }
    }
}
