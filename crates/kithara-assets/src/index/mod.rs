#![forbid(unsafe_code)]

mod lru;
mod pin;

pub use lru::EvictConfig;
pub(crate) use lru::LruIndex;
#[cfg_attr(not(feature = "internal"), expect(unreachable_pub))]
pub use pin::PinsIndex;
