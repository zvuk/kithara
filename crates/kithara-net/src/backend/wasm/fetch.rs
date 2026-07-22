pub(crate) use ::reqwest::{Client, RequestBuilder, Response, StatusCode};
use bytes::Bytes;
use url::Url;

use crate::{metrics::ConnectionMetrics, types::NetOptions};

pub(crate) type BackendError = ::reqwest::Error;

pub(crate) fn build_client(
    _options: &NetOptions,
    _metrics: &ConnectionMetrics,
) -> Result<Client, BackendError> {
    Client::builder().build()
}

pub(crate) fn head_request(client: &Client, url: &Url) -> RequestBuilder {
    client.get(url.as_str()).header("Range", "bytes=0-0")
}

pub(crate) fn post_request(client: &Client, url: &Url, body: Bytes) -> RequestBuilder {
    client.post(url.as_str()).body(body)
}
