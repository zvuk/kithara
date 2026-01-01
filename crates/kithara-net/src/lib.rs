#![forbid(unsafe_code)]

use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Invalid range header: {0}")]
    InvalidRange(String),
    #[error("Timeout")]
    Timeout,
    #[error("not implemented")]
    Unimplemented,
}

pub type NetResult<T> = Result<T, NetError>;

#[derive(Clone, Debug)]
pub struct NetOptions {
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetClient {
    client: reqwest::Client,
    #[allow(dead_code)]
    opts: NetOptions,
}

impl NetClient {
    pub fn new(opts: NetOptions) -> Self {
        let client = reqwest::Client::builder()
            .timeout(opts.request_timeout)
            .use_rustls_tls()
            .build()
            .expect("Failed to create HTTP client");

        Self { client, opts }
    }

    pub async fn get_bytes(&self, url: Url) -> NetResult<Bytes> {
        let response = self.client.get(url).send().await?;
        Ok(response.bytes().await?)
    }

    pub async fn stream(
        &self,
        url: Url,
        headers: Option<HashMap<String, String>>,
    ) -> NetResult<impl Stream<Item = NetResult<Bytes>>> {
        let mut request = self.client.get(url);

        if let Some(headers) = headers {
            for (key, value) in headers {
                request = request.header(&key, value);
            }
        }

        let response = request.send().await?;
        Ok(response
            .bytes_stream()
            .map(|result| result.map_err(NetError::Http)))
    }

    pub async fn get_range(
        &self,
        url: Url,
        range: (u64, Option<u64>),
        headers: Option<HashMap<String, String>>,
    ) -> NetResult<impl Stream<Item = NetResult<Bytes>>> {
        let mut request = self.client.get(url);

        let range_header = if let Some(end) = range.1 {
            format!("bytes={}-{}", range.0, end)
        } else {
            format!("bytes={}-", range.0)
        };
        request = request.header("Range", range_header);

        if let Some(headers) = headers {
            for (key, value) in headers {
                request = request.header(&key, value);
            }
        }

        let response = request.send().await?;
        Ok(response
            .bytes_stream()
            .map(|result| result.map_err(NetError::Http)))
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
        let client = NetClient::new(NetOptions::default());
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
        let client = NetClient::new(NetOptions::default());
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
        let client = NetClient::new(NetOptions::default());
        let url = format!("{}/test", server_url).parse().unwrap();

        let bytes = client.get_bytes(url).await.unwrap();
        assert_eq!(bytes, Bytes::from("Hello, World!"));
    }
}
