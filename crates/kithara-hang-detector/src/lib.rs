mod detector;

pub use detector::{HangDetector, default_timeout};
pub use kithara_hang_detector_macros::hang_watchdog;

#[cfg(test)]
mod tests;
