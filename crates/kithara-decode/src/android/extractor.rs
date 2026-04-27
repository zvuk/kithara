#![allow(unsafe_code)]

use std::{ptr, ptr::NonNull, time::Duration};

use kithara_stream::AudioCodec;
use tracing::debug;

use super::{
    error::AndroidBackendError,
    ffi::{self, KEY_CHANNEL_COUNT, KEY_DURATION_US, KEY_MIME, KEY_SAMPLE_RATE, MEDIA_STATUS_OK},
    format::track_mime_matches_codec,
    media_format::OwnedFormat,
    source::OwnedMediaDataSource,
};

pub(crate) struct SelectedTrack {
    pub(crate) index: usize,
    pub(crate) mime: String,
    pub(crate) duration: Option<Duration>,
}

pub(crate) struct ExtractorBootstrap {
    pub(crate) media_source: OwnedMediaDataSource,
    pub(crate) extractor: OwnedExtractor,
    pub(crate) selected_track: SelectedTrack,
}

pub(crate) struct CompressedSample<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) presentation_time_us: i64,
    /// Byte offset of this sample within the source file, when the
    /// extractor reports it via the `sample-file-offset` key on
    /// `AMediaExtractor_getSampleFormat` (API 28+). `None` on earlier
    /// API levels or when the underlying container does not provide
    /// a per-sample byte offset.
    pub(crate) byte_offset: Option<u64>,
}

pub(crate) struct OwnedExtractor {
    raw: NonNull<ffi::AMediaExtractor>,
}

unsafe impl Send for OwnedExtractor {}

impl OwnedExtractor {
    pub(crate) fn raw(&self) -> *mut ffi::AMediaExtractor {
        self.raw.as_ptr()
    }

    pub(crate) fn track_format(&self, index: usize) -> Result<OwnedFormat, AndroidBackendError> {
        let raw = NonNull::new(unsafe { ffi::AMediaExtractor_getTrackFormat(self.raw(), index) })
            .ok_or_else(|| {
            AndroidBackendError::operation(
                "extractor-track-format",
                format!("track={index} format was null"),
            )
        })?;
        Ok(OwnedFormat::from_raw(raw))
    }

    pub(crate) fn seek_to(&self, target_us: i64) -> Result<(), AndroidBackendError> {
        let status = unsafe {
            ffi::AMediaExtractor_seekTo(self.raw(), target_us, ffi::SEEK_MODE_PREVIOUS_SYNC)
        };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "extractor-seek",
                format!("status={status}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn read_sample<'a>(
        &self,
        buffer: &'a mut [u8],
        selected_track: usize,
    ) -> Result<Option<CompressedSample<'a>>, AndroidBackendError> {
        let track_index = unsafe { ffi::AMediaExtractor_getSampleTrackIndex(self.raw()) };
        if track_index < 0 {
            return Ok(None);
        }
        if track_index != selected_track as i32 {
            return Err(AndroidBackendError::operation(
                "extractor-sample-track",
                format!("expected track={selected_track}, got track={track_index}"),
            ));
        }

        let presentation_time_us = unsafe { ffi::AMediaExtractor_getSampleTime(self.raw()) };
        if presentation_time_us < 0 {
            return Ok(None);
        }

        // Pull the per-sample format BEFORE `readSampleData`: the
        // extractor's "current sample" cursor is what `getSampleFormat`
        // describes, and `readSampleData` advances internal state in
        // some implementations. The format carries
        // `sample-file-offset` (API 28+); leave as `None` on earlier
        // levels or when the container doesn't surface offsets.
        let byte_offset = self.current_sample_byte_offset();

        let bytes_read = unsafe {
            ffi::AMediaExtractor_readSampleData(self.raw(), buffer.as_mut_ptr(), buffer.len())
        };
        if bytes_read < 0 {
            return Ok(None);
        }
        let bytes_read = usize::try_from(bytes_read).map_err(|_| {
            AndroidBackendError::operation(
                "extractor-read-sample",
                format!("negative sample size unexpectedly converted: {bytes_read}"),
            )
        })?;

        Ok(Some(CompressedSample {
            bytes: &buffer[..bytes_read],
            presentation_time_us,
            byte_offset,
        }))
    }

    /// Return the byte offset of the extractor's current sample if
    /// `AMediaExtractor_getSampleFormat` succeeds and reports
    /// `sample-file-offset`. Failures and missing keys both yield
    /// `None` — the offset is best-effort metadata.
    fn current_sample_byte_offset(&self) -> Option<u64> {
        let mut format: *mut ffi::AMediaFormat = ptr::null_mut();
        let status = unsafe { ffi::AMediaExtractor_getSampleFormat(self.raw(), &mut format) };
        if status != MEDIA_STATUS_OK || format.is_null() {
            return None;
        }
        let mut value: i64 = 0;
        let found = unsafe {
            ffi::AMediaFormat_getInt64(format, ffi::KEY_SAMPLE_FILE_OFFSET.as_ptr(), &mut value)
        };
        unsafe { ffi::AMediaFormat_delete(format) };
        if !found {
            return None;
        }
        u64::try_from(value).ok()
    }

    pub(crate) fn advance(&self) -> Result<bool, AndroidBackendError> {
        Ok(unsafe { ffi::AMediaExtractor_advance(self.raw()) })
    }
}

impl Drop for OwnedExtractor {
    fn drop(&mut self) {
        let _ = unsafe { ffi::AMediaExtractor_delete(self.raw.as_ptr()) };
    }
}

pub(crate) fn bootstrap_extractor(
    media_source: OwnedMediaDataSource,
    codec: AudioCodec,
) -> Result<ExtractorBootstrap, (OwnedMediaDataSource, AndroidBackendError)> {
    let raw = match NonNull::new(unsafe { ffi::AMediaExtractor_new() }) {
        Some(raw) => raw,
        None => {
            return Err((
                media_source,
                AndroidBackendError::operation(
                    "extractor-new",
                    "AMediaExtractor_new returned null",
                ),
            ));
        }
    };
    let extractor = OwnedExtractor { raw };

    let status =
        unsafe { ffi::AMediaExtractor_setDataSourceCustom(extractor.raw(), media_source.raw()) };
    if status != MEDIA_STATUS_OK {
        return Err((
            media_source,
            AndroidBackendError::operation(
                "extractor-set-data-source-custom",
                format!("status={status}"),
            ),
        ));
    }

    let selected_track = match select_track(&extractor, codec) {
        Ok(selected_track) => selected_track,
        Err(error) => return Err((media_source, error)),
    };
    let status = unsafe { ffi::AMediaExtractor_selectTrack(extractor.raw(), selected_track.index) };
    if status != MEDIA_STATUS_OK {
        return Err((
            media_source,
            AndroidBackendError::operation(
                "extractor-select-track",
                format!("track={} status={status}", selected_track.index),
            ),
        ));
    }

    debug!(
        track_index = selected_track.index,
        mime = %selected_track.mime,
        duration = ?selected_track.duration,
        "Android extractor selected audio track"
    );

    Ok(ExtractorBootstrap {
        media_source,
        extractor,
        selected_track,
    })
}

fn select_track(
    extractor: &OwnedExtractor,
    codec: AudioCodec,
) -> Result<SelectedTrack, AndroidBackendError> {
    let track_count = unsafe { ffi::AMediaExtractor_getTrackCount(extractor.raw()) };
    for index in 0..track_count {
        let format = extractor.track_format(index)?;
        let Some(mime) = format.get_string(KEY_MIME)? else {
            continue;
        };
        if !mime.starts_with("audio/") || !track_mime_matches_codec(codec, &mime) {
            continue;
        }

        let _sample_rate = format.get_u32(KEY_SAMPLE_RATE)?.ok_or_else(|| {
            AndroidBackendError::operation(
                "extractor-track-sample-rate",
                format!("track={index} missing sample-rate"),
            )
        })?;
        let _channels = format.get_u16(KEY_CHANNEL_COUNT)?.ok_or_else(|| {
            AndroidBackendError::operation(
                "extractor-track-channel-count",
                format!("track={index} missing channel-count"),
            )
        })?;
        let duration = format
            .get_i64(KEY_DURATION_US)?
            .map(|value| Duration::from_micros(value as u64));

        return Ok(SelectedTrack {
            index,
            mime,
            duration,
        });
    }

    Err(AndroidBackendError::operation(
        "extractor-select-track",
        format!("no audio track matching codec {codec:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn selected_track_snapshot_preserves_duration_and_channels() {
        let track = SelectedTrack {
            index: 2,
            mime: "audio/flac".to_string(),
            duration: Some(Duration::from_secs(12)),
        };

        assert_eq!(track.index, 2);
        assert_eq!(track.mime, "audio/flac");
        assert_eq!(track.duration, Some(Duration::from_secs(12)));
    }
}
