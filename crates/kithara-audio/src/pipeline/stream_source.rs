//! Stream-based audio source with format change detection.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_decode::{InnerDecoder, PcmChunk, PcmSpec};
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
    pub(super) fn new(shared: SharedStream<T>, base_offset: u64) -> Self {
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
/// Production: creates Symphonia `Decoder` via `OffsetReader`.
/// Tests: returns `MockDecoder` without real I/O.
pub(super) type DecoderFactory<T> =
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn InnerDecoder>> + Send>;

/// Audio source for Stream with format change detection.
///
/// Monitors media_info changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(super) struct StreamAudioSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    decoder: Box<dyn InnerDecoder>,
    decoder_factory: DecoderFactory<T>,
    cached_media_info: Option<MediaInfo>,
    /// Pending format change: (new MediaInfo, byte offset where new segment starts).
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
    pub(super) fn new(
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
        if Some(&current_info) != self.cached_media_info.as_ref() {
            // Prefer format_change_segment_range() which returns the FIRST segment
            // of the new format (where init data lives). Fall back to current_segment_range()
            // if the source doesn't support format_change_segment_range().
            let seg_range = self
                .shared_stream
                .format_change_segment_range()
                .or_else(|| self.shared_stream.current_segment_range());

            if let Some(seg_range) = seg_range {
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

    /// Recreate decoder with new MediaInfo via factory.
    ///
    /// The factory handles OffsetReader creation and decoder instantiation.
    /// Returns true if decoder was recreated successfully.
    fn recreate_decoder(&mut self, new_info: MediaInfo, base_offset: u64) -> bool {
        debug!(
            old = ?self.cached_media_info,
            new = ?new_info,
            base_offset,
            "Recreating decoder for new format"
        );

        self.cached_media_info = Some(new_info.clone());
        self.base_offset = base_offset;

        match (self.decoder_factory)(self.shared_stream.clone(), &new_info, base_offset) {
            Some(new_decoder) => {
                let new_duration = new_decoder.duration();
                self.decoder = new_decoder;
                debug!(?new_duration, base_offset, "Decoder recreated successfully");
                true
            }
            None => {
                warn!(base_offset, "Failed to recreate decoder");
                false
            }
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
                let stream_pos = self.shared_stream.position();
                let segment_range = self.shared_stream.current_segment_range();

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
                            self.shared_stream.clear_boundary();
                            self.shared_stream
                                .seek(SeekFrom::Start(self.base_offset))
                                .is_ok()
                                && self.recreate_decoder(info, self.base_offset)
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
mod tests {
    use std::{
        collections::VecDeque,
        future::Future,
        ops::Range,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };

    use kithara_decode::{DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmSpec};
    use kithara_storage::WaitOutcome;
    use kithara_stream::{MediaInfo, Source, Stream, StreamResult, StreamType};
    use parking_lot::Mutex;
    use tokio::sync::broadcast;

    use super::*;

    // ========================================================================
    // MockDecoder
    // ========================================================================

    struct MockDecoder {
        chunks: VecDeque<PcmChunk<f32>>,
        spec: PcmSpec,
        duration_val: Option<Duration>,
        /// Pre-configured seek results. Popped in order.
        /// When empty, seek succeeds.
        seek_results: VecDeque<DecodeResult<()>>,
        /// Shared log of seek calls for external verification.
        seek_log: Arc<Mutex<Vec<Duration>>>,
        /// Shared log of update_byte_len calls.
        byte_len_log: Arc<Mutex<Vec<u64>>>,
    }

    impl MockDecoder {
        fn new(spec: PcmSpec, chunks: Vec<PcmChunk<f32>>) -> Self {
            Self {
                chunks: VecDeque::from(chunks),
                spec,
                duration_val: None,
                seek_results: VecDeque::new(),
                seek_log: Arc::new(Mutex::new(Vec::new())),
                byte_len_log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Add a seek result that will be returned on next seek() call.
        fn push_seek_result(mut self, result: DecodeResult<()>) -> Self {
            self.seek_results.push_back(result);
            self
        }

        #[allow(dead_code)]
        fn with_duration(mut self, d: Duration) -> Self {
            self.duration_val = Some(d);
            self
        }

        fn seek_log(&self) -> Arc<Mutex<Vec<Duration>>> {
            Arc::clone(&self.seek_log)
        }
    }

    impl InnerDecoder for MockDecoder {
        fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
            Ok(self.chunks.pop_front())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
            self.seek_log.lock().push(pos);
            if let Some(result) = self.seek_results.pop_front() {
                return result;
            }
            Ok(())
        }

        fn update_byte_len(&self, len: u64) {
            self.byte_len_log.lock().push(len);
        }

        fn duration(&self) -> Option<Duration> {
            self.duration_val
        }
    }

    // ========================================================================
    // InfiniteMockDecoder — produces chunks forever until stopped
    // ========================================================================

    struct InfiniteMockDecoder {
        spec: PcmSpec,
        stop: Arc<AtomicBool>,
        seek_log: Arc<Mutex<Vec<Duration>>>,
        byte_len_log: Arc<Mutex<Vec<u64>>>,
    }

    impl InfiniteMockDecoder {
        fn new(spec: PcmSpec, stop: Arc<AtomicBool>) -> Self {
            Self {
                spec,
                stop,
                seek_log: Arc::new(Mutex::new(Vec::new())),
                byte_len_log: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl InnerDecoder for InfiniteMockDecoder {
        fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
            if self.stop.load(Ordering::Acquire) {
                return Ok(None);
            }
            Ok(Some(PcmChunk::new(self.spec, vec![0.5; 1024])))
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
            self.seek_log.lock().push(pos);
            Ok(())
        }

        fn update_byte_len(&self, len: u64) {
            self.byte_len_log.lock().push(len);
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(220))
        }
    }

    // ========================================================================
    // TestSource + TestStream
    // ========================================================================

    struct TestSourceState {
        data: Vec<u8>,
        len: Option<u64>,
        media_info: Option<MediaInfo>,
        segment_range: Option<Range<u64>>,
        /// Range of the first segment with current format (for ABR switch).
        /// Used by `format_change_segment_range()` to return where init data lives.
        format_change_range: Option<Range<u64>>,
    }

    struct TestSource {
        state: Arc<Mutex<TestSourceState>>,
    }

    impl TestSource {
        fn new(data: Vec<u8>, len: Option<u64>) -> Self {
            Self {
                state: Arc::new(Mutex::new(TestSourceState {
                    data,
                    len,
                    media_info: None,
                    segment_range: None,
                    format_change_range: None,
                })),
            }
        }

        fn state_handle(&self) -> Arc<Mutex<TestSourceState>> {
            Arc::clone(&self.state)
        }
    }

    impl Source for TestSource {
        type Item = u8;
        type Error = std::io::Error;

        fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
            Ok(WaitOutcome::Ready)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
            let state = self.state.lock();
            let offset = offset as usize;
            if offset >= state.data.len() {
                return Ok(0);
            }
            let available = &state.data[offset..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            Ok(n)
        }

        fn len(&self) -> Option<u64> {
            self.state.lock().len
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.state.lock().media_info.clone()
        }

        fn current_segment_range(&self) -> Option<Range<u64>> {
            self.state.lock().segment_range.clone()
        }

        fn format_change_segment_range(&self) -> Option<Range<u64>> {
            self.state.lock().format_change_range.clone()
        }
    }

    struct TestConfig {
        source: Option<TestSource>,
        events_tx: Option<broadcast::Sender<()>>,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                source: None,
                events_tx: None,
            }
        }
    }

    struct TestStream;

    impl StreamType for TestStream {
        type Config = TestConfig;
        type Source = TestSource;
        type Error = std::io::Error;
        type Event = ();

        fn create(
            config: Self::Config,
        ) -> impl Future<Output = Result<Self::Source, Self::Error>> + Send {
            async move {
                config
                    .source
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no source"))
            }
        }

        fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<()> {
            if let Some(ref tx) = config.events_tx {
                tx.subscribe()
            } else {
                let (tx, rx) = broadcast::channel(16);
                config.events_tx = Some(tx);
                rx
            }
        }
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    fn make_chunk(spec: PcmSpec, num_samples: usize) -> PcmChunk<f32> {
        PcmChunk::new(spec, vec![0.5; num_samples])
    }

    fn make_shared_stream(
        data: Vec<u8>,
        len: Option<u64>,
    ) -> (SharedStream<TestStream>, Arc<Mutex<TestSourceState>>) {
        let source = TestSource::new(data, len);
        let state = source.state_handle();
        let stream = Stream::<TestStream>::from_source(source);
        (SharedStream::new(stream), state)
    }

    fn make_factory(decoders: Vec<Box<dyn InnerDecoder>>) -> DecoderFactory<TestStream> {
        let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
        Box::new(move |_stream, _info, _offset| queue.lock().pop_front())
    }

    /// Factory that records every base_offset it receives.
    fn make_tracking_factory(
        decoders: Vec<Box<dyn InnerDecoder>>,
    ) -> (DecoderFactory<TestStream>, Arc<Mutex<Vec<u64>>>) {
        let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
        let offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let offsets_clone = Arc::clone(&offsets);
        let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
            offsets_clone.lock().push(offset);
            queue.lock().pop_front()
        });
        (factory, offsets)
    }

    fn make_source(
        shared: SharedStream<TestStream>,
        decoder: Box<dyn InnerDecoder>,
        factory: DecoderFactory<TestStream>,
        media_info: Option<MediaInfo>,
    ) -> StreamAudioSource<TestStream> {
        let epoch = Arc::new(AtomicU64::new(0));
        StreamAudioSource::new(shared, decoder, factory, media_info, epoch, vec![])
    }

    fn v0_spec() -> PcmSpec {
        PcmSpec {
            sample_rate: 44100,
            channels: 2,
        }
    }

    fn v3_spec() -> PcmSpec {
        PcmSpec {
            sample_rate: 96000,
            channels: 2,
        }
    }

    fn v0_info() -> MediaInfo {
        MediaInfo::default()
            .with_sample_rate(44100)
            .with_channels(2)
    }

    fn v3_info() -> MediaInfo {
        MediaInfo::default()
            .with_sample_rate(96000)
            .with_channels(2)
    }

    // ========================================================================
    // Tests
    // ========================================================================

    /// Test that ABR switch uses `format_change_segment_range()` to find init data.
    ///
    /// Production scenario (HLS ABR switch V0 AAC → V3 FLAC):
    ///
    /// Byte layout:
    ///   0..964431:        V0 segments 0-18 (AAC)
    ///   964431..1732515:  V3 segment 19 (FLAC, has ftyp + moov init data)
    ///   1732515..2476302: V3 segment 20 (FLAC, media only, NO ftyp)
    ///
    /// The reader may pass segment 19 before `detect_format_change` runs,
    /// so `current_segment_range()` would return segment 20 (no init data).
    /// Using `format_change_segment_range()` returns segment 19 where
    /// ftyp/moov lives, allowing decoder to be recreated correctly.
    #[test]
    fn apply_format_change_must_use_first_new_format_segment_offset() {
        // Use production-like offsets from the log
        const V3_SEGMENT_19_START: u64 = 964431;
        const V3_SEGMENT_20_START: u64 = 1732515;
        const V3_SEGMENT_20_END: u64 = 2476302;

        let (shared, state) = make_shared_stream(
            vec![0u8; V3_SEGMENT_20_END as usize],
            Some(V3_SEGMENT_20_END),
        );

        // V0 decoder: 4 chunks then EOF
        let v0_chunks = vec![make_chunk(v0_spec(), 1024); 4];
        let v0_decoder = MockDecoder::new(v0_spec(), v0_chunks);

        // V3 decoder the factory will create
        let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
        let v3_decoder = MockDecoder::new(v3_spec(), v3_chunks);
        let (factory, factory_offsets) = make_tracking_factory(vec![Box::new(v3_decoder)]);

        let mut source = make_source(shared, Box::new(v0_decoder), factory, Some(v0_info()));

        // Decode 1 V0 chunk
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);

        // Simulate: reader passed first V3 segment (964431..1732515)
        // and is now in segment 20 (1732515..2476302).
        // This is what happens in production: the reader reads through
        // V3 segment 19 data before detect_format_change has a chance to run.
        {
            let mut s = state.lock();
            s.media_info = Some(v3_info());
            // current_segment_range returns segment 20 — reader already past segment 19
            s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
            // format_change_segment_range returns the FIRST V3 segment where init data lives
            s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
        }

        // Decode remaining V0 chunks + trigger EOF → apply_format_change
        loop {
            let fetch = source.fetch_next();
            if fetch.is_eof {
                break;
            }
        }

        // Verify factory was called at the correct offset
        let offsets = factory_offsets.lock();
        assert_eq!(offsets.len(), 1, "Factory should have been called once");

        // FIX: code now uses format_change_segment_range() which returns the FIRST segment
        // of the new format (964431), not current_segment_range() (1732515).
        assert_eq!(
            offsets[0], V3_SEGMENT_19_START,
            "Decoder must be recreated at first V3 segment ({V3_SEGMENT_19_START}) \
             where ftyp/moov init data lives, not at current segment ({V3_SEGMENT_20_START}) \
             which has no init data and causes 'missing ftyp atom' error"
        );
    }

    #[test]
    fn basic_decode_to_eof() {
        let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
        let chunks = vec![make_chunk(v0_spec(), 1024); 3];
        let decoder = MockDecoder::new(v0_spec(), chunks);
        let factory = make_factory(vec![]);
        let mut source = make_source(shared, Box::new(decoder), factory, Some(v0_info()));

        for _ in 0..3 {
            let fetch = source.fetch_next();
            assert!(!fetch.is_eof);
            assert!(!fetch.data.pcm.is_empty());
        }

        let fetch = source.fetch_next();
        assert!(fetch.is_eof);
    }

    #[test]
    fn format_change_recreates_decoder() {
        let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
        let v0_chunks = vec![make_chunk(v0_spec(), 1024); 2];
        let v0_decoder = MockDecoder::new(v0_spec(), v0_chunks);

        let v3_chunks = vec![make_chunk(v3_spec(), 2048); 3];
        let v3_decoder = MockDecoder::new(v3_spec(), v3_chunks);
        let factory = make_factory(vec![Box::new(v3_decoder)]);

        let mut source = make_source(shared, Box::new(v0_decoder), factory, Some(v0_info()));

        // Decode 1 V0 chunk
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);

        // Trigger format change
        {
            let mut s = state.lock();
            s.media_info = Some(v3_info());
            s.segment_range = Some(1000..2000);
        }

        // Decode remaining V0 chunk — detect_format_change sets boundary
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);
        assert!(source.has_pending_format_change());

        // V0 decoder exhausted → EOF → apply_format_change → V3 decoder
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof, "Should get V3 data after format change");
        assert_eq!(fetch.data.spec, v3_spec());
    }

    #[test]
    fn seek_updates_epoch_and_calls_decoder() {
        let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
        let chunks = vec![make_chunk(v0_spec(), 1024); 5];
        let decoder = MockDecoder::new(v0_spec(), chunks);
        let seek_log = decoder.seek_log();
        let factory = make_factory(vec![]);

        let epoch = Arc::new(AtomicU64::new(0));
        let mut source = StreamAudioSource::new(
            shared,
            Box::new(decoder),
            factory,
            Some(v0_info()),
            Arc::clone(&epoch),
            vec![],
        );

        source.handle_command(AudioCommand::Seek {
            position: Duration::from_secs(10),
            epoch: 42,
        });

        assert_eq!(epoch.load(Ordering::Acquire), 42);
        let seeks = seek_log.lock();
        assert_eq!(seeks.len(), 1);
        assert_eq!(seeks[0], Duration::from_secs(10));
    }

    #[test]
    fn seek_skips_byte_len_update_after_abr_switch() {
        let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
        let chunks = vec![make_chunk(v3_spec(), 2048); 5];
        let decoder = MockDecoder::new(v3_spec(), chunks);
        let byte_len_log = Arc::clone(&decoder.byte_len_log);
        let factory = make_factory(vec![]);

        let epoch = Arc::new(AtomicU64::new(0));
        let mut source = StreamAudioSource::new(
            shared,
            Box::new(decoder),
            factory,
            Some(v3_info()),
            Arc::clone(&epoch),
            vec![],
        );

        // Simulate that ABR switch already happened
        source.base_offset = 863137;

        source.handle_command(AudioCommand::Seek {
            position: Duration::from_secs(110),
            epoch: 1,
        });

        assert!(
            byte_len_log.lock().is_empty(),
            "update_byte_len must not be called when base_offset > 0"
        );
    }

    #[test]
    fn failed_seek_after_abr_switch_recovers() {
        let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));

        let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
        let v3_decoder = MockDecoder::new(v3_spec(), v3_chunks)
            .push_seek_result(Err(DecodeError::SeekError("unexpected end of file".into())));

        let recovery_chunks = vec![make_chunk(v3_spec(), 2048); 5];
        let recovery_decoder = MockDecoder::new(v3_spec(), recovery_chunks);
        let factory = make_factory(vec![Box::new(recovery_decoder)]);

        let epoch = Arc::new(AtomicU64::new(0));
        let mut source = StreamAudioSource::new(
            shared,
            Box::new(v3_decoder),
            factory,
            Some(v3_info()),
            Arc::clone(&epoch),
            vec![],
        );

        source.base_offset = 863137;
        source.cached_media_info = Some(v3_info());

        source.handle_command(AudioCommand::Seek {
            position: Duration::from_secs(10),
            epoch: 1,
        });

        let fetch = source.fetch_next();
        assert!(!fetch.is_eof, "Should produce data after seek recovery");
    }

    /// **BASELINE TEST** — reproduces the exact production bug.
    ///
    /// Scenario from production logs (HLS ABR switch V0 AAC → V3 FLAC):
    /// 1. V0 decoder active, base_offset=0
    /// 2. Format change detected (V0→V3), boundary set, pending_format_change=Some
    /// 3. Seek command arrives BEFORE apply_format_change runs
    /// 4. Old V0 decoder seek fails (at boundary EOF)
    /// 5. base_offset==0 → existing recovery is skipped
    /// 6. Seek position is lost — V3 decoder never receives it
    ///
    /// Expected: pending format change is applied, seek retried on V3 decoder.
    #[test]
    fn seek_during_pending_format_change_retries_on_new_decoder() {
        let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

        // V0 decoder: 3 chunks, then seek will fail
        let v0_chunks = vec![make_chunk(v0_spec(), 1024); 3];
        let v0_decoder = MockDecoder::new(v0_spec(), v0_chunks)
            .push_seek_result(Err(DecodeError::SeekError("unexpected end of file".into())));

        // V3 decoder that factory will create — should receive retried seek
        let v3_chunks = vec![make_chunk(v3_spec(), 2048); 10];
        let v3_decoder = MockDecoder::new(v3_spec(), v3_chunks);
        let v3_seek_log = v3_decoder.seek_log();
        let factory = make_factory(vec![Box::new(v3_decoder)]);

        let epoch = Arc::new(AtomicU64::new(0));
        let mut source = StreamAudioSource::new(
            shared,
            Box::new(v0_decoder),
            factory,
            Some(v0_info()),
            Arc::clone(&epoch),
            vec![],
        );

        // Decode 1 V0 chunk
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);

        // Trigger format change (ABR switch V0→V3)
        {
            let mut s = state.lock();
            s.media_info = Some(v3_info());
            s.segment_range = Some(1000..2000);
        }

        // Decode another V0 chunk — detect_format_change sets boundary + pending
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);
        assert!(
            source.has_pending_format_change(),
            "Format change should be pending after detection"
        );

        // Seek arrives BEFORE format change is applied.
        // Old V0 decoder seek fails. base_offset=0.
        let seek_pos = Duration::from_secs_f64(147.48);
        source.handle_command(AudioCommand::Seek {
            position: seek_pos,
            epoch: 1,
        });

        // EXPECTED: format change was applied, V3 decoder received the seek
        assert_eq!(
            source.current_base_offset(),
            1000,
            "Pending format change should have been applied during seek recovery"
        );

        let v3_seeks = v3_seek_log.lock();
        assert_eq!(
            v3_seeks.len(),
            1,
            "V3 decoder should have received the retried seek"
        );
        assert_eq!(
            v3_seeks[0], seek_pos,
            "V3 decoder should receive seek to original position"
        );
    }

    /// **STRESS TEST** — rapid seeking for 20 seconds during ABR switch.
    ///
    /// Reproduces production scenario where user scrubs the timeline rapidly
    /// while ABR switches from V0 (AAC) to V3 (FLAC). After the old decoder
    /// is exhausted and format change is applied at the wrong offset (1732515
    /// instead of 964431), the factory fails to create a decoder ("missing ftyp").
    /// Audio dies permanently — every subsequent fetch_next returns EOF.
    ///
    /// 30-second timeout catches deadlocks.
    #[test]
    fn stress_rapid_seeks_during_abr_switch_must_not_kill_audio() {
        const V3_SEGMENT_19_START: u64 = 964431;
        const V3_SEGMENT_20_START: u64 = 1732515;
        const V3_SEGMENT_20_END: u64 = 2476302;

        let handle = std::thread::spawn(move || {
            let (shared, state) = make_shared_stream(
                vec![0u8; V3_SEGMENT_20_END as usize],
                Some(V3_SEGMENT_20_END),
            );

            // V0 decoder: produces chunks until stopped
            let v0_stop = Arc::new(AtomicBool::new(false));
            let v0_decoder = InfiniteMockDecoder::new(v0_spec(), Arc::clone(&v0_stop));

            // Factory: only succeeds at correct offset, returns None at wrong offset.
            // Mimics production: ftyp atom only at 964431, not at 1732515.
            let factory_offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
            let factory_offsets_clone = Arc::clone(&factory_offsets);
            let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
                factory_offsets_clone.lock().push(offset);
                if offset == V3_SEGMENT_19_START {
                    // Correct offset — decoder would succeed
                    Some(Box::new(InfiniteMockDecoder::new(
                        v3_spec(),
                        Arc::new(AtomicBool::new(false)),
                    )))
                } else {
                    // Wrong offset (1732515) — "missing ftyp atom" in production
                    None
                }
            });

            let mut source = make_source(shared, Box::new(v0_decoder), factory, Some(v0_info()));

            let start = Instant::now();
            let mut epoch = 0u64;
            let mut format_changed = false;
            let mut v0_stopped = false;
            let mut chunks_after_v0_stop = 0u64;
            let mut eof_after_v0_stop = 0u64;

            // Cycling through various seek positions (like rapid slider scrubbing)
            let seek_positions: &[f64] = &[
                23.5, 147.48, 88.7, 5.0, 200.0, 120.0, 45.0, 180.0, 10.0, 160.0, 55.0, 95.0, 30.0,
                175.0, 65.0, 210.0, 15.0, 110.0, 70.0, 195.0,
            ];

            while start.elapsed() < Duration::from_secs(20) {
                let pos_idx = (epoch as usize) % seek_positions.len();
                let seek_pos = Duration::from_secs_f64(seek_positions[pos_idx]);
                epoch += 1;

                source.handle_command(AudioCommand::Seek {
                    position: seek_pos,
                    epoch,
                });

                // Fetch a few chunks but don't drain (simulating rapid scrubbing)
                for _ in 0..3 {
                    let fetch = source.fetch_next();
                    if v0_stopped {
                        if fetch.is_eof {
                            eof_after_v0_stop += 1;
                        } else {
                            chunks_after_v0_stop += 1;
                        }
                    }
                    if fetch.is_eof {
                        break;
                    }
                }

                // After 2s: ABR switch — media_info changes, reader past segment 19
                if !format_changed && start.elapsed() > Duration::from_secs(2) {
                    let mut s = state.lock();
                    s.media_info = Some(v3_info());
                    s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
                    // format_change_segment_range returns the FIRST V3 segment where init data lives
                    s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
                    drop(s);
                    format_changed = true;
                }

                // After 4s: old decoder hits boundary → EOF (simulates read boundary)
                if format_changed && !v0_stopped && start.elapsed() > Duration::from_secs(4) {
                    v0_stop.store(true, Ordering::Release);
                    v0_stopped = true;
                }
            }

            // After 20 seconds of rapid seeking with format_change_segment_range():
            //
            // Code uses correct offset (964431), factory succeeds,
            // V3 decoder installed, chunks_after_v0_stop > 0.
            assert!(
                chunks_after_v0_stop > 0,
                "Audio dead after ABR switch: {eof_after_v0_stop} EOFs, \
                 0 chunks produced after V0 decoder stopped. \
                 {epoch} seeks performed over 20s. \
                 Expected format_change_segment_range() to return 964431."
            );
        });

        // Timeout: 30s to catch deadlocks
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if handle.is_finished() {
                if let Err(e) = handle.join() {
                    std::panic::resume_unwind(e);
                }
                return;
            }
            if Instant::now() > deadline {
                panic!("Test timed out after 30s — deadlock in seek/format-change interaction");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
