//! Live HLS packaging and origin service for AAC-LC master PCM.

mod adts;
mod config;
mod error;
mod id3;
mod segment;
#[cfg(not(target_arch = "wasm32"))]
mod server;
#[cfg(not(target_arch = "wasm32"))]
mod service;
mod window;

pub use config::{BroadcastConfig, BroadcastConfigPatch};
pub use error::{BroadcastError, BroadcastResult};
pub use segment::{Segment, Segmenter};
#[cfg(not(target_arch = "wasm32"))]
pub use service::{Broadcast, BroadcastHandle, BroadcastOutput, BroadcastStatus};
pub use window::{LiveWindow, PlaylistSnapshot};
