#![allow(unsafe_code)]

use std::ffi::CStr;
#[cfg(target_os = "android")]
use std::ffi::{c_char, c_void};

pub(crate) const MEDIA_STATUS_OK: i32 = 0;
pub(crate) const MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED: i32 = -3;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED: i32 = -2;
pub(crate) const MEDIA_CODEC_INFO_TRY_AGAIN_LATER: i32 = -1;
pub(crate) const PCM_ENCODING_16BIT: i32 = 2;
pub(crate) const PCM_ENCODING_FLOAT: i32 = 4;
pub(crate) const SEEK_MODE_PREVIOUS_SYNC: u32 = 0;

pub(crate) const KEY_MIME: &CStr = c"mime";
pub(crate) const KEY_SAMPLE_RATE: &CStr = c"sample-rate";
pub(crate) const KEY_CHANNEL_COUNT: &CStr = c"channel-count";
pub(crate) const KEY_DURATION_US: &CStr = c"durationUs";
pub(crate) const KEY_PCM_ENCODING: &CStr = c"pcm-encoding";
/// `AMEDIAFORMAT_KEY_SAMPLE_FILE_OFFSET` — present on the per-sample
/// format returned by `AMediaExtractor_getSampleFormat` (API 28+).
/// Carries the byte offset of the current sample in the source file.
pub(crate) const KEY_SAMPLE_FILE_OFFSET: &CStr = c"sample-file-offset";
/// Codec-specific data payload (`csd-0`) — `AudioSpecificConfig` for AAC,
/// `STREAMINFO` for FLAC. Required when configuring the codec without
/// an `AMediaExtractor`-supplied format.
pub(crate) const KEY_CSD_0: &CStr = c"csd-0";

/// Android MediaCodec MIME type for AAC (raw frames or fMP4-stripped).
pub(crate) const MIME_AAC: &CStr = c"audio/mp4a-latm";
/// Android MediaCodec MIME type for FLAC.
pub(crate) const MIME_FLAC: &CStr = c"audio/flac";

#[cfg(target_os = "android")]
pub(crate) type MediaStatus = i32;
#[cfg(target_os = "android")]
pub(crate) type Off64 = i64;
#[cfg(target_os = "android")]
pub(crate) type SSize = isize;

#[cfg(target_os = "android")]
#[repr(C)]
pub(crate) struct AMediaCodecBufferInfo {
    pub(crate) offset: i32,
    pub(crate) size: i32,
    pub(crate) presentation_time_us: i64,
    pub(crate) flags: u32,
}

#[cfg(target_os = "android")]
#[repr(C)]
pub(crate) struct AMediaCodec {
    _private: [u8; 0],
}

#[cfg(target_os = "android")]
#[repr(C)]
pub(crate) struct AMediaDataSource {
    _private: [u8; 0],
}

#[cfg(target_os = "android")]
#[repr(C)]
pub(crate) struct AMediaExtractor {
    _private: [u8; 0],
}

#[cfg(target_os = "android")]
#[repr(C)]
pub(crate) struct AMediaFormat {
    _private: [u8; 0],
}

#[cfg(target_os = "android")]
pub(crate) type AMediaDataSourceReadAt =
    Option<unsafe extern "C" fn(*mut c_void, Off64, *mut c_void, usize) -> SSize>;
#[cfg(target_os = "android")]
pub(crate) type AMediaDataSourceGetSize = Option<unsafe extern "C" fn(*mut c_void) -> Off64>;
#[cfg(target_os = "android")]
pub(crate) type AMediaDataSourceClose = Option<unsafe extern "C" fn(*mut c_void)>;

#[cfg(target_os = "android")]
#[link(name = "android")]
unsafe extern "C" {
    fn android_get_device_api_level() -> i32;
}

#[cfg(target_os = "android")]
#[link(name = "mediandk")]
unsafe extern "C" {
    pub(crate) fn AMediaDataSource_new() -> *mut AMediaDataSource;
    pub(crate) fn AMediaDataSource_delete(source: *mut AMediaDataSource);
    pub(crate) fn AMediaDataSource_setUserdata(
        source: *mut AMediaDataSource,
        userdata: *mut c_void,
    );
    pub(crate) fn AMediaDataSource_setReadAt(
        source: *mut AMediaDataSource,
        callback: AMediaDataSourceReadAt,
    );
    pub(crate) fn AMediaDataSource_setGetSize(
        source: *mut AMediaDataSource,
        callback: AMediaDataSourceGetSize,
    );
    pub(crate) fn AMediaDataSource_setClose(
        source: *mut AMediaDataSource,
        callback: AMediaDataSourceClose,
    );

    pub(crate) fn AMediaExtractor_new() -> *mut AMediaExtractor;
    pub(crate) fn AMediaExtractor_delete(extractor: *mut AMediaExtractor) -> MediaStatus;
    pub(crate) fn AMediaExtractor_setDataSourceCustom(
        extractor: *mut AMediaExtractor,
        data_source: *mut AMediaDataSource,
    ) -> MediaStatus;
    pub(crate) fn AMediaExtractor_getTrackCount(extractor: *mut AMediaExtractor) -> usize;
    pub(crate) fn AMediaExtractor_getTrackFormat(
        extractor: *mut AMediaExtractor,
        index: usize,
    ) -> *mut AMediaFormat;
    pub(crate) fn AMediaExtractor_selectTrack(
        extractor: *mut AMediaExtractor,
        index: usize,
    ) -> MediaStatus;
    pub(crate) fn AMediaExtractor_seekTo(
        extractor: *mut AMediaExtractor,
        seek_pos_us: i64,
        mode: u32,
    ) -> MediaStatus;
    pub(crate) fn AMediaExtractor_readSampleData(
        extractor: *mut AMediaExtractor,
        buffer: *mut u8,
        capacity: usize,
    ) -> SSize;
    pub(crate) fn AMediaExtractor_getSampleTime(extractor: *mut AMediaExtractor) -> i64;
    pub(crate) fn AMediaExtractor_getSampleTrackIndex(extractor: *mut AMediaExtractor) -> i32;
    pub(crate) fn AMediaExtractor_advance(extractor: *mut AMediaExtractor) -> bool;
    /// Available since API 28 (Android 9). Populates `*out_format` with a
    /// new `AMediaFormat` carrying per-sample metadata, including
    /// `"sample-file-offset"` (the byte offset of the current sample in
    /// the source file).  Caller owns the returned format and must free
    /// it with [`AMediaFormat_delete`].
    pub(crate) fn AMediaExtractor_getSampleFormat(
        extractor: *mut AMediaExtractor,
        out_format: *mut *mut AMediaFormat,
    ) -> MediaStatus;

    pub(crate) fn AMediaCodec_createDecoderByType(mime_type: *const c_char) -> *mut AMediaCodec;
    pub(crate) fn AMediaCodec_delete(codec: *mut AMediaCodec) -> MediaStatus;
    pub(crate) fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *mut AMediaFormat,
        surface: *mut c_void,
        crypto: *mut c_void,
        flags: u32,
    ) -> MediaStatus;
    pub(crate) fn AMediaCodec_start(codec: *mut AMediaCodec) -> MediaStatus;
    pub(crate) fn AMediaCodec_stop(codec: *mut AMediaCodec) -> MediaStatus;
    pub(crate) fn AMediaCodec_flush(codec: *mut AMediaCodec) -> MediaStatus;
    pub(crate) fn AMediaCodec_getOutputFormat(codec: *mut AMediaCodec) -> *mut AMediaFormat;
    pub(crate) fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_getOutputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64)
    -> SSize;
    pub(crate) fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        offset: i64,
        size: usize,
        time: u64,
        flags: u32,
    ) -> MediaStatus;
    pub(crate) fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> SSize;
    pub(crate) fn AMediaCodec_releaseOutputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        render: bool,
    ) -> MediaStatus;

    pub(crate) fn AMediaFormat_new() -> *mut AMediaFormat;
    pub(crate) fn AMediaFormat_delete(format: *mut AMediaFormat);
    pub(crate) fn AMediaFormat_setString(
        format: *mut AMediaFormat,
        name: *const c_char,
        value: *const c_char,
    );
    pub(crate) fn AMediaFormat_setBuffer(
        format: *mut AMediaFormat,
        name: *const c_char,
        data: *const c_void,
        size: usize,
    );
    pub(crate) fn AMediaFormat_getString(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut *const c_char,
    ) -> bool;
    pub(crate) fn AMediaFormat_getInt32(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut i32,
    ) -> bool;
    pub(crate) fn AMediaFormat_getInt64(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut i64,
    ) -> bool;
    pub(crate) fn AMediaFormat_setInt32(
        format: *mut AMediaFormat,
        name: *const c_char,
        value: i32,
    ) -> bool;
}

#[cfg(target_os = "android")]
pub(crate) fn current_api_level() -> Option<u32> {
    let level = unsafe { android_get_device_api_level() };
    (level >= 0).then_some(level as u32)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn current_api_level() -> Option<u32> {
    // Host builds still compile Android capability logic and its tests, but
    // they must never claim that MediaCodec is available at runtime.
    None
}

#[must_use]
pub(crate) fn api_level_allows_hardware(api_level: Option<u32>) -> bool {
    /// Minimum Android API level that supports the hardware decoder path.
    const MIN_HARDWARE_API_LEVEL: u32 = 28;
    api_level.is_some_and(|level| level >= MIN_HARDWARE_API_LEVEL)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn api_gate_and_keys_are_stable() {
        assert_eq!(MEDIA_STATUS_OK, 0);
        assert_eq!(MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM, 4);
        assert_eq!(MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED, -3);
        assert_eq!(MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED, -2);
        assert_eq!(MEDIA_CODEC_INFO_TRY_AGAIN_LATER, -1);
        assert_eq!(PCM_ENCODING_16BIT, 2);
        assert_eq!(PCM_ENCODING_FLOAT, 4);
        assert_eq!(SEEK_MODE_PREVIOUS_SYNC, 0);
        assert_eq!(KEY_MIME.to_str().ok(), Some("mime"));
        assert_eq!(KEY_SAMPLE_RATE.to_str().ok(), Some("sample-rate"));
        assert_eq!(KEY_CHANNEL_COUNT.to_str().ok(), Some("channel-count"));
        assert_eq!(KEY_DURATION_US.to_str().ok(), Some("durationUs"));
        assert_eq!(KEY_PCM_ENCODING.to_str().ok(), Some("pcm-encoding"));
        // mirrors `MIN_HARDWARE_API_LEVEL` defined inside
        // `api_level_allows_hardware`.
        const MIN_HARDWARE_API_LEVEL: u32 = 28;
        assert!(api_level_allows_hardware(Some(MIN_HARDWARE_API_LEVEL)));
        assert!(!api_level_allows_hardware(Some(MIN_HARDWARE_API_LEVEL - 1)));
    }
}
