//! FFI wrapper for `UniqueBinaryCipher` from `kithara-drm`.

use std::sync::Arc;

use bytes::Bytes;
use kithara_drm::UniqueBinaryCipher as RustCipher;
use kithara_platform::Mutex;

use crate::observer::FfiKeyProcessor;

/// Position-dependent symmetric cipher for DRM key decryption.
///
/// Wraps `kithara_drm::UniqueBinaryCipher` for use from Swift/Kotlin.
/// Also implements `FfiKeyProcessor` so it can be passed directly
/// to `AudioPlayer.setKeyProcessor()`.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct FfiCipher {
    inner: Mutex<RustCipher>,
}

#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl FfiCipher {
    /// Create a new cipher from a key string.
    ///
    /// Takes `&str` rather than `String` — modern `UniFFI` (≥0.28) marshals
    /// `&str` across the FFI boundary by cloning on the bridge side, which
    /// keeps Rust callers free of unnecessary allocations.
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(key: &str) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RustCipher::new(key)),
        })
    }

    /// Decrypt data using this cipher.
    pub fn decrypt(&self, data: Vec<u8>) -> Vec<u8> {
        self.inner.lock_sync().decrypt(&Bytes::from(data)).to_vec()
    }
}

#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl FfiKeyProcessor for FfiCipher {
    fn process_key(&self, key: Vec<u8>) -> Vec<u8> {
        self.decrypt(key)
    }
}
