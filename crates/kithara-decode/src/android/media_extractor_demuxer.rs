#![cfg(target_os = "android")]

use std::sync::atomic::AtomicU64;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{AudioCodec, ByteMap, PrerollHint, SegmentDescriptor};

use super::{
    aformat::OwnedFormat,
    media_extractor::{AndroidMediaExtractor, TrackFormatInfo},
};
use crate::{
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// Container parsing through `AMediaExtractor`, retaining its selected track
/// format for codec configuration and reporting the extractor's actual seek
/// position. Segmented sources resolve packet metadata through their byte map.
pub(crate) struct AndroidMediaExtractorDemuxer {
    extractor: AndroidMediaExtractor,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    byte_map: Option<Arc<dyn ByteMap>>,
    segment: Option<SegmentDescriptor>,
}

impl AndroidMediaExtractorDemuxer {
    pub(crate) fn open(
        source: BoxedSource,
        codec: AudioCodec,
        byte_map: Option<Arc<dyn ByteMap>>,
        byte_len: Option<Arc<AtomicU64>>,
        init_end: Option<u64>,
    ) -> DecodeResult<(Self, OwnedFormat)> {
        let mut extractor = AndroidMediaExtractor::open(source, byte_len, init_end)?;
        let (
            TrackFormatInfo {
                sample_rate,
                channels,
                duration_us,
                csd_0,
                ..
            },
            format,
        ) = extractor.select_audio_track()?;

        let track_info = TrackInfo {
            codec,
            channels,
            sample_rate,
            duration: if duration_us > 0 {
                u64::try_from(duration_us).ok().map(Duration::from_micros)
            } else {
                None
            },
            gapless: None,
            extra_data: csd_0,
        };

        Ok((
            Self {
                extractor,
                track_info,
                read_buf: vec![0u8; 64 * 1024],
                byte_map,
                segment: None,
            },
            format,
        ))
    }
}

impl Demuxer for AndroidMediaExtractorDemuxer {
    delegate::delegate! {
        to self.segment {
            #[expr($.map(|segment| segment.segment_index))]
            #[call(as_ref)]
            fn current_segment_index(&self) -> Option<u32>;
            #[expr($.map(|segment| segment.variant_index))]
            #[call(as_ref)]
            fn current_variant_index(&self) -> Option<usize>;
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        let sample = match self.extractor.read_sample(&mut self.read_buf) {
            Ok(sample) => sample,
            Err(error) => {
                return error
                    .pending_reason()
                    .map_or_else(|| Err(error), |reason| Ok(DemuxOutcome::Pending(reason)));
            }
        };
        let Some((n, pts_us)) = sample else {
            return Ok(DemuxOutcome::Eof);
        };
        let pts = u64::try_from(pts_us)
            .ok()
            .map_or(Duration::ZERO, Duration::from_micros);
        self.segment = self
            .byte_map
            .as_ref()
            .and_then(|map| map.segment_at_time(pts));
        let frame = Frame {
            pts,
            data: &self.read_buf[..n],
            duration: Duration::ZERO,
            packet_desc: &[],
        };
        Ok(DemuxOutcome::Frame(frame))
    }

    fn seek(&mut self, target: Duration, _priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.segment = None;
        let pts_us = i64::try_from(target.as_micros()).unwrap_or(i64::MAX);
        let Some((landed_at, landed_byte)) = self.extractor.seek_to(pts_us)? else {
            return self
                .track_info
                .duration
                .map(|duration| DemuxSeekOutcome::PastEof { duration })
                .ok_or(DecodeError::SeekFailed {
                    detail: "extractor seek reached EOF without a duration",
                });
        };
        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte: Some(landed_byte),
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}
