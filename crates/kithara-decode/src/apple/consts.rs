#![allow(non_upper_case_globals)]

use super::ffi::{AudioFormatFlags, AudioFormatID, OSStatus};

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
    pub(crate) const kAudioConverterDecompressionMagicCookie: u32 = 0x646d_6763;
    pub(crate) const kAudioConverterErr_NoDataNow: OSStatus = 0x2164_6174;
    /// `'prim'` — codec-reported `AudioConverterPrimeInfo` (encoder
    /// priming + trailing padding in PCM frames). Available after the
    /// converter has consumed at least one input packet.
    pub(crate) const kAudioConverterPrimeInfo: u32 = 0x7072_696d;
    /// `'pmth'` — `AudioConverterPrimeMethod`: controls how the
    /// converter handles codec-side priming/padding. Set to
    /// [`Self::kConverterPrimeMethod_None`] so the decoder emits raw
    /// `'adts'` — raw AAC ADTS-framed bitstream file-type hint.
    pub(crate) const kAudioFileAAC_ADTSType: u32 = 0x6164_7473;
    /// `'caff'` — Core Audio Format file-type hint.
    pub(crate) const kAudioFileCAFType: u32 = 0x6361_6666;
    /// `'m4af'` — M4A (ALAC / AAC) file-type hint.
    pub(crate) const kAudioFileM4AType: u32 = 0x6d34_6166;
    /// `'MPG3'` — MPEG-1/2 Layer 3 file-type hint.
    pub(crate) const kAudioFileMP3Type: u32 = 0x4d50_4733;
    /// `'pcnt'` — total audio data packet count.
    pub(crate) const kAudioFilePropertyAudioDataPacketCount: u32 = 0x7063_6e74;
    /// `'dfmt'` — ASBD of the file's canonical audio data format.
    pub(crate) const kAudioFilePropertyDataFormat: u32 = 0x6466_6d74;
    /// `'mgic'` — codec-specific magic cookie (ALAC, ESDS-wrapped AAC).
    pub(crate) const kAudioFilePropertyMagicCookieData: u32 = 0x6d67_6963;
    /// `'psze'` — maximum audio packet size in bytes.
    pub(crate) const kAudioFilePropertyMaximumPacketSize: u32 = 0x7073_7a65;
    /// `'WAVE'` — RIFF WAV file-type hint for `AudioFileOpenWithCallbacks`.
    pub(crate) const kAudioFileWAVEType: u32 = 0x5741_5645;
    /// `'alac'` — Apple Lossless Audio Codec input format ID.
    pub(crate) const kAudioFormatAppleLossless: AudioFormatID = 0x616c_6163;
    pub(crate) const kAudioFormatFLAC: AudioFormatID = 0x666c_6163;
    pub(crate) const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    pub(crate) const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    pub(crate) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;
    pub(crate) const kAudioFormatLinearPCM: AudioFormatID = 0x6c70_636d;
    /// `'aac '` — MPEG-4 AAC LC input format ID. Also used as a hint for
    /// HE-AAC v1/v2: the actual codec class is derived from the ESDS
    /// cookie via `kAudioFormatProperty_FormatList`.
    pub(crate) const kAudioFormatMPEG4AAC: AudioFormatID = 0x6161_6320;
    /// `'.mp3'` — MPEG-1/2 Layer 3 input format ID.
    pub(crate) const kAudioFormatMPEGLayer3: AudioFormatID = 0x2e6d_7033;
    /// `'flst'` — `AudioFormatGetProperty` property that enumerates all
    /// `AudioFormatListItem`s an ESDS-wrapped cookie can decode to. For
    /// HE-AAC v1/v2 the ESDS encodes multiple compatible layers (LC
    /// base, HE-AAC v1 SBR, HE-AAC v2 PS). Apple returns them sorted
    /// from MOST rich to LEAST rich (channel count first, then sample
    /// rate), so `items[0]` is the full output format (e.g. stereo
    /// 44.1 kHz for HE-AAC v2). Specifier = `AudioFormatInfo` struct
    /// containing a partial ASBD (`mFormatID` required) + ESDS cookie.
    pub(crate) const kAudioFormatProperty_FormatList: u32 = 0x666c_7374;
    pub(crate) const noErr: OSStatus = 0;
}

/// Decode a `FourCC`-style `OSStatus` into an ASCII tag when possible.
pub(crate) fn os_status_to_string(status: OSStatus) -> String {
    if status == Consts::noErr {
        return "noErr".to_string();
    }
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        let s: String = bytes.iter().map(|&b| b as char).collect();
        format!("'{}' ({})", s, status)
    } else {
        format!("{}", status)
    }
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
