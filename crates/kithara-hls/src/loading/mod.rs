#![forbid(unsafe_code)]

//! HLS loading subsystem.
//!
//! Single-responsibility types for fetching and caching the inputs the
//! HLS pipeline needs from the network:
//!
//! - [`PlaylistCache`] — master/media playlist fetch + parse + dedup
//! - [`SegmentLoader`] — init/media segment download + DRM context
//!   resolution + in-flight dedup
//! - [`KeyManager`] — DRM key fetch + processor invocation
//! - [`SizeMapProbe`] — `Content-Length` HEAD probes for size maps
//! - [`atomic_fetch::fetch_atomic_body`] — shared cache→download
//!   helper used by [`PlaylistCache`] and [`KeyManager`]
//!
//! All four types take their dependencies (`Downloader`, `AssetStore`,
//! `Headers`, `KeyOptions`) directly. There is no god-object façade
//! aggregating them.

pub(crate) mod atomic_fetch;
pub(crate) mod keys;
pub(crate) mod playlist_cache;
pub(crate) mod segment_loader;
pub(crate) mod size_probe;

pub use keys::KeyManager;
pub use playlist_cache::PlaylistCache;
pub use segment_loader::{SegmentLoader, SegmentMeta};
pub(crate) use size_probe::SizeMapProbe;
