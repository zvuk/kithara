#![forbid(unsafe_code)]

pub mod base;
pub mod builder;
pub mod retry;
pub mod timeout;
pub mod traits;
pub mod types;

use bytes::Bytes;
use futures::Stream;
use std::collections::HashMap;
use std::time::Duration;

// Re-export main types
pub use base::ReqwestNet;
pub use builder::{create_default_client, NetBuilder};
pub use retry::{DefaultRetryClassifier, DefaultRetryPolicy, RetryNet, RetryPolicyTrait};
pub use timeout::TimeoutNet;
pub use traits::{Net, NetExt};
pub use types::{Headers, NetOptions, RangeSpec, RetryPolicy};

// Re-export error type from base
pub use base::NetError;

pub type NetResult<T> = Result<T, NetError>;
pub type ByteStream = crate::traits::ByteStream;

// Legacy NetClient for backward compatibility
#[derive(Clone)]
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

    #[tokio::test]
    async fn test_stream_get_returns_expected_bytes() {
        let server_url = run_test_server().await;
        let client = NetClient::new(NetOptions::default());
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
            NetError::HttpStatus { status, .. } =>         assert_eq!(status, 500),
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
        assert!(result.is_ok(), "NetBuilder client should work like regular client");
    }

    #[tokio::test]
    async fn test_net_builder_with_custom_options() {
        let retry_policy = RetryPolicy::new(2, Duration::from_millis(50), Duration::from_millis(200));
        let client = NetBuilder::new()
            .with_request_timeout(Duration::from_millis(100))
            .with_retry_policy(retry_policy)
            .build()
            .unwrap();
        
        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let result = client.get_bytes(url).await;
        assert!(result.is_ok(), "NetBuilder client with custom options should work");
    }
}
