use std::{num::NonZeroU64, ops::Range};

use kithara_bufpool::HasPool;
use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    ReaderInput, ReaderProfile, SourceError, SourceSeekAnchor, StreamError, StreamResult,
};
use tracing::{debug, trace};

use super::HlsVariant;
use crate::HlsError;

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct VariantReaderPreparation {
    input: ReaderInput,
    #[field(get, vis = "pub(crate)", copy)]
    anchor: SourceSeekAnchor,
    first_segment: u32,
    landing_segment: u32,
    read_ahead: u64,
    warmup: u64,
}

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Last segment an inactive session may fetch while it is being built.
    ///
    /// Covers the planned forward window and the first segment; the header is
    /// fetched separately before an init-bearing reader is transferred.
    pub(crate) fn construction_segment_end(&self, preparation: &VariantReaderPreparation) -> u32 {
        self.demand_segment_at_offset(self.forward_window(preparation).end.saturating_sub(1))
            .unwrap_or(preparation.first_segment)
            .max(preparation.first_segment)
    }

    /// Media window planned while the decoder is being constructed.
    ///
    /// Named by *segment*, resolved to bytes on every call. Segment lengths
    /// start as placeholders and are replaced by the committed ones, which moves
    /// every byte-to-segment boundary — most visibly when the init segment
    /// lands, since its real length re-keys everything after it. A window frozen
    /// in placeholder bytes at plan time stops describing the segments it was
    /// built from: readiness then waits on bytes that now belong elsewhere and
    /// never becomes true, so the reader is never handed over.
    fn forward_window(&self, preparation: &VariantReaderPreparation) -> Range<u64> {
        let landing = self
            .segment_byte_offset(preparation.landing_segment)
            .unwrap_or_default();
        let start = landing.saturating_sub(preparation.warmup);
        let end = landing.saturating_add(preparation.read_ahead);
        let end = self.stream_len().map_or(end, |len| end.min(len));
        start..end.max(start)
    }

    /// Segment the plan must carry so a reader can parse its container header,
    /// or `None` when the plan owes none.
    ///
    /// [`reader_is_ready`](Self::reader_is_ready) gates the handover on the
    /// header range, and a variant without an
    /// `EXT-X-MAP` keeps that header in its first segment — behind the tail the
    /// plan is aimed at, where nothing else would fetch it. A separate init is
    /// carried by [`PlannedFetch::Init`](crate::segment::PlannedFetch::Init)
    /// already, and a self-framing reader waits on no header at all.
    fn header_segment(&self, input: ReaderInput) -> StreamResult<Option<u32>> {
        if !matches!(input, ReaderInput::InitOnly) || self.has_init() {
            return Ok(None);
        }
        Ok(self.demand_segment_at_offset(self.header_byte_range()?.start))
    }

    /// The plan covers one segment behind the landing regardless of container: a demuxer parks at
    /// the packet boundary at or before the landing time, so when the landing is a segment start,
    /// the first packet read begins in the prior segment.
    pub(crate) fn prepare_reader(
        &self,
        profile: ReaderProfile,
        content_time: Duration,
    ) -> StreamResult<VariantReaderPreparation> {
        self.reset_to_full_range();
        let landing = self.descriptor_at_time(content_time).ok_or_else(|| {
            StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "reader landing not found: variant={} target_ms={}",
                    self.variant,
                    content_time.as_millis()
                ))
                .into(),
            )
        })?;
        let landing_byte = landing.byte_range.start;
        let warmup_start = profile.warmup().max_bytes().map_or(landing_byte, |bytes| {
            landing_byte.saturating_sub(bytes.get())
        });
        let landing_segment = landing.segment_index;
        let forward_segment = self.demand_segment_at_offset(warmup_start).ok_or_else(|| {
            StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "reader warmup not mapped: variant={} byte={warmup_start}",
                    self.variant
                ))
                .into(),
            )
        })?;

        self.set_prefetch_anchor(warmup_start);
        let tail_start = forward_segment.saturating_sub(1);
        let header_segment = self.header_segment(profile.input())?;
        self.set_segment_aware_seek_tail(tail_start);
        if !self.queue_matches_plan(tail_start)
            || header_segment.is_some_and(|seg| !self.segment_loaded(seg))
        {
            self.rebuild_queue(tail_start, header_segment);
        }

        let anchor = SourceSeekAnchor::builder()
            .byte_offset(landing_byte)
            .segment_start(landing.decode_time)
            .segment_end(landing.decode_time.saturating_add(landing.duration))
            .segment_index(landing.segment_index)
            .variant_index(landing.variant_index)
            .build();

        debug!(
            variant = self.variant,
            content_time_ms = content_time.as_millis(),
            segment_idx = landing_segment,
            bytes = landing_byte,
            "variant reader landed"
        );
        Ok(VariantReaderPreparation {
            anchor,
            landing_segment,
            first_segment: forward_segment,
            read_ahead: profile.read_ahead_bytes().get(),
            warmup: profile.warmup().max_bytes().map_or(0, NonZeroU64::get),
            input: profile.input(),
        })
    }

    pub(crate) fn reader_is_ready(
        &self,
        preparation: &VariantReaderPreparation,
    ) -> StreamResult<bool> {
        let ready = match preparation.input {
            ReaderInput::InitOnly => {
                let header = self.header_byte_range()?;
                !header.is_empty() && self.reader_range_is_ready(header)?
            }
            ReaderInput::Incremental => true,
            _ => {
                return Err(StreamError::Source(SourceError::Other(
                    "unsupported decoder reader input".into(),
                )));
            }
        };
        trace!(
            variant = self.variant,
            ready,
            ceiling = self.construction_segment_end(preparation),
            init_loaded = self.init().is_some_and(|i| i.state().is_loaded()),
            init_downloading = self.init().is_some_and(|i| i.state().is_downloading()),
            init_slow = self.init().is_some_and(|i| i.state().is_slow()),
            init_failed = self.init().is_some_and(|i| i.state().is_failed()),
            queued = self.flow.queue.lock().len(),
            "variant reader readiness"
        );
        Ok(ready)
    }

    fn reader_range_is_ready(&self, range: Range<u64>) -> StreamResult<bool> {
        match self.poll_range(range) {
            Ok(WaitOutcome::Ready | WaitOutcome::Eof) => Ok(true),
            Ok(WaitOutcome::Interrupted)
            | Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
