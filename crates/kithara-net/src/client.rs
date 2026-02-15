use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use reqwest::Client;
use url::Url;

use crate::{
    error::{NetError, NetResult},
    traits::Net,
    types::{Headers, NetOptions, RangeSpec},
};

#[derive(Clone, Debug)]
pub struct HttpClient {
    inner: Client,
    options: NetOptions,
}

impl HttpClient {
    /// # Panics
    ///
    /// Panics if the `reqwest::Client` builder fails to build.
    #[must_use]
    pub fn new(options: NetOptions) -> Self {
        let inner = Client::builder()
            .pool_max_idle_per_host(options.pool_max_idle_per_host)
            .build()
            .expect("failed to build reqwest client");
        Self { inner, options }
    }

    fn apply_headers(
        mut req: reqwest::RequestBuilder,
        headers: Option<Headers>,
    ) -> reqwest::RequestBuilder {
        if let Some(headers) = headers {
            for (k, v) in headers.iter() {
                req = req.header(k, v);
            }
        }
        req
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, timeout, or network error.
    pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes> {
        <Self as Net>::get_bytes(self, url, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure or network error.
    pub async fn stream(&self, url: Url, headers: Option<Headers>) -> NetResult<crate::ByteStream> {
        <Self as Net>::stream(self, url, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure or network error.
    pub async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> NetResult<crate::ByteStream> {
        <Self as Net>::get_range(self, url, range, headers).await
    }
}

#[async_trait]
impl Net for HttpClient {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        let req = self.inner.get(url.clone());
        let req = Self::apply_headers(req, headers);
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        resp.bytes().await.map_err(NetError::from)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn stream(
        &self,
        url: Url,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let req = self.inner.get(url.clone());
        let req = Self::apply_headers(req, headers);
        // No timeout for streaming - downloads can take arbitrary time

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        let stream = resp.bytes_stream().map_err(NetError::from);
        Ok(Box::pin(stream))
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let mut req = self
            .inner
            .get(url.clone())
            .header("Range", range.to_header_value());
        req = Self::apply_headers(req, headers);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !(status.is_success() || status.as_u16() == 206) {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        let stream = resp.bytes_stream().map_err(NetError::from);
        Ok(Box::pin(stream))
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        let req = self.inner.head(url.clone());
        let req = Self::apply_headers(req, headers);
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        let mut out = Headers::new();
        for (name, value) in resp.headers() {
            if let Ok(v) = value.to_str() {
                out.insert(name.as_str(), v);
            }
        }

        Ok(out)
    }
}
