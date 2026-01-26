//! Cache layer with segment loading.
//!
//! This module provides:
//! - `Loader`: Generic trait for loading segments
//! - `FetchLoader`: Adapter from FetchManager to Loader trait

mod fetch_loader;
mod loader;
mod types;

pub use fetch_loader::FetchLoader;
pub use loader::{Loader, MockLoader};
pub use types::{EncryptionInfo, SegmentMeta, SegmentType};
