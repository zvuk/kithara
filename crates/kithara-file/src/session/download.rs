//! Async download driver: full-file streaming GET and on-demand range fills.
//!
//! Public to `session/`: [`run_full_download`] and [`run_range_watcher`] are
//! spawned by `FileSource::remote`. All mutation of `FileInner` happens here;
//! the synchronous `Source` trait methods only read.

use std::{ops::Range, sync::Arc};

use futures::StreamExt;
use kithara_assets::AssetResource;
use kithara_events::{FileError, FileEvent};
use kithara_net::{Headers, RangeSpec};
use kithara_platform::{
    time::Duration,
    tokio,
    tokio::{task, time as tokio_time},
};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    AudioCodec,
    dl::{FetchCmd, PeerHandle, reject_html_response},
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::inner::{FileInner, FilePhase};
use crate::coord::FileCoord;

/// Full-file streaming download.
///
/// Builds a GET command, executes via [`PeerHandle`], streams body chunks
/// to the resource, and commits on completion.
pub(super) async fn run_full_download(
    inner: Arc<FileInner>,
    peer: PeerHandle,
    look_ahead_bytes: Option<u64>,
) {
    inner.set_phase(FilePhase::Downloading);

    let cmd = FetchCmd::get(inner.url.clone())
        .headers(inner.headers.clone())
        .with_validator(reject_html_response);

    let resp = match peer.execute(cmd).await {
        Ok(r) => r,
        Err(e) => {
            // The Downloader emits `RequestFailed` only for the
            // `Streaming` response target. `peer.execute` uses the
            // `Channel` target, so the network-level error never lands
            // on the bus from the Downloader side. Mirror it here as a
            // file-side error so subscribers of the file stream's bus
            // (UI, the html-error-cleanup test) still see a terminal
            // signal.
            let msg = e.to_string();
            inner.fail_and_evict(&msg);
            inner.bus.publish(FileEvent::Error {
                error: FileError::Io(msg),
            });
            return;
        }
    };

    // Process response headers.
    let expected_len = process_response_headers(&inner, &resp.headers);

    // Stream body to resource.
    let result = stream_body_to_resource(
        resp.body,
        &inner.res,
        &inner.coord,
        &inner.cancel,
        look_ahead_bytes,
    )
    .await;

    match result {
        Err(StreamBodyError::Write(e)) => {
            debug!(?e, "write error during full download");
            let msg = e.to_string();
            inner.fail_and_evict(&msg);
            inner.bus.publish(FileEvent::Error {
                error: FileError::Io(msg),
            });
        }
        Err(StreamBodyError::Net(e, written)) => {
            debug!("stream error during full download: {e}");
            // Only tear down the pre-allocated mmap when nothing was
            // persisted — any written bytes are still useful to the
            // reader and must not be discarded.
            if written == 0 {
                inner.fail_and_evict(&e.to_string());
            } else {
                inner.set_phase(FilePhase::Complete);
            }
            // The network-level error is already in
            // `DownloaderEvent::RequestFailed` — no `FileEvent::Error`.
        }
        Err(StreamBodyError::Cancelled) => {}
        Ok(bytes_written) => {
            commit_full_download(&inner, bytes_written, expected_len);
        }
    }
}

/// Extract content-length and content-type from response headers.
fn process_response_headers(inner: &Arc<FileInner>, headers: &Headers) -> Option<u64> {
    let expected_len = headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = expected_len {
        inner.coord.set_total_bytes(Some(len));
    }
    if let Some(codec) = headers
        .get("content-type")
        .or_else(|| headers.get("Content-Type"))
        .and_then(AudioCodec::from_mime)
    {
        let _ = inner.content_type_codec.set(codec);
    }
    expected_len
}

enum StreamBodyError {
    Write(kithara_storage::StorageError),
    Net(kithara_net::NetError, u64),
    Cancelled,
}

/// Stream body chunks into a resource, returning total bytes written.
async fn stream_body_to_resource(
    mut body: kithara_stream::dl::BodyStream,
    res: &AssetResource,
    coord: &Arc<FileCoord>,
    cancel: &CancellationToken,
    look_ahead_bytes: Option<u64>,
) -> Result<u64, StreamBodyError> {
    /// Backpressure pause when download is too far ahead of reader.
    const THROTTLE_PAUSE: Duration = Duration::from_millis(10);

    let mut written: u64 = 0;

    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(data) => {
                let pos = written;
                written += data.len() as u64;
                if let Err(e) = res.write_at(pos, &data) {
                    return Err(StreamBodyError::Write(e));
                }
                coord.set_download_pos(written);

                if let Some(limit) = look_ahead_bytes {
                    while written > 0 && written.saturating_sub(coord.read_pos()) > limit {
                        if cancel.is_cancelled() {
                            return Err(StreamBodyError::Cancelled);
                        }
                        tokio_time::sleep(THROTTLE_PAUSE).await;
                    }
                }
            }
            Err(e) => return Err(StreamBodyError::Net(e, written)),
        }
    }

    Ok(written)
}

/// Commit a completed full download.
fn commit_full_download(inner: &Arc<FileInner>, bytes_written: u64, expected_len: Option<u64>) {
    let expected = expected_len.unwrap_or(bytes_written);

    if bytes_written >= expected {
        if let Err(e) = inner.res.commit(Some(bytes_written)) {
            debug!(?e, "failed to commit file resource");
        }
        let _ = inner.coord.take_range_request();
        inner.set_phase(FilePhase::Complete);
        // Download completion is in `DownloaderEvent::RequestCompleted`;
        // reader-side `FileEvent::EndOfStream` fires when the reader
        // actually hits EOF.
    } else {
        inner.set_phase(FilePhase::Complete);
        debug!(
            bytes_written,
            expected, "partial download, resource stays active"
        );
        inner.bus.publish(FileEvent::Error {
            error: FileError::Io(format!("incomplete: {bytes_written}/{expected} bytes")),
        });
    }
}

/// Watch for on-demand range requests and spawn range downloads.
///
/// Waits on the coordinator's demand signal. When a range is
/// requested (e.g. by seek), spawns a range download task.
pub(super) async fn run_range_watcher(
    inner: Arc<FileInner>,
    peer: PeerHandle,
    coord: Arc<FileCoord>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = coord.demand_notify().notified() => {}
        }

        while let Some(range) = coord.take_range_request() {
            if inner.res.contains_range(range.clone()) {
                continue;
            }
            let task_inner = Arc::clone(&inner);
            let task_peer = peer.clone();
            task::spawn(async move {
                run_range_download(task_inner, task_peer, range).await;
            });
        }
    }
}

/// Download a byte range (on-demand seek fill).
async fn run_range_download(inner: Arc<FileInner>, peer: PeerHandle, range: Range<u64>) {
    let range_spec = RangeSpec::new(range.start, Some(range.end.saturating_sub(1)));
    let cmd = FetchCmd::get(inner.url.clone())
        .range(Some(range_spec))
        .headers(inner.headers.clone())
        .with_validator(reject_html_response);

    let resp = match peer.execute(cmd).await {
        Ok(r) => r,
        Err(e) => {
            // Network failure is in `DownloaderEvent::RequestFailed`;
            // mirror only the local resource teardown.
            if !matches!(inner.res.status(), ResourceStatus::Committed { .. }) {
                inner.res.fail(e.to_string());
            }
            return;
        }
    };

    let mut written = range.start;
    let mut body = resp.body;

    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(data) => {
                let pos = written;
                written += data.len() as u64;
                match inner.res.write_at(pos, &data) {
                    Ok(()) => {
                        inner.coord.set_download_pos(written);
                    }
                    Err(e) => {
                        // If the resource was committed (full download finished
                        // while this range request was in flight), silently
                        // succeed — data is already available.
                        if !matches!(inner.res.status(), ResourceStatus::Committed { .. }) {
                            debug!(?e, "range write error");
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                if !matches!(inner.res.status(), ResourceStatus::Committed { .. }) {
                    inner.res.fail(e.to_string());
                }
                return;
            }
        }
    }
}
