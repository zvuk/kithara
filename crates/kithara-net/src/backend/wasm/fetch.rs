pub(crate) use ::reqwest::{Client, RequestBuilder, Response};
use url::Url;

use crate::types::NetOptions;

pub(crate) type BackendError = ::reqwest::Error;

pub(crate) fn build_client(_options: &NetOptions) -> Result<Client, BackendError> {
    Client::builder().build()
}

pub(crate) fn head_request(client: &Client, url: Url) -> RequestBuilder {
    client.get(url).header("Range", "bytes=0-0")
}
