use std::ffi::c_void;

pub type OSStatus = i32;
pub type AudioFormatID = u32;
pub type AudioFormatFlags = u32;
pub type AudioConverterRef = *mut c_void;
pub type AudioFileID = *mut c_void;
pub type AudioFilePropertyID = u32;
pub type AudioFileTypeID = u32;
pub type AudioFormatPropertyID = u32;
pub type AudioChannelLayoutTag = u32;
pub type UInt32 = u32;
pub type SInt64 = i64;
pub type Float64 = f64;

pub const NO_ERR: OSStatus = 0;
pub const PARAM_ERR: OSStatus = -50;

pub const BITS_PER_F32_SAMPLE: UInt32 = 32;
pub const BYTES_PER_F32_SAMPLE: UInt32 = 4;
pub const AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE: UInt32 = 0x646d_6763;
pub const AUDIO_CONVERTER_ERR_NO_DATA_NOW: OSStatus = 0x2164_6174;
pub const AUDIO_CONVERTER_PRIME_INFO: UInt32 = 0x7072_696d;
pub const AUDIO_FILE_AAC_ADTS_TYPE: AudioFileTypeID = 0x6164_7473;
pub const AUDIO_FILE_CAF_TYPE: AudioFileTypeID = 0x6361_6666;
pub const AUDIO_FILE_FLAC_TYPE: AudioFileTypeID = 0x666c_6163;
pub const AUDIO_FILE_M4A_TYPE: AudioFileTypeID = 0x6d34_6166;
pub const AUDIO_FILE_MP3_TYPE: AudioFileTypeID = 0x4d50_4733;
pub const AUDIO_FILE_WAVE_TYPE: AudioFileTypeID = 0x5741_5645;
pub const AUDIO_FILE_PROPERTY_AUDIO_DATA_PACKET_COUNT: AudioFilePropertyID = 0x7063_6e74;
pub const AUDIO_FILE_PROPERTY_DATA_FORMAT: AudioFilePropertyID = 0x6466_6d74;
pub const AUDIO_FILE_PROPERTY_MAGIC_COOKIE_DATA: AudioFilePropertyID = 0x6d67_6963;
pub const AUDIO_FILE_PROPERTY_MAXIMUM_PACKET_SIZE: AudioFilePropertyID = 0x7073_7a65;
pub const AUDIO_FORMAT_APPLE_LOSSLESS: AudioFormatID = 0x616c_6163;
pub const AUDIO_FORMAT_FLAC: AudioFormatID = 0x666c_6163;
pub const AUDIO_FORMAT_FLAG_IS_FLOAT: AudioFormatFlags = 1 << 0;
pub const AUDIO_FORMAT_FLAG_IS_PACKED: AudioFormatFlags = 1 << 3;
pub const AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED: AudioFormatFlags = 1 << 5;
pub const AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED: AudioFormatFlags =
    AUDIO_FORMAT_FLAG_IS_FLOAT | AUDIO_FORMAT_FLAG_IS_PACKED;
pub const FLOAT32_PLANAR_FLAGS: AudioFormatFlags =
    AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED | AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED;
pub const AUDIO_FORMAT_LINEAR_PCM: AudioFormatID = 0x6c70_636d;
pub const AUDIO_FORMAT_MPEG4_AAC: AudioFormatID = 0x6161_6320;
pub const AUDIO_FORMAT_MPEG_LAYER3: AudioFormatID = 0x2e6d_7033;
pub const AUDIO_FORMAT_PROPERTY_FORMAT_LIST: AudioFormatPropertyID = 0x666c_7374;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioStreamPacketDescription {
    pub start_offset: SInt64,
    pub variable_frames_in_packet: UInt32,
    pub data_byte_size: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioStreamBasicDescription {
    pub sample_rate: Float64,
    pub format_id: AudioFormatID,
    pub format_flags: AudioFormatFlags,
    pub bytes_per_packet: UInt32,
    pub frames_per_packet: UInt32,
    pub bytes_per_frame: UInt32,
    pub channels_per_frame: UInt32,
    pub bits_per_channel: UInt32,
    pub reserved: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AudioBuffer {
    pub number_channels: UInt32,
    pub data_byte_size: UInt32,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferList {
    pub number_buffers: UInt32,
    pub buffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioConverterPrimeInfo {
    pub leading_frames: UInt32,
    pub trailing_frames: UInt32,
}

pub type AudioConverterComplexInputDataProc = extern "C" fn(
    converter: AudioConverterRef,
    packets: *mut UInt32,
    data: *mut AudioBufferList,
    packet_description: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioFormatInfo {
    pub asbd: AudioStreamBasicDescription,
    pub magic_cookie: *const c_void,
    pub magic_cookie_size: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioFormatListItem {
    pub asbd: AudioStreamBasicDescription,
    pub channel_layout_tag: AudioChannelLayoutTag,
}

pub type AudioFileReadProc = extern "C" fn(
    client_data: *mut c_void,
    position: SInt64,
    request_count: UInt32,
    buffer: *mut c_void,
    actual_count: *mut UInt32,
) -> OSStatus;

pub type AudioFileGetSizeProc = extern "C" fn(client_data: *mut c_void) -> SInt64;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    #[link_name = "AudioFormatGetPropertyInfo"]
    pub(crate) fn audio_format_get_property_info_raw(
        property: AudioFormatPropertyID,
        specifier_size: UInt32,
        specifier: *const c_void,
        property_data_size: *mut UInt32,
    ) -> OSStatus;

    #[link_name = "AudioFormatGetProperty"]
    pub(crate) fn audio_format_get_property_raw(
        property: AudioFormatPropertyID,
        specifier_size: UInt32,
        specifier: *const c_void,
        property_data_size: *mut UInt32,
        property_data: *mut c_void,
    ) -> OSStatus;

    #[link_name = "AudioConverterNew"]
    pub(crate) fn audio_converter_new(
        source_format: *const AudioStreamBasicDescription,
        destination_format: *const AudioStreamBasicDescription,
        converter: *mut AudioConverterRef,
    ) -> OSStatus;

    #[link_name = "AudioConverterSetProperty"]
    pub(crate) fn audio_converter_set_property(
        converter: AudioConverterRef,
        property: UInt32,
        property_data_size: UInt32,
        property_data: *const c_void,
    ) -> OSStatus;

    #[link_name = "AudioConverterGetProperty"]
    pub(crate) fn audio_converter_get_property(
        converter: AudioConverterRef,
        property: UInt32,
        property_data_size: *mut UInt32,
        property_data: *mut c_void,
    ) -> OSStatus;

    #[link_name = "AudioConverterFillComplexBuffer"]
    pub(crate) fn audio_converter_fill_complex_buffer(
        converter: AudioConverterRef,
        input_data_proc: AudioConverterComplexInputDataProc,
        input_data_proc_user_data: *mut c_void,
        output_data_packet_size: *mut UInt32,
        output_data: *mut AudioBufferList,
        packet_description: *mut AudioStreamPacketDescription,
    ) -> OSStatus;

    #[link_name = "AudioConverterDispose"]
    pub(crate) fn audio_converter_dispose(converter: AudioConverterRef) -> OSStatus;

    #[link_name = "AudioConverterReset"]
    pub(crate) fn audio_converter_reset(converter: AudioConverterRef) -> OSStatus;

    #[link_name = "AudioFileOpenWithCallbacks"]
    pub(crate) fn audio_file_open_with_callbacks(
        client_data: *mut c_void,
        read_func: AudioFileReadProc,
        write_func: *const c_void,
        get_size_func: Option<AudioFileGetSizeProc>,
        set_size_func: *const c_void,
        file_type_hint: AudioFileTypeID,
        audio_file: *mut AudioFileID,
    ) -> OSStatus;

    #[link_name = "AudioFileClose"]
    pub(crate) fn audio_file_close(audio_file: AudioFileID) -> OSStatus;

    #[link_name = "AudioFileGetProperty"]
    pub(crate) fn audio_file_get_property_raw(
        audio_file: AudioFileID,
        property: AudioFilePropertyID,
        data_size: *mut UInt32,
        property_data: *mut c_void,
    ) -> OSStatus;

    #[link_name = "AudioFileGetPropertyInfo"]
    pub(crate) fn audio_file_get_property_info_raw(
        audio_file: AudioFileID,
        property: AudioFilePropertyID,
        data_size: *mut UInt32,
        is_writable: *mut UInt32,
    ) -> OSStatus;

    #[link_name = "AudioFileReadPacketData"]
    pub(crate) fn audio_file_read_packet_data(
        audio_file: AudioFileID,
        use_cache: u8,
        num_bytes: *mut UInt32,
        packet_descriptions: *mut AudioStreamPacketDescription,
        starting_packet: SInt64,
        num_packets: *mut UInt32,
        buffer: *mut c_void,
    ) -> OSStatus;
}

#[must_use]
pub fn os_status_to_string(status: OSStatus) -> String {
    if status == NO_ERR {
        return "noErr".to_owned();
    }

    let bytes = status.to_be_bytes();
    if bytes.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        let tag = String::from_utf8_lossy(&bytes);
        format!("'{tag}' ({status})")
    } else {
        status.to_string()
    }
}
