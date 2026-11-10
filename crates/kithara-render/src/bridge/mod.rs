mod binding;
pub mod channels;
mod curve;
mod eq_program;
mod filter;
mod gain;
mod mark;
pub mod metrics;
pub mod mix;
pub mod protocol;
pub mod snapshot;

pub use binding::{EffectBinding, EqBinding, FilterBinding};
pub use channels::{DeckEnds, DeckEvents, MixTapWriter, MixerInputs, SessionInbox, scope_channels};
pub(crate) use curve::ParameterCurve;
pub use curve::{ParameterCurveError, ParameterKnot};
pub use eq_program::EqGainProgram;
pub use filter::{DeckFilterChange, FilterSnapshot};
pub use gain::{SlotBinding, SlotGainProgram, SlotOutputSnapshot};
pub use kithara_dsp::curve::{CurveShape, CurveShapeError};
pub use mark::SlotMark;
pub use metrics::{RtMetrics, RtMetricsSnapshot};
pub use mix::{CrossfaderGain, DeckMixSettings, DeckMixSettingsChange, DeckMixSettingsPatch, Trim};
pub use protocol::{
    DeckEqChange, DeckEvent, DeckFxChange, DeckPart, DeckProtocol, DeckRefusal, Fade, FadeDir,
    FxMix, PlaybackFault, Returned, Slot, SlotState, TrackClock,
};
pub use snapshot::{DeckSnapshot, EqSnapshot, SlotSnapshot};
