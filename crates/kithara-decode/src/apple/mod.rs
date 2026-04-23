//! Apple `AudioToolbox` decoder backend.
//!
//! This module implements hardware-accelerated audio decoding using Apple's
//! `AudioToolbox` framework via `AudioFileStream` + `AudioConverter` APIs.
//!
//! Uses streaming approach: `AudioFileStream` parses container format,
//! `AudioConverter` decodes compressed audio packets to PCM.
//!
//! Supports AAC, MP3, FLAC, and ALAC codecs with hardware acceleration
//! when available on macOS and iOS.

#![allow(unsafe_code)]
#![allow(dead_code)]

mod consts;
mod converter;
mod ffi;
mod parser;

use std::{
    cell::Cell,
    ffi::c_void,
    fmt,
    io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    marker::PhantomData,
    mem::size_of,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use kithara_bufpool::PcmPool;
use kithara_stream::{AudioCodec, ContainerFormat};
use tracing::{debug, trace, warn};

use self::{
    consts::{Consts, os_status_to_string},
    converter::{ConverterInputState, converter_input_callback},
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterDispose, AudioConverterFillComplexBuffer,
        AudioConverterGetProperty, AudioConverterNew, AudioConverterPrimeInfo, AudioConverterRef,
        AudioConverterReset, AudioConverterSetProperty, AudioFileStreamClose, AudioFileStreamID,
        AudioFileStreamOpen, AudioFileStreamParseBytes, AudioFileTypeID,
        AudioStreamBasicDescription, UInt32,
    },
    parser::{
        StreamParserState, container_to_file_type, packets_callback, property_listener_callback,
    },
};
use crate::{
    error::{DecodeError, DecodeResult},
    hardware::{BoxedSource, RecoverableHardwareError, recoverable_hardware_error},
    traits::{Aac, Alac, AudioDecoder, CodecType, DecoderInput, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Resolve the source length, preferring the HTTP byte-length handle.
///
/// Falls back to seeking to end and back to start when no handle is
/// available. Returns `None` only when neither path yields a positive length.
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

/// Configuration for Apple `AudioToolbox` decoder.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppleConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    /// Container format hint for file type detection.
    pub(crate) container: Option<ContainerFormat>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub(crate) pcm_pool: Option<PcmPool>,
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
    prime_info: Option<AudioConverterPrimeInfo>,
    config: &'a AppleConfig,
}

/// Apple `AudioToolbox` streaming decoder inner state.
struct AppleInner {
    /// `AudioFileStream` parser.
    stream_parser: AudioFileStreamID,
    /// `AudioConverter` for decoding.
    converter: AudioConverterRef,
    /// Parser state (format, packets, etc.).
    parser_state: Box<StreamParserState>,
    /// Converter input state.
    converter_input: Box<ConverterInputState>,
    /// Source reader (supports both Read and Seek).
    source: BoxedSource,
    /// Read buffer for feeding parser.
    read_buffer: Vec<u8>,
    /// Audio spec.
    spec: PcmSpec,
    /// Current playback position.
    position: Duration,
    /// Absolute frame offset from the start of the track.
    frame_offset: u64,
    /// Total duration if known.
    duration: Option<Duration>,
    /// Track metadata.
    metadata: TrackMetadata,
    /// Byte length handle for HLS.
    byte_len_handle: Arc<AtomicU64>,
    /// Buffer for PCM output.
    pcm_buffer: Vec<f32>,
    /// True if we've reached end of source.
    source_eof: bool,
    /// Total frames decoded for position tracking.
    frames_decoded: u64,
    /// Total source length in bytes (for seek calculations).
    source_len: Option<u64>,
    /// Current byte position in source.
    source_byte_pos: u64,
    /// Data offset where audio data starts (from parser).
    data_offset: u64,
    /// Codec-reported priming info (encoder delay).
    prime_info: Option<AudioConverterPrimeInfo>,
    /// Cached duration estimate computed before the first seek (while
    /// `frames_decoded` and `source_byte_pos` are still consistent).
    cached_duration: Option<Duration>,
    /// PCM buffer pool (resolved from config or global).
    pool: PcmPool,
    /// Owner thread for FFI decoder state (set on first decoder operation in debug builds).
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

    /// Feed the parser until the format is ready or an error occurs.
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

    /// Apply the magic cookie to the converter, if one was collected.
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

    /// Query codec-reported priming (encoder delay) from a freshly created converter.
    fn query_initial_prime_info(converter: AudioConverterRef) -> Option<AudioConverterPrimeInfo> {
        let mut info = AudioConverterPrimeInfo::default();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "size_of result fits in u32"
        )]
        let mut size = size_of::<AudioConverterPrimeInfo>() as UInt32;
        // SAFETY: `converter` is a valid handle; `info` is a valid mutable reference to a repr(C) struct.
        let status = unsafe {
            AudioConverterGetProperty(
                converter,
                Consts::kAudioConverterPrimeInfo,
                &mut size,
                &mut info as *mut _ as *mut c_void,
            )
        };
        if status == Consts::noErr {
            debug!(
                leading_frames = info.leading_frames,
                trailing_frames = info.trailing_frames,
                "Apple decoder: prime info from codec"
            );
            Some(info)
        } else {
            debug!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: prime info not available"
            );
            None
        }
    }

    fn try_new(
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
        let prime_info = Self::query_initial_prime_info(converter);

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
            prime_info,
            config,
        }))
    }

    /// Open an `AudioFileStream` parser handle for the given file type.
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

    /// Build PCM output format and create an `AudioConverter` from the parsed input format.
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

    /// Finalize `AppleInner` construction once parser, converter, and format are ready.
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
            prime_info,
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
            prime_info,
            cached_duration,
            pool,
            owner_thread: Cell::new(None),
        }
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
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

    /// Resize `pcm_buffer` to fit one converter call and return the target output frame count.
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

    /// Run the `AudioConverter` once for the current staged packet.
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

    /// Copy decoded PCM into a pooled chunk and advance position tracking.
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

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
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

    /// Convert a playback position into an absolute frame index at the current sample rate.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seek target frame fits in u64 for realistic durations"
    )]
    fn target_frame_for(&self, pos: Duration) -> u64 {
        (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64
    }

    /// Move the source to `byte_offset`, reset decoder state, and feed a discontinuity chunk.
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

    /// Reset the converter and clear all buffered packet/converter state.
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

    /// Estimate byte offset from time position using bitrate calculation.
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

    /// Query `Consts::kAudioConverterPrimeInfo` from the converter.
    fn query_prime_info(&self) -> Option<(u32, u32)> {
        self.assert_thread_affinity();
        if self.converter.is_null() {
            return None;
        }
        let mut info = AudioConverterPrimeInfo::default();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "size_of result fits in u32"
        )]
        let mut size = size_of::<AudioConverterPrimeInfo>() as UInt32;
        // SAFETY: `self.converter` is a valid, non-null handle; `info` is a valid mutable repr(C) struct.
        let status = unsafe {
            AudioConverterGetProperty(
                self.converter,
                Consts::kAudioConverterPrimeInfo,
                &mut size,
                &mut info as *mut _ as *mut c_void,
            )
        };
        if status == Consts::noErr {
            Some((info.leading_frames, info.trailing_frames))
        } else {
            None
        }
    }

    /// Compute a duration estimate from the probing phase, where
    /// `packets_parsed × frames_per_packet / bytes_parsed` gives an
    /// accurate bitrate ratio.
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

    /// Return the best available duration estimate.
    fn estimate_total_duration(&self) -> Duration {
        self.cached_duration
            .unwrap_or_else(|| self.compute_duration_from_ratio())
    }

    /// Compute duration from the current `frames_decoded / bytes_processed` ratio.
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

/// Apple `AudioToolbox` streaming decoder parameterized by codec type.
///
/// Uses `AudioFileStream` for parsing and `AudioConverter` for decoding.
pub(crate) struct Apple<C: CodecType> {
    inner: AppleInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> fmt::Debug for Apple<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Apple")
            .field("spec", &self.inner.spec)
            .field("position", &self.inner.position)
            .field("duration", &self.inner.duration)
            .finish_non_exhaustive()
    }
}

impl<C: CodecType> Apple<C> {
    /// Returns the codec-reported encoder delay (leading priming frames)
    /// and trailing padding, if available.
    pub(crate) fn prime_info(&self) -> Option<(u32, u32)> {
        self.inner.query_prime_info()
    }
}

impl<C: CodecType> AudioDecoder for Apple<C> {
    type Config = AppleConfig;
    type Source = Box<dyn DecoderInput>;

    fn create(source: Self::Source, config: Self::Config) -> DecodeResult<Self>
    where
        Self: Sized,
    {
        debug!(codec = ?C::CODEC, "Apple decoder: create called");
        let inner = AppleInner::try_new(source, &config).map_err(|error| error.error)?;
        Ok(Self {
            inner,
            _codec: PhantomData,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

impl<C: CodecType> InnerDecoder for Apple<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.byte_len_handle.store(len, Ordering::Release);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

/// Apple AAC decoder.
pub(crate) type AppleAac = Apple<Aac>;

/// Apple MP3 decoder.
pub(crate) type AppleMp3 = Apple<Mp3>;

/// Apple FLAC decoder.
pub(crate) type AppleFlac = Apple<Flac>;

/// Apple ALAC decoder.
pub(crate) type AppleAlac = Apple<Alac>;

pub(crate) fn try_create_apple_decoder(
    codec: AudioCodec,
    source: BoxedSource,
    config: &AppleConfig,
) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            AppleInner::try_new(source, config).map(|inner| {
                Box::new(Apple::<Aac> {
                    inner,
                    _codec: PhantomData,
                }) as Box<dyn InnerDecoder>
            })
        }
        AudioCodec::Mp3 => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Mp3> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        AudioCodec::Flac => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Flac> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        AudioCodec::Alac => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Alac> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        _ => Err(recoverable_hardware_error(
            source,
            DecodeError::UnsupportedCodec(codec),
        )),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::default(None)]
    #[case::fmp4(Some(ContainerFormat::Fmp4))]
    fn test_apple_config_container(#[case] container: Option<ContainerFormat>) {
        let config = AppleConfig {
            container,
            pcm_pool: None,
            ..Default::default()
        };
        assert!(config.byte_len_handle.is_none());
        assert_eq!(config.container, container);
        assert!(config.pcm_pool.is_none());
    }

    #[kithara::test]
    fn test_type_aliases_exist() {
        fn _check_aac(_: AppleAac) {}
        fn _check_mp3(_: AppleMp3) {}
        fn _check_flac(_: AppleFlac) {}
        fn _check_alac(_: AppleAlac) {}
    }
}
