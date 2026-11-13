//! Requests to a real local server. The `client-host` backend reaches a server
//! only through the transport a host installs, which only the Android runner
//! does for this binary.
#![cfg(not(any(
    target_arch = "wasm32",
    all(feature = "client-host", not(target_os = "android"))
)))]

use std::{
    collections::HashMap,
    sync::{
        Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, head},
};
use bytes::Bytes;
use futures::{StreamExt, join, stream};
use kithara_net::{
    Headers, HttpClient, NetError, NetObserver, NetOptions, Observer, RangeSpec, RetryPolicy,
};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Notify},
    time::{Duration, WallInstant, sleep},
};
use kithara_test_utils::{TestHttpServer, bufpool::pools, kithara};
use url::Url;

type TestServer = TestHttpServer;

async fn test_endpoint() -> &'static str {
    "Hello, World!"
}

async fn head_length_endpoint() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, "13".parse().unwrap());
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    (headers, ())
}

async fn range_endpoint(headers: HeaderMap) -> impl IntoResponse {
    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().unwrap();
        if let Some(range) = range_str.strip_prefix("bytes=") {
            let parts: Vec<&str> = range.split('-').collect();
            if parts.len() == 2 {
                let start_result = parts[0].parse::<u64>();
                let end_result = if parts[1].is_empty() {
                    Ok(None)
                } else {
                    parts[1].parse::<u64>().map(Some)
                };

                if let (Ok(start), Ok(end_opt)) = (start_result, end_result) {
                    let data = "Hello, World!".as_bytes();
                    let end = end_opt.unwrap_or((data.len() - 1) as u64);

                    if start <= end && end < data.len() as u64 {
                        let slice = &data[usize::try_from(start).unwrap_or(0)
                            ..=usize::try_from(end).unwrap_or(0)];
                        let mut response_headers = HeaderMap::new();
                        response_headers.insert(
                            header::CONTENT_RANGE,
                            format!("bytes {}-{}/{}", start, end, data.len())
                                .parse()
                                .unwrap(),
                        );
                        response_headers.insert(header::CONTENT_LENGTH, slice.len().into());
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            response_headers,
                            slice.to_vec(),
                        );
                    }
                }
            }
        }
    }
    (StatusCode::BAD_REQUEST, HeaderMap::new(), Vec::<u8>::new())
}

async fn headers_endpoint(headers: HeaderMap) -> impl IntoResponse {
    let mut response_headers = HeaderMap::new();

    if let Some(custom_header) = headers.get("X-Custom-Header") {
        response_headers.insert("X-Received-Header", custom_header.clone());
    }

    if let Some(user_agent) = headers.get(header::USER_AGENT) {
        response_headers.insert("X-User-Agent", user_agent.clone());
    }

    (response_headers, "Headers received")
}

async fn error_404_endpoint() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}

async fn error_500_endpoint() -> impl IntoResponse {
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn error_429_endpoint() -> impl IntoResponse {
    StatusCode::TOO_MANY_REQUESTS
}

async fn slow_headers_endpoint() -> impl IntoResponse {
    sleep(Duration::from_millis(500)).await;
    "Slow headers"
}

async fn slow_body_endpoint() -> impl IntoResponse {
    let mut chunks = vec![
        Ok::<_, axum::BoxError>(Bytes::from("Hello")),
        Ok(Bytes::from(", ")),
        Ok(Bytes::from("World")),
        Ok(Bytes::from("!")),
    ]
    .into_iter();
    let stream =
        stream::iter(chunks.next()).chain(stream::unfold(chunks, |mut chunks| async move {
            sleep(Duration::from_millis(200)).await;
            chunks.next().map(|chunk| (chunk, chunks))
        }));

    Response::builder()
        .status(StatusCode::OK)
        // NSURLSession buffers a tiny untyped body before reporting its response head.
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn ignore_range_endpoint() -> &'static str {
    "Full response ignoring range"
}

#[derive(Clone, Default)]
struct RequestCounter {
    count: Arc<AtomicUsize>,
}

impl RequestCounter {
    fn new() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn increment(&self) -> usize {
        self.count.fetch_add(1, Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct KeyRequestCounter {
    counts: Arc<RwLock<HashMap<String, usize>>>,
}

impl KeyRequestCounter {
    fn new() -> Self {
        Self {
            counts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn increment(&self, path: &str) {
        let mut counts = self.counts.write().unwrap();
        *counts.entry(path.to_string()).or_insert(0) += 1;
    }
}

const KEY_BYTES: &[u8] = &[
    0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];
const AUTH_KEY_BYTES: &[u8] = &[
    0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];
const PARAM_KEY_BYTES: &[u8] = &[
    0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0x12, 0x34, 0x56, 0x78,
];

fn key_response(key_bytes: &'static [u8]) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from(Bytes::from_static(key_bytes)))
        .unwrap()
}

async fn key_endpoint(
    State(counter): State<KeyRequestCounter>,
    _request: Request,
) -> Result<Response, StatusCode> {
    counter.increment("/key");
    Ok(key_response(KEY_BYTES))
}

async fn key_with_auth_endpoint(
    State(counter): State<KeyRequestCounter>,
    request: Request,
) -> Result<Response, StatusCode> {
    counter.increment("/key-with-auth");

    let headers = request.headers();
    let auth_header = headers.get("Authorization").and_then(|h| h.to_str().ok());

    match auth_header {
        Some("Bearer secret-key-token") => {
            let key_bytes = AUTH_KEY_BYTES;

            let range_header = headers.get("Range").and_then(|h| h.to_str().ok());

            if let Some(range) = range_header
                && let Some(range_str) = range.strip_prefix("bytes=")
                && let Some((start_str, end_str)) = range_str.split_once('-')
                && let Ok(start) = start_str.parse::<usize>()
            {
                let end = if end_str.is_empty() {
                    key_bytes.len() - 1
                } else {
                    end_str.parse().unwrap_or(key_bytes.len() - 1)
                };

                if start < key_bytes.len() && end < key_bytes.len() && start <= end {
                    let slice = &key_bytes[start..=end];
                    return Ok(Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("Content-Type", "application/octet-stream")
                        .header(
                            "Content-Range",
                            format!("bytes {}-{}/{}", start, end, key_bytes.len()),
                        )
                        .body(Body::from(Bytes::copy_from_slice(slice)))
                        .unwrap());
                }

                Err(StatusCode::BAD_REQUEST)
            } else {
                Ok(key_response(key_bytes))
            }
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn key_with_params_endpoint(
    State(counter): State<KeyRequestCounter>,
    request: Request,
) -> Result<Response, StatusCode> {
    counter.increment("/key-with-params");

    let uri = request.uri();
    let query_params = uri.query().unwrap_or("");

    let has_required_params =
        query_params.contains("drm_id=test123") && query_params.contains("version=1.0");

    if has_required_params {
        Ok(key_response(PARAM_KEY_BYTES))
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

#[kithara::fixture]
fn test_router() -> Router {
    Router::new()
        .route("/test", get(test_endpoint))
        .route("/head-length", head(head_length_endpoint))
        .route("/range", get(range_endpoint))
        .route("/headers", get(headers_endpoint))
        .route("/error404", get(error_404_endpoint))
        .route("/error500", get(error_500_endpoint))
        .route("/error429", get(error_429_endpoint))
        .route("/slow-headers", get(slow_headers_endpoint))
        .route("/slow-body", get(slow_body_endpoint))
        .route("/ignore-range", get(ignore_range_endpoint))
}

fn key_test_router() -> Router {
    let counter = KeyRequestCounter::new();

    Router::new()
        .route("/key", get(key_endpoint))
        .route("/key-with-auth", get(key_with_auth_endpoint))
        .route("/key-with-params", get(key_with_params_endpoint))
        .with_state(counter)
}

async fn key_test_server() -> TestServer {
    TestServer::new(key_test_router()).await
}

#[kithara::fixture]
async fn test_server(test_router: Router) -> TestServer {
    TestServer::new(test_router).await
}

#[kithara::fixture]
fn http_client() -> HttpClient {
    HttpClient::new(NetOptions::default(), pools(), CancelToken::never())
}

async fn test_get_bytes_success(client: &HttpClient, url: Url) -> Result<Bytes, NetError> {
    client.get_bytes(url, None).await
}

async fn test_stream_success(client: &HttpClient, url: Url) -> Result<Vec<u8>, NetError> {
    let mut stream = client.stream(url, None).await?;
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk?);
    }
    Ok(collected)
}

/// Build a single-pair `Authorization: Bearer <token>` header map.
fn auth_bearer(token: &str) -> Headers {
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    headers.into()
}

/// Assert that `result` is `NetError::Status` with the expected status.
fn assert_http_status<T: std::fmt::Debug>(
    result: Result<T, NetError>,
    expected_status: u16,
    context: &str,
) {
    match result {
        Ok(value) => panic!("expected HTTP error {expected_status} ({context}); got Ok({value:?})"),
        Err(NetError::Status { status, .. }) => assert_eq!(
            status.get(),
            expected_status,
            "wrong status for {context}: got {status}"
        ),
        Err(other) => panic!("expected Status({expected_status}) for {context}; got {other:?}"),
    }
}

async fn test_get_range_success(
    client: &HttpClient,
    url: Url,
    start: u64,
    end: Option<u64>,
) -> Result<Vec<u8>, NetError> {
    let range = RangeSpec::new(start, end);
    let mut stream = client.get_range(url, range, None).await?;
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk?);
    }
    Ok(collected)
}

async fn test_head_success(client: &HttpClient, url: Url) -> Result<Headers, NetError> {
    client.head(url, None).await
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case("/test", b"Hello, World!")]
#[case("/headers", b"Headers received")]
async fn test_get_bytes_success_cases(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
    #[case] expected_data: &'static [u8],
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let result = test_get_bytes_success(&http_client, url).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from(expected_data));
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case("/test")]
#[case("/headers")]
async fn test_stream_success_cases(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let result = test_stream_success(&http_client, url).await;

    assert!(result.is_ok());
    let expected: &[u8] = match path {
        "/test" => b"Hello, World!",
        "/headers" => b"Headers received",
        _ => unreachable!(),
    };
    assert_eq!(result.unwrap(), expected);
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case(7, Some(11), b"World")]
#[case(0, Some(4), b"Hello")]
#[case(7, None, b"World!")]
async fn test_get_range_success_cases(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] start: u64,
    #[case] end: Option<u64>,
    #[case] expected_data: &[u8],
) {
    let test_server = test_server.await;
    let url = test_server.url("/range");
    let result = test_get_range_success(&http_client, url, start, end).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), expected_data);
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case("/error404", 404, false)]
#[case("/error500", 500, true)]
#[case("/error429", 429, true)]
async fn test_http_errors(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
    #[case] expected_status: u16,
    #[case] is_retryable: bool,
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let result = http_client.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    // A non-retryable status (4xx) surfaces RAW. A retryable status (5xx/429)
    // is retried to exhaustion and then surfaces as a TERMINAL `RetryExhausted`
    // (Fatal) wrapping the original status — so the wrapper shape itself
    // encodes the retryability classification, and the underlying status stays
    // recoverable from `source`.
    let status = match &error {
        NetError::Status { status, .. } => {
            assert!(
                !is_retryable,
                "a retryable status must exhaust into RetryExhausted, not surface raw: {error:?}"
            );
            status.get()
        }
        NetError::RetryExhausted { source, .. } => {
            assert!(
                is_retryable,
                "only a retried (transient) status exhausts into RetryExhausted: {error:?}"
            );
            match source.as_ref() {
                NetError::Status { status, .. } => status.get(),
                other => panic!("RetryExhausted must wrap the HTTP status, got {other:?}"),
            }
        }
        other => panic!("expected Status or RetryExhausted, got {other:?}"),
    };
    assert_eq!(status, expected_status);
}

#[kithara::test(tokio, hang_timeout_secs(1))]
async fn test_head_success_case(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/head-length");
    let result = test_head_success(&http_client, url).await;

    assert!(result.is_ok());
    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("13"));
    assert_eq!(headers.get("content-type"), Some("text/plain"));
}

#[kithara::test(tokio, hang_timeout_secs(1))]
async fn test_range_on_non_range_supporting_server(
    #[future] test_server: TestServer,
    http_client: HttpClient,
) {
    let test_server = test_server.await;
    let url = test_server.url("/ignore-range");

    let range = RangeSpec::new(0, Some(5));
    let result = http_client.get_range(url, range, None).await;

    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(collected, b"Full response ignoring range");
}

#[kithara::test(tokio, hang_timeout_secs(1))]
async fn test_invalid_url() {
    let url = Url::parse("http://192.0.2.1:9999/invalid").unwrap();

    let client = HttpClient::new(
        NetOptions::builder()
            .inactivity_timeout(Duration::from_millis(100))
            .retry_policy(RetryPolicy::builder().max_retries(0).build())
            .build(),
        pools(),
        CancelToken::never(),
    );

    let result = client.get_bytes(url, None).await;

    assert!(result.is_err(), "Should fail for invalid URL");
    let error = result.err().unwrap();

    let is_acceptable_error = matches!(&error, NetError::Timeout | NetError::Network(_));

    assert!(
        is_acceptable_error,
        "Expected timeout or connection error, got {:?}",
        error
    );
}

#[kithara::test(tokio, hang_timeout_secs(1))]
async fn test_stream_cancellation(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/slow-body");

    let result = http_client.stream(url, None).await;
    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let first_chunk = stream.next().await;
    assert!(first_chunk.is_some());
    assert!(first_chunk.unwrap().is_ok());

    drop(stream);
}

#[kithara::test(tokio)]
async fn test_net_builder_with_custom_options() {
    let opts = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(100))
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(2)
                .base_delay(Duration::from_millis(50))
                .max_delay(Duration::from_millis(200))
                .build(),
        )
        .build();

    let client = HttpClient::new(opts, pools(), CancelToken::never());

    let server = TestServer::new(test_router()).await;
    let url = server.url("/test");

    let result = client.get_bytes(url, None).await;
    assert!(result.is_ok(), "HttpClient with custom options should work");
}

#[kithara::test(tokio)]
#[case::headers_passthrough("/key-with-auth", Some("secret-key-token"), 0xab)]
#[case::query_params_passthrough(
    "/key-with-params?drm_id=test123&version=1.0&extra=ignored",
    None,
    0xfe
)]
async fn test_key_request_passthrough(
    #[case] path: &str,
    #[case] bearer: Option<&str>,
    #[case] expected_first_byte: u8,
) {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default(), pools(), CancelToken::never());
    let url = server.url(path);
    let headers = bearer.map(auth_bearer);

    let mut stream = client.stream(url, headers).await.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(collected.len(), 16);
    assert_eq!(collected[0], expected_first_byte);
}

#[kithara::test(tokio)]
#[case::missing_auth_header("/key-with-auth", None, 401, "missing auth header")]
#[case::wrong_auth_header("/key-with-auth", Some("wrong-token"), 401, "wrong auth header")]
#[case::missing_required_query(
    "/key-with-params?drm_id=test123",
    None,
    400,
    "missing required query params"
)]
#[case::wrong_query_params(
    "/key-with-params?drm_id=wrong&version=1.0",
    None,
    400,
    "wrong query params"
)]
async fn test_key_request_rejects_bad_credentials(
    #[case] path: &str,
    #[case] bearer: Option<&str>,
    #[case] expected_status: u16,
    #[case] context: &str,
) {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default(), pools(), CancelToken::never());
    let url = server.url(path);
    let headers = bearer.map(auth_bearer);

    let result = client.stream(url, headers).await;
    assert_http_status(result.map(|_| "stream"), expected_status, context);
}

#[kithara::test(tokio)]
async fn test_key_request_range_with_headers() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default(), pools(), CancelToken::never());
    let url = server.url("/key-with-auth");

    let mut stream = client
        .get_range(
            url,
            RangeSpec::new(0, Some(7)),
            Some(auth_bearer("secret-key-token")),
        )
        .await
        .unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected.len(), 8);
    assert_eq!(collected[0], 0xab);
}

#[derive(Copy, Clone)]
enum TimeoutOp {
    GetBytes,
    Stream,
    GetRange { start: u64, end: Option<u64> },
}

#[derive(Default)]
struct HeadArrivals(Mutex<Vec<Duration>>);

impl NetObserver for HeadArrivals {
    fn first_byte(&self, elapsed: Duration, _status: u16, _partial: bool) {
        self.0.lock().unwrap().push(elapsed);
    }
}

#[kithara::test(
    tokio,
    timeout(::std::time::Duration::from_millis(1842)),
    hang_timeout_secs(1)
)]
#[case::slow_alive_stream_completes(
    "/slow-body",
    Duration::from_millis(400),
    TimeoutOp::Stream,
    Ok(&b"Hello, World!"[..]),
    1
)]
#[case::slow_alive_whole_body_completes(
    "/slow-body",
    Duration::from_millis(400),
    TimeoutOp::GetBytes,
    Ok(&b"Hello, World!"[..]),
    1
)]
#[case::body_silence_times_out_on_get_bytes(
    "/slow-body",
    Duration::from_millis(50),
    TimeoutOp::GetBytes,
    Err(NetError::Timeout),
    1
)]
#[case::header_silence_times_out_on_stream(
    "/slow-headers",
    Duration::from_millis(100),
    TimeoutOp::Stream,
    Err(NetError::Timeout),
    0
)]
#[case::header_silence_times_out_on_range(
    "/slow-headers",
    Duration::from_millis(100),
    TimeoutOp::GetRange { start: 0, end: Some(10) },
    Err(NetError::Timeout),
    0
)]
async fn inactivity_timeout_bounds_silence_not_total_time(
    #[case] path: &str,
    #[case] inactivity: Duration,
    #[case] op: TimeoutOp,
    #[case] expected: Result<&'static [u8], NetError>,
    #[case] expected_heads: usize,
) {
    let server = TestServer::new(test_router()).await;
    let arrivals = Arc::new(HeadArrivals::default());
    let client = HttpClient::new(
        NetOptions::builder()
            .inactivity_timeout(inactivity)
            .retry_policy(RetryPolicy::builder().max_retries(0).build())
            .build(),
        pools(),
        CancelToken::never(),
    )
    .with_observer(Some(Observer(arrivals.clone())));
    let url = server.url(path);
    let wall = WallInstant::now();

    let result = match op {
        TimeoutOp::GetBytes => client
            .get_bytes(url, None)
            .await
            .map(|bytes| bytes.to_vec()),
        TimeoutOp::Stream => test_stream_success(&client, url).await,
        TimeoutOp::GetRange { start, end } => {
            test_get_range_success(&client, url, start, end).await
        }
    };

    assert_eq!(arrivals.0.lock().unwrap().len(), expected_heads);
    match (&result, &expected) {
        (Ok(body), Ok(expected_body)) => assert_eq!(body.as_slice(), *expected_body),
        (Err(NetError::Timeout), Err(NetError::Timeout)) => {
            assert!(
                wall.elapsed() >= inactivity,
                "real I/O timed out prematurely"
            );
        }
        _ => panic!(
            "expected {expected:?}, got {result:?}; response heads: {:?}",
            arrivals.0.lock().unwrap()
        ),
    }
}

#[derive(Default)]
struct BackoffStarted(Notify);

impl NetObserver for BackoffStarted {
    fn retrying(&self, _attempt: u32, _max_retries: u32, _error: &NetError, backoff: Duration) {
        if !backoff.is_zero() {
            self.0.notify_one();
        }
    }
}

#[kithara::test(tokio, flash(false), hang_timeout_secs(1))]
async fn cancel_during_backoff_ends_the_call_without_the_delay() {
    let server = TestServer::new(test_router()).await;
    let cancel = CancelToken::root();
    let backoff_started = Arc::new(BackoffStarted::default());
    let options = NetOptions::builder()
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(3)
                .base_delay(Duration::from_secs(10))
                .max_delay(Duration::from_secs(10))
                .build(),
        )
        .build();
    let client = HttpClient::new(options, pools(), cancel.clone())
        .with_observer(Some(Observer(backoff_started.clone())));
    let cancel_in_backoff = async {
        backoff_started.0.notified().await;
        sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    };

    let (result, ()) = join!(
        client.get_bytes(server.url("/error500"), None),
        cancel_in_backoff
    );

    match result {
        Err(NetError::Cancelled) => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case::budget_spent(2, None, "/error500", true)]
#[case::no_budget(0, None, "/error500", false)]
#[case::fatal_cause(2, None, "/error404", false)]
#[case::policy_after_observer(0, Some(2), "/error500", true)]
async fn establish_exhaustion_is_reported_once_with_its_cause(
    #[case] max_retries: u32,
    #[case] then_max_retries: Option<u32>,
    #[case] path: &str,
    #[case] reported: bool,
) {
    let server = TestServer::new(test_router()).await;
    let reports = Arc::new(Reports::default());
    let options = NetOptions::builder()
        .retry_policy(fast_retry_policy(max_retries))
        .build();
    let observed = HttpClient::new(options, pools(), CancelToken::never())
        .with_observer(Some(Observer(reports.clone())));
    let client = match then_max_retries {
        Some(max_retries) => observed.with_retry_policy(fast_retry_policy(max_retries)),
        None => observed,
    };

    let _ = client.get_bytes(server.url(path), None).await;

    let exhausted: Vec<Report> = reports
        .take()
        .into_iter()
        .filter(|report| matches!(report, Report::Exhausted { .. }))
        .collect();
    if reported {
        assert!(
            matches!(
                exhausted.as_slice(),
                [Report::Exhausted {
                    max_retries: 2,
                    consumed: 0,
                    error: NetError::Status { status, .. },
                }] if status.get() == 500
            ),
            "expected one exhaustion report (2, 0, HTTP 500), got {exhausted:?}"
        );
    } else {
        assert!(
            exhausted.is_empty(),
            "expected no exhaustion report, got {exhausted:?}"
        );
    }
}

#[derive(Debug)]
enum Report {
    FirstByte {
        status: u16,
    },
    Retrying {
        attempt: u32,
        max_retries: u32,
    },
    Exhausted {
        max_retries: u32,
        consumed: u64,
        error: NetError,
    },
}

/// Records every report in order.
#[derive(Default)]
struct Reports(Mutex<Vec<Report>>);

impl Reports {
    fn take(&self) -> Vec<Report> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl NetObserver for Reports {
    fn first_byte(&self, _ttfb: Duration, status: u16, _partial: bool) {
        self.0.lock().unwrap().push(Report::FirstByte { status });
    }

    fn retry_exhausted(&self, max_retries: u32, consumed: u64, error: &NetError) {
        self.0.lock().unwrap().push(Report::Exhausted {
            max_retries,
            consumed,
            error: error.clone(),
        });
    }

    fn retrying(&self, attempt: u32, max_retries: u32, _error: &NetError, _backoff: Duration) {
        self.0.lock().unwrap().push(Report::Retrying {
            attempt,
            max_retries,
        });
    }
}

fn fast_retry_policy(max_retries: u32) -> RetryPolicy {
    RetryPolicy::builder()
        .max_retries(max_retries)
        .base_delay(Duration::from_millis(1))
        .max_delay(Duration::from_millis(5))
        .build()
}

fn fast_retry_client() -> HttpClient {
    HttpClient::new(
        NetOptions::builder()
            .retry_policy(fast_retry_policy(2))
            .build(),
        pools(),
        CancelToken::never(),
    )
}

#[derive(Copy, Clone, Debug)]
enum Op {
    GetBytes,
    Head,
    Post,
    Stream,
    Range,
}

/// The body of a successful `op`: empty for `head`.
async fn call(client: &HttpClient, url: Url, op: Op) -> Result<Vec<u8>, NetError> {
    match op {
        Op::GetBytes => client.get_bytes(url, None).await.map(|body| body.to_vec()),
        Op::Head => client.head(url, None).await.map(|_| Vec::new()),
        Op::Post => client
            .post_bytes(url, Bytes::from_static(b"ping"), None)
            .await
            .map(|body| body.to_vec()),
        Op::Stream => test_stream_success(client, url).await,
        Op::Range => test_get_range_success(client, url, 0, Some(1)).await,
    }
}

#[kithara::test(tokio, hang_timeout_secs(1))]
#[case::get_bytes(Op::GetBytes)]
#[case::stream(Op::Stream)]
#[case::range(Op::Range)]
#[case::head(Op::Head)]
async fn an_observed_handle_reports_every_attempt(#[case] op: Op) {
    let server = TestServer::new(test_router()).await;
    let client = fast_retry_client();
    let reports = Arc::new(Reports::default());
    let observed = client.with_observer(Some(Observer(reports.clone())));
    let url = server.url("/error500");

    call(&observed, url.clone(), op)
        .await
        .expect_err("every attempt answers 500");
    let observed_reports = reports.take();
    call(&client, url, op)
        .await
        .expect_err("every attempt answers 500");

    let is_500 =
        |error: &NetError| matches!(error, NetError::Status { status, .. } if status.get() == 500);
    let reported = match op {
        Op::Head => matches!(
            observed_reports
                .iter()
                .filter(|report| !matches!(report, Report::FirstByte { .. }))
                .collect::<Vec<_>>()
                .as_slice(),
            [
                Report::Retrying {
                    attempt: 1,
                    max_retries: 2
                },
                Report::Retrying {
                    attempt: 2,
                    max_retries: 2
                },
                Report::Exhausted {
                    max_retries: 2,
                    consumed: 0,
                    error,
                },
            ] if is_500(error)
        ),
        _ => matches!(
            observed_reports.as_slice(),
            [
                Report::FirstByte { status: 500 },
                Report::Retrying {
                    attempt: 1,
                    max_retries: 2
                },
                Report::FirstByte { status: 500 },
                Report::Retrying {
                    attempt: 2,
                    max_retries: 2
                },
                Report::FirstByte { status: 500 },
                Report::Exhausted {
                    max_retries: 2,
                    consumed: 0,
                    error,
                },
            ] if is_500(error)
        ),
    };
    assert!(reported, "got {observed_reports:?}");
    assert!(
        reports.take().is_empty(),
        "the base client reports to no observer"
    );
}

/// `/flaky-body` answers 503 to its first request and `ok`, or the posted
/// body, after; the counter counts every request.
fn flaky_body_router() -> (Router, RequestCounter) {
    let counter = RequestCounter::new();
    let router = Router::new()
        .route(
            "/flaky-body",
            get(|State(counter): State<RequestCounter>| async move {
                if counter.increment() == 0 {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    "ok".into_response()
                }
            })
            .post(
                |State(counter): State<RequestCounter>, body: Bytes| async move {
                    if counter.increment() == 0 {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        body.into_response()
                    }
                },
            ),
        )
        .with_state(counter.clone());
    (router, counter)
}

#[kithara::test(tokio, hang_timeout_secs(1), timeout(Short))]
#[case::get_bytes(Op::GetBytes, b"ok")]
#[case::head(Op::Head, b"")]
#[case::post(Op::Post, b"ping")]
#[case::stream(Op::Stream, b"ok")]
#[case::range(Op::Range, b"ok")]
async fn a_transient_status_is_retried(#[case] op: Op, #[case] expected: &[u8]) {
    let (router, counter) = flaky_body_router();
    let server = TestServer::new(router).await;

    let body = call(&fast_retry_client(), server.url("/flaky-body"), op).await;

    assert_eq!(body.expect("the retried request answers"), expected);
    assert_eq!(counter.count.load(Ordering::SeqCst), 2);
}
