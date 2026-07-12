use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_ERR_NO_DATA_NOW, AUDIO_FILE_AAC_ADTS_TYPE, AUDIO_FILE_CAF_TYPE,
    AUDIO_FILE_FLAC_TYPE, AUDIO_FILE_M4A_TYPE, AUDIO_FILE_MP3_TYPE, AUDIO_FILE_WAVE_TYPE,
    AUDIO_FORMAT_APPLE_LOSSLESS, AUDIO_FORMAT_FLAC, AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED,
    AUDIO_FORMAT_LINEAR_PCM, AUDIO_FORMAT_MPEG_LAYER3, AUDIO_FORMAT_MPEG4_AAC,
    AUDIO_FORMAT_PROPERTY_FORMAT_LIST, AudioFormatFlags, AudioFormatID, OSStatus,
};

pub(crate) struct Consts;

impl Consts {
    /// AAC always decodes 1024 samples per packet (SBR spectral bands
    /// use the same packet boundary).
    pub(crate) const AAC_FRAMES_PER_PACKET: u32 = 1024;
    pub(crate) const BITS_PER_F32_SAMPLE: u32 = 32;
    pub(crate) const BYTES_PER_F32_SAMPLE: u32 = 4;
    /// Bytes prepended to STREAMINFO in the Apple FLAC magic cookie:
    /// 4 bytes `"fLaC"` marker + 4 bytes `METADATA_BLOCK_HEADER`.
    pub(crate) const FLAC_COOKIE_PREFIX_LEN: usize = 8;
    /// Size of the FLAC STREAMINFO metadata block body (fixed by spec).
    pub(crate) const FLAC_STREAMINFO_LEN: usize = 34;
    /// Same as [`Self::FLAC_STREAMINFO_LEN`] typed for the
    /// `METADATA_BLOCK_LENGTH` u8 byte in the Apple FLAC magic cookie.
    pub(crate) const FLAC_STREAMINFO_LEN_U8: u8 = 34;
    pub(crate) const CONVERTER_ERR_NO_DATA_NOW: OSStatus = AUDIO_CONVERTER_ERR_NO_DATA_NOW;
    /// `'adts'` — raw AAC ADTS-framed bitstream file-type hint.
    pub(crate) const FILE_AAC_ADTS_TYPE: u32 = AUDIO_FILE_AAC_ADTS_TYPE;
    /// `'caff'` — Core Audio Format file-type hint.
    pub(crate) const FILE_CAF_TYPE: u32 = AUDIO_FILE_CAF_TYPE;
    /// `'flac'` — native FLAC bitstream file-type hint (macOS 10.13+ /
    /// iOS 11+). Same four-cc as [`Self::FORMAT_FLAC`].
    pub(crate) const FILE_FLAC_TYPE: u32 = AUDIO_FILE_FLAC_TYPE;
    /// `'m4af'` — M4A (ALAC / AAC) file-type hint.
    pub(crate) const FILE_M4A_TYPE: u32 = AUDIO_FILE_M4A_TYPE;
    /// `'MPG3'` — MPEG-1/2 Layer 3 file-type hint.
    pub(crate) const FILE_MP3_TYPE: u32 = AUDIO_FILE_MP3_TYPE;
    /// `'WAVE'` — RIFF WAV file-type hint for `audio_file_open_with_callbacks`.
    pub(crate) const FILE_WAVE_TYPE: u32 = AUDIO_FILE_WAVE_TYPE;
    /// `'alac'` — Apple Lossless Audio Codec input format ID.
    pub(crate) const FORMAT_APPLE_LOSSLESS: AudioFormatID = AUDIO_FORMAT_APPLE_LOSSLESS;
    pub(crate) const FORMAT_FLAC: AudioFormatID = AUDIO_FORMAT_FLAC;
    pub(crate) const FORMAT_FLAGS_NATIVE_FLOAT_PACKED: AudioFormatFlags =
        AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED;
    pub(crate) const FORMAT_LINEAR_PCM: AudioFormatID = AUDIO_FORMAT_LINEAR_PCM;
    /// `'aac '` — MPEG-4 AAC LC input format ID. Also used as a hint for
    /// HE-AAC v1/v2: the actual codec class is derived from the ESDS
    /// cookie via `kAudioFormatProperty_FormatList`.
    pub(crate) const FORMAT_MPEG4_AAC: AudioFormatID = AUDIO_FORMAT_MPEG4_AAC;
    /// `'.mp3'` — MPEG-1/2 Layer 3 input format ID.
    pub(crate) const FORMAT_MPEG_LAYER3: AudioFormatID = AUDIO_FORMAT_MPEG_LAYER3;
    /// `'flst'` — `audio_format_get_property_raw` property that enumerates all
    /// `AudioFormatListItem`s an ESDS-wrapped cookie can decode to. For
    /// HE-AAC v1/v2 the ESDS encodes multiple compatible layers (LC
    /// base, HE-AAC v1 SBR, HE-AAC v2 PS). Apple returns them sorted
    /// from MOST rich to LEAST rich (channel count first, then sample
    /// rate), so `items[0]` is the full output format (e.g. stereo
    /// 44.1 kHz for HE-AAC v2). Specifier = `AudioFormatInfo` struct
    /// containing a partial ASBD (`format_id` required) + ESDS cookie.
    pub(crate) const FORMAT_PROPERTY_FORMAT_LIST: u32 = AUDIO_FORMAT_PROPERTY_FORMAT_LIST;
    pub(crate) const NO_ERR: OSStatus = kithara_apple::audio_toolbox::NO_ERR;
}

/// Decode a `FourCC`-style `OSStatus` into an ASCII tag when possible.
pub(crate) fn os_status_to_string(status: OSStatus) -> String {
    kithara_apple::audio_toolbox::os_status_to_string(status)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_os_status_to_string() {
        assert_eq!(os_status_to_string(Consts::NO_ERR), "noErr");
        assert!(os_status_to_string(0x7768_743f).contains("wht?"));
    }
}
