#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};

pub(crate) const MEDIA_STATUS_OK: i32 = 0;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED: i32 = -3;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED: i32 = -2;
pub(crate) const MEDIA_CODEC_INFO_TRY_AGAIN_LATER: i32 = -1;
pub(crate) const MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
pub(crate) const PCM_ENCODING_16BIT: i32 = 2;
pub(crate) const PCM_ENCODING_FLOAT: i32 = 4;
pub(crate) const SEEK_MODE_PREVIOUS_SYNC: u32 = 0;

pub(crate) const KEY_ENCODER_DELAY: &CStr = c"encoder-delay";
pub(crate) const KEY_ENCODER_PADDING: &CStr = c"encoder-padding";
pub(crate) const KEY_MIME: &CStr = c"mime";
pub(crate) const KEY_SAMPLE_RATE: &CStr = c"sample-rate";
pub(crate) const KEY_CHANNEL_COUNT: &CStr = c"channel-count";
pub(crate) const KEY_DURATION_US: &CStr = c"durationUs";
pub(crate) const KEY_PCM_ENCODING: &CStr = c"pcm-encoding";
/// Codec-specific data payload (`csd-0`) — `AudioSpecificConfig` for AAC,
/// `STREAMINFO` for FLAC. Required when configuring the codec without
/// an `AMediaExtractor`-supplied format.
pub(crate) const KEY_CSD_0: &CStr = c"csd-0";

/// Android `MediaCodec` MIME type for AAC (raw frames or fMP4-stripped).
pub(crate) const MIME_AAC: &CStr = c"audio/mp4a-latm";
/// Android `MediaCodec` MIME type for FLAC.
pub(crate) const MIME_FLAC: &CStr = c"audio/flac";
/// Android `MediaCodec` MIME type for raw PCM. WAV passes through with no
/// decoding work — the extractor's per-sample bytes are interleaved PCM.
pub(crate) const MIME_RAW: &CStr = c"audio/raw";
/// Android `MediaCodec` MIME type for MPEG-1/2 Layer 3 (MP3).
pub(crate) const MIME_MP3: &CStr = c"audio/mpeg";
/// Android `MediaCodec` MIME type for Apple Lossless (ALAC).
pub(crate) const MIME_ALAC: &CStr = c"audio/alac";

pub(crate) type MediaStatus = i32;
pub(crate) type Off64 = i64;
pub(crate) type SSize = isize;

#[repr(C)]
pub(crate) struct AMediaCodecBufferInfo {
    pub(crate) offset: i32,
    pub(crate) size: i32,
    pub(crate) presentation_time_us: i64,
    pub(crate) flags: u32,
}

#[repr(C)]
pub(crate) struct AMediaCodec {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct AMediaDataSource {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct AMediaExtractor {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct AMediaFormat {
    _private: [u8; 0],
}

pub(crate) type AMediaDataSourceReadAt =
    Option<unsafe extern "C" fn(*mut c_void, Off64, *mut c_void, usize) -> SSize>;
pub(crate) type AMediaDataSourceGetSize = Option<unsafe extern "C" fn(*mut c_void) -> Off64>;

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
    pub(crate) fn AMediaExtractor_advance(extractor: *mut AMediaExtractor) -> bool;

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
    pub(crate) fn AMediaFormat_setInt32(format: *mut AMediaFormat, name: *const c_char, value: i32);
    /// `AMediaFormat_getBuffer` — fetch a byte-buffer property (e.g.
    /// `csd-0`). Returns `false` when the key is absent; on success
    /// `*data` / `*size` borrow from the format's internal storage and
    /// are valid until the format is deleted.
    pub(crate) fn AMediaFormat_getBuffer(
        format: *mut AMediaFormat,
        name: *const c_char,
        data: *mut *mut c_void,
        size: *mut usize,
    ) -> bool;
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn ndk_constants_are_stable() {
        assert_eq!(MEDIA_STATUS_OK, 0);
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
    }
}
