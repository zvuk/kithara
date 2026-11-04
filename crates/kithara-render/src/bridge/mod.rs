pub mod channels;
mod mark;
pub mod metrics;
pub mod mix;
pub mod protocol;
pub mod snapshot;

pub use channels::{DeckEnds, DeckEvents, MixTapWriter, MixerInputs, SessionInbox, scope_channels};
pub use mark::SlotMark;
pub use metrics::{RtMetrics, RtMetricsSnapshot};
pub use mix::{CrossfaderGain, DeckMixSettings, DeckMixSettingsChange, DeckMixSettingsPatch, Trim};
pub use protocol::{
    DeckEqChange, DeckEvent, DeckFxChange, DeckPart, DeckProtocol, DeckRefusal, Fade, FadeDir,
    FxMix, PlaybackFault, Returned, Slot, SlotState,
};
pub use snapshot::{DeckSnapshot, EqSnapshot, SlotSnapshot};
