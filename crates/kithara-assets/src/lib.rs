#![forbid(unsafe_code)]

//! # kithara-assets
//!
//! Persistent disk assets store for Kithara.
//!
//! ## Public contract
//!
//! The explicit public contract is the [`Assets`] trait.
//! Everything else should be considered an implementation detail.
//!
//! ## Key mapping (normative)
//!
//! Resources are addressed by strings chosen by higher layers:
//! - `asset_root`: e.g. hex(AssetId) / ResourceHash
//! - `rel_path`: e.g. `media/audio.mp3`, `segments/0001.m4s`
//!
//! Disk mapping is:
//! - `<cache_root>/<asset_root>/<rel_path>`
//!
//! Assets does not "invent" paths; it only enforces safety (no absolute paths, no `..`, no empty
//! segments).
//!
//! ## Auto-pin (lease) semantics
//!
//! All resources opened through the leasing decorator (`LeaseAssets`) are automatically pinned by
//! `asset_root` for the lifetime of the returned handle.
//!
//! The pin is expressed as an RAII guard stored inside [`AssetResource`]. Drop the handle to release
//! the pin.
//!
//! ## Best-effort metadata (`_index/*`)
//!
//! The `_index/*` namespace is reserved for small, best-effort metadata files stored via
//! `kithara-storage::AtomicResource` (temp → rename).
//!
//! Current metadata used by this crate:
//! - `_index/pins.json`: persisted pin table written by the `LeaseAssets` decorator.
//!
//! Filesystem remains the source of truth; metadata may be missing and can be rebuilt/recreated
//! by higher layers (e.g. by reopening assets and repinning).

mod cache;
mod error;
mod index;
mod key;
mod lease;
mod resource;
mod store;

// Re-exports
pub use cache::Assets;
pub use error::{CacheError, CacheResult};
pub use index::{AssetIndex, AssetIndexEntry, ResourceStatus};
pub use key::ResourceKey;
pub use lease::{LeaseAssets, LeaseAssetsExt, LeaseGuard};
pub use resource::AssetResource;
pub use store::{AssetStore, DiskAssetStore, asset_store};
