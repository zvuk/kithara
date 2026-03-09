#![forbid(unsafe_code)]

pub mod engine {
    pub use crate::{
        EngineConfig, EngineEvent, EngineImpl, PlayError, SessionDuckingMode, SlotId,
        traits::{dj::crossfade::CrossfadeConfig, engine::Engine},
    };

    #[must_use]
    pub fn slot_id(value: u64) -> SlotId {
        SlotId(value)
    }
}

pub use crate::{
    ActionAtItemEnd, DjEvent, EngineConfig, EngineEvent, EngineImpl, ItemStatus, MediaTime,
    ObserverId, PlayError, PlayerConfig, PlayerEvent, PlayerImpl, PlayerStatus, Resource,
    ResourceConfig, ResourceSrc, SessionDuckingMode, SessionEvent, SlotId, SourceType,
    TimeControlStatus, TimeRange, WaitingReason,
};
