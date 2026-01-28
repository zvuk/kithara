//! Cache layer with segment loading.
//!
//! This module provides:
//! - `Loader`: Generic trait for loading segments
//! - `FetchLoader`: Adapter from FetchManager to Loader trait

mod fetcher;
mod loader;
mod types;

pub use fetcher::FetchLoader;
pub use loader::{Loader, MockLoader};
pub use types::{EncryptionInfo, SegmentMeta, SegmentType};
