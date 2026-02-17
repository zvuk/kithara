#![allow(clippy::missing_errors_doc, clippy::ignored_unit_patterns)]

mod asset;
pub mod dj;
mod engine;
mod error;
mod events;
mod item;
mod metadata;
mod mixer;
mod player;
mod queue;
mod session;
mod time;
mod types;

pub use asset::Asset;
pub use engine::Engine;
pub use error::PlayError;
pub use events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, PlayerEvent, RouteChangeReason, SessionEvent,
};
pub use item::PlayerItem;
pub use metadata::{Artwork, Metadata};
pub use mixer::Mixer;
pub use player::Player;
pub use queue::QueuePlayer;
pub use session::{
    AudioSession, PortDescription, PortType, RouteDescription, SessionCategory, SessionMode,
    SessionOptions,
};
pub use time::MediaTime;
pub use types::{
    ActionAtItemEnd, EqBand, ItemStatus, ObserverId, PlayerStatus, SlotId, TimeControlStatus,
    TimeRange, WaitingReason,
};

pub use dj::bpm::{BeatGrid, BpmAnalyzer, BpmInfo, BpmSync};
pub use dj::crossfade::{CrossfadeConfig, CrossfadeController, CrossfadeCurve};
pub use dj::effects::{DjEffect, DjEffectKind};
pub use dj::eq::{EqConfig, Equalizer};

#[cfg(any(test, feature = "test-utils"))]
pub use asset::AssetMock;
#[cfg(any(test, feature = "test-utils"))]
pub use dj::bpm::{BpmAnalyzerMock, BpmSyncMock};
#[cfg(any(test, feature = "test-utils"))]
pub use dj::crossfade::CrossfadeControllerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use dj::effects::DjEffectMock;
#[cfg(any(test, feature = "test-utils"))]
pub use dj::eq::EqualizerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use engine::EngineMock;
#[cfg(any(test, feature = "test-utils"))]
pub use mixer::MixerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use session::AudioSessionMock;
