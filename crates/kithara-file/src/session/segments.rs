//! File-side fMP4 segment index.
//!
//! Lazily walks a fully downloaded fragmented mp4 / m4a file and
//! produces an in-memory list of `(decode_time, duration, byte_range)`
//! per `moof` fragment. Used by [`super::source::FileSource`] to expose
//! `Source::init_segment_range / segment_at_time / segment_after_byte /
//! segment_count`, which in turn lets the decoder factory route
//! file-fmp4 through `UniversalDecoder<Fmp4SegmentDemuxer,
//! SymphoniaCodec>` instead of Symphonia's whole-stream `IsoMp4Reader`.
//!
//! Scope: only fragmented mp4 (with a non-empty `moof` chain). Classic
//! mp4 (`stbl`-driven sample table) is not handled here — that path
//! still flows through the legacy `SymphoniaDecoder` god-type. Streams
//! whose moov is at the tail (rare for streaming) are also out of scope
//! because the file may not be fully downloaded when the index is
//! probed.

use std::{ops::Range, time::Duration};

use kithara_stream::SegmentDescriptor;
use re_mp4::Mp4;

/// Pre-computed fragmented-mp4 layout for a fully cached file.
#[derive(Clone, Debug)]
pub(crate) struct FileSegmentIndex {
    init_range: Range<u64>,
    segments: Vec<SegmentDescriptor>,
}

impl FileSegmentIndex {
    pub(crate) fn init_range(&self) -> Range<u64> {
        self.init_range.clone()
    }

    pub(crate) fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.segments
            .iter()
            .find(|desc| desc.byte_range.start >= byte_offset)
            .cloned()
    }

    pub(crate) fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.segments
            .iter()
            .find(|desc| t < desc.decode_time.saturating_add(desc.duration))
            .or_else(|| self.segments.last())
            .cloned()
    }

    pub(crate) fn segment_count(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    /// Try to derive a fragmented-mp4 index from the given file bytes.
    ///
    /// Returns `None` when:
    /// - the bytes do not parse as mp4,
    /// - the mp4 has no `moof` boxes (classic mp4 file),
    /// - the audio track's timescale is unavailable,
    /// - any moof is malformed (zero or unknown duration).
    pub(crate) fn try_build(bytes: &[u8]) -> Option<Self> {
        let mp4 = Mp4::read_bytes(bytes).ok()?;
        if mp4.moofs.is_empty() {
            return None;
        }

        let timescale = audio_track_timescale(&mp4)?;
        if timescale == 0 {
            return None;
        }

        let first_moof_start = mp4.moofs.first()?.start;
        if first_moof_start == 0 {
            return None;
        }
        let init_range = 0..first_moof_start;

        let total = u64::try_from(bytes.len()).ok()?;
        let mut segments = Vec::with_capacity(mp4.moofs.len());
        let mut cumulative_decode_time: Option<u64> = None;

        for (idx, moof) in mp4.moofs.iter().enumerate() {
            let traf = moof.trafs.first()?;
            let decode_ticks = traf
                .tfdt
                .as_ref()
                .map(|t| t.base_media_decode_time)
                .or(cumulative_decode_time)
                .unwrap_or(0);
            let mut frag_duration_ticks: u64 = 0;
            for trun in &traf.truns {
                if !trun.sample_durations.is_empty() {
                    frag_duration_ticks += trun
                        .sample_durations
                        .iter()
                        .map(|d| u64::from(*d))
                        .sum::<u64>();
                } else if trun.sample_count > 0 {
                    // Fall back to tfhd default sample duration when the
                    // trun does not list per-sample durations.
                    let default_dur = traf.tfhd.default_sample_duration.unwrap_or(0);
                    frag_duration_ticks +=
                        u64::from(trun.sample_count).saturating_mul(u64::from(default_dur));
                }
            }
            if frag_duration_ticks == 0 {
                return None;
            }
            cumulative_decode_time = Some(decode_ticks.saturating_add(frag_duration_ticks));

            let byte_start = moof.start;
            let byte_end = mp4.moofs.get(idx + 1).map_or(total, |next| next.start);
            if byte_end <= byte_start {
                return None;
            }

            let segment_index = u32::try_from(idx).ok()?;
            segments.push(SegmentDescriptor::new(
                byte_start..byte_end,
                ticks_to_duration(decode_ticks, timescale),
                ticks_to_duration(frag_duration_ticks, timescale),
                segment_index,
                0,
            ));
        }

        Some(Self {
            init_range,
            segments,
        })
    }
}

fn audio_track_timescale(mp4: &Mp4) -> Option<u32> {
    mp4.moov
        .traks
        .iter()
        .find(|t| {
            matches!(
                t.mdia.minf.stbl.stsd.contents,
                re_mp4::StsdBoxContent::Mp4a(_) | re_mp4::StsdBoxContent::Unknown(_)
            )
        })
        .map(|t| t.mdia.mdhd.timescale)
}

fn ticks_to_duration(ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let secs = ticks / u64::from(timescale);
    let rem = ticks % u64::from(timescale);
    let nanos = rem.saturating_mul(1_000_000_000) / u64::from(timescale);
    let nanos_u32 = u32::try_from(nanos).unwrap_or(999_999_999);
    Duration::new(secs, nanos_u32)
}
