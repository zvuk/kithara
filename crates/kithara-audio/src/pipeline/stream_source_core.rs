//! Stream-based audio source with format change detection.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use fallible_iterator::FallibleIterator;
use kithara_decode::{DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmSpec};
use kithara_platform::Mutex;
use kithara_stream::{Fetch, MediaInfo, Stream, StreamType};
use tracing::{debug, trace, warn};

use crate::pipeline::worker::{
    AudioCommand, AudioWorkerSource, apply_effects, flush_effects, reset_effects,
};
use kithara_events::AudioEvent;

use crate::traits::AudioEffect;

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(in crate::pipeline) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(in crate::pipeline) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
        }
    }

    fn position(&self) -> u64 {
        self.inner.lock().position()
    }

    fn len(&self) -> Option<u64> {
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

    fn clear_variant_fence(&self) {
        self.inner.lock().clear_variant_fence();
    }

    /// Build a `StreamContext` from the inner stream's source.
    pub(in crate::pipeline) fn build_stream_context(
        &self,
    ) -> Arc<dyn kithara_stream::StreamContext> {
        let stream = self.inner.lock();
        T::build_stream_context(stream.source(), stream.position_handle())
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
pub(in crate::pipeline) struct OffsetReader<T: StreamType> {
    shared: SharedStream<T>,
    base_offset: u64,
}

impl<T: StreamType> OffsetReader<T> {
    pub(in crate::pipeline) fn new(mut shared: SharedStream<T>, base_offset: u64) -> Self {
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
pub(in crate::pipeline) type DecoderFactory<T> =
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn InnerDecoder>> + Send>;

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(in crate::pipeline) struct StreamAudioSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    decoder: Box<dyn InnerDecoder>,
    decoder_factory: DecoderFactory<T>,
    cached_media_info: Option<MediaInfo>,
    /// Pending format change: (new `MediaInfo`, byte offset where new segment starts).
    pending_format_change: Option<(MediaInfo, u64)>,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    last_spec: Option<PcmSpec>,
    emit: Option<Box<dyn Fn(AudioEvent) + Send>>,
    effects: Vec<Box<dyn AudioEffect>>,
    /// Base offset of current decoder in the virtual stream.
    /// Used to adjust `update_byte_len` after ABR variant switch:
    /// symphonia sees `total_len - base_offset` as byte length.
    base_offset: u64,
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(in crate::pipeline) fn new(
        shared_stream: SharedStream<T>,
        decoder: Box<dyn InnerDecoder>,
        decoder_factory: DecoderFactory<T>,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
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
        }
    }

    pub(in crate::pipeline) fn with_emit(mut self, emit: Box<dyn Fn(AudioEvent) + Send>) -> Self {
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
        if Some(&current_info) != self.cached_media_info.as_ref() {
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
}

/// Test-only accessors.
#[cfg(test)]
impl<T: StreamType> StreamAudioSource<T> {
    pub(crate) fn has_pending_format_change(&self) -> bool {
        self.pending_format_change.is_some()
    }

    pub(crate) fn current_base_offset(&self) -> u64 {
        self.base_offset
    }
}

impl<T: StreamType> FallibleIterator for StreamAudioSource<T> {
    type Item = PcmChunk;
    type Error = DecodeError;

    fn next(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            self.detect_format_change();

            match self.decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    if chunk.pcm.is_empty() {
                        continue;
                    }

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
        let current_epoch = self.epoch.load(Ordering::Acquire);
        match FallibleIterator::next(self) {
            Ok(Some(chunk)) => Fetch::new(chunk, false, current_epoch),
            Ok(None) | Err(_) => Fetch::new(PcmChunk::default(), true, current_epoch),
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "command dispatch with multiple variants"
    )]
    fn handle_command(&mut self, cmd: Self::Command) {
        match cmd {
            AudioCommand::Seek { position, epoch } => {
                let stream_pos = self.shared_stream.position();
                let segment_range = self.shared_stream.current_segment_range();

                // Seek to beginning after ABR switch: reset decoder to offset 0
                // so V0 segments are accessible. Format change detection will
                // naturally handle V0→V1 transition at the variant boundary.
                if position.is_zero() && self.base_offset > 0 {
                    debug!(
                        base_offset = self.base_offset,
                        "seek to start: resetting decoder to offset 0"
                    );
                    self.shared_stream.clear_variant_fence();
                    let probe_info = MediaInfo::new(None, None);
                    if let Some(new_decoder) =
                        (self.decoder_factory)(self.shared_stream.clone(), &probe_info, 0)
                    {
                        self.decoder = new_decoder;
                        self.base_offset = 0;
                        self.cached_media_info = None;
                        self.pending_format_change = None;
                        self.epoch.store(epoch, Ordering::Release);
                        reset_effects(&mut self.effects);
                        self.emit_event(AudioEvent::SeekComplete { position });
                        return;
                    }
                    warn!("Failed to reset decoder to offset 0, falling through to normal seek");
                }

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

                debug!(
                    ?position,
                    epoch,
                    stream_pos,
                    ?segment_range,
                    base_offset = self.base_offset,
                    "seek: about to call decoder.seek()"
                );
                self.epoch.store(epoch, Ordering::Release);
                if let Err(e) = self.decoder.seek(position) {
                    warn!(?e, "seek failed");
                    let recovered = if self.pending_format_change.is_some() {
                        // Seek failed while format change is pending (ABR switch
                        // detected but not yet applied). Apply it now and retry
                        // seek on the new decoder.
                        debug!("seek failed during pending format change, applying now");
                        self.apply_format_change() && self.decoder.seek(position).is_ok()
                    } else if self.base_offset > 0 {
                        // After ABR switch (no pending change), failed seek can
                        // corrupt Symphonia's internal state. Recreate decoder.
                        debug!("recovering from failed seek after ABR switch");
                        self.cached_media_info.clone().is_some_and(|info| {
                            self.shared_stream.clear_variant_fence();
                            self.shared_stream
                                .seek(SeekFrom::Start(self.base_offset))
                                .is_ok()
                                && self.recreate_decoder(&info, self.base_offset)
                        })
                    } else {
                        false
                    };

                    if recovered {
                        reset_effects(&mut self.effects);
                        if let Some(ref emit) = self.emit {
                            emit(AudioEvent::SeekComplete { position });
                        }
                    }
                } else {
                    reset_effects(&mut self.effects);
                    if let Some(ref emit) = self.emit {
                        emit(AudioEvent::SeekComplete { position });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "stream_source_tests.rs"]
mod stream_source_tests;
