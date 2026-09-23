use bytes::Bytes;
use futures::{TryStreamExt, future::poll_fn};
use kithara_platform::{CancelToken, sync::Arc, tokio::select};
use url::Url;

use super::{
    call::{Call, CallState, HostBodyStream},
    protocol::{request_headers, response_headers},
    slot,
    transport::{HostEvents, HostMethod, HostRequest, HostRequestBody},
};
use crate::{
    backend::pooled::ByteBuffers,
    error::NetError,
    types::{AcceptEncodingPolicy, Headers, RangeSpec},
};

pub(super) struct Opened {
    pub(super) call: Call,
    pub(super) headers: Headers,
    pub(super) status: u16,
}

pub(super) struct Target<'a> {
    pub(super) method: HostMethod,
    pub(super) accept_encoding: AcceptEncodingPolicy,
    pub(super) url: &'a Url,
    pub(super) range: Option<&'a RangeSpec>,
    pub(super) headers: Option<&'a Headers>,
    pub(super) body: Option<Bytes>,
}

#[derive(Clone)]
pub(super) struct Exchange {
    pub(super) buffers: ByteBuffers,
    pub(super) cancel: CancelToken,
}

impl Exchange {
    pub(super) fn body(&self, call: Call) -> HostBodyStream {
        HostBodyStream::new(call, self.buffers.clone(), self.cancel.clone())
    }

    pub(super) async fn drain(&self, call: Call) -> Result<Bytes, NetError> {
        let chunks: Vec<Bytes> = self.body(call).try_collect().await?;
        Ok(join(chunks))
    }

    pub(super) async fn open(&self, target: Target<'_>) -> Result<Opened, NetError> {
        let transport = slot::installed()?;
        let body = target
            .body
            .map(|body| HostRequestBody::with_bytes(&self.buffers, &body))
            .transpose()?;
        let state = Arc::new(CallState::default());
        let request = HostRequest {
            method: target.method,
            url: target.url.clone(),
            headers: request_headers(target.headers, target.range, target.accept_encoding),
            body,
        };
        let call = transport.start(request, HostEvents(Arc::clone(&state)))?;
        let call = Call::new(call, state);

        let response = select! {
            biased;
            () = self.cancel.cancelled() => Err(NetError::Cancelled),
            response = poll_fn(|cx| call.state().poll_response(cx)) => response,
        }?;
        let headers = response_headers(response.headers, response.status, target.url)?;
        Ok(Opened {
            call,
            headers,
            status: response.status,
        })
    }
}

fn join(chunks: Vec<Bytes>) -> Bytes {
    if chunks.len() == 1 {
        return chunks.into_iter().next().unwrap_or_default();
    }
    Bytes::from(chunks.concat())
}
