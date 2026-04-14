//! Test fixtures for HLS integration tests.
//!
//! Server-side HLS helpers now live in `kithara-test-utils`; this module keeps
//! local asset and crypto helpers plus compatibility re-exports for tests.

pub mod assets;
pub mod builders;
pub mod crypto;

// Re-export commonly used types
pub use assets::*;
pub use builders::*;
#[cfg(not(target_arch = "wasm32"))]
pub use crypto::*;
#[cfg(target_arch = "wasm32")]
pub use crypto::{aes128_iv, aes128_plaintext_segment};
// Common types
use kithara::hls::HlsError;
pub use kithara_test_utils::hls_fixture::{
    AbrTestServer, EncryptionConfig, HlsTestServer, HlsTestServerConfig, PackagedTestServer,
    TestServer, abr, compat, master_playlist, packaged, packaged_test_server, test_master_playlist,
    test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist,
    test_media_playlist_encrypted, test_media_playlist_with_init, test_segment_data, test_server,
};

pub type HlsResult<T> = Result<T, HlsError>;
