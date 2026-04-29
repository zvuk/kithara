//! Decoder-backend protocol shared by all backends.
//!
//! Each backend module owns one decoder type that implements both
//! [`crate::traits::Decoder`] (runtime) and [`Backend`] (capability +
//! factory): [`crate::apple::AppleDecoder`],
//! [`crate::android::AndroidDecoder`],
//! [`crate::symphonia::SymphoniaDecoder`]. The factory dispatches on
//! [`crate::DecoderBackend`] at call time.

mod protocol;

#[cfg(test)]
mod tests;

pub(crate) use protocol::{Backend, BoxedSource};
