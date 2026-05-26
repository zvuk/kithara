pub use kithara_events::BpmInfo;
use kithara_platform::time::Duration;

mod kithara {}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BeatGrid {
    pub offset: Duration,
    pub bpm: f64,
    pub beats_per_bar: u8,
}

impl Default for BeatGrid {
    fn default() -> Self {
        Self {
            offset: Duration::ZERO,
            bpm: 120.0,
            beats_per_bar: 4,
        }
    }
}
