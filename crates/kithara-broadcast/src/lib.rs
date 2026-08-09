//! Live HLS packaging and origin service for AAC-LC PCM feeds.

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
