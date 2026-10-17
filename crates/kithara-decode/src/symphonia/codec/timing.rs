use std::collections::VecDeque;

use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::{
    DecodeError, DecodeResult,
    demuxer::{Frame, PacketMetadata},
};

struct Interval {
    pts: Duration,
    valid: u64,
    block: u64,
    consumed: u64,
    metadata: PacketMetadata,
}

pub(super) enum Output {
    Samples {
        frames: u64,
        metadata: PacketMetadata,
    },
    Padding(u64),
}

#[derive(Default)]
pub(super) struct Timing {
    intervals: VecDeque<Interval>,
    decoded_at: Option<Duration>,
    spec: Option<AudioSpec>,
}

impl Timing {
    pub(super) fn push(
        &mut self,
        frame: Frame<'_>,
        metadata: PacketMetadata,
        spec: AudioSpec,
        block: u64,
    ) -> DecodeResult<()> {
        if self.spec.is_some_and(|previous| previous != spec) && !self.intervals.is_empty() {
            return Err(DecodeError::InvalidData {
                detail: "AAC output spec changed with pending packet intervals",
            });
        }
        self.spec = Some(spec);
        let valid = if frame.duration.is_zero() {
            block
        } else {
            spec.frame_at(frame.duration)?
        };
        if valid > block || block == 0 {
            return Err(DecodeError::InvalidData {
                detail: "AAC presentation exceeds its decoded block",
            });
        }
        self.intervals
            .try_reserve(1)
            .map_err(DecodeError::backend)?;
        self.intervals.push_back(Interval {
            pts: frame.pts,
            valid,
            block,
            consumed: 0,
            metadata,
        });
        Ok(())
    }

    pub(super) fn take(&mut self, available: u64, spec: AudioSpec) -> DecodeResult<Output> {
        let interval = self.intervals.front_mut().ok_or(DecodeError::InvalidData {
            detail: "AAC decoder emitted PCM beyond its supplied packets",
        })?;
        let step = if interval.consumed < interval.valid {
            let frames = available.min(interval.valid - interval.consumed);
            let start = spec.frame_at(interval.pts)?;
            let end = start
                .checked_add(interval.consumed)
                .ok_or(DecodeError::InvalidData {
                    detail: "AAC PCM frame position overflow",
                })?;
            let elapsed = spec
                .duration_for(end)?
                .checked_sub(spec.duration_for(start)?)
                .ok_or(DecodeError::InvalidData {
                    detail: "AAC PCM frame phase underflow",
                })?;
            let at = interval
                .pts
                .checked_add(elapsed)
                .ok_or(DecodeError::InvalidData {
                    detail: "AAC PCM position overflow",
                })?;
            let metadata = PacketMetadata {
                source_bytes: if interval.consumed == 0 {
                    interval.metadata.source_bytes
                } else {
                    0
                },
                ..interval.metadata
            };
            interval.consumed += frames;
            self.decoded_at = Some(at);
            Output::Samples { frames, metadata }
        } else {
            let frames = available.min(interval.block - interval.consumed);
            interval.consumed += frames;
            Output::Padding(frames)
        };
        if interval.consumed == interval.block {
            self.intervals.pop_front();
        }
        Ok(step)
    }

    pub(super) fn decoded_pts(&self) -> Option<Duration> {
        self.decoded_at
            .or_else(|| self.intervals.front().map(|interval| interval.pts))
    }

    pub(super) fn is_complete(&self) -> bool {
        self.intervals.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.intervals.clear();
        self.decoded_at = None;
        self.spec = None;
    }
}
