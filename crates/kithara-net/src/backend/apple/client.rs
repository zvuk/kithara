use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use http::{Method as HttpMethod, Request, Response};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::CancelToken;
use url::Url;

use super::{
    request::{AppleRequest, Method},
    session::AppleSession,
    stream::AppleStreamResponse,
};
use crate::{
    ByteStream, HttpBody, HttpTransport, NetError, NetOptions,
    metrics::ConnectionMetrics,
    transport::external_body,
    types::{AcceptEncodingPolicy, Headers},
};

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

pub(crate) struct Transport {
    session: AppleSession,
    cancel: CancelToken,
}

impl Transport {
    pub(crate) fn new<S>(
        options: &NetOptions,
        pools: PoolRegion<S>,
        cancel: CancelToken,
        metrics: &ConnectionMetrics,
    ) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        Self {
            session: AppleSession::new(options, pools, metrics.clone()),
            cancel,
        }
    }

    #[kithara::flash(io)]
    async fn send(&self, request: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        let (parts, body) = request.into_parts();
        let method = match parts.method {
            HttpMethod::GET => Method::Get,
            HttpMethod::HEAD => Method::Head,
            HttpMethod::POST => Method::Post,
            method => {
                return Err(NetError::Protocol(format!(
                    "unsupported Apple method {method}"
                )));
            }
        };
        let url = Url::parse(&parts.uri.to_string())
            .map_err(|error| NetError::Network(error.to_string()))?;
        let mut headers = Headers::default();
        for (name, value) in &parts.headers {
            headers.insert(
                name.as_str(),
                value
                    .to_str()
                    .map_err(|error| NetError::Network(error.to_string()))?,
            );
        }
        let accept_encoding = if headers
            .get("accept-encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("identity"))
        {
            AcceptEncodingPolicy::Identity
        } else {
            AcceptEncodingPolicy::Configured
        };
        let body = matches!(method, Method::Post).then_some(body);
        let request = AppleRequest::new(&url, method, None, Some(headers), body, accept_encoding)?;
        let response = self.session.stream(request, self.cancel.clone()).await?;
        Self::response(method, response, &url).await
    }

    async fn response(
        method: Method,
        response: AppleStreamResponse,
        url: &Url,
    ) -> Result<Response<HttpBody>, NetError> {
        let Some(status) = response.status else {
            response.cancel();
            return Err(NetError::Network(format!(
                "NSURLSession returned a non-HTTP response for {url}"
            )));
        };
        let mut head = Response::builder().status(status);
        for (name, value) in response.headers.iter() {
            head = head.header(name, value);
        }
        let mut stream = ByteStream::from(response).into_inner();
        let body: HttpBody = if matches!(method, Method::Head) {
            // Release request admission only after the native HEAD task completes.
            while let Some(chunk) = stream.next().await {
                chunk?;
            }
            Box::pin(futures::stream::empty())
        } else {
            external_body(stream)
        };
        head.body(body)
            .map_err(|error| NetError::Network(error.to_string()))
    }
}

#[async_trait]
impl HttpTransport for Transport {
    async fn open(&self, request: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        self.send(request).await
    }
}

#[cfg(test)]
mod tests {
    use std::{pin::pin, task::Poll};

    use futures::{StreamExt, poll};
    use kithara_apple::foundation::urlsession::UrlSessionEvents;
    use kithara_platform::{sync::Arc, tokio::sync::oneshot};
    use kithara_test_utils::{cancel_token, kithara};

    use super::{Method, Transport};
    use crate::{
        Headers,
        backend::{
            apple::{
                delegate::{AppleSessionEvents, StreamState},
                response::StreamHead,
                session::AppleTask,
                stream::{
                    AppleBodyQueue, StartedStream, tests::suspended_task, wait_for_stream_head,
                },
            },
            pooled::ByteBuffers,
        },
        types::AcceptEncodingPolicy,
    };

    #[kithara::test(native, tokio)]
    #[case::head_empty(Method::Head, Some(0), true)]
    #[case::head_representation(Method::Head, Some(4096), true)]
    #[case::head_unknown(Method::Head, None, true)]
    #[case::get_streaming(Method::Get, Some(4), false)]
    async fn response_respects_native_completion_boundary(
        #[case] method: Method,
        #[case] representation_len: Option<u64>,
        #[case] waits_for_completion: bool,
    ) {
        let raw = suspended_task(method);
        let task: AppleTask = raw.clone().into();
        let id = task.id();
        let queue = Arc::new(AppleBodyQueue::new(
            task.clone(),
            16,
            8,
            ByteBuffers::new(crate::test_pools::pools()),
        ));
        let events = AppleSessionEvents::new(false);
        events.register_stream(
            id,
            StreamState {
                accept_encoding: AcceptEncodingPolicy::Identity,
                body_queue: Some(Arc::clone(&queue)),
                head_sender: None,
            },
        );
        let mut headers = Headers::default();
        if let Some(len) = representation_len {
            headers.insert("content-length", len.to_string());
        }
        let (sender, receiver) = oneshot::channel();
        assert!(
            sender
                .send(Ok(StreamHead {
                    headers,
                    status: Some(200)
                }))
                .is_ok()
        );
        let response = wait_for_stream_head(
            StartedStream {
                task,
                body_queue: Arc::clone(&queue),
                head_receiver: receiver,
                task_id: id,
            },
            Arc::clone(&events),
            cancel_token(),
        )
        .await
        .expect("native headers");
        let url = url::Url::parse("https://example.invalid/not-started").expect("test URI");
        let mut response = pin!(Transport::response(method, response, &url));
        let before_completion = poll!(response.as_mut());
        let was_pending = before_completion.is_pending();
        if matches!(method, Method::Get) {
            queue.push(bytes::Bytes::from_static(b"body"));
        }
        events.did_complete(id, None);
        let response = match before_completion {
            Poll::Ready(response) => response,
            Poll::Pending => response.await,
        }
        .expect("completed HTTP response");
        let mut body = response.into_body();
        let mut received = Vec::new();
        while let Some(chunk) = body.next().await {
            received.extend_from_slice(&chunk.expect("body delivery"));
        }
        raw.cancel();

        assert_eq!(was_pending, waits_for_completion);
        assert_eq!(
            received.as_slice(),
            if waits_for_completion {
                &b""[..]
            } else {
                &b"body"[..]
            }
        );
    }
}
