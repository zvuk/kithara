//! Foreign observer traits for push-based property updates.
//!
//! Each observer has a single `on_event()` method receiving a typed enum
//! variant. This keeps the FFI surface minimal — Swift needs only one
//! callback bridge class per observer instead of one per property.

use crate::types::{FfiItemEvent, FfiPlayerEvent};

/// Receives player-level state changes from Rust.
///
/// All calls happen on an arbitrary background thread.
/// Swift implementations must dispatch to the main actor as needed.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait PlayerObserver: Send + Sync {
    fn on_event(&self, event: FfiPlayerEvent);
}

/// Receives item-level state changes from Rust.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait ItemObserver: Send + Sync {
    fn on_event(&self, event: FfiItemEvent);
}

/// Callback for seek completion.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait SeekCallback: Send + Sync {
    fn on_complete(&self, finished: bool);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[kithara::test]
    fn observer_traits_are_send_sync() {
        assert_send_sync::<dyn PlayerObserver>();
        assert_send_sync::<dyn ItemObserver>();
        assert_send_sync::<dyn SeekCallback>();
    }
}
