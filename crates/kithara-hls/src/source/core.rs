#![forbid(unsafe_code)]

use std::{io::ErrorKind, num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_assets::{AssetsError, ResourceKey};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    PendingReason, ReadOutcome, SegmentLayout, Source, SourcePhase, SourceSeekAnchor, StreamError,
    StreamResult, Timeline,
};

use crate::{
    HlsError,
    coord::HlsCoord,
    peer::HlsPeer,
    playlist::{PlaylistAccess, PlaylistState},
};

/// HLS source: provides random-access reading from loaded segments.
///
/// Reads bytes directly from the [`AssetStore`](kithara_assets::AssetStore)
/// backend — segment downloading is the Downloader's job via `poll_next`,
/// not the source's.
pub struct HlsSource {
    coord: Arc<HlsCoord>,
    playlist_state: Arc<PlaylistState>,
    hls_peer: Option<Arc<HlsPeer>>,
    peer_handle: Option<kithara_stream::dl::PeerHandle>,
    /// Reader→peer wake handle. Cloned from the owning `HlsPeer` once it
    /// is bound via [`Self::set_hls_peer`]; firing it makes the peer's
    /// `poll_next` resume on the next event loop tick.
    peer_wake: Option<Arc<Notify>>,
}

impl HlsSource {
    pub(crate) fn new(coord: Arc<HlsCoord>, playlist_state: Arc<PlaylistState>) -> Self {
        Self {
            coord,
            playlist_state,
            hls_peer: None,
            peer_handle: None,
            peer_wake: None,
        }
    }

    pub(crate) fn set_hls_peer(&mut self, peer: Arc<HlsPeer>) {
        self.peer_wake = Some(peer.reader_wake());
        self.hls_peer = Some(peer);
    }

    pub(crate) fn set_peer_handle(&mut self, handle: kithara_stream::dl::PeerHandle) {
        self.peer_handle = Some(handle);
    }

    fn wake_peer(&self) {
        if let Some(ref n) = self.peer_wake {
            n.notify_one();
        }
    }

    /// Locate the segment covering `byte_offset` and resolve the init /
    /// media resource split. Segment 0 of an fMP4 variant is the only
    /// place where `init_len > 0`; for raw TS/AAC variants and for
    /// segments >= 1 the init portion is empty and reads go straight to
    /// the media resource.
    fn segment_resource_key(&self, byte_offset: u64) -> Option<SegmentRead> {
        let (seg_idx, seg_byte_offset, seg_size) = self.coord.find_at_offset(byte_offset)?;
        let active = self.coord.active()?;
        let media_key = active.segment_resource(seg_idx)?;
        let init = (seg_idx == 0)
            .then(|| active.init_resource().map(|key| (key, active.init_size())))
            .flatten();
        Some(SegmentRead {
            media_key,
            init,
            segment_byte_offset: seg_byte_offset,
            segment_size: seg_size,
        })
    }

    fn range_ready(&self, range: &Range<u64>) -> bool {
        let Some(seg) = self.segment_resource_key(range.start) else {
            return false;
        };
        let seg_end = seg.segment_byte_offset + seg.segment_size;
        let read_end = range.end.min(seg_end);
        let local_start = range.start.saturating_sub(seg.segment_byte_offset);
        let local_end = read_end.saturating_sub(seg.segment_byte_offset);
        if local_start >= local_end {
            return true;
        }
        seg.split(local_start, local_end).iter().all(|chunk| {
            self.coord
                .asset_store
                .contains_range(chunk.key, chunk.range.clone())
        })
    }
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        if let Some(ref peer) = self.hls_peer {
            peer.teardown();
        }
    }
}

struct SegmentRead {
    media_key: ResourceKey,
    /// Init resource paired with its prefix size in segment-local bytes;
    /// `None` when the segment has no init prefix.
    init: Option<(ResourceKey, u64)>,
    segment_byte_offset: u64,
    segment_size: u64,
}

/// One slice of a segment-local range mapped onto a single resource.
/// `range` is local to the resource (offset 0 = start of init or media).
struct ResourceSlice<'a> {
    key: &'a ResourceKey,
    range: Range<u64>,
}

impl SegmentRead {
    /// Split a segment-local range `[local_start, local_end)` into the
    /// init prefix and media body slices. Returns one or two entries —
    /// `local_end <= init_len` yields init only, `local_start >= init_len`
    /// yields media only, the rest yields both.
    fn split(&self, local_start: u64, local_end: u64) -> Vec<ResourceSlice<'_>> {
        let mut out = Vec::with_capacity(2);
        if let Some((ref init_key, init_len)) = self.init {
            let init_cut = local_end.min(init_len);
            if local_start < init_cut {
                out.push(ResourceSlice {
                    key: init_key,
                    range: local_start..init_cut,
                });
            }
            if local_end > init_len {
                let media_start = local_start.saturating_sub(init_len);
                let media_end = local_end - init_len;
                out.push(ResourceSlice {
                    key: &self.media_key,
                    range: media_start..media_end,
                });
            }
        } else {
            out.push(ResourceSlice {
                key: &self.media_key,
                range: local_start..local_end,
            });
        }
        out
    }
}

impl Source for HlsSource {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.peer_handle.as_ref().map(|h| h.abr().clone())
    }

    fn as_segment_layout(&self) -> Option<Arc<dyn SegmentLayout>> {
        Some(self.coord.segment_view() as Arc<dyn SegmentLayout>)
    }

    fn len(&self) -> Option<u64> {
        let total = self.coord.total_bytes();
        (total > 0).then_some(total)
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let notify = self.peer_wake.clone()?;
        Some(Box::new(move || notify.notify_one()))
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        self.coord.active()?.header_byte_range()
    }

    fn notify_waiting(&self) {
        self.wake_peer();
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.coord.cancel.is_cancelled() {
            return SourcePhase::Cancelled;
        }
        if self.range_ready(&range) {
            return SourcePhase::Ready;
        }
        if self.coord.timeline.is_flushing() {
            return SourcePhase::Seeking;
        }
        let total = self.coord.total_bytes();
        if total > 0 && range.start >= total {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let total = self.coord.total_bytes();
        if total > 0 && offset >= total {
            return Ok(ReadOutcome::Eof);
        }
        let Some(seg) = self.segment_resource_key(offset) else {
            return Ok(ReadOutcome::Pending(PendingReason::Retry));
        };
        let local_offset = offset.saturating_sub(seg.segment_byte_offset);
        let buf_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let local_end = (local_offset + buf_len).min(seg.segment_size);
        let mut written: usize = 0;
        for chunk in seg.split(local_offset, local_end) {
            let take = usize::try_from(chunk.range.end - chunk.range.start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            let resource = match self.coord.asset_store.open_resource(chunk.key) {
                Ok(res) => res,
                Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => break,
                Err(e) => return Err(StreamError::Source(HlsError::from(e).into())),
            };
            let n = resource
                .read_at(chunk.range.start, dst)
                .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
            written += n;
            if n < take {
                break;
            }
        }
        Ok(NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        ))
    }

    fn seek_time_anchor(&mut self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        let variant = self.coord.variant_index();
        let Some((segment_index, segment_start, segment_end)) = self
            .playlist_state
            .find_seek_point_for_time(variant, position)
        else {
            return Err(StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
                .into(),
            ));
        };
        let byte_offset = self
            .coord
            .active()
            .and_then(|v| v.segment_byte_offset(segment_index.try_into().unwrap_or(u32::MAX)))
            .or_else(|| {
                self.playlist_state
                    .segment_byte_offset(variant, segment_index)
            })
            .ok_or_else(|| {
                StreamError::Source(
                    HlsError::SegmentNotFound(format!(
                        "seek offset not found: variant={variant} segment={segment_index}"
                    ))
                    .into(),
                )
            })?;
        let seg_idx_u32 = u32::try_from(segment_index).unwrap_or(u32::MAX);
        let anchor = SourceSeekAnchor::new(byte_offset, segment_start)
            .with_segment_end(segment_end)
            .with_segment_index(seg_idx_u32)
            .with_variant_index(variant);
        self.coord.set_position(byte_offset);
        self.wake_peer();
        Ok(Some(anchor))
    }

    fn set_seek_epoch(&mut self, _seek_epoch: u64) {
        self.wake_peer();
    }

    fn timeline(&self) -> Timeline {
        self.coord.timeline()
    }

    fn position(&self) -> u64 {
        self.coord.position()
    }

    fn advance(&self, n: u64) {
        self.coord.advance(n);
    }

    fn set_position(&self, pos: u64) {
        self.coord.set_position(pos);
    }

    #[kithara_hang_detector::hang_watchdog]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let started_at = Instant::now();
        loop {
            if self.coord.cancel.is_cancelled() {
                return Err(StreamError::Source(HlsError::Cancelled.into()));
            }
            if self.range_ready(&range) {
                hang_reset!();
                return Ok(WaitOutcome::Ready);
            }
            if self.coord.timeline.is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
            let total = self.coord.total_bytes();
            if total > 0 && range.start >= total {
                return Ok(WaitOutcome::Eof);
            }
            if let Some(budget) = timeout
                && started_at.elapsed() > budget
            {
                return Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()));
            }
            self.wake_peer();
            hang_tick!();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
