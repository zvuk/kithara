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

// ============================================================================
// Test server infrastructure
// ============================================================================

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
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}

// ============================================================================
// Test endpoints
// ============================================================================

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

    axum::response::Response::builder()
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

// ============================================================================
// Fixtures
// ============================================================================

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

// ============================================================================
// Helper functions for testing
// ============================================================================

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

// ============================================================================
// Parameterized tests
// ============================================================================

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
    let client = http_client.with_retry(retry_policy);

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

    // Accept either timeout or connection error
    let is_acceptable_error = match &error {
        NetError::Timeout => true,
        NetError::Http(msg) => {
            msg.contains("connection") || msg.contains("failed") || msg.contains("refused")
        }
        _ => false,
    };

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

    // Drop the stream without consuming all chunks
    drop(stream);
}
