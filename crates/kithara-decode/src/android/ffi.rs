#![allow(unsafe_code)]

use std::ffi::CStr;
#[cfg(target_os = "android")]
use std::ffi::{c_char, c_void};
#[cfg(target_os = "android")]
use std::panic::catch_unwind;

#[cfg(target_os = "android")]
use jni::{signature::RuntimeFieldSignature, strings::JNIString, JavaVM};

pub(crate) const MEDIA_STATUS_OK: i32 = 0;
pub(crate) const MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED: i32 = -3;
pub(crate) const MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED: i32 = -2;
pub(crate) const MEDIA_CODEC_INFO_TRY_AGAIN_LATER: i32 = -1;
pub(crate) const PCM_ENCODING_16BIT: i32 = 2;
pub(crate) const PCM_ENCODING_FLOAT: i32 = 4;
pub(crate) const SEEK_MODE_PREVIOUS_SYNC: u32 = 0;
pub(crate) const MIN_HARDWARE_API_LEVEL: u32 = 28;

pub(crate) const KEY_MIME: &CStr = c"mime";
pub(crate) const KEY_SAMPLE_RATE: &CStr = c"sample-rate";
pub(crate) const KEY_CHANNEL_COUNT: &CStr = c"channel-count";
pub(crate) const KEY_DURATION_US: &CStr = c"durationUs";
pub(crate) const KEY_PCM_ENCODING: &CStr = c"pcm-encoding";
pub(crate) const KEY_ENCODER_DELAY: &CStr = c"encoder-delay";
pub(crate) const KEY_ENCODER_PADDING: &CStr = c"encoder-padding";

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

    pub(crate) fn AMediaFormat_delete(format: *mut AMediaFormat);
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
    runtime_api_level().or_else(compile_time_api_level)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn current_api_level() -> Option<u32> {
    // Host builds still compile Android capability logic and its tests, but
    // they must never claim that MediaCodec is available at runtime.
    None
}

#[cfg(target_os = "android")]
fn runtime_api_level() -> Option<u32> {
    let context = catch_unwind(ndk_context::android_context).ok()?;
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
    let sig = RuntimeFieldSignature::from_str("I").ok()?;
    vm.attach_current_thread(|env| -> jni::errors::Result<u32> {
        let build_version = env.find_class(JNIString::from("android/os/Build$VERSION"))?;
        let sdk_int = env
            .get_static_field(build_version, JNIString::from("SDK_INT"), sig.field_signature())?
            .i()?;
        u32::try_from(sdk_int).map_err(|e| jni::errors::Error::ParseFailed(e.to_string()))
    })
    .ok()
}

#[cfg(target_os = "android")]
fn compile_time_api_level() -> Option<u32> {
    option_env!("CARGO_NDK_PLATFORM").and_then(|value| value.parse::<u32>().ok())
}

#[cfg(target_os = "android")]
pub(crate) fn capability_api_level() -> Option<u32> {
    compile_time_api_level()
}

#[cfg(not(target_os = "android"))]
pub(crate) fn capability_api_level() -> Option<u32> {
    None
}

#[must_use]
pub(crate) fn api_level_allows_hardware(api_level: Option<u32>) -> bool {
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
        assert_eq!(KEY_ENCODER_DELAY.to_str().ok(), Some("encoder-delay"));
        assert_eq!(KEY_ENCODER_PADDING.to_str().ok(), Some("encoder-padding"));
        assert_eq!(current_api_level(), None);
        assert!(api_level_allows_hardware(Some(MIN_HARDWARE_API_LEVEL)));
        assert!(!api_level_allows_hardware(Some(MIN_HARDWARE_API_LEVEL - 1)));
    }
}
