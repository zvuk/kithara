//! Cache layer with offset mapping and generic loader.
//!
//! This module provides:
//! - `OffsetMap`: Maps byte offsets to segments for a single variant
//! - `Loader`: Generic trait for loading segments
//! - `CachedLoader`: Full Source implementation with caching and ABR support
//! - `FetchLoader`: Adapter from FetchManager to Loader trait

mod cached_loader;
mod fetch_loader;
mod loader;
mod offset_map;
mod types;

pub use cached_loader::CachedLoader;
pub use fetch_loader::FetchLoader;
pub use loader::{Loader, MockLoader};
pub use offset_map::{CachedSegment, OffsetMap};
pub use types::{EncryptionInfo, SegmentMeta, SegmentType};
