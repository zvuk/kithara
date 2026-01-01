use bytes::Bytes;
use futures::StreamExt;
use thiserror::Error;

use crate::traits::Net;
use crate::types::{Headers, RangeSpec};

#[derive(Debug, Error, Clone)]
pub enum NetError {
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("Invalid range header: {0}")]
    InvalidRange(String),
    #[error("Timeout")]
    Timeout,
    #[error("Request failed after {max_retries} retries: {source}")]
    RetryExhausted {
        max_retries: u32,
        source: Box<NetError>,
    },
    #[error("HTTP {status} for URL: {url}")]
    HttpStatus { status: u16, url: String },
    #[error("not implemented")]
    Unimplemented,
}

pub type NetResult<T> = Result<T, NetError>;

/// Base HTTP client implementation using reqwest
#[derive(Clone, Debug)]
pub struct ReqwestNet {
    client: reqwest::Client,
}

impl ReqwestNet {
    pub fn new() -> Result<Self, NetError> {
        Self::with_options(None)
    }

    pub fn with_options(timeout: Option<std::time::Duration>) -> Result<Self, NetError> {
        let mut builder = reqwest::Client::builder().use_rustls_tls();
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }

        let client = builder.build().map_err(|e| NetError::Http(e.to_string()))?;

        Ok(Self { client })
    }

    pub fn with_timeout(timeout: std::time::Duration) -> Result<Self, NetError> {
        Self::with_options(Some(timeout))
    }

    fn build_request(
        &self,
        url: url::Url,
        headers: Option<Headers>,
        range: Option<RangeSpec>,
    ) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url);

        if let Some(headers) = headers {
            for (key, value) in headers.iter() {
                request = request.header(key, value);
            }
        }

        if let Some(range_spec) = range {
            request = request.header("Range", range_spec.to_header_value());
        }

        request
    }

    async fn handle_response(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, NetError> {
        let status = response.status();

        if status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT {
            Ok(response)
        } else {
            let url = response.url().to_string();
            Err(NetError::HttpStatus {
                status: status.as_u16(),
                url,
            })
        }
    }
}

impl Net for ReqwestNet {
    async fn get_bytes(&self, url: url::Url) -> Result<Bytes, NetError> {
        let request = self.build_request(url, None, None);
        let response = request.send().await?;
        let response = self.handle_response(response).await?;
        Ok(response.bytes().await?)
    }

    async fn stream(
        &self,
        url: url::Url,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let request = self.build_request(url, headers, None);
        let response = request.send().await?;
        let response = self.handle_response(response).await?;

        let stream = response
            .bytes_stream()
            .map(|result| result.map_err(NetError::from));

        Ok(Box::pin(stream))
    }

    async fn get_range(
        &self,
        url: url::Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let request = self.build_request(url, headers, Some(range));
        let response = request.send().await?;
        let response = self.handle_response(response).await?;

        let stream = response
            .bytes_stream()
            .map(|result| result.map_err(NetError::from));

        Ok(Box::pin(stream))
    }
}

impl From<reqwest::Error> for NetError {
    fn from(error: reqwest::Error) -> Self {
        NetError::Http(error.to_string())
    }
}

impl Default for ReqwestNet {
    fn default() -> Self {
        Self::new().expect("Failed to create default ReqwestNet client with no timeout")
    }
}
