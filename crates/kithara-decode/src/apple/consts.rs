//! `FourCC` constants and a small helper to stringify `OSStatus` values.
//!
//! Codec-only surface kept after the legacy AudioFile-based decoder was
//! removed; only the constants `crate::codec::AppleCodec` references
//! survive.

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
    pub(crate) const kAudioConverterDecompressionMagicCookie: u32 = 0x646d_6763; // 'dmgc'
    pub(crate) const kAudioConverterErr_NoDataNow: OSStatus = 0x2164_6174; // '!dat'
    pub(crate) const kAudioFormatFLAC: AudioFormatID = 0x666c_6163; // 'flac'
    pub(crate) const kAudioFormatFlagIsFloat: AudioFormatFlags = 1 << 0;
    pub(crate) const kAudioFormatFlagIsPacked: AudioFormatFlags = 1 << 3;
    pub(crate) const kAudioFormatFlagsNativeFloatPacked: AudioFormatFlags =
        Self::kAudioFormatFlagIsFloat | Self::kAudioFormatFlagIsPacked;
    pub(crate) const kAudioFormatLinearPCM: AudioFormatID = 0x6c70_636d; // 'lpcm'
    pub(crate) const kAudioFormatMPEG4AAC: AudioFormatID = 0x6161_6320; // 'aac '
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
        assert!(os_status_to_string(0x7768_743f).contains("wht?"));
    }
}
