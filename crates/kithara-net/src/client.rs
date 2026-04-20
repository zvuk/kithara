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

/// HTTP 206 Partial Content status code.
const HTTP_PARTIAL_CONTENT: u16 = 206;

/// Truncate an HTTP error body so it stays useful in logs without dumping
/// kilobytes of HTML (rate-limit stubs, anti-bot challenges). Preserves
/// the first 200 characters (char-aligned to not split a UTF-8 codepoint)
/// and appends a `…(truncated, N chars total)` suffix for anything longer.
fn truncate_error_body(mut body: String) -> String {
    /// Maximum characters of an HTTP error body kept in
    /// [`NetError::HttpError`].
    const MAX_CHARS: usize = 200;

    let total = body.chars().count();
    if total <= MAX_CHARS {
        return body;
    }
    let cut_at = body
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(body.len(), |(i, _)| i);
    body.truncate(cut_at);
    body.push_str(&format!("…(truncated, {total} chars total)"));
    body
}

/// Extract response headers into our [`Headers`] type.
fn extract_headers(resp: &reqwest::Response) -> Headers {
    let mut headers = Headers::new();
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str(), v);
        }
    }
    headers
}

#[derive(Clone)]
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
        let builder = Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .pool_max_idle_per_host(options.pool_max_idle_per_host)
            .danger_accept_invalid_certs(options.insecure)
            .read_timeout(options.request_timeout);
        let inner = builder.build().expect("failed to build reqwest client");
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

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure or network error.
    pub async fn head(&self, url: Url, headers: Option<Headers>) -> NetResult<Headers> {
        <Self as Net>::head(self, url, headers).await
    }

    /// Convert a reqwest Response to a [`ByteStream`](crate::ByteStream).
    fn response_to_stream(resp: reqwest::Response) -> crate::ByteStream {
        let headers = extract_headers(&resp);
        let stream = resp.bytes_stream().map_err(NetError::from);
        crate::ByteStream::new(headers, Box::pin(stream))
    }
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Net for HttpClient {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        let req = self.inner.get(url.clone());
        let req = Self::apply_headers(req, headers);
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
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
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        Ok(Self::response_to_stream(resp))
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
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !(status.is_success() || status.as_u16() == HTTP_PARTIAL_CONTENT) {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        Ok(Self::response_to_stream(resp))
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        // On WASM, HEAD is often blocked by CORS (servers typically allow
        // only GET/OPTIONS). Use a zero-byte range GET instead — the 206
        // response carries the same metadata headers.
        #[cfg(target_arch = "wasm32")]
        let resp = {
            let mut req = self.inner.get(url.clone()).header("Range", "bytes=0-0");
            req = Self::apply_headers(req, headers);
            let req = req.timeout(self.options.request_timeout);
            req.send().await.map_err(NetError::from)?
        };

        #[cfg(not(target_arch = "wasm32"))]
        let resp = {
            let req = self.inner.head(url.clone());
            let req = Self::apply_headers(req, headers);
            let req = req.timeout(self.options.request_timeout);
            req.send().await.map_err(NetError::from)?
        };

        let status = resp.status();

        if !status.is_success() && status.as_u16() != HTTP_PARTIAL_CONTENT {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
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

        // For range GET responses, derive content-length from Content-Range.
        // Format: "bytes 0-0/12345678" → total = 12345678.
        if out.get("content-length").is_none() {
            let total_from_range = out
                .get("content-range")
                .and_then(|h| h.split('/').nth(1))
                .filter(|s| *s != "*")
                .map(str::to_owned);
            if let Some(total) = total_from_range {
                out.insert("content-length", total);
            }
        }

        Ok(out)
    }
}
