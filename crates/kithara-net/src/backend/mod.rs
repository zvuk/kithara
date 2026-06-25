#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use self::native::{
    BackendError, Client, RequestBuilder, Response, build_client, head_request,
};

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub(crate) use self::wasm::{
    BackendError, Client, RequestBuilder, Response, build_client, head_request,
};
