use std::{num::NonZeroU16, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use kithara_platform::{CancelToken, time::timeout};
use url::Url;

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

use crate::{
    backend::{
        Client, RequestBuilder, Response, StatusCode, build_client, head_request, post_request,
    },
    error::{NetError, NetResult},
    metrics::ConnectionMetrics,
    resumable::{Refetch, Resumed, resumable_body},
    retry::{DefaultRetryPolicy, RetryNet},
    traits::{Net, NetExt},
    types::{Headers, NetOptions, RangeSpec},
};

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

/// Read an error response's body for [`NetError::Status`] context — a real
/// socket read, so the fn is one `flash(io)` bracket.
#[kithara::flash(io)]
async fn error_body(resp: Response) -> String {
    truncate_error_body(resp.text().await.unwrap_or_default())
}

/// Collect a full response body — a real socket read, so the fn is one
/// `flash(io)` bracket. The `Net` trait methods cannot carry the attribute
/// themselves: `#[async_trait]` rewrites them into sync constructors of boxed
/// futures, which would drop the bracket before the I/O starts.
#[kithara::flash(io)]
async fn body_bytes(resp: Response) -> Result<Bytes, NetError> {
    resp.bytes().await.map_err(NetError::from)
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

/// Extract response headers into our [`Headers`] type.
fn extract_headers(resp: &Response) -> Headers {
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

/// Raw HTTP client (one `Client`, no retry layer). Lives
/// behind [`HttpClient`]'s [`RetryNet`] decorator — exposed only via
/// the [`Net`] trait, never constructed by callers directly.
#[derive(Clone)]
struct RawHttp {
    /// Master-derived cancel, captured by the self-healing body so a re-fetch
    /// loop exits promptly on teardown. Same token the `RetryNet` layer uses.
    cancel: CancelToken,
    inner: Client,
    options: NetOptions,
}

impl RawHttp {
    fn apply_headers(mut req: RequestBuilder, headers: Option<Headers>) -> RequestBuilder {
        if let Some(headers) = headers {
            for (k, v) in headers.iter() {
                req = req.header(k, v);
            }
        }
        req
    }

    fn head_request(&self, url: Url) -> RequestBuilder {
        head_request(&self.inner, url)
    }

    /// Establish ONE body stream (no self-healing) for `range` (`None` = full
    /// GET). Shared by the first attempt and by the resume re-fetch — keeping
    /// it un-wrapped is what prevents the resilient wrapper recursing.
    async fn raw_body(
        &self,
        url: Url,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<crate::ByteStream, NetError> {
        let mut req = self.inner.get(url.clone());
        if let Some(range) = &range {
            req = req.header("Range", range.to_string());
        }
        let resp = self.send_checked(req, headers, url, accept_partial).await?;
        Ok(Self::response_to_stream(resp))
    }

    /// Body-chunk awaits on this stream happen inside [`resumable_body`]'s
    /// `flash(io)` bracket (`next_chunk`) — every streaming fetch is wrapped
    /// by [`Self::wrap_resumable`] before it reaches a consumer.
    fn response_to_stream(resp: Response) -> crate::ByteStream {
        let headers = extract_headers(&resp);
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        let stream = resp.bytes_stream().map_err(NetError::from);
        crate::ByteStream::with_partial(headers, Box::pin(stream), partial)
    }

    async fn send_checked(
        &self,
        req: RequestBuilder,
        headers: Option<Headers>,
        url: Url,
        accept_partial: bool,
    ) -> Result<Response, NetError> {
        let req = Self::apply_headers(req, headers);
        let req = if let Some(total) = self.options.total_timeout {
            req.timeout(total)
        } else {
            req
        };
        let resp = self.send_idle_bounded(req).await?;
        let status = resp.status();

        let ok = status.is_success() || (accept_partial && status == StatusCode::PARTIAL_CONTENT);
        if !ok {
            let body = error_body(resp).await;
            return Err(status_error(url, status.as_u16(), body));
        }

        Ok(resp)
    }

    /// Await a request's response under the idle/stall timeout: no response
    /// headers within `inactivity_timeout` ⇒ a transient [`NetError::Timeout`]
    /// so the retry decorator re-issues it. This is the establish-side half of
    /// the single idle timer (the body half lives in [`resumable_body`]).
    ///
    /// This bound measures a REAL socket operation (connect + header wait), so
    /// the fn is a `flash(io)` bracket: under `flash` the virtual clock
    /// is paced to real time while the op is in flight, so the (virtual) idle
    /// timer cannot be fired by the quiescence engine racing ahead of an
    /// in-flight loopback establish — it measures at least the equivalent real
    /// time, and never fires for a fast healthy fetch.
    #[kithara::flash(io)]
    async fn send_idle_bounded(&self, req: RequestBuilder) -> Result<Response, NetError> {
        timeout(self.options.inactivity_timeout, req.send())
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(NetError::from)
    }

    /// Wrap a freshly-established body in the self-healing stream: on a stall
    /// (no chunk within `inactivity_timeout`) or transient body error it
    /// re-fetches `bytes=base_start+consumed-end` up to the retry policy, then
    /// surfaces a terminal error. A resume is always a partial (206) request.
    fn wrap_resumable(
        &self,
        first: crate::ByteStream,
        url: Url,
        base_start: u64,
        end: Option<u64>,
        headers: Option<Headers>,
    ) -> crate::ByteStream {
        let out_headers = first.headers.clone();
        let partial = first.is_partial();
        let me = self.clone();
        let refetch: Refetch = Box::new(move |consumed| {
            let me = me.clone();
            let url = url.clone();
            let headers = headers.clone();
            let abs = base_start.saturating_add(consumed);
            let resume = RangeSpec::new(abs, end);
            Box::pin(async move {
                let stream = me.raw_body(url, Some(resume), headers, true).await?;
                // `206` → body already starts at `abs` (skip 0); `200` → server
                // ignored Range and re-sent from zero, drop the consumed prefix.
                let skip = if stream.is_partial() { 0 } else { abs };
                Ok(Resumed { stream, skip })
            })
        });
        let body = resumable_body(
            first,
            refetch,
            self.options.inactivity_timeout,
            self.options.retry_policy.clone(),
            self.cancel.clone(),
        );
        crate::ByteStream::with_partial(out_headers, body, partial)
    }
}

/// Production HTTP client used across the workspace. Wraps a raw
/// `Client` with the workspace's [`RetryNet`] decorator so
/// every [`Net`] method (`head`/`get_bytes`/`post_bytes`/`get_range`/`stream`) honours
/// `options.retry_policy` — retryable errors (TLS-close, timeout,
/// 5xx, IO) are re-issued with exponential backoff; non-retryable
/// errors (HTTP 4xx, cancellation) propagate immediately.
#[derive(Clone)]
pub struct HttpClient {
    net: Arc<RetryNet<RawHttp, DefaultRetryPolicy>>,
    connection_metrics: ConnectionMetrics,
    options: NetOptions,
}

impl HttpClient {
    /// Build a retry-decorated HTTP client rooted on `cancel`. The
    /// `RetryNet` layer aborts pending retries when that token is
    /// cancelled. Callers MUST pass a token that lives in the
    /// consumer-crate's cancel tree — typically
    /// `master_cancel.child()` derived at the consumer-crate top
    /// (`App`, `Queue`, FFI player). The workspace cancel hierarchy
    /// forbids orphan tokens in production code.
    ///
    /// # Panics
    ///
    /// Panics if the HTTP `Client` builder fails to build.
    #[must_use]
    pub fn new(options: NetOptions, cancel: CancelToken) -> Self {
        let connection_metrics = ConnectionMetrics::default();
        let inner = build_client(&options, &connection_metrics)
            .expect("BUG: HTTP client builder with our defaults cannot fail");
        let raw = RawHttp {
            inner,
            options: options.clone(),
            cancel: cancel.clone(),
        };
        let net = Arc::new(raw.with_retry(options.retry_policy.clone(), cancel));
        Self {
            net,
            connection_metrics,
            options,
        }
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, timeout, or network error.
    pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes> {
        self.net.get_bytes(url, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, timeout, or network error.
    pub async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> NetResult<Bytes> {
        self.net.post_bytes(url, body, headers).await
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

    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connection_metrics.connection_count()
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

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        self.net.post_bytes(url, body, headers).await
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
        body_bytes(resp).await
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        let req = post_request(&self.inner, url.clone(), body);
        let resp = self.send_checked(req, headers, url, false).await?;
        body_bytes(resp).await
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<crate::ByteStream, NetError> {
        let first = self
            .raw_body(url.clone(), Some(range.clone()), headers.clone(), true)
            .await?;
        Ok(self.wrap_resumable(first, url, range.start, range.end, headers))
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
        let resp = self.send_idle_bounded(req).await?;

        let status = resp.status();

        if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
            let body = error_body(resp).await;
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
        let first = self
            .raw_body(url.clone(), None, headers.clone(), false)
            .await?;
        // Full GET; a resume re-fetches `bytes=consumed-` (base 0).
        Ok(self.wrap_resumable(first, url, 0, None, headers))
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
    };

    use axum::{
        Router,
        http::StatusCode,
        routing::{get, post},
    };
    use kithara_platform::{
        time::Duration,
        tokio::{net::TcpListener, task::spawn},
    };

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
        spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("serve");
        });
        let url = Url::parse(&format!("http://{addr}/probe")).expect("url");
        (url, counter)
    }

    /// Spawn an axum server whose `/echo` POST route returns 503 for the
    /// first `fail_count` requests, then echoes the request body with 200.
    async fn server_post_echo_failing_first_n(fail_count: u32) -> (Url, Arc<AtomicU32>) {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_c = Arc::clone(&counter);
        let app = Router::new().route(
            "/echo",
            post(move |body: Bytes| {
                let counter = Arc::clone(&counter_c);
                async move {
                    let seen = counter.fetch_add(1, Ordering::SeqCst);
                    if seen < fail_count {
                        (StatusCode::SERVICE_UNAVAILABLE, Bytes::from_static(b"busy"))
                    } else {
                        (StatusCode::OK, body)
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
        spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("serve");
        });
        let url = Url::parse(&format!("http://{addr}/echo")).expect("url");
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
        let client = HttpClient::new(fast_options(3), CancelToken::never());
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
        let client = HttpClient::new(fast_options(0), CancelToken::never());
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
        let client = HttpClient::new(fast_options(2), CancelToken::never());
        client.head(url, None).await.expect("HEAD must retry");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn http_client_post_retries_then_echoes_body() {
        let (url, counter) = server_post_echo_failing_first_n(1).await;
        let client = HttpClient::new(fast_options(2), CancelToken::never());
        let echoed = client
            .post_bytes(url, Bytes::from_static(b"ping"), None)
            .await
            .expect("post_bytes must succeed after retry");
        assert_eq!(&echoed[..], b"ping", "server must echo the posted body");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "exactly 2 attempts: 1 failed (503) + 1 ok"
        );
    }
}
