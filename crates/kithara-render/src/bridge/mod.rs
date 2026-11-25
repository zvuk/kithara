pub mod channels;
mod filter;
mod gain;
mod mark;
pub mod metrics;
pub mod mix;
mod offline;
mod program;
pub mod protocol;
pub mod snapshot;

pub use channels::{DeckEnds, DeckEvents, MixTapWriter, SessionInbox};
pub use filter::FilterSnapshot;
pub use gain::{SlotBinding, SlotGainProgram, SlotOutputSnapshot};
pub use kithara_dsp::curve::{CurveShape, CurveShapeError};
pub use mark::SlotMark;
pub use metrics::{RtMetrics, RtMetricsSnapshot};
pub use mix::{CrossfaderGain, DeckMixSettings, DeckMixSettingsChange, DeckMixSettingsPatch, Trim};
pub use offline::OfflineDeckError;
pub(crate) use program::{CurveCursor, ParameterCurve};
pub use program::{
    DeckParameter, DeckProgram, FxTarget, LayoutRevision, ParameterCurveError, ParameterKnot,
};
pub use protocol::{
    DeckEqChange, DeckEvent, DeckFxChange, DeckPart, DeckProtocol, DeckRefusal, Fade, FadeDir,
    FxMix, PlaybackFault, Returned, Slot, SlotState, TrackClock,
};
pub use snapshot::{DeckSnapshot, EqSnapshot, FxSnapshot, ParameterSnapshot, SlotSnapshot};
