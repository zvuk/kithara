pub(crate) use ::reqwest::{Client, RequestBuilder, Response, StatusCode};
use url::Url;

use crate::{metrics::ConnectionMetrics, types::NetOptions};

pub(crate) type BackendError = ::reqwest::Error;

pub(crate) fn build_client(
    _options: &NetOptions,
    _metrics: &ConnectionMetrics,
) -> Result<Client, BackendError> {
    Client::builder().build()
}

pub(crate) fn head_request(client: &Client, url: Url) -> RequestBuilder {
    client.get(url).header("Range", "bytes=0-0")
}
