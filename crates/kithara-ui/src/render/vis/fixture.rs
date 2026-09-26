//! Visualiser frames built the way a leaf reads them.

use super::VisFrame;
use crate::render::{ReadValue, Reads, StereoLevels};

/// A master bus at one level and a host clock at one time.
struct Bus {
    level: f32,
    time: f64,
}

impl Reads for Bus {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: self.level,
                r: self.level,
                volume: 1.0,
            })),
            "vis.time" => Some(ReadValue::Scalar(self.time)),
            _ => None,
        }
    }
}

/// The frame a leaf reads with the master bus at `level`, the clock at `time`
/// seconds and `preset` selected.
pub(super) fn frame(level: f32, time: f64, preset: u32) -> VisFrame {
    VisFrame::read(
        Some(ReadValue::Scalar(f64::from(preset))),
        &Bus { level, time },
    )
    .expect("a preset in range reads a frame")
}
