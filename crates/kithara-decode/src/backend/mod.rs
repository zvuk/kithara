//! Hardware-backend protocol shared by Apple `AudioToolbox` and
//! Android `MediaCodec`.
//!
//! Each per-platform impl lives alongside its backend module
//! ([`crate::apple::AppleBackend`], [`crate::android::AndroidBackend`]);
//! the factory picks one through [`current::Current`] (see
//! [`current`]).

pub(crate) mod current;
mod protocol;

#[cfg(test)]
mod tests;

pub(crate) use protocol::{BoxedSource, HardwareBackend, hardware_accepts};
