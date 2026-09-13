#![allow(unsafe_code)]
#![cfg(target_os = "android")]

use std::{
    ffi::c_void,
    io::{self, ErrorKind, Read, Seek, SeekFrom},
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};

use super::{
    aformat::OwnedFormat,
    ffi::{
        AMediaDataSource, AMediaDataSource_delete, AMediaDataSource_new,
        AMediaDataSource_setGetSize, AMediaDataSource_setReadAt, AMediaDataSource_setUserdata,
        AMediaExtractor, AMediaExtractor_advance, AMediaExtractor_delete,
        AMediaExtractor_getSampleTime, AMediaExtractor_getTrackCount,
        AMediaExtractor_getTrackFormat, AMediaExtractor_new, AMediaExtractor_readSampleData,
        AMediaExtractor_seekTo, AMediaExtractor_selectTrack, AMediaExtractor_setDataSourceCustom,
        AMediaFormat_getBuffer, AMediaFormat_getInt32, AMediaFormat_getInt64, KEY_CHANNEL_COUNT,
        KEY_CSD_0, KEY_DURATION_US, KEY_MIME, KEY_SAMPLE_RATE, MEDIA_STATUS_OK, Off64,
        SEEK_MODE_PREVIOUS_SYNC, SSize,
    },
    media_codec::{AndroidPcmEncoding, OutputFormat},
};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// `AMediaDataSourceGetSize` reads -1 as "the source has no known length".
const SIZE_UNKNOWN: i64 = -1;

struct DataSourceCtx {
    source: BoxedSource,
    size: SourceSize,
    error: Option<io::Error>,
    // Container recognition sees only the separately supplied init segment.
    init_end: Option<u64>,
}

/// Streaming length follows the pipeline's publication without seeking the
/// source to EOF. Standalone readers supply their probed fixed length.
enum SourceSize {
    Fixed(i64),
    Published(Arc<AtomicU64>),
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
    ctx: Box<DataSourceCtx>,
    data_source: NonNull<AMediaDataSource>,
    raw: NonNull<AMediaExtractor>,
    track_count: usize,
    cursor: SampleCursor,
    pcm_output: Option<OutputFormat>,
}

#[derive(Clone, Copy, Default)]
enum SampleCursor {
    #[default]
    Current,
    Advance {
        next_pcm: Option<Duration>,
    },
    Recover {
        at: Duration,
    },
}

// SAFETY: the wrapper exclusively owns both NDK handles and its pinned callback
// context; operations require a mutable borrow and NDK serializes callbacks.
unsafe impl Send for AndroidMediaExtractor {}

impl AndroidMediaExtractor {
    pub(crate) fn open(
        mut source: BoxedSource,
        byte_len: Option<Arc<AtomicU64>>,
        init_end: Option<u64>,
    ) -> DecodeResult<Self> {
        let size = match byte_len {
            Some(length) => SourceSize::Published(length),
            None => SourceSize::Fixed(probe_size(&mut source)?),
        };
        let mut ctx = Box::new(DataSourceCtx {
            source,
            size,
            error: None,
            init_end,
        });

        // SAFETY: the constructor takes no arguments; its nullable result is checked.
        let ds =
            NonNull::new(unsafe { AMediaDataSource_new() }).ok_or(DecodeError::InvalidData {
                detail: "AMediaDataSource_new returned null",
            })?;

        // SAFETY: ds is live; the boxed context stays at this address until after
        // the extractor and data source are deleted.
        unsafe {
            AMediaDataSource_setUserdata(
                ds.as_ptr(),
                ctx.as_mut() as *mut DataSourceCtx as *mut c_void,
            );
            AMediaDataSource_setReadAt(ds.as_ptr(), Some(read_at_callback));
            AMediaDataSource_setGetSize(ds.as_ptr(), Some(get_size_callback));
        }

        // SAFETY: `AMediaExtractor_new` returns NULL on failure.
        let Some(ex) = NonNull::new(unsafe { AMediaExtractor_new() }) else {
            // SAFETY: `ds` is live and unreferenced; nothing took ownership of it.
            unsafe { AMediaDataSource_delete(ds.as_ptr()) };
            return Err(DecodeError::InvalidData {
                detail: "AMediaExtractor_new returned null",
            });
        };

        // SAFETY: both handles are live, and the data source outlives the extractor.
        let st = unsafe { AMediaExtractor_setDataSourceCustom(ex.as_ptr(), ds.as_ptr()) };
        if st != MEDIA_STATUS_OK {
            // SAFETY: both handles are owned here and no wrapper will free them on this exit.
            unsafe {
                AMediaExtractor_delete(ex.as_ptr());
                AMediaDataSource_delete(ds.as_ptr());
            }
            if let Some(source) = ctx.error.take() {
                return Err(DecodeError::Io { source });
            }
            return Err(DecodeError::BackendStatus {
                code: st,
                op: "AMediaExtractor_setDataSourceCustom",
            });
        }

        ctx.error = None;
        // SAFETY: extractor is live.
        let track_count = unsafe { AMediaExtractor_getTrackCount(ex.as_ptr()) };

        Ok(Self {
            track_count,
            raw: ex,
            data_source: ds,
            ctx,
            cursor: SampleCursor::Current,
            pcm_output: None,
        })
    }

    /// Read the current sample into `buf`. Returns `Ok(Some((n, pts_us)))`
    /// or `Ok(None)` at EOF.
    pub(crate) fn read_sample(&mut self, buf: &mut [u8]) -> DecodeResult<Option<(usize, i64)>> {
        self.prepare_sample()?;
        // SAFETY: the extractor is live and buf is writable for its declared length.
        let n = unsafe {
            AMediaExtractor_readSampleData(self.raw.as_ptr(), buf.as_mut_ptr(), buf.len())
        };
        if n < 0 {
            return self
                .ctx
                .error
                .take()
                .map_or_else(|| Ok(None), |source| Err(DecodeError::Io { source }));
        }
        let read = usize::try_from(n).map_err(DecodeError::backend)?;
        // SAFETY: extractor is live.
        let pts_us = unsafe { AMediaExtractor_getSampleTime(self.raw.as_ptr()) };
        let next_pcm = self
            .pcm_output
            .as_ref()
            .map(|output| -> DecodeResult<Duration> {
                let bytes_per_sample = match output.pcm_encoding {
                    AndroidPcmEncoding::Pcm16 => size_of::<i16>(),
                    AndroidPcmEncoding::Float => size_of::<f32>(),
                };
                let frames = read / (usize::from(output.spec.channels) * bytes_per_sample);
                let pts =
                    Duration::from_micros(u64::try_from(pts_us).map_err(DecodeError::backend)?);
                let next_frame = output
                    .spec
                    .frame_at(pts)
                    .map_err(DecodeError::backend)?
                    .saturating_add(u64::try_from(frames).map_err(DecodeError::backend)?);
                output
                    .spec
                    .duration_for(next_frame)
                    .map_err(DecodeError::backend)
            })
            .transpose()?;
        self.cursor = SampleCursor::Advance { next_pcm };
        Ok(Some((read, pts_us)))
    }

    fn prepare_sample(&mut self) -> DecodeResult<()> {
        match self.cursor {
            SampleCursor::Current => Ok(()),
            SampleCursor::Recover { at } => {
                // Native seeks floor microsecond timestamps onto the PCM grid.
                let micros = at.as_nanos().div_ceil(1_000);
                let result = self.seek_to(i64::try_from(micros).unwrap_or(i64::MAX));
                if result.is_err() {
                    self.cursor = SampleCursor::Recover { at };
                }
                result.map(|_| ())
            }
            SampleCursor::Advance { next_pcm } => {
                self.cursor = SampleCursor::Current;
                // Advancing fetches the next sample and can block on streaming input.
                // SAFETY: the extractor is live; its previous sample has been copied out.
                unsafe { AMediaExtractor_advance(self.raw.as_ptr()) };
                if let Some(source) = self.ctx.error.take() {
                    let error = DecodeError::Io { source };
                    if error.pending_reason().is_some()
                        && let Some(at) = next_pcm
                    {
                        self.cursor = SampleCursor::Recover { at };
                    }
                    return Err(error);
                }
                Ok(())
            }
        }
    }

    /// Seek to nearest previous-sync sample at or before `pts_us`.
    pub(crate) fn seek_to(&mut self, pts_us: i64) -> DecodeResult<Option<(Duration, u64)>> {
        self.ctx.error = None;
        self.cursor = SampleCursor::Current;
        // SAFETY: extractor is live.
        let st =
            unsafe { AMediaExtractor_seekTo(self.raw.as_ptr(), pts_us, SEEK_MODE_PREVIOUS_SYNC) };
        if let Some(source) = self.ctx.error.take() {
            return Err(DecodeError::Io { source });
        }
        if st != MEDIA_STATUS_OK {
            return Err(DecodeError::BackendStatus {
                code: st,
                op: "AMediaExtractor_seekTo",
            });
        }
        // SAFETY: the extractor is live and the seek completed successfully.
        let landed_us = unsafe { AMediaExtractor_getSampleTime(self.raw.as_ptr()) };
        if let Some(source) = self.ctx.error.take() {
            return Err(DecodeError::Io { source });
        }
        if landed_us == -1 {
            return Ok(None);
        }
        let landed_at =
            Duration::from_micros(u64::try_from(landed_us).map_err(DecodeError::backend)?);
        let landed_byte = self.ctx.source.stream_position()?;
        Ok(Some((landed_at, landed_byte)))
    }

    pub(crate) fn select_audio_track(&mut self) -> DecodeResult<(TrackFormatInfo, OwnedFormat)> {
        for i in 0..self.track_count {
            let (info, format) = self.track_info(i)?;
            if info.mime.starts_with("audio/") {
                // SAFETY: i is bounded by the track count returned by this extractor.
                let st = unsafe { AMediaExtractor_selectTrack(self.raw.as_ptr(), i) };
                if st != MEDIA_STATUS_OK {
                    return Err(DecodeError::BackendStatus {
                        code: st,
                        op: "AMediaExtractor_selectTrack",
                    });
                }
                if info.mime == "audio/raw" {
                    self.pcm_output = Some(OutputFormat::read(&format)?);
                }
                if self.ctx.init_end.take().is_some() {
                    // Track selection can cache EOF at the init boundary.
                    self.cursor = SampleCursor::Recover { at: Duration::ZERO };
                }
                return Ok((info, format));
            }
        }
        Err(DecodeError::InvalidData {
            detail: "no audio track found in extractor",
        })
    }

    fn track_info(&self, idx: usize) -> DecodeResult<(TrackFormatInfo, OwnedFormat)> {
        // SAFETY: the extractor is live; the returned format is independently owned.
        let fmt_raw = unsafe { AMediaExtractor_getTrackFormat(self.raw.as_ptr(), idx) };
        let fmt = NonNull::new(fmt_raw).ok_or(DecodeError::InvalidData {
            detail: "AMediaExtractor_getTrackFormat returned null",
        })?;
        let format = OwnedFormat::from(fmt);
        let info = read_track_format(&format)?;
        Ok((info, format))
    }
}

impl Drop for AndroidMediaExtractor {
    fn drop(&mut self) {
        // SAFETY: both handles are uniquely owned; the callback context is still alive.
        unsafe {
            AMediaExtractor_delete(self.raw.as_ptr());
            AMediaDataSource_delete(self.data_source.as_ptr());
        }
    }
}

fn read_track_format(fmt: &OwnedFormat) -> DecodeResult<TrackFormatInfo> {
    let mime = fmt
        .get_str(KEY_MIME)
        .ok_or(DecodeError::InvalidData {
            detail: "track format missing mime",
        })?
        .to_string_lossy()
        .into_owned();

    let mut sample_rate_i: i32 = 0;
    // SAFETY: `fmt` live; `sample_rate_i` exclusively borrowed.
    let _ =
        unsafe { AMediaFormat_getInt32(fmt.raw(), KEY_SAMPLE_RATE.as_ptr(), &mut sample_rate_i) };
    let mut channels_i: i32 = 0;
    // SAFETY: `fmt` live; `channels_i` exclusively borrowed.
    let _ =
        unsafe { AMediaFormat_getInt32(fmt.raw(), KEY_CHANNEL_COUNT.as_ptr(), &mut channels_i) };
    let mut duration_us: i64 = 0;
    // SAFETY: `fmt` live; `duration_us` exclusively borrowed.
    let _ = unsafe { AMediaFormat_getInt64(fmt.raw(), KEY_DURATION_US.as_ptr(), &mut duration_us) };

    let mut csd_data: *mut c_void = ptr::null_mut();
    let mut csd_size: usize = 0;
    // SAFETY: fmt and the static key are live; both out-parameters are writable.
    let has_csd = unsafe {
        AMediaFormat_getBuffer(fmt.raw(), KEY_CSD_0.as_ptr(), &mut csd_data, &mut csd_size)
    };
    let csd_0 = if has_csd && !csd_data.is_null() && csd_size > 0 {
        // SAFETY: the successful query returned csd_size readable bytes owned by fmt.
        unsafe { std::slice::from_raw_parts(csd_data as *const u8, csd_size) }.to_vec()
    } else {
        Vec::new()
    };

    let raw_rate = u32::try_from(sample_rate_i.max(0)).unwrap_or(0);
    if raw_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "android.extractor",
        });
    }
    Ok(TrackFormatInfo {
        mime,
        duration_us,
        csd_0,
        channels: u16::try_from(channels_i.max(0)).unwrap_or(2),
        sample_rate: raw_rate,
    })
}

/// Total length of `source`, cursor restored to the start.
/// `ErrorKind::Unsupported` is how a source states it has no authoritative
/// length, which the data source expresses as [`SIZE_UNKNOWN`].
fn probe_size(source: &mut BoxedSource) -> DecodeResult<i64> {
    let size = match source.seek(SeekFrom::End(0)) {
        Ok(end) => i64::try_from(end).map_err(DecodeError::backend)?,
        Err(err) if err.kind() == ErrorKind::Unsupported => SIZE_UNKNOWN,
        Err(err) => return Err(DecodeError::backend(err)),
    };
    source
        .seek(SeekFrom::Start(0))
        .map_err(DecodeError::backend)?;
    Ok(size)
}

extern "C" fn read_at_callback(
    userdata: *mut c_void,
    offset: Off64,
    buffer: *mut c_void,
    size: usize,
) -> SSize {
    // SAFETY: userdata names the pinned context retained by AndroidMediaExtractor.
    // The NDK invokes this callback serially with a writable buffer of size bytes.
    let (ctx, slice) = unsafe {
        (
            &mut *(userdata as *mut DataSourceCtx),
            std::slice::from_raw_parts_mut(buffer as *mut u8, size),
        )
    };

    let Ok(pos) = u64::try_from(offset) else {
        return -1;
    };
    let available = ctx.init_end.map_or(size, |end| {
        usize::try_from(end.saturating_sub(pos)).map_or(size, |bytes| bytes.min(size))
    });
    if available == 0 {
        return 0;
    }
    let result = ctx
        .source
        .seek(SeekFrom::Start(pos))
        .and_then(|_| ctx.source.read(&mut slice[..available]));
    match result {
        Ok(n) => SSize::try_from(n).unwrap_or(-1),
        Err(error) => {
            ctx.error.get_or_insert(error);
            -1
        }
    }
}

extern "C" fn get_size_callback(userdata: *mut c_void) -> Off64 {
    // SAFETY: userdata names the pinned context retained until both NDK handles drop.
    let ctx = unsafe { &*(userdata as *const DataSourceCtx) };
    if let Some(end) = ctx.init_end {
        return i64::try_from(end).unwrap_or(SIZE_UNKNOWN);
    }
    match &ctx.size {
        SourceSize::Fixed(size) => *size,
        SourceSize::Published(length) => match length.load(Ordering::Acquire) {
            0 => SIZE_UNKNOWN,
            length => i64::try_from(length).unwrap_or(SIZE_UNKNOWN),
        },
    }
}
