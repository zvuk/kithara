use std::{io::ErrorKind, ops::Range};

use kithara_assets::{AssetsError, ResourceKey};
use kithara_storage::ResourceExt;

use super::{core::HlsSource, types::ReadSegment};
use crate::{
    HlsError,
    ids::{SegmentIndex, VariantIndex},
    stream_index::StreamIndex,
};

impl HlsSource {
    pub(crate) fn range_ready_from_segments(
        &self,
        segments: &StreamIndex,
        range: &Range<u64>,
    ) -> bool {
        let Some(seg_ref) = segments.find_at_offset(range.start) else {
            tracing::trace!(
                range_start = range.start,
                layout = segments.layout_variant(),
                "range_ready: find_at_offset=None"
            );
            return false;
        };

        let seg_end = seg_ref.byte_offset + seg_ref.data.total_len();
        let range_end = range.end.min(seg_end);
        let local_start = range.start.saturating_sub(seg_ref.byte_offset);
        let local_end = range_end.saturating_sub(seg_ref.byte_offset);

        let init_end = seg_ref.data.init_len.min(local_end);
        if local_start < init_end {
            let Some(ref init_url) = seg_ref.data.init_url else {
                tracing::trace!(seg = seg_ref.segment_index, "range_ready: no init_url");
                return false;
            };
            if !self
                .backend
                .contains_range(&ResourceKey::from_url(init_url), local_start..init_end)
            {
                tracing::trace!(
                    seg = seg_ref.segment_index,
                    local_start,
                    init_end,
                    "range_ready: init not in backend"
                );
                return false;
            }
        }

        let media_start = local_start
            .max(seg_ref.data.init_len)
            .saturating_sub(seg_ref.data.init_len);
        let media_end = local_end.saturating_sub(seg_ref.data.init_len);
        if media_start < media_end
            && !self.backend.contains_range(
                &ResourceKey::from_url(&seg_ref.data.media_url),
                media_start..media_end,
            )
        {
            tracing::trace!(
                seg = seg_ref.segment_index,
                variant = seg_ref.variant,
                media_start, media_end,
                url = %seg_ref.data.media_url,
                "range_ready: media not in backend"
            );
            return false;
        }

        true
    }

    /// Read from a loaded segment.
    ///
    /// Returns `Ok(None)` when the resource was evicted from the LRU cache
    /// between `wait_range` (metadata ready) and this read attempt.
    /// The caller should convert this to `ReadOutcome::Retry`.
    pub(super) fn read_from_entry(
        &self,
        seg: &ReadSegment,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let local_offset = offset - seg.byte_offset;

        if local_offset < seg.init_len {
            let Some(ref init_url) = seg.init_url else {
                return Ok(Some(0));
            };

            let key = ResourceKey::from_url(init_url);
            let read_end = (local_offset + buf.len() as u64).min(seg.init_len);
            if !self.backend.contains_range(&key, local_offset..read_end) {
                return Ok(None);
            }
            let resource = match self.backend.open_resource(&key) {
                Ok(res) => res,
                Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            };
            resource.wait_range(local_offset..read_end)?;

            #[expect(
                clippy::cast_possible_truncation,
                reason = "init segment fits in memory"
            )]
            let available = (seg.init_len - local_offset) as usize;
            let to_read = buf.len().min(available);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read])?;

            if bytes_from_init < buf.len() && seg.media_len > 0 {
                let remaining = &mut buf[bytes_from_init..];
                Ok(self
                    .read_media_segment_checked(seg, 0, remaining)?
                    .map(|n| bytes_from_init + n))
            } else {
                Ok(Some(bytes_from_init))
            }
        } else {
            let media_offset = local_offset - seg.init_len;
            self.read_media_segment_checked(seg, media_offset, buf)
        }
    }

    pub(super) fn read_media_segment_checked(
        &self,
        seg: &ReadSegment,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let key = ResourceKey::from_url(&seg.media_url);
        let read_end = (media_offset + buf.len() as u64).min(seg.media_len);
        if !self.backend.contains_range(&key, media_offset..read_end) {
            return Ok(None);
        }
        let resource = match self.backend.open_resource(&key) {
            Ok(res) => res,
            Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(Some(bytes_read))
    }

    /// Check committed `StreamIndex` for the segment covering `range_start`.
    /// Returns `Some(segment_index)` if a request should be issued.
    pub(super) fn committed_segment_for_offset(
        &self,
        range_start: u64,
        variant: VariantIndex,
    ) -> Option<SegmentIndex> {
        self.segments
            .lock_sync()
            .find_at_offset(range_start)
            .filter(|seg_ref| seg_ref.variant == variant)
            .map(|seg_ref| seg_ref.segment_index)
    }
}
