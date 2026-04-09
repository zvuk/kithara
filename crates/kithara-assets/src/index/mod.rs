#![forbid(unsafe_code)]

// Phase P-1 adds `availability` but does not re-export anything to
// crate scope yet — Phase P-2 will wire it into `AssetStore` and pull
// the handful of items it actually needs. Until then callers reach the
// module via its full path if they want to (tests only).
#[expect(dead_code, reason = "wired from Phase P-2")]
pub(crate) mod availability;
mod lru;
mod pin;

pub use lru::EvictConfig;
pub(crate) use lru::LruIndex;
pub(crate) use pin::PinsIndex;
