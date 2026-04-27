//! `AudioConverter` input-data callback and associated state.
//!
//! The callback hands `AudioConverter` a pointer+len into the active
//! packet buffer without any per-packet allocation. `AppleInner` writes
//! the current packet into `ConverterInputState` before each
//! `AudioConverterFillComplexBuffer` call and clears `has_packet` once
//! the converter consumes it.

#![allow(unsafe_code)]

use std::{ffi::c_void, ptr};

use super::{
    consts::Consts,
    ffi::{AudioBufferList, AudioConverterRef, AudioStreamPacketDescription, OSStatus, UInt32},
};

/// Zero-alloc input state fed into `AudioConverterFillComplexBuffer`.
///
/// All fields point at memory owned by the `PacketReader`'s internal
/// buffer — they stay valid until the next `read_next_packet` call, which
/// is always issued by `AppleInner` *after* the converter has finished
/// consuming the current packet.
pub(super) struct ConverterInputState {
    pub(super) packet_ptr: *const u8,
    pub(super) packet_len: UInt32,
    pub(super) packet_desc: AudioStreamPacketDescription,
    pub(super) has_packet: bool,
}

impl ConverterInputState {
    pub(super) fn new() -> Self {
        Self {
            packet_ptr: ptr::null(),
            packet_len: 0,
            packet_desc: AudioStreamPacketDescription::default(),
            has_packet: false,
        }
    }

    pub(super) fn set(&mut self, data: &[u8], description: AudioStreamPacketDescription) {
        #[expect(clippy::cast_possible_truncation, reason = "packet size fits in u32")]
        let len = data.len() as UInt32;
        self.packet_ptr = data.as_ptr();
        self.packet_len = len;
        // `mStartOffset` here is the offset of the packet WITHIN the
        // `mData` buffer fed to `AudioConverter`. Since `mData` already
        // points at the packet body, this must stay zero; non-zero
        // values cause the converter to read past the buffer.
        self.packet_desc = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: description.mVariableFramesInPacket,
            mDataByteSize: len,
        };
        self.has_packet = true;
    }

    pub(super) fn clear(&mut self) {
        self.packet_ptr = ptr::null();
        self.packet_len = 0;
        self.has_packet = false;
    }
}

/// `AudioConverter` input data callback.
pub(super) extern "C" fn converter_input_callback(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    // SAFETY: `user_data` was set to a valid `ConverterInputState` pointer
    // by `AppleInner::run_converter`; the state outlives the callback.
    let state = unsafe { &mut *(user_data as *mut ConverterInputState) };

    if !state.has_packet {
        // SAFETY: `io_num_packets` is a valid out-param provided by AudioConverter.
        unsafe {
            *io_num_packets = 0;
        }
        return Consts::kAudioConverterErr_NoDataNow;
    }

    // SAFETY: `io_data`, `io_num_packets` are valid pointers supplied by
    // AudioConverter. `packet_ptr` is either owned by the
    // `PacketReader`'s internal buffer (AudioFile) or by an `Fmp4Reader`
    // buffer; both stay alive for the duration of this call.
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
