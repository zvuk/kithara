//! `AppleInner` — the lifecycle state shared across all Apple codec generics.

#![allow(unsafe_code)]

use std::{
    cell::Cell,
    ffi::c_void,
    io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use kithara_bufpool::PcmPool;
use tracing::{debug, trace, warn};

use super::{
    config::AppleConfig,
    consts::{Consts, os_status_to_string},
    converter::{ConverterInputState, converter_input_callback},
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterDispose, AudioConverterFillComplexBuffer,
        AudioConverterNew, AudioConverterRef, AudioConverterReset, AudioConverterSetProperty,
        AudioFileStreamClose, AudioFileStreamID, AudioFileStreamOpen, AudioFileStreamParseBytes,
        AudioFileTypeID, AudioStreamBasicDescription, UInt32,
    },
    parser::{
        StreamParserState, container_to_file_type, packets_callback, property_listener_callback,
    },
};
use crate::{
    backend::{BoxedSource, RecoverableHardwareError, recoverable_hardware_error},
    error::{DecodeError, DecodeResult},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Resolve the source length, preferring the HTTP byte-length handle.
fn resolve_source_len(source: &mut BoxedSource, config: &AppleConfig) -> Option<u64> {
    config
        .byte_len_handle
        .as_ref()
        .map(|h| h.load(Ordering::Acquire))
        .filter(|&len| len > 0)
        .or_else(|| {
            let len = source.seek(SeekFrom::End(0)).ok()?;
            source.seek(SeekFrom::Start(0)).ok()?;
            Some(len)
        })
}

/// Owned inputs gathered during `AppleInner::try_new` and handed to `assemble_inner`.
struct AssembleParts<'a> {
    stream_parser: AudioFileStreamID,
    converter: AudioConverterRef,
    parser_state: Box<StreamParserState>,
    source: BoxedSource,
    read_buffer: Vec<u8>,
    channels: u16,
    sample_rate: u32,
    source_len: Option<u64>,
    total_parsed: u64,
    config: &'a AppleConfig,
}

/// Apple `AudioToolbox` streaming decoder inner state.
pub(super) struct AppleInner {
    stream_parser: AudioFileStreamID,
    converter: AudioConverterRef,
    parser_state: Box<StreamParserState>,
    converter_input: Box<ConverterInputState>,
    source: BoxedSource,
    read_buffer: Vec<u8>,
    pub(super) spec: PcmSpec,
    pub(super) position: Duration,
    frame_offset: u64,
    pub(super) duration: Option<Duration>,
    pub(super) metadata: TrackMetadata,
    pub(super) byte_len_handle: Arc<AtomicU64>,
    pcm_buffer: Vec<f32>,
    source_eof: bool,
    frames_decoded: u64,
    source_len: Option<u64>,
    source_byte_pos: u64,
    data_offset: u64,
    cached_duration: Option<Duration>,
    pool: PcmPool,
    owner_thread: Cell<Option<thread::ThreadId>>,
}

impl AppleInner {
    #[inline]
    fn assert_thread_affinity(&self) {
        #[cfg(debug_assertions)]
        {
            let current = thread::current().id();
            if let Some(owner) = self.owner_thread.get() {
                debug_assert_eq!(
                    owner, current,
                    "Apple decoder used from multiple threads; this backend requires thread affinity"
                );
            } else {
                self.owner_thread.set(Some(current));
            }
        }
    }

    fn parse_until_format_ready(
        source: &mut BoxedSource,
        stream_parser: AudioFileStreamID,
        parser_state: &mut StreamParserState,
        read_buffer: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let mut total_parsed = 0usize;
        loop {
            let n = source
                .read(read_buffer)
                .map_err(|error| DecodeError::Backend(Box::new(error)))?;

            if n == 0 {
                return Err(DecodeError::InvalidData(
                    "EOF before audio format detected".into(),
                ));
            }

            total_parsed += n;

            #[expect(
                clippy::cast_possible_truncation,
                reason = "read buffer is 32 KB, fits in u32"
            )]
            // SAFETY: `stream_parser` is a valid handle created by `AudioFileStreamOpen`.
            // `read_buffer` is a valid slice with `n` bytes read from the source.
            let status = unsafe {
                AudioFileStreamParseBytes(
                    stream_parser,
                    n as UInt32,
                    read_buffer.as_ptr() as *const c_void,
                    0,
                )
            };

            if status != Consts::noErr && status != Consts::kAudioFileStreamError_NotOptimized {
                let err_str = os_status_to_string(status);
                warn!(status, err = %err_str, "Apple decoder: AudioFileStreamParseBytes failed");
                return Err(DecodeError::Backend(Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    format!("AudioFileStreamParseBytes failed: {}", err_str),
                ))));
            }

            if let Some(ref format) = parser_state.format
                && parser_state.ready
            {
                debug!(
                    sample_rate = format.mSampleRate,
                    channels = format.mChannelsPerFrame,
                    format_id = format.mFormatID,
                    frames_per_packet = format.mFramesPerPacket,
                    bytes_parsed = total_parsed,
                    "Apple decoder: format detected"
                );
                return Ok(total_parsed);
            }

            if let Some(ref err) = parser_state.error {
                return Err(DecodeError::InvalidData(err.clone()));
            }

            if total_parsed > Consts::MAX_PARSE_BYTES {
                return Err(DecodeError::InvalidData(
                    "Could not detect format after 1MB".into(),
                ));
            }
        }
    }

    fn apply_magic_cookie(converter: AudioConverterRef, cookie: Option<&[u8]>) {
        let Some(cookie) = cookie else {
            return;
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "magic cookie size fits in u32"
        )]
        // SAFETY: `converter` is a valid handle. `cookie` is a valid byte slice.
        let status = unsafe {
            AudioConverterSetProperty(
                converter,
                Consts::kAudioConverterDecompressionMagicCookie,
                cookie.len() as UInt32,
                cookie.as_ptr() as *const c_void,
            )
        };
        if status == Consts::noErr {
            debug!(
                cookie_size = cookie.len(),
                "Apple decoder: magic cookie set"
            );
        } else {
            warn!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: failed to set magic cookie (continuing anyway)"
            );
        }
    }

    pub(super) fn try_new(
        mut source: BoxedSource,
        config: &AppleConfig,
    ) -> Result<Self, RecoverableHardwareError> {
        let Some(container) = config.container else {
            return Err(recoverable_hardware_error(
                source,
                DecodeError::InvalidData(
                    "Container format must be specified for Apple decoder".into(),
                ),
            ));
        };

        let Some(file_type) = container_to_file_type(container) else {
            return Err(recoverable_hardware_error(
                source,
                DecodeError::UnsupportedContainer(container),
            ));
        };

        let source_len = resolve_source_len(&mut source, config);

        debug!(
            ?container,
            file_type,
            ?source_len,
            "Apple decoder: creating streaming decoder"
        );

        let mut parser_state = Box::new(StreamParserState::new());
        let state_ptr = parser_state.as_mut() as *mut StreamParserState as *mut c_void;

        let stream_parser = match Self::open_stream_parser(state_ptr, file_type) {
            Ok(handle) => handle,
            Err(error) => return Err(recoverable_hardware_error(source, error)),
        };

        debug!("Apple decoder: AudioFileStream opened");

        let mut read_buffer = vec![0u8; Consts::PARSE_READ_BUFFER_SIZE];
        let total_parsed = match Self::parse_until_format_ready(
            &mut source,
            stream_parser,
            &mut parser_state,
            &mut read_buffer,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(recoverable_hardware_error(source, error));
            }
        };

        let Some(format) = parser_state.format else {
            // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
            unsafe {
                AudioFileStreamClose(stream_parser);
            }
            return Err(recoverable_hardware_error(
                source,
                DecodeError::InvalidData("No audio format detected".into()),
            ));
        };

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "sample rate fits in u32 for valid audio"
        )]
        let sample_rate = format.mSampleRate as u32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "channel count fits in u16 for valid audio"
        )]
        let channels = format.mChannelsPerFrame as u16;

        let converter = match Self::new_audio_converter(&format, channels) {
            Ok(handle) => handle,
            Err(error) => {
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(recoverable_hardware_error(source, error));
            }
        };

        debug!("Apple decoder: AudioConverter created");

        Self::apply_magic_cookie(converter, parser_state.magic_cookie.as_deref());

        Ok(Self::assemble_inner(AssembleParts {
            stream_parser,
            converter,
            parser_state,
            source,
            read_buffer,
            channels,
            sample_rate,
            source_len,
            total_parsed: total_parsed as u64,
            config,
        }))
    }

    fn open_stream_parser(
        state_ptr: *mut c_void,
        file_type: AudioFileTypeID,
    ) -> DecodeResult<AudioFileStreamID> {
        let mut stream_parser: AudioFileStreamID = ptr::null_mut();
        // SAFETY: `state_ptr` points to a live, pinned `StreamParserState` owned by the caller's
        // `Box<StreamParserState>`. The callbacks receive this pointer and cast it back; the Box
        // keeps it stable for the lifetime of the parser.
        let status = unsafe {
            AudioFileStreamOpen(
                state_ptr,
                property_listener_callback,
                packets_callback,
                file_type,
                &mut stream_parser,
            )
        };

        if status != Consts::noErr {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioFileStreamOpen failed");
            return Err(DecodeError::Backend(Box::new(IoError::new(
                ErrorKind::InvalidData,
                format!("AudioFileStreamOpen failed: {}", err_str),
            ))));
        }

        Ok(stream_parser)
    }

    fn new_audio_converter(
        input_format: &AudioStreamBasicDescription,
        channels: u16,
    ) -> DecodeResult<AudioConverterRef> {
        let output_format = AudioStreamBasicDescription {
            mSampleRate: input_format.mSampleRate,
            mFormatID: Consts::kAudioFormatLinearPCM,
            mFormatFlags: Consts::kAudioFormatFlagsNativeFloatPacked,
            mBytesPerPacket: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mFramesPerPacket: 1,
            mBytesPerFrame: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mChannelsPerFrame: u32::from(channels),
            mBitsPerChannel: Consts::BITS_PER_F32_SAMPLE,
            mReserved: 0,
        };

        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: `input_format` and `output_format` are valid C structs on the stack;
        // `converter` receives the new handle on success.
        let status = unsafe { AudioConverterNew(input_format, &output_format, &mut converter) };

        if status != Consts::noErr {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioConverterNew failed");
            return Err(DecodeError::Backend(Box::new(IoError::new(
                ErrorKind::InvalidData,
                format!("AudioConverterNew failed: {}", err_str),
            ))));
        }

        Ok(converter)
    }

    fn assemble_inner(parts: AssembleParts<'_>) -> Self {
        let AssembleParts {
            stream_parser,
            converter,
            parser_state,
            source,
            read_buffer,
            channels,
            sample_rate,
            source_len,
            total_parsed,
            config,
        } = parts;

        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        let byte_len_handle = config.byte_len_handle.clone().unwrap_or_default();
        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());

        let buffer_frames = Consts::DEFAULT_BUFFER_FRAMES;
        let pcm_buffer = vec![0.0f32; buffer_frames * channels as usize];

        let converter_input = Box::new(ConverterInputState::new());

        debug!(
            ?spec,
            buffer_frames,
            packets_buffered = parser_state.packet_buffer.len(),
            "Apple decoder: initialized successfully"
        );

        let data_offset = parser_state.data_offset.cast_unsigned();

        let cached_duration = Self::initial_duration_estimate(
            &parser_state,
            total_parsed,
            data_offset,
            source_len,
            sample_rate,
        );

        Self {
            stream_parser,
            converter,
            parser_state,
            converter_input,
            source,
            read_buffer,
            spec,
            position: Duration::ZERO,
            frame_offset: 0,
            duration: cached_duration,
            metadata: TrackMetadata::default(),
            byte_len_handle,
            pcm_buffer,
            source_eof: false,
            frames_decoded: 0,
            source_len,
            source_byte_pos: total_parsed,
            data_offset,
            cached_duration,
            pool,
            owner_thread: Cell::new(None),
        }
    }

    pub(super) fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.assert_thread_affinity();
        loop {
            while self.parser_state.packet_buffer.len() < Consts::MIN_PACKETS_FOR_DECODE
                && !self.source_eof
            {
                self.feed_parser()?;
            }

            if self.parser_state.packet_buffer.is_empty() && self.source_eof {
                debug!(
                    position = ?self.position,
                    frames_decoded = self.frames_decoded,
                    "Apple decoder: EOF reached"
                );
                return Ok(None);
            }

            let Some(packet) = self.parser_state.packet_buffer.pop() else {
                return Ok(None);
            };

            self.converter_input.current_packet = Some(packet);

            let channels = self.spec.channels as usize;
            let output_frames = self.ensure_pcm_buffer_capacity(channels);

            match self.run_converter(output_frames)? {
                Some(output_packets) => {
                    return Ok(Some(self.finalize_chunk(output_packets, channels)?));
                }
                None => continue,
            }
        }
    }

    fn ensure_pcm_buffer_capacity(&mut self, channels: usize) -> usize {
        let frames_per_packet = self
            .parser_state
            .format
            .map_or(Consts::DEFAULT_BUFFER_FRAMES, |f| {
                f.mFramesPerPacket as usize
            });
        let output_frames = frames_per_packet.max(Consts::DEFAULT_BUFFER_FRAMES);
        if self.pcm_buffer.len() < output_frames * channels {
            self.pcm_buffer.resize(output_frames * channels, 0.0);
        }
        output_frames
    }

    fn run_converter(&mut self, output_frames: usize) -> DecodeResult<Option<UInt32>> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "PCM buffer size fits in u32"
        )]
        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.spec.channels),
                mDataByteSize: (self.pcm_buffer.len() * Consts::BYTES_PER_F32_SAMPLE as usize)
                    as u32,
                mData: self.pcm_buffer.as_mut_ptr() as *mut c_void,
            }],
        };

        #[expect(
            clippy::cast_possible_truncation,
            reason = "output frame count fits in u32"
        )]
        let mut output_packets = output_frames as UInt32;
        let input_ptr = self.converter_input.as_mut() as *mut ConverterInputState as *mut c_void;

        // SAFETY: `self.converter` is a valid handle; `input_ptr` points to a live `ConverterInputState`;
        // `buffer_list` is a valid output buffer on the stack.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                converter_input_callback,
                input_ptr,
                &mut output_packets,
                &mut buffer_list,
                ptr::null_mut(),
            )
        };

        if status == Consts::kAudioConverterErr_NoDataNow {
            trace!("Apple decoder: converter needs more data");
            return Ok(None);
        }

        if status != Consts::noErr && output_packets == 0 {
            let err_str = os_status_to_string(status);
            warn!(
                status,
                err = %err_str,
                "Apple decoder: AudioConverterFillComplexBuffer failed"
            );
            return Err(DecodeError::Backend(Box::new(IoError::other(format!(
                "AudioConverterFillComplexBuffer failed: {}",
                err_str
            )))));
        }

        if output_packets == 0 {
            return Ok(None);
        }

        Ok(Some(output_packets))
    }

    fn finalize_chunk(
        &mut self,
        output_packets: UInt32,
        channels: usize,
    ) -> DecodeResult<PcmChunk> {
        let frames = output_packets as usize;
        let samples = frames * channels;

        let mut pooled = self.pool.get();
        pooled
            .ensure_len(samples)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;
        pooled[..samples].copy_from_slice(&self.pcm_buffer[..samples]);

        let meta = PcmMeta {
            spec: self.spec,
            frame_offset: self.frame_offset,
            timestamp: self.position,
            segment_index: None,
            variant_index: None,
            epoch: 0,
        };
        let chunk = PcmChunk::new(meta, pooled);

        self.frames_decoded += frames as u64;
        self.frame_offset += frames as u64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for position tracking"
        )]
        let pos_secs = self.frames_decoded as f64 / f64::from(self.spec.sample_rate);
        self.position = Duration::from_secs_f64(pos_secs);

        trace!(
            frames,
            samples,
            position_ms = self.position.as_millis(),
            "Apple decoder: decoded chunk"
        );

        Ok(chunk)
    }

    fn feed_parser(&mut self) -> DecodeResult<()> {
        self.feed_parser_with_flags(0)
    }

    fn feed_parser_with_flags(&mut self, flags: UInt32) -> DecodeResult<()> {
        let n = self
            .source
            .read(&mut self.read_buffer)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        if n == 0 {
            self.source_eof = true;
            return Ok(());
        }

        self.source_byte_pos += n as u64;

        #[expect(
            clippy::cast_possible_truncation,
            reason = "read buffer is 32 KB, fits in u32"
        )]
        // SAFETY: `self.stream_parser` is a valid handle; `self.read_buffer` is a live slice with `n` valid bytes.
        let status = unsafe {
            AudioFileStreamParseBytes(
                self.stream_parser,
                n as UInt32,
                self.read_buffer.as_ptr() as *const c_void,
                flags,
            )
        };

        if status != Consts::noErr && status != Consts::kAudioFileStreamError_NotOptimized {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, bytes = n, "Apple decoder: parse failed");
            return Err(DecodeError::Backend(Box::new(IoError::new(
                ErrorKind::InvalidData,
                format!("AudioFileStreamParseBytes failed: {}", err_str),
            ))));
        }

        trace!(
            bytes = n,
            byte_pos = self.source_byte_pos,
            packets = self.parser_state.packet_buffer.len(),
            "Apple decoder: fed parser"
        );
        Ok(())
    }

    pub(super) fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.assert_thread_affinity();
        let target_frame = self.target_frame_for(pos);
        let byte_offset = self.estimate_byte_offset(pos);

        self.repoint_source_and_prime(byte_offset, pos, target_frame)?;

        self.position = pos;
        self.frames_decoded = target_frame;
        self.frame_offset = target_frame;

        debug!(
            new_position = ?self.position,
            frames_decoded = self.frames_decoded,
            packets_buffered = self.parser_state.packet_buffer.len(),
            "Apple decoder: seek complete"
        );

        Ok(())
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seek target frame fits in u64 for realistic durations"
    )]
    fn target_frame_for(&self, pos: Duration) -> u64 {
        (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64
    }

    fn repoint_source_and_prime(
        &mut self,
        byte_offset: u64,
        pos: Duration,
        target_frame: u64,
    ) -> DecodeResult<()> {
        debug!(
            position_secs = pos.as_secs_f64(),
            target_frame,
            current_frame = self.frames_decoded,
            estimated_byte_offset = byte_offset,
            data_offset = self.data_offset,
            "Apple decoder: seeking to byte offset"
        );

        self.source
            .seek(SeekFrom::Start(byte_offset))
            .map_err(|e| DecodeError::SeekFailed(format!("Source seek failed: {}", e)))?;
        self.source_byte_pos = byte_offset;

        self.reset_for_seek();

        self.feed_parser_with_flags(Consts::kAudioFileStreamParseFlag_Discontinuity)
    }

    fn reset_for_seek(&mut self) {
        // SAFETY: `self.converter` is a valid handle from `AudioConverterNew`.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            warn!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: converter reset failed"
            );
        }

        while self.parser_state.packet_buffer.pop().is_some() {}

        self.converter_input.current_packet = None;
        self.converter_input.held_packet_data = None;

        self.source_eof = false;
    }

    fn estimate_byte_offset(&self, pos: Duration) -> u64 {
        let Some(source_len) = self.source_len else {
            return self.data_offset;
        };

        let audio_data_size = source_len.saturating_sub(self.data_offset);
        if audio_data_size == 0 {
            return self.data_offset;
        }

        let total_duration = self.estimate_total_duration();
        if total_duration.as_secs_f64() <= 0.0 {
            let ratio = pos.as_secs_f64() / Consts::DEFAULT_SEEK_DURATION_SECS;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "byte offset estimation — precision loss acceptable"
            )]
            let offset = (audio_data_size as f64 * ratio.min(1.0)) as u64;
            return self.data_offset + offset;
        }

        let ratio = pos.as_secs_f64() / total_duration.as_secs_f64();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "byte offset estimation — precision loss acceptable"
        )]
        let offset = (audio_data_size as f64 * ratio.clamp(0.0, 1.0)) as u64;

        self.data_offset + offset
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "precision loss acceptable for duration estimation"
    )]
    fn initial_duration_estimate(
        parser_state: &StreamParserState,
        total_parsed: u64,
        data_offset: u64,
        source_len: Option<u64>,
        sample_rate: u32,
    ) -> Option<Duration> {
        let format = parser_state.format?;
        let frames_per_packet = u64::from(format.mFramesPerPacket);
        if frames_per_packet == 0 {
            return None;
        }

        if let Some(packet_count) = parser_state.audio_data_packet_count {
            let total_frames = packet_count * frames_per_packet;
            let duration_secs = total_frames as f64 / f64::from(sample_rate);
            if duration_secs > 0.0 {
                return Some(Duration::from_secs_f64(duration_secs));
            }
        }

        let source_len = source_len?;
        if total_parsed <= data_offset {
            return None;
        }
        let packets_parsed = parser_state.packet_buffer.len() as u64;
        if packets_parsed == 0 {
            return None;
        }

        let frames_parsed = packets_parsed * frames_per_packet;
        let bytes_of_audio = total_parsed - data_offset;
        let frames_per_byte = frames_parsed as f64 / bytes_of_audio as f64;

        let audio_data_size = source_len.saturating_sub(data_offset);
        let total_frames = audio_data_size as f64 * frames_per_byte;
        let duration_secs = total_frames / f64::from(sample_rate);

        if duration_secs > 0.0 {
            Some(Duration::from_secs_f64(duration_secs))
        } else {
            None
        }
    }

    fn estimate_total_duration(&self) -> Duration {
        self.cached_duration
            .unwrap_or_else(|| self.compute_duration_from_ratio())
    }

    fn compute_duration_from_ratio(&self) -> Duration {
        if self.frames_decoded == 0 || self.source_byte_pos <= self.data_offset {
            return Duration::ZERO;
        }

        let Some(source_len) = self.source_len else {
            return Duration::ZERO;
        };

        let bytes_processed = self.source_byte_pos - self.data_offset;
        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for duration estimation"
        )]
        let frames_per_byte = self.frames_decoded as f64 / bytes_processed as f64;

        let audio_data_size = source_len.saturating_sub(self.data_offset);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for duration estimation"
        )]
        let estimated_total_frames = (audio_data_size as f64 * frames_per_byte) as u64;

        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for duration estimation"
        )]
        let duration_secs = estimated_total_frames as f64 / f64::from(self.spec.sample_rate);
        Duration::from_secs_f64(duration_secs)
    }
}

// SAFETY:
// - AudioToolbox handles (`AudioFileStreamID`, `AudioConverterRef`) are only touched
//   by methods that require thread affinity (`next_chunk`, `seek`, `query_prime_info`).
// - In debug builds we enforce this with `assert_thread_affinity()`.
// - Cross-thread update path (`update_byte_len`) only writes to an `AtomicU64` and does
//   not touch AudioToolbox state.
unsafe impl Send for AppleInner {}

impl Drop for AppleInner {
    fn drop(&mut self) {
        self.assert_thread_affinity();
        debug!("Apple decoder: disposing");
        // SAFETY: Both handles were obtained from their respective `Open`/`New` calls
        // and have not been disposed yet. `drop` runs at most once.
        unsafe {
            if !self.converter.is_null() {
                AudioConverterDispose(self.converter);
            }
            if !self.stream_parser.is_null() {
                AudioFileStreamClose(self.stream_parser);
            }
        }
    }
}
