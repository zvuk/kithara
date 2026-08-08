//! # Kithara Broadcast
//!
//! Live HLS packaging core: ADTS framing of AAC-LC access units behind the
//! RFC 8216 §3.4 timestamp tag, media-clock segment rotation, and a sliding
//! playlist window that renders the media playlist text.

mod adts;
mod config;
mod error;
mod id3;
mod segment;
mod window;

pub use config::BroadcastConfig;
pub use error::{BroadcastError, BroadcastResult};
pub use segment::{Segment, Segmenter};
pub use window::{LiveWindow, PlaylistSnapshot};
