pub mod assets;
pub mod builders;
pub mod crypto;

pub use assets::*;
pub use builders::*;
#[cfg(not(target_arch = "wasm32"))]
pub use crypto::*;
#[cfg(target_arch = "wasm32")]
pub use crypto::{aes128_iv, aes128_plaintext_segment};
use kithara::hls::HlsError;

pub type HlsResult<T> = Result<T, HlsError>;
