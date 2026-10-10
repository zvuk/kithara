pub(crate) const DISCRIMINATOR_DOMAIN: &[u8] = b"kithara.play.query-discriminator.v1\0";
pub(crate) const HASH_BYTES: usize = 16;
pub(crate) const IDENTITY_DOMAIN: &[u8] = b"kithara.play.query-identity.v1\0";

#[cfg(test)]
pub(crate) const BLOCK_FRAMES: usize = 512;

#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 44_100;

#[cfg(test)]
pub(crate) const DROPPED_AFTER_CANCEL: u8 = 2;

#[cfg(test)]
pub(crate) const DROPPED_BEFORE_CANCEL: u8 = 1;

#[cfg(test)]
pub(crate) const NOT_DROPPED: u8 = 0;

#[cfg(test)]
pub(crate) const RATE_RING_PACKETS: usize = 16;

/// Upper bound on the ticks that fill a lane; a filled ring stops progress far
/// sooner, so the bound only turns a livelock into a failure.
#[cfg(test)]
pub(crate) const FILL_TICKS: usize = 1_024;
