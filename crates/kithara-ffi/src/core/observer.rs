use crate::types::{FfiItemEvent, FfiPlayerEvent};

/// Local shim so `#[kithara::mock]` resolves on wasm, which does not
/// depend on the `kithara` facade crate (only on `kithara-test-macros`).
/// On native the real `kithara` crate is in scope and provides the macro,
/// so the shim is wasm-only to avoid an ambiguous-name conflict.
#[cfg(target_arch = "wasm32")]
mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Receives player-level state changes from Rust.
///
/// All calls happen on an arbitrary background thread.
/// Platform bindings must dispatch to the UI thread as needed.
#[kithara::mock(api = PlayerObserverMock)]
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait PlayerObserver: Send + Sync {
    fn on_event(&self, event: FfiPlayerEvent);
}

/// Receives item-level state changes from Rust.
#[kithara::mock(api = ItemObserverMock)]
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait ItemObserver: Send + Sync {
    fn on_event(&self, event: FfiItemEvent);
}

/// Callback for seek completion.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait SeekCallback: Send + Sync {
    fn on_complete(&self, finished: bool);
}

/// Callback fired when [`crate::item::AudioPlayerItem::load`] resolves.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait ItemLoadCallback: Send + Sync {
    fn on_complete(&self, result: crate::types::FfiItemLoadResult);
}

/// Callback for processing (decrypting) HLS encryption keys.
///
/// Called for each key fetched from the server. `salt` carries the value
/// the player generated and attached to outgoing HTTP requests under
/// [`SALT_HEADER`] — implementations that derive a per-session cipher
/// from the salt should re-build it on every call. Implementations that
/// hold a pre-built cipher (legacy behaviour) can ignore the argument.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait FfiKeyProcessor: Send + Sync {
    fn process_key(&self, key: Vec<u8>, salt: String) -> Vec<u8>;
}

/// HTTP header name carrying the player-generated DRM salt. Mirrors the
/// value [`crate::player::AudioPlayer::setup_hls_aes`] writes into the
/// player-wide header map and forwards into [`FfiKeyProcessor::process_key`].
pub const SALT_HEADER: &str = "X-Encrypted-Key";

/// HTTP header name carrying the auth token configured via
/// [`crate::player::AudioPlayer::setup_network`].
pub const AUTH_TOKEN_HEADER: &str = "X-Auth-Token";

#[cfg(test)]
mod tests {
    use unimock::Unimock;

    use super::*;

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[kithara::test]
    fn observer_traits_are_send_sync() {
        assert_send_sync::<dyn PlayerObserver>();
        assert_send_sync::<dyn ItemObserver>();
        assert_send_sync::<dyn SeekCallback>();
    }

    #[kithara::test]
    fn observer_mock_apis_are_generated() {
        let _ = PlayerObserverMock::on_event;
        let _ = ItemObserverMock::on_event;
        let _ = Unimock::new(());
    }
}
