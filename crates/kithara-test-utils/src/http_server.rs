//! Shared async HTTP test server helpers.

use std::{io, sync::OnceLock, time::Duration};

use axum::Router;
use tokio::{
    net::TcpListener,
    sync::{Mutex as TokioMutex, oneshot, watch},
    time::sleep as tokio_sleep,
};
use url::Url;

/// Process-global serialization gate for test HTTP listener bind.
/// Under `just test` workspace runs, ~1800 tests in parallel each
/// call `TcpListener::bind("127.0.0.1:0")`; the OS-provided ephemeral
/// port pool can briefly saturate, exhausting the retry budget and
/// panicking. Serializing the actual bind call eliminates that
/// cross-test race without slowing the OS-level allocation itself.
fn bind_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

/// Lightweight HTTP test server wrapper.
pub struct TestHttpServer {
    base_url: Url,
    shutdown_tx: Option<oneshot::Sender<()>>,
    done_rx: watch::Receiver<bool>,
}

impl TestHttpServer {
    /// Spawn `router` on a random localhost port.
    ///
    /// # Panics
    ///
    /// Panics if listener bind or URL parsing fails.
    pub async fn new(router: Router) -> Self {
        Self::bind("127.0.0.1:0", router).await
    }

    /// Spawn `router` on the given address (e.g. `"127.0.0.1:3444"`).
    ///
    /// # Panics
    ///
    /// Panics if listener bind or URL parsing fails.
    pub async fn bind(addr: &str, router: Router) -> Self {
        const BIND_RETRIES: usize = 25;
        const BIND_RETRY_DELAY_MS: u64 = 40;

        let listener = {
            let _guard = bind_lock().lock().await;
            let mut attempts = 0usize;
            loop {
                match TcpListener::bind(addr).await {
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
            }
        };
        let addr = listener
            .local_addr()
            .expect("read test listener local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (done_tx, done_rx) = watch::channel(false);
        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        });

        tokio::spawn(async move {
            server.await.expect("run test HTTP server");
            let _ = done_tx.send(true);
        });

        tokio_sleep(Duration::from_millis(100)).await;

        Self {
            base_url: Url::parse(&format!("http://{}", addr)).expect("parse base URL"),
            shutdown_tx: Some(shutdown_tx),
            done_rx,
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

    /// Wait until the server task completes.
    pub async fn completion(&mut self) {
        while !*self.done_rx.borrow_and_update() {
            if self.done_rx.changed().await.is_err() {
                break;
            }
        }
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}
