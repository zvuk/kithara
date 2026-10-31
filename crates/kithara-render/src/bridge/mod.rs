pub mod channels;
mod curve;
mod gain;
mod mark;
pub mod metrics;
pub mod mix;
pub mod protocol;
pub mod snapshot;

pub use channels::{DeckEnds, DeckEvents, MixTapWriter, MixerInputs, SessionInbox, scope_channels};
pub(crate) use curve::ParameterCurve;
pub use curve::{ParameterCurveError, ParameterKnot};
pub use gain::{SlotBinding, SlotGainProgram, SlotOutputSnapshot};
pub use kithara_dsp::param::{CurveShape, CurveShapeError};
pub use mark::SlotMark;
pub use metrics::{RtMetrics, RtMetricsSnapshot};
pub use mix::{
    DeckMixSettings, DeckMixSettingsChange, DeckMixSettingsPatch, DeckMixSettingsPatchError,
    InvalidMixLevel,
};
pub use protocol::{
    DeckEqChange, DeckEvent, DeckPart, DeckProtocol, DeckRefusal, Fade, FadeDir, PlaybackFault,
    Returned, Slot, SlotState,
};
pub use snapshot::{DeckSnapshot, EqSnapshot, SlotSnapshot};
