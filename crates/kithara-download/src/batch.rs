use std::sync::atomic::Ordering;

use kithara_abr::{AbrController, AbrPeerId, BandwidthSource};
use kithara_events::EventBus;
use kithara_net::{HttpClient, NetError, NetObserver, Observer, Retryability};
use kithara_platform::{
    CancelGroup, CancelToken,
    flash::virtual_now,
    sync::Arc,
    time::{Duration, Instant},
    tokio,
    tokio::task,
};
use kithara_test_utils::kithara;
use tracing::warn;

use super::{
    cmd::FetchCmd,
    downloader::DownloaderInner,
    peer::{InternalCmd, ResponseTarget, SlotEntry},
    response::{BodyStream, FetchResponse},
};
use crate::{CancelReason, DownloaderEvent, RequestId, RequestMethod};

struct RequestObserver {
    bus: EventBus,
    request_id: RequestId,
}

impl NetObserver for RequestObserver {
    fn body_resumed(&self, resume_number: u32, from_offset: u64, honoured_range: bool) {
        self.bus.publish(DownloaderEvent::BodyResumed {
            resume_number,
            from_offset,
            honoured_range,
            request_id: self.request_id,
        });
    }

    fn body_stalled(&self, consumed: u64, expected: Option<u64>, stall: Duration) {
        self.bus.publish(DownloaderEvent::BodyStalled {
            consumed,
            expected,
            stall,
            request_id: self.request_id,
        });
    }

    fn first_byte(&self, ttfb: Duration, status: u16, partial: bool) {
        self.bus.publish(DownloaderEvent::FirstByte {
            ttfb,
            status,
            partial,
            request_id: self.request_id,
        });
    }

    fn retry_exhausted(&self, max_retries: u32, consumed: u64, error: &NetError) {
        self.bus.publish(DownloaderEvent::RetryExhausted {
            max_retries,
            consumed,
            request_id: self.request_id,
            error: error.clone(),
        });
    }

    fn retrying(&self, attempt: u32, max_retries: u32, error: &NetError, backoff: Duration) {
        self.bus.publish(DownloaderEvent::RequestRetrying {
            attempt,
            max_retries,
            backoff,
            request_id: self.request_id,
            error: error.clone(),
        });
    }
}

fn publish_cancelled(
    bus: Option<&EventBus>,
    request_id: RequestId,
    reason: CancelReason,
    bytes_transferred: u64,
) {
    if let Some(bus) = bus {
        bus.publish(DownloaderEvent::RequestCancelled {
            request_id,
            reason,
            bytes_transferred,
        });
    }
}

/// Collects slot entries and executes them via fire-and-forget spawn.
pub(super) struct BatchGroup {
    entries: Vec<SlotEntry>,
}

pub(super) struct BatchResult {
    pub(super) pending: Vec<SlotEntry>,
    pub(super) dispatched: usize,
}

impl FromIterator<SlotEntry> for BatchGroup {
    fn from_iter<I: IntoIterator<Item = SlotEntry>>(entries: I) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

impl BatchGroup {
    pub(super) const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Spawn the live FIFO prefix that fits the current concurrency capacity,
    /// return the remaining entries to the registry, and deliver-cancel dead
    /// entries. The registry can then admit newly urgent work before revisiting
    /// the pending demand tail.
    ///
    /// Returns the number of fetches actually spawned (cancelled cmds don't
    /// count), so
    /// [`Registry::tick`](super::registry::Registry::tick) can treat a non-zero
    /// dispatch as forward progress for the hang watchdog.
    ///
    /// The `batch_size` / `first_request_id` probe values are written as
    /// `name = expr`: the macro emits them only with the `usdt` feature, so
    /// ordinary production builds do not evaluate the metric expressions.
    #[kithara::flash(true)]
    #[kithara::probe(
        batch_size = self.entries.len(),
        first_request_id = self
            .entries
            .first()
            .map_or(0, |entry| entry.cmd.request_id.get())
    )]
    pub(super) fn process(self, inner: &DownloaderInner) -> BatchResult {
        let capacity = inner
            .max_concurrent
            .saturating_sub(inner.inflight.load(Ordering::Relaxed));
        let mut dispatched = 0;
        let mut pending: Vec<SlotEntry> = Vec::new();
        for SlotEntry { cmd, peer_cancel } in self.entries {
            if cmd.cancel.is_cancelled() {
                deliver_cancelled_with_event(cmd, &peer_cancel);
            } else if dispatched < capacity {
                spawn_fetch(inner, cmd, peer_cancel);
                dispatched += 1;
            } else {
                pending.push(SlotEntry { peer_cancel, cmd });
            }
        }
        BatchResult {
            pending,
            dispatched,
        }
    }
}

/// Spawn an HTTP fetch task for one command.
fn spawn_fetch(inner: &DownloaderInner, internal: InternalCmd, peer_cancel: CancelToken) {
    let client = inner.client.clone();
    let soft_timeout = inner.soft_timeout;
    let inflight = inner.inflight.clone();
    let fetch_waker = inner.fetch_waker.clone();
    let capacity_notify = Arc::clone(&inner.capacity_notify);
    let abr = Arc::clone(&inner.abr);
    let downloader_cancel = inner.cancel.clone();
    let peer_id = internal.peer_id;
    let request_id = internal.request_id;
    let started = Instant::now();
    let wait_in_queue = started.saturating_duration_since(internal.enqueued_at);
    let mut cmd = internal.cmd;
    let writer = cmd.take_writer();
    let on_complete_cb = cmd.take_on_complete();
    let on_response_cb = cmd.on_response.take();
    let on_slow_cb = cmd.on_slow.take();
    let bus = internal.bus;
    let cancel = internal.cancel.clone();
    let epoch_cancel = cmd.cancel.clone();

    kithara::probe_event!(start_request, request_id, wait_in_queue);
    inflight.fetch_add(1, Ordering::Relaxed);
    if let Some(bus) = bus.as_ref() {
        bus.publish(DownloaderEvent::RequestStarted {
            request_id,
            wait_in_queue,
        });
    }

    task::spawn(async move {
        let slow_bus = bus.clone();
        let fetch = async move {
            let result = establish(&client, &cancel, cmd, request_id, bus.as_ref()).await;
            deliver(
                request_id,
                DeliveryContext {
                    result,
                    writer,
                    on_complete_cb,
                    on_response_cb,
                    abr,
                    peer_id,
                    started,
                    bus,
                    target: internal.response,
                    peer_cancel: &peer_cancel,
                    epoch_cancel: epoch_cancel.as_ref(),
                    downloader_cancel: &downloader_cancel,
                },
            )
            .await;
        };
        with_soft_timeout(
            fetch,
            soft_timeout,
            slow_bus.as_ref(),
            request_id,
            on_slow_cb,
        )
        .await;
        inflight.fetch_sub(1, Ordering::Relaxed);
        capacity_notify.notify_one();
        fetch_waker.wake();
    });
}

/// Measured duration of a fetch, robust to the flash clock split.
///
/// `started` is captured in the `#[kithara::flash(true)]` dispatch (`process`),
/// so under `flash` it reads the VIRTUAL clock; this delivery runs in a
/// non-flash fetch task (its real-socket I/O and timeouts must stay on real
/// time), where `started.elapsed()` reads the REAL clock. A virtual `started`
/// minus a real `now` would saturate to ZERO (and silently drop the bandwidth
/// sample). Take the larger of the real elapsed and the virtual-clock delta: a
/// real server delay (off-feature / `flash(false)`) is captured by the real
/// elapsed, a virtual server delay (a flash test's withhold gate) is captured by
/// the virtual delta, and an instant fetch yields zero on both. Off the
/// `flash` feature `virtual_now` is real `Instant::now`, so both terms
/// collapse to the same real elapsed.
fn fetch_elapsed(started: Instant) -> Duration {
    started
        .elapsed()
        .max(virtual_now().saturating_duration_since(started))
}

/// Race `fut` against a `soft_timeout` timer. When the timer wins, publish
/// [`DownloaderEvent::LoadSlow`] on `bus` (if any) and keep waiting for
/// `fut` to complete. Does not abort the underlying request.
#[kithara::probe(request_id)]
async fn with_soft_timeout<F, T>(
    fut: F,
    soft: Duration,
    bus: Option<&EventBus>,
    request_id: RequestId,
    on_slow: Option<super::cmd::OnSlowFn>,
) -> T
where
    F: Future<Output = T>,
{
    tokio::pin!(fut);
    let started = Instant::now();
    tokio::select! {
        r = &mut fut => r,
        () = kithara_platform::time::sleep(soft) => {
            if let Some(bus) = bus {
                bus.publish(DownloaderEvent::LoadSlow {
                    request_id,
                    elapsed: started.elapsed(),
                });
            }
            if let Some(on_slow) = on_slow {
                on_slow();
            }
            fut.await
        }
    }
}

/// Establish an HTTP connection and return a [`FetchResponse`].
#[kithara::probe(request_id)]
async fn establish(
    client: &HttpClient,
    cancel: &CancelGroup,
    cmd: FetchCmd,
    request_id: RequestId,
    bus: Option<&EventBus>,
) -> Result<FetchResponse, NetError> {
    let FetchCmd {
        method,
        url,
        range,
        headers,
        validator,
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

    let resp_headers = byte_stream.headers.clone();
    let body = BodyStream::wrap_http(byte_stream, cancel.clone());
    Ok(FetchResponse {
        body,
        headers: resp_headers,
    })
}

/// Compute pre-rounded bandwidth in bps. Guards against zero duration
/// (cache hits / instant responses) so subscribers don't repeat the
/// math or hit a div-by-zero.
fn bandwidth_bps(bytes: u64, duration: Duration) -> u64 {
    /// Bits per byte × milliseconds-per-second-power-of-ten conversion
    /// for the bps formula `bytes * 8 * 1000 / duration_ms`.
    const BITS_TIMES_MS_PER_SEC: u64 = 8_000;
    let ms = u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    bytes.saturating_mul(BITS_TIMES_MS_PER_SEC) / ms
}

/// Determine why a fetch was cancelled.
///
/// Order of checks reflects priority: peer-cancel implies the whole
/// peer is going away; epoch-cancel is a per-fetch invalidation;
/// downloader-shutdown is the global stop. `BeforeStart` catches the
/// race where the cancel token was set before any fetch task ran.
fn classify_cancel(
    peer_cancel: &CancelToken,
    epoch_cancel: Option<&CancelToken>,
    downloader_cancel: &CancelToken,
) -> CancelReason {
    if peer_cancel.is_cancelled() {
        CancelReason::PeerCancel
    } else if epoch_cancel.is_some_and(CancelToken::is_cancelled) {
        CancelReason::EpochCancel
    } else if downloader_cancel.is_cancelled() {
        CancelReason::DownloaderShutdown
    } else {
        CancelReason::BeforeStart
    }
}

/// All the per-fetch context `deliver` needs: identity (request id, peer id,
/// abr controller), wall-clock anchor (`started`), the body sinks (writer +
/// completion callback), the `bus` for telemetry, and the three nested cancel
/// tokens (peer, epoch, downloader) used to classify cancellation reasons.
struct DeliveryContext<'a> {
    downloader_cancel: &'a CancelToken,
    peer_cancel: &'a CancelToken,
    peer_id: AbrPeerId,
    abr: Arc<AbrController>,
    started: Instant,
    bus: Option<EventBus>,
    epoch_cancel: Option<&'a CancelToken>,
    on_complete_cb: Option<super::cmd::OnCompleteFn>,
    on_response_cb: Option<super::cmd::OnResponseFn>,
    writer: Option<super::cmd::WriterFn>,
    target: ResponseTarget,
    result: Result<FetchResponse, NetError>,
}

/// Route a fetch result to its target and publish the matching
/// `DownloaderEvent` on `bus` (if any).
#[kithara::probe(request_id)]
async fn deliver(request_id: RequestId, ctx: DeliveryContext<'_>) {
    let DeliveryContext {
        target,
        result,
        writer,
        on_complete_cb,
        on_response_cb,
        abr,
        peer_id,
        started,
        bus,
        peer_cancel,
        epoch_cancel,
        downloader_cancel,
    } = ctx;
    match target {
        ResponseTarget::Channel(tx) => {
            // WHY: Collect the body here, on the downloader's (possibly separate) worker, so only `Send` bytes cross back to the caller - the
            // raw HTTP body stream is `!Send` on wasm.
            let collected = match result {
                Ok(resp) => {
                    let headers = resp.headers.clone();
                    resp.body.collect().await.map(|bytes| (headers, bytes))
                }
                Err(e) => Err(e),
            };
            tx.send(collected).ok();
        }
        ResponseTarget::Streaming => match result {
            Ok(resp) => {
                let headers = resp.headers.clone();
                let Some(mut w) = writer else {
                    if let Some(cb) = on_complete_cb {
                        cb(0, Some(&headers), None);
                    }
                    return;
                };
                if let Some(cb) = on_response_cb {
                    cb(&headers);
                }
                let write_result = resp.body.write_all(|chunk| w(chunk)).await;
                let elapsed = fetch_elapsed(started);
                match write_result {
                    Ok(total) => {
                        kithara::probe_event!(
                            finish_request,
                            request_id,
                            bytes_transferred = total,
                            duration = elapsed
                        );
                        if total > 0 {
                            abr.record_bandwidth(peer_id, total, elapsed, BandwidthSource::Network);
                        }
                        if let Some(bus) = bus.as_ref() {
                            bus.publish(DownloaderEvent::RequestCompleted {
                                request_id,
                                bytes_transferred: total,
                                duration: elapsed,
                                bandwidth_bps: bandwidth_bps(total, elapsed),
                            });
                        }
                        if let Some(cb) = on_complete_cb {
                            cb(total, Some(&headers), None);
                        }
                    }
                    Err(ref e) => {
                        publish_failure_or_cancel(
                            bus.as_ref(),
                            request_id,
                            e,
                            0,
                            peer_cancel,
                            epoch_cancel,
                            downloader_cancel,
                        );
                        if let Some(cb) = on_complete_cb {
                            cb(0, Some(&headers), Some(e));
                        }
                    }
                }
            }
            Err(ref e) => {
                publish_failure_or_cancel(
                    bus.as_ref(),
                    request_id,
                    e,
                    0,
                    peer_cancel,
                    epoch_cancel,
                    downloader_cancel,
                );
                if let Some(cb) = on_complete_cb {
                    cb(0, None, Some(e));
                }
            }
        },
    }
}

/// Publish `RequestFailed` (network error) or `RequestCancelled`
/// (cancel token fired) depending on the error variant.
fn publish_failure_or_cancel(
    bus: Option<&EventBus>,
    request_id: RequestId,
    err: &NetError,
    bytes_transferred: u64,
    peer_cancel: &CancelToken,
    epoch_cancel: Option<&CancelToken>,
    downloader_cancel: &CancelToken,
) {
    if matches!(err, NetError::Cancelled) {
        let reason = classify_cancel(peer_cancel, epoch_cancel, downloader_cancel);
        kithara::probe_event!(
            abort_request,
            request_id,
            reason,
            bytes_transferred,
            was_in_flight = true
        );
        publish_cancelled(bus, request_id, reason, bytes_transferred);
    } else {
        let retryable = err.retryability() == Retryability::Transient;
        kithara::probe_event!(fail_request, request_id, retryable);
        if let Some(bus) = bus {
            bus.publish(DownloaderEvent::RequestFailed {
                request_id,
                retryable,
                error: err.clone(),
            });
        }
    }
}

/// Cancel an [`InternalCmd`] before it ever spawned a task. Publishes
/// `RequestCancelled { reason: BeforeStart }` (or whichever token is
/// already cancelled — the classifier takes care of that) on the
/// command's bus.
///
/// Public to siblings (used by [`Registry::reschedule`] when a peer
/// went away) and by [`BatchGroup::process`] for early-cancel paths.
pub(super) fn deliver_cancelled_with_event(internal: InternalCmd, peer_cancel: &CancelToken) {
    let request_id = internal.request_id;
    let bus = internal.bus.clone();
    let epoch_cancel = internal.cmd.cancel.clone();
    let placeholder_inner = CancelToken::never();
    let reason = classify_cancel(peer_cancel, epoch_cancel.as_ref(), &placeholder_inner);
    kithara::probe_event!(
        abort_request,
        request_id,
        reason,
        bytes_transferred = 0_u64,
        was_in_flight = false
    );
    publish_cancelled(bus.as_ref(), request_id, reason, 0);
    deliver_cancelled(internal.response, internal.cmd);
}

/// Route a cancellation to its target. Does NOT publish events — use
/// [`deliver_cancelled_with_event`] for that.
///
/// The oneshot is the channel path's completion signal, matching `deliver`,
/// which never calls `on_complete` for a `Channel` target. Only the streaming
/// path owns a claim through its callback.
pub(super) fn deliver_cancelled(target: ResponseTarget, mut cmd: FetchCmd) {
    let err = NetError::Cancelled;
    match target {
        ResponseTarget::Channel(tx) => {
            tx.send(Err(err)).ok();
        }
        ResponseTarget::Streaming => {
            if let Some(cb) = cmd.take_on_complete() {
                cb(0, None, Some(&err));
            }
        }
    }
}
