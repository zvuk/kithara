pub(crate) const BYTE_MAX_BUFFERS: usize = 32;
pub(crate) const BYTE_MAX_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
pub(crate) const DEFAULT_OVERALL_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const SAMPLE_MAX_BUFFERS: usize = 128;
pub(crate) const SAMPLE_MAX_RETAINED_CAPACITY: usize = 200_000;

#[cfg(all(feature = "hang", not(miri)))]
#[cfg(test)]
pub(crate) const LOOP_BREAK_COUNT_2: i32 = 2;

#[cfg(all(feature = "hang", not(miri)))]
#[cfg(test)]
pub(crate) const LOOP_BREAK_COUNT_3: i32 = 3;

#[cfg(all(test, feature = "no-block"))]
pub(crate) const BUDGET_MS: u64 = 5;

#[cfg(all(test, feature = "no-block"))]
pub(crate) const SLEEP_MS: u64 = 1;

/// Verbosity order, so a merge can take the wider of two levels.
#[cfg(not(rtsan))]
pub(crate) const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];
