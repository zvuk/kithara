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
        let by_time = self
            .segments
            .iter()
            .find(|desc| t < desc.decode_time.saturating_add(desc.duration));
        let last = self.segments.last();
        by_time.or(last).cloned()
    }

    pub(crate) fn segment_count(&self) -> u32 {
        let n = self.segments.len();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(segment_count = n, "BUG: fragment count exceeds u32::MAX");
            0
        })
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
            let byte_end = mp4.moofs.get(idx + 1).map_or(total, |next| next.start);
            let segment_index = u32::try_from(idx).ok()?;
            let (descriptor, decode_end) = segment_descriptor(
                moof,
                FragmentInput {
                    byte_end,
                    timescale,
                    segment_index,
                    prev_decode_time: cumulative_decode_time,
                },
            )?;
            cumulative_decode_time = Some(decode_end);
            segments.push(descriptor);
        }

        Some(Self {
            init_range,
            segments,
        })
    }
}

#[derive(Clone, Copy)]
struct FragmentInput {
    prev_decode_time: Option<u64>,
    segment_index: u32,
    timescale: u32,
    byte_end: u64,
}

/// Build the [`SegmentDescriptor`] for one `moof` and return it alongside
/// the cumulative decode-time tick count after this fragment. Returns
/// `None` for a malformed fragment (no `traf`, zero duration, or a byte
/// range that does not advance).
fn segment_descriptor(
    moof: &re_mp4::MoofBox,
    input: FragmentInput,
) -> Option<(SegmentDescriptor, u64)> {
    let FragmentInput {
        byte_end,
        timescale,
        segment_index,
        prev_decode_time,
    } = input;
    let traf = moof.trafs.first()?;
    let decode_ticks = traf
        .tfdt
        .as_ref()
        .map(|t| t.base_media_decode_time)
        .or(prev_decode_time)
        .unwrap_or(0);
    let frag_duration_ticks: u64 = traf
        .truns
        .iter()
        .map(|trun| trun_duration_ticks(trun, &traf.tfhd))
        .sum();
    if frag_duration_ticks == 0 {
        return None;
    }
    let byte_start = moof.start;
    if byte_end <= byte_start {
        return None;
    }
    let descriptor = SegmentDescriptor::new(
        byte_start..byte_end,
        ticks_to_duration(decode_ticks, timescale),
        ticks_to_duration(frag_duration_ticks, timescale),
        segment_index,
        0,
    );
    Some((descriptor, decode_ticks.saturating_add(frag_duration_ticks)))
}

fn trun_duration_ticks(trun: &re_mp4::TrunBox, tfhd: &re_mp4::TfhdBox) -> u64 {
    if !trun.sample_durations.is_empty() {
        return trun
            .sample_durations
            .iter()
            .map(|d| u64::from(*d))
            .sum::<u64>();
    }
    if trun.sample_count > 0 {
        let default_dur = tfhd.default_sample_duration.unwrap_or(0);
        return u64::from(trun.sample_count).saturating_mul(u64::from(default_dur));
    }
    0
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
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    const NANOS_PER_SEC_MINUS_ONE: u32 = 999_999_999;
    if timescale == 0 {
        return Duration::ZERO;
    }
    let secs = ticks / u64::from(timescale);
    let rem = ticks % u64::from(timescale);
    let nanos = rem.saturating_mul(NANOS_PER_SEC) / u64::from(timescale);
    let nanos_u32 = u32::try_from(nanos).unwrap_or(NANOS_PER_SEC_MINUS_ONE);
    Duration::new(secs, nanos_u32)
}
