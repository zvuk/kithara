use std::num::NonZeroU64;

use super::{Mp4EditListEntry, Mp4MediaTiming};
use crate::GaplessInfo;

/// A track's edit list as the decoder applies it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TrackEdit {
    /// No edit: media time zero is presentation time zero.
    #[default]
    Identity,
    /// One edit at normal rate that starts the presentation `media_time`
    /// media ticks into the track.
    Offset {
        media_time: u64,
        /// Presented length in movie ticks; `None` when the edit runs to the
        /// end of the media (`segment_duration == 0`).
        segment: Option<NonZeroU64>,
    },
    /// An edit list the decoder does not apply.
    Unsupported,
}

impl From<&[Mp4EditListEntry]> for TrackEdit {
    fn from(entries: &[Mp4EditListEntry]) -> Self {
        match entries {
            [] => Self::Identity,
            [edit] if edit.media_rate == 1 << 16 => {
                u64::try_from(edit.media_time).map_or(Self::Unsupported, |media_time| {
                    Self::Offset {
                        media_time,
                        segment: NonZeroU64::new(edit.segment_duration),
                    }
                })
            }
            _ => Self::Unsupported,
        }
    }
}

impl TrackEdit {
    /// Leading and trailing frames the edit trims at `sample_rate`.
    pub(crate) fn gapless(
        self,
        media: Mp4MediaTiming,
        sample_rate: u32,
        movie_timescale: Option<u32>,
    ) -> Option<GaplessInfo> {
        let Self::Offset {
            media_time,
            segment,
        } = self
        else {
            return None;
        };
        let leading = scale_frames(media_time, sample_rate, media.timescale)?;
        let trailing = match segment {
            None => 0,
            Some(segment) => {
                let valid = scale_frames(segment.get(), sample_rate, movie_timescale?)?;
                let total = scale_frames(media.duration, sample_rate, media.timescale)?;
                total.checked_sub(leading.checked_add(valid)?)?
            }
        };
        (leading != 0 || trailing != 0).then_some(GaplessInfo {
            leading_frames: leading,
            trailing_frames: trailing,
        })
    }
}

fn scale_frames(value: u64, numerator: u32, denominator: u32) -> Option<u64> {
    if denominator == 0 {
        return None;
    }

    let scaled = u128::from(value)
        .saturating_mul(u128::from(numerator))
        .saturating_add(u128::from(denominator / 2))
        / u128::from(denominator);
    u64::try_from(scaled).ok()
}
