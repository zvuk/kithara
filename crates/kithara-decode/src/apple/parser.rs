//! `AudioFileStream` parser state, packet buffer and C callbacks.

#![allow(unsafe_code)]

use std::{collections::VecDeque, ffi::c_void, mem::size_of, slice};

use kithara_stream::ContainerFormat;
use tracing::trace;

use super::{
    consts::{Consts, os_status_to_string},
    ffi::{
        AudioFileStreamGetProperty, AudioFileStreamGetPropertyInfo, AudioFileStreamID,
        AudioFileStreamPropertyID, AudioFileTypeID, AudioStreamBasicDescription,
        AudioStreamPacketDescription, SInt64, UInt32,
    },
};

/// A compressed audio packet with its description.
#[derive(Clone)]
pub(super) struct AudioPacket {
    pub(super) data: Vec<u8>,
    pub(super) description: AudioStreamPacketDescription,
}

/// Thread-safe packet buffer for storing compressed audio packets.
pub(super) struct PacketBuffer {
    packets: VecDeque<AudioPacket>,
}

impl PacketBuffer {
    pub(super) fn new() -> Self {
        Self {
            packets: VecDeque::new(),
        }
    }

    pub(super) fn push(&mut self, packet: AudioPacket) {
        self.packets.push_back(packet);
    }

    pub(super) fn pop(&mut self) -> Option<AudioPacket> {
        self.packets.pop_front()
    }

    pub(super) fn len(&self) -> usize {
        self.packets.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

/// State shared between `AudioFileStream` callbacks and the decoder.
pub(super) struct StreamParserState {
    /// Audio format from the stream.
    pub(super) format: Option<AudioStreamBasicDescription>,
    /// Magic cookie data for codec initialization.
    pub(super) magic_cookie: Option<Vec<u8>>,
    /// Buffered compressed packets from callbacks.
    pub(super) packet_buffer: PacketBuffer,
    /// Data offset in stream (where audio data starts).
    pub(super) data_offset: SInt64,
    /// Total audio data packet count (from Xing/VBRI header via `AudioFileStream`).
    pub(super) audio_data_packet_count: Option<u64>,
    /// True when stream is ready to produce packets.
    pub(super) ready: bool,
    /// Error from callbacks.
    pub(super) error: Option<String>,
}

impl StreamParserState {
    pub(super) fn new() -> Self {
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

pub(super) fn container_to_file_type(container: ContainerFormat) -> Option<AudioFileTypeID> {
    match container {
        ContainerFormat::Fmp4 => Some(Consts::kAudioFileM4AType),
        ContainerFormat::Adts => Some(Consts::kAudioFileAAC_ADTSType),
        ContainerFormat::MpegAudio => Some(Consts::kAudioFileMP3Type),
        ContainerFormat::Flac => Some(Consts::kAudioFileFLACType),
        ContainerFormat::Caf => Some(Consts::kAudioFileCAFType),
        _ => None,
    }
}

/// `AudioFileStream` property listener callback.
#[expect(
    clippy::cognitive_complexity,
    reason = "extern C callback; match arms are independent"
)]
pub(super) extern "C" fn property_listener_callback(
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
pub(super) extern "C" fn packets_callback(
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

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
}
