pub mod channels;
mod curve;
mod eq_program;
mod filter;
mod filter_program;
mod gain;
mod mark;
pub mod metrics;
pub mod mix;
pub mod protocol;
pub mod snapshot;

pub use channels::{DeckEnds, DeckEvents, MixTapWriter, MixerInputs, SessionInbox, scope_channels};
pub(crate) use curve::ParameterCurve;
pub use curve::{ParameterCurveError, ParameterKnot};
pub use eq_program::{EqBinding, EqGainProgram};
pub use filter::{DeckFilterChange, FilterBinding, FilterSnapshot};
pub use filter_program::FilterProgram;
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
