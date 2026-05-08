//! Offline audio backend used by integration tests.
//!
//! This is a test-only firewheel `AudioBackend` plus a per-instance
//! [`OfflineSession`] that drives the audio graph without touching real
//! hardware. Hosts wire it into [`kithara_play::EngineConfig::session`]
//! so tests can render samples deterministically.

pub mod backend;
pub mod player;
pub mod session;

pub use backend::{OfflineBackend, OfflineConfig, OfflineError};
pub use player::{
    NotificationKind, OfflinePlayer, resource_from_reader, resource_from_reader_with_src,
};
pub use session::OfflineSession;
