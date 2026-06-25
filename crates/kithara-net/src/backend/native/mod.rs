#[cfg(not(feature = "client-wreq"))]
mod reqwest;
mod shared;
#[cfg(feature = "client-wreq")]
mod wreq;

#[cfg(not(feature = "client-wreq"))]
pub(crate) use self::reqwest::{
    BackendError, Client, ClientBuilder, RequestBuilder, Response, build_client,
};
pub(crate) use self::shared::{apply_compression, head_request};
#[cfg(feature = "client-wreq")]
pub(crate) use self::wreq::{
    BackendError, Client, ClientBuilder, RequestBuilder, Response, build_client,
};
