//! Stream-based audio source with format change detection.

use std::{
    any::Any,
    io::{Read, Seek, SeekFrom},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use fallible_iterator::FallibleIterator;
use kithara_decode::{DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmSpec};
use kithara_events::{AudioEvent, SeekLifecycleStage};
use kithara_platform::Mutex;
use kithara_stream::{Fetch, MediaInfo, SourceSeekAnchor, Stream, StreamType, Timeline};
use tracing::{debug, trace, warn};

use crate::{
    pipeline::worker::{
        AudioCommand, AudioWorkerSource, apply_effects, flush_effects, reset_effects,
    },
    traits::AudioEffect,
};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(crate) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
        }
    }

    fn position(&self) -> u64 {
        self.inner.lock().position()
    }

    pub(crate) fn len(&self) -> Option<u64> {
        self.inner.lock().len()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner.lock().media_info()
    }

    fn current_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.inner.lock().current_segment_range()
    }

    fn format_change_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.inner.lock().format_change_segment_range()
    }

    pub(crate) fn clear_variant_fence(&self) {
        self.inner.lock().clear_variant_fence();
    }

    pub(crate) fn set_seek_epoch(&self, seek_epoch: u64) {
        self.inner.lock().set_seek_epoch(seek_epoch);
    }

    fn seek_time_anchor(
        &self,
        position: Duration,
    ) -> Result<Option<SourceSeekAnchor>, std::io::Error> {
        self.inner.lock().seek_time_anchor(position)
    }

    /// Build a `StreamContext` from the inner stream's source.
    pub(crate) fn build_stream_context(&self) -> Arc<dyn kithara_stream::StreamContext> {
        let stream = self.inner.lock();
        T::build_stream_context(stream.source(), stream.timeline())
    }

    /// Get the shared timeline for flushing checks.
    pub(crate) fn timeline(&self) -> Timeline {
        self.inner.lock().timeline()
    }

    /// Create a lock-free callback for waking blocked `wait_range()`.
    ///
    /// Called once during `Audio::new()` (before the worker starts),
    /// so the inner mutex lock is safe. The returned closure captures
    /// only the condvar/notify primitive — it never takes the inner
    /// mutex, preventing deadlock when called from `Audio::seek()`
    /// while the worker holds the lock inside `read()`.
    pub(crate) fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        self.inner.lock().make_notify_fn()
    }
}

impl<T: StreamType> Clone for SharedStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.lock().read(buf)
    }
}

impl<T: StreamType> Seek for SharedStream<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.lock().seek(pos)
    }
}

/// Reader that offsets all positions by a base offset.
///
/// When Symphonia seeks to position X, the real stream position is `base_offset + X`.
/// This is needed when recreating a decoder after ABR variant switch:
/// the new segment starts at `base_offset` in the virtual stream, but Symphonia
/// expects positions starting from 0.
pub(crate) struct OffsetReader<T: StreamType> {
    shared: SharedStream<T>,
    base_offset: u64,
}

impl<T: StreamType> OffsetReader<T> {
    pub(crate) fn new(mut shared: SharedStream<T>, base_offset: u64) -> Self {
        // Ensure the stream is positioned at base_offset so reads start from
        // the correct location. This is critical when multiple fallback attempts
        // share the same underlying stream — each one may leave the position
        // in an arbitrary state.
        let _ = shared.seek(SeekFrom::Start(base_offset));
        Self {
            shared,
            base_offset,
        }
    }
}

impl<T: StreamType> Read for OffsetReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.shared.read(buf)
    }
}

impl<T: StreamType> Seek for OffsetReader<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(p) => {
                let real_pos = self.shared.seek(SeekFrom::Start(self.base_offset + p))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::Current(delta) => {
                let real_pos = self.shared.seek(SeekFrom::Current(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::End(delta) => {
                let real_pos = self.shared.seek(SeekFrom::End(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
        }
    }
}

/// Factory closure that creates a new decoder from stream, media info, and base offset.
///
/// Production: creates Symphonia `Decoder` via [`OffsetReader`].
/// Tests: returns `MockDecoder` without real I/O.
pub(crate) type DecoderFactory<T> =
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn InnerDecoder>> + Send>;

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(crate) struct StreamAudioSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    decoder: Box<dyn InnerDecoder>,
    decoder_factory: DecoderFactory<T>,
    pub(crate) cached_media_info: Option<MediaInfo>,
    /// Pending format change: (new `MediaInfo`, byte offset where new segment starts).
    pub(crate) pending_format_change: Option<(MediaInfo, u64)>,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    last_spec: Option<PcmSpec>,
    emit: Option<Box<dyn Fn(AudioEvent) + Send>>,
    effects: Vec<Box<dyn AudioEffect>>,
    /// Base offset of current decoder in the virtual stream.
    /// Used to adjust `update_byte_len` after ABR variant switch:
    /// symphonia sees `total_len - base_offset` as byte length.
    pub(crate) base_offset: u64,
    pub(crate) pending_decode_started_epoch: Option<u64>,
    pending_seek_skip: Option<(u64, Duration)>,
    pub(crate) pending_seek_recover_target: Option<(u64, Duration)>,
    pub(crate) pending_seek_recover_attempts: u8,
    /// Cached timeline for lock-free flushing checks.
    pub(crate) timeline: Timeline,
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(crate) fn new(
        shared_stream: SharedStream<T>,
        decoder: Box<dyn InnerDecoder>,
        decoder_factory: DecoderFactory<T>,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
        let timeline = shared_stream.timeline();
        Self {
            shared_stream,
            decoder,
            decoder_factory,
            cached_media_info: initial_media_info,
            pending_format_change: None,
            epoch,
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
            effects,
            base_offset: 0,
            pending_decode_started_epoch: None,
            pending_seek_skip: None,
            pending_seek_recover_target: None,
            pending_seek_recover_attempts: 0,
            timeline,
        }
    }

    pub(crate) fn with_emit(mut self, emit: Box<dyn Fn(AudioEvent) + Send>) -> Self {
        self.emit = Some(emit);
        self
    }

    /// Detect `media_info` change: mark as pending.
    ///
    /// The variant fence in `Source::read_at()` prevents the old decoder
    /// from reading data from a new variant. This causes Symphonia to hit
    /// EOF naturally, after which `fetch_next` recreates the decoder.
    fn detect_format_change(&mut self) {
        if self.pending_format_change.is_some() {
            return;
        }
        let Some(current_info) = self.shared_stream.media_info() else {
            return;
        };
        let codec_changed = self
            .cached_media_info
            .as_ref()
            .is_some_and(|cached| cached.codec != current_info.codec);
        if !codec_changed {
            return;
        }

        // Prefer format_change_segment_range() which returns the FIRST segment
        // of the new format (where init data lives). Fall back to current_segment_range()
        // if the source doesn't support format_change_segment_range().
        let seg_range = self
            .shared_stream
            .format_change_segment_range()
            .or_else(|| self.shared_stream.current_segment_range());

        if let Some(seg_range) = seg_range {
            let current_pos = self.shared_stream.position();
            let remaining_bytes = seg_range.start.saturating_sub(current_pos);

            debug!(
                old = ?self.cached_media_info,
                new = ?current_info,
                current_pos,
                new_segment_start = seg_range.start,
                remaining_bytes,
                "Format change detected"
            );
            self.pending_format_change = Some((current_info, seg_range.start));
        }
    }

    /// Apply pending format change: clear fence, seek to segment start, recreate decoder.
    /// Returns true if decoder was recreated successfully.
    fn apply_format_change(&mut self) -> bool {
        let Some((new_info, target_offset)) = self.pending_format_change.take() else {
            return false;
        };

        let current_pos = self.shared_stream.position();
        debug!(
            current_pos,
            target_offset,
            chunks_decoded = self.chunks_decoded,
            total_samples = self.total_samples,
            "Applying format change: old decoder finished, seeking to new segment start"
        );

        // Clear variant fence so the new decoder can read the new variant.
        self.shared_stream.clear_variant_fence();

        if let Err(e) = self.shared_stream.seek(SeekFrom::Start(target_offset)) {
            warn!(?e, target_offset, "Failed to seek to segment boundary");
            return false;
        }

        self.recreate_decoder(&new_info, target_offset)
    }

    /// Track chunk statistics and emit format events.
    fn track_chunk(&mut self, chunk: &PcmChunk) {
        self.chunks_decoded += 1;
        self.total_samples += chunk.pcm.len() as u64;

        // Emit FormatDetected on first chunk
        if self.chunks_decoded == 1
            && let Some(ref emit) = self.emit
        {
            emit(AudioEvent::FormatDetected { spec: chunk.spec() });
            self.last_spec = Some(chunk.spec());
        }

        // Detect spec change (e.g. after ABR switch)
        if let Some(old_spec) = self.last_spec
            && old_spec != chunk.spec()
        {
            self.emit_event(AudioEvent::FormatChanged {
                old: old_spec,
                new: chunk.spec(),
            });
            self.last_spec = Some(chunk.spec());
        }
    }

    /// Emit an audio event if the callback is set.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit(event);
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors AudioEvent::SeekLifecycle fields"
    )]
    fn emit_seek_lifecycle(
        &self,
        stage: SeekLifecycleStage,
        seek_epoch: u64,
        task_id: u64,
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
    ) {
        self.emit_event(AudioEvent::SeekLifecycle {
            stage,
            seek_epoch,
            task_id,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
        });
    }

    fn seek_context(&self) -> (Option<usize>, Option<u32>, Option<u64>, Option<u64>) {
        let stream_ctx = self.shared_stream.build_stream_context();
        let segment_range = self.shared_stream.current_segment_range();
        (
            stream_ctx.variant_index(),
            stream_ctx.segment_index(),
            segment_range.as_ref().map(|range| range.start),
            segment_range.as_ref().map(|range| range.end),
        )
    }

    fn decode_panic_message(payload: Box<dyn Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(msg) => *msg,
            Err(payload) => payload.downcast::<&'static str>().map_or_else(
                |_| "unknown panic payload".to_string(),
                |msg| (*msg).to_string(),
            ),
        }
    }

    fn decoder_seek_safe(&mut self, position: Duration) -> DecodeResult<()> {
        match catch_unwind(AssertUnwindSafe(|| self.decoder.seek(position))) {
            Ok(result) => result,
            Err(payload) => Err(DecodeError::InvalidData(format!(
                "decoder panic during seek: {}",
                Self::decode_panic_message(payload)
            ))),
        }
    }

    fn decoder_next_chunk_safe(&mut self) -> DecodeResult<Option<PcmChunk>> {
        match catch_unwind(AssertUnwindSafe(|| self.decoder.next_chunk())) {
            Ok(result) => result,
            Err(payload) => Err(DecodeError::InvalidData(format!(
                "decoder panic during next_chunk: {}",
                Self::decode_panic_message(payload)
            ))),
        }
    }

    /// Check if decode error occurred near a format boundary and recover.
    ///
    /// Returns true if the decoder was recreated at the boundary.
    fn try_recover_at_boundary(&mut self) -> bool {
        if let Some((_, target_offset)) = &self.pending_format_change {
            let current_pos = self.shared_stream.position();
            let remaining = target_offset.saturating_sub(current_pos);

            if remaining < 1024 * 1024 {
                debug!(
                    chunks = self.chunks_decoded,
                    samples = self.total_samples,
                    current_pos,
                    target_offset = *target_offset,
                    remaining,
                    "Decoder error at format boundary, recreating decoder"
                );
                if self.apply_format_change() {
                    return true;
                }
            } else {
                debug!(
                    current_pos,
                    target_offset = *target_offset,
                    remaining,
                    "Decoder error far from format boundary, not switching"
                );
            }
        }
        false
    }

    /// Recreate decoder with new `MediaInfo` via factory.
    ///
    /// The factory handles `OffsetReader` creation and decoder instantiation.
    /// Returns true if decoder was recreated successfully.
    fn recreate_decoder(&mut self, new_info: &MediaInfo, base_offset: u64) -> bool {
        debug!(
            old = ?self.cached_media_info,
            new = ?new_info,
            base_offset,
            "Recreating decoder for new format"
        );

        self.cached_media_info = Some(new_info.clone());
        self.base_offset = base_offset;

        if let Some(new_decoder) =
            (self.decoder_factory)(self.shared_stream.clone(), new_info, base_offset)
        {
            let new_duration = new_decoder.duration();
            self.decoder = new_decoder;
            debug!(?new_duration, base_offset, "Decoder recreated successfully");
            true
        } else {
            warn!(base_offset, "Failed to recreate decoder");
            false
        }
    }

    fn apply_seek_applied(
        &mut self,
        epoch: u64,
        position: Duration,
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
    ) {
        reset_effects(&mut self.effects);
        self.emit_seek_lifecycle(
            SeekLifecycleStage::SeekApplied,
            epoch,
            epoch,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
        );
        self.pending_decode_started_epoch = Some(epoch);
        self.pending_seek_recover_target = Some((epoch, position));
        self.pending_seek_recover_attempts = 0;
    }

    fn apply_seek_from_decoder(
        &mut self,
        epoch: u64,
        position: Duration,
        preferred_recreate_offset: Option<u64>,
        allow_direct_seek: bool,
    ) -> bool {
        if allow_direct_seek {
            let stream_pos = self.shared_stream.position();
            let segment_range = self.shared_stream.current_segment_range();

            self.update_decoder_len_for_seek();

            debug!(
                ?position,
                epoch,
                stream_pos,
                ?segment_range,
                base_offset = self.base_offset,
                "seek: about to call decoder.seek()"
            );
            if let Err(err) = self.decoder_seek_safe(position) {
                warn!(?err, "seek failed");
            } else {
                let (variant, segment_index, byte_range_start, byte_range_end) =
                    self.seek_context();
                self.apply_seek_applied(
                    epoch,
                    position,
                    variant,
                    segment_index,
                    byte_range_start,
                    byte_range_end,
                );
                return true;
            }
        }

        if self.recover_seek_after_failed_seek(position, preferred_recreate_offset) {
            let (variant, segment_index, byte_range_start, byte_range_end) = self.seek_context();
            self.apply_seek_applied(
                epoch,
                position,
                variant,
                segment_index,
                byte_range_start,
                byte_range_end,
            );
            return true;
        }

        false
    }

    fn apply_time_anchor_seek(
        &mut self,
        position: Duration,
        epoch: u64,
        anchor: SourceSeekAnchor,
    ) -> bool {
        self.shared_stream.clear_variant_fence();
        debug!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: starting decoder seek to segment start"
        );
        if let Err(err) = self.decoder_seek_safe(anchor.segment_start) {
            warn!(
                ?err,
                ?position,
                anchor_start = ?anchor.segment_start,
                "seek anchor path: decoder seek to segment start failed"
            );
            return false;
        }
        debug!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: decoder seek to segment start succeeded"
        );

        let relative = position.saturating_sub(anchor.segment_start);
        self.pending_seek_skip = if relative.is_zero() {
            None
        } else {
            Some((epoch, relative))
        };

        self.apply_seek_applied(
            epoch,
            position,
            anchor.variant_index,
            anchor.segment_index,
            Some(anchor.byte_offset),
            None,
        );
        true
    }

    fn apply_anchor_seek_with_fallback(
        &mut self,
        epoch: u64,
        position: Duration,
        anchor: SourceSeekAnchor,
    ) -> bool {
        if !self.align_decoder_with_seek_anchor(anchor) {
            warn!("seek anchor path: decoder alignment failed, falling back to direct seek");
            return self.apply_seek_from_decoder(epoch, position, Some(anchor.byte_offset), true);
        }

        if self.apply_time_anchor_seek(position, epoch, anchor) {
            return true;
        }

        warn!("seek anchor path failed, falling back to direct seek");
        self.apply_seek_from_decoder(epoch, position, Some(anchor.byte_offset), true)
    }

    fn align_decoder_with_seek_anchor(&mut self, anchor: SourceSeekAnchor) -> bool {
        let current_codec = self.cached_media_info.as_ref().and_then(|info| info.codec);
        let target_info = self.shared_stream.media_info();
        let target_codec = target_info.as_ref().and_then(|info| info.codec);
        let current_variant = self
            .cached_media_info
            .as_ref()
            .and_then(|info| info.variant_index);
        let target_variant = target_info.as_ref().and_then(|info| info.variant_index);

        // Seek must only rebuild decoder on codec change.
        // Variant-only seeks keep the current decoder state.
        let codec_changed =
            matches!((current_codec, target_codec), (Some(from), Some(to)) if from != to);
        let variant_changed =
            matches!((current_variant, target_variant), (Some(from), Some(to)) if from != to);
        debug!(
            ?current_codec,
            ?target_codec,
            ?current_variant,
            ?target_variant,
            codec_changed,
            variant_changed,
            "seek anchor alignment: compare format"
        );
        if !codec_changed {
            return true;
        }

        let Some(target_info) = target_info else {
            warn!(
                ?current_codec,
                ?current_variant,
                target_variant = ?anchor.variant_index,
                target_offset = anchor.byte_offset,
                "seek anchor alignment: codec changed but media info unavailable"
            );
            return false;
        };

        let recreate_offset = self
            .shared_stream
            .format_change_segment_range()
            .map_or(anchor.byte_offset, |range| range.start);

        self.shared_stream.clear_variant_fence();
        if let Err(err) = self.shared_stream.seek(SeekFrom::Start(recreate_offset)) {
            warn!(
                ?err,
                target_offset = recreate_offset,
                "seek anchor alignment: failed to seek stream"
            );
            return false;
        }

        if !self.recreate_decoder(&target_info, recreate_offset) {
            warn!(
                target_offset = recreate_offset,
                "seek anchor alignment: decoder recreation failed"
            );
            return false;
        }

        self.pending_format_change = None;
        true
    }

    fn decoder_recreate_offset(&self, preferred: Option<u64>) -> u64 {
        // Recovery seeks must start from an init-bearing segment when available.
        // For segmented formats (notably fMP4), recreating from a media-only
        // offset often fails ("missing ftyp"/probe failure).
        self.shared_stream
            .format_change_segment_range()
            .map(|range| range.start)
            .or(preferred)
            .or_else(|| {
                self.shared_stream
                    .current_segment_range()
                    .map(|range| range.start)
            })
            .unwrap_or(self.base_offset)
    }

    fn recreate_decoder_for_seek(
        &mut self,
        media_info: &MediaInfo,
        recreate_offset: u64,
        seek_position: Duration,
        log_context: &'static str,
    ) -> bool {
        self.shared_stream.clear_variant_fence();
        if let Err(err) = self.shared_stream.seek(SeekFrom::Start(recreate_offset)) {
            warn!(
                ?err,
                recreate_offset,
                ?seek_position,
                "{log_context}: failed to seek stream for decoder recreate"
            );
            return false;
        }

        if !self.recreate_decoder(media_info, recreate_offset) {
            warn!(
                recreate_offset,
                ?seek_position,
                "{log_context}: decoder recreate failed"
            );
            return false;
        }

        match self.decoder_seek_safe(seek_position) {
            Ok(()) => {
                self.pending_format_change = None;
                debug!(
                    recreate_offset,
                    ?seek_position,
                    "{log_context}: decoder recreated and seek retry succeeded"
                );
                true
            }
            Err(err) => {
                warn!(
                    ?err,
                    recreate_offset,
                    ?seek_position,
                    "{log_context}: decoder recreated but seek retry failed"
                );
                false
            }
        }
    }

    fn recover_seek_after_failed_seek(
        &mut self,
        seek_position: Duration,
        preferred_recreate_offset: Option<u64>,
    ) -> bool {
        if self.pending_format_change.is_some() {
            debug!("seek failed during pending format change, applying now");
            if self.apply_format_change() && self.decoder_seek_safe(seek_position).is_ok() {
                return true;
            }
        }

        let Some(media_info) = self
            .shared_stream
            .media_info()
            .or_else(|| self.cached_media_info.clone())
        else {
            warn!(
                ?seek_position,
                "seek failed: no media info for decoder recovery"
            );
            return false;
        };

        let recreate_offset = self.decoder_recreate_offset(preferred_recreate_offset);
        self.recreate_decoder_for_seek(
            &media_info,
            recreate_offset,
            seek_position,
            "seek failed recovery",
        )
    }

    fn update_decoder_len_for_seek(&self) {
        // Only update byte_len for original decoder (no ABR switch).
        // After ABR switch (base_offset > 0), byte_len is intentionally 0
        // to prevent mismatch between byte_len and moov duration.
        // Symphonia uses moof seek index for fMP4, not byte_len.
        if self.base_offset == 0
            && let Some(len) = self.shared_stream.len()
            && len > 0
        {
            self.decoder.update_byte_len(len);
        }
    }

    fn duration_for_frames(spec: PcmSpec, frames: usize) -> Duration {
        if spec.sample_rate == 0 {
            return Duration::ZERO;
        }
        let nanos = (frames as u128)
            .saturating_mul(1_000_000_000)
            .saturating_div(u128::from(spec.sample_rate));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to u64::MAX before cast"
        )]
        {
            Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
        }
    }

    fn frames_for_duration(spec: PcmSpec, duration: Duration) -> usize {
        if spec.sample_rate == 0 {
            return 0;
        }
        let frames = duration
            .as_nanos()
            .saturating_mul(u128::from(spec.sample_rate))
            .saturating_div(1_000_000_000);
        frames.min(usize::MAX as u128) as usize
    }

    fn apply_seek_skip(&mut self, epoch: u64, mut chunk: PcmChunk) -> Option<PcmChunk> {
        let Some((skip_epoch, remaining)) = self.pending_seek_skip else {
            return Some(chunk);
        };
        if skip_epoch != epoch {
            self.pending_seek_skip = None;
            return Some(chunk);
        }
        if remaining.is_zero() {
            self.pending_seek_skip = None;
            return Some(chunk);
        }

        let spec = chunk.spec();
        let channels = usize::from(spec.channels.max(1));
        let chunk_frames = chunk.frames();
        if chunk_frames == 0 {
            return None;
        }

        let mut drop_frames = Self::frames_for_duration(spec, remaining);
        if drop_frames == 0 {
            drop_frames = 1;
        }

        if drop_frames >= chunk_frames {
            let dropped = Self::duration_for_frames(spec, chunk_frames);
            let next_remaining = remaining.saturating_sub(dropped);
            self.pending_seek_skip = if next_remaining.is_zero() {
                None
            } else {
                Some((skip_epoch, next_remaining))
            };
            return None;
        }

        let drop_samples = drop_frames.saturating_mul(channels);
        let len = chunk.pcm.len();
        chunk.pcm.copy_within(drop_samples..len, 0);
        chunk.pcm.truncate(len - drop_samples);

        chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(drop_frames as u64);
        chunk.meta.timestamp = chunk
            .meta
            .timestamp
            .saturating_add(Self::duration_for_frames(spec, drop_frames));
        self.pending_seek_skip = None;
        Some(chunk)
    }

    pub(crate) fn retry_decode_error_after_seek(&mut self) -> bool {
        let Some(seek_epoch) = self.pending_decode_started_epoch else {
            return false;
        };
        let Some((target_epoch, target)) = self.pending_seek_recover_target else {
            return false;
        };
        if target_epoch != seek_epoch {
            return false;
        }

        let retry_pos = if let Some((skip_epoch, remaining)) = self.pending_seek_skip {
            if skip_epoch == seek_epoch {
                target.saturating_sub(remaining)
            } else {
                target
            }
        } else {
            target
        };

        // First retry: lightweight decoder.seek() on the same decoder.
        // If error repeats in the same seek epoch, escalate to decoder recreate.
        if self.pending_seek_recover_attempts == 0 {
            self.pending_seek_recover_attempts = 1;
            match self.decoder_seek_safe(retry_pos) {
                Ok(()) => {
                    debug!(
                        epoch = seek_epoch,
                        ?retry_pos,
                        "decode error right after seek: one-shot decoder.seek retry succeeded"
                    );
                    return true;
                }
                Err(err) => {
                    warn!(
                        ?err,
                        epoch = seek_epoch,
                        ?retry_pos,
                        "decode error right after seek: one-shot decoder.seek retry failed"
                    );
                }
            }
        }

        self.pending_seek_recover_target = None;
        self.pending_seek_recover_attempts = 0;

        let Some(new_info) = self
            .shared_stream
            .media_info()
            .or_else(|| self.cached_media_info.clone())
        else {
            warn!(
                epoch = seek_epoch,
                ?retry_pos,
                "decode error right after seek: no media info for decoder recovery"
            );
            return false;
        };

        let recreate_offset = self.decoder_recreate_offset(None);
        self.recreate_decoder_for_seek(
            &new_info,
            recreate_offset,
            retry_pos,
            "decode error right after seek",
        )
    }
}

impl<T: StreamType> FallibleIterator for StreamAudioSource<T> {
    type Item = PcmChunk;
    type Error = DecodeError;

    fn next(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            self.detect_format_change();

            match self.decoder_next_chunk_safe() {
                Ok(Some(chunk)) => {
                    let current_epoch = self.epoch.load(Ordering::Acquire);
                    let Some(chunk) = self.apply_seek_skip(current_epoch, chunk) else {
                        continue;
                    };
                    if chunk.pcm.is_empty() {
                        continue;
                    }
                    self.pending_seek_recover_target = None;
                    self.pending_seek_recover_attempts = 0;

                    self.track_chunk(&chunk);
                    self.detect_format_change();

                    if self.chunks_decoded.is_multiple_of(100) {
                        trace!(
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            spec = ?chunk.spec(),
                            "decode progress"
                        );
                    }

                    match apply_effects(&mut self.effects, chunk) {
                        Some(processed) => return Ok(Some(processed)),
                        None => continue,
                    }
                }
                Ok(None) => {
                    let pos_at_eof = self.shared_stream.position();
                    self.detect_format_change();
                    if self.pending_format_change.is_some() {
                        debug!(
                            pos_at_eof,
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            "Decoder EOF at format boundary, recreating decoder"
                        );
                        if self.apply_format_change() {
                            reset_effects(&mut self.effects);
                            continue;
                        }
                    }

                    debug!(
                        chunks = self.chunks_decoded,
                        samples = self.total_samples,
                        pos_at_eof,
                        "decode complete (true EOF)"
                    );

                    if let Some(flushed) = flush_effects(&mut self.effects) {
                        self.emit_event(AudioEvent::EndOfStream);
                        return Ok(Some(flushed));
                    }

                    self.emit_event(AudioEvent::EndOfStream);
                    return Ok(None);
                }
                Err(e) => {
                    // Check if error is due to format boundary (variant fence EOF).
                    // The fence causes Ok(0) from read_at(), which Symphonia may
                    // surface as an error rather than clean EOF in some codecs.
                    //
                    // Only apply the format change if we're near the variant boundary.
                    // Far from it, the error is a genuine decode issue (e.g. corrupted
                    // DRM data), not fence-induced. Applying it prematurely would skip
                    // large portions of the current variant's audio.
                    self.detect_format_change();
                    if self.try_recover_at_boundary() {
                        reset_effects(&mut self.effects);
                        continue;
                    }

                    if self.retry_decode_error_after_seek() {
                        continue;
                    }

                    warn!(?e, "decode error, signaling EOF");
                    return Err(e);
                }
            }
        }
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;
    type Command = AudioCommand;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        if self.timeline.total_duration().is_none() {
            let duration = self.decoder.duration();
            if duration.is_some() {
                self.timeline.set_total_duration(duration);
            }
        }
        let current_epoch = self.epoch.load(Ordering::Acquire);
        match FallibleIterator::next(self) {
            Ok(Some(chunk)) => {
                if self.pending_decode_started_epoch == Some(current_epoch) {
                    let segment_range = self.shared_stream.current_segment_range();
                    self.emit_seek_lifecycle(
                        SeekLifecycleStage::DecodeStarted,
                        current_epoch,
                        current_epoch,
                        chunk.meta.variant_index,
                        chunk.meta.segment_index,
                        segment_range.as_ref().map(|range| range.start),
                        segment_range.as_ref().map(|range| range.end),
                    );
                    self.pending_decode_started_epoch = None;
                }
                Fetch::new(chunk, false, current_epoch)
            }
            Ok(None) | Err(_) => Fetch::new(PcmChunk::default(), true, current_epoch),
        }
    }

    fn handle_command(&mut self, _cmd: Self::Command) {
        // No commands left to handle. Seek flows through Timeline.
        // This match is intentionally empty; AudioCommand currently has no variants.
        // Future non-seek commands will be dispatched here.
    }

    fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    fn apply_pending_seek(&mut self) {
        let epoch = self.timeline.seek_epoch();
        let Some(position) = self.timeline.seek_target() else {
            // No target set — another thread already cleared it. Complete anyway.
            self.timeline.complete_seek(epoch);
            return;
        };

        let current_epoch = self.epoch.load(Ordering::Acquire);
        if epoch <= current_epoch {
            trace!(
                current_epoch,
                stale_epoch = epoch,
                "apply_pending_seek: dropping stale seek"
            );
            self.timeline.complete_seek(epoch);
            return;
        }

        let (variant, segment_index, byte_range_start, byte_range_end) = self.seek_context();
        self.emit_seek_lifecycle(
            SeekLifecycleStage::SeekRequest,
            epoch,
            epoch,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
        );

        self.shared_stream.set_seek_epoch(epoch);
        self.epoch.store(epoch, Ordering::Release);

        // Clear any variant fence so seek can move to any timeline
        // position, including positions in previous variants.
        self.shared_stream.clear_variant_fence();
        self.pending_seek_skip = None;

        // Decoder alignment may read from the stream (decoder recreate path).
        // Complete flush first so wait_range can block and request data.
        self.timeline.complete_seek(epoch);

        let applied = match self.shared_stream.seek_time_anchor(position) {
            Ok(Some(anchor)) => self.apply_anchor_seek_with_fallback(epoch, position, anchor),
            Ok(None) => self.apply_seek_from_decoder(epoch, position, None, true),
            Err(err) => {
                warn!(
                    ?err,
                    "seek anchor resolution failed, using direct seek path"
                );
                self.apply_seek_from_decoder(epoch, position, None, true)
            }
        };

        if !applied {
            warn!(epoch, ?position, "failed to apply pending seek");
        }
    }
}
