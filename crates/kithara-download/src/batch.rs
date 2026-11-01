use std::sync::atomic::Ordering;

use kithara_events::EventBus;
use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Duration, Instant, WallInstant},
    tokio,
    tokio::task,
};
use kithara_test_utils::kithara;

use super::{
    downloader::DownloaderInner,
    peer::{InternalCmd, SlotEntry},
    request::{
        Cancellation, DeliveryContext, RequestContext, deliver, deliver_cancelled,
        publish_cancelled,
    },
};
use crate::{DownloaderEvent, RequestId};

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

    /// Spawn the live FIFO prefix admitted by capacity, requeue its tail and cancel dead entries.
    /// Returns actual spawns as watchdog progress, allowing newly urgent work ahead of the demand tail.
    /// Probe metrics use `name = expr` so builds without `usdt` never evaluate those expressions.
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
            .config
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
    let client = inner.config.client.clone();
    let soft_timeout = inner.config.soft_timeout;
    let inflight = inner.inflight.clone();
    let fetch_waker = inner.fetch_waker.clone();
    let abr = Arc::clone(&inner.abr);
    let downloader_cancel = inner.cancel.clone();
    let peer_id = internal.peer_id;
    let request_id = internal.request_id;
    let wait_in_queue = Instant::now().saturating_duration_since(internal.enqueued_at);
    let started = WallInstant::now();
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
            let result = RequestContext::new(&client, &cancel, request_id, bus.as_ref())
                .establish(cmd)
                .await;
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
                    cancellation: Cancellation {
                        peer: &peer_cancel,
                        epoch: epoch_cancel.as_ref(),
                        downloader: &downloader_cancel,
                    },
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
        fetch_waker.wake();
    });
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
    on_slow: Option<crate::cmd::OnSlowFn>,
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
    let reason = Cancellation {
        peer: peer_cancel,
        epoch: epoch_cancel.as_ref(),
        downloader: &placeholder_inner,
    }
    .reason();
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
