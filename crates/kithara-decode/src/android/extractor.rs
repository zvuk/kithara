#![allow(unsafe_code)]

use std::{ptr::NonNull, time::Duration};

use kithara_stream::{AudioCodec, ContainerFormat};
use tracing::debug;

use super::{
    error::AndroidBackendError,
    ffi::{
        self, KEY_CHANNEL_COUNT, KEY_DURATION_US, KEY_ENCODER_DELAY, KEY_ENCODER_PADDING, KEY_MIME,
        KEY_SAMPLE_RATE, MEDIA_STATUS_OK,
    },
    format::track_mime_matches_codec,
    media_format::OwnedFormat,
    source::OwnedMediaDataSource,
};
use crate::GaplessInfo;

pub(crate) struct SelectedTrack {
    pub(crate) index: usize,
    pub(crate) mime: String,
    pub(crate) duration: Option<Duration>,
    pub(crate) gapless: Option<GaplessInfo>,
}

pub(crate) struct ExtractorBootstrap {
    pub(crate) media_source: OwnedMediaDataSource,
    pub(crate) extractor: OwnedExtractor,
    pub(crate) selected_track: SelectedTrack,
}

pub(crate) struct CompressedSample<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) presentation_time_us: i64,
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
        }))
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
    container: Option<ContainerFormat>,
    gapless_enabled: bool,
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

    let mut selected_track = match select_track(&extractor, codec) {
        Ok(selected_track) => selected_track,
        Err(error) => return Err((media_source, error)),
    };
    selected_track.gapless = match resolve_gapless_info(
        &media_source,
        &extractor,
        &selected_track,
        codec,
        container,
        gapless_enabled,
    ) {
        Ok(gapless) => gapless,
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
        gapless = ?selected_track.gapless,
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
            gapless: None,
        });
    }

    Err(AndroidBackendError::operation(
        "extractor-select-track",
        format!("no audio track matching codec {codec:?}"),
    ))
}

fn resolve_gapless_info(
    media_source: &OwnedMediaDataSource,
    extractor: &OwnedExtractor,
    selected_track: &SelectedTrack,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    gapless_enabled: bool,
) -> Result<Option<GaplessInfo>, AndroidBackendError> {
    if !gapless_enabled || !codec_supports_gapless(codec) {
        return Ok(None);
    }

    if ffi::current_api_level().is_some_and(|level| level >= 30)
        && let Some(info) = gapless_from_media_format(extractor, selected_track.index)?
    {
        debug!(
            target: "kithara::gapless",
            source = "android_media_format",
            track_index = selected_track.index,
            leading_frames = info.leading_frames,
            trailing_frames = info.trailing_frames,
            "captured gapless metadata from Android MediaFormat"
        );
        return Ok(Some(info));
    }

    if should_probe_mp4_gapless(codec, container) {
        let gapless = media_source.probe_mp4_gapless()?;
        if let Some(info) = gapless {
            debug!(
                target: "kithara::gapless",
                source = "android_mp4_probe",
                track_index = selected_track.index,
                leading_frames = info.leading_frames,
                trailing_frames = info.trailing_frames,
                "captured gapless metadata from MP4 probe for Android"
            );
        }
        return Ok(gapless);
    }

    Ok(None)
}

fn gapless_from_media_format(
    extractor: &OwnedExtractor,
    track_index: usize,
) -> Result<Option<GaplessInfo>, AndroidBackendError> {
    let format = extractor.track_format(track_index)?;
    let leading_frames = format
        .get_i32(KEY_ENCODER_DELAY)
        .map(u64::try_from)
        .transpose()
        .map_err(|_| {
            AndroidBackendError::operation(
                "extractor-gapless-media-format",
                format!("track={track_index} encoder-delay out of range"),
            )
        })?
        .unwrap_or(0);
    let trailing_frames = format
        .get_i32(KEY_ENCODER_PADDING)
        .map(u64::try_from)
        .transpose()
        .map_err(|_| {
            AndroidBackendError::operation(
                "extractor-gapless-media-format",
                format!("track={track_index} encoder-padding out of range"),
            )
        })?
        .unwrap_or(0);

    if leading_frames == 0 && trailing_frames == 0 {
        return Ok(None);
    }

    Ok(Some(GaplessInfo {
        leading_frames,
        trailing_frames,
    }))
}

fn codec_supports_gapless(codec: AudioCodec) -> bool {
    matches!(
        codec,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2
    )
}

fn should_probe_mp4_gapless(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    codec_supports_gapless(codec)
        && matches!(
            container,
            Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
        )
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
            gapless: None,
        };

        assert_eq!(track.index, 2);
        assert_eq!(track.mime, "audio/flac");
        assert_eq!(track.duration, Some(Duration::from_secs(12)));
        assert_eq!(track.gapless, None);
    }

    #[kithara::test]
    fn mp4_gapless_probe_is_limited_to_aac_mp4_containers() {
        assert!(should_probe_mp4_gapless(
            AudioCodec::AacLc,
            Some(ContainerFormat::Fmp4)
        ));
        assert!(should_probe_mp4_gapless(
            AudioCodec::AacHe,
            Some(ContainerFormat::Mp4)
        ));
        assert!(!should_probe_mp4_gapless(
            AudioCodec::Mp3,
            Some(ContainerFormat::Fmp4)
        ));
        assert!(!should_probe_mp4_gapless(
            AudioCodec::AacLc,
            Some(ContainerFormat::Adts)
        ));
        assert!(!should_probe_mp4_gapless(AudioCodec::AacLc, None));
    }
}
