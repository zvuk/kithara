use async_trait::async_trait;
use bytes::Bytes;
use http::{Method, Request, Response};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::CancelToken;
use url::Url;

use super::{
    exchange::{Exchange, Opened},
    transport::{HostMethod, HostRequest, HostRequestBody},
};
use crate::{
    HttpBody, HttpTransport, NetError, NetOptions, backend::pooled::ByteBuffers,
    metrics::ConnectionMetrics, transport::external_body,
};

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

pub(crate) struct Transport {
    exchange: Exchange,
}

impl Transport {
    pub(crate) fn new<S>(
        _options: &NetOptions,
        pools: PoolRegion<S>,
        cancel: CancelToken,
        _metrics: &ConnectionMetrics,
    ) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        Self {
            exchange: Exchange {
                cancel,
                buffers: ByteBuffers::new(pools),
            },
        }
    }

    #[kithara::flash(io)]
    async fn send(&self, request: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        let (parts, body) = request.into_parts();
        let method = match parts.method {
            Method::GET => HostMethod::Get,
            Method::HEAD => HostMethod::Head,
            Method::POST => HostMethod::Post,
            method => {
                return Err(NetError::Protocol(format!(
                    "unsupported host method {method}"
                )));
            }
        };
        let url = Url::parse(&parts.uri.to_string())
            .map_err(|error| NetError::Network(error.to_string()))?;
        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.to_string(),
                    value
                        .to_str()
                        .map_err(|error| NetError::Network(error.to_string()))?
                        .to_owned(),
                ))
            })
            .collect::<Result<Vec<_>, NetError>>()?;
        let body = if method == HostMethod::Post {
            Some(HostRequestBody::with_bytes(&self.exchange.buffers, &body)?)
        } else {
            None
        };
        let Opened {
            call,
            headers,
            status,
        } = self
            .exchange
            .open(HostRequest {
                method,
                body,
                url,
                headers,
            })
            .await?;
        let mut response = Response::builder().status(status);
        for (name, value) in headers {
            response = response.header(name, value);
        }
        let body = external_body(Box::pin(self.exchange.body(call)));
        response
            .body(body)
            .map_err(|error| NetError::Network(error.to_string()))
    }
}

#[async_trait]
impl HttpTransport for Transport {
    async fn open(&self, request: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        self.send(request).await
    }
}
