//! `FourCC` constants and a small helper to stringify `OSStatus` values.

#![allow(non_upper_case_globals)]

use super::ffi::{AudioFileTypeID, AudioFormatFlags, AudioFormatID, OSStatus};

pub(super) struct Consts;

impl Consts {
    pub(super) const noErr: OSStatus = 0;
    pub(super) const kAudioFileStreamError_NotOptimized: OSStatus = 0x6f707469; // 'opti'
    pub(super) const kAudioConverterErr_NoDataNow: OSStatus = 0x21646174; // '!dat'
    pub(super) const kAudioFormatLinearPCM: AudioFormatID = 0x6c70636d; // 'lpcm'
    pub(super) const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    pub(super) const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    pub(super) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;
    pub(super) const kAudioFileAAC_ADTSType: AudioFileTypeID = 0x61647473; // 'adts'
    pub(super) const kAudioFileM4AType: AudioFileTypeID = 0x6d346166; // 'm4af'
    pub(super) const kAudioFileMP3Type: AudioFileTypeID = 0x4d504733; // 'MPG3'
    pub(super) const kAudioFileFLACType: AudioFileTypeID = 0x666c6163; // 'flac'
    pub(super) const kAudioFileCAFType: AudioFileTypeID = 0x63616666; // 'caff'
    pub(super) const kAudioFileStreamProperty_ReadyToProducePackets: u32 = 0x72656479; // 'redy'
    pub(super) const kAudioFileStreamProperty_DataFormat: u32 = 0x64666d74; // 'dfmt'
    pub(super) const kAudioFileStreamProperty_MagicCookieData: u32 = 0x6d676963; // 'mgic'
    pub(super) const kAudioFileStreamProperty_DataOffset: u32 = 0x646f6666; // 'doff'
    pub(super) const kAudioFileStreamProperty_AudioDataPacketCount: u32 = 0x70636e74; // 'pcnt'
    pub(super) const kAudioFileStreamParseFlag_Discontinuity: u32 = 1;
    pub(super) const BYTES_PER_F32_SAMPLE: u32 = 4;
    pub(super) const BITS_PER_F32_SAMPLE: u32 = 32;
    pub(super) const PARSE_READ_BUFFER_SIZE: usize = 32768;
    pub(super) const MAX_PARSE_BYTES: usize = 1024 * 1024;
    pub(super) const DEFAULT_BUFFER_FRAMES: usize = 1024;
    pub(super) const MIN_PACKETS_FOR_DECODE: usize = 4;
    pub(super) const DEFAULT_SEEK_DURATION_SECS: f64 = 300.0;
    pub(super) const kAudioConverterDecompressionMagicCookie: u32 = 0x646d6763; // 'dmgc'
}

/// Decode a `FourCC`-style `OSStatus` into an ASCII tag when possible.
pub(super) fn os_status_to_string(status: OSStatus) -> String {
    if status == Consts::noErr {
        return "Consts::noErr".to_string();
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
        assert_eq!(os_status_to_string(Consts::noErr), "Consts::noErr");
        // 'wht?' = 0x7768743f
        assert!(os_status_to_string(0x7768743f).contains("wht?"));
    }
}
