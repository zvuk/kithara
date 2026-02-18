//! Shared async HTTP test server helpers.

use axum::Router;
use tokio::net::TcpListener;
use url::Url;

/// Lightweight HTTP test server wrapper.
pub struct TestHttpServer {
    base_url: Url,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestHttpServer {
    /// Spawn `router` on a random localhost port.
    ///
    /// # Panics
    ///
    /// Panics if listener bind or URL parsing fails.
    pub async fn new(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test HTTP listener");
        let addr = listener
            .local_addr()
            .expect("read test listener local addr");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        });

        tokio::spawn(async move {
            server.await.expect("run test HTTP server");
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
