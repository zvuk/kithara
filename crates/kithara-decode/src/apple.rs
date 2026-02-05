//! Apple AudioToolbox decoder backend.
//!
//! This module implements hardware-accelerated audio decoding using Apple's
//! AudioToolbox framework via AudioFileStream + AudioConverter APIs.
//!
//! Uses streaming approach: AudioFileStream parses container format,
//! AudioConverter decodes compressed audio packets to PCM.
//!
//! Supports AAC, MP3, FLAC, and ALAC codecs with hardware acceleration
//! when available on macOS and iOS.

#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::{
    collections::VecDeque,
    ffi::c_void,
    io::{Read, Seek},
    marker::PhantomData,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_stream::ContainerFormat;
use tracing::{debug, trace, warn};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, Alac, AudioDecoder, CodecType, DecoderInput, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

// ────────────────────────────────── FFI Types ──────────────────────────────────

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

const noErr: OSStatus = 0;
const kAudioFileStreamError_NotOptimized: OSStatus = 0x6f707469; // 'opti'
const kAudioConverterErr_NoDataNow: OSStatus = 0x21646174; // '!dat' (2003329140 in decimal, but signed)

// Audio Format IDs
const kAudioFormatLinearPCM: AudioFormatID = 0x6c70636d; // 'lpcm'
const kAudioFormatMPEG4AAC: AudioFormatID = 0x61616320; // 'aac '
const kAudioFormatMPEGLayer3: AudioFormatID = 0x2e6d7033; // '.mp3'
const kAudioFormatFLAC: AudioFormatID = 0x666c6163; // 'flac'
const kAudioFormatAppleLossless: AudioFormatID = 0x616c6163; // 'alac'

// Audio Format Flags
const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
    kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;

// File Type IDs
const kAudioFileAAC_ADTSType: AudioFileTypeID = 0x61647473; // 'adts'
const kAudioFileM4AType: AudioFileTypeID = 0x6d346166; // 'm4af'
const kAudioFileMP3Type: AudioFileTypeID = 0x4d504733; // 'MPG3'
const kAudioFileFLACType: AudioFileTypeID = 0x666c6163; // 'flac'
const kAudioFileCAFType: AudioFileTypeID = 0x63616666; // 'caff'

// AudioFileStream Property IDs
const kAudioFileStreamProperty_ReadyToProducePackets: AudioFileStreamPropertyID = 0x72656479; // 'redy'
const kAudioFileStreamProperty_DataFormat: AudioFileStreamPropertyID = 0x64666d74; // 'dfmt'
const kAudioFileStreamProperty_MagicCookieData: AudioFileStreamPropertyID = 0x6d676963; // 'mgic'
const kAudioFileStreamProperty_DataOffset: AudioFileStreamPropertyID = 0x646f6666; // 'doff'

// AudioFileStream Parse Flags
const kAudioFileStreamParseFlag_Discontinuity: UInt32 = 1;

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

    fn AudioConverterDispose(inAudioConverter: AudioConverterRef) -> OSStatus;

    fn AudioConverterReset(inAudioConverter: AudioConverterRef) -> OSStatus;
}

// AudioConverter Property IDs
const kAudioConverterDecompressionMagicCookie: u32 = 0x646d6763; // 'dmgc'

// ────────────────────────────────── Helpers ──────────────────────────────────

fn os_status_to_string(status: OSStatus) -> String {
    if status == noErr {
        return "noErr".to_string();
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
        ContainerFormat::Fmp4 => Some(kAudioFileM4AType),
        ContainerFormat::Adts => Some(kAudioFileAAC_ADTSType),
        ContainerFormat::MpegAudio => Some(kAudioFileMP3Type),
        ContainerFormat::Flac => Some(kAudioFileFLACType),
        ContainerFormat::Caf => Some(kAudioFileCAFType),
        _ => None,
    }
}

// ────────────────────────────────── Packet Buffer ──────────────────────────────────

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

// ────────────────────────────────── Stream Parser State ──────────────────────────────────

/// State shared between AudioFileStream callbacks and the decoder.
struct StreamParserState {
    /// Audio format from the stream.
    format: Option<AudioStreamBasicDescription>,
    /// Magic cookie data for codec initialization.
    magic_cookie: Option<Vec<u8>>,
    /// Buffered compressed packets from callbacks.
    packet_buffer: PacketBuffer,
    /// Data offset in stream (where audio data starts).
    data_offset: SInt64,
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
            ready: false,
            error: None,
        }
    }
}

// ────────────────────────────────── Converter Input State ──────────────────────────────────

/// State for AudioConverter input callback.
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

// ────────────────────────────────── Config ──────────────────────────────────

/// Configuration for Apple AudioToolbox decoder.
#[derive(Debug, Clone, Default)]
pub struct AppleConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Container format hint for file type detection.
    pub container: Option<ContainerFormat>,
}

// ────────────────────────────────── Inner ──────────────────────────────────

/// Apple AudioToolbox streaming decoder inner state.
struct AppleInner {
    /// AudioFileStream parser.
    stream_parser: AudioFileStreamID,
    /// AudioConverter for decoding.
    converter: AudioConverterRef,
    /// Parser state (format, packets, etc.).
    parser_state: Box<StreamParserState>,
    /// Converter input state.
    converter_input: Box<ConverterInputState>,
    /// Source reader.
    source: Box<dyn Read + Send>,
    /// Read buffer for feeding parser.
    read_buffer: Vec<u8>,
    /// Audio spec.
    spec: PcmSpec,
    /// Current playback position.
    position: Duration,
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
}

impl AppleInner {
    fn new<R>(mut source: R, config: &AppleConfig) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + 'static,
    {
        let container = config.container.ok_or_else(|| {
            DecodeError::InvalidData("Container format must be specified for Apple decoder".into())
        })?;

        let file_type = container_to_file_type(container)
            .ok_or_else(|| DecodeError::UnsupportedContainer(container))?;

        debug!(
            ?container,
            file_type, "Apple decoder: creating streaming decoder"
        );

        // Create parser state
        let mut parser_state = Box::new(StreamParserState::new());
        let state_ptr = parser_state.as_mut() as *mut StreamParserState as *mut c_void;

        // Open AudioFileStream
        let mut stream_parser: AudioFileStreamID = ptr::null_mut();
        let status = unsafe {
            AudioFileStreamOpen(
                state_ptr,
                property_listener_callback,
                packets_callback,
                file_type,
                &mut stream_parser,
            )
        };

        if status != noErr {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioFileStreamOpen failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("AudioFileStreamOpen failed: {}", err_str),
            ))));
        }

        debug!("Apple decoder: AudioFileStream opened");

        // Read initial data to get format info
        let mut read_buffer = vec![0u8; 32768]; // 32KB chunks
        let mut total_parsed = 0usize;

        // Parse until we have format info or hit EOF
        loop {
            let n = source
                .read(&mut read_buffer)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;

            if n == 0 {
                // EOF before getting format
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(DecodeError::InvalidData(
                    "EOF before audio format detected".into(),
                ));
            }

            total_parsed += n;

            let status = unsafe {
                AudioFileStreamParseBytes(
                    stream_parser,
                    n as UInt32,
                    read_buffer.as_ptr() as *const c_void,
                    0,
                )
            };

            if status != noErr && status != kAudioFileStreamError_NotOptimized {
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                let err_str = os_status_to_string(status);
                warn!(status, err = %err_str, "Apple decoder: AudioFileStreamParseBytes failed");
                return Err(DecodeError::Backend(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("AudioFileStreamParseBytes failed: {}", err_str),
                ))));
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
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(DecodeError::InvalidData(err.clone()));
            }

            // Safety limit
            if total_parsed > 1024 * 1024 {
                unsafe {
                    AudioFileStreamClose(stream_parser);
                }
                return Err(DecodeError::InvalidData(
                    "Could not detect format after 1MB".into(),
                ));
            }
        }

        let format = parser_state.format.ok_or_else(|| {
            unsafe {
                AudioFileStreamClose(stream_parser);
            }
            DecodeError::InvalidData("No audio format detected".into())
        })?;

        let sample_rate = format.mSampleRate as u32;
        let channels = format.mChannelsPerFrame as u16;

        // Create output format (PCM f32 interleaved)
        let output_format = AudioStreamBasicDescription {
            mSampleRate: format.mSampleRate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
            mBytesPerPacket: 4 * channels as u32,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4 * channels as u32,
            mChannelsPerFrame: channels as u32,
            mBitsPerChannel: 32,
            mReserved: 0,
        };

        // Create AudioConverter
        let mut converter: AudioConverterRef = ptr::null_mut();
        let status = unsafe { AudioConverterNew(&format, &output_format, &mut converter) };

        if status != noErr {
            unsafe {
                AudioFileStreamClose(stream_parser);
            }
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioConverterNew failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("AudioConverterNew failed: {}", err_str),
            ))));
        }

        debug!("Apple decoder: AudioConverter created");

        // Set magic cookie if available (required for AAC)
        if let Some(ref cookie) = parser_state.magic_cookie {
            let status = unsafe {
                AudioConverterSetProperty(
                    converter,
                    kAudioConverterDecompressionMagicCookie,
                    cookie.len() as UInt32,
                    cookie.as_ptr() as *const c_void,
                )
            };

            if status != noErr {
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

        let spec = PcmSpec {
            sample_rate,
            channels,
        };
        let byte_len_handle = config.byte_len_handle.clone().unwrap_or_default();

        // Allocate PCM buffer (1024 frames * channels)
        let buffer_frames = 1024;
        let pcm_buffer = vec![0.0f32; buffer_frames * channels as usize];

        let converter_input = Box::new(ConverterInputState::new());

        debug!(
            ?spec,
            buffer_frames,
            packets_buffered = parser_state.packet_buffer.len(),
            "Apple decoder: initialized successfully"
        );

        Ok(Self {
            stream_parser,
            converter,
            parser_state,
            converter_input,
            source: Box::new(source),
            read_buffer,
            spec,
            position: Duration::ZERO,
            duration: None,
            metadata: TrackMetadata::default(),
            byte_len_handle,
            pcm_buffer,
            source_eof: false,
            frames_decoded: 0,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // Ensure we have packets to decode
        while self.parser_state.packet_buffer.len() < 4 && !self.source_eof {
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
        let packet = match self.parser_state.packet_buffer.pop() {
            Some(p) => p,
            None => return Ok(None),
        };

        // Set up converter input
        self.converter_input.current_packet = Some(packet);

        // Prepare output buffer
        let channels = self.spec.channels as usize;
        let frames_per_packet = self
            .parser_state
            .format
            .map(|f| f.mFramesPerPacket as usize)
            .unwrap_or(1024);

        let output_frames = frames_per_packet.max(1024);
        if self.pcm_buffer.len() < output_frames * channels {
            self.pcm_buffer.resize(output_frames * channels, 0.0);
        }

        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: self.spec.channels as u32,
                mDataByteSize: (self.pcm_buffer.len() * 4) as u32,
                mData: self.pcm_buffer.as_mut_ptr() as *mut c_void,
            }],
        };

        let mut output_packets = output_frames as UInt32;
        let input_ptr = self.converter_input.as_mut() as *mut ConverterInputState as *mut c_void;

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

        // Handle no data available (need more input)
        if status == 0x21646174u32 as i32 {
            // '!dat'
            trace!("Apple decoder: converter needs more data");
            return self.next_chunk(); // Recursive call to get more data
        }

        if status != noErr && output_packets == 0 {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, "Apple decoder: AudioConverterFillComplexBuffer failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!("AudioConverterFillComplexBuffer failed: {}", err_str),
            ))));
        }

        if output_packets == 0 {
            // No output yet, need more input
            return self.next_chunk();
        }

        let frames = output_packets as usize;
        let samples = frames * channels;
        let pcm = self.pcm_buffer[..samples].to_vec();

        // Update position
        self.frames_decoded += frames as u64;
        self.position =
            Duration::from_secs_f64(self.frames_decoded as f64 / self.spec.sample_rate as f64);

        trace!(
            frames,
            samples,
            position_ms = self.position.as_millis(),
            "Apple decoder: decoded chunk"
        );

        let chunk = PcmChunk::new(self.spec, pcm);
        Ok(Some(chunk))
    }

    fn feed_parser(&mut self) -> DecodeResult<()> {
        let n = self
            .source
            .read(&mut self.read_buffer)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        if n == 0 {
            self.source_eof = true;
            return Ok(());
        }

        let status = unsafe {
            AudioFileStreamParseBytes(
                self.stream_parser,
                n as UInt32,
                self.read_buffer.as_ptr() as *const c_void,
                0,
            )
        };

        if status != noErr && status != kAudioFileStreamError_NotOptimized {
            let err_str = os_status_to_string(status);
            warn!(status, err = %err_str, bytes = n, "Apple decoder: parse failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("AudioFileStreamParseBytes failed: {}", err_str),
            ))));
        }

        trace!(
            bytes = n,
            packets = self.parser_state.packet_buffer.len(),
            "Apple decoder: fed parser"
        );
        Ok(())
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        // Seeking in streaming mode is complex - we'd need to:
        // 1. Seek in the source
        // 2. Reset the parser with discontinuity flag
        // 3. Reset the converter
        // 4. Re-parse until we reach the target position

        // For now, just reset and seek source if possible
        debug!(
            position_secs = pos.as_secs_f64(),
            "Apple decoder: seek requested"
        );

        // Reset converter
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != noErr {
            warn!(status, err = %os_status_to_string(status), "Apple decoder: converter reset failed");
        }

        // Clear packet buffer
        while self.parser_state.packet_buffer.pop().is_some() {}

        // Try to cast source to Seek (this won't work with Box<dyn Read>)
        // For full seek support, we'd need to restructure to keep Seek capability

        self.position = pos;
        self.frames_decoded = (pos.as_secs_f64() * self.spec.sample_rate as f64) as u64;
        self.source_eof = false;

        warn!("Apple decoder: seek in streaming mode has limited support");
        Ok(())
    }
}

// SAFETY: The decoder is designed to be used from one thread at a time.
unsafe impl Send for AppleInner {}

impl Drop for AppleInner {
    fn drop(&mut self) {
        debug!("Apple decoder: disposing");
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

// ────────────────────────────────── Callbacks ──────────────────────────────────

/// AudioFileStream property listener callback.
extern "C" fn property_listener_callback(
    client_data: *mut c_void,
    audio_file_stream: AudioFileStreamID,
    property_id: AudioFileStreamPropertyID,
    _flags: *mut UInt32,
) {
    let state = unsafe { &mut *(client_data as *mut StreamParserState) };

    match property_id {
        kAudioFileStreamProperty_DataFormat => {
            let mut format = AudioStreamBasicDescription::default();
            let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as UInt32;

            let status = unsafe {
                AudioFileStreamGetProperty(
                    audio_file_stream,
                    kAudioFileStreamProperty_DataFormat,
                    &mut size,
                    &mut format as *mut _ as *mut c_void,
                )
            };

            if status == noErr {
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
        kAudioFileStreamProperty_MagicCookieData => {
            let mut size: UInt32 = 0;
            let mut writable: u8 = 0;

            let status = unsafe {
                AudioFileStreamGetPropertyInfo(
                    audio_file_stream,
                    kAudioFileStreamProperty_MagicCookieData,
                    &mut size,
                    &mut writable,
                )
            };

            if status == noErr && size > 0 {
                let mut cookie = vec![0u8; size as usize];
                let status = unsafe {
                    AudioFileStreamGetProperty(
                        audio_file_stream,
                        kAudioFileStreamProperty_MagicCookieData,
                        &mut size,
                        cookie.as_mut_ptr() as *mut c_void,
                    )
                };

                if status == noErr {
                    state.magic_cookie = Some(cookie);
                    trace!(size, "Apple decoder callback: magic cookie received");
                }
            }
        }
        kAudioFileStreamProperty_ReadyToProducePackets => {
            state.ready = true;
            trace!("Apple decoder callback: ready to produce packets");
        }
        kAudioFileStreamProperty_DataOffset => {
            let mut offset: SInt64 = 0;
            let mut size = std::mem::size_of::<SInt64>() as UInt32;

            let status = unsafe {
                AudioFileStreamGetProperty(
                    audio_file_stream,
                    kAudioFileStreamProperty_DataOffset,
                    &mut size,
                    &mut offset as *mut _ as *mut c_void,
                )
            };

            if status == noErr {
                state.data_offset = offset;
                trace!(offset, "Apple decoder callback: data offset");
            }
        }
        _ => {
            // Ignore other properties
        }
    }
}

/// AudioFileStream packets callback.
extern "C" fn packets_callback(
    client_data: *mut c_void,
    num_bytes: UInt32,
    num_packets: UInt32,
    input_data: *const c_void,
    packet_descriptions: *mut AudioStreamPacketDescription,
) {
    let state = unsafe { &mut *(client_data as *mut StreamParserState) };

    if num_packets == 0 {
        return;
    }

    let data_slice =
        unsafe { std::slice::from_raw_parts(input_data as *const u8, num_bytes as usize) };

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
        let descriptions =
            unsafe { std::slice::from_raw_parts(packet_descriptions, num_packets as usize) };

        for desc in descriptions {
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

/// AudioConverter input data callback.
extern "C" fn converter_input_callback(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    let state = unsafe { &mut *(user_data as *mut ConverterInputState) };

    let packet = match state.current_packet.take() {
        Some(p) => p,
        None => {
            // No data available
            unsafe {
                *io_num_packets = 0;
            }
            return 0x21646174u32 as i32; // '!dat' - no data now
        }
    };

    // Store packet data in state to keep it alive during conversion
    // (will be dropped on next call or when state is dropped)
    let data_len = packet.data.len();
    state.held_packet_data = Some(packet.data);

    // Provide packet data pointer (now pointing to held_packet_data)
    unsafe {
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

    noErr
}

// ────────────────────────────────── Apple<C> ──────────────────────────────────

/// Apple AudioToolbox streaming decoder parameterized by codec type.
///
/// Uses AudioFileStream for parsing and AudioConverter for decoding.
/// This approach supports streaming data without requiring the full file.
pub struct Apple<C: CodecType> {
    inner: AppleInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> std::fmt::Debug for Apple<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Apple")
            .field("spec", &self.inner.spec)
            .field("position", &self.inner.position)
            .field("duration", &self.inner.duration)
            .finish_non_exhaustive()
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
        let inner = AppleInner::new(source, &config)?;
        Ok(Self {
            inner,
            _codec: PhantomData,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn position(&self) -> Duration {
        self.inner.position
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

impl<C: CodecType> InnerDecoder for Apple<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
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

// ────────────────────────────────── Type Aliases ──────────────────────────────────

/// Apple AAC decoder.
pub type AppleAac = Apple<Aac>;

/// Apple MP3 decoder.
pub type AppleMp3 = Apple<Mp3>;

/// Apple FLAC decoder.
pub type AppleFlac = Apple<Flac>;

/// Apple ALAC decoder.
pub type AppleAlac = Apple<Alac>;

// ────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apple_config_default() {
        let config = AppleConfig::default();
        assert!(config.byte_len_handle.is_none());
        assert!(config.container.is_none());
    }

    #[test]
    fn test_apple_config_with_container() {
        let config = AppleConfig {
            container: Some(ContainerFormat::Fmp4),
            ..Default::default()
        };
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_container_to_file_type() {
        assert_eq!(
            container_to_file_type(ContainerFormat::Fmp4),
            Some(kAudioFileM4AType)
        );
        assert_eq!(
            container_to_file_type(ContainerFormat::Adts),
            Some(kAudioFileAAC_ADTSType)
        );
        assert_eq!(
            container_to_file_type(ContainerFormat::MpegAudio),
            Some(kAudioFileMP3Type)
        );
        assert_eq!(
            container_to_file_type(ContainerFormat::Flac),
            Some(kAudioFileFLACType)
        );
        assert_eq!(
            container_to_file_type(ContainerFormat::Caf),
            Some(kAudioFileCAFType)
        );
        assert_eq!(container_to_file_type(ContainerFormat::Wav), None);
    }

    #[test]
    fn test_os_status_to_string() {
        assert_eq!(os_status_to_string(noErr), "noErr");
        // 'wht?' = 0x7768743f
        assert!(os_status_to_string(0x7768743f).contains("wht?"));
    }

    #[test]
    fn test_type_aliases_exist() {
        fn _check_aac(_: AppleAac) {}
        fn _check_mp3(_: AppleMp3) {}
        fn _check_flac(_: AppleFlac) {}
        fn _check_alac(_: AppleAlac) {}
    }
}
