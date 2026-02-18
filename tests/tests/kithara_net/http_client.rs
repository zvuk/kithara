use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, head},
};
use bytes::Bytes;
use futures::StreamExt;
use kithara::net::{
    Headers, HttpClient, Net, NetError, NetExt, NetOptions, RangeSpec, RetryPolicy, TimeoutNet,
};
use kithara_test_utils::TestHttpServer;
use rstest::*;
use tokio_util::sync::CancellationToken;
use url::Url;

// Test server infrastructure

type TestServer = TestHttpServer;

// Test endpoints

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
                        let slice = &data[start as usize..=end as usize];
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
    tokio::time::sleep(Duration::from_millis(500)).await;
    "Slow headers"
}

async fn slow_body_endpoint() -> impl IntoResponse {
    let stream = futures::stream::iter(vec![
        Ok::<_, axum::BoxError>(Bytes::from("Hello")),
        Ok(Bytes::from(", ")),
        Ok(Bytes::from("World")),
        Ok(Bytes::from("!")),
    ])
    .then(|chunk| async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        chunk
    });

    Response::builder()
        .status(StatusCode::OK)
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
}

async fn ignore_range_endpoint() -> &'static str {
    "Full response ignoring range"
}

#[derive(Clone, Default)]
struct RequestCounter {
    count: Arc<std::sync::atomic::AtomicUsize>,
}

impl RequestCounter {
    fn new() -> Self {
        Self {
            count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn increment(&self) -> usize {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
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

async fn retry_test_endpoint(State(counter): State<RequestCounter>) -> impl IntoResponse {
    let attempt = counter.increment();

    match attempt {
        0 | 1 => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        _ => "Success after retries".into_response(),
    }
}

async fn timeout_test_endpoint() -> impl IntoResponse {
    tokio::time::sleep(Duration::from_secs(2)).await;
    "Should timeout"
}

async fn key_endpoint(
    State(counter): State<KeyRequestCounter>,
    _request: Request,
) -> Result<Response, StatusCode> {
    counter.increment("/key");

    let key_bytes = vec![
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32,
        0x10,
    ];
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(axum::body::Body::from(Bytes::from(key_bytes)))
        .unwrap())
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
            let key_bytes = vec![
                0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
                0x32, 0x10,
            ];

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
                        .body(axum::body::Body::from(Bytes::copy_from_slice(slice)))
                        .unwrap());
                }

                Err(StatusCode::BAD_REQUEST)
            } else {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/octet-stream")
                    .body(axum::body::Body::from(Bytes::from(key_bytes)))
                    .unwrap())
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
        let key_bytes = vec![
            0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0x12, 0x34,
            0x56, 0x78,
        ];
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::from(Bytes::from(key_bytes)))
            .unwrap())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

// Fixtures

#[fixture]
fn test_router() -> Router {
    let counter = RequestCounter::new();

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
        .route("/retry-test", get(retry_test_endpoint))
        .with_state(counter)
        .route("/timeout-test", get(timeout_test_endpoint))
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

#[fixture]
async fn test_server(test_router: Router) -> TestServer {
    TestServer::new(test_router).await
}

#[fixture]
fn http_client() -> HttpClient {
    HttpClient::new(NetOptions::default())
}

// Helper functions for testing

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

// Parameterized tests

#[rstest]
#[case("/test", b"Hello, World!")]
#[case("/headers", b"Headers received")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

#[rstest]
#[case("/test")]
#[case("/headers")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

#[rstest]
#[case(7, Some(11), b"World")]
#[case(0, Some(4), b"Hello")]
#[case(7, None, b"World!")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

// Error handling tests - simplified to handle HttpError variant
#[rstest]
#[case("/error404", 404, false)]
#[case("/error500", 500, true)]
#[case("/error429", 429, true)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

    // Check if it's an HTTP error (either NetError::Http or NetError::HttpError)
    let is_http_error = match &error {
        NetError::Http(_) => true,
        NetError::HttpError { status, .. } => {
            assert_eq!(*status, expected_status);
            true
        }
        _ => false,
    };

    assert!(is_http_error, "Expected HTTP error, got {:?}", error);
    assert_eq!(error.is_retryable(), is_retryable);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_head_success_case(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/head-length");
    let result = test_head_success(&http_client, url).await;

    assert!(result.is_ok());
    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("13"));
    assert_eq!(headers.get("content-type"), Some("text/plain"));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_headers_variants(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/headers");

    let mut headers = Headers::new();
    headers.insert("X-Custom-Header", "test-value");
    headers.insert("User-Agent", "test-agent");

    let result = http_client.get_bytes(url, Some(headers)).await;

    assert!(result.is_ok());
    let data = result.unwrap();
    assert_eq!(data, Bytes::from("Headers received"));
}

// Fix timeout tests - use appropriate timeouts
#[rstest]
#[case("/slow-headers", Duration::from_millis(1000), true)]
#[case("/slow-body", Duration::from_millis(1000), true)]
#[case("/timeout-test", Duration::from_millis(300), false)]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_timeout_variants(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let client = http_client.with_timeout(timeout);

    let result = client.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok(), "Should succeed for path: {}", path);
    } else {
        assert!(result.is_err(), "Should fail for path: {}", path);
        let error = result.err().unwrap();
        assert!(
            matches!(error, NetError::Timeout),
            "Expected timeout error, got {:?}",
            error
        );
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_retry_variants(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/retry-test");

    let retry_policy = RetryPolicy::new(2, Duration::from_millis(10), Duration::from_secs(5));
    let client = http_client.with_retry(retry_policy, CancellationToken::new());

    let result = client.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from("Success after retries"));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

// Fix invalid URL test - use shorter timeout and expect connection error
#[rstest]
#[timeout(Duration::from_secs(1))]
#[tokio::test]
async fn test_invalid_url(http_client: HttpClient) {
    // Use a URL that should fail quickly (non-routable IP)
    let url = Url::parse("http://192.0.2.1:9999/invalid").unwrap();

    // Use a very short timeout for this test
    let client = http_client.with_timeout(Duration::from_millis(100));

    let result = client.get_bytes(url, None).await;

    assert!(result.is_err(), "Should fail for invalid URL");
    let error = result.err().unwrap();

    // Accept either timeout or any HTTP error for a non-routable address —
    // the point is that the request fails, not the exact error wording.
    let is_acceptable_error = matches!(&error, NetError::Timeout | NetError::Http(_));

    assert!(
        is_acceptable_error,
        "Expected timeout or connection error, got {:?}",
        error
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
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

#[tokio::test]
async fn test_stream_get_returns_expected_bytes() {
    let server = TestServer::new(test_router()).await;
    let url = server.url("/test");

    let client = HttpClient::new(NetOptions::default());
    let mut stream = client.stream(url, None).await.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected, b"Hello, World!");
}

#[tokio::test]
async fn test_range_request_returns_correct_slice() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/range");

    let mut stream = client
        .get_range(url, RangeSpec::new(7, Some(11)), None)
        .await
        .unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected, b"World");
}

#[tokio::test]
async fn test_headers_are_sent_correctly() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/headers");

    let mut headers = HashMap::new();
    headers.insert("X-Custom-Header".to_string(), "custom-value".to_string());

    let result = client.get_bytes(url, Some(headers.into())).await.unwrap();
    assert_eq!(result, Bytes::from("Headers received"));
}

#[tokio::test]
async fn test_get_bytes_simple() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/test");

    let bytes = client.get_bytes(url, None).await.unwrap();
    assert_eq!(bytes, Bytes::from("Hello, World!"));
}

#[tokio::test]
async fn test_head_returns_content_length() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/head-length");

    let headers = client.head(url, None).await.unwrap();
    let content_length = headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"));
    assert_eq!(content_length, Some("13"));
}

#[tokio::test]
async fn test_http_error_404_returns_error() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/error404");

    let result = client.get_bytes(url, None).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        NetError::HttpError { status, .. } => assert_eq!(status, 404),
        _ => panic!("Expected HTTP error"),
    }
}

#[tokio::test]
async fn test_http_error_500_returns_error() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/error500");

    let result = client.get_bytes(url, None).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        NetError::HttpError { status, .. } => assert_eq!(status, 500),
        _ => panic!("Expected HTTP error"),
    }
}

#[tokio::test]
async fn test_timeout_behavior() {
    let server = TestServer::new(test_router()).await;
    let base = HttpClient::new(NetOptions::default());
    let timeout_duration = Duration::from_millis(10);
    let timeout_client = base.with_timeout(timeout_duration);

    let url = server.url("/test");
    let result = timeout_client.get_bytes(url, None).await;
    assert!(result.is_ok(), "Request should succeed within timeout");
}

#[tokio::test]
async fn test_retry_policy_exponential_backoff() {
    let policy = RetryPolicy::new(3, Duration::from_millis(10), Duration::from_millis(100));

    assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(0));
    assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(10));
    assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(20));
    assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(40));
    assert_eq!(policy.delay_for_attempt(10), Duration::from_millis(100));
}

#[tokio::test]
async fn test_net_builder_creates_functional_client() {
    let client = HttpClient::new(NetOptions::default());
    let server = TestServer::new(test_router()).await;
    let url = server.url("/test");

    let result = client.get_bytes(url, None).await;
    assert!(
        result.is_ok(),
        "NetBuilder client should work like regular client"
    );
}

#[tokio::test]
async fn test_net_builder_with_custom_options() {
    let opts = NetOptions {
        request_timeout: Duration::from_millis(100),
        retry_policy: RetryPolicy::new(2, Duration::from_millis(50), Duration::from_millis(200)),
        ..Default::default()
    };

    let client = HttpClient::new(opts);

    let server = TestServer::new(test_router()).await;
    let url = server.url("/test");

    let result = client.get_bytes(url, None).await;
    assert!(result.is_ok(), "HttpClient with custom options should work");
}

#[tokio::test]
async fn test_key_request_headers_passthrough() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-auth");

    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer secret-key-token".to_string(),
    );

    let mut stream = client.stream(url, Some(headers.into())).await.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected.len(), 16);
    assert_eq!(collected[0], 0xab);
}

#[tokio::test]
async fn test_key_request_missing_required_headers_fails() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-auth");

    let stream_result = client.stream(url, None).await;
    assert!(
        stream_result.is_err(),
        "Stream creation should fail when auth header is missing"
    );

    match stream_result {
        Ok(_) => panic!("Expected error but got success"),
        Err(NetError::HttpError { status, .. }) => assert_eq!(status, 401),
        Err(_) => panic!("Expected HTTP status error with 401 status"),
    }
}

#[tokio::test]
async fn test_key_request_wrong_auth_header_fails() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-auth");

    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer wrong-token".to_string(),
    );

    let result = client.stream(url, Some(headers.into())).await;
    assert!(
        result.is_err(),
        "Stream creation should fail when auth header is wrong"
    );

    match result {
        Err(NetError::HttpError { status, .. }) => assert_eq!(status, 401),
        Err(_) => panic!("Expected HTTP status error"),
        Ok(_) => panic!("Expected error"),
    }
}

#[tokio::test]
async fn test_key_request_query_params_passthrough() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-params?drm_id=test123&version=1.0&extra=ignored");

    let mut stream = client.stream(url, None).await.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected.len(), 16);
    assert_eq!(collected[0], 0xfe);
}

#[tokio::test]
async fn test_key_request_missing_required_query_params_fails() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-params?drm_id=test123");

    let stream_result = client.stream(url, None).await;
    assert!(
        stream_result.is_err(),
        "Stream creation should fail when query params are missing"
    );

    match stream_result {
        Ok(_) => panic!("Expected error but got success"),
        Err(NetError::HttpError { status, .. }) => assert_eq!(status, 400),
        Err(_) => panic!("Expected HTTP status error with 400 status"),
    }
}

#[tokio::test]
async fn test_key_request_wrong_query_params_fails() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-params?drm_id=wrong&version=1.0");

    let stream_result = client.stream(url, None).await;
    assert!(
        stream_result.is_err(),
        "Stream creation should fail when query params are wrong"
    );

    match stream_result {
        Ok(_) => panic!("Expected error but got success"),
        Err(NetError::HttpError { status, .. }) => assert_eq!(status, 400),
        Err(_) => panic!("Expected HTTP status error with 400 status"),
    }
}

#[tokio::test]
async fn test_key_request_stream_with_headers() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-auth");

    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer secret-key-token".to_string(),
    );

    let mut stream = client.stream(url, Some(headers.into())).await.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected.len(), 16);
    assert_eq!(collected[0], 0xab);
}

#[tokio::test]
async fn test_key_request_range_with_headers() {
    let server = key_test_server().await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/key-with-auth");

    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer secret-key-token".to_string(),
    );

    let mut stream = client
        .get_range(url, RangeSpec::new(0, Some(7)), Some(headers.into()))
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

#[tokio::test]
async fn test_no_mid_stream_retry() {
    let timeout_error = NetError::Timeout;
    assert!(
        timeout_error.is_retryable(),
        "Timeout errors should be retryable (happen before body)"
    );

    let http_500_error = NetError::HttpError {
        status: 500,
        url: Url::parse("http://example.com").unwrap(),
        body: None,
    };
    assert!(
        http_500_error.is_retryable(),
        "HTTP 5xx errors should be retryable (happen before body)"
    );

    let http_429_error = NetError::HttpError {
        status: 429,
        url: Url::parse("http://example.com").unwrap(),
        body: None,
    };
    assert!(
        http_429_error.is_retryable(),
        "HTTP 429 errors should be retryable"
    );

    let http_408_error = NetError::HttpError {
        status: 408,
        url: Url::parse("http://example.com").unwrap(),
        body: None,
    };
    assert!(
        http_408_error.is_retryable(),
        "HTTP 408 errors should be retryable (as per reference spec)"
    );

    let http_404_error = NetError::HttpError {
        status: 404,
        url: Url::parse("http://example.com").unwrap(),
        body: None,
    };
    assert!(
        !http_404_error.is_retryable(),
        "HTTP 4xx errors (except 408/429) should not be retryable"
    );
}

#[tokio::test]
async fn test_timeout_matrix_get_bytes_times_out_on_body() {
    let server = TestServer::new(test_router()).await;
    let base = HttpClient::new(NetOptions::default());

    let timeout_client = TimeoutNet::new(base, Duration::from_millis(50));

    let url = server.url("/slow-body");

    let result = timeout_client.get_bytes(url, None).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        NetError::Timeout => (),
        other => panic!("Expected Timeout error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_timeout_matrix_stream_times_out_on_headers() {
    let server = TestServer::new(test_router()).await;
    let base = HttpClient::new(NetOptions::default());

    let timeout_client = TimeoutNet::new(base, Duration::from_millis(100));

    let url = server.url("/slow-headers");

    let result = timeout_client.stream(url, None).await;
    assert!(result.is_err());

    match result {
        Err(NetError::Timeout) => (),
        Ok(_) => panic!("Expected Timeout error, got Ok"),
        Err(e) => panic!("Expected Timeout error, got {:?}", e),
    }
}

#[tokio::test]
async fn test_timeout_matrix_get_range_times_out_on_headers() {
    let server = TestServer::new(test_router()).await;
    let base = HttpClient::new(NetOptions::default());

    let timeout_client = TimeoutNet::new(base, Duration::from_millis(100));

    let url = server.url("/slow-headers");
    let range = RangeSpec::new(0, Some(10));

    let result = timeout_client.get_range(url, range, None).await;
    assert!(result.is_err());

    match result {
        Err(NetError::Timeout) => (),
        Ok(_) => panic!("Expected Timeout error, got Ok"),
        Err(e) => panic!("Expected Timeout error, got {:?}", e),
    }
}

#[tokio::test]
async fn test_range_behavior_contract() {
    let server = TestServer::new(test_router()).await;
    let client = HttpClient::new(NetOptions::default());
    let url = server.url("/ignore-range");

    let mut stream = client
        .get_range(url, RangeSpec::new(0, Some(5)), None)
        .await
        .unwrap();
    let mut collected = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        collected.extend_from_slice(&chunk);
    }

    assert_eq!(collected, b"Full response ignoring range");
}
