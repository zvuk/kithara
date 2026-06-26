#[cfg(any(
    not(any(
        feature = "client-reqwest",
        feature = "client-wreq",
        feature = "client-apple"
    )),
    all(target_arch = "wasm32", not(feature = "client-reqwest"))
))]
compile_error!(
    "kithara-net: enable at least one HTTP client backend; wasm32 requires `client-reqwest`"
);

#[cfg(all(feature = "client-apple", any(target_os = "macos", target_os = "ios")))]
#[path = "apple/mod.rs"]
mod selected;
#[cfg(all(feature = "client-apple", any(target_os = "macos", target_os = "ios")))]
pub use self::selected::HttpClient;

#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
mod selected;
#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
pub use self::selected::HttpClient;
#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
pub(crate) use self::selected::{
    BackendError, Client, RequestBuilder, Response, StatusCode, build_client, head_request,
};
