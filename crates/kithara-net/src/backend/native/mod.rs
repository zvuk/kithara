#[cfg(any(feature = "client-reqwest", feature = "client-wreq"))]
mod metrics;
#[cfg(all(feature = "client-reqwest", not(feature = "client-wreq")))]
mod reqwest;
#[cfg(any(feature = "client-reqwest", feature = "client-wreq"))]
mod shared;
#[cfg(feature = "client-wreq")]
mod wreq;

#[cfg(all(feature = "client-reqwest", not(feature = "client-wreq")))]
pub(crate) use self::reqwest::{
    BackendError, Client, ClientBuilder, RequestBuilder, Response, StatusCode, build_client,
};
#[cfg(any(feature = "client-reqwest", feature = "client-wreq"))]
pub(crate) use self::shared::{apply_compression, head_request, post_request};
#[cfg(feature = "client-wreq")]
pub(crate) use self::wreq::{
    BackendError, Client, ClientBuilder, RequestBuilder, Response, StatusCode, build_client,
};
