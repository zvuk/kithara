//! `FourCC` constants and a small helper to stringify `OSStatus` values.

#![allow(non_upper_case_globals)]

use super::ffi::{AudioFilePropertyID, AudioFileTypeID, AudioFormatFlags, AudioFormatID, OSStatus};

pub(crate) struct Consts;

impl Consts {
    /// AAC always decodes 1024 samples per packet (SBR spectral bands
    /// use the same packet boundary).
    pub(crate) const AAC_FRAMES_PER_PACKET: u32 = 1024;
    pub(crate) const BITS_PER_F32_SAMPLE: u32 = 32;
    pub(crate) const BYTES_PER_F32_SAMPLE: u32 = 4;
    pub(crate) const DEFAULT_BUFFER_FRAMES: usize = 1024;
    /// Bytes prepended to STREAMINFO in the Apple FLAC magic cookie:
    /// 4 bytes `"fLaC"` marker + 4 bytes `METADATA_BLOCK_HEADER`.
    pub(crate) const FLAC_COOKIE_PREFIX_LEN: usize = 8;
    /// Size of the FLAC STREAMINFO metadata block body (fixed by spec).
    pub(crate) const FLAC_STREAMINFO_LEN: usize = 34;
    pub(crate) const kAudioConverterDecompressionMagicCookie: u32 = 0x646d6763; // 'dmgc'
    pub(crate) const kAudioConverterErr_NoDataNow: OSStatus = 0x21646174; // '!dat'
    // AudioFile container-type hints
    pub(crate) const kAudioFileAAC_ADTSType: AudioFileTypeID = 0x61647473; // 'adts'
    pub(crate) const kAudioFileCAFType: AudioFileTypeID = 0x63616666; // 'caff'
    pub(crate) const kAudioFileEndOfFileError: OSStatus = -39;
    pub(crate) const kAudioFileFLACType: AudioFileTypeID = 0x666c6163; // 'flac'
    pub(crate) const kAudioFileMP3Type: AudioFileTypeID = 0x4d504733; // 'MPG3'
    pub(crate) const kAudioFileMPEG4Type: AudioFileTypeID = 0x6d703466; // 'mp4f'
    pub(crate) const kAudioFilePositionError: OSStatus = -40;
    // AudioFile property IDs
    pub(crate) const kAudioFilePropertyDataFormat: AudioFilePropertyID = 0x64666d74; // 'dfmt'
    pub(crate) const kAudioFilePropertyEstimatedDuration: AudioFilePropertyID = 0x65647572; // 'edur'
    pub(crate) const kAudioFilePropertyFrameToPacket: AudioFilePropertyID = 0x66727470; // 'frtp'
    pub(crate) const kAudioFilePropertyMagicCookieData: AudioFilePropertyID = 0x6d676963; // 'mgic'
    pub(crate) const kAudioFilePropertyMaximumPacketSize: AudioFilePropertyID = 0x70737a65; // 'psze'
    pub(crate) const kAudioFilePropertyPacketSizeUpperBound: AudioFilePropertyID = 0x706b7562; // 'pkub'
    pub(crate) const kAudioFilePropertyPacketToByte: AudioFilePropertyID = 0x706b6279; // 'pkby'
    pub(crate) const kAudioFileWAVEType: AudioFileTypeID = 0x57415645; // 'WAVE'
    pub(crate) const kAudioFormatFLAC: AudioFormatID = 0x666c6163; // 'flac'
    pub(crate) const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    pub(crate) const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    pub(crate) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;
    pub(crate) const kAudioFormatLinearPCM: AudioFormatID = 0x6c70636d; // 'lpcm'
    pub(crate) const kAudioFormatMPEG4AAC: AudioFormatID = 0x61616320; // 'aac '
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
        // 'wht?' = 0x7768743f
        assert!(os_status_to_string(0x7768743f).contains("wht?"));
    }
}
