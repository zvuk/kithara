use std::{fmt::Write, num::NonZeroU16};

use url::Url;

pub use crate::client::HttpClient;
use crate::error::NetError;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native/mod.rs"]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use self::native::{
    BackendError, Client, RequestBuilder, Response, StatusCode, build_client, head_request,
    post_request,
};

#[cfg(target_arch = "wasm32")]
#[path = "wasm/mod.rs"]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub(crate) use self::wasm::{
    BackendError, Client, RequestBuilder, Response, StatusCode, build_client, head_request,
    post_request,
};

impl From<BackendError> for NetError {
    fn from(e: BackendError) -> Self {
        if e.is_timeout() {
            return Self::Timeout;
        }
        // WHY: non-status reqwest errors (connect, body-EOF, decode) stay retryable so an early stream close can resume; fatal Decode is for a local sink write (dl/response.rs).
        e.status()
            .and_then(|s| NonZeroU16::new(s.as_u16()))
            .map_or_else(
                || Self::Network(error_chain(&e)),
                |status| Self::Status {
                    status,
                    url: error_url(&e),
                    body: None,
                },
            )
    }
}

#[cfg(all(feature = "client-wreq", not(target_arch = "wasm32")))]
fn error_url(error: &BackendError) -> Option<Url> {
    let uri = error.uri()?.to_string();
    Url::parse(&uri).ok()
}

#[cfg(any(not(feature = "client-wreq"), target_arch = "wasm32"))]
fn error_url(error: &BackendError) -> Option<Url> {
    error.url().cloned()
}

fn error_chain(e: &BackendError) -> String {
    let mut msg = e.to_string();
    let mut current: &dyn std::error::Error = e;
    while let Some(source) = current.source() {
        let _ = write!(msg, ": {source}");
        current = source;
    }
    msg
}
