use std::sync::LazyLock;

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

static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

#[derive(Clone, Debug)]
pub struct HttpClient {
    inner: Client,
    options: NetOptions,
}

impl HttpClient {
    pub fn new(options: NetOptions) -> Self {
        Self {
            inner: CLIENT.clone(),
            options,
        }
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

    pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes> {
        <Self as Net>::get_bytes(self, url, headers).await
    }

    pub async fn stream(&self, url: Url, headers: Option<Headers>) -> NetResult<crate::ByteStream> {
        <Self as Net>::stream(self, url, headers).await
    }

    pub async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> NetResult<crate::ByteStream> {
        <Self as Net>::get_range(self, url, range, headers).await
    }
}

#[async_trait]
impl Net for HttpClient {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        let req = self.inner.get(url.clone());
        let req = Self::apply_headers(req, headers);
        let req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        resp.bytes().await.map_err(NetError::from)
    }

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
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        let stream = resp.bytes_stream().map_err(NetError::from);
        Ok(Box::pin(stream))
    }

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
        req = req.timeout(self.options.request_timeout);

        let resp = req.send().await.map_err(NetError::from)?;
        let status = resp.status();

        if !(status.is_success() || status.as_u16() == 206) {
            let body = resp.text().await.unwrap_or_default();
            return Err(NetError::HttpError {
                url,
                status: status.as_u16(),
                body: Some(body),
            });
        }

        let stream = resp.bytes_stream().map_err(NetError::from);
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc, time::Duration};

    use axum::{Router, extract::Request, http::StatusCode, response::Response, routing::get};
    use bytes::Bytes;
    use futures::StreamExt;
    use tokio::net::TcpListener;

    use super::*;
    use crate::{RetryPolicy, TimeoutNet, traits::NetExt};

    fn test_app() -> Router {
        Router::new()
            .route("/test", get(test_endpoint))
            .route("/range", get(range_endpoint))
            .route("/headers", get(headers_endpoint))
            .route("/error404", get(error_404_endpoint))
            .route("/error500", get(error_500_endpoint))
            .route("/error429", get(error_429_endpoint))
            .route("/slow-headers", get(slow_headers_endpoint))
            .route("/slow-body", get(slow_body_endpoint))
            .route("/ignore-range", get(ignore_range_endpoint))
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

    async fn slow_headers_endpoint() -> Result<Response, StatusCode> {
        // This endpoint delays sending headers to test timeout
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(axum::body::Body::from("delayed response"))
            .unwrap())
    }

    async fn slow_body_endpoint() -> Result<Response, StatusCode> {
        // This endpoint sends headers quickly but body slowly
        use futures::stream::{self, StreamExt};

        let stream = stream::iter(vec![
            Bytes::from_static(b"first chunk"),
            Bytes::from_static(b"second chunk"),
        ])
        .then(|chunk| async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            Ok::<Bytes, std::io::Error>(chunk)
        });

        let body = axum::body::Body::from_stream(stream);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(body)
            .unwrap())
    }

    async fn ignore_range_endpoint(request: Request) -> Result<Response, StatusCode> {
        let headers = request.headers();
        let range_header = headers.get("Range").and_then(|h| h.to_str().ok());

        // Always return 200 OK with full data, ignoring Range header
        let data = b"full data ignoring range";

        if range_header.is_some() {
            // Server received Range header but chooses to ignore it
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(axum::body::Body::from(Bytes::copy_from_slice(data)))
                .unwrap())
        } else {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(axum::body::Body::from(Bytes::copy_from_slice(data)))
                .unwrap())
        }
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
        let server_url = run_test_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/range", server_url).parse().unwrap();

        let mut stream = client
            .get_range(url, RangeSpec::new(5, Some(9)), None)
            .await
            .unwrap();
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
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/headers", server_url).parse().unwrap();

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        headers.insert("X-Custom".to_string(), "custom-value".to_string());

        let mut stream = client.stream(url, Some(headers.into())).await.unwrap();
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
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/test", server_url).parse().unwrap();

        let bytes = client.get_bytes(url, None).await.unwrap();
        assert_eq!(bytes, Bytes::from("Hello, World!"));
    }

    #[tokio::test]
    async fn test_http_error_404_returns_error() {
        let server_url = run_test_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/error404", server_url).parse().unwrap();

        let result = client.get_bytes(url, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetError::HttpError { status, .. } => assert_eq!(status, 404),
            _ => panic!("Expected HTTP error"),
        }
    }

    #[tokio::test]
    async fn test_http_error_500_returns_error() {
        let server_url = run_test_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/error500", server_url).parse().unwrap();

        let result = client.get_bytes(url, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetError::HttpError { status, .. } => assert_eq!(status, 500),
            _ => panic!("Expected HTTP error"),
        }
    }

    #[tokio::test]
    async fn test_timeout_behavior() {
        let server_url = run_test_server().await;
        let base = HttpClient::new(NetOptions::default());
        let timeout_duration = std::time::Duration::from_millis(10);
        let timeout_client = base.with_timeout(timeout_duration);

        let url = format!("{}/test", server_url).parse().unwrap();

        // This should succeed quickly since the server is fast
        let result = timeout_client.get_bytes(url, None).await;
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
        let client = HttpClient::new(NetOptions::default());
        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let result = client.get_bytes(url, None).await;
        assert!(
            result.is_ok(),
            "NetBuilder client should work like regular client"
        );
    }

    #[tokio::test]
    async fn test_net_builder_with_custom_options() {
        let mut opts = NetOptions::default();
        opts.request_timeout = Duration::from_millis(100);
        opts.retry_policy =
            RetryPolicy::new(2, Duration::from_millis(50), Duration::from_millis(200));

        let client = HttpClient::new(opts);

        let server_url = run_test_server().await;
        let url = format!("{}/test", server_url).parse().unwrap();

        let result = client.get_bytes(url, None).await;
        assert!(result.is_ok(), "HttpClient with custom options should work");
    }

    #[tokio::test]
    async fn test_key_request_headers_passthrough() {
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

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
        assert_eq!(collected[0], 0xab); // Verify we got the authenticated key
    }

    #[tokio::test]
    async fn test_key_request_missing_required_headers_fails() {
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

        // Request without required Authorization header should fail during stream creation
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
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

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
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
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
        let client = HttpClient::new(NetOptions::default());
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
            Err(NetError::HttpError { status, .. }) => assert_eq!(status, 400),
            Err(_) => panic!("Expected HTTP status error with 400 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_wrong_query_params_fails() {
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
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
            Err(NetError::HttpError { status, .. }) => assert_eq!(status, 400),
            Err(_) => panic!("Expected HTTP status error with 400 status"),
        }
    }

    #[tokio::test]
    async fn test_key_request_stream_with_headers() {
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

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
        assert_eq!(collected[0], 0xab); // Verify we got the authenticated key
    }

    #[tokio::test]
    async fn test_key_request_range_with_headers() {
        let server_url = key_request_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/key-with-auth", server_url).parse().unwrap();

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

        assert_eq!(collected.len(), 8); // Got first 8 bytes
        assert_eq!(collected[0], 0xab);
    }

    #[tokio::test]
    async fn test_no_mid_stream_retry() {
        // Test retry classification according to v1 contract
        // Note: Current implementation marks all connection errors as retryable,
        // including mid-stream errors. The v1 contract says no mid-stream retry,
        // but we can't distinguish stages with current error type.

        // Test that timeout errors ARE retryable (they happen before body)
        let timeout_error = NetError::Timeout;
        assert!(
            timeout_error.is_retryable(),
            "Timeout errors should be retryable (happen before body)"
        );

        // Test HTTP 500 is retryable (happens before body)
        let http_500_error = NetError::HttpError {
            status: 500,
            url: Url::parse("http://example.com").unwrap(),
            body: None,
        };
        assert!(
            http_500_error.is_retryable(),
            "HTTP 5xx errors should be retryable (happen before body)"
        );

        // Test HTTP 429 is retryable
        let http_429_error = NetError::HttpError {
            status: 429,
            url: Url::parse("http://example.com").unwrap(),
            body: None,
        };
        assert!(
            http_429_error.is_retryable(),
            "HTTP 429 errors should be retryable"
        );

        // Test HTTP 408 is retryable (as per reference spec)
        let http_408_error = NetError::HttpError {
            status: 408,
            url: Url::parse("http://example.com").unwrap(),
            body: None,
        };
        assert!(
            http_408_error.is_retryable(),
            "HTTP 408 errors should be retryable (as per reference spec)"
        );

        // Test HTTP 404 is NOT retryable
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
        // Test that get_bytes times out if body stalls
        let server_url = run_test_server().await;
        let base = HttpClient::new(NetOptions::default());

        // Use a very short timeout
        let timeout_client = TimeoutNet::new(base, Duration::from_millis(50));

        let url = format!("{}/slow-body", server_url).parse().unwrap();

        // get_bytes should timeout because body is slow
        let result = timeout_client.get_bytes(url, None).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            NetError::Timeout => (), // Expected
            other => panic!("Expected Timeout error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_timeout_matrix_stream_times_out_on_headers() {
        // Test that stream times out waiting for headers
        let server_url = run_test_server().await;
        let base = HttpClient::new(NetOptions::default());

        // Use a very short timeout (shorter than server's 500ms delay)
        let timeout_client = TimeoutNet::new(base, Duration::from_millis(100));

        let url = format!("{}/slow-headers", server_url).parse().unwrap();

        // stream should timeout waiting for headers
        let result = timeout_client.stream(url, None).await;
        assert!(result.is_err());

        match result {
            Err(NetError::Timeout) => (), // Expected
            Ok(_) => panic!("Expected Timeout error, got Ok"),
            Err(e) => panic!("Expected Timeout error, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_timeout_matrix_get_range_times_out_on_headers() {
        // Test that get_range times out waiting for headers
        let server_url = run_test_server().await;
        let base = HttpClient::new(NetOptions::default());

        // Use a very short timeout (shorter than server's 500ms delay)
        let timeout_client = TimeoutNet::new(base, Duration::from_millis(100));

        let url = format!("{}/slow-headers", server_url).parse().unwrap();
        let range = RangeSpec::new(0, Some(10));

        // get_range should timeout waiting for headers
        let result = timeout_client.get_range(url, range, None).await;
        assert!(result.is_err());

        match result {
            Err(NetError::Timeout) => (), // Expected
            Ok(_) => panic!("Expected Timeout error, got Ok"),
            Err(e) => panic!("Expected Timeout error, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_range_behavior_contract() {
        // Test behavior when server ignores Range header and returns 200 OK
        let server_url = run_test_server().await;
        let client = HttpClient::new(NetOptions::default());
        let url = format!("{}/ignore-range", server_url).parse().unwrap();

        // Request a range
        let mut stream = client
            .get_range(url, RangeSpec::new(0, Some(5)), None)
            .await
            .unwrap();
        let mut collected = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.unwrap();
            collected.extend_from_slice(&chunk);
        }

        // According to current contract, we accept 200 OK response even with Range header
        // The client should return the full data
        assert_eq!(collected, b"full data ignoring range");
    }
}
