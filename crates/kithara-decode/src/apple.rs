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
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    fmt,
    io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    marker::PhantomData,
    mem::size_of,
    ptr, slice,
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

struct Consts;
impl Consts {
    const noErr: OSStatus = 0;
    const kAudioFileStreamError_NotOptimized: OSStatus = 0x6f707469;
    const kAudioConverterErr_NoDataNow: OSStatus = 0x21646174;
    const kAudioFormatLinearPCM: AudioFormatID = 0x6c70636d;
    const kAudioFormatMPEG4AAC: AudioFormatID = 0x61616320;
    const kAudioFormatMPEGLayer3: AudioFormatID = 0x2e6d7033;
    const kAudioFormatFLAC: AudioFormatID = 0x666c6163;
    const kAudioFormatAppleLossless: AudioFormatID = 0x616c6163;
    const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;
    const kAudioFileAAC_ADTSType: AudioFileTypeID = 0x61647473;
    const kAudioFileM4AType: AudioFileTypeID = 0x6d346166;
    const kAudioFileMP3Type: AudioFileTypeID = 0x4d504733;
    const kAudioFileFLACType: AudioFileTypeID = 0x666c6163;
    const kAudioFileCAFType: AudioFileTypeID = 0x63616666;
    const kAudioFileStreamProperty_ReadyToProducePackets: AudioFileStreamPropertyID = 0x72656479;
    const kAudioFileStreamProperty_DataFormat: AudioFileStreamPropertyID = 0x64666d74;
    const kAudioFileStreamProperty_MagicCookieData: AudioFileStreamPropertyID = 0x6d676963;
    const kAudioFileStreamProperty_DataOffset: AudioFileStreamPropertyID = 0x646f6666;
    const kAudioFileStreamProperty_AudioDataPacketCount: AudioFileStreamPropertyID = 0x70636e74;
    const kAudioFileStreamParseFlag_Discontinuity: UInt32 = 1;
    const BYTES_PER_F32_SAMPLE: u32 = 4;
    const BITS_PER_F32_SAMPLE: u32 = 32;
    const PARSE_READ_BUFFER_SIZE: usize = 32768;
    const MAX_PARSE_BYTES: usize = 1024 * 1024;
    const DEFAULT_BUFFER_FRAMES: usize = 1024;
    const MIN_PACKETS_FOR_DECODE: usize = 4;
    const DEFAULT_SEEK_DURATION_SECS: f64 = 300.0;
    const kAudioConverterDecompressionMagicCookie: u32 = 0x646d6763;
    const kAudioConverterPrimeInfo: u32 = 0x7072696d;
}

use crate::{
    error::{DecodeError, DecodeResult},
    hardware::{BoxedSource, RecoverableHardwareError, recoverable_hardware_error},
    traits::{Aac, Alac, AudioDecoder, CodecType, DecoderInput, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

type OSStatus = i32;
type AudioFileStreamID = *mut c_void;
type AudioFileTypeID = u32;
type AudioFileStreamPropertyID = u32;
type AudioFormatID = u32;
type AudioFormatFlags = u32;
type AudioConverterRef = *mut c_void;
type UInt32 = u32;
type SInt64 = i64;
type Float64 = f64;

// 'opti'
// '!dat' (2003329140 in decimal, but signed)

// Audio Format IDs
// 'lpcm'
// 'aac '
// '.mp3'
// 'flac'
// 'alac'

// Audio Format Flags

// File Type IDs
// 'adts'
// 'm4af'
// 'MPG3'
// 'flac'
// 'caff'

// AudioFileStream Property IDs
// 'redy'
// 'dfmt'
// 'mgic'
// 'doff'
// 'pcnt'

// AudioFileStream Parse Flags

// AudioStreamPacketDescription
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct AudioStreamPacketDescription {
    mStartOffset: SInt64,
    mVariableFramesInPacket: UInt32,
    mDataByteSize: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct AudioStreamBasicDescription {
    mSampleRate: Float64,
    mFormatID: AudioFormatID,
    mFormatFlags: AudioFormatFlags,
    mBytesPerPacket: UInt32,
    mFramesPerPacket: UInt32,
    mBytesPerFrame: UInt32,
    mChannelsPerFrame: UInt32,
    mBitsPerChannel: UInt32,
    mReserved: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AudioBuffer {
    mNumberChannels: UInt32,
    mDataByteSize: UInt32,
    mData: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    mNumberBuffers: UInt32,
    mBuffers: [AudioBuffer; 1],
}

// Callback types for AudioFileStream
type AudioFileStream_PropertyListenerProc = extern "C" fn(
    inClientData: *mut c_void,
    inAudioFileStream: AudioFileStreamID,
    inPropertyID: AudioFileStreamPropertyID,
    ioFlags: *mut UInt32,
);

type AudioFileStream_PacketsProc = extern "C" fn(
    inClientData: *mut c_void,
    inNumberBytes: UInt32,
    inNumberPackets: UInt32,
    inInputData: *const c_void,
    inPacketDescriptions: *mut AudioStreamPacketDescription,
);

// Callback type for AudioConverter
type AudioConverterComplexInputDataProc = extern "C" fn(
    inAudioConverter: AudioConverterRef,
    ioNumberDataPackets: *mut UInt32,
    ioData: *mut AudioBufferList,
    outDataPacketDescription: *mut *mut AudioStreamPacketDescription,
    inUserData: *mut c_void,
) -> OSStatus;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    // AudioFileStream functions
    fn AudioFileStreamOpen(
        inClientData: *mut c_void,
        inPropertyListenerProc: AudioFileStream_PropertyListenerProc,
        inPacketsProc: AudioFileStream_PacketsProc,
        inFileTypeHint: AudioFileTypeID,
        outAudioFileStream: *mut AudioFileStreamID,
    ) -> OSStatus;

    fn AudioFileStreamParseBytes(
        inAudioFileStream: AudioFileStreamID,
        inDataByteSize: UInt32,
        inData: *const c_void,
        inFlags: UInt32,
    ) -> OSStatus;

    fn AudioFileStreamGetPropertyInfo(
        inAudioFileStream: AudioFileStreamID,
        inPropertyID: AudioFileStreamPropertyID,
        outPropertyDataSize: *mut UInt32,
        outWritable: *mut u8,
    ) -> OSStatus;

    fn AudioFileStreamGetProperty(
        inAudioFileStream: AudioFileStreamID,
        inPropertyID: AudioFileStreamPropertyID,
        ioPropertyDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    fn AudioFileStreamSeek(
        inAudioFileStream: AudioFileStreamID,
        inPacketOffset: SInt64,
        outDataByteOffset: *mut SInt64,
        ioFlags: *mut UInt32,
    ) -> OSStatus;

    fn AudioFileStreamClose(inAudioFileStream: AudioFileStreamID) -> OSStatus;

    // AudioConverter functions
    fn AudioConverterNew(
        inSourceFormat: *const AudioStreamBasicDescription,
        inDestinationFormat: *const AudioStreamBasicDescription,
        outAudioConverter: *mut AudioConverterRef,
    ) -> OSStatus;

    fn AudioConverterSetProperty(
        inAudioConverter: AudioConverterRef,
        inPropertyID: u32,
        inPropertyDataSize: UInt32,
        inPropertyData: *const c_void,
    ) -> OSStatus;

    fn AudioConverterFillComplexBuffer(
        inAudioConverter: AudioConverterRef,
        inInputDataProc: AudioConverterComplexInputDataProc,
        inInputDataProcUserData: *mut c_void,
        ioOutputDataPacketSize: *mut UInt32,
        outOutputData: *mut AudioBufferList,
        outPacketDescription: *mut AudioStreamPacketDescription,
    ) -> OSStatus;

    fn AudioConverterGetProperty(
        inAudioConverter: AudioConverterRef,
        inPropertyID: u32,
        ioPropertyDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    fn AudioConverterDispose(inAudioConverter: AudioConverterRef) -> OSStatus;

    fn AudioConverterReset(inAudioConverter: AudioConverterRef) -> OSStatus;
}

// AudioConverter Property IDs
// 'dmgc'
// 'prim'

/// Priming information reported by `AudioConverter`.
///
/// `leading_frames` is the encoder delay (priming samples the decoder produces
/// before real audio). `trailing_frames` is the padding at the end.
/// These values come directly from the codec — no heuristics.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct AudioConverterPrimeInfo {
    leading_frames: u32,
    trailing_frames: u32,
}

fn os_status_to_string(status: OSStatus) -> String {
    if status == Consts::noErr {
        return "Consts::noErr".to_string();
    }
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        let s: String = bytes.iter().map(|&b| b as char).collect();
        format!("'{}' ({})", s, status)
    } else {
        format!("{}", status)
    }
}

fn container_to_file_type(container: ContainerFormat) -> Option<AudioFileTypeID> {
    match container {
        ContainerFormat::Fmp4 => Some(Consts::kAudioFileM4AType),
        ContainerFormat::Adts => Some(Consts::kAudioFileAAC_ADTSType),
        ContainerFormat::MpegAudio => Some(Consts::kAudioFileMP3Type),
        ContainerFormat::Flac => Some(Consts::kAudioFileFLACType),
        ContainerFormat::Caf => Some(Consts::kAudioFileCAFType),
        _ => None,
    }
}

/// A compressed audio packet with its description.
#[derive(Clone)]
struct AudioPacket {
    data: Vec<u8>,
    description: AudioStreamPacketDescription,
}

/// Thread-safe packet buffer for storing compressed audio packets.
struct PacketBuffer {
    packets: VecDeque<AudioPacket>,
}

impl PacketBuffer {
    fn new() -> Self {
        Self {
            packets: VecDeque::new(),
        }
    }

    fn push(&mut self, packet: AudioPacket) {
        self.packets.push_back(packet);
    }

    fn pop(&mut self) -> Option<AudioPacket> {
        self.packets.pop_front()
    }

    fn len(&self) -> usize {
        self.packets.len()
    }

    fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

/// State shared between `AudioFileStream` callbacks and the decoder.
struct StreamParserState {
    /// Audio format from the stream.
    format: Option<AudioStreamBasicDescription>,
    /// Magic cookie data for codec initialization.
    magic_cookie: Option<Vec<u8>>,
    /// Buffered compressed packets from callbacks.
    packet_buffer: PacketBuffer,
    /// Data offset in stream (where audio data starts).
    data_offset: SInt64,
    /// Total audio data packet count (from Xing/VBRI header via `AudioFileStream`).
    audio_data_packet_count: Option<u64>,
    /// True when stream is ready to produce packets.
    ready: bool,
    /// Error from callbacks.
    error: Option<String>,
}

impl StreamParserState {
    fn new() -> Self {
        Self {
            format: None,
            magic_cookie: None,
            packet_buffer: PacketBuffer::new(),
            data_offset: 0,
            audio_data_packet_count: None,
            ready: false,
            error: None,
        }
    }
}

/// State for `AudioConverter` input callback.
struct ConverterInputState {
    /// Current packet being provided to converter.
    current_packet: Option<AudioPacket>,
    /// Packet description for current packet.
    packet_desc: AudioStreamPacketDescription,
    /// Holds packet data while converter uses it (prevents memory leak).
    held_packet_data: Option<Vec<u8>>,
}

impl ConverterInputState {
    fn new() -> Self {
        Self {
            current_packet: None,
            packet_desc: AudioStreamPacketDescription::default(),
            held_packet_data: None,
        }
    }
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

    #[expect(
        clippy::cognitive_complexity,
        reason = "FFI initialization sequence is inherently sequential"
    )]
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

        // Get source length for duration estimation and seek calculations.
        // Prefer byte_len_handle (Content-Length from HTTP) over seek(End(0))
        // because streaming sources may have only a fraction downloaded.
        let source_len = config
            .byte_len_handle
            .as_ref()
            .map(|h| h.load(Ordering::Acquire))
            .filter(|&len| len > 0)
            .or_else(|| {
                let len = source.seek(SeekFrom::End(0)).ok()?;
                source.seek(SeekFrom::Start(0)).ok()?;
                Some(len)
            });

        debug!(
            ?container,
            file_type,
            ?source_len,
            "Apple decoder: creating streaming decoder"
        );

        // Create parser state
        let mut parser_state = Box::new(StreamParserState::new());
        let state_ptr = parser_state.as_mut() as *mut StreamParserState as *mut c_void;

        // Open AudioFileStream
        let mut stream_parser: AudioFileStreamID = ptr::null_mut();
        // SAFETY: `state_ptr` points to a live, pinned `StreamParserState` owned by `parser_state`.
        // The callbacks receive this pointer and cast it back; the Box keeps it stable for the
        // lifetime of the parser.
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
            return Err(recoverable_hardware_error(
                source,
                DecodeError::Backend(Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    format!("AudioFileStreamOpen failed: {}", err_str),
                ))),
            ));
        }

        debug!("Apple decoder: AudioFileStream opened");

        // Read initial data to get format info
        let mut read_buffer = vec![0u8; Consts::PARSE_READ_BUFFER_SIZE];
        let mut total_parsed = 0usize;

        // Parse until we have format info or hit EOF
        loop {
            let n = match source.read(&mut read_buffer) {
                Ok(n) => n,
                Err(error) => {
                    // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                    unsafe {
                        AudioFileStreamClose(stream_parser);
                    }
                    return Err(recoverable_hardware_error(
                        source,
                        DecodeError::Backend(Box::new(error)),
                    ));
                }
            };

            if n == 0 {
                // EOF before getting format
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(recoverable_hardware_error(
                    source,
                    DecodeError::InvalidData("EOF before audio format detected".into()),
                ));
            }

            total_parsed += n;

            // SAFETY: `stream_parser` is a valid handle; `read_buffer` is a live slice with `n` valid bytes.
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
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                let err_str = os_status_to_string(status);
                warn!(status, err = %err_str, "Apple decoder: AudioFileStreamParseBytes failed");
                return Err(recoverable_hardware_error(
                    source,
                    DecodeError::Backend(Box::new(IoError::new(
                        ErrorKind::InvalidData,
                        format!("AudioFileStreamParseBytes failed: {}", err_str),
                    ))),
                ));
            }

            // Check if we have format now
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
                break;
            }

            // Check for callback error
            if let Some(ref err) = parser_state.error {
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(recoverable_hardware_error(
                    source,
                    DecodeError::InvalidData(err.clone()),
                ));
            }

            // Safety limit
            if total_parsed > Consts::MAX_PARSE_BYTES {
                // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(recoverable_hardware_error(
                    source,
                    DecodeError::InvalidData("Could not detect format after 1MB".into()),
                ));
            }
        }

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

        // Create output format (PCM f32 interleaved)
        let output_format = AudioStreamBasicDescription {
            mSampleRate: format.mSampleRate,
            mFormatID: Consts::kAudioFormatLinearPCM,
            mFormatFlags: Consts::kAudioFormatFlagsNativeFloatPacked,
            mBytesPerPacket: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mFramesPerPacket: 1,
            mBytesPerFrame: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mChannelsPerFrame: u32::from(channels),
            mBitsPerChannel: Consts::BITS_PER_F32_SAMPLE,
            mReserved: 0,
        };

        // Create AudioConverter
        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: `format` and `output_format` are valid C structs on the stack; `converter` receives the new handle.
        let status = unsafe { AudioConverterNew(&format, &output_format, &mut converter) };

        if status != Consts::noErr {
            // SAFETY: `stream_parser` is a valid handle from `AudioFileStreamOpen`.
            unsafe {
                AudioFileStreamClose(stream_parser);
            }
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioConverterNew failed");
            return Err(recoverable_hardware_error(
                source,
                DecodeError::Backend(Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    format!("AudioConverterNew failed: {}", err_str),
                ))),
            ));
        }

        debug!("Apple decoder: AudioConverter created");

        // Set magic cookie if available (required for AAC)
        if let Some(ref cookie) = parser_state.magic_cookie {
            // SAFETY: `converter` is a valid handle; `cookie` is a live slice.
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

            if status != Consts::noErr {
                warn!(
                    status,
                    err = %os_status_to_string(status),
                    "Apple decoder: failed to set magic cookie (continuing anyway)"
                );
            } else {
                debug!(
                    cookie_size = cookie.len(),
                    "Apple decoder: magic cookie set"
                );
            }
        }

        // Query codec-reported priming (encoder delay).
        let prime_info = {
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
        };

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

        // Get data offset from parser state
        let data_offset = parser_state.data_offset.cast_unsigned();

        let cached_duration = Self::initial_duration_estimate(
            &parser_state,
            total_parsed as u64,
            data_offset,
            source_len,
            sample_rate,
        );

        Ok(Self {
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
            source_byte_pos: total_parsed as u64,
            data_offset,
            prime_info,
            cached_duration,
            pool,
            owner_thread: Cell::new(None),
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.assert_thread_affinity();
        loop {
            // Ensure we have packets to decode
            while self.parser_state.packet_buffer.len() < Consts::MIN_PACKETS_FOR_DECODE
                && !self.source_eof
            {
                self.feed_parser()?;
            }

            // Check if we have any packets
            if self.parser_state.packet_buffer.is_empty() && self.source_eof {
                debug!(
                    position = ?self.position,
                    frames_decoded = self.frames_decoded,
                    "Apple decoder: EOF reached"
                );
                return Ok(None);
            }

            // Get next packet for converter
            let Some(packet) = self.parser_state.packet_buffer.pop() else {
                return Ok(None);
            };

            // Set up converter input
            self.converter_input.current_packet = Some(packet);

            // Prepare output buffer
            let channels = self.spec.channels as usize;
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
            let input_ptr =
                self.converter_input.as_mut() as *mut ConverterInputState as *mut c_void;

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

            // Converter needs more input packets.
            if status == Consts::kAudioConverterErr_NoDataNow {
                trace!("Apple decoder: converter needs more data");
                continue;
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
                // No output yet, need more input.
                continue;
            }

            let frames = output_packets as usize;
            let samples = frames * channels;

            // Decode into a pool-backed buffer (reuses allocations).
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

            // Update position and frame offset
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

            return Ok(Some(chunk));
        }
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

        // SAFETY: `self.stream_parser` is a valid handle; `self.read_buffer` is a live slice with `n` valid bytes.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "read buffer is 32 KB, fits in u32"
        )]
        // SAFETY: `stream_parser` is a valid handle. `read_buffer` contains `n` bytes.
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
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "seek target frame fits in u64"
        )]
        let target_frame = (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;

        debug!(
            position_secs = pos.as_secs_f64(),
            target_frame,
            current_frame = self.frames_decoded,
            "Apple decoder: seek requested"
        );

        // Calculate approximate byte offset using bitrate estimation
        let byte_offset = self.estimate_byte_offset(pos);

        debug!(
            estimated_byte_offset = byte_offset,
            data_offset = self.data_offset,
            "Apple decoder: seeking to byte offset"
        );

        // Seek in source
        self.source
            .seek(SeekFrom::Start(byte_offset))
            .map_err(|e| DecodeError::SeekFailed(format!("Source seek failed: {}", e)))?;

        self.source_byte_pos = byte_offset;

        // Reset converter
        // SAFETY: `self.converter` is a valid handle from `AudioConverterNew`.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            warn!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: converter reset failed"
            );
        }

        // Clear packet buffer
        while self.parser_state.packet_buffer.pop().is_some() {}

        // Clear converter input state
        self.converter_input.current_packet = None;
        self.converter_input.held_packet_data = None;

        // Reset EOF flag
        self.source_eof = false;

        // Parse with discontinuity flag to signal decoder reset
        self.feed_parser_with_flags(Consts::kAudioFileStreamParseFlag_Discontinuity)?;

        // Update position tracking
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

    /// Estimate byte offset from time position using bitrate calculation.
    fn estimate_byte_offset(&self, pos: Duration) -> u64 {
        let Some(source_len) = self.source_len else {
            // No source length known, start from data offset
            return self.data_offset;
        };

        // Get audio data size (total - header)
        let audio_data_size = source_len.saturating_sub(self.data_offset);
        if audio_data_size == 0 {
            return self.data_offset;
        }

        // Estimate total duration from format info
        let total_duration = self.estimate_total_duration();
        if total_duration.as_secs_f64() <= 0.0 {
            // Can't estimate, use linear interpolation
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

        // Calculate byte offset proportionally
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
    /// accurate bitrate ratio.  Returns `None` when there is not enough
    /// data (e.g. streaming source with unknown length).
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

        // Prefer exact packet count from AudioFileStream (Xing/VBRI header).
        if let Some(packet_count) = parser_state.audio_data_packet_count {
            let total_frames = packet_count * frames_per_packet;
            let duration_secs = total_frames as f64 / f64::from(sample_rate);
            if duration_secs > 0.0 {
                return Some(Duration::from_secs_f64(duration_secs));
            }
        }

        // Fallback: estimate from bytes/frames ratio (CBR or VBR without header).
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
    ///
    /// Prefers the cached value (computed before any seek corrupts the
    /// `frames_decoded / source_byte_pos` ratio).  Falls back to a
    /// live computation when no cache is available yet.
    fn estimate_total_duration(&self) -> Duration {
        self.cached_duration
            .unwrap_or_else(|| self.compute_duration_from_ratio())
    }

    /// Compute duration from the current `frames_decoded / bytes_processed` ratio.
    ///
    /// Only reliable before the first seek — afterwards `frames_decoded`
    /// and `source_byte_pos` become inconsistent (seek sets `frames_decoded`
    /// to the target frame but `source_byte_pos` to the estimated byte offset).
    fn compute_duration_from_ratio(&self) -> Duration {
        if self.frames_decoded == 0 || self.source_byte_pos <= self.data_offset {
            return Duration::ZERO;
        }

        let Some(source_len) = self.source_len else {
            return Duration::ZERO;
        };

        // Calculate frames per byte ratio
        let bytes_processed = self.source_byte_pos - self.data_offset;
        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for duration estimation"
        )]
        let frames_per_byte = self.frames_decoded as f64 / bytes_processed as f64;

        // Estimate total frames
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

/// `AudioFileStream` property listener callback.
#[expect(
    clippy::cognitive_complexity,
    reason = "extern C callback; match arms are independent"
)]
extern "C" fn property_listener_callback(
    client_data: *mut c_void,
    audio_file_stream: AudioFileStreamID,
    property_id: AudioFileStreamPropertyID,
    _flags: *mut UInt32,
) {
    // SAFETY: `client_data` was set to a valid `StreamParserState` pointer in `AudioFileStreamOpen`.
    // The Box that owns it is alive for the lifetime of the parser.
    let state = unsafe { &mut *(client_data as *mut StreamParserState) };

    match property_id {
        Consts::kAudioFileStreamProperty_DataFormat => {
            let mut format = AudioStreamBasicDescription::default();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "size_of result fits in u32"
            )]
            let mut size = size_of::<AudioStreamBasicDescription>() as UInt32;

            // SAFETY: `audio_file_stream` is a valid handle; `format` is a valid mutable repr(C) struct.
            let status = unsafe {
                AudioFileStreamGetProperty(
                    audio_file_stream,
                    Consts::kAudioFileStreamProperty_DataFormat,
                    &mut size,
                    &mut format as *mut _ as *mut c_void,
                )
            };

            if status == Consts::noErr {
                state.format = Some(format);
                trace!(
                    sample_rate = format.mSampleRate,
                    channels = format.mChannelsPerFrame,
                    format_id = format.mFormatID,
                    "Apple decoder callback: format received"
                );
            } else {
                state.error = Some(format!(
                    "Failed to get format: {}",
                    os_status_to_string(status)
                ));
            }
        }
        Consts::kAudioFileStreamProperty_MagicCookieData => {
            let mut size: UInt32 = 0;
            let mut writable: u8 = 0;

            // SAFETY: `audio_file_stream` is a valid handle; out-params are valid mutable references.
            let status = unsafe {
                AudioFileStreamGetPropertyInfo(
                    audio_file_stream,
                    Consts::kAudioFileStreamProperty_MagicCookieData,
                    &mut size,
                    &mut writable,
                )
            };

            if status == Consts::noErr && size > 0 {
                let mut cookie = vec![0u8; size as usize];
                // SAFETY: `audio_file_stream` is a valid handle; `cookie` is a live buffer of `size` bytes.
                let status = unsafe {
                    AudioFileStreamGetProperty(
                        audio_file_stream,
                        Consts::kAudioFileStreamProperty_MagicCookieData,
                        &mut size,
                        cookie.as_mut_ptr() as *mut c_void,
                    )
                };

                if status == Consts::noErr {
                    state.magic_cookie = Some(cookie);
                    trace!(size, "Apple decoder callback: magic cookie received");
                }
            }
        }
        Consts::kAudioFileStreamProperty_AudioDataPacketCount => {
            let mut count: SInt64 = 0;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "size_of result fits in u32"
            )]
            let mut size = size_of::<SInt64>() as UInt32;

            // SAFETY: `audio_file_stream` is a valid handle; `count` is a valid mutable SInt64.
            let status = unsafe {
                AudioFileStreamGetProperty(
                    audio_file_stream,
                    Consts::kAudioFileStreamProperty_AudioDataPacketCount,
                    &mut size,
                    &mut count as *mut _ as *mut c_void,
                )
            };

            if status == Consts::noErr && count > 0 {
                state.audio_data_packet_count = Some(count.cast_unsigned());
                trace!(count, "Apple decoder callback: audio data packet count");
            }
        }
        Consts::kAudioFileStreamProperty_ReadyToProducePackets => {
            state.ready = true;
            trace!("Apple decoder callback: ready to produce packets");
        }
        Consts::kAudioFileStreamProperty_DataOffset => {
            let mut offset: SInt64 = 0;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "size_of result fits in u32"
            )]
            let mut size = size_of::<SInt64>() as UInt32;

            // SAFETY: `audio_file_stream` is a valid handle; `offset` is a valid mutable SInt64.
            let status = unsafe {
                AudioFileStreamGetProperty(
                    audio_file_stream,
                    Consts::kAudioFileStreamProperty_DataOffset,
                    &mut size,
                    &mut offset as *mut _ as *mut c_void,
                )
            };

            if status == Consts::noErr {
                state.data_offset = offset;
                trace!(offset, "Apple decoder callback: data offset");
            }
        }
        _ => {
            // Ignore other properties
        }
    }
}

/// `AudioFileStream` packets callback.
extern "C" fn packets_callback(
    client_data: *mut c_void,
    num_bytes: UInt32,
    num_packets: UInt32,
    input_data: *const c_void,
    packet_descriptions: *mut AudioStreamPacketDescription,
) {
    // SAFETY: `client_data` was set to a valid `StreamParserState` pointer in `AudioFileStreamOpen`.
    let state = unsafe { &mut *(client_data as *mut StreamParserState) };

    if num_packets == 0 {
        return;
    }

    // SAFETY: `input_data` is a valid buffer of `num_bytes` bytes provided by AudioFileStream.
    let data_slice = unsafe { slice::from_raw_parts(input_data as *const u8, num_bytes as usize) };

    if packet_descriptions.is_null() {
        // CBR format - single packet with all data
        let packet = AudioPacket {
            data: data_slice.to_vec(),
            description: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: num_bytes,
            },
        };
        state.packet_buffer.push(packet);
    } else {
        // VBR format - multiple packets with descriptions
        // SAFETY: `packet_descriptions` is a valid array of `num_packets` elements provided by AudioFileStream.
        let descriptions =
            unsafe { slice::from_raw_parts(packet_descriptions, num_packets as usize) };

        for desc in descriptions {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "mStartOffset is a byte offset within the packet buffer, fits in usize"
            )]
            let start = desc.mStartOffset as usize;
            let size = desc.mDataByteSize as usize;

            if start + size <= data_slice.len() {
                let packet = AudioPacket {
                    data: data_slice[start..start + size].to_vec(),
                    description: *desc,
                };
                state.packet_buffer.push(packet);
            }
        }
    }

    trace!(
        num_packets,
        num_bytes,
        buffered = state.packet_buffer.len(),
        "Apple decoder callback: packets received"
    );
}

/// `AudioConverter` input data callback.
extern "C" fn converter_input_callback(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    // SAFETY: `user_data` was set to a valid `ConverterInputState` pointer by the caller.
    let state = unsafe { &mut *(user_data as *mut ConverterInputState) };

    let Some(packet) = state.current_packet.take() else {
        // No data available
        // SAFETY: `io_num_packets` is a valid mutable pointer provided by AudioConverter.
        unsafe {
            *io_num_packets = 0;
        }
        return Consts::kAudioConverterErr_NoDataNow;
    };

    // Store packet data in state to keep it alive during conversion
    // (will be dropped on next call or when state is dropped)
    let data_len = packet.data.len();
    state.held_packet_data = Some(packet.data);

    // Provide packet data pointer (now pointing to held_packet_data)
    // SAFETY: `io_data` and `io_num_packets` are valid pointers provided by AudioConverter.
    // `held_packet_data` was set to `Some` on the line above, so `unwrap` cannot panic.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "packet data length fits in u32"
    )]
    // SAFETY: `io_data` is a valid pointer provided by `AudioConverter` callback.
    // `held_packet_data` was set to `Some` two lines above.
    unsafe {
        #[expect(
            clippy::unwrap_used,
            reason = "held_packet_data was set to Some two lines above"
        )]
        let data_ptr = state.held_packet_data.as_ref().unwrap().as_ptr();
        (*io_data).mBuffers[0].mDataByteSize = data_len as UInt32;
        (*io_data).mBuffers[0].mData = data_ptr as *mut c_void;
        *io_num_packets = 1;

        // Provide packet description if requested
        if !out_packet_desc.is_null() {
            state.packet_desc = AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: packet.description.mVariableFramesInPacket,
                mDataByteSize: data_len as UInt32,
            };
            *out_packet_desc = &mut state.packet_desc;
        }
    }

    Consts::noErr
}

/// Apple `AudioToolbox` streaming decoder parameterized by codec type.
///
/// Uses `AudioFileStream` for parsing and `AudioConverter` for decoding.
/// This approach supports streaming data without requiring the full file.
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
    ///
    /// This is the same information `AVPlayer` uses internally via
    /// `Consts::kAudioConverterPrimeInfo`. Values come directly from the codec,
    /// not from container metadata or audio analysis.
    ///
    /// Re-queries the converter each time, since `PrimeInfo` may become
    /// available only after some packets have been decoded.
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
    #[case::fmp4(ContainerFormat::Fmp4, Some(Consts::kAudioFileM4AType))]
    #[case::adts(ContainerFormat::Adts, Some(Consts::kAudioFileAAC_ADTSType))]
    #[case::mpeg(ContainerFormat::MpegAudio, Some(Consts::kAudioFileMP3Type))]
    #[case::flac(ContainerFormat::Flac, Some(Consts::kAudioFileFLACType))]
    #[case::caf(ContainerFormat::Caf, Some(Consts::kAudioFileCAFType))]
    #[case::wav(ContainerFormat::Wav, None)]
    fn test_container_to_file_type(
        #[case] container: ContainerFormat,
        #[case] expected: Option<AudioFileTypeID>,
    ) {
        assert_eq!(container_to_file_type(container), expected);
    }

    #[kithara::test]
    fn test_os_status_to_string() {
        assert_eq!(os_status_to_string(Consts::noErr), "Consts::noErr");
        // 'wht?' = 0x7768743f
        assert!(os_status_to_string(0x7768743f).contains("wht?"));
    }

    #[kithara::test]
    fn test_type_aliases_exist() {
        fn _check_aac(_: AppleAac) {}
        fn _check_mp3(_: AppleMp3) {}
        fn _check_flac(_: AppleFlac) {}
        fn _check_alac(_: AppleAlac) {}
    }
}
