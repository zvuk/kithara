//! Batch execution: epoch-aware grouping and fetch spawning.

use std::sync::atomic::Ordering;

use kithara_events::{DownloaderEvent, EventBus};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{CancelGroup, time::Duration, tokio, tokio::task};
use tracing::warn;

use super::{
    cmd::{FetchCmd, FetchMethod},
    downloader::DownloaderInner,
    peer::{InternalCmd, ResponseTarget},
    response::{BodyStream, FetchResponse},
};

/// Group of commands sharing the same cancel token (epoch).
struct EpochGroup {
    cancel: CancelGroup,
    cmds: Vec<InternalCmd>,
}

/// Collects commands, groups by epoch, executes via fire-and-forget spawn.
pub(super) struct BatchGroup {
    epochs: Vec<EpochGroup>,
}

impl BatchGroup {
    /// Build from a drain of commands, grouping by cancel token identity.
    pub(super) fn from_iter(cmds: impl Iterator<Item = InternalCmd>) -> Self {
        let mut epochs: Vec<EpochGroup> = Vec::new();
        for cmd in cmds {
            let found = epochs.iter_mut().find(|g| g.cancel.ptr_eq(&cmd.cancel));
            match found {
                Some(group) => group.cmds.push(cmd),
                None => epochs.push(EpochGroup {
                    cancel: cmd.cancel.clone(),
                    cmds: vec![cmd],
                }),
            }
        }
        Self { epochs }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.epochs.is_empty()
    }

    /// Process all epoch groups. Skip cancelled groups entirely.
    /// Respects `max_concurrent` — waits when at capacity.
    pub(super) async fn process(self, inner: &DownloaderInner) {
        for group in self.epochs {
            if group.cancel.is_cancelled() {
                for cmd in group.cmds {
                    deliver_cancelled(cmd.response, cmd.cmd);
                }
                continue;
            }
            for cmd in group.cmds {
                if cmd.cancel.is_cancelled() {
                    deliver_cancelled(cmd.response, cmd.cmd);
                    continue;
                }
                // Wait until under capacity before spawning.
                while inner.inflight.load(Ordering::Relaxed) >= inner.max_concurrent {
                    task::yield_now().await;
                }
                spawn_fetch(inner, cmd);
                task::yield_now().await;
            }
        }
    }
}

/// Spawn an HTTP fetch task for one command.
fn spawn_fetch(inner: &DownloaderInner, internal: InternalCmd) {
    let client = inner.client.clone();
    let chunk_timeout = inner.chunk_timeout;
    let soft_timeout = inner.soft_timeout;
    let inflight = inner.inflight.clone();
    let fetch_waker = inner.fetch_waker.clone();
    let mut cmd = internal.cmd;
    let writer = cmd.writer.take();
    let on_complete_cb = cmd.on_complete.take();
    let bus = internal.bus;

    inflight.fetch_add(1, Ordering::Relaxed);

    task::spawn(async move {
        let result = establish(
            &client,
            chunk_timeout,
            soft_timeout,
            &internal.cancel,
            bus,
            cmd,
        )
        .await;
        deliver(internal.response, result, writer, on_complete_cb).await;
        inflight.fetch_sub(1, Ordering::Relaxed);
        fetch_waker.wake();
    });
}

/// Race `fut` against a `soft_timeout` timer. When the timer wins, publish
/// [`DownloaderEvent::LoadSlow`] on `bus` (if any) and keep waiting for
/// `fut` to complete. Does not abort the underlying request.
async fn with_soft_timeout<F, T>(fut: F, soft: Duration, bus: Option<&EventBus>) -> T
where
    F: Future<Output = T>,
{
    tokio::pin!(fut);
    tokio::select! {
        r = &mut fut => r,
        () = tokio::time::sleep(soft) => {
            if let Some(bus) = bus {
                bus.publish(DownloaderEvent::LoadSlow);
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
) -> Result<FetchResponse, NetError> {
    let FetchCmd {
        method,
        url,
        range,
        headers,
        validator,
        ..
    } = cmd;

    if method == FetchMethod::Head {
        let resp_headers = tokio::select! {
            () = cancel.cancelled() => return Err(NetError::Cancelled),
            r = with_soft_timeout(client.head(url, headers), soft_timeout, bus.as_ref()) => r?,
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
        r = with_soft_timeout(fetch, soft_timeout, bus.as_ref()) => r?,
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
        headers: resp_headers,
        body,
    })
}

/// Route a fetch result to its target.
async fn deliver(
    target: ResponseTarget,
    result: Result<FetchResponse, NetError>,
    mut writer: Option<super::cmd::WriterFn>,
    on_complete_cb: Option<super::cmd::OnCompleteFn>,
) {
    match target {
        ResponseTarget::Channel(tx) => {
            let _ = tx.send(result);
        }
        ResponseTarget::Streaming => match result {
            Ok(resp) => {
                if let Some(ref mut w) = writer {
                    let write_result = resp.body.write_all(|chunk| w(chunk)).await;
                    match write_result {
                        Ok(total) => {
                            if let Some(cb) = on_complete_cb {
                                cb(total, None);
                            }
                        }
                        Err(ref e) => {
                            if let Some(cb) = on_complete_cb {
                                cb(0, Some(e));
                            }
                        }
                    }
                }
            }
            Err(ref e) => {
                if let Some(cb) = on_complete_cb {
                    cb(0, Some(e));
                }
            }
        },
    }
}

/// Route a cancellation to its target.
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
