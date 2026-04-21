mod detector;
mod dump;

pub use detector::{HangDetector, default_timeout};
pub use dump::{HangDump, NoContext};
pub use kithara_hang_detector_macros::hang_watchdog;

#[cfg(test)]
mod tests;
