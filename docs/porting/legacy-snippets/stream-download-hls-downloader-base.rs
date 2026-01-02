//! Base HTTP downloader implementation using HttpStream.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::stream::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use stream_download::http::HttpStream;
use stream_download::http::reqwest::Client as ReqwestClient;
use stream_download::http::reqwest::Url;
use stream_download::source::{DecodeError, SourceStream, StreamMsg};

use crate::error::{HlsError, HlsResult};

use super::traits::{ByteStream, Downloader, Headers};
use super::types::Resource;

/// Base HTTP downloader using HttpStream.
///
/// This is the lowest-level downloader that directly uses HttpStream
/// from the stream-download crate.
#[derive(Debug, Clone)]
pub struct HttpDownloader {
    /// Request timeout for individual operations.
    request_timeout: Duration,
    /// Cancellation token for all operations.
    cancel: CancellationToken,
    /// Optional headers for key requests.
    key_request_headers: Option<HashMap<String, String>>,
}

impl HttpDownloader {
    /// Create a new HttpDownloader.
    pub fn new(
        request_timeout: Duration,
        cancel: CancellationToken,
        key_request_headers: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            request_timeout,
            cancel,
            key_request_headers,
        }
    }

    /// Build headers for key requests.
    fn build_key_headers(&self) -> HlsResult<HeaderMap> {
        let mut headers = HeaderMap::new();

        if let Some(h) = &self.key_request_headers {
            for (k, v) in h {
                let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                    HlsError::io_kind(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid header name `{}`: {}", k, e),
                    )
                })?;
                let value = HeaderValue::from_str(v).map_err(|e| {
                    HlsError::io_kind(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid header value for `{}`: {}", k, e),
                    )
                })?;
                headers.insert(name, value);
            }
        }

        Ok(headers)
    }

    /// Create an HttpStream for a URL.
    async fn create_stream(&self, url: &Url) -> HlsResult<HttpStream<ReqwestClient>> {
        let create_fut = timeout(
            self.request_timeout,
            HttpStream::<ReqwestClient>::create(url.clone()),
        );

        let res = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(HlsError::Cancelled),
            res = create_fut => res,
        };

        match res {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => {
                let msg = e.decode_error().await;
                Err(HlsError::http_stream_create_failed(msg))
            }
            Err(_) => Err(HlsError::timeout(url.to_string())),
        }
    }

    /// Create an HttpStream with custom headers.
    async fn create_stream_with_headers(
        &self,
        url: &Url,
        headers: HeaderMap,
    ) -> HlsResult<HttpStream<ReqwestClient>> {
        let create_fut = timeout(
            self.request_timeout,
            HttpStream::<ReqwestClient>::create_with_headers(url.clone(), headers),
        );

        let res = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(HlsError::Cancelled),
            res = create_fut => res,
        };

        match res {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => {
                let msg = e.decode_error().await;
                Err(HlsError::http_stream_create_failed(msg))
            }
            Err(_) => Err(HlsError::timeout(url.to_string())),
        }
    }

    /// Map stream errors to HlsError.
    fn map_stream_errors(&self, url: &Url, stream: HttpStream<ReqwestClient>) -> ByteStream {
        let url_str = url.to_string();
        let url: Arc<str> = Arc::from(url_str);
        stream
            .filter_map(move |res| {
                let url = Arc::clone(&url);
                async move {
                    match res {
                        Ok(StreamMsg::Data(bytes)) => Some(Ok(bytes)),
                        Ok(StreamMsg::Control(_)) => None,
                        Err(e) => Some(Err(HlsError::io(format!(
                            "stream read error (url={}): {}",
                            url, e
                        )))),
                    }
                }
            })
            .boxed()
    }

    /// Collect stream bytes into a single Bytes object.
    async fn collect_stream_to_bytes(
        &self,
        mut stream: HttpStream<ReqwestClient>,
        url: &Url,
    ) -> HlsResult<Bytes> {
        let mut buf = BytesMut::with_capacity(16 * 1024);

        loop {
            let next = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(HlsError::Cancelled),
                item = async {
                    match tokio::time::timeout(self.request_timeout, stream.next()).await {
                        Ok(v) => Ok(v),
                        Err(_) => Err(()),
                    }
                } => {
                    match item {
                        Ok(v) => v,
                        Err(()) => return Err(HlsError::timeout(url.to_string())),
                    }
                },
            };

            match next {
                Some(Ok(StreamMsg::Data(chunk))) => {
                    buf.extend_from_slice(&chunk);
                }
                Some(Ok(StreamMsg::Control(_))) => {
                    // Ignore control messages
                    continue;
                }
                Some(Err(e)) => {
                    return Err(HlsError::io(e.to_string()));
                }
                None => break,
            }
        }

        Ok(buf.freeze())
    }

    /// Parse Content-Range header to extract total length.
    fn parse_content_range_total(header_val: &str) -> Option<u64> {
        let idx = header_val.rfind('/')?;
        let total_str = header_val.get(idx + 1..)?.trim();
        if total_str == "*" {
            None
        } else {
            total_str.parse::<u64>().ok()
        }
    }
}

#[async_trait::async_trait]
impl Downloader for HttpDownloader {
    async fn download_with_headers(
        &self,
        resource: &Resource,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        let url = resource.url();

        let mut header_map = if let Some(headers) = headers {
            let mut map = HeaderMap::new();
            for (k, v) in headers {
                let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                    HlsError::io_kind(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid header name `{}`: {}", k, e),
                    )
                })?;
                let value = HeaderValue::from_str(&v).map_err(|e| {
                    HlsError::io_kind(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid header value for `{}`: {}", k, e),
                    )
                })?;
                map.insert(name, value);
            }
            map
        } else {
            HeaderMap::new()
        };

        // Merge with key request headers if this is a key request
        // (we can't know for sure, but if caller didn't provide headers,
        // we use the configured key headers)
        if header_map.is_empty() {
            if let Ok(key_headers) = self.build_key_headers() {
                header_map = key_headers;
            }
        }

        let http = self.create_stream_with_headers(url, header_map).await?;
        self.collect_stream_to_bytes(http, url).await
    }

    async fn stream(&self, resource: &Resource) -> HlsResult<ByteStream> {
        let url = resource.url();
        let http = self.create_stream(url).await?;
        Ok(self.map_stream_errors(url, http))
    }

    async fn stream_range(
        &self,
        resource: &Resource,
        start: u64,
        end: Option<u64>,
    ) -> HlsResult<ByteStream> {
        let url = resource.url();
        let mut http = self.create_stream(url).await?;

        http.seek_range(start, end)
            .await
            .map_err(|e| HlsError::Io(e))?;

        Ok(self.map_stream_errors(url, http))
    }

    async fn probe_content_length(&self, resource: &Resource) -> HlsResult<Option<u64>> {
        let url = resource.url();

        // Try a 0-0 range request first
        let client = <ReqwestClient as stream_download::http::Client>::create();
        let range_fut =
            <ReqwestClient as stream_download::http::Client>::get_range(&client, url, 0, Some(0));

        let response_res = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(HlsError::Cancelled),
            res = tokio::time::timeout(self.request_timeout, range_fut) => res,
        };

        match response_res {
            Ok(Ok(response)) => {
                let status = response.status();
                if !status.is_success() {
                    return Err(HlsError::HttpError {
                        status: status.as_u16(),
                        url: url.to_string(),
                    });
                }

                let headers = response.headers();

                // Prefer Content-Range for total length
                if let Some(cr) = headers.get("Content-Range").and_then(|v| v.to_str().ok()) {
                    if let Some(total) = Self::parse_content_range_total(cr) {
                        return Ok(Some(total));
                    }
                }

                // Fallback: Content-Length in 200 OK response
                if let Some(cl) = headers
                    .get("Content-Length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if status.as_u16() == 200 {
                        return Ok(Some(cl));
                    }
                }

                // Last resort: create HttpStream and read metadata
                let create_fut = tokio::time::timeout(
                    self.request_timeout,
                    HttpStream::<ReqwestClient>::create(url.clone()),
                );

                let create_res = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => return Err(HlsError::Cancelled),
                    res = create_fut => res,
                };

                let http = match create_res {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(e)) => {
                        let msg = e.decode_error().await;
                        return Err(HlsError::http_stream_create_failed_during_probe(msg));
                    }
                    Err(_) => return Err(HlsError::timeout(url.to_string())),
                };

                let cl_opt: Option<u64> = http.content_length().into();
                Ok(cl_opt)
            }
            Ok(Err(e)) => Err(HlsError::io(e.to_string())),
            Err(_) => Err(HlsError::timeout(url.to_string())),
        }
    }

    fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }
}
