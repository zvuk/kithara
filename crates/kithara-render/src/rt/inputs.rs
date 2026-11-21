use kithara_command::ScopeId;
use ringbuf::{HeapProd, HeapRb, traits::Split};
use triple_buffer::{Input, triple_buffer};

use super::DeckMixerConfig;
use crate::bridge::{DeckEnds, DeckEvent, DeckEvents, DeckSnapshot};

/// Fixed mixer configuration and its publishing ends, consumed at construction.
#[non_exhaustive]
pub struct MixerInputs {
    pub(crate) scope: ScopeId,
    pub(crate) events: HeapProd<DeckEvent>,
    pub(crate) snapshot: Input<DeckSnapshot>,
    pub(crate) config: DeckMixerConfig,
}

impl MixerInputs {
    /// Allocates the deck's event and snapshot channels before mixer construction.
    #[must_use]
    pub fn new(scope: ScopeId, config: DeckMixerConfig) -> (DeckEnds, Self) {
        let slots = config.slots().get();
        let (events_tx, events_rx) =
            HeapRb::<DeckEvent>::new(slots * crate::consts::EVENTS_PER_SLOT).split();
        let initial = DeckSnapshot::new(
            config.slots(),
            config.eq_bands(),
            config.fx_units(),
            config.fx_parameters(),
        );
        let (snapshot_in, snapshot_out) = triple_buffer(&initial);
        (
            DeckEnds {
                scope,
                events: DeckEvents(events_rx),
                snapshot: snapshot_out,
            },
            Self {
                scope,
                config,
                events: events_tx,
                snapshot: snapshot_in,
            },
        )
    }
}
