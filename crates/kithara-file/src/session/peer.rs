use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use kithara_abr::Abr;
use kithara_events::{FileError, FileEvent};
use kithara_net::{Headers, NetError, RangeSpec};
use kithara_storage::ResourceExt;
use kithara_stream::{
    AudioCodec,
    dl::{FetchCmd, Peer, RequestPriority, reject_html_response},
};

use super::inner::{FileInner, FilePhase};

/// File-track Peer: gap-driven downloader over a single remote file.
///
/// Each `poll_next` issues at most one fetch and remains pending until
/// the previous fetch finalises. On finalise the peer re-polls,
/// inspects the resource's remaining gap and emits a Range GET for the
/// next missing range — so partial / truncated transfers resume
/// transparently. Returns `Ready(None)` only once the resource has no
/// gaps left.
pub(crate) struct FilePeer {
    /// `true` while a fetch issued by this peer is still in flight.
    /// Cleared from the `on_complete` callback before the Downloader
    /// re-polls.
    inflight: Arc<AtomicBool>,
    inner: Arc<FileInner>,
}

impl FilePeer {
    pub(crate) fn new(inner: Arc<FileInner>) -> Self {
        Self {
            inner,
            inflight: Arc::new(AtomicBool::new(false)),
        }
    }

    fn build_fetch_cmd(&self, resume_from: u64) -> FetchCmd {
        let url = self.inner.asset.url.clone();
        let headers = self.inner.asset.headers.clone();
        let cancel = self.inner.source.cancel.clone();

        let resource = self.inner.asset.res.clone();
        let coord_writer = Arc::clone(&self.inner.source.coord);
        let offset = Arc::new(AtomicU64::new(resume_from));
        let writer_offset = Arc::clone(&offset);
        let writer = Box::new(move |chunk: &[u8]| -> io::Result<()> {
            let pos = writer_offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            resource.write_at(pos, chunk).map_err(io::Error::other)?;
            coord_writer.set_download_pos(pos + chunk.len() as u64);
            Ok(())
        });

        let inner_for_resp = Arc::clone(&self.inner);
        let on_response = Box::new(move |headers: &Headers| {
            inner_for_resp.capture_content_metadata(headers, resume_from);
        });

        let inner = Arc::clone(&self.inner);
        let inflight = Arc::clone(&self.inflight);
        let cb_offset = Arc::clone(&offset);
        let on_complete = Box::new(
            move |_reported_total: u64, _headers: Option<&Headers>, err: Option<&NetError>| {
                let written = cb_offset
                    .load(Ordering::Relaxed)
                    .saturating_sub(resume_from);
                inner.finalize_fetch(resume_from, written, err);
                inflight.store(false, Ordering::Release);
            },
        );

        FetchCmd::get(url)
            .cancel(cancel)
            .writer(writer)
            .validator(reject_html_response)
            .on_response(on_response)
            .maybe_range((resume_from > 0).then(|| RangeSpec::new(resume_from, None)))
            .maybe_headers(headers)
            .on_complete(on_complete)
            .build()
    }
}

impl Abr for FilePeer {}

impl Peer for FilePeer {
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        if self.inflight.load(Ordering::Acquire) {
            return Poll::Pending;
        }
        if !matches!(
            self.inner.asset.res.status(),
            kithara_storage::ResourceStatus::Active
                | kithara_storage::ResourceStatus::Committed { .. }
        ) {
            return Poll::Ready(None);
        }
        let Some(gap_start) = self.inner.next_gap_start() else {
            return Poll::Ready(None);
        };
        self.inflight.store(true, Ordering::Release);
        self.inner.set_phase(FilePhase::Downloading);
        Poll::Ready(Some(vec![self.build_fetch_cmd(gap_start)]))
    }

    fn priority(&self) -> RequestPriority {
        if self.inner.source.coord.timeline().is_playing() {
            RequestPriority::High
        } else {
            RequestPriority::Low
        }
    }
}

impl FileInner {
    /// Pull `Content-Length` and `Content-Type` out of the response
    /// headers and seed the coord / codec hint. Both lookups try the
    /// lower-cased header first (per HTTP/2 RFC) and fall back to the
    /// title-cased form from older HTTP/1.1 servers.
    ///
    /// `resume_from` is the byte offset our Range request started at:
    /// on a `206 Partial Content` response, `Content-Length` describes
    /// the partial body length, so the resource's full size is
    /// `resume_from + content_length`.
    fn capture_content_metadata(&self, headers: &Headers, resume_from: u64) {
        let content_length = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok());
        if let Some(len) = content_length {
            self.source.coord.set_total_bytes(Some(resume_from + len));
        }
        let codec = headers
            .get("content-type")
            .or_else(|| headers.get("Content-Type"))
            .and_then(AudioCodec::parse_mime);
        if let Some(c) = codec {
            let _ = self.content_type_codec.set(c);
        }
    }

    /// Apply the result of a streaming fetch to the resource.
    ///
    /// * commits the resource once the full byte space is covered
    /// * on a transient error with bytes already received, leaves the
    ///   resource active so the peer's next `poll_next` issues a
    ///   Range GET for the remaining gap
    /// * on a terminal error (non-retryable, or hard failure with no
    ///   bytes received), fails+evicts the resource and publishes a
    ///   `FileEvent::Error`
    ///
    /// Header capture (`Content-Length` / `Content-Type`) happens
    /// eagerly in `on_response`, not here, so a reader blocked on the
    /// first byte sees the seeded coord the instant `write_at` fires.
    fn finalize_fetch(&self, resume_from: u64, bytes_written: u64, err: Option<&NetError>) {
        if let Some(e) = err {
            let terminal = !e.is_retryable() || (resume_from == 0 && bytes_written == 0);
            if terminal {
                let msg = e.to_string();
                self.fail_and_evict(&msg);
                self.source.bus.publish(FileEvent::Error {
                    error: FileError::Io(msg),
                });
            }
            return;
        }

        if self.next_gap_start().is_some() {
            return;
        }

        let final_len = self
            .source
            .coord
            .total_bytes()
            .unwrap_or(resume_from + bytes_written);
        match self.asset.res.commit(Some(final_len)) {
            Ok(()) => self.set_phase(FilePhase::Complete),
            Err(e) => {
                let msg = e.to_string();
                self.fail_and_evict(&msg);
                self.source.bus.publish(FileEvent::Error {
                    error: FileError::Io(msg),
                });
            }
        }
    }

    /// Start of the next missing byte range on this resource, or
    /// `None` when the resource is fully covered. Upper bound is the
    /// known total — committed length (when reactivating a partial)
    /// or the discovered `Content-Length` (after the first response
    /// headers seed `coord.total_bytes`). Without either, falls back
    /// to `u64::MAX` so the gap walker scans the whole space.
    fn next_gap_start(&self) -> Option<u64> {
        let upper = self
            .asset
            .res
            .len()
            .or_else(|| self.source.coord.total_bytes())
            .unwrap_or(u64::MAX);
        self.asset.res.next_gap(0, upper).map(|gap| gap.start)
    }
}
