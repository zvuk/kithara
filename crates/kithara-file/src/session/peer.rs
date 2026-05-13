use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use kithara_abr::Abr;
use kithara_events::{FileError, FileEvent};
use kithara_net::{Headers, NetError};
use kithara_platform::Mutex;
use kithara_storage::ResourceExt;
use kithara_stream::{
    AudioCodec,
    dl::{FetchCmd, Peer, RequestPriority, reject_html_response},
};

use super::inner::{FileInner, FilePhase};

/// File-track Peer: one full-file GET, fully driven by the
/// [`kithara_stream::dl::Downloader`].
///
/// `poll_next` yields a single `FetchCmd` on its first call (Downloader
/// pulls bytes via the `writer` closure and finalizes through
/// `on_complete`); subsequent calls return `Ready(None)` — no per-track
/// download loop on our side.
pub(crate) struct FilePeer {
    inner: Arc<FileInner>,
    started: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl FilePeer {
    pub(crate) fn new(inner: Arc<FileInner>) -> Self {
        Self {
            inner,
            started: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn build_fetch_cmd(&self) -> FetchCmd {
        let url = self.inner.asset.url.clone();
        let headers = self.inner.asset.headers.clone();
        let cancel = self.inner.source.cancel.clone();

        let resource = self.inner.asset.res.clone();
        let coord_writer = Arc::clone(&self.inner.source.coord);
        let offset = Arc::new(AtomicU64::new(0));
        let writer = {
            let resource = resource.clone();
            Box::new(move |chunk: &[u8]| -> io::Result<()> {
                let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                resource.write_at(pos, chunk).map_err(io::Error::other)?;
                coord_writer.set_download_pos(pos + chunk.len() as u64);
                Ok(())
            })
        };

        let inner = Arc::clone(&self.inner);
        let on_complete = Box::new(
            move |bytes_written: u64, headers: Option<&Headers>, err: Option<&NetError>| {
                finalize(&inner, bytes_written, headers, err);
            },
        );

        let mut cmd = FetchCmd::get(url)
            .cancel(Some(cancel))
            .writer(writer)
            .with_validator(reject_html_response);
        if let Some(h) = headers {
            cmd = cmd.headers(Some(h));
        }
        cmd.on_complete(on_complete)
    }
}

impl Abr for FilePeer {}

impl Peer for FilePeer {
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Poll::Ready(None);
        }
        let _ = Pin::new(&mut *self.waker.lock_sync()).replace(cx.waker().clone());
        self.inner.set_phase(FilePhase::Downloading);
        Poll::Ready(Some(vec![self.build_fetch_cmd()]))
    }

    fn priority(&self) -> RequestPriority {
        if self.inner.source.coord.timeline().is_playing() {
            RequestPriority::High
        } else {
            RequestPriority::Low
        }
    }
}

/// Commit / fail / publish error based on the fetch result.
fn finalize(
    inner: &Arc<FileInner>,
    bytes_written: u64,
    headers: Option<&Headers>,
    err: Option<&NetError>,
) {
    if let Some(h) = headers {
        capture_content_metadata(inner, h);
    }

    if let Some(e) = err {
        let msg = e.to_string();
        if bytes_written == 0 {
            inner.fail_and_evict(&msg);
            inner.source.bus.publish(FileEvent::Error {
                error: FileError::Io(msg),
            });
        } else {
            inner.set_phase(FilePhase::Complete);
        }
        return;
    }

    match inner.asset.res.commit(Some(bytes_written)) {
        Ok(()) => inner.set_phase(FilePhase::Complete),
        Err(e) => {
            let msg = e.to_string();
            inner.fail_and_evict(&msg);
            inner.source.bus.publish(FileEvent::Error {
                error: FileError::Io(msg),
            });
        }
    }
}

/// Pull `Content-Length` and `Content-Type` out of the response headers
/// and seed the coord / codec hint. Both lookups try the lower-cased
/// header first (per HTTP/2 RFC) and fall back to the title-cased form
/// from older HTTP/1.1 servers.
fn capture_content_metadata(inner: &Arc<FileInner>, headers: &Headers) {
    let content_length = headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = content_length {
        inner.source.coord.set_total_bytes(Some(len));
    }
    let codec = headers
        .get("content-type")
        .or_else(|| headers.get("Content-Type"))
        .and_then(AudioCodec::from_mime);
    if let Some(c) = codec {
        let _ = inner.content_type_codec.set(c);
    }
}
