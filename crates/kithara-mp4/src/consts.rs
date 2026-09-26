/// Window the box walk reads through. Box headers are 8 or 16 bytes and the
/// `moov`/`moof` boxes an index is made of are kilobytes, so one window
/// absorbs a whole run of small reads. A `BufReader` cannot stand in here:
/// every box read ends in an absolute seek, which would drop its buffer.
pub(crate) const WALK_WINDOW_BYTES: usize = 16 * 1024;

/// Media timescale of the synthetic track, in ticks per second.
#[cfg(test)]
pub(crate) const TIMESCALE: u32 = 44_100;

/// Ticks per synthetic sample, matching an AAC access unit.
#[cfg(test)]
pub(crate) const SAMPLE_TICKS: u32 = 1024;

/// Samples per fragment: one fragment is just under a second of audio.
#[cfg(test)]
pub(crate) const SAMPLES_PER_FRAGMENT: u32 = 43;

/// Payload bytes per sample. Sized so a fragment's payload is about a
/// mebibyte, which makes a whole-file read unmistakable against the
/// header-walk budget.
#[cfg(test)]
pub(crate) const SAMPLE_BYTES: u32 = 24_384;

#[cfg(test)]
pub(crate) const FRAGMENTS: u32 = 8;

/// Bytes a header walk may pull. Box headers are 8 bytes and the
/// `moov`/`moof` boxes are hundreds, so the read-ahead window dominates.
#[cfg(test)]
pub(crate) const WALK_BUDGET_BYTES: u64 = 256 * 1024;

/// Track the synthetic fragments address.
#[cfg(test)]
pub(crate) const TRACK_ID: u32 = 1;
