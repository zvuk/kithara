use kithara_platform::time::Duration;

use crate::Meter;

/// One listed media-time beat with its stable musical ordinal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackBeat {
    pub at: f64,
    pub ordinal: i64,
}

/// Checked immutable musical geometry, independent of a wire document or decoder.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
#[non_exhaustive]
pub struct TrackGrid {
    /// Returns the listed musical beats in media order.
    #[field(get, deref = "[TrackBeat]")]
    beats: Vec<TrackBeat>,
    /// Returns the explicitly listed bar lines without deriving missing ones.
    #[field(get, deref = "[TrackBeat]")]
    downbeats: Vec<TrackBeat>,
    /// Returns the stated bar length and musical origin.
    #[field(get, copy)]
    meter: Option<Meter>,
    /// Returns the positive finite stated source tempo.
    #[field(get, copy)]
    bpm: f64,
    /// Returns the document's known media length.
    #[field(get, copy)]
    duration: Option<f64>,
}

/// A media grid or its preparation cannot represent the requested geometry.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum TrackGridError {
    #[error("{bpm} is not a positive finite tempo")]
    Bpm { bpm: f64 },
    #[error("{seconds} s is not a position on a media timeline")]
    Time { seconds: f64 },
    #[error("beat {ordinal} at {seconds} s does not follow the marker before it")]
    Order { ordinal: i64, seconds: f64 },
    #[error("beat {ordinal} at {seconds} s lies past the stated {duration} s")]
    PastDuration {
        ordinal: i64,
        seconds: f64,
        duration: f64,
    },
    #[error("the downbeat on beat {ordinal} does not match the listed beat")]
    Anchor { ordinal: i64 },
    #[error("a bar of {beats_per_bar} beats cannot start on beat {ordinal}")]
    Meter { beats_per_bar: u16, ordinal: i64 },
}

impl TrackGrid {
    /// Checks the listed beats, listed bar lines, meter and stated media length.
    ///
    /// # Errors
    /// Returns an error for invalid time, order, tempo, bar anchoring or duration.
    pub fn new(
        beats: Vec<TrackBeat>,
        downbeats: Vec<TrackBeat>,
        meter: Option<Meter>,
        bpm: f64,
        duration: Option<f64>,
    ) -> Result<Self, TrackGridError> {
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(TrackGridError::Bpm { bpm });
        }
        if let Some(seconds) = duration
            && (!seconds.is_finite() || seconds < 0.0)
        {
            return Err(TrackGridError::Time { seconds });
        }
        check_markers(&beats, duration)?;
        check_markers(&downbeats, duration)?;
        for downbeat in &downbeats {
            if !beats
                .binary_search_by_key(&downbeat.ordinal, |beat| beat.ordinal)
                .is_ok_and(|index| beats[index].at == downbeat.at)
            {
                return Err(TrackGridError::Anchor {
                    ordinal: downbeat.ordinal,
                });
            }
        }
        if let Some(meter) = meter {
            for downbeat in &downbeats {
                if downbeat
                    .ordinal
                    .saturating_sub(meter.origin_beat_ordinal)
                    .rem_euclid(i64::from(meter.beats_per_bar.get()))
                    != 0
                {
                    return Err(TrackGridError::Meter {
                        beats_per_bar: meter.beats_per_bar.get(),
                        ordinal: downbeat.ordinal,
                    });
                }
            }
        }
        Ok(Self {
            beats,
            downbeats,
            meter,
            bpm,
            duration,
        })
    }
}

fn check_markers(beats: &[TrackBeat], duration: Option<f64>) -> Result<(), TrackGridError> {
    let mut previous: Option<TrackBeat> = None;
    for beat in beats {
        if !beat.at.is_finite() || beat.at < 0.0 {
            return Err(TrackGridError::Time { seconds: beat.at });
        }
        if let Some(duration) = duration
            && beat.at > duration
        {
            return Err(TrackGridError::PastDuration {
                ordinal: beat.ordinal,
                seconds: beat.at,
                duration,
            });
        }
        if previous.is_some_and(|before| beat.at <= before.at || beat.ordinal <= before.ordinal) {
            return Err(TrackGridError::Order {
                ordinal: beat.ordinal,
                seconds: beat.at,
            });
        }
        previous = Some(*beat);
    }
    Ok(())
}

/// First listed bar line at or after a cue, or listed beat without meter.
#[must_use]
pub fn boundary(grid: &TrackGrid, requested: Duration) -> Option<Duration> {
    let beats = if grid.meter().is_some() {
        grid.downbeats()
    } else {
        grid.beats()
    };
    let beat = beats
        .iter()
        .find(|beat| beat.at >= requested.as_secs_f64())?;
    Duration::try_from_secs_f64(beat.at).ok()
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::*;
    use crate::{SourceGrid, SourceGridError};

    #[kithara::test]
    fn track_geometry_rejects_invalid_times_order_and_source_tempo() {
        let beats = vec![
            TrackBeat {
                at: 0.0,
                ordinal: 0,
            },
            TrackBeat {
                at: 0.5,
                ordinal: 1,
            },
        ];
        for bpm in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                TrackGrid::new(beats.clone(), Vec::new(), None, bpm, None),
                Err(TrackGridError::Bpm { .. })
            ));
        }
        for at in [-1.0, f64::NAN, f64::INFINITY] {
            let mut changed = beats.clone();
            changed[1].at = at;
            assert!(matches!(
                TrackGrid::new(changed, Vec::new(), None, 120.0, None),
                Err(TrackGridError::Time { .. })
            ));
        }
        for changed in [vec![beats[0], beats[0]], vec![beats[1], beats[0]]] {
            assert!(matches!(
                TrackGrid::new(changed, Vec::new(), None, 120.0, None),
                Err(TrackGridError::Order { .. })
            ));
        }
        let grid = TrackGrid::new(beats, Vec::new(), None, 1_001.0, Some(1.0))
            .expect("source tempo is not range-limited Host Tempo");
        assert!(matches!(
            SourceGrid::new(&grid, NonZeroU32::MIN, None, true),
            Err(SourceGridError::IncompleteAxis)
        ));
        assert!(SourceGrid::new(&grid, NonZeroU32::MIN, None, false).is_ok());
    }

    #[kithara::test]
    fn boundaries_use_only_the_listed_bar_lines() {
        let beats: Vec<_> = (0..=8)
            .map(|ordinal| TrackBeat {
                at: ordinal.to_f64().expect("beat ordinal") * 0.5,
                ordinal,
            })
            .collect();
        let meter = Some(Meter {
            beats_per_bar: NonZeroU16::new(4).expect("bar"),
            origin_beat_ordinal: 0,
        });
        let grid = TrackGrid::new(beats.clone(), vec![beats[4]], meter, 120.0, None)
            .expect("incomplete listed bar lines");
        assert_eq!(
            boundary(&grid, Duration::ZERO),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            boundary(&grid, Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(boundary(&grid, Duration::from_secs(3)), None);
        let empty = TrackGrid::new(beats.clone(), Vec::new(), meter, 120.0, None)
            .expect("no listed bar line");
        assert_eq!(boundary(&empty, Duration::ZERO), None);
        let unmetered = TrackGrid::new(beats, Vec::new(), None, 120.0, None).expect("unmetered");
        assert_eq!(
            boundary(&unmetered, Duration::from_millis(750)),
            Some(Duration::from_secs(1))
        );
    }
}
