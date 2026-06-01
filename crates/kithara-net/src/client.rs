use std::{num::NonZeroU16, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use kithara_platform::CancellationToken;
use reqwest::Client;
use url::Url;

use crate::{
    error::{NetError, NetResult},
    retry::{DefaultRetryPolicy, RetryNet},
    traits::{Net, NetExt},
    types::{Compression, Headers, NetOptions, RangeSpec},
};

/// HTTP 206 Partial Content status code.
const HTTP_PARTIAL_CONTENT: u16 = 206;

/// Truncate an HTTP error body so it stays useful in logs without dumping
/// kilobytes of HTML (rate-limit stubs, anti-bot challenges). Preserves
/// the first 200 characters (char-aligned to not split a UTF-8 codepoint)
/// and appends a `…(truncated, N chars total)` suffix for anything longer.
fn truncate_error_body(mut body: String) -> String {
    /// Maximum characters of an HTTP error body kept in
    /// [`NetError::Status`].
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

fn status_error(url: Url, status: u16, body: String) -> NetError {
    match NonZeroU16::new(status) {
        Some(status) => NetError::Status {
            status,
            url: Some(url),
            body: Some(body),
        },
        None => NetError::Network(format!("unexpected zero HTTP status for {url}")),
    }
}

/// Build a `reqwest::Client` with our default configuration. Native
/// build applies pool / TLS / read-timeout knobs; wasm32 takes the
/// builder defaults because most options aren't supported there.
#[cfg(not(target_arch = "wasm32"))]
type ClientBuilderMod = fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder;

#[cfg(not(target_arch = "wasm32"))]
impl From<Compression> for Vec<ClientBuilderMod> {
    fn from(c: Compression) -> Self {
        [
            (
                Compression::GZIP,
                reqwest::ClientBuilder::no_gzip as ClientBuilderMod,
            ),
            (Compression::DEFLATE, reqwest::ClientBuilder::no_deflate),
            (Compression::BROTLI, reqwest::ClientBuilder::no_brotli),
            (Compression::ZSTD, reqwest::ClientBuilder::no_zstd),
        ]
        .into_iter()
        .filter(|(flag, _)| !c.contains(*flag))
        .map(|(_, disable)| disable)
        .collect()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn build_client(options: &NetOptions) -> reqwest::Result<Client> {
    let base = Client::builder()
        .cookie_store(true)
        .pool_max_idle_per_host(options.pool_max_idle_per_host)
        .pool_idle_timeout(Some(std::time::Duration::from_secs(5)))
        .danger_accept_invalid_certs(options.is_insecure)
        .read_timeout(options.inactivity_timeout);
    Vec::<ClientBuilderMod>::from(options.compression)
        .into_iter()
        .fold(base, |b, disable| disable(b))
        .build()
}

#[cfg(target_arch = "wasm32")]
fn build_client(_options: &NetOptions) -> reqwest::Result<Client> {
    Client::builder().build()
}

/// Extract response headers into our [`Headers`] type.
fn extract_headers(resp: &reqwest::Response) -> Headers {
    let mut headers = Headers::new();
    let str_pairs = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v)));
    for (name, value) in str_pairs {
        headers.insert(name, value);
    }
    headers
}

/// Raw HTTP client (one `reqwest::Client`, no retry layer). Lives
/// behind [`HttpClient`]'s [`RetryNet`] decorator — exposed only via
/// the [`Net`] trait, never constructed by callers directly.
#[derive(Clone)]
struct RawHttp {
    inner: Client,
    options: NetOptions,
}

impl RawHttp {
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

    #[cfg(not(target_arch = "wasm32"))]
    fn head_request(&self, url: Url) -> reqwest::RequestBuilder {
        self.inner.head(url)
    }

    #[cfg(target_arch = "wasm32")]
    fn head_request(&self, url: Url) -> reqwest::RequestBuilder {
        self.inner.get(url).header("Range", "bytes=0-0")
    }

    fn response_to_stream(resp: reqwest::Response) -> crate::ByteStream {
        let headers = extract_headers(&resp);
        let stream = resp.bytes_stream().map_err(NetError::from);
        crate::ByteStream::new(headers, Box::pin(stream))
    }

    async fn send_checked(
        &self,
        req: reqwest::RequestBuilder,
        headers: Option<Headers>,
        url: Url,
        accept_partial: bool,
    ) -> Result<reqwest::Response, NetError> {
        let req = Self::apply_headers(req, headers);
        let req = if let Some(total) = self.options.total_timeout {
            req.timeout(total)
        } else {
            req
        };
        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        let ok = status.is_success() || (accept_partial && status.as_u16() == HTTP_PARTIAL_CONTENT);
        if !ok {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
            return Err(status_error(url, status.as_u16(), body));
        }

        Ok(resp)
    }
}

/// Production HTTP client used across the workspace. Wraps a raw
/// `reqwest::Client` with the workspace's [`RetryNet`] decorator so
/// every [`Net`] method (`head`/`get_bytes`/`get_range`/`stream`) honours
/// `options.retry_policy` — retryable errors (TLS-close, timeout,
/// 5xx, IO) are re-issued with exponential backoff; non-retryable
/// errors (HTTP 4xx, cancellation) propagate immediately.
#[derive(Clone)]
pub struct HttpClient {
    net: Arc<RetryNet<RawHttp, DefaultRetryPolicy>>,
    options: NetOptions,
}

impl HttpClient {
    /// Build a retry-decorated HTTP client rooted on `cancel`. The
    /// `RetryNet` layer aborts pending retries when that token is
    /// cancelled. Callers MUST pass a token that lives in the
    /// consumer-crate's cancel tree — typically
    /// `master_cancel.child_token()` derived at the consumer-crate top
    /// (`App`, `Queue`, FFI player). The workspace cancel hierarchy
    /// forbids orphan tokens in production code.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest::Client` builder fails to build.
    #[must_use]
    pub fn new(options: NetOptions, cancel: CancellationToken) -> Self {
        let inner = build_client(&options)
            .expect("BUG: reqwest::Client::builder().build() with our defaults cannot fail");
        let raw = RawHttp {
            inner,
            options: options.clone(),
        };
        let net = Arc::new(raw.with_retry(options.retry_policy.clone(), cancel));
        Self { net, options }
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, timeout, or network error.
    pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes> {
        self.net.get_bytes(url, headers).await
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
        self.net.get_range(url, range, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure or network error.
    pub async fn head(&self, url: Url, headers: Option<Headers>) -> NetResult<Headers> {
        self.net.head(url, headers).await
    }

    #[must_use]
    pub fn options(&self) -> &NetOptions {
        &self.options
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure or network error.
    pub async fn stream(&self, url: Url, headers: Option<Headers>) -> NetResult<crate::ByteStream> {
        self.net.stream(url, headers).await
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
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.net.get_bytes(url, headers).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        self.net.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.net.head(url, headers).await
    }

    async fn stream(
        &self,
        url: Url,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        self.net.stream(url, headers).await
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Net for RawHttp {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        let req = self.inner.get(url.clone());
        let resp = self.send_checked(req, headers, url, false).await?;
        resp.bytes().await.map_err(NetError::from)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let req = self
            .inner
            .get(url.clone())
            .header("Range", range.to_string());
        let resp = self.send_checked(req, headers, url, true).await?;
        Ok(Self::response_to_stream(resp))
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        let req = self.head_request(url.clone());
        let req = Self::apply_headers(req, headers);
        let req = if let Some(total) = self.options.total_timeout {
            req.timeout(total)
        } else {
            req
        };
        let resp = req.send().await.map_err(NetError::from)?;

        let status = resp.status();

        if !status.is_success() && status.as_u16() != HTTP_PARTIAL_CONTENT {
            let body = truncate_error_body(resp.text().await.unwrap_or_default());
            return Err(status_error(url, status.as_u16(), body));
        }

        let mut out = Headers::new();
        let str_pairs = resp
            .headers()
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v)));
        for (name, v) in str_pairs {
            out.insert(name, v);
        }

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

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn stream(
        &self,
        url: Url,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let req = self.inner.get(url.clone());
        let resp = self.send_checked(req, headers, url, false).await?;
        Ok(Self::response_to_stream(resp))
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    };

    use axum::{Router, http::StatusCode, routing::get};
    use tokio::net::TcpListener;

    use super::*;
    use crate::types::RetryPolicy;

    /// Spawn an axum server that returns 503 for the first
    /// `fail_count` requests against `/probe`, then 200 `"ok"` for
    /// every subsequent request. Returns the bound URL and a counter
    /// shared with the handler.
    async fn server_failing_first_n(fail_count: u32) -> (Url, Arc<AtomicU32>) {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_c = Arc::clone(&counter);
        let app = Router::new().route(
            "/probe",
            get(move || {
                let counter = Arc::clone(&counter_c);
                async move {
                    let seen = counter.fetch_add(1, Ordering::SeqCst);
                    if seen < fail_count {
                        (StatusCode::SERVICE_UNAVAILABLE, "busy")
                    } else {
                        (StatusCode::OK, "ok")
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("serve");
        });
        let url = Url::parse(&format!("http://{addr}/probe")).expect("url");
        (url, counter)
    }

    fn fast_options(max_retries: u32) -> NetOptions {
        NetOptions::builder()
            .retry_policy(RetryPolicy {
                max_retries,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            })
            .build()
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn http_client_retries_503_until_ok() {
        let (url, counter) = server_failing_first_n(2).await;
        let client = HttpClient::new(fast_options(3), CancellationToken::default());
        let bytes = client
            .get_bytes(url, None)
            .await
            .expect("get_bytes must succeed after retries");
        assert_eq!(&bytes[..], b"ok");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "exactly 3 attempts: 2 failed (503) + 1 ok"
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn http_client_no_retry_propagates_5xx() {
        let (url, counter) = server_failing_first_n(2).await;
        let client = HttpClient::new(fast_options(0), CancellationToken::default());
        let err = client
            .get_bytes(url, None)
            .await
            .expect_err("max_retries=0 must propagate the 503");
        assert!(
            matches!(err, NetError::Status { status, .. } if status.get() == 503),
            "expected Status(503), got {err:?}"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "max_retries=0 issues exactly one attempt"
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn http_client_head_retries_503_until_ok() {
        let (url, counter) = server_failing_first_n(1).await;
        let client = HttpClient::new(fast_options(2), CancellationToken::default());
        client.head(url, None).await.expect("HEAD must retry");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
