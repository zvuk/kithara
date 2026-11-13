use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::{Method, Request, Response, StatusCode};
use kithara_bufpool::{HasPool, PoolRegion};
#[cfg(all(test, reqwest_backend, not(target_arch = "wasm32")))]
use kithara_platform::sync::Mutex;
use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Duration, Instant, timeout},
};
use url::{Position, Url};

mod kithara {
    pub(crate) use kithara_test_macros::measure;
}

use crate::{
    HttpBody,
    backend::{
        SelectedTransport,
        common::{normalize_head_headers, status_error},
    },
    error::{NetError, truncate_error_body},
    metrics::ConnectionMetrics,
    observe::Observer,
    range_response::{accepts_response_status, validate_range_response},
    resumable::{Refetch, Resumed, resumable_body},
    retry::{RawNet, RetryClient, RetryNet},
    transport::HttpTransport,
    types::{AcceptEncodingPolicy, Headers, NetOptions, RangeSpec, RetryPolicy},
};

async fn error_body(resp: Response<HttpBody>, inactivity: Duration) -> String {
    let body = timeout(inactivity, body_bytes(resp, inactivity))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    truncate_error_body(String::from_utf8_lossy(&body).into_owned(), "\u{2026}")
}

async fn body_bytes(resp: Response<HttpBody>, inactivity: Duration) -> Result<Bytes, NetError> {
    let mut stream = resp.into_body();
    let mut buf = BytesMut::new();
    while let Some(chunk) = timeout(inactivity, stream.next())
        .await
        .map_err(|_| NetError::Timeout)?
    {
        buf.extend_from_slice(&chunk?);
    }
    Ok(buf.freeze())
}

#[cfg(not(target_arch = "wasm32"))]
fn reject_undecoded_content_encoding(resp: &Response<HttpBody>, url: &Url) -> Result<(), NetError> {
    for value in resp.headers().get_all("content-encoding") {
        let value = value.to_str().map_err(|error| {
            NetError::Decode(format!(
                "invalid content-encoding header for {url}: {error}"
            ))
        })?;
        if value
            .split(',')
            .map(str::trim)
            .any(|coding| !coding.eq_ignore_ascii_case("identity"))
        {
            return Err(NetError::Decode(format!(
                "response body for {url} retained content-encoding: {value}"
            )));
        }
    }
    Ok(())
}

/// Extract response headers into our [`Headers`] type.
fn extract_headers(resp: &Response<HttpBody>) -> Headers {
    let mut headers = Headers::default();
    let str_pairs = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v)));
    for (name, value) in str_pairs {
        headers.insert(name, value);
    }
    headers
}

/// Raw HTTP policy owner over one transport resource. Lives
/// behind [`HttpClient`]'s [`RetryNet`] decorator through [`RawNet`].
#[derive(Clone)]
pub struct RawHttp {
    /// Master-derived cancel, captured by the self-healing body so a re-fetch
    /// loop exits promptly on teardown. Same token the `RetryNet` retry loop uses.
    cancel: CancelToken,
    inner: Arc<dyn HttpTransport>,
    options: NetOptions,
}

impl RawHttp {
    fn request(
        method: Method,
        url: &Url,
        body: Bytes,
        headers: Option<Headers>,
        encoding: AcceptEncodingPolicy,
    ) -> Result<Request<Bytes>, NetError> {
        let mut request = Request::builder()
            .method(method)
            .uri(&url[..Position::AfterQuery]);
        if let Some(headers) = headers {
            for (name, value) in headers
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("accept-encoding"))
            {
                request = request.header(name, value);
            }
        }
        if encoding == AcceptEncodingPolicy::Identity {
            request = request.header("Accept-Encoding", "identity");
        }
        request
            .body(body)
            .map_err(|error| NetError::Network(error.to_string()))
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
        observer: Option<&Observer>,
    ) -> Result<crate::ByteStream, NetError> {
        let mut req = Self::request(
            Method::GET,
            &url,
            Bytes::new(),
            headers,
            AcceptEncodingPolicy::Identity,
        )?;
        if let Some(range) = &range {
            req.headers_mut().insert(
                "range",
                range
                    .to_string()
                    .parse()
                    .map_err(|error: http::header::InvalidHeaderValue| {
                        NetError::Network(error.to_string())
                    })?,
            );
        }
        let resp = self
            .send_checked(req, url.clone(), accept_partial, observer)
            .await?;
        Self::response_to_stream(resp, range.as_ref(), &url)
    }

    fn response_to_stream(
        resp: Response<HttpBody>,
        range: Option<&RangeSpec>,
        url: &Url,
    ) -> Result<crate::ByteStream, NetError> {
        let headers = extract_headers(&resp);
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        validate_range_response(resp.status().as_u16(), range, &headers, url)?;
        let stream = resp.into_body();
        Ok(crate::ByteStream::with_partial(headers, stream, partial))
    }

    async fn send_checked(
        &self,
        req: Request<Bytes>,
        url: Url,
        accept_partial: bool,
        observer: Option<&Observer>,
    ) -> Result<Response<HttpBody>, NetError> {
        let started = Instant::now();
        let resp = self.send_idle_bounded(req).await?;
        let status = resp.status();
        if let Some(observer) = observer {
            observer.0.first_byte(
                started.elapsed(),
                status.as_u16(),
                status == StatusCode::PARTIAL_CONTENT,
            );
        }

        let ok = accepts_response_status(status.as_u16(), accept_partial);
        if !ok {
            let body = error_body(resp, self.options.inactivity_timeout).await;
            return Err(status_error(url, status.as_u16(), body));
        }

        #[cfg(not(target_arch = "wasm32"))]
        reject_undecoded_content_encoding(&resp, &url)?;

        Ok(resp)
    }

    async fn send_idle_bounded(&self, req: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        timeout(self.options.inactivity_timeout, self.inner.open(req))
            .await
            .map_err(|_| NetError::Timeout)?
    }

    /// Wrap a freshly-established body in the self-healing stream: on a stall
    /// (no chunk within `inactivity_timeout`) or transient body error it
    /// re-fetches `bytes=base_start+consumed-end` up to the retry policy, then
    /// surfaces a terminal error. A resume is always a partial (206) request.
    ///
    /// On a `206` response the body already starts at the requested offset (skip 0); on `200` the
    /// server ignored the Range and resent from the start, so the already-consumed prefix is
    /// dropped.
    fn wrap_resumable(
        &self,
        first: crate::ByteStream,
        url: Url,
        base_start: u64,
        end: Option<u64>,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> crate::ByteStream {
        let out_headers = first.headers.clone();
        let partial = first.is_partial();
        let me = self.clone();
        let resource = url.clone();
        let observer = observer.cloned();
        let refetch_observer = observer.clone();
        let (resume_base, resume_end) = if partial {
            (base_start, end)
        } else {
            (0, None)
        };
        let refetch: Refetch = Box::new(move |consumed| {
            let me = me.clone();
            let url = url.clone();
            let headers = headers.clone();
            let observer = refetch_observer.clone();
            let abs = resume_base.saturating_add(consumed);
            let resume = RangeSpec::new(abs, resume_end);
            Box::pin(async move {
                let stream = me
                    .raw_body(url, Some(resume), headers, true, observer.as_ref())
                    .await?;
                let skip = if stream.is_partial() { 0 } else { abs };
                Ok(Resumed { stream, skip })
            })
        });
        let body = resumable_body(
            first,
            refetch,
            resource,
            self.options.inactivity_timeout,
            self.options.retry_policy,
            self.cancel.clone(),
            observer,
        );
        crate::ByteStream::with_partial(out_headers, body, partial)
    }
}

/// Production HTTP client used across the workspace. Runs every
/// [`Net`](crate::Net) method (`head`/`get_bytes`/`post_bytes`/`get_range`/`stream`)
/// through [`RetryNet`]'s retry loop over a raw `Client`, honouring
/// `options.retry_policy`: retryable errors (TLS-close, timeout,
/// 5xx, IO) are re-issued with exponential backoff; non-retryable
/// errors (HTTP 4xx, cancellation) propagate immediately.
pub type HttpClient = RetryClient<RawHttp>;

impl std::fmt::Debug for RetryClient<RawHttp> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("options", self.options())
            .finish()
    }
}

impl RetryClient<RawHttp> {
    /// Build an HTTP client rooted on `cancel`. The `RetryNet` retry
    /// loop aborts pending retries when that token is
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
    pub fn new<S>(options: NetOptions, pools: PoolRegion<S>, cancel: CancelToken) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        let connection_metrics = ConnectionMetrics::default();
        let transport =
            SelectedTransport::new(&options, pools, cancel.clone(), &connection_metrics);
        let mut client = Self::new_with_transport(options, transport, cancel);
        client.connection_metrics = connection_metrics;
        client
    }

    /// Build a client over an owned transport, sharing it with derived handles.
    /// The client applies the same inactivity, validation, retry and cancellation policies
    /// as it does to built-in transports.
    #[must_use]
    pub fn new_with_transport<T: HttpTransport + 'static>(
        options: NetOptions,
        transport: T,
        cancel: CancelToken,
    ) -> Self {
        let retry_policy = options.retry_policy;
        let raw = RawHttp {
            inner: Arc::new(transport),
            options,
            cancel: cancel.clone(),
        };
        Self {
            net: Arc::new(RetryNet::new(raw, retry_policy, cancel)),
            connection_metrics: ConnectionMetrics::default(),
            observer: None,
        }
    }

    /// Returns a handle with a different retry policy, sharing the HTTP
    /// transport and connection pool with this client.
    #[must_use]
    pub fn with_retry_policy(&self, retry_policy: RetryPolicy) -> Self {
        let mut options = self.options().clone();
        options.retry_policy = retry_policy;
        self.with_options(options)
    }

    fn with_options(&self, options: NetOptions) -> Self {
        let current = self.net.inner();
        let raw = RawHttp {
            inner: current.inner.clone(),
            options,
            cancel: current.cancel.clone(),
        };
        let retry_policy = raw.options.retry_policy;
        let net = Arc::new(RetryNet::new(raw, retry_policy, current.cancel.clone()));
        Self {
            net,
            connection_metrics: self.connection_metrics.clone(),
            observer: self.observer.clone(),
        }
    }

    delegate::delegate! {
        to self.net.inner() {
            #[must_use]
            #[field(&options)]
            pub fn options(&self) -> &NetOptions;
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RawNet for RawHttp {
    #[kithara::measure]
    async fn get_bytes(
        &self,
        url: Url,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> Result<Bytes, NetError> {
        let req = Self::request(
            Method::GET,
            &url,
            Bytes::new(),
            headers,
            AcceptEncodingPolicy::Configured,
        )?;
        let resp = self.send_checked(req, url, false, observer).await?;
        body_bytes(resp, self.options.inactivity_timeout).await
    }

    #[kithara::measure]
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> Result<crate::ByteStream, NetError> {
        let first = self
            .raw_body(
                url.clone(),
                Some(range.clone()),
                headers.clone(),
                true,
                observer,
            )
            .await?;
        Ok(self.wrap_resumable(first, url, range.start, range.end, headers, observer))
    }

    #[kithara::measure]
    async fn head(
        &self,
        url: Url,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> Result<Headers, NetError> {
        let req = Self::request(
            Method::HEAD,
            &url,
            Bytes::new(),
            headers,
            AcceptEncodingPolicy::Identity,
        )?;
        let resp = self.send_checked(req, url, true, observer).await?;
        let out = normalize_head_headers(extract_headers(&resp));
        body_bytes(resp, self.options.inactivity_timeout).await?;
        Ok(out)
    }

    #[kithara::measure]
    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> Result<Bytes, NetError> {
        let req = Self::request(
            Method::POST,
            &url,
            body,
            headers,
            AcceptEncodingPolicy::Configured,
        )?;
        let resp = self.send_checked(req, url, false, observer).await?;
        body_bytes(resp, self.options.inactivity_timeout).await
    }

    /// Issues a full GET; a resume re-fetches `bytes=consumed-` from base offset 0.
    #[kithara::measure]
    async fn stream(
        &self,
        url: Url,
        headers: Option<Headers>,
        observer: Option<&Observer>,
    ) -> Result<crate::ByteStream, NetError> {
        let first = self
            .raw_body(url.clone(), None, headers.clone(), false, observer)
            .await?;
        Ok(self.wrap_resumable(first, url, 0, None, headers, observer))
    }
}

#[cfg(all(test, reqwest_backend, not(target_arch = "wasm32")))]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::sync::atomic::{AtomicU32, Ordering};

    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, StatusCode, header::CONTENT_ENCODING},
        response::Response as AxumResponse,
        routing::{any, get, post},
    };
    use futures::{StreamExt, stream};
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_test_utils::TestHttpServer;

    use super::*;
    use crate::types::RetryPolicy;

    /// Spawn an axum server that returns 503 for the first
    /// `fail_count` requests against `/probe`, then 200 `"ok"` for
    /// every subsequent request. Returns the server and a counter shared
    /// with the handler.
    async fn server_failing_first_n(fail_count: u32) -> (TestHttpServer, Arc<AtomicU32>) {
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
        (TestHttpServer::new(app).await, counter)
    }

    /// Spawn an axum server whose `/echo` POST route returns 503 for the
    /// first `fail_count` requests, then echoes the request body with 200.
    async fn server_post_echo_failing_first_n(fail_count: u32) -> (TestHttpServer, Arc<AtomicU32>) {
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
        (TestHttpServer::new(app).await, counter)
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

    /// Spawn an axum server whose `/stall` routes send `200 OK` with headers
    /// and one body chunk, then never deliver the rest of the body — the
    /// throttling-CDN shape that must surface as a timeout, not a hang.
    async fn server_stalling_body() -> TestHttpServer {
        use axum::{body::Body, response::Response as HttpResponse};
        use futures::{StreamExt, stream};

        fn stalled() -> HttpResponse {
            HttpResponse::new(Body::from_stream(
                stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"partial"))])
                    .chain(stream::pending()),
            ))
        }
        let app = Router::new()
            .route("/stall", get(|| async { stalled() }))
            .route("/stall", post(|_body: Bytes| async { stalled() }));
        TestHttpServer::new(app).await
    }

    fn stall_options() -> NetOptions {
        stall_options_with_retries(1)
    }

    fn stall_options_with_retries(max_retries: u32) -> NetOptions {
        NetOptions::builder()
            .inactivity_timeout(Duration::from_millis(100))
            .retry_policy(RetryPolicy {
                max_retries,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            })
            .build()
    }

    fn assert_timeout_class(err: &NetError) {
        assert!(
            matches!(
                err,
                NetError::Timeout | NetError::RetryExhausted { .. } | NetError::Network(_)
            ),
            "expected timeout-class error, got {err:?}"
        );
    }

    #[kithara::test(tokio)]
    async fn get_bytes_aborts_when_body_stalls() {
        let server = server_stalling_body().await;
        let url = server.url("/stall");
        let client = HttpClient::new(
            stall_options(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let err = client
            .get_bytes(url, None)
            .await
            .expect_err("stalled body must abort, not hang");
        assert_timeout_class(&err);
    }

    #[kithara::test(tokio)]
    async fn post_bytes_aborts_when_body_stalls() {
        let server = server_stalling_body().await;
        let url = server.url("/stall");
        let client = HttpClient::new(
            stall_options(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let err = client
            .post_bytes(url, Bytes::from_static(b"req"), None)
            .await
            .expect_err("stalled body must abort, not hang");
        assert_timeout_class(&err);
    }

    #[kithara::test(tokio)]
    async fn native_content_coding_policy_is_authoritative_per_request() {
        const PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na.m3u8\n";
        const BROTLI_PLAYLIST: &[u8] = &[
            0x0f, 0x16, 0x80, 0x23, 0x45, 0x58, 0x54, 0x4d, 0x33, 0x55, 0x0a, 0x23, 0x45, 0x58,
            0x54, 0x2d, 0x58, 0x2d, 0x53, 0x54, 0x52, 0x45, 0x41, 0x4d, 0x2d, 0x49, 0x4e, 0x46,
            0x3a, 0x42, 0x41, 0x4e, 0x44, 0x57, 0x49, 0x44, 0x54, 0x48, 0x3d, 0x31, 0x0a, 0x61,
            0x2e, 0x6d, 0x33, 0x75, 0x38, 0x0a, 0x03,
        ];
        const GZIP_PLAYLIST: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x53, 0x76, 0x8d, 0x08,
            0xf1, 0x35, 0x0e, 0xe5, 0x52, 0x06, 0xd2, 0xba, 0x11, 0xba, 0xc1, 0x21, 0x41, 0xae,
            0x8e, 0xbe, 0xba, 0x9e, 0x7e, 0x6e, 0x56, 0x4e, 0x8e, 0x7e, 0x2e, 0xe1, 0x9e, 0x2e,
            0x21, 0x1e, 0xb6, 0x86, 0x5c, 0x89, 0x7a, 0xb9, 0xc6, 0xa5, 0x16, 0x5c, 0x00, 0xf1,
            0x51, 0x3e, 0xd3, 0x2d, 0x00, 0x00, 0x00,
        ];

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let app = Router::new().route(
            "/master.m3u8",
            any(move |headers: HeaderMap| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    let accept_encoding = headers
                        .get("accept-encoding")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    seen.lock().push(accept_encoding.clone());
                    if accept_encoding
                        .split(',')
                        .any(|coding| coding.trim() == "br")
                    {
                        AxumResponse::builder()
                            .header(CONTENT_ENCODING, "br")
                            .body(Body::from(BROTLI_PLAYLIST))
                            .expect("brotli response")
                    } else if accept_encoding
                        .split(',')
                        .any(|coding| coding.trim() == "gzip")
                    {
                        AxumResponse::builder()
                            .header(CONTENT_ENCODING, "gzip")
                            .body(Body::from(GZIP_PLAYLIST))
                            .expect("gzip response")
                    } else {
                        AxumResponse::new(Body::from(PLAYLIST))
                    }
                }
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/master.m3u8");
        let options = NetOptions::builder()
            .compression(crate::Compression::GZIP | crate::Compression::DEFLATE)
            .build();
        let client = HttpClient::new(options, crate::test_pools::pools(), CancelToken::never());

        let mut caller_headers = Headers::default();
        caller_headers.insert("AcCePt-EnCoDiNg", "br");
        let bytes = client
            .get_bytes(url.clone(), Some(caller_headers))
            .await
            .expect("atomic response");
        assert_eq!(bytes, PLAYLIST);

        let mut body = client
            .stream(url.clone(), None)
            .await
            .expect("stream response");
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk.expect("body chunk"));
        }
        assert_eq!(bytes, PLAYLIST);

        let _range = client
            .get_range(url.clone(), RangeSpec::new(0, Some(3)), None)
            .await
            .expect("range response");
        client.head(url.clone(), None).await.expect("head response");
        let posted = client
            .post_bytes(url, Bytes::from_static(b"request"), None)
            .await
            .expect("post response");
        assert_eq!(posted, PLAYLIST);

        assert_eq!(
            seen.lock().as_slice(),
            [
                "gzip, deflate",
                "identity",
                "identity",
                "identity",
                "gzip, deflate"
            ]
        );
    }

    #[kithara::test(tokio)]
    async fn native_rejects_an_undecoded_content_encoding() {
        let app = Router::new().route(
            "/encoded",
            get(|| async {
                AxumResponse::builder()
                    .header(CONTENT_ENCODING, "br")
                    .body(Body::from(Bytes::from_static(b"encoded")))
                    .expect("encoded response")
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/encoded");
        let options = NetOptions::builder()
            .compression(crate::Compression::GZIP | crate::Compression::DEFLATE)
            .build();
        let client = HttpClient::new(options, crate::test_pools::pools(), CancelToken::never());

        let Err(error) = client.stream(url, None).await else {
            panic!("encoded bytes must not reach a byte-addressed stream");
        };

        assert!(matches!(error, NetError::Decode(detail) if detail.contains("content-encoding")));
    }

    #[kithara::test(tokio)]
    async fn native_range_rejects_partial_without_content_range() {
        let app = Router::new().route(
            "/range",
            get(|| async {
                AxumResponse::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("content-length", "4")
                    .body(Body::from(Bytes::from_static(b"part")))
                    .expect("partial response")
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/range");
        let client = HttpClient::new(
            NetOptions::default(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );

        let Err(error) = client
            .get_range(url, RangeSpec::new(0, Some(3)), None)
            .await
        else {
            panic!("partial range without content-range must fail before body delivery");
        };

        assert!(matches!(error, NetError::Decode(detail) if detail.contains("content-range")));
    }

    #[kithara::test(tokio)]
    async fn native_range_rejects_partial_without_content_length() {
        let app = Router::new().route(
            "/range",
            get(|| async {
                AxumResponse::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("content-range", "bytes 0-3/8")
                    .body(Body::from_stream(stream::iter([Ok::<_, std::io::Error>(
                        Bytes::from_static(b"abcdefgh"),
                    )])))
                    .expect("chunked partial response")
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/range");
        let client = HttpClient::new(
            NetOptions::default(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );

        let Err(error) = client
            .get_range(url, RangeSpec::new(0, Some(3)), None)
            .await
        else {
            panic!("partial range without content-length must fail before body delivery");
        };

        assert!(matches!(error, NetError::Decode(detail) if detail.contains("content-length")));
    }

    #[kithara::test(tokio)]
    async fn native_resume_rejects_a_different_content_range() {
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        let app = Router::new().route(
            "/resume",
            get(move |headers: HeaderMap| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    if headers.contains_key("range") {
                        AxumResponse::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "3")
                            .header("content-range", "bytes 0-2/6")
                            .body(Body::from(Bytes::from_static(b"abc")))
                            .expect("wrong resumed response")
                    } else {
                        let body =
                            stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))])
                                .chain(stream::pending());
                        AxumResponse::builder()
                            .header("content-length", "6")
                            .body(Body::from_stream(body))
                            .expect("stalled initial response")
                    }
                }
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/resume");
        let client = HttpClient::new(
            stall_options(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let mut body = client.stream(url, None).await.expect("initial stream");
        let error = loop {
            match body.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("wrong resumed range must terminate the stream"),
            }
        };

        assert!(
            matches!(error, NetError::Decode(detail) if detail.contains("bytes=3-") && detail.contains("bytes 0-2/6"))
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[kithara::test(tokio)]
    async fn native_resume_rejects_a_conflicting_representation_total() {
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        let app = Router::new().route(
            "/resume",
            get(move |headers: HeaderMap| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    if headers.contains_key("range") {
                        AxumResponse::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "3")
                            .header("content-range", "bytes 3-5/9")
                            .body(Body::from(Bytes::from_static(b"def")))
                            .expect("conflicting resumed response")
                    } else {
                        let body =
                            stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))])
                                .chain(stream::pending());
                        AxumResponse::builder()
                            .header("content-length", "6")
                            .body(Body::from_stream(body))
                            .expect("stalled initial response")
                    }
                }
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/resume");
        let client = HttpClient::new(
            stall_options(),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let mut body = client.stream(url, None).await.expect("initial stream");
        let error = loop {
            match body.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("conflicting resumed total must terminate the stream"),
            }
        };

        assert!(matches!(error, NetError::Decode(detail) if detail.contains("total from 6 to 9")));
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[kithara::test(tokio)]
    async fn native_ignored_range_resume_continues_after_discovering_total() {
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        let app = Router::new().route(
            "/resume",
            get(move |_headers: HeaderMap| {
                let attempt = seen.fetch_add(1, Ordering::SeqCst);
                async move {
                    match attempt {
                        0 => {
                            let body =
                                stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))])
                                    .chain(stream::pending());
                            AxumResponse::builder()
                                .body(Body::from_stream(body))
                                .expect("stalled chunked response")
                        }
                        1 => AxumResponse::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "2")
                            .header("content-range", "bytes 3-4/6")
                            .body(Body::from(Bytes::from_static(b"de")))
                            .expect("first capped response"),
                        2 => AxumResponse::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "1")
                            .header("content-range", "bytes 5-5/6")
                            .body(Body::from(Bytes::from_static(b"f")))
                            .expect("final capped response"),
                        _ => panic!("unexpected request {attempt}"),
                    }
                }
            }),
        );
        let server = TestHttpServer::new(app).await;
        let url = server.url("/resume");
        let client = HttpClient::new(
            stall_options_with_retries(2),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let mut body = client
            .get_range(url, RangeSpec::new(0, Some(1)), None)
            .await
            .expect("initial stream");
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk.expect("resumed chunk"));
        }

        assert_eq!(bytes, b"abcdef");
        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }

    #[kithara::test(tokio)]
    async fn http_client_no_retry_propagates_5xx() {
        let (server, counter) = server_failing_first_n(2).await;
        let url = server.url("/probe");
        let client = HttpClient::new(
            fast_options(0),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
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

    /// A handle derived with its own retry policy retries by that policy, and
    /// the original keeps retrying by its own.
    #[kithara::test(tokio)]
    async fn a_retry_policy_handle_leaves_the_original_policy() {
        let (server, counter) = server_post_echo_failing_first_n(u32::MAX).await;
        let client = HttpClient::new(
            fast_options(2),
            crate::test_pools::pools(),
            CancelToken::never(),
        );
        let derived = client.with_retry_policy(fast_options(0).retry_policy);

        let _ = derived
            .post_bytes(server.url("/echo"), Bytes::new(), None)
            .await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "the derived handle sends once"
        );
        let _ = client
            .post_bytes(server.url("/echo"), Bytes::new(), None)
            .await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            4,
            "the original retries twice"
        );
    }
}
