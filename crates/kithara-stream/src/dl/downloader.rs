//! [`Downloader`] — unified download orchestrator implementation.

use std::{pin::Pin, sync::Arc};

use futures::{StreamExt, stream::SelectAll};
use kithara_bufpool::{BytePool, byte_pool};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{
    Mutex,
    time::{Duration, sleep},
    tokio,
    tokio::{sync::mpsc, task},
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::cmd::{FetchCmd, FetchMethod, FetchResult};

/// Backpressure pause between chunks when throttle returns `true`.
const THROTTLE_PAUSE: Duration = Duration::from_millis(10);

pub(super) type BoxStream = Pin<Box<dyn futures::Stream<Item = FetchCmd> + Send>>;

/// Unified downloader — sole HTTP client owner and fetch orchestrator.
///
/// Created once at the application level, then shared (via [`Clone`]) across
/// protocol configs. Protocols register themselves with
/// [`register`](Self::register); the async loop polls all registered streams
/// cooperatively via `SelectAll`.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Downloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloader").finish_non_exhaustive()
    }
}

// Debug on Inner would require Debug on HttpClient/Mutex/mpsc — skip internals.
struct Inner {
    client: HttpClient,
    cancel: CancellationToken,
    chunk_timeout: Duration,
    pool: BytePool,
    runtime: Option<tokio::runtime::Handle>,
    /// Sender for registering new protocol streams (cold path).
    register_tx: mpsc::UnboundedSender<BoxStream>,
    /// Receiver — taken once by [`spawn`](Downloader::spawn).
    register_rx: Mutex<Option<mpsc::UnboundedReceiver<BoxStream>>>,
}

impl Downloader {
    /// Create a new downloader from configuration.
    ///
    /// Constructs the internal `HttpClient` from the supplied network options.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let chunk_timeout = config.net.request_timeout;
        let runtime = config.runtime;
        let pool = config.pool.unwrap_or_else(|| byte_pool().clone());
        Self {
            inner: Arc::new(Inner {
                client: HttpClient::new(config.net),
                cancel: config.cancel,
                chunk_timeout,
                pool,
                runtime,
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
            }),
        }
    }

    /// Register a protocol track and return a handle for waiting.
    ///
    /// Lazily spawns the download loop on the first call. The protocol
    /// is cloned internally — the clone is polled by the loop, the
    /// original is returned inside the [`TrackHandle`] for wait calls
    /// and eventual use as a `Source`.
    pub fn register<S>(&self, stream: S) -> super::TrackHandle<S>
    where
        S: futures::Stream<Item = FetchCmd> + Clone + Send + Unpin + 'static,
    {
        self.ensure_spawned();
        let handle_copy = stream.clone();
        let _ = self.inner.register_tx.send(Box::pin(stream));
        super::TrackHandle::new(handle_copy)
    }

    /// Execute a single [`FetchCmd`] directly and return the result.
    ///
    /// Uses the same `HttpClient` and pool as the streaming pipeline.
    /// Intended for control-plane requests (playlists, DRM keys) where the
    /// caller needs the result before proceeding.
    ///
    /// If `cmd.on_complete` is set, it is called with `&result` before
    /// the result is returned — both the callback and the caller observe
    /// the same result.
    pub async fn execute(&self, cmd: FetchCmd) -> FetchResult {
        let client = self.inner.client.clone();
        let chunk_timeout = self.inner.chunk_timeout;
        let pool = self.inner.pool.clone();
        Self::execute_one(&client, chunk_timeout, &pool, cmd).await
    }

    /// Execute a batch of [`FetchCmd`] concurrently and return all results.
    ///
    /// Runs all commands in parallel via `join_all` using the same
    /// `HttpClient` and pool as the streaming pipeline. Intended for
    /// batched control-plane requests (e.g. HEAD size-map queries for all
    /// segments at once) and for HLS plan loop dispatch where a set of
    /// segment fetches should run concurrently but deliver `on_complete`
    /// callbacks in a deterministic order so that downstream state
    /// transitions (commit, layout update) are applied in batch-index
    /// order regardless of network completion order.
    ///
    /// Ordering guarantees:
    /// - **Network fetches**: run concurrently; completion order is
    ///   determined by network timing.
    /// - **`on_complete` callbacks**: fired in input order (index 0 first,
    ///   then index 1, etc.), after **all** fetches in the batch have
    ///   finished. A slow fetch at index 0 delays the callbacks for
    ///   indices 1..N until index 0 finishes.
    /// - **Returned `Vec<FetchResult>`**: preserves input order of `cmds`.
    pub async fn execute_batch(&self, cmds: Vec<FetchCmd>) -> Vec<FetchResult> {
        let client = self.inner.client.clone();
        let chunk_timeout = self.inner.chunk_timeout;
        let pool = self.inner.pool.clone();

        // Strip on_completes so we can fire them in input order after all
        // fetches finish. The fetch itself runs without on_complete.
        let mut on_completes: Vec<Option<super::cmd::OnCompleteFn>> =
            Vec::with_capacity(cmds.len());
        let mut stripped: Vec<FetchCmd> = Vec::with_capacity(cmds.len());
        for mut cmd in cmds {
            on_completes.push(cmd.on_complete.take());
            stripped.push(cmd);
        }

        // Run all fetches concurrently (without firing on_complete).
        let futs: Vec<_> = stripped
            .into_iter()
            .map(|cmd| {
                let client = client.clone();
                let pool = pool.clone();
                async move { Self::fetch_only(&client, chunk_timeout, &pool, cmd).await }
            })
            .collect();
        let results = futures::future::join_all(futs).await;

        // Fire on_completes in input order, after all fetches are done.
        for (result, cb_opt) in results.iter().zip(on_completes.into_iter()) {
            if let Some(cb) = cb_opt {
                cb(result);
            }
        }

        results
    }

    /// Ensure the download loop is running (lazy spawn on first register).
    fn ensure_spawned(&self) {
        let Some(rx) = self.inner.register_rx.lock_sync().take() else {
            return;
        };
        let this = self.clone();
        if let Some(handle) = &self.inner.runtime {
            handle.spawn(async move { this.run(rx).await });
        } else {
            task::spawn(async move { this.run(rx).await });
        }
    }

    /// Core download loop.
    ///
    /// Picks up newly registered streams, waits for the next command from
    /// any stream via `SelectAll`, spawns its execution, and loops. Each
    /// fetch runs as an independent task so new commands (e.g. seek Range)
    /// are not blocked by in-flight downloads.
    #[kithara_hang_detector::hang_watchdog]
    async fn run(&self, mut rx: mpsc::UnboundedReceiver<BoxStream>) {
        let mut streams: SelectAll<BoxStream> = SelectAll::new();
        let mut ever_had_streams = false;

        loop {
            // Cold path: pick up newly registered streams.
            while let Ok(s) = rx.try_recv() {
                streams.push(s);
                ever_had_streams = true;
            }

            // Wait for next command (or registration, or cancel).
            let cmd = if !ever_had_streams {
                // No streams yet — wait for first registration.
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    stream = rx.recv() => match stream {
                        Some(s) => {
                            streams.push(s);
                            ever_had_streams = true;
                            hang_reset!();
                            continue;
                        }
                        None => return,
                    },
                }
            } else {
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    stream = rx.recv() => {
                        if let Some(s) = stream {
                            streams.push(s);
                            ever_had_streams = true;
                        }
                        hang_reset!();
                        continue;
                    },
                    cmd = streams.next() => if let Some(c) = cmd { c } else {
                        debug!("all protocol streams completed, download loop exiting");
                        return;
                    },
                }
            };

            let client = self.inner.client.clone();
            let chunk_timeout = self.inner.chunk_timeout;
            let pool = self.inner.pool.clone();
            task::spawn(async move {
                Self::execute_one(&client, chunk_timeout, &pool, cmd).await;
            });

            hang_tick!();
        }
    }

    /// Execute a single fetch, calling `on_complete` with `&result` when done.
    ///
    /// Convenience wrapper: runs the fetch via [`fetch_only`](Self::fetch_only),
    /// then invokes `on_complete` (if any) before returning the result.
    async fn execute_one(
        client: &HttpClient,
        chunk_timeout: Duration,
        pool: &BytePool,
        mut cmd: FetchCmd,
    ) -> FetchResult {
        let on_complete = cmd.on_complete.take();
        let result = Self::fetch_only(client, chunk_timeout, pool, cmd).await;
        if let Some(cb) = on_complete {
            cb(&result);
        }
        result
    }

    /// Execute the fetch without calling `on_complete`.
    ///
    /// Calls `on_connect`, streams body through `writer`, accumulates body
    /// for [`FetchMethod::Get`], and returns the result. The caller is
    /// responsible for invoking `on_complete` (if any) — this split enables
    /// ordered `on_complete` delivery in batched execution where the caller
    /// wants to fire callbacks in a deterministic order.
    #[expect(clippy::cognitive_complexity)]
    async fn fetch_only(
        client: &HttpClient,
        chunk_timeout: Duration,
        pool: &BytePool,
        mut cmd: FetchCmd,
    ) -> FetchResult {
        let url = cmd.url.clone();

        // HEAD — headers only, no body.
        if cmd.method == FetchMethod::Head {
            return match client.head(url.clone(), cmd.headers.take()).await {
                Ok(headers) => {
                    if let Some(on_connect) = cmd.on_connect.take() {
                        on_connect(&headers);
                    }
                    FetchResult::Ok {
                        bytes_written: 0,
                        headers,
                        body: None,
                    }
                }
                Err(e) => {
                    debug!(?url, "head failed: {e}");
                    FetchResult::Err(e)
                }
            };
        }

        // GET or Stream — both fetch a body stream.
        let stream_result = match cmd.range.take() {
            Some(range) => {
                client
                    .get_range(url.clone(), range, cmd.headers.take())
                    .await
            }
            None => client.stream(url.clone(), cmd.headers.take()).await,
        };

        let mut byte_stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                debug!(?url, "fetch failed: {e}");
                return FetchResult::Err(e);
            }
        };

        let response_headers = byte_stream.headers.clone();

        if let Some(on_connect) = cmd.on_connect.take() {
            on_connect(&response_headers);
        }

        // Get mode: accumulate body into pool buffer.
        let mut body_buf = if cmd.method == FetchMethod::Get {
            Some(pool.get())
        } else {
            None
        };

        let mut bytes_written: u64 = 0;

        loop {
            let chunk = tokio::select! {
                c = byte_stream.next() => c,
                () = sleep(chunk_timeout) => None,
            };
            match chunk {
                Some(Ok(data)) => {
                    if let Some(ref mut writer) = cmd.writer
                        && let Err(io_err) = writer(data.as_ref())
                    {
                        debug!(?url, bytes_written, "writer error: {io_err}");
                        return FetchResult::Err(NetError::Http(io_err.to_string()));
                    }
                    if let Some(ref mut buf) = body_buf {
                        buf.extend_from_slice(data.as_ref());
                    }
                    bytes_written += data.len() as u64;

                    // Backpressure: pause while download is too far ahead.
                    if let Some(ref throttle) = cmd.throttle {
                        while throttle() {
                            sleep(THROTTLE_PAUSE).await;
                        }
                    }
                }
                Some(Err(net_err)) => {
                    debug!(?url, bytes_written, "stream error: {net_err}");
                    return FetchResult::Err(net_err);
                }
                None => break, // stream ended or chunk idle timeout
            }
        }

        debug!(?url, bytes_written, "fetch complete");
        FetchResult::Ok {
            bytes_written,
            headers: response_headers,
            body: body_buf.map(kithara_bufpool::PooledOwned::into_inner),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
