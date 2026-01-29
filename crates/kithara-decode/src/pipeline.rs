//! Generic decoder that runs in a separate blocking thread.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kanal::Receiver;
use kithara_bufpool::byte_pool;
use kithara_stream::{EpochValidator, Fetch, MediaInfo, Stream, StreamType};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::{
    decoder::SymphoniaDecoder,
    events::{DecodeEvent, DecoderEvent},
    types::{DecodeError, DecodeResult, PcmChunk, PcmSpec},
};

/// Command for decode worker.
#[derive(Debug)]
pub enum DecodeCommand {
    /// Seek to position with new epoch.
    Seek { position: Duration, epoch: u64 },
}

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in Arc<Mutex> to allow:
/// - SymphoniaDecoder to read via Read + Seek
/// - DecodeSource to check media_info() for format changes
///
/// Supports a read boundary: when set, reads at or past the boundary
/// return 0 (EOF). This prevents the old decoder from reading data
/// from a new segment after an ABR variant switch.
struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
    /// Byte offset at which reads stop (return EOF).
    /// `u64::MAX` means no boundary (unlimited reads).
    boundary: Arc<AtomicU64>,
}

impl<T: StreamType> SharedStream<T> {
    fn new(stream: Stream<T>) -> Self {
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
struct OffsetReader<T: StreamType> {
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

/// Trait for decode sources processed in a blocking worker thread.
trait DecodeWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk>;
    fn handle_command(&mut self, cmd: Self::Command);
}

/// Run a blocking decode loop: drain commands, fetch data, send through channel.
fn run_decode_loop<S: DecodeWorkerSource>(
    mut source: S,
    cmd_rx: kanal::Receiver<S::Command>,
    data_tx: kanal::Sender<Fetch<S::Chunk>>,
) {
    trace!("decode worker started");
    let mut at_eof = false;

    loop {
        // Drain pending commands
        let mut cmd_received = false;
        while let Ok(Some(cmd)) = cmd_rx.try_recv() {
            source.handle_command(cmd);
            cmd_received = true;
        }
        if cmd_received {
            at_eof = false;
        }

        if !at_eof {
            let fetch = source.fetch_next();
            let is_eof = fetch.is_eof();

            let mut item = Some(fetch);

            // Non-blocking send with backpressure
            loop {
                match data_tx.try_send_option(&mut item) {
                    Ok(true) => break,
                    Ok(false) => match cmd_rx.try_recv() {
                        Ok(Some(cmd)) => {
                            source.handle_command(cmd);
                            break; // Discard stale chunk, refetch
                        }
                        Ok(None) => {
                            std::thread::sleep(std::time::Duration::from_micros(100));
                        }
                        Err(_) => return,
                    },
                    Err(_) => return,
                }
            }

            if is_eof {
                at_eof = true;
            }
        } else {
            match cmd_rx.try_recv() {
                Ok(Some(cmd)) => {
                    source.handle_command(cmd);
                    at_eof = false;
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    }

    trace!("decode worker stopped");
}

/// Simple decode source (non-streaming, e.g. file).
struct DecodeSource {
    decoder: SymphoniaDecoder,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    emit: Option<Box<dyn Fn(DecodeEvent) + Send>>,
}

impl DecodeSource {
    fn new(decoder: SymphoniaDecoder, epoch: Arc<AtomicU64>) -> Self {
        Self {
            decoder,
            epoch,
            chunks_decoded: 0,
            total_samples: 0,
            emit: None,
        }
    }

    fn with_emit(mut self, emit: Box<dyn Fn(DecodeEvent) + Send>) -> Self {
        self.emit = Some(emit);
        self
    }
}

impl DecodeWorkerSource for DecodeSource {
    type Chunk = PcmChunk<f32>;
    type Command = DecodeCommand;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        let current_epoch = self.epoch.load(Ordering::Acquire);

        loop {
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
                        emit(DecodeEvent::FormatDetected { spec: chunk.spec });
                    }

                    if self.chunks_decoded.is_multiple_of(100) {
                        trace!(
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            spec = ?chunk.spec,
                            epoch = current_epoch,
                            "decode progress"
                        );
                    }

                    return Fetch::new(chunk, false, current_epoch);
                }
                Ok(None) => {
                    debug!(
                        chunks = self.chunks_decoded,
                        samples = self.total_samples,
                        epoch = current_epoch,
                        "decode complete (EOF)"
                    );
                    if let Some(ref emit) = self.emit {
                        emit(DecodeEvent::EndOfStream);
                    }
                    return Fetch::new(PcmChunk::default(), true, current_epoch);
                }
                Err(e) => {
                    warn!(?e, "decode error, attempting to continue");
                    continue;
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Self::Command) {
        match cmd {
            DecodeCommand::Seek { position, epoch } => {
                debug!(?position, epoch, "seek command received");
                self.epoch.store(epoch, Ordering::Release);
                if let Err(e) = self.decoder.seek(position) {
                    warn!(?e, "seek failed");
                } else if let Some(ref emit) = self.emit {
                    emit(DecodeEvent::SeekComplete { position });
                }
            }
        }
    }
}

/// Decode source for Stream with format change detection.
///
/// Monitors media_info changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
struct StreamDecodeSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    decoder: SymphoniaDecoder,
    cached_media_info: Option<MediaInfo>,
    /// Pending format change: (new MediaInfo, byte offset where new segment starts).
    pending_format_change: Option<(MediaInfo, u64)>,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    last_spec: Option<PcmSpec>,
    emit: Option<Box<dyn Fn(DecodeEvent) + Send>>,
}

impl<T: StreamType> StreamDecodeSource<T> {
    fn new(
        shared_stream: SharedStream<T>,
        decoder: SymphoniaDecoder,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
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
        }
    }

    fn with_emit(mut self, emit: Box<dyn Fn(DecodeEvent) + Send>) -> Self {
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
        match SymphoniaDecoder::new_from_media_info(offset_reader, &new_info) {
            Ok(new_decoder) => {
                self.decoder = new_decoder;
                debug!("Decoder recreated successfully");
                true
            }
            Err(e) => {
                warn!(?e, "Failed to recreate decoder, trying probe fallback");
                let offset_reader = OffsetReader::new(self.shared_stream.clone(), base_offset);
                match SymphoniaDecoder::new_with_probe(offset_reader, None) {
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

impl<T: StreamType> DecodeWorkerSource for StreamDecodeSource<T> {
    type Chunk = PcmChunk<f32>;
    type Command = DecodeCommand;

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
                        emit(DecodeEvent::FormatDetected { spec: chunk.spec });
                        self.last_spec = Some(chunk.spec);
                    }

                    // Detect spec change (e.g. after ABR switch)
                    if let Some(old_spec) = self.last_spec
                        && old_spec != chunk.spec
                    {
                        if let Some(ref emit) = self.emit {
                            emit(DecodeEvent::FormatChanged {
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

                    return Fetch::new(chunk, false, current_epoch);
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
                    if let Some(ref emit) = self.emit {
                        emit(DecodeEvent::EndOfStream);
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
            DecodeCommand::Seek { position, epoch } => {
                debug!(?position, epoch, "seek command received");
                self.epoch.store(epoch, Ordering::Release);
                if let Err(e) = self.decoder.seek(position) {
                    warn!(?e, "seek failed");
                } else if let Some(ref emit) = self.emit {
                    emit(DecodeEvent::SeekComplete { position });
                }
            }
        }
    }
}

/// Generic audio decoder running in a separate thread.
///
/// Provides a simple interface for reading decoded PCM audio,
/// compatible with cpal and rodio audio backends.
///
/// # Example
///
/// ```ignore
/// use kithara_decode::{Decoder, DecodeOptions};
/// use std::io::Cursor;
///
/// let source = Cursor::new(wav_data);
/// let mut decoder = Decoder::from_source(source, DecodeOptions::new())?;
///
/// // Get audio format
/// let spec = decoder.spec();
/// println!("{}Hz, {} channels", spec.sample_rate, spec.channels);
///
/// // Read PCM samples
/// let mut buf = [0.0f32; 1024];
/// while !decoder.is_eof() {
///     let n = decoder.read(&mut buf);
///     play_samples(&buf[..n]);
/// }
/// ```
pub struct Decoder<S> {
    /// Command sender for seek.
    cmd_tx: kanal::Sender<DecodeCommand>,

    /// PCM chunk receiver.
    pcm_rx: Receiver<Fetch<PcmChunk<f32>>>,

    /// Shared epoch counter with worker.
    epoch: Arc<AtomicU64>,

    /// Epoch validator for filtering stale chunks.
    validator: EpochValidator,

    /// Current audio specification (updated from chunks).
    spec: PcmSpec,

    /// Current chunk being read.
    current_chunk: Option<Vec<f32>>,

    /// Current position in chunk.
    chunk_offset: usize,

    /// End of stream reached.
    eof: bool,

    /// Decode events channel (used by `from_source`).
    decode_events_tx: broadcast::Sender<DecodeEvent>,

    /// Unified events sender for `Decoder<Stream<T>>`.
    /// Holds `broadcast::Sender<DecoderEvent<T::Event>>` type-erased.
    /// `None` for non-stream decoders (created via `from_source`).
    unified_events: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Cancellation token for graceful shutdown.
    cancel: Option<CancellationToken>,

    /// Marker for source type.
    _marker: std::marker::PhantomData<S>,
}

/// Decode-specific options.
#[derive(Debug, Clone, Default)]
pub struct DecodeOptions {
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
    /// Command channel capacity.
    pub command_channel_capacity: usize,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Media info hint for format detection
    pub media_info: Option<MediaInfo>,
}

impl DecodeOptions {
    /// Create default options.
    pub fn new() -> Self {
        Self {
            pcm_buffer_chunks: 10,
            command_channel_capacity: 4,
            hint: None,
            media_info: None,
        }
    }

    /// Set format hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Set media info.
    pub fn with_media_info(mut self, info: MediaInfo) -> Self {
        self.media_info = Some(info);
        self
    }
}

/// Configuration for decoder with stream config.
///
/// Generic over StreamType to include stream-specific configuration.
pub struct DecoderConfig<T: StreamType> {
    /// Stream configuration (HlsConfig, FileConfig, etc.)
    pub stream: T::Config,
    /// Decode-specific options
    pub decode: DecodeOptions,
    /// Unified events sender (optional — if not provided, one is created internally).
    events_tx: Option<broadcast::Sender<DecoderEvent<T::Event>>>,
}

impl<T: StreamType> DecoderConfig<T> {
    /// Create config with stream config.
    pub fn new(stream: T::Config) -> Self {
        Self {
            stream,
            decode: DecodeOptions::new(),
            events_tx: None,
        }
    }

    /// Set decode options.
    pub fn with_decode(mut self, decode: DecodeOptions) -> Self {
        self.decode = decode;
        self
    }

    /// Set format hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.decode.hint = Some(hint.into());
        self
    }

    /// Set unified events channel.
    ///
    /// Stream events and decode events are forwarded as `DecoderEvent::Stream(e)`
    /// and `DecoderEvent::Decode(e)` respectively.
    pub fn with_events(mut self, events_tx: broadcast::Sender<DecoderEvent<T::Event>>) -> Self {
        self.events_tx = Some(events_tx);
        self
    }
}

impl<S> Decoder<S>
where
    S: Read + Seek + Send + Sync + 'static,
{
    /// Create a new decoder from any Read + Seek source.
    ///
    /// Spawns a blocking decode thread.
    pub fn from_source(source: S, options: DecodeOptions) -> DecodeResult<Self> {
        let cmd_capacity = options.command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = kanal::bounded(cmd_capacity);
        let (data_tx, data_rx) = kanal::bounded(options.pcm_buffer_chunks.max(1));

        let epoch = Arc::new(AtomicU64::new(0));
        let (decode_events_tx, _) = broadcast::channel(64);

        let symphonia = Self::create_decoder(source, &options)?;
        let initial_spec = symphonia.spec();

        let emit_tx = decode_events_tx.clone();
        let emit = Box::new(move |event: DecodeEvent| {
            let _ = emit_tx.send(event);
        });

        let decode_source = DecodeSource::new(symphonia, Arc::clone(&epoch)).with_emit(emit);

        std::thread::Builder::new()
            .name("kithara-decode".to_string())
            .spawn(move || {
                run_decode_loop(decode_source, cmd_rx, data_tx);
            })
            .map_err(|e| {
                DecodeError::Io(std::io::Error::other(format!(
                    "failed to spawn decode thread: {}",
                    e
                )))
            })?;

        Ok(Self {
            cmd_tx,
            pcm_rx: data_rx,
            epoch,
            validator: EpochValidator::new(),
            spec: initial_spec,
            current_chunk: None,
            chunk_offset: 0,
            eof: false,
            decode_events_tx,
            unified_events: None,
            cancel: None,
            _marker: std::marker::PhantomData,
        })
    }

    /// Get reference to PCM receiver for direct channel access.
    pub fn pcm_rx(&self) -> &Receiver<Fetch<PcmChunk<f32>>> {
        &self.pcm_rx
    }

    /// Create Symphonia decoder with appropriate method based on options.
    fn create_decoder(source: S, options: &DecodeOptions) -> DecodeResult<SymphoniaDecoder> {
        if let Some(ref media_info) = options.media_info {
            SymphoniaDecoder::new_from_media_info(source, media_info)
        } else {
            SymphoniaDecoder::new_with_probe(source, options.hint.as_deref())
        }
    }
}

// ============================================================================
// Public API for cpal/rodio compatibility
// ============================================================================

impl<S> Decoder<S> {
    /// Subscribe to decode events.
    ///
    /// For `Decoder<Stream<T>>`, prefer `events()` which provides unified
    /// stream + decode events.
    pub fn decode_events(&self) -> broadcast::Receiver<DecodeEvent> {
        self.decode_events_tx.subscribe()
    }

    /// Get current audio specification.
    ///
    /// Returns sample rate and channel count for audio output setup.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Check if end of stream has been reached.
    pub fn is_eof(&self) -> bool {
        self.eof
    }

    /// Read decoded PCM samples into buffer.
    ///
    /// Returns number of samples written (may be less than buffer size).
    /// Returns 0 when EOF is reached.
    ///
    /// Samples are interleaved f32 (e.g., LRLRLR for stereo).
    pub fn read(&mut self, buf: &mut [f32]) -> usize {
        if self.eof || buf.is_empty() {
            return 0;
        }

        let mut written = 0;

        while written < buf.len() {
            // Try to read from current chunk
            if let Some(ref chunk) = self.current_chunk {
                let remaining_in_chunk = chunk.len() - self.chunk_offset;
                let to_copy = (buf.len() - written).min(remaining_in_chunk);

                buf[written..written + to_copy]
                    .copy_from_slice(&chunk[self.chunk_offset..self.chunk_offset + to_copy]);

                written += to_copy;
                self.chunk_offset += to_copy;

                if self.chunk_offset >= chunk.len() {
                    self.current_chunk = None;
                    self.chunk_offset = 0;
                }

                if written >= buf.len() {
                    break;
                }
            }

            // Need more data - fetch next chunk
            if !self.fill_buffer() {
                break;
            }
        }

        written
    }

    /// Seek to position in the audio stream.
    ///
    /// Note: Seek clears internal buffers and invalidates pending chunks.
    pub fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        // Increment epoch to invalidate pending chunks
        let new_epoch = self.validator.next_epoch();
        self.epoch.store(new_epoch, Ordering::Release);

        // Send seek command to worker with new epoch
        self.cmd_tx
            .send(DecodeCommand::Seek {
                position,
                epoch: new_epoch,
            })
            .map_err(|_| DecodeError::SeekError("channel closed".to_string()))?;

        // Clear local state
        self.current_chunk = None;
        self.chunk_offset = 0;
        self.eof = false;

        debug!(?position, epoch = new_epoch, "seek initiated");
        Ok(())
    }

    /// Receive next chunk from channel, filtering stale chunks.
    fn fill_buffer(&mut self) -> bool {
        if self.eof {
            return false;
        }

        loop {
            match self.pcm_rx.recv() {
                Ok(fetch) => {
                    // Skip stale chunks (from before seek)
                    if !self.validator.is_valid(&fetch) {
                        trace!(
                            chunk_epoch = fetch.epoch(),
                            current_epoch = self.validator.epoch,
                            "skipping stale chunk"
                        );
                        continue;
                    }

                    if fetch.is_eof() {
                        debug!(epoch = fetch.epoch(), "Decoder: received EOF");
                        self.eof = true;
                        return false;
                    }

                    let chunk = fetch.into_inner();
                    trace!(
                        samples = chunk.pcm.len(),
                        spec = ?chunk.spec,
                        "Decoder: received chunk"
                    );
                    self.spec = chunk.spec;
                    self.current_chunk = Some(chunk.pcm);
                    self.chunk_offset = 0;
                    return true;
                }
                Err(_) => {
                    debug!("Decoder: channel closed (EOF)");
                    self.eof = true;
                    return false;
                }
            }
        }
    }
}

/// Specialized impl for Stream-based decoders.
///
/// Provides async constructor that creates Stream internally.
/// Uses StreamDecodeSource for automatic format change detection on ABR switch.
impl<T> Decoder<Stream<T>>
where
    T: StreamType,
{
    /// Create decoder from DecoderConfig.
    ///
    /// This is the target API for Stream sources.
    /// Uses StreamDecodeSource for automatic decoder recreation on format change.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = DecoderConfig::<Hls>::new(hls_config);
    /// let decoder = Decoder::new(config).await?;
    /// sink.append(decoder);
    /// ```
    pub async fn new(mut config: DecoderConfig<T>) -> Result<Self, DecodeError> {
        let cancel = CancellationToken::new();

        // Create unified events channel (or use the one from config).
        let unified_tx = config
            .events_tx
            .take()
            .unwrap_or_else(|| broadcast::channel(64).0);

        // Create stream (ensure_events is called internally by Stream::new).
        let mut stream = Stream::<T>::new(config.stream)
            .await
            .map_err(|e| DecodeError::Io(std::io::Error::other(e.to_string())))?;

        // Forward stream events into unified channel.
        if let Some(mut stream_events_rx) = stream.take_events_rx() {
            let forward_tx = unified_tx.clone();
            let forward_cancel = cancel.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = forward_cancel.cancelled() => break,
                        result = stream_events_rx.recv() => {
                            match result {
                                Ok(event) => {
                                    debug!("forwarding stream event to unified channel");
                                    let _ = forward_tx.send(DecoderEvent::Stream(event));
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(skipped = n, "stream events receiver lagged");
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }
            });
        }

        // Trigger first segment load to get MediaInfo for proper decoder creation.
        let mut probe_buf = byte_pool().get_with(|b| b.resize(1024, 0));
        let _ = stream.read(&mut probe_buf);

        // Seek back to start
        stream.seek(SeekFrom::Start(0)).map_err(DecodeError::Io)?;

        // Get initial MediaInfo
        let initial_media_info = stream.media_info();
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        // Create shared stream for format change detection
        let shared_stream = SharedStream::new(stream);

        // Create initial decoder
        let symphonia = if let Some(ref info) = initial_media_info {
            SymphoniaDecoder::new_from_media_info(shared_stream.clone(), info)?
        } else {
            SymphoniaDecoder::new_with_probe(shared_stream.clone(), config.decode.hint.as_deref())?
        };

        let initial_spec = symphonia.spec();
        let options = config.decode;

        let cmd_capacity = options.command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = kanal::bounded(cmd_capacity);
        let (data_tx, data_rx) = kanal::bounded(options.pcm_buffer_chunks.max(1));

        let epoch = Arc::new(AtomicU64::new(0));
        let (decode_events_tx, _) = broadcast::channel(64);

        // Emit closure sends DecodeEvent to both raw and unified channels.
        let emit_raw_tx = decode_events_tx.clone();
        let emit_unified_tx = unified_tx.clone();
        let emit = Box::new(move |event: DecodeEvent| {
            let _ = emit_raw_tx.send(event.clone());
            let _ = emit_unified_tx.send(DecoderEvent::Decode(event));
        });

        // Use StreamDecodeSource for format change detection
        let decode_source = StreamDecodeSource::new(
            shared_stream,
            symphonia,
            initial_media_info,
            Arc::clone(&epoch),
        )
        .with_emit(emit);

        std::thread::Builder::new()
            .name("kithara-decode".to_string())
            .spawn(move || {
                run_decode_loop(decode_source, cmd_rx, data_tx);
            })
            .map_err(|e| {
                DecodeError::Io(std::io::Error::other(format!(
                    "failed to spawn decode thread: {}",
                    e
                )))
            })?;

        Ok(Self {
            cmd_tx,
            pcm_rx: data_rx,
            epoch,
            validator: EpochValidator::new(),
            spec: initial_spec,
            current_chunk: None,
            chunk_offset: 0,
            eof: false,
            decode_events_tx,
            unified_events: Some(Box::new(unified_tx)),
            cancel: Some(cancel),
            _marker: std::marker::PhantomData,
        })
    }

    /// Subscribe to unified events (stream + decode).
    pub fn events(&self) -> broadcast::Receiver<DecoderEvent<T::Event>> {
        self.unified_events
            .as_ref()
            .and_then(|any| any.downcast_ref::<broadcast::Sender<DecoderEvent<T::Event>>>())
            .expect("unified_events always set for Stream-based decoders")
            .subscribe()
    }
}

impl<S> Drop for Decoder<S> {
    fn drop(&mut self) {
        if let Some(ref cancel) = self.cancel {
            cancel.cancel();
        }
    }
}

// rodio::Source implementation for Decoder
#[cfg(feature = "rodio")]
impl<S> Iterator for Decoder<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        // Try to get sample from current chunk
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        // Chunk exhausted or no chunk - need more data
        if self.fill_buffer()
            && let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        None
    }
}

#[cfg(feature = "rodio")]
impl<S> rodio::Source for Decoder<S> {
    fn current_span_len(&self) -> Option<usize> {
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            return Some(chunk.len() - self.chunk_offset);
        }
        None
    }

    fn channels(&self) -> u16 {
        self.spec.channels
    }

    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Create minimal valid WAV file
    fn create_test_wav(sample_count: usize) -> Vec<u8> {
        let channels = 2u16;
        let sample_rate = 44100u32;
        let bytes_per_sample = 2;
        let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
        let file_size = 36 + data_size;

        let mut wav = Vec::new();

        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * bytes_per_sample as u16;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());

        // Generate samples
        for i in 0..sample_count {
            let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
            for _ in 0..channels {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
        }

        wav
    }

    #[test]
    fn test_decoder_from_source() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let _decoder = Decoder::from_source(cursor, options).unwrap();
    }

    #[test]
    fn test_decoder_receive_chunks() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let decoder = Decoder::from_source(cursor, options).unwrap();

        let mut chunk_count = 0;
        while let Ok(fetch) = decoder.pcm_rx().recv() {
            if fetch.is_eof() {
                break;
            }
            chunk_count += 1;
            assert!(!fetch.data.pcm.is_empty());
            if chunk_count >= 5 {
                break;
            }
        }

        assert!(chunk_count > 0);
    }

    #[test]
    fn test_decode_options_with_media_info() {
        let info = MediaInfo::default()
            .with_container(kithara_stream::ContainerFormat::Wav)
            .with_sample_rate(44100);

        let options = DecodeOptions::new().with_media_info(info.clone());

        assert!(options.media_info.is_some());
        assert_eq!(
            options.media_info.unwrap().container,
            Some(kithara_stream::ContainerFormat::Wav)
        );
    }

    #[test]
    fn test_decoder_spec() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let decoder = Decoder::from_source(cursor, options).unwrap();

        let spec = decoder.spec();
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.channels, 2);
    }

    #[test]
    fn test_decoder_read() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let mut decoder = Decoder::from_source(cursor, options).unwrap();

        let mut buf = [0.0f32; 256];
        let mut total_read = 0;

        while !decoder.is_eof() {
            let n = decoder.read(&mut buf);
            if n == 0 {
                break;
            }
            total_read += n;
        }

        assert!(total_read > 0);
        assert!(decoder.is_eof());
    }

    #[test]
    fn test_decoder_read_small_buffer() {
        let wav_data = create_test_wav(100);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let mut decoder = Decoder::from_source(cursor, options).unwrap();

        // Read with very small buffer
        let mut buf = [0.0f32; 4];
        let n = decoder.read(&mut buf);

        assert_eq!(n, 4);
    }

    #[test]
    fn test_decoder_is_eof() {
        let wav_data = create_test_wav(10);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let mut decoder = Decoder::from_source(cursor, options).unwrap();

        assert!(!decoder.is_eof());

        // Read all data
        let mut buf = [0.0f32; 1024];
        while decoder.read(&mut buf) > 0 {}

        assert!(decoder.is_eof());
    }

    #[test]
    fn test_decoder_seek() {
        let wav_data = create_test_wav(44100); // 1 second
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let mut decoder = Decoder::from_source(cursor, options).unwrap();

        // Read some data
        let mut buf = [0.0f32; 256];
        decoder.read(&mut buf);

        // Seek to beginning
        let result = decoder.seek(Duration::from_secs(0));
        assert!(result.is_ok());

        // Should be able to read again
        assert!(!decoder.is_eof());
    }
}
