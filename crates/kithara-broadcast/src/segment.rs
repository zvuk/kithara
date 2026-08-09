use bytes::Bytes;
use kithara_encode::EncodedAccessUnit;

use crate::{
    BroadcastError, BroadcastResult, adts::AdtsPacker, config::BroadcastConfig, id3::TimestampTag,
};

/// A closed ADTS media segment and its playlist metadata.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Segment {
    pub seq: u64,
    pub bytes: Bytes,
    pub duration_ts: u32,
    pub discontinuity: bool,
}

/// Frames access units into ADTS and rotates segments on the media clock.
#[derive(Debug)]
pub struct Segmenter {
    packer: AdtsPacker,
    timescale: u32,
    target_ts: u64,
    next_seq: u64,
    stream_ts: u64,
    frames: Vec<u8>,
    units: usize,
    duration_ts: u32,
    discontinuity: bool,
}

impl Segmenter {
    /// Open a segmenter for validated, ADTS-representable audio.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or unsupported ADTS audio.
    pub fn new(config: &BroadcastConfig) -> BroadcastResult<Self> {
        config.validate()?;

        Ok(Self {
            packer: AdtsPacker::new(config.sample_rate, config.channels)?,
            timescale: config.sample_rate,
            target_ts: config.target_ticks()?,
            next_seq: 0,
            stream_ts: 0,
            frames: Vec::new(),
            units: 0,
            duration_ts: 0,
            discontinuity: false,
        })
    }

    /// Append an access unit and close the segment once it reaches the target.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized ADTS frame or segment duration.
    pub fn push(&mut self, unit: &EncodedAccessUnit) -> BroadcastResult<Option<Segment>> {
        let duration_ts = self.duration_ts.checked_add(unit.duration).ok_or_else(|| {
            BroadcastError::DurationOutOfRange {
                duration_ts: u64::from(self.duration_ts) + u64::from(unit.duration),
            }
        })?;
        self.packer.pack_into(&unit.bytes, &mut self.frames)?;
        self.duration_ts = duration_ts;
        self.units += 1;

        if u64::from(self.duration_ts) >= self.target_ts {
            return Ok(self.close());
        }
        Ok(None)
    }

    /// Close the open segment and mark the next one discontinuous.
    pub fn mark_drop(&mut self) -> Option<Segment> {
        let closed = self.close();
        self.discontinuity = true;
        closed
    }

    /// Close the open segment.
    pub fn flush(&mut self) -> Option<Segment> {
        self.close()
    }

    fn close(&mut self) -> Option<Segment> {
        if self.units == 0 {
            return None;
        }

        let mut bytes = Vec::with_capacity(TimestampTag::LEN + self.frames.len());
        bytes.extend_from_slice(&TimestampTag::render(self.stream_ts, self.timescale));
        bytes.extend_from_slice(&self.frames);

        let segment = Segment {
            seq: self.next_seq,
            bytes: Bytes::from(bytes),
            duration_ts: self.duration_ts,
            discontinuity: self.discontinuity,
        };
        self.next_seq += 1;
        self.stream_ts += u64::from(self.duration_ts);
        self.frames.clear();
        self.units = 0;
        self.duration_ts = 0;
        self.discontinuity = false;
        Some(segment)
    }
}

#[cfg(test)]
mod tests {
    use kithara_encode::EncodedAccessUnit;
    use kithara_platform::time::Duration;

    use super::{Segment, Segmenter};
    use crate::{adts::AdtsPacker, config::BroadcastConfig, id3::TimestampTag};

    struct Consts;

    impl Consts {
        const PAYLOAD: usize = 200;
        const SAMPLE_RATE: u32 = 48_000;
        const UNIT_DURATION: u32 = 1_024;
        const UNITS_PER_TARGET: usize = 188;
    }

    fn segmenter() -> Segmenter {
        Segmenter::new(&BroadcastConfig::builder().build()).expect("segmenter")
    }

    fn frame_bytes(units: usize) -> usize {
        TimestampTag::LEN + units * (AdtsPacker::HEADER_LEN + Consts::PAYLOAD)
    }

    fn start_ts(segment: &Segment) -> u64 {
        let timestamp: [u8; 8] = segment.bytes[TimestampTag::LEN - 8..TimestampTag::LEN]
            .try_into()
            .expect("the tag ends in eight timestamp bytes");
        u64::from_be_bytes(timestamp)
    }

    fn unit() -> EncodedAccessUnit {
        EncodedAccessUnit {
            bytes: vec![0x5A; Consts::PAYLOAD],
            is_sync: true,
            duration: Consts::UNIT_DURATION,
            dts: 0,
            pts: 0,
        }
    }

    fn push_units(segmenter: &mut Segmenter, count: usize) -> Vec<Segment> {
        (0..count)
            .filter_map(|_| segmenter.push(&unit()).expect("push"))
            .collect()
    }

    #[test]
    fn the_unit_that_crosses_the_target_closes_the_segment() {
        let mut segmenter = segmenter();

        let before = push_units(&mut segmenter, Consts::UNITS_PER_TARGET - 1);
        let closing = push_units(&mut segmenter, 1);

        assert!(before.is_empty(), "the target is not reached yet");
        assert_eq!(closing.len(), 1);
        assert_eq!(
            closing[0].duration_ts,
            Consts::UNIT_DURATION * u32::try_from(Consts::UNITS_PER_TARGET).expect("count fits")
        );
    }

    #[test]
    fn segments_carry_every_framed_access_unit_and_number_from_zero() {
        let mut segmenter = segmenter();

        let segments = push_units(&mut segmenter, 3 * Consts::UNITS_PER_TARGET);

        assert_eq!(segments.len(), 3);
        for (index, segment) in segments.iter().enumerate() {
            assert_eq!(segment.seq, u64::try_from(index).expect("index fits"));
            assert!(!segment.discontinuity);
            assert_eq!(segment.bytes.len(), frame_bytes(Consts::UNITS_PER_TARGET));
        }
    }

    #[test]
    fn a_segment_opens_with_the_timestamp_of_its_first_sample() {
        let mut segmenter = segmenter();

        let segments = push_units(&mut segmenter, 3 * Consts::UNITS_PER_TARGET);

        for pair in segments.windows(2) {
            assert!(pair[0].bytes.starts_with(b"ID3"));
            assert_eq!(
                start_ts(&pair[1]),
                start_ts(&pair[0])
                    + TimestampTag::mpeg_timestamp(
                        u64::from(pair[0].duration_ts),
                        Consts::SAMPLE_RATE
                    ),
                "the next segment starts where the previous one ended"
            );
        }
        assert_eq!(start_ts(&segments[0]), 0);
    }

    #[test]
    fn a_drop_leaves_the_media_clock_running() {
        let mut segmenter = segmenter();

        push_units(&mut segmenter, 10);
        let closed = segmenter.mark_drop().expect("the open segment closes");
        push_units(&mut segmenter, 10);
        let next = segmenter.flush().expect("the next segment closes");

        assert_eq!(
            start_ts(&next),
            TimestampTag::mpeg_timestamp(u64::from(closed.duration_ts), Consts::SAMPLE_RATE),
            "a gap in the intake does not move the encoded time base"
        );
    }

    #[test]
    fn segment_durations_sum_to_the_pushed_audio() {
        let mut segmenter = segmenter();
        let units = 3 * Consts::UNITS_PER_TARGET + 7;

        let mut segments = push_units(&mut segmenter, units);
        segments.extend(segmenter.flush());

        let pushed = Consts::UNIT_DURATION * u32::try_from(units).expect("count fits");
        let packaged: u32 = segments.iter().map(|segment| segment.duration_ts).sum();
        assert_eq!(packaged, pushed);
    }

    #[test]
    fn a_drop_closes_the_open_segment_and_marks_the_next_one() {
        let mut segmenter = segmenter();

        push_units(&mut segmenter, 10);
        let closed = segmenter.mark_drop().expect("the open segment closes");
        push_units(&mut segmenter, 10);
        let next = segmenter.flush().expect("the next segment closes");

        assert_eq!(closed.seq, 0);
        assert!(!closed.discontinuity);
        assert_eq!(closed.duration_ts, Consts::UNIT_DURATION * 10);
        assert_eq!(next.seq, 1);
        assert!(
            next.discontinuity,
            "the segment after a drop is discontinuous"
        );
    }

    #[test]
    fn a_drop_on_an_empty_buffer_emits_nothing_and_still_marks_the_next_one() {
        let mut segmenter = segmenter();

        assert!(segmenter.mark_drop().is_none(), "no audio, no segment");
        assert!(
            segmenter.mark_drop().is_none(),
            "a repeated drop is idempotent"
        );
        push_units(&mut segmenter, 5);
        let next = segmenter.flush().expect("the next segment closes");

        assert_eq!(
            next.seq, 0,
            "an unemitted segment consumes no sequence number"
        );
        assert!(next.discontinuity);
    }

    #[test]
    fn a_rejected_access_unit_leaves_the_media_clock_alone() {
        let mut segmenter = segmenter();
        let oversized = EncodedAccessUnit {
            bytes: vec![0; AdtsPacker::MAX_PAYLOAD + 1],
            ..unit()
        };

        push_units(&mut segmenter, 5);
        segmenter
            .push(&oversized)
            .expect_err("oversized access unit");
        push_units(&mut segmenter, 5);
        let segment = segmenter.flush().expect("the open segment closes");

        assert_eq!(
            segment.duration_ts,
            Consts::UNIT_DURATION * 10,
            "the rejected unit contributes no duration"
        );
        assert_eq!(segment.bytes.len(), frame_bytes(10));
    }

    #[test]
    fn flushing_an_empty_segmenter_emits_nothing() {
        assert!(segmenter().flush().is_none());
    }

    #[test]
    fn a_target_shorter_than_one_tick_is_rejected() {
        assert!(
            Segmenter::new(
                &BroadcastConfig::builder()
                    .segment_target(Duration::ZERO)
                    .build()
            )
            .is_err()
        );
    }
}
