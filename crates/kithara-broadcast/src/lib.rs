//! # Kithara Broadcast
//!
//! Live HLS packaging core: ADTS framing of AAC-LC access units, media-clock
//! segment rotation, and a sliding playlist window that renders the media
//! playlist text.

mod adts;
mod error;
mod segment;
mod window;

pub use error::{BroadcastError, BroadcastResult};
pub use segment::{Segment, Segmenter};
pub use window::{LiveWindow, PlaylistSnapshot};
