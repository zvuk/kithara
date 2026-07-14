use std::{fmt::Write, num::NonZeroU16};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Instant, timeout},
};
use url::Url;

use super::{
    request::{AppleRequest, Method},
    response::{AppleDataResponse, HTTP_PARTIAL_CONTENT},
    session::AppleSession,
};
use crate::{
    ByteStream,
    error::{NetError, NetResult},
    metrics::ConnectionMetrics,
    observe::Observer,
    resumable::{Refetch, Resumed, resumable_body},
    retry::{DefaultRetryPolicy, RetryNet},
    traits::{Net, NetExt},
    types::{Headers, NetOptions, RangeSpec},
};

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

#[derive(Clone)]
struct RawAppleNet {
    session: AppleSession,
    cancel: CancelToken,
    options: NetOptions,
}

impl RawAppleNet {
    async fn body_stream(
        &self,
        url: Url,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<ByteStream, NetError> {
        let base_start = range.as_ref().map_or(0, |range| range.start);
        let end = range.as_ref().and_then(|range| range.end);
        let first = self
            .raw_body(url.clone(), range, headers.clone(), accept_partial)
            .await?;
        Ok(self.wrap_resumable(first, url, base_start, end, headers))
    }

    #[kithara::flash(io)]
    async fn data(
        &self,
        method: Method,
        url: Url,
        body: Option<Bytes>,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<AppleDataResponse, NetError> {
        let started = Instant::now();
        let response = {
            let request = AppleRequest::new(&url, method, range, headers, body)?;
            timeout(
                self.options.inactivity_timeout,
                self.session.data(request, self.cancel.clone()),
            )
        }
        .await
        .map_err(|_| NetError::Timeout)??;
        if let Some(observer) = self.options.observer.as_ref() {
            observer.0.first_byte(
                started.elapsed(),
                response.status,
                response.status == HTTP_PARTIAL_CONTENT,
            );
        }
        check_status(url, response.status, &response.body, accept_partial)?;
        Ok(response)
    }

    #[kithara::flash(io)]
    async fn raw_body(
        &self,
        url: Url,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<ByteStream, NetError> {
        let started = Instant::now();
        let response = {
            let request = AppleRequest::new(&url, Method::Get, range, headers, None)?;
            timeout(
                self.options.inactivity_timeout,
                self.session.stream(request, self.cancel.clone()),
            )
        }
        .await
        .map_err(|_| NetError::Timeout)??;
        if let Some(observer) = self.options.observer.as_ref() {
            observer.0.first_byte(
                started.elapsed(),
                response.status,
                response.status == HTTP_PARTIAL_CONTENT,
            );
        }
        if let Err(error) = check_status(url, response.status, &Bytes::new(), accept_partial) {
            response.cancel();
            return Err(error);
        }
        Ok(response.into())
    }

    fn wrap_resumable(
        &self,
        first: ByteStream,
        url: Url,
        base_start: u64,
        end: Option<u64>,
        headers: Option<Headers>,
    ) -> ByteStream {
        let out_headers = first.headers.clone();
        let partial = first.is_partial();
        let me = self.clone();
        let refetch: Refetch = Box::new(move |consumed| {
            let me = me.clone();
            let url = url.clone();
            let headers = headers.clone();
            let abs = base_start.saturating_add(consumed);
            let resume = RangeSpec::new(abs, end);
            Box::pin(async move {
                let stream = me.raw_body(url, Some(resume), headers, true).await?;
                let skip = if stream.is_partial() { 0 } else { abs };
                Ok(Resumed { stream, skip })
            })
        });
        let body = resumable_body(
            first,
            refetch,
            self.options.inactivity_timeout,
            self.options.retry_policy.clone(),
            self.cancel.clone(),
            self.options.observer.clone(),
        );
        ByteStream::with_partial(out_headers, body, partial)
    }
}

#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AppleNet {
    cancel: CancelToken,
    session: AppleSession,
    net: Arc<RetryNet<RawAppleNet, DefaultRetryPolicy>>,
    connection_metrics: ConnectionMetrics,
    #[field(get)]
    options: NetOptions,
}

impl AppleNet {
    #[must_use]
    pub fn new(options: NetOptions, cancel: CancelToken) -> Self {
        let connection_metrics = ConnectionMetrics::default();
        let session = AppleSession::new(&options, connection_metrics.clone());
        let raw = RawAppleNet {
            session: session.clone(),
            cancel: cancel.clone(),
            options: options.clone(),
        };
        let net = Arc::new(RetryNet::new(
            raw,
            DefaultRetryPolicy::new(options.retry_policy.clone()),
            cancel.clone(),
            options.observer.clone(),
        ));
        Self {
            cancel,
            session,
            net,
            connection_metrics,
            options,
        }
    }

    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connection_metrics.connection_count()
    }

    delegate::delegate! {
        to self.net {
            /// # Errors
            ///
            /// Returns [`NetError`] on HTTP failure, timeout, cancellation, or network error.
            pub async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> NetResult<Bytes>;
            /// # Errors
            ///
            /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
            pub async fn get_range(
                &self,
                url: Url,
                range: RangeSpec,
                headers: Option<Headers>,
            ) -> NetResult<ByteStream>;
            /// # Errors
            ///
            /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
            pub async fn head(&self, url: Url, headers: Option<Headers>) -> NetResult<Headers>;
            /// # Errors
            ///
            /// Returns [`NetError`] on HTTP failure, timeout, cancellation, or network error.
            pub async fn post_bytes(
                &self,
                url: Url,
                body: Bytes,
                headers: Option<Headers>,
            ) -> NetResult<Bytes>;
            /// # Errors
            ///
            /// Returns [`NetError`] on HTTP failure, cancellation, or network error.
            pub async fn stream(&self, url: Url, headers: Option<Headers>) -> NetResult<ByteStream>;
        }
    }

    #[must_use]
    pub fn with_observer(&self, observer: Option<Observer>) -> Self {
        let options = NetOptions::builder()
            .compression(self.options.compression)
            .inactivity_timeout(self.options.inactivity_timeout)
            .impersonate(self.options.impersonate)
            .byte_pool(self.options.byte_pool.clone())
            .retry_policy(self.options.retry_policy.clone())
            .is_insecure(self.options.is_insecure)
            .body_queue_capacity(self.options.body_queue_capacity)
            .body_queue_resume_at(self.options.body_queue_resume_at)
            .pool_max_idle_per_host(self.options.pool_max_idle_per_host)
            .maybe_observer(observer)
            .build();
        let raw = RawAppleNet {
            session: self.session.clone(),
            cancel: self.cancel.clone(),
            options: options.clone(),
        };
        let net = Arc::new(RetryNet::new(
            raw,
            DefaultRetryPolicy::new(options.retry_policy.clone()),
            self.cancel.clone(),
            options.observer.clone(),
        ));
        Self {
            cancel: self.cancel.clone(),
            session: self.session.clone(),
            net,
            connection_metrics: self.connection_metrics.clone(),
            options,
        }
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

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        self.net.post_bytes(url, body, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.net.stream(url, headers).await
    }
}

#[async_trait]
impl Net for RawAppleNet {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.data(Method::Get, url, None, None, headers, false)
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
        self.data(Method::Head, url, None, None, headers, true)
            .await
            .map(|response| normalize_head_headers(response.headers))
    }

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        self.data(Method::Post, url, Some(body), None, headers, false)
            .await
            .map(|response| response.body)
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
    let _ = write!(body, "...(truncated, {total} chars total)");
    body
}
