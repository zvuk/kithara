use async_trait::async_trait;
use bytes::Bytes;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Instant, timeout},
};
use url::Url;

use super::{
    exchange::{Exchange, Opened, Target},
    protocol::{HTTP_PARTIAL_CONTENT, check_status},
    transport::HostMethod,
};
use crate::{
    ByteStream,
    backend::{common::normalize_head_headers, pooled::ByteBuffers},
    error::{NetError, NetResult},
    observe::Observer,
    range_response::validate_range_response,
    resumable::{Refetch, Resumed, resumable_body},
    retry::RetryNet,
    traits::Net,
    types::{AcceptEncodingPolicy, Headers, NetOptions, RangeSpec},
};

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

struct HostDataResponse {
    body: Bytes,
    headers: Headers,
    status: u16,
}

#[derive(Clone)]
struct RawHostNet {
    exchange: Exchange,
    options: NetOptions,
}

impl RawHostNet {
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
            .raw_body(&url, range, headers.as_ref(), accept_partial)
            .await?;
        Ok(self.wrap_resumable(first, url, base_start, end, headers))
    }

    #[kithara::flash(io)]
    async fn data(
        &self,
        method: HostMethod,
        url: Url,
        body: Option<Bytes>,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        accept_partial: bool,
    ) -> Result<HostDataResponse, NetError> {
        let started = Instant::now();
        let response = timeout(self.options.inactivity_timeout, async {
            let Opened {
                call,
                headers,
                status,
            } = self
                .exchange
                .open(Target {
                    method,
                    accept_encoding: AcceptEncodingPolicy::from(method),
                    url: &url,
                    range: range.as_ref(),
                    headers: headers.as_ref(),
                    body,
                })
                .await?;
            let body = self.exchange.drain(call).await?;
            Ok::<_, NetError>(HostDataResponse {
                body,
                headers,
                status,
            })
        })
        .await
        .map_err(|_| NetError::Timeout)??;

        self.observe_first_byte(started, response.status);
        check_status(&url, response.status, &response.body, accept_partial)?;
        Ok(response)
    }

    #[kithara::flash(io)]
    async fn raw_body(
        &self,
        url: &Url,
        range: Option<RangeSpec>,
        headers: Option<&Headers>,
        accept_partial: bool,
    ) -> Result<ByteStream, NetError> {
        let started = Instant::now();
        let opened = timeout(
            self.options.inactivity_timeout,
            self.exchange.open(Target {
                method: HostMethod::Get,
                accept_encoding: AcceptEncodingPolicy::Identity,
                url,
                range: range.as_ref(),
                headers,
                body: None,
            }),
        )
        .await
        .map_err(|_| NetError::Timeout)??;

        self.observe_first_byte(started, opened.status);
        let status = check_status(url, opened.status, &Bytes::new(), accept_partial)?;
        validate_range_response(status, range.as_ref(), &opened.headers, url)?;

        let partial = status == HTTP_PARTIAL_CONTENT;
        let Opened { call, headers, .. } = opened;
        let body = self.exchange.body(call);
        Ok(ByteStream::with_partial(headers, Box::pin(body), partial))
    }

    fn observe_first_byte(&self, started: Instant, status: u16) {
        if let Some(observer) = self.options.observer.as_ref() {
            observer
                .0
                .first_byte(started.elapsed(), status, status == HTTP_PARTIAL_CONTENT);
        }
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
        let resource = url.clone();
        let (resume_base, resume_end) = if partial {
            (base_start, end)
        } else {
            (0, None)
        };
        let refetch: Refetch = Box::new(move |consumed| {
            let me = me.clone();
            let url = url.clone();
            let headers = headers.clone();
            let abs = resume_base.saturating_add(consumed);
            let resume = RangeSpec::new(abs, resume_end);
            Box::pin(async move {
                let stream = me
                    .raw_body(&url, Some(resume), headers.as_ref(), true)
                    .await?;
                let skip = if stream.is_partial() { 0 } else { abs };
                Ok(Resumed { stream, skip })
            })
        });
        let body = resumable_body(
            first,
            refetch,
            resource,
            self.options.inactivity_timeout,
            self.options.retry_policy,
            self.exchange.cancel.clone(),
            self.options.observer.clone(),
        );
        ByteStream::with_partial(out_headers, body, partial)
    }
}

/// Every request, through the transport installed in the process.
#[derive(Clone)]
pub struct HostNet {
    net: Arc<RetryNet<RawHostNet>>,
    raw: RawHostNet,
}

impl HostNet {
    #[must_use]
    pub fn new<S>(options: NetOptions, pools: PoolRegion<S>, cancel: CancelToken) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        Self::from(RawHostNet {
            exchange: Exchange {
                buffers: ByteBuffers::new(pools),
                cancel,
            },
            options,
        })
    }

    #[must_use]
    pub fn with_observer(&self, observer: Option<Observer>) -> Self {
        Self::from(RawHostNet {
            options: self.raw.options.with_observer(observer),
            exchange: self.raw.exchange.clone(),
        })
    }

    #[must_use]
    pub fn options(&self) -> &NetOptions {
        &self.raw.options
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
}

impl From<RawHostNet> for HostNet {
    fn from(raw: RawHostNet) -> Self {
        let net = Arc::new(RetryNet::new(
            raw.clone(),
            raw.options.retry_policy,
            raw.exchange.cancel.clone(),
            raw.options.observer.clone(),
        ));
        Self { net, raw }
    }
}

impl std::fmt::Debug for HostNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostNet")
            .field("options", &self.raw.options)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Net for HostNet {
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
impl Net for RawHostNet {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.data(HostMethod::Get, url, None, None, headers, false)
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
        self.data(HostMethod::Head, url, None, None, headers, true)
            .await
            .map(|response| normalize_head_headers(response.headers))
    }

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        self.data(HostMethod::Post, url, Some(body), None, headers, false)
            .await
            .map(|response| response.body)
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.body_stream(url, None, headers, false).await
    }
}
