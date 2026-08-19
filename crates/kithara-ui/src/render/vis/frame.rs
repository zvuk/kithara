use num_traits::ToPrimitive;

use crate::render::{ReadValue, Reads};

/// Host-owned values projected into one visualiser frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub(crate) struct VisFrame {
    level: f32,
    time: f32,
    preset: u32,
}

impl VisFrame {
    const CLOCK: &'static str = "vis.time";
    const MASTER_LEVEL: &'static str = "player.output.levels";
    const PRESET_COUNT: u32 = 3;

    pub(crate) fn read(preset: Option<ReadValue<'_>>, reads: &dyn Reads) -> Option<Self> {
        let Some(ReadValue::Scalar(preset)) = preset else {
            return None;
        };
        let preset = preset
            .round()
            .to_u32()
            .filter(|preset| *preset < Self::PRESET_COUNT)?;
        let Some(ReadValue::Stereo(levels)) = reads.get(Self::MASTER_LEVEL) else {
            return None;
        };
        if !levels.l.is_finite() || !levels.r.is_finite() || !levels.volume.is_finite() {
            return None;
        }
        let level = levels.l.max(levels.r) * levels.volume;
        if !level.is_finite() {
            return None;
        }
        let level = level.clamp(0.0, 1.0);
        let time = match reads.get(Self::CLOCK) {
            Some(ReadValue::Scalar(seconds)) if seconds.is_finite() => match seconds.to_f32() {
                Some(seconds) if seconds.is_finite() => seconds,
                _ => 0.0,
            },
            _ => 0.0,
        };
        Some(Self {
            level,
            time,
            preset,
        })
    }

    #[cfg(all(test, any(feature = "gpu", feature = "masonry")))]
    pub(crate) const fn new(level: f32, time: f32, preset: u32) -> Self {
        Self {
            level,
            time,
            preset,
        }
    }

    /// Master reaction level after volume and range projection.
    #[must_use]
    pub(crate) const fn level(self) -> f32 {
        self.level
    }

    /// Host-provided animation time in seconds.
    #[must_use]
    pub(crate) const fn time(self) -> f32 {
        self.time
    }

    /// Rounded shader preset index in `0..=2`.
    #[must_use]
    pub(crate) const fn preset(self) -> u32 {
        self.preset
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::render::StereoLevels;

    struct FrameReads {
        clock: Option<ReadValue<'static>>,
        levels: Option<ReadValue<'static>>,
    }

    impl Reads for FrameReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                VisFrame::CLOCK => self.clock,
                VisFrame::MASTER_LEVEL => self.levels,
                _ => None,
            }
        }
    }

    fn reads(levels: StereoLevels, clock: Option<ReadValue<'static>>) -> FrameReads {
        FrameReads {
            clock,
            levels: Some(ReadValue::Stereo(levels)),
        }
    }

    fn levels(l: f32, r: f32, volume: f32) -> StereoLevels {
        StereoLevels { l, r, volume }
    }

    #[kithara::test]
    fn preset_is_rounded_only_into_the_shader_range() {
        let reads = reads(levels(0.2, 0.4, 1.0), None);
        for (value, expected) in [(-0.49, 0), (0.49, 0), (0.5, 1), (1.5, 2), (2.49, 2)] {
            assert_eq!(
                VisFrame::read(Some(ReadValue::Scalar(value)), &reads).map(VisFrame::preset),
                Some(expected)
            );
        }
        for value in [-0.51, 2.5, f64::NAN, f64::INFINITY] {
            assert_eq!(VisFrame::read(Some(ReadValue::Scalar(value)), &reads), None);
        }
    }

    #[kithara::test]
    fn level_uses_the_louder_channel_volume_and_clamp() {
        let loud = reads(levels(0.4, 0.8, 2.0), None);
        let quiet = reads(levels(-0.8, -0.4, 1.0), None);

        assert_eq!(
            VisFrame::read(Some(ReadValue::Scalar(1.0)), &loud).map(VisFrame::level),
            Some(1.0)
        );
        assert_eq!(
            VisFrame::read(Some(ReadValue::Scalar(1.0)), &quiet).map(VisFrame::level),
            Some(0.0)
        );
    }

    #[kithara::test]
    fn invalid_preset_or_levels_produce_no_frame() {
        let valid = reads(levels(0.4, 0.8, 1.0), None);
        assert_eq!(VisFrame::read(None, &valid), None);
        assert_eq!(VisFrame::read(Some(ReadValue::Bool(true)), &valid), None);

        let missing = FrameReads {
            clock: None,
            levels: None,
        };
        assert_eq!(VisFrame::read(Some(ReadValue::Scalar(0.0)), &missing), None);
        let wrong = FrameReads {
            clock: None,
            levels: Some(ReadValue::Bool(true)),
        };
        assert_eq!(VisFrame::read(Some(ReadValue::Scalar(0.0)), &wrong), None);
        for levels in [
            levels(f32::NAN, 0.4, 1.0),
            levels(0.4, f32::INFINITY, 1.0),
            levels(0.4, 0.8, f32::NAN),
            levels(f32::MAX, f32::MAX, f32::MAX),
        ] {
            assert_eq!(
                VisFrame::read(Some(ReadValue::Scalar(0.0)), &reads(levels, None)),
                None
            );
        }
    }

    #[kithara::test]
    fn missing_wrong_or_non_finite_clock_is_a_static_frame() {
        for clock in [
            None,
            Some(ReadValue::Bool(true)),
            Some(ReadValue::Scalar(f64::NAN)),
            Some(ReadValue::Scalar(f64::INFINITY)),
            Some(ReadValue::Scalar(f64::MAX)),
        ] {
            assert_eq!(
                VisFrame::read(
                    Some(ReadValue::Scalar(0.0)),
                    &reads(levels(0.4, 0.8, 1.0), clock)
                )
                .map(VisFrame::time),
                Some(0.0)
            );
        }
        assert_eq!(
            VisFrame::read(
                Some(ReadValue::Scalar(0.0)),
                &reads(levels(0.4, 0.8, 1.0), Some(ReadValue::Scalar(12.5)))
            )
            .map(VisFrame::time),
            Some(12.5)
        );
    }
}
