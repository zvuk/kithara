#![allow(non_upper_case_globals)]

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
    pub(crate) const kAudioConverterErr_NoDataNow: OSStatus = AUDIO_CONVERTER_ERR_NO_DATA_NOW;
    /// `'pmth'` — `AudioConverterPrimeMethod`: controls how the
    /// converter handles codec-side priming/padding. Set to
    /// [`Self::kConverterPrimeMethod_None`] so the decoder emits raw
    /// `'adts'` — raw AAC ADTS-framed bitstream file-type hint.
    pub(crate) const kAudioFileAAC_ADTSType: u32 = AUDIO_FILE_AAC_ADTS_TYPE;
    /// `'caff'` — Core Audio Format file-type hint.
    pub(crate) const kAudioFileCAFType: u32 = AUDIO_FILE_CAF_TYPE;
    /// `'flac'` — native FLAC bitstream file-type hint (macOS 10.13+ /
    /// iOS 11+). Same four-cc as [`Self::kAudioFormatFLAC`].
    pub(crate) const kAudioFileFLACType: u32 = AUDIO_FILE_FLAC_TYPE;
    /// `'m4af'` — M4A (ALAC / AAC) file-type hint.
    pub(crate) const kAudioFileM4AType: u32 = AUDIO_FILE_M4A_TYPE;
    /// `'MPG3'` — MPEG-1/2 Layer 3 file-type hint.
    pub(crate) const kAudioFileMP3Type: u32 = AUDIO_FILE_MP3_TYPE;
    /// `'WAVE'` — RIFF WAV file-type hint for `audio_file_open_with_callbacks`.
    pub(crate) const kAudioFileWAVEType: u32 = AUDIO_FILE_WAVE_TYPE;
    /// `'alac'` — Apple Lossless Audio Codec input format ID.
    pub(crate) const kAudioFormatAppleLossless: AudioFormatID = AUDIO_FORMAT_APPLE_LOSSLESS;
    pub(crate) const kAudioFormatFLAC: AudioFormatID = AUDIO_FORMAT_FLAC;
    pub(crate) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PACKED;
    pub(crate) const kAudioFormatLinearPCM: AudioFormatID = AUDIO_FORMAT_LINEAR_PCM;
    /// `'aac '` — MPEG-4 AAC LC input format ID. Also used as a hint for
    /// HE-AAC v1/v2: the actual codec class is derived from the ESDS
    /// cookie via `kAudioFormatProperty_FormatList`.
    pub(crate) const kAudioFormatMPEG4AAC: AudioFormatID = AUDIO_FORMAT_MPEG4_AAC;
    /// `'.mp3'` — MPEG-1/2 Layer 3 input format ID.
    pub(crate) const kAudioFormatMPEGLayer3: AudioFormatID = AUDIO_FORMAT_MPEG_LAYER3;
    /// `'flst'` — `audio_format_get_property_raw` property that enumerates all
    /// `AudioFormatListItem`s an ESDS-wrapped cookie can decode to. For
    /// HE-AAC v1/v2 the ESDS encodes multiple compatible layers (LC
    /// base, HE-AAC v1 SBR, HE-AAC v2 PS). Apple returns them sorted
    /// from MOST rich to LEAST rich (channel count first, then sample
    /// rate), so `items[0]` is the full output format (e.g. stereo
    /// 44.1 kHz for HE-AAC v2). Specifier = `AudioFormatInfo` struct
    /// containing a partial ASBD (`format_id` required) + ESDS cookie.
    pub(crate) const kAudioFormatProperty_FormatList: u32 = AUDIO_FORMAT_PROPERTY_FORMAT_LIST;
    pub(crate) const noErr: OSStatus = kithara_apple::audio_toolbox::NO_ERR;
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
        assert_eq!(os_status_to_string(Consts::noErr), "noErr");
        assert!(os_status_to_string(0x7768_743f).contains("wht?"));
    }
}
