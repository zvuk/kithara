//! # Kithara Broadcast
//!
//! Live HLS origin: ADTS framing of AAC-LC access units, media-clock segment
//! rotation, a sliding playlist window, and the HTTP service that serves them.

mod adts;
mod config;
mod error;
mod feed;
mod id3;
mod segment;
mod server;
mod service;
mod window;

pub use config::BroadcastConfig;
pub use error::{BroadcastError, BroadcastResult};
pub use feed::{FeedChunk, LivePcmFeed, RingFeed};
pub use segment::{Segment, Segmenter};
pub use service::{Broadcast, BroadcastHandle, BroadcastStatus};
pub use window::{LiveWindow, PlaylistSnapshot};
