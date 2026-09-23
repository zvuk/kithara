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
    request::{AppleRequest, Method},
    response::{AppleDataResponse, HTTP_PARTIAL_CONTENT},
    session::AppleSession,
};
use crate::{
    ByteStream,
    backend::common::{normalize_head_headers, status_error},
    error::{NetError, NetResult},
    metrics::ConnectionMetrics,
    observe::Observer,
    range_response::{accepts_response_status, validate_range_response},
    resumable::{Refetch, Resumed, resumable_body},
    retry::RetryNet,
    traits::Net,
    types::{AcceptEncodingPolicy, Headers, NetOptions, RangeSpec},
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
            let accept_encoding = match method {
                Method::Get | Method::Post => AcceptEncodingPolicy::Configured,
                Method::Head => AcceptEncodingPolicy::Identity,
            };
            let request = AppleRequest::new(&url, method, range, headers, body, accept_encoding)?;
            timeout(
                self.options.inactivity_timeout,
                self.session.data(request, self.cancel.clone()),
            )
        }
        .await
        .map_err(|_| NetError::Timeout)??;
        if let (Some(observer), Some(status)) = (self.options.observer.as_ref(), response.status) {
            observer
                .0
                .first_byte(started.elapsed(), status, status == HTTP_PARTIAL_CONTENT);
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
            let request = AppleRequest::new(
                &url,
                Method::Get,
                range.clone(),
                headers,
                None,
                AcceptEncodingPolicy::Identity,
            )?;
            timeout(
                self.options.inactivity_timeout,
                self.session.stream(request, self.cancel.clone()),
            )
        }
        .await
        .map_err(|_| NetError::Timeout)??;
        if let (Some(observer), Some(status)) = (self.options.observer.as_ref(), response.status) {
            observer
                .0
                .first_byte(started.elapsed(), status, status == HTTP_PARTIAL_CONTENT);
        }
        let status = match check_status(url.clone(), response.status, &Bytes::new(), accept_partial)
        {
            Ok(status) => status,
            Err(error) => {
                response.cancel();
                return Err(error);
            }
        };
        if let Err(error) = validate_range_response(status, range.as_ref(), &response.headers, &url)
        {
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
                let stream = me.raw_body(url, Some(resume), headers, true).await?;
                let skip = if stream.is_partial() { 0 } else { abs };
                Ok(Resumed { stream, skip })
            })
        });
        let body = resumable_body(
            first,
            refetch,
            resource,
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
#[derive(derive_more::Debug)]
pub struct AppleNet {
    #[debug(skip)]
    session: AppleSession,
    #[debug(skip)]
    net: Arc<RetryNet<RawAppleNet>>,
    #[debug(skip)]
    cancel: CancelToken,
    #[debug(skip)]
    connection_metrics: ConnectionMetrics,
    #[field(get)]
    options: NetOptions,
}

impl AppleNet {
    #[must_use]
    pub fn new<S>(options: NetOptions, pools: PoolRegion<S>, cancel: CancelToken) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        let connection_metrics = ConnectionMetrics::default();
        let session = AppleSession::new(&options, pools, connection_metrics.clone());
        let raw = RawAppleNet {
            session: session.clone(),
            cancel: cancel.clone(),
            options: options.clone(),
        };
        let net = Arc::new(RetryNet::new(
            raw,
            options.retry_policy.clone(),
            cancel.clone(),
            options.observer.clone(),
        ));
        Self {
            session,
            net,
            cancel,
            connection_metrics,
            options,
        }
    }

    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connection_metrics.connection_count()
    }

    #[must_use]
    pub fn with_observer(&self, observer: Option<Observer>) -> Self {
        let options = self.options.with_observer(observer);
        let raw = RawAppleNet {
            session: self.session.clone(),
            cancel: self.cancel.clone(),
            options: options.clone(),
        };
        let net = Arc::new(RetryNet::new(
            raw,
            options.retry_policy.clone(),
            self.cancel.clone(),
            options.observer.clone(),
        ));
        Self {
            net,
            options,
            cancel: self.cancel.clone(),
            session: self.session.clone(),
            connection_metrics: self.connection_metrics.clone(),
        }
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
) -> Result<u16, NetError> {
    let Some(status) = status else {
        return Err(NetError::Network(format!(
            "NSURLSession returned a non-HTTP response for {url}"
        )));
    };
    let ok = accepts_response_status(status, accept_partial);
    if ok {
        return Ok(status);
    }
    Err(status_error(url, status, body))
}
