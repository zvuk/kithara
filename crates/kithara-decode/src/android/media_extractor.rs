#![allow(unsafe_code)]
#![cfg(target_os = "android")]

use std::{
    ffi::{CStr, c_void},
    io::{Read, Seek, SeekFrom},
    ptr::{self, NonNull},
};

use super::ffi::{
    self, AMediaDataSource, AMediaDataSource_delete, AMediaDataSource_new,
    AMediaDataSource_setGetSize, AMediaDataSource_setReadAt, AMediaDataSource_setUserdata,
    AMediaDataSourceGetSize, AMediaDataSourceReadAt, AMediaExtractor, AMediaExtractor_advance,
    AMediaExtractor_delete, AMediaExtractor_getSampleTime, AMediaExtractor_getTrackCount,
    AMediaExtractor_getTrackFormat, AMediaExtractor_new, AMediaExtractor_readSampleData,
    AMediaExtractor_seekTo, AMediaExtractor_selectTrack, AMediaExtractor_setDataSourceCustom,
    AMediaFormat_delete, AMediaFormat_getBuffer, AMediaFormat_getInt32, AMediaFormat_getInt64,
    AMediaFormat_getString, KEY_CHANNEL_COUNT, KEY_CSD_0, KEY_DURATION_US, KEY_MIME,
    KEY_SAMPLE_RATE, MEDIA_STATUS_OK, Off64, SEEK_MODE_PREVIOUS_SYNC, SSize,
};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

struct DataSourceCtx {
    source: BoxedSource,
    size: i64,
}

/// Track-level info read out of an [`AMediaFormat`] handle.
pub(crate) struct TrackFormatInfo {
    pub(crate) mime: String,
    /// `csd-0` blob (AAC `AudioSpecificConfig`, FLAC `STREAMINFO`,
    /// ALAC magic cookie). Empty when the format reports none.
    pub(crate) csd_0: Vec<u8>,
    pub(crate) duration_us: i64,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
}

/// Safe wrapper around an Android NDK `AMediaExtractor` handle fed by a
/// custom `AMediaDataSource` that bridges to a Rust `Read + Seek` source.
pub(crate) struct AndroidMediaExtractor {
    _ctx: Box<DataSourceCtx>,
    data_source: NonNull<AMediaDataSource>,
    raw: NonNull<AMediaExtractor>,
    selected_track: Option<usize>,
    track_count: usize,
}

// SAFETY: `AMediaExtractor` handles are single-threaded; we never share
unsafe impl Send for AndroidMediaExtractor {}

impl AndroidMediaExtractor {
    /// Advance to the next sample. Returns `false` at EOF.
    pub(crate) fn advance(&mut self) -> bool {
        // SAFETY: extractor is live.
        unsafe { AMediaExtractor_advance(self.raw.as_ptr()) }
    }

    pub(crate) fn open(mut source: BoxedSource) -> DecodeResult<Self> {
        let end = source
            .seek(SeekFrom::End(0))
            .map_err(DecodeError::backend)?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(DecodeError::backend)?;
        let size = i64::try_from(end).map_err(DecodeError::backend)?;
        let mut ctx = Box::new(DataSourceCtx { source, size });

        // SAFETY: `AMediaDataSource_new` returns NULL on failure; we
        let ds = NonNull::new(unsafe { AMediaDataSource_new() })
            .ok_or_else(|| DecodeError::backend_msg("AMediaDataSource_new returned null"))?;

        // SAFETY: `ds` is live; `ctx` is boxed (stable address) and
        unsafe {
            AMediaDataSource_setUserdata(
                ds.as_ptr(),
                ctx.as_mut() as *mut DataSourceCtx as *mut c_void,
            );
            AMediaDataSource_setReadAt(ds.as_ptr(), Some(read_at_callback));
            AMediaDataSource_setGetSize(ds.as_ptr(), Some(get_size_callback));
        }

        // SAFETY: `AMediaExtractor_new` returns NULL on failure; we
        let ex = match NonNull::new(unsafe { AMediaExtractor_new() }) {
            Some(e) => e,
            None => {
                // SAFETY: `ds` is non-null and we're abandoning it
                unsafe { AMediaDataSource_delete(ds.as_ptr()) };
                return Err(DecodeError::backend_msg(
                    "AMediaExtractor_new returned null",
                ));
            }
        };

        // SAFETY: both handles are live; the extractor takes a reference
        let st = unsafe { AMediaExtractor_setDataSourceCustom(ex.as_ptr(), ds.as_ptr()) };
        if st != MEDIA_STATUS_OK {
            // SAFETY: both handles are non-null and freshly created; we
            unsafe {
                AMediaExtractor_delete(ex.as_ptr());
                AMediaDataSource_delete(ds.as_ptr());
            }
            return Err(DecodeError::backend_msg(format!(
                "AMediaExtractor_setDataSourceCustom failed: {st}"
            )));
        }

        // SAFETY: extractor is live.
        let track_count = unsafe { AMediaExtractor_getTrackCount(ex.as_ptr()) };

        Ok(Self {
            track_count,
            raw: ex,
            data_source: ds,
            _ctx: ctx,
            selected_track: None,
        })
    }

    /// Read the current sample into `buf`. Returns `Ok(Some((n, pts_us)))`
    /// or `Ok(None)` at EOF.
    pub(crate) fn read_sample(&mut self, buf: &mut [u8]) -> DecodeResult<Option<(usize, i64)>> {
        // SAFETY: extractor is live; `buf` is exclusively borrowed for
        let n = unsafe {
            AMediaExtractor_readSampleData(self.raw.as_ptr(), buf.as_mut_ptr(), buf.len())
        };
        if n < 0 {
            return Ok(None);
        }
        let read = usize::try_from(n).map_err(DecodeError::backend)?;
        // SAFETY: extractor is live.
        let pts_us = unsafe { AMediaExtractor_getSampleTime(self.raw.as_ptr()) };
        Ok(Some((read, pts_us)))
    }

    /// Seek to nearest previous-sync sample at or before `pts_us`.
    pub(crate) fn seek_to(&mut self, pts_us: i64) -> DecodeResult<()> {
        // SAFETY: extractor is live.
        let st =
            unsafe { AMediaExtractor_seekTo(self.raw.as_ptr(), pts_us, SEEK_MODE_PREVIOUS_SYNC) };
        if st != MEDIA_STATUS_OK {
            return Err(DecodeError::backend_msg(format!(
                "AMediaExtractor_seekTo failed: {st}"
            )));
        }
        Ok(())
    }

    pub(crate) fn select_audio_track(&mut self) -> DecodeResult<TrackFormatInfo> {
        for i in 0..self.track_count {
            let info = self.track_info(i)?;
            if info.mime.starts_with("audio/") {
                // SAFETY: extractor is live; `i` is bounded by
                let st = unsafe { AMediaExtractor_selectTrack(self.raw.as_ptr(), i) };
                if st != MEDIA_STATUS_OK {
                    return Err(DecodeError::backend_msg(format!(
                        "AMediaExtractor_selectTrack({i}) failed: {st}"
                    )));
                }
                self.selected_track = Some(i);
                return Ok(info);
            }
        }
        Err(DecodeError::backend_msg(
            "no audio track found in extractor",
        ))
    }

    pub(crate) fn track_count(&self) -> usize {
        self.track_count
    }

    pub(crate) fn track_info(&self, idx: usize) -> DecodeResult<TrackFormatInfo> {
        // SAFETY: extractor is live; the NDK bounds-checks `idx` and
        let fmt_raw = unsafe { AMediaExtractor_getTrackFormat(self.raw.as_ptr(), idx) };
        let fmt = NonNull::new(fmt_raw).ok_or_else(|| {
            DecodeError::backend_msg(format!(
                "AMediaExtractor_getTrackFormat({idx}) returned null"
            ))
        })?;
        let info = read_track_format(fmt);
        // SAFETY: we own the returned format handle; free it on return.
        unsafe { AMediaFormat_delete(fmt.as_ptr()) };
        info
    }
}

impl Drop for AndroidMediaExtractor {
    fn drop(&mut self) {
        // SAFETY: both handles are non-null until drop; freed exactly
        unsafe {
            AMediaExtractor_delete(self.raw.as_ptr());
            AMediaDataSource_delete(self.data_source.as_ptr());
        }
    }
}

fn read_track_format(fmt: NonNull<ffi::AMediaFormat>) -> DecodeResult<TrackFormatInfo> {
    let mut mime_ptr: *const std::ffi::c_char = ptr::null();
    // SAFETY: `fmt` is non-null; `mime_ptr` is an out-param. The NDK
    let ok = unsafe { AMediaFormat_getString(fmt.as_ptr(), KEY_MIME.as_ptr(), &mut mime_ptr) };
    if !ok || mime_ptr.is_null() {
        return Err(DecodeError::backend_msg("format missing mime"));
    }
    // SAFETY: `mime_ptr` is a NUL-terminated C string valid for the
    let mime = unsafe { CStr::from_ptr(mime_ptr) }
        .to_string_lossy()
        .into_owned();

    let mut sample_rate_i: i32 = 0;
    // SAFETY: `fmt` live; `sample_rate_i` exclusively borrowed.
    let _ = unsafe {
        AMediaFormat_getInt32(fmt.as_ptr(), KEY_SAMPLE_RATE.as_ptr(), &mut sample_rate_i)
    };
    let mut channels_i: i32 = 0;
    // SAFETY: `fmt` live; `channels_i` exclusively borrowed.
    let _ =
        unsafe { AMediaFormat_getInt32(fmt.as_ptr(), KEY_CHANNEL_COUNT.as_ptr(), &mut channels_i) };
    let mut duration_us: i64 = 0;
    // SAFETY: `fmt` live; `duration_us` exclusively borrowed.
    let _ =
        unsafe { AMediaFormat_getInt64(fmt.as_ptr(), KEY_DURATION_US.as_ptr(), &mut duration_us) };

    let mut csd_data: *mut c_void = ptr::null_mut();
    let mut csd_size: usize = 0;
    // SAFETY: out-params; `getBuffer` returns false when the key is
    let has_csd = unsafe {
        AMediaFormat_getBuffer(
            fmt.as_ptr(),
            KEY_CSD_0.as_ptr(),
            &mut csd_data,
            &mut csd_size,
        )
    };
    let csd_0 = if has_csd && !csd_data.is_null() && csd_size > 0 {
        // SAFETY: NDK returns a buffer valid for the lifetime of the
        unsafe { std::slice::from_raw_parts(csd_data as *const u8, csd_size) }.to_vec()
    } else {
        Vec::new()
    };

    Ok(TrackFormatInfo {
        mime,
        duration_us,
        csd_0,
        channels: u16::try_from(channels_i.max(0)).unwrap_or(2),
        sample_rate: u32::try_from(sample_rate_i.max(0)).unwrap_or(0),
    })
}

extern "C" fn read_at_callback(
    userdata: *mut c_void,
    offset: Off64,
    buffer: *mut c_void,
    size: usize,
) -> SSize {
    // SAFETY: `userdata` is the boxed `DataSourceCtx` pinned in
    let (ctx, slice) = unsafe {
        (
            &mut *(userdata as *mut DataSourceCtx),
            std::slice::from_raw_parts_mut(buffer as *mut u8, size),
        )
    };

    let Ok(pos) = u64::try_from(offset) else {
        return -1;
    };
    if ctx.source.seek(SeekFrom::Start(pos)).is_err() {
        return -1;
    }
    match ctx.source.read(slice) {
        Ok(n) => SSize::try_from(n).unwrap_or(-1),
        Err(_) => -1,
    }
}

extern "C" fn get_size_callback(userdata: *mut c_void) -> Off64 {
    // SAFETY: see `read_at_callback`.
    let ctx = unsafe { &*(userdata as *const DataSourceCtx) };
    ctx.size
}
