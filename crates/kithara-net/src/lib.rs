#![forbid(unsafe_code)]

pub mod base;
pub mod builder;
pub mod error;
pub mod retry;
pub mod timeout;
pub mod traits;
pub mod types;

use bytes::Bytes;
use futures::Stream;
use std::collections::HashMap;

// Re-export main types
pub use base::ReqwestNet;
pub use builder::{NetBuilder, create_default_client};
pub use error::{NetError, NetResult};
pub use retry::{DefaultRetryClassifier, DefaultRetryPolicy, RetryNet, RetryPolicyTrait};
pub use timeout::TimeoutNet;
pub use traits::{Net, NetExt};
pub use types::{Headers, NetOptions, RangeSpec, RetryPolicy};
pub type ByteStream = crate::traits::ByteStream;

// Legacy NetClient for backward compatibility
#[derive(Clone, Debug)]
pub struct NetClient {
    inner: ReqwestNet,
}

impl NetClient {
    pub fn new(opts: NetOptions) -> NetResult<Self> {
        let _retry_policy = RetryPolicy::new(
            opts.max_retries,
            opts.retry_base_delay,
            opts.max_retry_delay,
        );

        // For simplicity in legacy client, just use the base client without decorators
        // Users should migrate to NetBuilder for full functionality
        let base = ReqwestNet::with_timeout(opts.request_timeout)?;
        Ok(Self { inner: base })
    }

    pub async fn get_bytes(&self, url: url::Url) -> NetResult<Bytes> {
        self.inner.get_bytes(url).await
    }

    pub async fn stream(
        &self,
        url: url::Url,
        headers: Option<HashMap<String, String>>,
    ) -> NetResult<impl Stream<Item = NetResult<Bytes>>> {
        let headers = headers.map(Headers::from);
        let stream = self.inner.stream(url, headers).await?;

        // Convert ByteStream to concrete stream type for compatibility
        Ok(Box::pin(stream))
    }

    pub async fn get_range(
        &self,
        url: url::Url,
        range: (u64, Option<u64>),
        headers: Option<HashMap<String, String>>,
    ) -> NetResult<impl Stream<Item = NetResult<Bytes>>> {
        let range_spec = RangeSpec::new(range.0, range.1);
        let headers = headers.map(Headers::from);
        let stream = self.inner.get_range(url, range_spec, headers).await?;

        // Convert ByteStream to concrete stream type for compatibility
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::Request, http::StatusCode, response::Response, routing::get};
    use bytes::Bytes;
    use futures::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    fn test_app() -> Router {
        Router::new()
            .route("/test", get(test_endpoint))
            .route("/range", get(range_endpoint))
            .route("/headers", get(headers_endpoint))
            .route("/error404", get(error_404_endpoint))
            .route("/error500", get(error_500_endpoint))
            .route("/error429", get(error_429_endpoint))
    }

    async fn test_endpoint() -> &'static str {
        "Hello, World!"
    }

    async fn range_endpoint(request: Request) -> Result<Response, StatusCode> {
        let headers = request.headers();
        let range_header = headers.get("Range").and_then(|h| h.to_str().ok());

        let data = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

        if let Some(range) = range_header {
            if let Some(range_str) = range.strip_prefix("bytes=") {
                if let Some((start_str, end_str)) = range_str.split_once('-') {
                    let start: usize = start_str.parse().unwrap_or(0);
                    let end = if end_str.is_empty() {
                        data.len() - 1
                    } else {
                        end_str.parse().unwrap_or(data.len() - 1)
                    };

                    if start < data.len() && end < data.len() && start <= end {
                        let slice = &data[start..=end];
                        return Ok(Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", start, end, data.len()),
                            )
                            .body(axum::body::Body::from(Bytes::copy_from_slice(slice)))
                            .unwrap());
                    }
                }
            }
            Err(StatusCode::BAD_REQUEST)
        } else {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(axum::body::Body::from(Bytes::copy_from_slice(data)))
                .unwrap())
        }
    }

    async fn headers_endpoint(request: Request) -> Result<Response, StatusCode> {
        let headers = request.headers();
        let auth_header = headers.get("Authorization").and_then(|h| h.to_str().ok());
        let x_custom = headers.get("X-Custom").and_then(|h| h.to_str().ok());

        let mut response = "Headers received:".to_string();
        if let Some(auth) = auth_header {
            response.push_str(&format!(" Authorization={}", auth));
        }
        if let Some(custom) = x_custom {
            response.push_str(&format!(" X-Custom={}", custom));
        }

        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(axum::body::Body::from(response))
            .unwrap())
    }

    async fn error_404_endpoint() -> Result<Response, StatusCode> {
        Err(StatusCode::NOT_FOUND)
    }

    async fn error_500_endpoint() -> Result<Response, StatusCode> {
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }

    async fn error_429_endpoint() -> Result<Response, StatusCode> {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }

    async fn run_test_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = test_app();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://127.0.0.1:{}", addr.port())
    }

    // Request counter for test assertions
    #[derive(Clone)]
    struct RequestCounter {
        counts: Arc<std::sync::RwLock<HashMap<String, usize>>>,
    }

    impl RequestCounter {
        fn new() -> Self {
            Self {
                counts: Arc::new(std::sync::RwLock::new(HashMap::new())),
            }
        }

        fn increment(&self, path: &str) {
            let mut counts = self.counts.write().unwrap();
            *counts.entry(path.to_string()).or_insert(0) += 1;
        }
    }

    async fn key_request_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counter = RequestCounter::new();

        let app = axum::Router::new()
            .route("/key", get(key_endpoint))
            .route("/key-with-auth", get(key_with_auth_endpoint))
            .route("/key-with-params", get(key_with_params_endpoint))
            .with_state(counter);

        let port = addr.port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://127.0.0.1:{}", port)
    }

    async fn key_endpoint(
        axum::extract::State(counter): axum::extract::State<RequestCounter>,
        _request: Request,
    ) -> Result<Response, StatusCode> {
        counter.increment("/key");

        // Key endpoint that returns a mock key
        let key_bytes = vec![
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ];
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::from(Bytes::from(key_bytes)))
            .unwrap())
    }

    async fn key_with_auth_endpoint(
        axum::extract::State(counter): axum::extract::State<RequestCounter>,
        request: Request,
    ) -> Result<Response, StatusCode> {
        counter.increment("/key-with-auth");

        let headers = request.headers();
        let auth_header = headers.get("Authorization").and_then(|h| h.to_str().ok());

        // Require Authorization header
        match auth_header {
            Some(token) if token == "Bearer secret-key-token" => {
                let key_bytes = vec![
                    0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xfe, 0xdc, 0xba, 0x98, 0x76,
                    0x54, 0x32, 0x10,
                ];

                // Check for range header
                let range_header = headers.get("Range").and_then(|h| h.to_str().ok());

                if let Some(range) = range_header {
                    if let Some(range_str) = range.strip_prefix("bytes=") {
                        if let Some((start_str, end_str)) = range_str.split_once('-') {
                            let start: usize = start_str.parse().unwrap_or(0);
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
                        }
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
            _ => {
                // Missing or invalid auth header - fail as expected
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }

    async fn key_with_params_endpoint(
        axum::extract::State(counter): axum::extract::State<RequestCounter>,
        request: Request,
    ) -> Result<Response, StatusCode> {
        counter.increment("/key-with-params");

        let uri = request.uri();
        let query_params = uri.query().unwrap_or("");

        // Require specific query parameters
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
            // Missing required query parameters - fail as expected
            Err(StatusCode::BAD_REQUEST)
        }
    }

    #[tokio::test]
    async fn test_stream_get_returns_expected_bytes() {
        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let client = NetClient::new(NetOptions::default()).unwrap();
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
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/range", server_url).parse().unwrap();

        let mut stream = client.get_range(url, (5, Some(9)), None).await.unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        assert_eq!(collected, b"56789");
    }

    #[tokio::test]
    async fn test_headers_are_sent_correctly() {
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/headers", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        headers.insert("X-Custom".to_string(), "custom-value".to_string());

        let mut stream = client.stream(url, Some(headers)).await.unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        let response = String::from_utf8(collected).unwrap();
        assert!(response.contains("Authorization=Bearer token123"));
        assert!(response.contains("X-Custom=custom-value"));
    }

    #[tokio::test]
    async fn test_get_bytes_simple() {
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/test", server_url).parse().unwrap();

        let bytes = client.get_bytes(url).await.unwrap();
        assert_eq!(bytes, Bytes::from("Hello, World!"));
    }

    #[tokio::test]
    async fn test_http_error_404_returns_error() {
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/error404", server_url).parse().unwrap();

        let result = client.get_bytes(url).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetError::HttpStatus { status, .. } => assert_eq!(status, 404),
            _ => panic!("Expected HttpStatus error"),
        }
    }

    #[tokio::test]
    async fn test_http_error_500_returns_error() {
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/error500", server_url).parse().unwrap();

        let result = client.get_bytes(url).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetError::HttpStatus { status, .. } => assert_eq!(status, 500),
            _ => panic!("Expected HttpStatus error"),
        }
    }

    #[tokio::test]
    async fn test_timeout_behavior() {
        let server_url = run_test_server().await;
        let base = ReqwestNet::new().unwrap();
        let timeout_duration = std::time::Duration::from_millis(10);
        let timeout_client = base.with_timeout(timeout_duration);

        let url = format!("{}/test", server_url).parse().unwrap();

        // This should succeed quickly since the server is fast
        let result = timeout_client.get_bytes(url).await;
        assert!(result.is_ok(), "Request should succeed within timeout");
    }

    #[tokio::test]
    async fn test_retry_policy_exponential_backoff() {
        let policy = RetryPolicy::new(3, Duration::from_millis(10), Duration::from_millis(100));

        // Test exponential backoff calculation
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(0));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(10));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(20));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(40));

        // Test cap at max_delay
        assert_eq!(policy.delay_for_attempt(10), Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_net_builder_creates_functional_client() {
        let client = create_default_client().unwrap();
        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let result = client.get_bytes(url).await;
        assert!(
            result.is_ok(),
            "NetBuilder client should work like regular client"
        );
    }

    #[tokio::test]
    async fn test_net_builder_with_custom_options() {
        let retry_policy =
            RetryPolicy::new(2, Duration::from_millis(50), Duration::from_millis(200));
        let client = NetBuilder::new()
            .with_request_timeout(Duration::from_millis(100))
            .with_retry_policy(retry_policy)
            .build()
            .unwrap();

        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let result = client.get_bytes(url).await;
        assert!(
            result.is_ok(),
            "NetBuilder client with custom options should work"
        );
    }

    #[tokio::test]
    async fn test_key_request_headers_passthrough() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer secret-key-token".to_string(),
        );

        let mut stream = client.stream(url, Some(headers)).await.unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        assert_eq!(collected.len(), 16);
        assert_eq!(collected[0], 0xab); // Verify we got the authenticated key
    }

    #[tokio::test]
    async fn test_key_request_missing_required_headers_fails() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        // Request without required Authorization header should fail during stream creation
        let stream_result = client.stream(url, None).await;
        assert!(
            stream_result.is_err(),
            "Stream creation should fail when auth header is missing"
        );

        match stream_result {
            Ok(_) => panic!("Expected error but got success"),
            Err(NetError::HttpStatus { status, .. }) => assert_eq!(status, 401),
            Err(_) => panic!("Expected HttpStatus error with 401 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_wrong_auth_header_fails() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer wrong-token".to_string(),
        );

        let stream_result = client.stream(url, Some(headers)).await;
        assert!(
            stream_result.is_err(),
            "Stream creation should fail when auth header is wrong"
        );

        match stream_result {
            Ok(_) => panic!("Expected error but got success"),
            Err(NetError::HttpStatus { status, .. }) => assert_eq!(status, 401),
            Err(_) => panic!("Expected HttpStatus error with 401 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_query_params_passthrough() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!(
            "{}/key-with-params?drm_id=test123&version=1.0&extra=ignored",
            server_url
        )
        .parse()
        .unwrap();

        let mut stream = client.stream(url, None).await.unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        assert_eq!(collected.len(), 16);
        assert_eq!(collected[0], 0xfe); // Verify we got param-authenticated key
    }

    #[tokio::test]
    async fn test_key_request_missing_required_query_params_fails() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-params?drm_id=test123", server_url)
            .parse()
            .unwrap(); // Missing version

        let stream_result = client.stream(url, None).await;
        assert!(
            stream_result.is_err(),
            "Stream creation should fail when query params are missing"
        );

        match stream_result {
            Ok(_) => panic!("Expected error but got success"),
            Err(NetError::HttpStatus { status, .. }) => assert_eq!(status, 400),
            Err(_) => panic!("Expected HttpStatus error with 400 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_wrong_query_params_fails() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-params?drm_id=wrong&version=1.0", server_url)
            .parse()
            .unwrap();

        let stream_result = client.stream(url, None).await;
        assert!(
            stream_result.is_err(),
            "Stream creation should fail when query params are wrong"
        );

        match stream_result {
            Ok(_) => panic!("Expected error but got success"),
            Err(NetError::HttpStatus { status, .. }) => assert_eq!(status, 400),
            Err(_) => panic!("Expected HttpStatus error with 400 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_stream_with_headers() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer secret-key-token".to_string(),
        );

        let mut stream = client.stream(url, Some(headers)).await.unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        assert_eq!(collected.len(), 16);
        assert_eq!(collected[0], 0xab); // Verify we got the authenticated key
    }

    #[tokio::test]
    async fn test_key_request_range_with_headers() {
        let server_url = key_request_server().await;
        let client = NetClient::new(NetOptions::default()).unwrap();
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer secret-key-token".to_string(),
        );

        let mut stream = client
            .get_range(url, (0, Some(7)), Some(headers))
            .await
            .unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        assert_eq!(collected.len(), 8); // Got first 8 bytes
        assert_eq!(collected[0], 0xab);
    }
}
