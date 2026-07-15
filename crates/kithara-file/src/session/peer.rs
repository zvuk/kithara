use std::{
    io,
    io::Error,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::{Context, Poll},
};

use kithara_abr::Abr;
use kithara_assets::{ProducerHandle, ReadSide, WriteSide};
use kithara_events::{FileError, FileEvent, TotalBytesSource};
use kithara_net::{Headers, NetError, RangeSpec, Retryability};
use kithara_platform::sync::{Arc, Mutex};
use kithara_storage::ResourceStatus;
use kithara_stream::{
    MediaInfo,
    dl::{FetchCmd, Peer, RequestPriority, reject_html_response},
};

use crate::session::inner::{FileInner, FilePhase};

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
    /// Single-producer election handle. `Some` only while this peer is
    /// the elected producer for the shared resource. Starts as the
    /// attach-time winner (or `None` for a loser); a loser promotes
    /// itself via [`DemandLease::try_take_producer`] once the previous
    /// producer drops. When no demand lease was attached (standalone,
    /// no shared store) the peer always drives - see `poll_next`.
    producer: Mutex<Option<ProducerHandle>>,
}

impl FilePeer {
    pub(crate) fn new(inner: Arc<FileInner>, producer: Option<ProducerHandle>) -> Self {
        Self {
            inner,
            inflight: Arc::new(AtomicBool::new(false)),
            producer: Mutex::new(producer),
        }
    }

    fn build_fetch_cmd(&self, resume_from: u64) -> FetchCmd {
        let url = self.inner.asset.url.clone();
        let headers = self.inner.asset.headers.clone();
        let cancel = self.inner.source.cancel.clone();

        let raw = self.inner.asset.raw.clone();
        let coord_writer = Arc::clone(&self.inner.source.coord);
        let inner_for_write = Arc::clone(&self.inner);
        let offset = Arc::new(AtomicU64::new(resume_from));
        let writer_offset = Arc::clone(&offset);
        let writer = Box::new(move |chunk: &[u8]| -> io::Result<()> {
            let chunk_len = u64::try_from(chunk.len()).map_err(|err| {
                Error::other(format!("file chunk length does not fit u64: {err}"))
            })?;
            let pos = writer_offset.fetch_add(chunk_len, Ordering::Relaxed);
            let end = pos
                .checked_add(chunk_len)
                .ok_or_else(|| Error::other("file download offset overflow"))?;
            let Some(raw) = raw.as_ref() else {
                return Err(Error::other(
                    "file resource has no writer (already committed or read-only)",
                ));
            };
            raw.write_at(pos, chunk).map_err(Error::other)?;
            coord_writer.set_download_pos(end);
            inner_for_write.wake_worker();
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

    /// Whether this peer may issue GETs for the shared resource.
    ///
    /// Resources with no demand lease (standalone store, single
    /// consumer) always drive. With a lease, only the elected producer
    /// drives; a non-producer first tries to take over an abandoned slot
    /// (`try_take_producer`) and otherwise yields to the live producer.
    fn ensure_producer(&self) -> bool {
        let Some(lease) = self.inner.demand_lease.as_ref() else {
            return true;
        };
        let mut producer = self.producer.lock();
        if producer.is_some() {
            return true;
        }
        if let Some(handle) = lease.try_take_producer() {
            *producer = Some(handle);
            return true;
        }
        drop(producer);
        false
    }

    /// Start of the next byte range worth fetching, or `None` when the
    /// resource is terminal (neither `Active` nor `Committed`) or already
    /// fully covered. The gap walk only runs once the status check passes.
    fn next_fetchable_gap(&self) -> Option<u64> {
        matches!(
            self.inner.asset.reader.status(),
            ResourceStatus::Active | ResourceStatus::Committed { .. }
        )
        .then(|| self.inner.next_gap_start())
        .flatten()
    }
}

impl Abr for FilePeer {}

impl Peer for FilePeer {
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        if self.inflight.load(Ordering::Acquire) {
            return Poll::Pending;
        }
        let Some(gap_start) = self.next_fetchable_gap() else {
            return Poll::Ready(None);
        };
        // A gap remains. Only the elected producer issues GETs so two
        // consumers of one shared resource (e.g. player + waveform) share
        // a single download. A non-producer yields and re-polls on the
        // next downloader tick (a sibling fetch completing), which also
        // covers producer handoff after the original producer drops.
        if !self.ensure_producer() {
            return Poll::Pending;
        }
        self.inflight.store(true, Ordering::Release);
        self.inner.set_phase(FilePhase::Downloading);
        Poll::Ready(Some(vec![self.build_fetch_cmd(gap_start)]))
    }

    fn priority(&self) -> RequestPriority {
        if self.inner.source.coord.activity().is_playing() {
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
        let previous_total = self.source.coord.total_bytes();
        let content_length = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok());
        if let Some(len) = content_length {
            let total_bytes = resume_from + len;
            self.source.coord.set_total_bytes(Some(total_bytes));
            if previous_total != Some(total_bytes) {
                self.publish_total_bytes_resolved(total_bytes, TotalBytesSource::ContentLength);
            }
        }
        let info = headers
            .get("content-type")
            .or_else(|| headers.get("Content-Type"))
            .and_then(MediaInfo::parse_mime);
        if let Some(i) = info {
            let _ = self.content_type_info.set(i);
        }
        self.publish_opened(self.source.coord.total_bytes(), false, None);
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
            let terminal =
                e.retryability() == Retryability::Fatal || (resume_from == 0 && bytes_written == 0);
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
        let Some(writer) = self.take_writer() else {
            // Writer already consumed (committed by a sibling/race) — the
            // resource is final; just advance the FSM.
            self.set_phase(FilePhase::Complete);
            return;
        };
        match writer.commit(Some(final_len)) {
            Ok(_reader) => self.set_phase(FilePhase::Complete),
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
            .reader
            .len()
            .or_else(|| self.source.coord.total_bytes())
            .unwrap_or(u64::MAX);
        self.asset.reader.next_gap(0, upper).map(|gap| gap.start)
    }
}

#[cfg(test)]
mod tests {
    use kithara_assets::{
        AcquisitionResult, AssetResourceState, AssetStoreBuilder, StorageBackend,
    };
    use kithara_events::{Envelope, Event, EventBus};
    use kithara_platform::{CancelToken, sync::Arc};
    use kithara_stream::{PlayheadState, SeekState};
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::coord::FileCoord;

    fn make_inner() -> Arc<FileInner> {
        let store = Arc::new(
            AssetStoreBuilder::default()
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build(),
        );
        let key = store.scope("test").key("remote.dat");
        let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
            panic!("fresh acquire must be Pending");
        };
        let coord = Arc::new(FileCoord::new(
            Arc::new(PlayheadState::new()),
            Arc::new(SeekState::new()),
        ));
        let bus = EventBus::new(16);
        Arc::new(FileInner::new(
            crate::session::inner::FileSourceCtx {
                coord,
                cancel: CancelToken::never(),
                bus,
            },
            crate::session::inner::FileAssetCtx {
                backend: store,
                reader: writer.reader(),
                writer: Mutex::new(Some(writer)),
                headers: None,
                raw: None,
                key,
                url: Url::parse("http://127.0.0.1/test.mp3").expect("test url"),
            },
            FilePhase::Init,
            None,
        ))
    }

    #[kithara::test]
    fn remote_capture_metadata_publishes_opened() {
        let inner = make_inner();
        let mut rx = inner.source.bus.subscribe();
        let mut headers = Headers::new();
        headers.insert("content-type", "audio/mpeg");
        headers.insert("content-length", "12");

        inner.capture_content_metadata(&headers, 0);

        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: Event::File(FileEvent::TotalBytesResolved { .. }),
                ..
            })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: Event::File(FileEvent::Opened {
                    cached: false,
                    total_bytes: Some(12),
                    ..
                }),
                ..
            })
        ));
    }

    #[kithara::test]
    fn complete_phase_publishes_cache_complete() {
        let inner = make_inner();
        let mut rx = inner.source.bus.subscribe();
        inner.source.coord.set_total_bytes(Some(12));

        inner.set_phase(FilePhase::Complete);

        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: Event::File(FileEvent::CacheComplete { total_bytes: 12 }),
                ..
            })
        ));
    }

    #[kithara::test]
    fn terminal_failure_does_not_publish_cache_complete() {
        let inner = make_inner();
        let mut rx = inner.source.bus.subscribe();
        inner.source.coord.set_total_bytes(Some(12));
        inner.set_phase(FilePhase::Downloading);

        inner.fail_and_evict("fixture terminal failure");

        assert!(rx.try_recv().is_err());
        assert!(matches!(
            inner
                .asset
                .backend
                .resource_state(&inner.asset.key)
                .expect("resource state"),
            AssetResourceState::Missing
        ));
    }
}
