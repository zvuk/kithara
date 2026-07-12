mod buffer;
mod converter;
mod error;
mod file;
mod format;
mod pod;
pub mod sys;

pub use buffer::{
    AudioBufferListTarget, AudioBufferListWriter, OwnedAudioBufferList, SingleAudioBufferList,
    f32_byte_len,
};
pub use converter::{
    AudioConverter, AudioConverterInput, AudioConverterInputRequest, AudioConverterPacketInput,
    PacketDescriptionsOut,
};
pub use error::AudioToolboxError;
pub use file::{AudioFile, AudioFileCallbacks, AudioFilePacketRead};
pub use format::{audio_format_get_property, audio_format_get_property_info};
pub use pod::{ApplePod, pod_from_prefix, pod_to_vec, pod_write_to_slice};
pub use sys::{
    AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, AUDIO_CONVERTER_ERR_NO_DATA_NOW,
    AUDIO_CONVERTER_PRIME_INFO, AUDIO_FILE_AAC_ADTS_TYPE, AUDIO_FILE_CAF_TYPE,
    AUDIO_FILE_FLAC_TYPE, AUDIO_FILE_M4A_TYPE, AUDIO_FILE_MP3_TYPE,
    AUDIO_FILE_PROPERTY_AUDIO_DATA_PACKET_COUNT, AUDIO_FILE_PROPERTY_DATA_FORMAT,
    AUDIO_FILE_PROPERTY_MAGIC_COOKIE_DATA, AUDIO_FILE_PROPERTY_MAXIMUM_PACKET_SIZE,
    AUDIO_FILE_WAVE_TYPE, AUDIO_FORMAT_APPLE_LOSSLESS, AUDIO_FORMAT_FLAC,
    AUDIO_FORMAT_FLAG_IS_FLOAT, AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED, AUDIO_FORMAT_FLAG_IS_PACKED,
    AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED, AUDIO_FORMAT_LINEAR_PCM, AUDIO_FORMAT_MPEG_LAYER3,
    AUDIO_FORMAT_MPEG4_AAC, AUDIO_FORMAT_PROPERTY_FORMAT_LIST, AudioBuffer, AudioBufferList,
    AudioChannelLayoutTag, AudioConverterPrimeInfo, AudioConverterRef, AudioFileTypeID,
    AudioFormatFlags, AudioFormatID, AudioFormatInfo, AudioFormatListItem,
    AudioStreamBasicDescription, AudioStreamPacketDescription, BITS_PER_F32_SAMPLE,
    BYTES_PER_F32_SAMPLE, FLOAT32_PLANAR_FLAGS, Float64, NO_ERR, OSStatus, PARAM_ERR, SInt64,
    UInt32, os_status_to_string,
};
