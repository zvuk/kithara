pub use crate::client::HttpClient;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native/mod.rs"]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use self::native::{
    BackendError, Client, RequestBuilder, Response, StatusCode, build_client, head_request,
};

#[cfg(target_arch = "wasm32")]
#[path = "wasm/mod.rs"]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub(crate) use self::wasm::{
    BackendError, Client, RequestBuilder, Response, StatusCode, build_client, head_request,
};
