//! `AudioConverter` input-data callback and associated state.

#![allow(unsafe_code)]

use std::ffi::c_void;

use super::{
    consts::Consts,
    ffi::{AudioBufferList, AudioConverterRef, AudioStreamPacketDescription, OSStatus, UInt32},
    parser::AudioPacket,
};

/// State for `AudioConverter` input callback.
pub(super) struct ConverterInputState {
    /// Current packet being provided to converter.
    pub(super) current_packet: Option<AudioPacket>,
    /// Packet description for current packet.
    pub(super) packet_desc: AudioStreamPacketDescription,
    /// Holds packet data while converter uses it (prevents memory leak).
    pub(super) held_packet_data: Option<Vec<u8>>,
}

impl ConverterInputState {
    pub(super) fn new() -> Self {
        Self {
            current_packet: None,
            packet_desc: AudioStreamPacketDescription::default(),
            held_packet_data: None,
        }
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
    let data_len = packet.data.len();
    state.held_packet_data = Some(packet.data);

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
