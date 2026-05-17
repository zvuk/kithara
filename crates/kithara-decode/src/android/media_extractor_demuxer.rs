#![cfg(target_os = "android")]

use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;

use super::media_extractor::{AndroidMediaExtractor, TrackFormatInfo};
use crate::{
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::DecodeResult,
    traits::BoxedSource,
};

/// `Demuxer` over `AMediaExtractor` for standalone (non-fMP4) container
/// formats. Currently wires WAV/PCM, MP3, and ALAC-in-M4A; Android's
/// `AMediaExtractor` does not parse CAF, so ALAC-CAF is unsupported on
/// this backend.
pub(crate) struct AndroidMediaExtractorDemuxer {
    extractor: AndroidMediaExtractor,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_read_len: usize,
    last_pts_us: i64,
}

impl AndroidMediaExtractorDemuxer {
    pub(crate) fn open_wav(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, AudioCodec::Pcm)
    }

    pub(crate) fn open_mp3(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, AudioCodec::Mp3)
    }

    pub(crate) fn open_alac_m4a(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, AudioCodec::Alac)
    }

    fn open(source: BoxedSource, codec: AudioCodec) -> DecodeResult<Self> {
        let mut extractor = AndroidMediaExtractor::open(source)?;
        let TrackFormatInfo {
            sample_rate,
            channels,
            duration_us,
            csd_0,
            ..
        } = extractor.select_audio_track()?;

        let track_info = TrackInfo {
            codec,
            duration: if duration_us > 0 {
                u64::try_from(duration_us).ok().map(Duration::from_micros)
            } else {
                None
            },
            gapless: None,
            extra_data: csd_0,
            channels,
            sample_rate,
        };

        Ok(Self {
            extractor,
            track_info,
            // Android sample max is typically well under 32 KiB; we
            // round to 64 KiB for headroom on lossless tracks.
            read_buf: vec![0u8; 64 * 1024],
            last_read_len: 0,
            last_pts_us: 0,
        })
    }
}

impl Demuxer for AndroidMediaExtractorDemuxer {
    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        let Some((n, pts_us)) = self.extractor.read_sample(&mut self.read_buf)? else {
            return Ok(DemuxOutcome::Eof);
        };
        self.last_read_len = n;
        self.last_pts_us = pts_us;

        let pts = u64::try_from(pts_us)
            .ok()
            .map(Duration::from_micros)
            .unwrap_or(Duration::ZERO);
        let frame = Frame {
            data: &self.read_buf[..n],
            // Per-sample duration is reported by the codec on output; we
            // leave it zero here since the demuxer side doesn't carry it.
            duration: Duration::ZERO,
            pts,
            // Android `MediaCodec` consumes PTS through
            // `OwnedCodec::queue_input` directly; no opaque per-packet
            // descriptor is needed on this backend.
            packet_desc: &[],
        };
        let _ = self.extractor.advance();
        Ok(DemuxOutcome::Frame(frame))
    }

    fn seek(&mut self, target: Duration) -> DecodeResult<DemuxSeekOutcome> {
        let pts_us = i64::try_from(target.as_micros()).unwrap_or(i64::MAX);
        self.extractor.seek_to(pts_us)?;
        Ok(DemuxSeekOutcome::Landed {
            landed_at: target,
            landed_byte: None,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}
