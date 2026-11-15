#[cfg(all(not(target_arch = "wasm32"), feature = "client-wreq"))]
mod certificate;
#[cfg(all(not(target_arch = "wasm32"), feature = "client-wreq"))]
mod emulate;
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod metrics;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "client-wreq")))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod shared;
mod transport;
#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(all(not(target_arch = "wasm32"), feature = "client-wreq"))]
mod wreq;

pub(crate) use transport::Transport;

#[cfg(all(not(target_arch = "wasm32"), not(feature = "client-wreq")))]
pub(crate) use self::native::{BackendError, Client, ClientBuilder, build_client};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use self::shared::apply_compression;
#[cfg(target_arch = "wasm32")]
pub(crate) use self::wasm::{BackendError, Client, build_client};
#[cfg(all(not(target_arch = "wasm32"), feature = "client-wreq"))]
pub(crate) use self::wreq::{BackendError, Client, ClientBuilder, build_client};
