#![forbid(unsafe_code)]

//! # kithara-assets
//!
//! Persistent disk assets store for Kithara.
//!
//! ## Public contract
//!
//! The explicit public contract is the [`AssetStore`] type alias.
//! Everything else should be considered an implementation detail (even if it is currently `pub`).
//!
//! ## Key mapping (normative)
//!
//! Resources are addressed by strings chosen by higher layers:
//! - `asset_root`: e.g. hex(AssetId)
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
//! ## Global index (best-effort)
//!
//! `_index/*` stores small, atomic files (temp → rename) used as best-effort metadata.
//! Filesystem remains the source of truth; indexes may be missing and can be rebuilt later.

mod asset_id;
mod cache;
mod canonicalization;
mod error;
mod evict;
mod index;
mod key;
mod lease;
mod resource;
mod store;

// Public API - used by other crates
pub use asset_id::AssetId;
// Internal types - exported only with "internal" feature flag
#[cfg(feature = "internal")]
pub use cache::Assets;
#[cfg(feature = "internal")]
pub use canonicalization::{canonicalize_for_asset, canonicalize_for_resource};
pub use error::{AssetsError, AssetsResult};
pub use evict::EvictAssets;
pub use index::EvictConfig;
#[cfg(feature = "internal")]
pub use index::PinsIndex;
pub use key::ResourceKey;
pub use lease::{LeaseAssets, LeaseGuard};
pub use resource::AssetResource;
pub use store::{AssetStore, AssetStoreBuilder, DiskAssetStore};
