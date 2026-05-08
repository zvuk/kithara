#![forbid(unsafe_code)]

pub mod engine {
    #[rustfmt::skip]
    pub use crate::traits::dj::crossfade::CrossfadeConfig;
    #[rustfmt::skip]
    pub use crate::traits::engine::Engine;
    pub use crate::{EngineConfig, EngineEvent, EngineImpl, PlayError, SessionDuckingMode, SlotId};

    #[must_use]
    pub fn slot_id(value: u64) -> SlotId {
        SlotId::new(value)
    }
}

pub use crate::{
    ActionAtItemEnd, DjEvent, EngineConfig, EngineEvent, EngineImpl, ItemStatus, MediaTime,
    ObserverId, PlayError, PlayerConfig, PlayerEvent, PlayerImpl, PlayerStatus, Resource,
    ResourceConfig, ResourceSrc, SessionDuckingMode, SessionEvent, SlotId, SourceType,
    TimeControlStatus, TimeRange, WaitingReason,
};
