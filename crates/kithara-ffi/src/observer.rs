//! Foreign observer traits for push-based property updates.
//!
//! Each observer has a single `on_event()` method receiving a typed enum
//! variant. This keeps the FFI surface minimal — Swift needs only one
//! callback bridge class per observer instead of one per property.

use crate::types::{FfiItemEvent, FfiPlayerEvent};

/// Receives player-level state changes from Rust.
///
/// All calls happen on an arbitrary background thread.
/// Platform bindings must dispatch to the UI thread as needed.
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

/// Callback for processing (decrypting) HLS encryption keys.
///
/// Called for each key fetched from the server. The implementation should
/// return the decrypted key bytes.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait FfiKeyProcessor: Send + Sync {
    fn process_key(&self, key: Vec<u8>) -> Vec<u8>;
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
