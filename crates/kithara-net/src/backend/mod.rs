mod guards;

#[cfg(not(reqwest_backend))]
mod common;
#[cfg(not(reqwest_backend))]
mod pooled;

#[cfg(feature = "client-host")]
pub(crate) mod host;
#[cfg(feature = "client-host")]
pub use self::host::HttpClient;

#[cfg(apple_backend)]
#[path = "apple/mod.rs"]
mod selected;
#[cfg(apple_backend)]
pub use self::selected::HttpClient;

#[cfg(reqwest_backend)]
mod selected;
#[cfg(reqwest_backend)]
pub use self::selected::HttpClient;
#[cfg(reqwest_backend)]
pub(crate) use self::selected::{
    Client, RequestBuilder, Response, StatusCode, build_client, head_request, post_request,
};
