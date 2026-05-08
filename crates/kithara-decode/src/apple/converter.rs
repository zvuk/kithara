//! `AudioConverter` input-data callback and associated state.
//!
//! The callback hands `AudioConverter` a pointer+len into the active
//! packet buffer without any per-packet allocation. `AppleDecoder` writes
//! the current packet into `ConverterInputState` before each
//! `AudioConverterFillComplexBuffer` call and clears `has_packet` once
//! the converter consumes it.

#![allow(unsafe_code)]

use std::{ffi::c_void, mem::size_of, ptr};

use tracing::debug;

use super::{
    consts::Consts,
    ffi::{
        AudioBufferList, AudioConverterGetProperty, AudioConverterPrimeInfo, AudioConverterRef,
        AudioStreamPacketDescription, OSStatus, UInt32,
    },
};
use crate::GaplessInfo;

/// Query `kAudioConverterPrimeInfo` from a live converter.
///
/// Returns `None` when the converter is null or when the property is
/// not yet populated (AAC reports priming only after at least one
/// `AudioConverterFillComplexBuffer` call has consumed an input packet —
/// see `refresh_after_first_chunk` callsites).
pub(crate) fn prime_info_from_converter(
    converter: AudioConverterRef,
) -> Option<AudioConverterPrimeInfo> {
    if converter.is_null() {
        return None;
    }
    let mut info = AudioConverterPrimeInfo::default();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "size_of result fits in u32"
    )]
    let mut size = size_of::<AudioConverterPrimeInfo>() as UInt32;
    // SAFETY: `converter` is a valid handle (caller-checked); `info` is a
    let status = unsafe {
        AudioConverterGetProperty(
            converter,
            Consts::kAudioConverterPrimeInfo,
            &mut size,
            &mut info as *mut _ as *mut c_void,
        )
    };
    if status == Consts::noErr {
        Some(info)
    } else {
        None
    }
}

/// Map raw `AudioConverterPrimeInfo` into our `GaplessInfo` contract.
///
/// Returns `None` when the codec reports no encoder priming and no
/// trailing padding — there is nothing for the [`crate::GaplessTrimmer`] to do
/// in that case.
pub(crate) fn gapless_info_from_prime_info(info: AudioConverterPrimeInfo) -> Option<GaplessInfo> {
    if info.leading_frames == 0 && info.trailing_frames == 0 {
        return None;
    }
    Some(GaplessInfo {
        leading_frames: u64::from(info.leading_frames),
        trailing_frames: u64::from(info.trailing_frames),
    })
}

/// Emit a `kithara::gapless` debug record describing one `PrimeInfo`
/// observation. `stage` distinguishes the init query from the post-first-
/// chunk refresh.
pub(crate) fn log_gapless_prime_info(
    stage: &'static str,
    prime_info: Option<AudioConverterPrimeInfo>,
    gapless: Option<GaplessInfo>,
) {
    match (prime_info, gapless) {
        (_, Some(info)) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            leading_frames = info.leading_frames,
            trailing_frames = info.trailing_frames,
            "captured gapless metadata from Apple PrimeInfo"
        ),
        (Some(info), None) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            leading_frames = info.leading_frames,
            trailing_frames = info.trailing_frames,
            "Apple PrimeInfo reported no gapless trim"
        ),
        (None, None) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            "Apple PrimeInfo not available"
        ),
    }
}

/// Zero-alloc input state fed into `AudioConverterFillComplexBuffer`.
///
/// All fields point at memory owned by the `PacketReader`'s internal
/// buffer — they stay valid until the next `read_next_packet` call, which
/// is always issued by `AppleDecoder` *after* the converter has finished
/// consuming the current packet.
pub(crate) struct ConverterInputState {
    pub(crate) packet_ptr: *const u8,
    pub(crate) packet_desc: AudioStreamPacketDescription,
    pub(crate) packet_len: UInt32,
    pub(crate) has_packet: bool,
}

impl ConverterInputState {
    pub(crate) fn new() -> Self {
        Self {
            packet_ptr: ptr::null(),
            packet_desc: AudioStreamPacketDescription::default(),
            packet_len: 0,
            has_packet: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.packet_ptr = ptr::null();
        self.packet_len = 0;
        self.has_packet = false;
    }

    pub(crate) fn set(&mut self, data: &[u8], description: AudioStreamPacketDescription) {
        #[expect(clippy::cast_possible_truncation, reason = "packet size fits in u32")]
        let len = data.len() as UInt32;
        self.packet_ptr = data.as_ptr();
        self.packet_len = len;
        self.packet_desc = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: description.mVariableFramesInPacket,
            mDataByteSize: len,
        };
        self.has_packet = true;
    }
}

/// `AudioConverter` input data callback.
pub(crate) extern "C" fn converter_input_callback(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    // SAFETY: `user_data` was set to a valid `ConverterInputState` pointer
    let state = unsafe { &mut *(user_data as *mut ConverterInputState) };

    if !state.has_packet {
        // SAFETY: `io_num_packets` is a valid out-param provided by AudioConverter.
        unsafe {
            *io_num_packets = 0;
        }
        return Consts::kAudioConverterErr_NoDataNow;
    }

    // SAFETY: `io_data`, `io_num_packets` are valid pointers supplied by
    unsafe {
        (*io_data).mBuffers[0].mDataByteSize = state.packet_len;
        (*io_data).mBuffers[0].mData = state.packet_ptr as *mut c_void;
        *io_num_packets = 1;

        if !out_packet_desc.is_null() {
            *out_packet_desc = &mut state.packet_desc;
        }
    }

    state.has_packet = false;
    Consts::noErr
}
