use std::{future::Future, pin::Pin, task::Context};

use crate::metrics::ConnectionMetrics;

#[derive(Clone)]
pub(super) struct CountConnectionsLayer {
    metrics: ConnectionMetrics,
}

impl CountConnectionsLayer {
    pub(super) fn new(metrics: ConnectionMetrics) -> Self {
        Self { metrics }
    }
}

impl<S> tower_layer::Layer<S> for CountConnectionsLayer {
    type Service = CountConnectionsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CountConnectionsService {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct CountConnectionsService<S> {
    metrics: ConnectionMetrics,
    inner: S,
}

impl<S, Request> tower_service::Service<Request> for CountConnectionsService<S>
where
    S: tower_service::Service<Request>,
    S::Future: Send + 'static,
    S::Response: 'static,
    S::Error: 'static,
{
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    type Response = S::Response;

    fn call(&mut self, request: Request) -> Self::Future {
        let future = self.inner.call(request);
        let metrics = self.metrics.clone();
        Box::pin(async move {
            let result = future.await;
            if result.is_ok() {
                metrics.record_opened_connection();
            }
            result
        })
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }
}
