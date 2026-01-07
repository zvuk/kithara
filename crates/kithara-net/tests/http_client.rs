use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, head},
};
use bytes::Bytes;
use futures::StreamExt;
use kithara_net::{Headers, HttpClient, Net, NetError, NetExt, RangeSpec, RetryPolicy};
use rstest::*;
use tokio::net::TcpListener;
use url::Url;

// Test server fixture
struct TestServer {
    base_url: Url,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestServer {
    async fn new(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        });

        tokio::spawn(async move {
            server.await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            base_url: Url::parse(&format!("http://{}", addr)).unwrap(),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    fn url(&self, path: &str) -> Url {
        self.base_url.join(path).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Use take() to move the Sender out of self
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}

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
        if range_str.starts_with("bytes=") {
            let range = &range_str[6..];
            let parts: Vec<&str> = range.split('-').collect();
            if parts.len() == 2 {
                // Handle open-ended range (e.g., "bytes=7-")
                let start_result = parts[0].parse::<u64>();
                let end_result = if parts[1].is_empty() {
                    Ok(None)
                } else {
                    parts[1].parse::<u64>().map(Some)
                };

                if let (Ok(start), Ok(end_opt)) = (start_result, end_result) {
                    let data = "Hello, World!".as_bytes();
                    let end = end_opt.unwrap_or((data.len() - 1) as u64);

                    // Validate range
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

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
}

async fn ignore_range_endpoint() -> &'static str {
    "Full response ignoring range"
}

// Request counter for testing retries
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

    fn get(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
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

#[fixture]
async fn test_server(test_router: Router) -> TestServer {
    TestServer::new(test_router).await
}

#[fixture]
fn http_client() -> HttpClient {
    HttpClient::new(kithara_net::NetOptions::default())
}

// Parameterized test data
#[derive(Debug, Clone)]
struct TestCase {
    path: &'static str,
    expected_status: Option<u16>,
    expected_data: Option<&'static [u8]>,
    is_retryable: bool,
}

#[fixture]
fn success_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            path: "/test",
            expected_status: Some(200),
            expected_data: Some(b"Hello, World!"),
            is_retryable: false,
        },
        TestCase {
            path: "/headers",
            expected_status: Some(200),
            expected_data: Some(b"Headers received"),
            is_retryable: false,
        },
    ]
}

#[fixture]
fn error_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            path: "/error404",
            expected_status: Some(404),
            expected_data: None,
            is_retryable: false,
        },
        TestCase {
            path: "/error500",
            expected_status: Some(500),
            expected_data: None,
            is_retryable: true,
        },
        TestCase {
            path: "/error429",
            expected_status: Some(429),
            expected_data: None,
            is_retryable: true,
        },
    ]
}

#[fixture]
fn range_cases() -> Vec<(u64, Option<u64>, &'static [u8])> {
    vec![
        (0, Some(4), b"Hello"),
        (7, Some(11), b"World"),
        (7, None, b"World!"),
        (0, None, b"Hello, World!"),
    ]
}

// Basic functionality tests - parameterized
#[rstest]
#[case("/test", b"Hello, World!")]
#[case("/headers", b"Headers received")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_get_bytes_success(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
    #[case] expected_data: &'static [u8],
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let result = http_client.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from(expected_data));
}

#[rstest]
#[case("/test")]
#[case("/headers")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_stream_success(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let result = http_client.stream(url, None).await;

    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }

    let expected: &[u8] = match path {
        "/test" => b"Hello, World!",
        "/headers" => b"Headers received",
        _ => unreachable!(),
    };
    assert_eq!(collected.as_slice(), expected);
}

// Range tests - parameterized
#[rstest]
#[case(7, Some(11), b"World")]
#[case(0, Some(4), b"Hello")]
#[case(7, None, b"World!")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_get_range_success(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] start: u64,
    #[case] end: Option<u64>,
    #[case] expected_data: &[u8],
) {
    let test_server = test_server.await;
    let url = test_server.url("/range");
    let range = RangeSpec::new(start, end);

    let result = http_client.get_range(url, range, None).await;

    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(collected, expected_data);
}

// Error handling tests - parameterized
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

    let error = result.unwrap_err();
    assert!(matches!(error, NetError::HttpError { status, .. } if status == expected_status));
    assert_eq!(error.is_retryable(), is_retryable);
}

// Head tests
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_head_success(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/head-length");
    let result = http_client.head(url, None).await;

    assert!(result.is_ok());

    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("13"));
    assert_eq!(headers.get("content-type"), Some("text/plain"));
}

// Headers tests - parameterized
#[rstest]
#[case(None)]
#[case(Some(Headers::new()))]
#[case(Some({
    let mut h = Headers::new();
    h.insert("X-Custom-Header", "test");
    h
}))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_headers_variants(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] headers: Option<Headers>,
) {
    let test_server = test_server.await;
    let url = test_server.url("/test");
    let result = http_client.get_bytes(url, headers).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from("Hello, World!"));
}

// Timeout layer tests - parameterized
#[rstest]
#[case("/slow-body", Duration::from_millis(100))]
#[case("/slow-headers", Duration::from_millis(100))]
#[case("/timeout-test", Duration::from_millis(100))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_variants(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] path: &str,
    #[case] timeout_duration: Duration,
) {
    let test_server = test_server.await;
    let url = test_server.url(path);
    let timeout_client = http_client.with_timeout(timeout_duration);

    let result: Result<bytes::Bytes, NetError> = match path {
        "/slow-body" => timeout_client.get_bytes(url, None).await,
        "/slow-headers" => timeout_client
            .stream(url, None)
            .await
            .map(|_| bytes::Bytes::new()),
        "/timeout-test" => timeout_client
            .head(url, None)
            .await
            .map(|_| bytes::Bytes::new()),
        _ => unreachable!(),
    };

    assert!(result.is_err());
    let error = result.err().unwrap();
    assert!(matches!(error, NetError::Timeout));
}

// Retry layer tests - parameterized
#[rstest]
#[case(0, false)] // 0 retries should fail immediately (attempts 0: error)
#[case(1, false)] // 1 retry should fail (attempts 0: error, 1: error, no more retries)
#[case(2, true)] // 2 retries should succeed (attempts 0: error, 1: error, 2: success)
#[case(3, true)] // 3 retries should succeed
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_retry_variants(
    #[future] test_server: TestServer,
    http_client: HttpClient,
    #[case] max_retries: u32,
    #[case] should_succeed: bool,
) {
    let test_server = test_server.await;
    let url = test_server.url("/retry-test");
    let retry_policy = RetryPolicy::new(
        max_retries,
        Duration::from_millis(10),
        Duration::from_millis(100),
    );
    let retry_client = http_client.with_retry(retry_policy);

    let result = retry_client.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from("Success after retries"));
    } else {
        assert!(result.is_err());
        let error = result.err().unwrap();
        // For max_retries=0 or 1, we should get an error
        // With max_retries=0: no retry, original error
        // With max_retries=1: one retry, still error (server fails on attempts 0 and 1)
        match error {
            NetError::HttpError { status, .. } => {
                assert_eq!(status, 500);
            }
            NetError::RetryExhausted { max_retries, .. } => {
                // This could happen with certain retry implementations
                assert!(max_retries <= 1);
            }
            _ => panic!(
                "Expected HttpError with status 500 or RetryExhausted, got {:?}",
                error
            ),
        }
    }
}

// Edge cases
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_range_on_non_range_supporting_server(
    #[future] test_server: TestServer,
    http_client: HttpClient,
) {
    let test_server = test_server.await;
    let url = test_server.url("/ignore-range");
    let range = RangeSpec::new(0, Some(4));

    let result = http_client.get_range(url, range, None).await;

    // Should still work, just return full response
    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = Vec::new();

    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(collected, b"Full response ignoring range");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_invalid_url(http_client: HttpClient) {
    let url = Url::parse("http://invalid-domain-that-does-not-exist-12345.test").unwrap();

    let result = http_client.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(matches!(error, NetError::Http(_)));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_stream_cancellation(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/slow-body");

    let result = http_client.stream(url, None).await;
    assert!(result.is_ok());

    // Take only first chunk and drop the stream
    let mut stream = result.unwrap();
    let first_chunk = stream.next().await;

    assert!(first_chunk.is_some());
    assert!(first_chunk.unwrap().is_ok());

    // Stream should be dropped here without consuming all data
}

// Test NetExt trait methods
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_net_ext_chaining(#[future] test_server: TestServer, http_client: HttpClient) {
    let test_server = test_server.await;
    let url = test_server.url("/test");

    // Chain timeout and retry
    let client = http_client
        .with_timeout(Duration::from_secs(5))
        .with_retry(RetryPolicy::new(
            3,
            Duration::from_millis(10),
            Duration::from_millis(100),
        ));

    let result = client.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from("Hello, World!"));
}
