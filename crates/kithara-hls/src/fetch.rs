#![forbid(unsafe_code)]

//! Backwards-compat shim for the deleted [`FetchManager`].
//!
//! After Phase 4a.5 the segment-loading + playlist + DRM
//! responsibilities live in [`crate::segment_loader::SegmentLoader`],
//! [`crate::playlist_cache::PlaylistCache`], and
//! [`crate::keys::KeyManager`]. This module exists only as a place to
//! re-export the public segment-data types so existing
//! `crate::fetch::SegmentMeta` paths keep resolving inside the crate.

pub use crate::segment_loader::{FetchResult, SegmentMeta, SegmentType};
