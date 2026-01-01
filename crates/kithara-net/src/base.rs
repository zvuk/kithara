use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;

use crate::error::NetError;
use crate::traits::Net;
use crate::types::{Headers, RangeSpec};

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
            Err(NetError::http_status(status.as_u16(), url))
        }
    }
}

#[async_trait]
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

impl Default for ReqwestNet {
    fn default() -> Self {
        Self::new().expect("Failed to create default ReqwestNet client with no timeout")
    }
}
