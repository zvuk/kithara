//! `FourCC` constants and a small helper to stringify `OSStatus` values.

#![allow(non_upper_case_globals)]

use super::ffi::{AudioFilePropertyID, AudioFileTypeID, AudioFormatFlags, AudioFormatID, OSStatus};

pub(super) struct Consts;

impl Consts {
    pub(super) const noErr: OSStatus = 0;
    pub(super) const kAudioFileEndOfFileError: OSStatus = -39;
    pub(super) const kAudioFilePositionError: OSStatus = -40;

    pub(super) const kAudioConverterErr_NoDataNow: OSStatus = 0x21646174; // '!dat'

    pub(super) const kAudioFormatLinearPCM: AudioFormatID = 0x6c70636d; // 'lpcm'
    pub(super) const kAudioFormatMPEG4AAC: AudioFormatID = 0x61616320; // 'aac '
    pub(super) const kAudioFormatFLAC: AudioFormatID = 0x666c6163; // 'flac'
    /// AAC always decodes 1024 samples per packet (SBR spectral bands
    /// use the same packet boundary).
    pub(super) const AAC_FRAMES_PER_PACKET: u32 = 1024;
    pub(super) const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    pub(super) const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    pub(super) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;

    // AudioFile container-type hints
    pub(super) const kAudioFileAAC_ADTSType: AudioFileTypeID = 0x61647473; // 'adts'
    pub(super) const kAudioFileMPEG4Type: AudioFileTypeID = 0x6d703466; // 'mp4f'
    pub(super) const kAudioFileMP3Type: AudioFileTypeID = 0x4d504733; // 'MPG3'
    pub(super) const kAudioFileFLACType: AudioFileTypeID = 0x666c6163; // 'flac'
    pub(super) const kAudioFileCAFType: AudioFileTypeID = 0x63616666; // 'caff'
    pub(super) const kAudioFileWAVEType: AudioFileTypeID = 0x57415645; // 'WAVE'

    // AudioFile property IDs
    pub(super) const kAudioFilePropertyDataFormat: AudioFilePropertyID = 0x64666d74; // 'dfmt'
    pub(super) const kAudioFilePropertyMaximumPacketSize: AudioFilePropertyID = 0x70737a65; // 'psze'
    pub(super) const kAudioFilePropertyPacketSizeUpperBound: AudioFilePropertyID = 0x706b7562; // 'pkub'
    pub(super) const kAudioFilePropertyMagicCookieData: AudioFilePropertyID = 0x6d676963; // 'mgic'
    pub(super) const kAudioFilePropertyEstimatedDuration: AudioFilePropertyID = 0x65647572; // 'edur'
    pub(super) const kAudioFilePropertyFrameToPacket: AudioFilePropertyID = 0x66727470; // 'frtp'
    pub(super) const kAudioFilePropertyPacketToByte: AudioFilePropertyID = 0x706b6279; // 'pkby'

    pub(super) const kAudioConverterDecompressionMagicCookie: u32 = 0x646d6763; // 'dmgc'

    pub(super) const BYTES_PER_F32_SAMPLE: u32 = 4;
    pub(super) const BITS_PER_F32_SAMPLE: u32 = 32;
    pub(super) const DEFAULT_BUFFER_FRAMES: usize = 1024;

    /// Size of the FLAC STREAMINFO metadata block body (fixed by spec).
    pub(super) const FLAC_STREAMINFO_LEN: usize = 34;
    /// Bytes prepended to STREAMINFO in the Apple FLAC magic cookie:
    /// 4 bytes `"fLaC"` marker + 4 bytes `METADATA_BLOCK_HEADER`.
    pub(super) const FLAC_COOKIE_PREFIX_LEN: usize = 8;
}

/// Decode a `FourCC`-style `OSStatus` into an ASCII tag when possible.
pub(super) fn os_status_to_string(status: OSStatus) -> String {
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
        // 'wht?' = 0x7768743f
        assert!(os_status_to_string(0x7768743f).contains("wht?"));
    }
}
