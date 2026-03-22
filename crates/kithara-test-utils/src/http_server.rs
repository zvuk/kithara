//! Shared async HTTP test server helpers.

use std::{io, time::Duration};

use axum::Router;
use tokio::{net::TcpListener, sync::oneshot, time::sleep as tokio_sleep};
use url::Url;

/// Lightweight HTTP test server wrapper.
pub struct TestHttpServer {
    base_url: Url,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl TestHttpServer {
    /// Spawn `router` on a random localhost port.
    ///
    /// # Panics
    ///
    /// Panics if listener bind or URL parsing fails.
    pub async fn new(router: Router) -> Self {
        const BIND_RETRIES: usize = 25;
        const BIND_RETRY_DELAY_MS: u64 = 40;

        let mut attempts = 0usize;
        let listener = loop {
            match TcpListener::bind("127.0.0.1:0").await {
                Ok(listener) => break listener,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied
                            | io::ErrorKind::AddrInUse
                            | io::ErrorKind::AddrNotAvailable
                    ) =>
                {
                    attempts = attempts.saturating_add(1);
                    if attempts >= BIND_RETRIES {
                        panic!("bind test HTTP listener after retries: {error}");
                    }
                }
                Err(error) => panic!("bind test HTTP listener: {error}"),
            }

            tokio_sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
        };
        let addr = listener
            .local_addr()
            .expect("read test listener local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        });

        tokio::spawn(async move {
            server.await.expect("run test HTTP server");
        });

        tokio_sleep(Duration::from_millis(100)).await;

        Self {
            base_url: Url::parse(&format!("http://{}", addr)).expect("parse base URL"),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Join path to server base URL.
    ///
    /// # Panics
    ///
    /// Panics if URL join fails.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        self.base_url.join(path).expect("join server URL path")
    }

    /// Base URL of this server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}
