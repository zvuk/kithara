use std::ffi::c_void;

pub(super) type OsStatus = i32;
pub(super) type AudioFormatId = u32;
pub(super) type AudioFormatFlags = u32;
pub(super) type AudioConverterRef = *mut c_void;
type Float64 = f64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioStreamPacketDescription {
    pub(super) start_offset: i64,
    pub(super) variable_frames_in_packet: u32,
    pub(super) data_byte_size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioStreamBasicDescription {
    pub(super) sample_rate: Float64,
    pub(super) format_id: AudioFormatId,
    pub(super) format_flags: AudioFormatFlags,
    pub(super) bytes_per_packet: u32,
    pub(super) frames_per_packet: u32,
    pub(super) bytes_per_frame: u32,
    pub(super) channels_per_frame: u32,
    pub(super) bits_per_channel: u32,
    pub(super) reserved: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct AudioBuffer {
    pub(super) number_channels: u32,
    pub(super) data_byte_size: u32,
    pub(super) data: *mut c_void,
}

#[repr(C)]
pub(super) struct AudioBufferList {
    pub(super) number_buffers: u32,
    pub(super) buffers: [AudioBuffer; 1],
}

pub(super) type AudioConverterComplexInputDataProc = extern "C" fn(
    in_audio_converter: AudioConverterRef,
    io_number_data_packets: *mut u32,
    io_data: *mut AudioBufferList,
    out_data_packet_description: *mut *mut AudioStreamPacketDescription,
    in_user_data: *mut c_void,
) -> OsStatus;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    #[link_name = "AudioConverterNew"]
    pub(super) fn audio_converter_new(
        source_format: *const AudioStreamBasicDescription,
        destination_format: *const AudioStreamBasicDescription,
        converter: *mut AudioConverterRef,
    ) -> OsStatus;

    #[link_name = "AudioConverterFillComplexBuffer"]
    pub(super) fn audio_converter_fill_complex_buffer(
        converter: AudioConverterRef,
        input_data_proc: AudioConverterComplexInputDataProc,
        input_data_proc_user_data: *mut c_void,
        output_data_packet_size: *mut u32,
        output_data: *mut AudioBufferList,
        packet_description: *mut AudioStreamPacketDescription,
    ) -> OsStatus;

    #[link_name = "AudioConverterDispose"]
    pub(super) fn audio_converter_dispose(converter: AudioConverterRef) -> OsStatus;

    #[link_name = "AudioConverterReset"]
    pub(super) fn audio_converter_reset(converter: AudioConverterRef) -> OsStatus;
}
