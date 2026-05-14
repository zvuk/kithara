#![forbid(unsafe_code)]

use std::{io::ErrorKind, num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_assets::{AssetsError, ResourceKey};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    MediaInfo, PendingReason, ReadOutcome, SegmentLayout, Source, SourcePhase, SourceSeekAnchor,
    StreamError, StreamResult, Timeline,
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

    /// Init prefix descriptor: `(resource_key, range)` covering
    /// `byte_offset`, resolved against active + historical variants. The
    /// returned `range` is in the combined byte stream's virtual space,
    /// so callers translate to the local resource offset by subtracting
    /// `range.start`. `None` when the byte lies outside any variant's
    /// init prefix (typically the case for byte offsets inside media
    /// segments).
    fn init_descriptor_at(&self, byte_offset: u64) -> Option<(ResourceKey, Range<u64>)> {
        self.coord.init_descriptor_at(byte_offset)
    }

    /// Open the resource and read the requested range. Returns
    /// `Ok(None)` when the resource was evicted between `wait_range`
    /// metadata visibility and this call — the caller should treat it
    /// as "no more bytes available right now" (drives `Retry`).
    fn read_resource(
        &self,
        key: &ResourceKey,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        let resource = match self.coord.asset_store.open_resource(key) {
            Ok(res) => res,
            Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StreamError::Source(HlsError::from(e).into())),
        };
        // For DRM (`ProcessedResource`) this closes the race between
        // `inner.commit()` and `mark_ready()`. For plain resources it's
        // effectively a no-op.
        resource
            .wait_range(range.clone())
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        let n = resource
            .read_at(range.start, dst)
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        Ok(Some(n))
    }

    fn wrap(written: usize) -> ReadOutcome {
        NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        )
    }

    /// Media descriptor for the segment covering `byte_offset` —
    /// `(resource_key, segment_byte_offset, segment_size)`. Returns
    /// `None` when the offset falls in the init prefix or past the last
    /// media segment. Cross-variant aware: resolves through active +
    /// historical variants via `HlsCoord::resolve_variant`.
    fn media_descriptor(&self, byte_offset: u64) -> Option<(ResourceKey, u64, u64)> {
        let (variant, seg_idx, seg_byte_offset, seg_size) =
            self.coord.resolve_variant(byte_offset)?;
        let key = variant.segment_resource(seg_idx)?;
        Some((key, seg_byte_offset, seg_size))
    }

    fn range_ready(&self, range: &Range<u64>) -> bool {
        // Clamp to the variant's current total — for DRM the total
        // shrinks on every commit (PKCS7 strip), so a 64KiB-aligned
        // read range often extends past the last byte. Without the
        // clamp the loop below would search for a segment to cover
        // those non-existent bytes and report `false` forever.
        let total = self.coord.total_bytes();
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return true;
        }

        let mut cursor = range.start;
        // Init prefix portion (cross-variant aware).
        while let Some((ref key, init_range)) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self
                .coord
                .asset_store
                .contains_range(key, local_start..local_end)
            {
                return false;
            }
            cursor = slice_end;
            if cursor >= end {
                return true;
            }
        }
        if cursor >= end {
            return true;
        }

        // Media portion: may span multiple segments (and multiple variants
        // after an Auto switch).
        while cursor < end {
            let Some((ref key, seg_off, seg_size)) = self.media_descriptor(cursor) else {
                return false;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self
                .coord
                .asset_store
                .contains_range(key, local_start..local_end)
            {
                return false;
            }
            cursor = slice_end;
        }
        cursor >= end
    }
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        if let Some(ref peer) = self.hls_peer {
            peer.teardown();
        }
    }
}

impl Source for HlsSource {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.peer_handle.as_ref().map(|h| h.abr().clone())
    }

    fn as_segment_layout(&self) -> Option<Arc<dyn SegmentLayout>> {
        Some(Arc::clone(&self.coord) as Arc<dyn SegmentLayout>)
    }

    fn len(&self) -> Option<u64> {
        let total = self.coord.total_bytes();
        (total > 0).then_some(total)
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let notify = self.peer_wake.clone()?;
        Some(Box::new(move || notify.notify_one()))
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let variant = self.coord.variant_index();
        let codec = self.playlist_state.variant_codec(variant);
        let container = self.playlist_state.variant_container(variant);
        let variant_u32 = u32::try_from(variant).unwrap_or(u32::MAX);
        Some(MediaInfo::new(codec, container).with_variant_index(variant_u32))
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        if let Some(active) = self.coord.active()
            && let Some(range) = active.header_byte_range()
        {
            return Some(range);
        }
        // After an ABR commit the active variant has `served_from > 0`,
        // so its init prefix lives in a historical variant's territory.
        // The audio FSM still needs a virtual offset it can pass to the
        // recreated decoder for the format-change resync (fmp4 moov);
        // give it the most recent variant whose init range is still
        // mapped into virtual byte space.
        self.coord.last_header_byte_range_in_history()
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

    #[kithara_hang_detector::hang_watchdog]
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let total = self.coord.total_bytes();
        if total > 0 && offset >= total {
            return Ok(ReadOutcome::Eof);
        }

        let buf_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let mut written: usize = 0;
        let mut cursor = offset;
        let read_end = offset.saturating_add(buf_len);

        // Init prefix (cross-variant aware): walks every init region that
        // covers `cursor`. After an Auto switch the active variant's
        // init lives outside `[0, init_size)`, so this only fires for
        // bytes that fall in the most-recent activator's init range.
        while let Some((ref key, init_range)) = self.init_descriptor_at(cursor) {
            hang_tick!();
            if cursor >= init_range.end {
                break;
            }
            let slice_end = read_end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.read_resource(key, local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        return Ok(Self::wrap(written));
                    }
                    if cursor >= read_end {
                        return Ok(Self::wrap(written));
                    }
                }
                None => return Ok(Self::wrap(written)),
            }
        }

        // Media: walk segments while the read range remains. The reader
        // pulls 64KiB at a time but segments are larger, so this loop
        // usually runs once.
        while cursor < read_end {
            hang_tick!();
            let Some((key, seg_off, seg_size)) = self.media_descriptor(cursor) else {
                break;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = read_end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.read_resource(&key, local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        break;
                    }
                }
                None => break,
            }
        }

        Ok(Self::wrap(written))
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
