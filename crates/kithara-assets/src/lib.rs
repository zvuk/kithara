#![forbid(unsafe_code)]

//! # kithara-assets
//!
//! Persistent disk assets store for Kithara.
//!
//! ## Public contract
//!
//! The explicit public contract is the [`AssetStore`] type alias.
//! Everything else should be considered an implementation detail (even if it is currently `pub`), and constructors must propagate the shared cancellation token (use `AssetStore::new`/`AssetStoreBuilder`).
//!
//! ## Key mapping (normative)
//!
//! Resources are addressed by strings chosen by higher layers:
//! - `asset_root`: e.g. `hex(hash(canonical_url))`
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
//! The pin is expressed as an RAII guard stored inside [`LeaseResource`]. Drop the handle to release
//! the pin.
//!
//! ## Global index (best-effort)
//!
//! `_index/*` stores small files used as best-effort metadata.
//! Filesystem remains the source of truth; indexes may be missing and can be rebuilt later.

mod backend;
mod base;
mod cache;
mod error;
mod evict;
mod index;
mod key;
mod lease;
mod mem_store;
mod process;
mod store;

// Public API - used by other crates
pub use backend::AssetsBackend;
pub use error::{AssetsError, AssetsResult};
pub use index::CoverageIndex;
pub use key::{ResourceKey, asset_root_for_url};
pub use process::ProcessChunkFn;
pub use store::{AssetResource, AssetStore, AssetStoreBuilder, MemStore, StoreOptions};

// Hidden re-exports (needed by type aliases or cross-crate internals, not end-user API)
#[doc(hidden)]
pub use base::{Assets, DiskAssetStore};
#[doc(hidden)]
pub use cache::CachedAssets;
#[doc(hidden)]
pub use evict::EvictAssets;
#[cfg(feature = "internal")]
pub use index::PinsIndex;
#[doc(hidden)]
pub use index::{DiskCoverage, EvictConfig};
#[cfg(feature = "internal")]
pub use key::canonicalize_for_asset;
#[doc(hidden)]
pub use kithara_bufpool::{BytePool, byte_pool};
#[doc(hidden)]
pub use lease::{LeaseAssets, LeaseGuard, LeaseResource};
#[doc(hidden)]
pub use mem_store::MemAssetStore;
#[doc(hidden)]
pub use process::{ProcessedResource, ProcessingAssets};
