//! Batch execution: epoch-aware grouping and fetch spawning.

use std::sync::{Arc, atomic::Ordering};

use kithara_abr::{AbrController, AbrPeerId};
use kithara_events::{
    BandwidthSource, CancelReason, DownloaderEvent, EventBus, RequestId, RequestMethod,
};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{
    CancelGroup,
    time::{Duration, Instant},
    tokio,
    tokio::task,
};
use kithara_probes::kithara;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{
    cmd::FetchCmd,
    downloader::DownloaderInner,
    peer::{InternalCmd, ResponseTarget, SlotEntry},
    response::{BodyStream, FetchResponse},
};

/// Transition a fetch from queued → in-flight. Increments the
/// `inflight` counter (the fact subscribers and the watchdog actually
/// observe) and notifies bus listeners with the realised
/// queue-residence time.
#[kithara::probe(request_id, wait_in_queue)]
fn start_request(
    bus: Option<&EventBus>,
    inflight: &std::sync::atomic::AtomicUsize,
    request_id: RequestId,
    wait_in_queue: Duration,
) {
    inflight.fetch_add(1, Ordering::Relaxed);
    if let Some(b) = bus {
        b.publish(DownloaderEvent::RequestStarted {
            request_id,
            wait_in_queue,
        });
    }
}

/// Record a fetch as successfully finished. Feeds the realised
/// bandwidth into ABR (the fact downstream throttling cares about) and
/// notifies subscribers.
#[kithara::probe(request_id, bytes_transferred, duration)]
fn finish_request(
    bus: Option<&EventBus>,
    abr: &AbrController,
    peer_id: AbrPeerId,
    request_id: RequestId,
    bytes_transferred: u64,
    duration: Duration,
) {
    if bytes_transferred > 0 {
        abr.record_bandwidth(
            peer_id,
            bytes_transferred,
            duration,
            BandwidthSource::Network,
        );
    }
    if let Some(b) = bus {
        b.publish(DownloaderEvent::RequestCompleted {
            request_id,
            bytes_transferred,
            duration,
            bandwidth_bps: bandwidth_bps(bytes_transferred, duration),
        });
    }
}

/// Abort a fetch and propagate the cancel reason. `was_in_flight`
/// distinguishes a mid-flight kill (a fetch task was already running)
/// from a before-start kill the [`deliver_cancelled_with_event`] path
/// performs on entries that never spawned.
#[kithara::probe(request_id, reason, bytes_transferred, was_in_flight)]
fn abort_request(
    bus: Option<&EventBus>,
    request_id: RequestId,
    reason: CancelReason,
    bytes_transferred: u64,
    was_in_flight: bool,
) {
    if let Some(bus) = bus {
        bus.publish(DownloaderEvent::RequestCancelled {
            request_id,
            reason,
            bytes_transferred,
        });
    }
    let _ = was_in_flight;
}

/// Mark a fetch as failed (network or protocol error). Tags the
/// failure as retryable or terminal so callers can decide whether to
/// re-queue.
#[kithara::probe(request_id, retryable)]
fn fail_request(bus: Option<&EventBus>, request_id: RequestId, err: &NetError, retryable: bool) {
    if let Some(bus) = bus {
        bus.publish(DownloaderEvent::RequestFailed {
            request_id,
            retryable,
            error: err.clone(),
        });
    }
}

/// Group of slot entries sharing the same cancel-token identity (epoch).
struct EpochGroup {
    cancel: CancelGroup,
    entries: Vec<SlotEntry>,
}

/// Collects slot entries, groups by epoch, executes via fire-and-forget spawn.
pub(super) struct BatchGroup {
    epochs: Vec<EpochGroup>,
}

impl BatchGroup {
    /// Build from a drain of slot entries, grouping by cancel token identity.
    pub(super) fn from_iter(entries: impl Iterator<Item = SlotEntry>) -> Self {
        let mut epochs: Vec<EpochGroup> = Vec::new();
        for entry in entries {
            let found = epochs
                .iter_mut()
                .find(|g| g.cancel.ptr_eq(&entry.cmd.cancel));
            match found {
                Some(group) => group.entries.push(entry),
                None => epochs.push(EpochGroup {
                    cancel: entry.cmd.cancel.clone(),
                    entries: vec![entry],
                }),
            }
        }
        Self { epochs }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.epochs.is_empty()
    }

    /// Process all epoch groups. Skip cancelled groups entirely.
    /// Respects `max_concurrent` — waits when at capacity. Returns the
    /// number of fetches actually spawned (cancelled cmds don't count),
    /// so [`Registry::tick`](super::registry::Registry::tick) can treat
    /// a non-zero dispatch as forward progress for the hang watchdog.
    pub(super) async fn process(self, inner: &DownloaderInner) -> usize {
        let mut dispatched: usize = 0;
        for group in self.epochs {
            if group.cancel.is_cancelled() {
                for entry in group.entries {
                    deliver_cancelled_with_event(entry.cmd, &entry.peer_cancel);
                }
                continue;
            }
            for entry in group.entries {
                if entry.cmd.cancel.is_cancelled() {
                    deliver_cancelled_with_event(entry.cmd, &entry.peer_cancel);
                    continue;
                }
                // Wait until under capacity before spawning.
                while inner.inflight.load(Ordering::Relaxed) >= inner.max_concurrent {
                    task::yield_now().await;
                }
                spawn_fetch(inner, entry.cmd, entry.peer_cancel);
                dispatched += 1;
                task::yield_now().await;
            }
        }
        dispatched
    }
}

/// Spawn an HTTP fetch task for one command.
fn spawn_fetch(inner: &DownloaderInner, internal: InternalCmd, peer_cancel: CancellationToken) {
    let client = inner.client.clone();
    let chunk_timeout = inner.chunk_timeout;
    let soft_timeout = inner.soft_timeout;
    let inflight = inner.inflight.clone();
    let fetch_waker = inner.fetch_waker.clone();
    let abr = Arc::clone(&inner.abr);
    let downloader_cancel = inner.cancel.clone();
    let peer_id = internal.peer_id;
    let request_id = internal.request_id;
    let started = Instant::now();
    let wait_in_queue = started.saturating_duration_since(internal.enqueued_at);
    let mut cmd = internal.cmd;
    let writer = cmd.writer.take();
    let on_complete_cb = cmd.on_complete.take();
    let bus = internal.bus;
    let cancel = internal.cancel.clone();
    let epoch_cancel = cmd.cancel.clone();

    start_request(bus.as_ref(), &inflight, request_id, wait_in_queue);

    task::spawn(async move {
        let result = establish(
            &client,
            chunk_timeout,
            soft_timeout,
            &cancel,
            bus.clone(),
            cmd,
            request_id,
        )
        .await;
        deliver(
            internal.response,
            result,
            writer,
            on_complete_cb,
            abr,
            peer_id,
            started,
            request_id,
            bus,
            &peer_cancel,
            epoch_cancel.as_ref(),
            &downloader_cancel,
        )
        .await;
        inflight.fetch_sub(1, Ordering::Relaxed);
        fetch_waker.wake();
    });
}

/// Race `fut` against a `soft_timeout` timer. When the timer wins, publish
/// [`DownloaderEvent::LoadSlow`] on `bus` (if any) and keep waiting for
/// `fut` to complete. Does not abort the underlying request.
async fn with_soft_timeout<F, T>(
    fut: F,
    soft: Duration,
    bus: Option<&EventBus>,
    request_id: RequestId,
) -> T
where
    F: Future<Output = T>,
{
    tokio::pin!(fut);
    let started = Instant::now();
    tokio::select! {
        r = &mut fut => r,
        () = tokio::time::sleep(soft) => {
            if let Some(bus) = bus {
                bus.publish(DownloaderEvent::LoadSlow {
                    request_id,
                    elapsed: started.elapsed(),
                });
            }
            fut.await
        }
    }
}

/// Establish an HTTP connection and return a [`FetchResponse`].
async fn establish(
    client: &HttpClient,
    chunk_timeout: Duration,
    soft_timeout: Duration,
    cancel: &CancelGroup,
    bus: Option<EventBus>,
    cmd: FetchCmd,
    request_id: RequestId,
) -> Result<FetchResponse, NetError> {
    let FetchCmd {
        method,
        url,
        range,
        headers,
        validator,
        ..
    } = cmd;

    if method == RequestMethod::Head {
        let resp_headers = tokio::select! {
            () = cancel.cancelled() => return Err(NetError::Cancelled),
            r = with_soft_timeout(client.head(url, headers), soft_timeout, bus.as_ref(), request_id) => r?,
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
        r = with_soft_timeout(fetch, soft_timeout, bus.as_ref(), request_id) => r?,
    };

    if let Some(validate) = validator
        && let Err(e) = validate(&byte_stream.headers)
    {
        warn!(url = %fetch_url, error = %e, "fetch rejected by response validator");
        return Err(e);
    }

    let resp_headers = byte_stream.headers.clone();
    let body = BodyStream::from_http(byte_stream, cancel.clone(), chunk_timeout);
    Ok(FetchResponse {
        body,
        headers: resp_headers,
    })
}

/// Compute pre-rounded bandwidth in bps. Guards against zero duration
/// (cache hits / instant responses) so subscribers don't repeat the
/// math or hit a div-by-zero.
fn bandwidth_bps(bytes: u64, duration: Duration) -> u64 {
    // `u128 → u64` is safe in practice — durations longer than
    // 2^64 ms (≈ 580M years) cannot occur for a single fetch.
    let ms = u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    bytes.saturating_mul(8_000) / ms
}

/// Determine why a fetch was cancelled.
///
/// Order of checks reflects priority: peer-cancel implies the whole
/// peer is going away; epoch-cancel is a per-fetch invalidation;
/// downloader-shutdown is the global stop. `BeforeStart` catches the
/// race where the cancel token was set before any fetch task ran.
fn classify_cancel(
    peer_cancel: &CancellationToken,
    epoch_cancel: Option<&CancellationToken>,
    downloader_cancel: &CancellationToken,
) -> CancelReason {
    if peer_cancel.is_cancelled() {
        CancelReason::PeerCancel
    } else if epoch_cancel.is_some_and(CancellationToken::is_cancelled) {
        CancelReason::EpochCancel
    } else if downloader_cancel.is_cancelled() {
        CancelReason::DownloaderShutdown
    } else {
        CancelReason::BeforeStart
    }
}

/// Route a fetch result to its target and publish the matching
/// `DownloaderEvent` on `bus` (if any).
#[expect(clippy::too_many_arguments, reason = "delivery needs full context")]
async fn deliver(
    target: ResponseTarget,
    result: Result<FetchResponse, NetError>,
    mut writer: Option<super::cmd::WriterFn>,
    on_complete_cb: Option<super::cmd::OnCompleteFn>,
    abr: Arc<AbrController>,
    peer_id: AbrPeerId,
    started: Instant,
    request_id: RequestId,
    bus: Option<EventBus>,
    peer_cancel: &CancellationToken,
    epoch_cancel: Option<&CancellationToken>,
    downloader_cancel: &CancellationToken,
) {
    match target {
        ResponseTarget::Channel(tx) => {
            let _ = tx.send(result);
        }
        ResponseTarget::Streaming => match result {
            Ok(resp) => {
                if let Some(ref mut w) = writer {
                    let write_result = resp.body.write_all(|chunk| w(chunk)).await;
                    let elapsed = started.elapsed();
                    match write_result {
                        Ok(total) => {
                            finish_request(bus.as_ref(), &abr, peer_id, request_id, total, elapsed);
                            if let Some(cb) = on_complete_cb {
                                cb(total, None);
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
                                cb(0, Some(e));
                            }
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
                    cb(0, Some(e));
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
    peer_cancel: &CancellationToken,
    epoch_cancel: Option<&CancellationToken>,
    downloader_cancel: &CancellationToken,
) {
    if matches!(err, NetError::Cancelled) {
        let reason = classify_cancel(peer_cancel, epoch_cancel, downloader_cancel);
        abort_request(bus, request_id, reason, bytes_transferred, true);
    } else {
        let retryable = err.is_retryable();
        fail_request(bus, request_id, err, retryable);
    }
}

/// Cancel an [`InternalCmd`] before it ever spawned a task. Publishes
/// `RequestCancelled { reason: BeforeStart }` (or whichever token is
/// already cancelled — the classifier takes care of that) on the
/// command's bus.
///
/// Public to siblings (used by [`Registry::reschedule`] when a peer
/// went away) and by [`BatchGroup::process`] for early-cancel paths.
pub(super) fn deliver_cancelled_with_event(internal: InternalCmd, peer_cancel: &CancellationToken) {
    let request_id = internal.request_id;
    let bus = internal.bus.clone();
    // Reuse the same classifier; `epoch_cancel` lives on the cmd until
    // it's torn apart in `deliver_cancelled` below.
    let epoch_cancel = internal.cmd.cancel.clone();
    let reason = classify_cancel(
        peer_cancel,
        epoch_cancel.as_ref(),
        // Without an inner-cancel reference here we treat "no
        // peer or epoch" as `BeforeStart` — same outcome the
        // classifier would land on.
        &CancellationToken::new(),
    );
    abort_request(bus.as_ref(), request_id, reason, 0, false);
    deliver_cancelled(internal.response, internal.cmd);
}

/// Route a cancellation to its target. Does NOT publish events — use
/// [`deliver_cancelled_with_event`] for that.
pub(super) fn deliver_cancelled(target: ResponseTarget, mut cmd: FetchCmd) {
    let err = NetError::Cancelled;
    match target {
        ResponseTarget::Channel(tx) => {
            let _ = tx.send(Err(err));
        }
        ResponseTarget::Streaming => {
            if let Some(cb) = cmd.on_complete.take() {
                cb(0, Some(&err));
            }
        }
    }
}
