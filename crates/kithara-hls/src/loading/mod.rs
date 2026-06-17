#![forbid(unsafe_code)]

//! HLS loading subsystem.
//!
//! Single-responsibility helpers that pre-load everything `HlsVariant`
//! needs **before** it starts dispatching segment fetches:
//!
//! - [`PlaylistCache`] — master/media playlist fetch + parse + dedup
//! - [`KeyStore`] — DRM key fetch + processor invocation + sync
//!   [`DecryptContext`](kithara_drm::DecryptContext) derivation
//! - [`atomic_fetch::fetch_atomic_body`] — shared cache→download
//!   helper used by both
//!
//! Segment and init downloads are driven by [`HlsVariant::dispatch`](
//! crate::variant::HlsVariant) directly through
//! [`PlanCtx::asset_store`](crate::variant::PlanCtx) — there is no
//! separate `SegmentLoader` orchestrator.

pub(crate) mod atomic_fetch;
pub(crate) mod keys;
pub(crate) mod playlist_cache;
pub(crate) mod size_estimation;
pub use keys::KeyStore;
pub use playlist_cache::PlaylistCache;
