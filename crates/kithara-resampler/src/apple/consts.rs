use super::ffi::{AudioFormatFlags, AudioFormatId, OsStatus};

pub(super) struct Consts;

impl Consts {
    pub(super) const BITS_PER_F32_SAMPLE: u32 = 32;
    pub(super) const BYTES_PER_F32_SAMPLE: u32 = 4;
    pub(super) const AUDIO_CONVERTER_ERR_NO_DATA_NOW: OsStatus = 0x2164_6174;
    pub(super) const AUDIO_FORMAT_FLAG_IS_FLOAT: AudioFormatFlags = 1 << 0;
    pub(super) const AUDIO_FORMAT_FLAG_IS_PACKED: AudioFormatFlags = 1 << 3;
    pub(super) const AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED: AudioFormatFlags =
        Self::AUDIO_FORMAT_FLAG_IS_FLOAT | Self::AUDIO_FORMAT_FLAG_IS_PACKED;
    pub(super) const AUDIO_FORMAT_LINEAR_PCM: AudioFormatId = 0x6c70_636d;
    pub(super) const NO_ERR: OsStatus = 0;
}

pub(super) fn os_status_to_string(status: OsStatus) -> String {
    if status == Consts::NO_ERR {
        return "noErr".to_owned();
    }

    let bytes = status.to_be_bytes();
    if bytes.iter().all(u8::is_ascii_graphic) {
        let fourcc = String::from_utf8_lossy(&bytes);
        format!("{status} ('{fourcc}')")
    } else {
        status.to_string()
    }
}
