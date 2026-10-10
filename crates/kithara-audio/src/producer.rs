use kithara_events::EventSet;

use crate::{AudioEvent, DecoderEvent};

/// Deferred diagnostics from the owner-thread decoder chain.
#[derive(Clone, Debug, EventSet)]
#[non_exhaustive]
pub enum AudioLaneEvent {
    Decoder(DecoderEvent),
    Audio(AudioEvent),
}
