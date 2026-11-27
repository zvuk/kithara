use kithara_events::EventBus;
use kithara_net::{HttpClient, NetError, Observer};
use kithara_platform::{CancelGroup, sync::Arc, tokio};
use kithara_test_utils::kithara;
use tracing::warn;

use super::observer::RequestObserver;
use crate::{
    RequestId, RequestMethod,
    cmd::FetchCmd,
    response::{BodyStream, FetchResponse},
};

pub(crate) struct RequestContext<'a> {
    client: &'a HttpClient,
    cancel: &'a CancelGroup,
    bus: Option<&'a EventBus>,
    request_id: RequestId,
}

impl<'a> RequestContext<'a> {
    pub(crate) const fn new(
        client: &'a HttpClient,
        cancel: &'a CancelGroup,
        request_id: RequestId,
        bus: Option<&'a EventBus>,
    ) -> Self {
        Self {
            client,
            cancel,
            bus,
            request_id,
        }
    }

    /// Establish an HTTP connection and return a [`FetchResponse`].
    #[kithara::probe(request_id = self.request_id)]
    pub(crate) async fn establish(self, cmd: FetchCmd) -> Result<FetchResponse, NetError> {
        let Self {
            client,
            cancel,
            request_id,
            bus,
        } = self;
        let FetchCmd {
            method,
            url,
            range,
            headers,
            validator,
            max_bytes,
            ..
        } = cmd;

        if tracing::enabled!(tracing::Level::TRACE) {
            let names: Vec<&str> = headers
                .as_ref()
                .map(|h| h.iter().map(|(k, _)| k).collect())
                .unwrap_or_default();
            tracing::trace!(%url, ?method, ?range, header_names = ?names, "fetch: outgoing FetchCmd");
        }

        let client = client.with_observer(
            bus.cloned()
                .map(|bus| Observer(Arc::new(RequestObserver { bus, request_id }))),
        );

        if method == RequestMethod::Head {
            let resp_headers = tokio::select! {
                () = cancel.cancelled() => return Err(NetError::Cancelled),
                r = client.head(url, headers) => r?,
            };
            return Ok(FetchResponse {
                headers: resp_headers,
                body: BodyStream::empty(),
            });
        }

        let fetch_url = url.clone();
        let fetch = async {
            match range {
                Some(range) => client.get_range(url, range, headers).await,
                None => client.stream(url, headers).await,
            }
        };
        let byte_stream = tokio::select! {
            () = cancel.cancelled() => return Err(NetError::Cancelled),
            r = fetch => r?,
        };

        if let Some(validate) = validator
            && let Err(e) = validate(&byte_stream.headers)
        {
            warn!(url = %fetch_url, error = %e, "fetch rejected by response validator");
            return Err(e);
        }

        if let Some(limit) = max_bytes
            && let Some(declared) = byte_stream
                .headers
                .get("content-length")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|declared| *declared > limit)
        {
            return Err(NetError::TooLarge {
                bytes: declared,
                limit,
            });
        }

        let resp_headers = byte_stream.headers.clone();
        let body = BodyStream::wrap_http(byte_stream, cancel.clone(), max_bytes);
        Ok(FetchResponse {
            body,
            headers: resp_headers,
        })
    }
}
