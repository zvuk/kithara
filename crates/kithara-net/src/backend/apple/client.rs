use std::{num::NonZeroU16, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_platform::CancelToken;
use url::Url;

use super::{
    request::{AppleRequest, Method},
    response::AppleDataResponse,
    session::AppleSession,
};
use crate::{
    ByteStream,
    error::{NetError, NetResult},
    retry::{DefaultRetryPolicy, RetryNet},
    traits::{Net, NetExt},
    types::{Headers, NetOptions, RangeSpec},
};

#[derive(Clone)]
struct RawAppleNet {
    session: AppleSession,
    cancel: CancelToken,
}

impl RawAppleNet {
    async fn body_stream(
        &self,
        url: Url,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<ByteStream, NetError> {
        let response = {
            let request = AppleRequest::new(&url, Method::Get, range, headers)?;
            self.session.stream(request, self.cancel.clone())
        }
        .await?;
        if let Err(error) = check_status(url, response.status, &Bytes::new(), accept_partial) {
            response.cancel();
            return Err(error);
        }
        Ok(response.into())
    }

    async fn data(
        &self,
        method: Method,
        url: Url,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<AppleDataResponse, NetError> {
        let response = {
            let request = AppleRequest::new(&url, method, range, headers)?;
            self.session.data(request, self.cancel.clone())
        }
        .await?;
        check_status(url, response.status, &response.body, accept_partial)?;
        Ok(response)
    }
}

#[derive(Clone)]
pub struct AppleNet {
    net: Arc<RetryNet<RawAppleNet, DefaultRetryPolicy>>,
    options: NetOptions,
}

impl AppleNet {
    #[must_use]
    pub fn new(options: NetOptions, cancel: CancelToken) -> Self {
        let raw = RawAppleNet {
            session: AppleSession::new(&options),
            cancel: cancel.clone(),
        };
        let net = Arc::new(raw.with_retry(options.retry_policy.clone(), cancel));
        Self { net, options }
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, timeout, cancellation, or network error.
    pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes> {
        self.net.get_bytes(url, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
    pub async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> NetResult<ByteStream> {
        self.net.get_range(url, range, headers).await
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
    pub async fn head(&self, url: Url, headers: Option<Headers>) -> NetResult<Headers> {
        self.net.head(url, headers).await
    }

    #[must_use]
    pub fn options(&self) -> &NetOptions {
        &self.options
    }

    /// # Errors
    ///
    /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
    pub async fn stream(&self, url: Url, headers: Option<Headers>) -> NetResult<ByteStream> {
        self.net.stream(url, headers).await
    }
}

impl std::fmt::Debug for AppleNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppleNet")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Net for AppleNet {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.net.get_bytes(url, headers).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        self.net.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.net.head(url, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.net.stream(url, headers).await
    }
}

#[async_trait]
impl Net for RawAppleNet {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.data(Method::Get, url, None, headers, false)
            .await
            .map(|response| response.body)
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        self.body_stream(url, Some(range), headers, true).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.data(Method::Head, url, None, headers, true)
            .await
            .map(|response| normalize_head_headers(response.headers))
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.body_stream(url, None, headers, false).await
    }
}

fn check_status(
    url: Url,
    status: Option<u16>,
    body: &Bytes,
    accept_partial: bool,
) -> Result<(), NetError> {
    const HTTP_PARTIAL_CONTENT: u16 = 206;

    let Some(status) = status else {
        return Err(NetError::Network(format!(
            "NSURLSession returned a non-HTTP response for {url}"
        )));
    };
    let ok = (200..300).contains(&status) || (accept_partial && status == HTTP_PARTIAL_CONTENT);
    if ok {
        return Ok(());
    }
    status_error(url, status, body)
}

fn status_error(url: Url, status: u16, body: &Bytes) -> Result<(), NetError> {
    let body = if body.is_empty() {
        None
    } else {
        Some(truncate_error_body(
            String::from_utf8_lossy(body).into_owned(),
        ))
    };
    match NonZeroU16::new(status) {
        Some(status) => Err(NetError::Status {
            status,
            body,
            url: Some(url),
        }),
        None => Err(NetError::Network(format!(
            "unexpected zero HTTP status for {url}"
        ))),
    }
}

fn normalize_head_headers(mut headers: Headers) -> Headers {
    if headers.get("content-length").is_none()
        && let Some(total) = content_length_from_range(&headers)
    {
        headers.insert("content-length", total);
    }
    headers
}

fn content_length_from_range(headers: &Headers) -> Option<String> {
    headers
        .get("content-range")
        .and_then(|header| header.split('/').nth(1))
        .filter(|total| *total != "*")
        .map(str::to_owned)
}

fn truncate_error_body(mut body: String) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 200;

    let total = body.chars().count();
    if total <= MAX_ERROR_BODY_CHARS {
        return body;
    }
    let cut_at = body
        .char_indices()
        .nth(MAX_ERROR_BODY_CHARS)
        .map_or(body.len(), |(index, _)| index);
    body.truncate(cut_at);
    body.push_str(&format!("...(truncated, {total} chars total)"));
    body
}
