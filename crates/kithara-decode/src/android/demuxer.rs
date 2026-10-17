#![cfg(target_os = "android")]

use std::sync::atomic::AtomicU64;

use kithara_android::media::OwnedFormat;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{AudioCodec, ByteMap, PrerollHint, SegmentDescriptor};

use super::extractor::{AndroidMediaExtractor, TrackFormatInfo};
use crate::{
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PreparedPacket, TrackInfo},
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// Container parsing through `AMediaExtractor`, retaining its selected track
/// format for codec configuration and reporting the extractor's actual seek
/// position. Segmented sources resolve packet metadata through their byte map.
pub(crate) struct AndroidMediaExtractorDemuxer {
    extractor: AndroidMediaExtractor,
    byte_map: Option<Arc<dyn ByteMap>>,
    segment: Option<SegmentDescriptor>,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_read_len: usize,
    prepared: Option<PreparedPacket>,
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
                u64::try_from(duration_us)
                    .ok()
                    .map(Duration::from_micros)
                    .map(crate::DecoderDuration::RawLength)
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
                byte_map,
                read_buf: vec![0u8; 64 * 1024],
                segment: None,
                last_read_len: 0,
                prepared: None,
            },
            format,
        ))
    }
}

impl Demuxer for AndroidMediaExtractorDemuxer {
    fn duration(&self) -> Option<crate::DecoderDuration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        Ok(match self.prepared.take() {
            Some(PreparedPacket::Frame { pts, duration }) => DemuxOutcome::Frame(Frame {
                pts,
                duration,
                data: &self.read_buf[..self.last_read_len],
                packet_desc: &[],
            }),
            Some(PreparedPacket::Pending(reason)) => DemuxOutcome::Pending(reason),
            Some(PreparedPacket::Eof) => DemuxOutcome::Eof,
            None => DemuxOutcome::Pending(kithara_stream::PendingReason::Retry),
        })
    }

    fn prepare_frame(&mut self) -> DecodeResult<()> {
        if self.prepared.is_some() {
            return Ok(());
        }
        let sample = match self.extractor.read_sample(&mut self.read_buf) {
            Ok(sample) => sample,
            Err(error) => {
                if let Some(reason) = error.pending_reason() {
                    self.prepared = Some(PreparedPacket::Pending(reason));
                    return Ok(());
                }
                return Err(error);
            }
        };
        let Some((n, pts_us)) = sample else {
            self.prepared = Some(PreparedPacket::Eof);
            return Ok(());
        };
        let pts = u64::try_from(pts_us)
            .ok()
            .map_or(Duration::ZERO, Duration::from_micros);
        self.segment = self
            .byte_map
            .as_ref()
            .and_then(|map| map.segment_at_time(pts));
        self.last_read_len = n;
        self.prepared = Some(PreparedPacket::Frame {
            pts,
            duration: Duration::ZERO,
        });
        Ok(())
    }

    fn seek(&mut self, target: Duration, _priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.segment = None;
        self.prepared = None;
        self.last_read_len = 0;
        let pts_us = i64::try_from(target.as_micros()).unwrap_or(i64::MAX);
        let Some((landed_at, landed_byte)) = self.extractor.seek_to(pts_us)? else {
            return self
                .track_info
                .duration
                .and_then(|duration| match duration {
                    crate::DecoderDuration::RawLength(duration) => {
                        Some(DemuxSeekOutcome::PastEof { duration })
                    }
                    crate::DecoderDuration::MediaEnd(_) => None,
                })
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

    fn media_origin(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }

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
}
