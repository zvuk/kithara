//! Stream-based audio source with format change detection.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_bufpool::PcmPool;
use kithara_decode::{Decoder, PcmChunk, PcmSpec};
use kithara_stream::{Fetch, MediaInfo, Stream, StreamType};
use parking_lot::Mutex;
use tracing::{debug, trace, warn};

use super::worker::{AudioCommand, AudioWorkerSource, apply_effects, flush_effects, reset_effects};
use crate::{events::AudioEvent, traits::AudioEffect};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in Arc<Mutex> to allow:
/// - Decoder to read via Read + Seek
/// - StreamAudioSource to check media_info() for format changes
///
/// Supports a read boundary: when set, reads at or past the boundary
/// return 0 (EOF). This prevents the old decoder from reading data
/// from a new segment after an ABR variant switch.
pub(super) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
    /// Byte offset at which reads stop (return EOF).
    /// `u64::MAX` means no boundary (unlimited reads).
    boundary: Arc<AtomicU64>,
}

impl<T: StreamType> SharedStream<T> {
    pub(super) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
            boundary: Arc::new(AtomicU64::new(u64::MAX)),
        }
    }

    /// Set read boundary at the given byte offset.
    /// Reads at or past this offset will return 0 (EOF).
    fn set_boundary(&self, offset: u64) {
        self.boundary.store(offset, Ordering::Release);
    }

    /// Clear read boundary (allow unlimited reads).
    fn clear_boundary(&self) {
        self.boundary.store(u64::MAX, Ordering::Release);
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner.lock().media_info()
    }

    fn current_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.inner.lock().current_segment_range()
    }
}

impl<T: StreamType> Clone for SharedStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            boundary: Arc::clone(&self.boundary),
        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let boundary = self.boundary.load(Ordering::Acquire);
        let mut stream = self.inner.lock();

        if boundary < u64::MAX {
            let pos = stream.position();
            if pos >= boundary {
                return Ok(0);
            }
            let remaining = (boundary - pos) as usize;
            if remaining < buf.len() {
                return stream.read(&mut buf[..remaining]);
            }
        }

        stream.read(buf)
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
pub(super) struct OffsetReader<T: StreamType> {
    shared: SharedStream<T>,
    base_offset: u64,
}

impl<T: StreamType> OffsetReader<T> {
    fn new(shared: SharedStream<T>, base_offset: u64) -> Self {
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

/// Audio source for Stream with format change detection.
///
/// Monitors media_info changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(super) struct StreamAudioSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    decoder: Decoder,
    cached_media_info: Option<MediaInfo>,
    /// Pending format change: (new MediaInfo, byte offset where new segment starts).
    pending_format_change: Option<(MediaInfo, u64)>,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    last_spec: Option<PcmSpec>,
    emit: Option<Box<dyn Fn(AudioEvent) + Send>>,
    pcm_pool: PcmPool,
    effects: Vec<Box<dyn AudioEffect>>,
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(super) fn new(
        shared_stream: SharedStream<T>,
        decoder: Decoder,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        pcm_pool: PcmPool,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
        Self {
            shared_stream,
            decoder,
            cached_media_info: initial_media_info,
            pending_format_change: None,
            epoch,
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
            pcm_pool,
            effects,
        }
    }

    pub(super) fn with_emit(mut self, emit: Box<dyn Fn(AudioEvent) + Send>) -> Self {
        self.emit = Some(emit);
        self
    }

    /// Detect media_info change: mark as pending and set read boundary.
    ///
    /// The boundary prevents the old decoder from reading past the new
    /// segment start. This causes Symphonia to hit EOF naturally, after
    /// which `fetch_next` recreates the decoder for the new format.
    fn detect_format_change(&mut self) {
        if self.pending_format_change.is_some() {
            return;
        }
        let Some(current_info) = self.shared_stream.media_info() else {
            return;
        };
        if Some(&current_info) != self.cached_media_info.as_ref()
            && let Some(seg_range) = self.shared_stream.current_segment_range()
        {
            debug!(
                old = ?self.cached_media_info,
                new = ?current_info,
                segment_start = seg_range.start,
                "Format change detected, setting read boundary"
            );
            self.shared_stream.set_boundary(seg_range.start);
            self.pending_format_change = Some((current_info, seg_range.start));
        }
    }

    /// Apply pending format change: clear boundary, seek to segment start, recreate decoder.
    /// Returns true if decoder was recreated successfully.
    fn apply_format_change(&mut self) -> bool {
        let Some((new_info, target_offset)) = self.pending_format_change.take() else {
            return false;
        };

        debug!(
            target_offset,
            "Applying format change: clearing boundary, seeking to segment start"
        );

        // Clear boundary so the new decoder can read freely.
        self.shared_stream.clear_boundary();

        if let Err(e) = self.shared_stream.seek(SeekFrom::Start(target_offset)) {
            warn!(?e, target_offset, "Failed to seek to segment boundary");
            return false;
        }

        self.recreate_decoder(new_info, target_offset)
    }

    /// Recreate decoder with new MediaInfo using OffsetReader.
    ///
    /// OffsetReader translates Symphonia's 0-based positions to real stream
    /// positions (base_offset + X). This is needed because Symphonia internally
    /// tracks byte positions and may seek to them. Without the offset,
    /// Symphonia would seek to absolute position 0 instead of base_offset.
    fn recreate_decoder(&mut self, new_info: MediaInfo, base_offset: u64) -> bool {
        debug!(
            old = ?self.cached_media_info,
            new = ?new_info,
            base_offset,
            "Recreating decoder for new format"
        );

        self.cached_media_info = Some(new_info.clone());

        // Use OffsetReader so Symphonia's internal positions are correctly mapped
        let offset_reader = OffsetReader::new(self.shared_stream.clone(), base_offset);
        match Decoder::new_from_media_info(offset_reader, &new_info, self.pcm_pool.clone()) {
            Ok(new_decoder) => {
                self.decoder = new_decoder;
                debug!("Decoder recreated successfully");
                true
            }
            Err(e) => {
                warn!(?e, "Failed to recreate decoder, trying probe fallback");
                let offset_reader = OffsetReader::new(self.shared_stream.clone(), base_offset);
                match Decoder::new_with_probe(offset_reader, None, self.pcm_pool.clone()) {
                    Ok(new_decoder) => {
                        self.decoder = new_decoder;
                        debug!("Decoder recreated with probe");
                        true
                    }
                    Err(e) => {
                        warn!(?e, "Failed to recreate decoder with probe");
                        false
                    }
                }
            }
        }
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk<f32>;
    type Command = AudioCommand;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        let current_epoch = self.epoch.load(Ordering::Acquire);

        loop {
            self.detect_format_change();

            match self.decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    if chunk.pcm.is_empty() {
                        continue;
                    }

                    self.chunks_decoded += 1;
                    self.total_samples += chunk.pcm.len() as u64;

                    // Emit FormatDetected on first chunk
                    if self.chunks_decoded == 1
                        && let Some(ref emit) = self.emit
                    {
                        emit(AudioEvent::FormatDetected { spec: chunk.spec });
                        self.last_spec = Some(chunk.spec);
                    }

                    // Detect spec change (e.g. after ABR switch)
                    if let Some(old_spec) = self.last_spec
                        && old_spec != chunk.spec
                    {
                        if let Some(ref emit) = self.emit {
                            emit(AudioEvent::FormatChanged {
                                old: old_spec,
                                new: chunk.spec,
                            });
                        }
                        self.last_spec = Some(chunk.spec);
                    }

                    self.detect_format_change();

                    if self.chunks_decoded.is_multiple_of(100) {
                        trace!(
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            spec = ?chunk.spec,
                            epoch = current_epoch,
                            "decode progress"
                        );
                    }

                    // Apply effects chain
                    match apply_effects(&mut self.effects, chunk) {
                        Some(processed) => {
                            return Fetch::new(processed, false, current_epoch);
                        }
                        None => continue,
                    }
                }
                Ok(None) => {
                    self.detect_format_change();
                    if self.pending_format_change.is_some() {
                        debug!("Decoder EOF at format boundary, recreating");
                        if self.apply_format_change() {
                            continue;
                        }
                    }

                    debug!(
                        chunks = self.chunks_decoded,
                        samples = self.total_samples,
                        epoch = current_epoch,
                        "decode complete (EOF)"
                    );

                    // Flush effects at end of stream
                    if let Some(flushed) = flush_effects(&mut self.effects) {
                        if let Some(ref emit) = self.emit {
                            emit(AudioEvent::EndOfStream);
                        }
                        return Fetch::new(flushed, false, current_epoch);
                    }

                    if let Some(ref emit) = self.emit {
                        emit(AudioEvent::EndOfStream);
                    }
                    return Fetch::new(PcmChunk::default(), true, current_epoch);
                }
                Err(e) => {
                    warn!(?e, "decode error, signaling EOF");
                    return Fetch::new(PcmChunk::default(), true, current_epoch);
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Self::Command) {
        match cmd {
            AudioCommand::Seek { position, epoch } => {
                debug!(?position, epoch, "seek command received");
                self.epoch.store(epoch, Ordering::Release);
                if let Err(e) = self.decoder.seek(position) {
                    warn!(?e, "seek failed");
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
