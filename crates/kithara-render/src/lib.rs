#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Render stages for Kithara playback: the producer side that renders decoded
//! audio through Warp, and the deck that mixes loaded tracks on the audio
//! thread.

pub mod bridge;
mod consts;
mod crossfade;
mod dispatcher;
mod lane;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod priority;
pub mod rt;
mod source;
mod worker;
pub use crossfade::{
    CrossfadeCurve, CrossfadeSettings, CrossfadeSettingsPatch, CrossfadeSettingsPatchError,
    InvalidCrossfade,
};
pub use dispatcher::{
    Dispatched, DispatcherCommand, DispatcherProtocol, LaneId, LaneStart, LaneTask, LoadRequest,
    Loaded, Open, OpenResult, dispatch,
};
use humantime_serde as _;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use lane::{LaneApplied, LaneCommand, LaneFrame, LaneProtocol, LaneSetup};
pub use priority::ServiceClass;
pub use source::WarpSource;
pub use worker::{
    DecoderNode, EngineLoad, EngineLoadSnapshot, LoadRefusal, PcmPacket, PcmReceiver, PlayWorker,
    PlayWorkerConfig, PlayWorkerConfigPatch, StreamWake, TrackConfig,
};
